//! Bind XML declaration refusals to validated complete physical source bytes.
//! Encoding admission runs before native parsing; its opaque proof permits only
//! exact source preservation, without granting syntax or semantic authority.

use super::*;
use rootlight_adapter_sdk::AnalysisLimits;
use rootlight_encoding::xml::{
    CompleteAscii, DeclarationError, PhysicalEncoding, validate_declaration,
};
use rootlight_ids::content_hash;
use rootlight_ir::SourceTextEncoding;

/// Provider-owned evidence of an unverified XML declaration interpretation.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeXmlEncodingRefusal {
    provider_id: u64,
    source: SourceRef,
    language: ContentHash,
    encoding: ContentHash,
    limits: AnalysisLimits,
    reason: DeclarationError,
}

impl std::fmt::Debug for NativeXmlEncodingRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeXmlEncodingRefusal")
            .field("source", &self.source)
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl NativeXmlEncodingRefusal {
    /// Recognizes a candidate error without granting source-preservation authority.
    #[must_use]
    pub fn recognizes_error(error: &AdapterError) -> bool {
        matches!(
            error,
            AdapterError::RejectedRequest(RequestError::UnsupportedEncoding)
        )
    }

    /// Checks the process-local provider that independently issued this proof.
    #[must_use]
    pub fn was_issued_by(&self, provider: &TreeSitterProvider) -> bool {
        self.provider_id == provider.provider_id
    }

    /// Returns the complete immutable physical source reference.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns bounded declaration metadata without exposing source text.
    #[must_use]
    pub const fn reason(&self) -> DeclarationError {
        self.reason
    }

    /// Checks exact source identity, XML language and physical encoding.
    #[must_use]
    pub fn matches_source(&self, source: &SourceRef, language: &str, encoding: &str) -> bool {
        language == "xml"
            && self.source == *source
            && self.language == content_hash(language.as_bytes())
            && self.encoding == content_hash(encoding.as_bytes())
    }

    /// Checks original quotas while excluding partial or foreign native contexts.
    #[must_use]
    pub fn matches_request(&self, request: &ParseRequest<'_>) -> bool {
        request.included_ranges().is_empty()
            && request.native_vba_module().is_none()
            && self.matches_source(
                request.source().source_ref(),
                request.language().as_str(),
                request.encoding().as_str(),
            )
            && self.limits == *request.limits()
    }

    /// Matches only the typed encoding refusal after proof authentication.
    #[must_use]
    pub fn matches_error(&self, error: &AdapterError) -> bool {
        Self::recognizes_error(error)
    }

    /// Reconstructs the strict error without issuing another certificate.
    #[must_use]
    pub fn adapter_error(&self) -> AdapterError {
        RequestError::UnsupportedEncoding.into()
    }
}

fn declaration_refusal(
    request: &ParseRequest<'_>,
    view: &NativeSourceView<'_>,
    cancellation: &Cancellation,
) -> Result<Option<DeclarationError>, AdapterError> {
    let physical = match view.encoding() {
        None => PhysicalEncoding::Utf8,
        Some(SourceTextEncoding::Utf16Le) => PhysicalEncoding::Utf16Le,
        Some(SourceTextEncoding::Utf16Be) => PhysicalEncoding::Utf16Be,
    };
    let result = validate_declaration(view.as_str(), physical, CompleteAscii::Unproven);
    if result != Err(DeclarationError::AsciiProofRequired) {
        return Ok(result.err());
    }
    for chunk in request.source().bytes().chunks(64 * 1024) {
        cancellation.check()?;
        if !chunk.is_ascii() {
            return Ok(result.err());
        }
    }
    cancellation.check()?;
    Ok(validate_declaration(view.as_str(), physical, CompleteAscii::Proven).err())
}

impl TreeSitterProvider {
    /// Independently validates XML source before certifying a declaration refusal.
    ///
    /// Full physical UTF-8 or BOM-backed UTF-16 validation precedes the shared
    /// declaration check. Healthy declarations return `None`. Included ranges,
    /// foreign languages and VBA contexts cannot supply a proof. No native tree,
    /// parser facts or fabricated memory report is produced.
    ///
    /// # Errors
    /// Returns unchanged request, physical encoding, quota, allocation or
    /// cancellation errors. Unavailable hard memory enforcement remains strict.
    pub fn certify_native_xml_encoding_refusal(
        &self,
        request: &ParseRequest<'_>,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<Option<NativeXmlEncodingRefusal>, AdapterError> {
        cancellation.check()?;
        if request.language().as_str() != "xml"
            || !request.included_ranges().is_empty()
            || request.native_vba_module().is_some()
        {
            return Ok(None);
        }
        if self.capabilities.memory_enforcement() == MemoryEnforcement::Unavailable
            && memory_policy == MemoryAdmissionPolicy::RequireHardOrAccounted
        {
            return Err(RequestError::MemoryEnforcementUnavailable.into());
        }
        let span = request.source().source_ref().span();
        if span.start_byte() != 0
            || usize::try_from(span.end_byte()).ok() != Some(request.source().bytes().len())
        {
            return Ok(None);
        }
        self.admit_parse_input(
            request,
            None,
            &[],
            self.config.default_settings(),
            cancellation,
        )?;
        let view = self.physical_source_view(request, cancellation)?;
        let reason = declaration_refusal(request, &view, cancellation)?;
        cancellation.check()?;
        Ok(reason.map(|reason| NativeXmlEncodingRefusal {
            provider_id: self.provider_id,
            source: request.source().source_ref().clone(),
            language: content_hash(request.language().as_str().as_bytes()),
            encoding: content_hash(request.encoding().as_str().as_bytes()),
            limits: request.limits().clone(),
            reason,
        }))
    }

    pub(super) fn require_xml_declaration(
        &self,
        request: &ParseRequest<'_>,
        view: &NativeSourceView<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        if request.language().as_str() == "xml"
            && declaration_refusal(request, view, cancellation)?.is_some()
        {
            return Err(RequestError::UnsupportedEncoding.into());
        }
        cancellation.check()?;
        Ok(())
    }
}
