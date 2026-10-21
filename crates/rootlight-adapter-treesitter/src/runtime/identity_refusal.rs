//! Opaque proof of a genuine per-source native identity refusal.
//! The existing SDK transaction authenticates admission and discards staged
//! facts; no caller-provided code, number or metadata can construct this proof.

use super::*;
use crate::query_pack::identity_refusal::IdentityRefusal;
use rootlight_adapter_sdk::AnalysisLimits;
use rootlight_ids::content_hash;

/// Source-bound evidence that native identity extraction genuinely refused work.
/// This carries truthful operation metadata, never partial syntax authority.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeIdentityRefusal {
    provider_id: u64,
    source: SourceRef,
    language: ContentHash,
    encoding: ContentHash,
    limits: AnalysisLimits,
    native: IdentityRefusal,
}

impl std::fmt::Debug for NativeIdentityRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeIdentityRefusal")
            .field("source", &self.source)
            .field("details", &self.details())
            .finish_non_exhaustive()
    }
}

impl NativeIdentityRefusal {
    /// Identifies errors eligible for a probe; this does not certify their origin.
    #[must_use]
    pub fn recognizes_error(error: &AdapterError) -> bool {
        IdentityRefusal::recognizes(error)
    }

    /// Checks the native process-local provider that actually issued this proof.
    #[must_use]
    pub fn was_issued_by(&self, provider: &TreeSitterProvider) -> bool {
        self.provider_id == provider.provider_id
    }

    /// Returns the complete physical source identity checked by the transaction.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns descriptive metadata without granting a syntax or source claim.
    #[must_use]
    pub const fn details(&self) -> NativeIdentityRefusalDetails {
        self.native.details()
    }

    /// Checks exact source, language and encoding before source-only publication.
    #[must_use]
    pub fn matches_source(&self, source: &SourceRef, language: &str, encoding: &str) -> bool {
        self.source == *source
            && self.language == content_hash(language.as_bytes())
            && self.encoding == content_hash(encoding.as_bytes())
    }

    /// Checks source/request/limit conservation and excluded native contexts.
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

    /// Checks the exact closed refusal code after private native issuance.
    #[must_use]
    pub fn matches_error(&self, error: &AdapterError) -> bool {
        self.native.matches_error(error)
    }

    /// Restores the typed source-free refusal when host policy stays strict.
    #[must_use]
    pub fn adapter_error(&self) -> AdapterError {
        self.native.error()
    }
}

impl TreeSitterProvider {
    /// Repeats an admitted full parse to authenticate an intrinsic identity refusal.
    /// Success returns no proof. Caller quotas, partial traversal, allocation,
    /// encoding, parser and cancellation errors cannot produce this certificate.
    ///
    /// # Errors
    /// Returns unchanged transaction failures that lack private native issuance.
    pub fn certify_intrinsic_identity_refusal(
        &self,
        request: &ParseRequest<'_>,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<Option<NativeIdentityRefusal>, AdapterError> {
        cancellation.check()?;
        if !request.included_ranges().is_empty() || request.native_vba_module().is_some() {
            return Ok(None);
        }
        let full = request.source().source_ref().span();
        if full.start_byte() != 0
            || usize::try_from(full.end_byte()).ok() != Some(request.source().bytes().len())
        {
            return Ok(None);
        }
        let mut native = None;
        let result = execute_parse_transaction(
            &self.capabilities,
            request,
            memory_policy,
            cancellation,
            |sink, cancellation| {
                self.parse_with_previous_raw(
                    request,
                    None,
                    &[],
                    self.config.default_settings(),
                    sink,
                    cancellation,
                    None,
                    Some(&mut native),
                    None,
                )
                .map(RawParseWithPrevious::into_transaction_parts)
            },
        );
        cancellation.check()?;
        match result {
            Err(error) => match native {
                Some(native) if native.matches_error(&error) => Ok(Some(NativeIdentityRefusal {
                    provider_id: self.provider_id,
                    source: request.source().source_ref().clone(),
                    language: content_hash(request.language().as_str().as_bytes()),
                    encoding: content_hash(request.encoding().as_str().as_bytes()),
                    limits: request.limits().clone(),
                    native,
                })),
                _ => Err(error),
            },
            Ok(_) => Ok(None),
        }
    }
}
