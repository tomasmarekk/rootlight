//! Source-bound evidence for the native identity-query hard work ceiling.
//! No syntax artifact is returned on this path: the SDK discards all staged facts
//! and only this private native failure branch can construct a certificate.

use super::*;
use crate::query_pack::IntrinsicIdentityMatchLimit;
use rootlight_adapter_sdk::AnalysisLimits;
use rootlight_ids::content_hash;

/// Exact native identity work refusal for one complete checked parse request.
///
/// This does not certify language semantics, a complete syntax partition, or
/// caller policy eligibility. Construction is private to the native provider.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeIdentityWorkLimit {
    provider_id: u64,
    source: SourceRef,
    language: ContentHash,
    encoding: ContentHash,
    limits: AnalysisLimits,
    native: IntrinsicIdentityMatchLimit,
}

impl std::fmt::Debug for NativeIdentityWorkLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeIdentityWorkLimit")
            .field("source", &self.source)
            .field("observed", &self.observed())
            .field("limit", &self.limit())
            .finish_non_exhaustive()
    }
}

impl NativeIdentityWorkLimit {
    /// Checks the process-local native instance that actually issued this event.
    /// The provider identity is not serialized or exposed as a numeric identifier.
    #[must_use]
    pub fn was_issued_by(&self, provider: &TreeSitterProvider) -> bool {
        self.provider_id == provider.provider_id
    }

    /// Returns the full physical source identity that exhausted native work.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns the first rejected match ordinal, without source text or paths.
    #[must_use]
    pub const fn observed(&self) -> usize {
        self.native.observed()
    }

    /// Returns the unchanged intrinsic native identity match ceiling.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.native.limit()
    }

    /// Checks complete physical identity and the preserved language/encoding labels.
    #[must_use]
    pub fn matches_source(&self, source: &SourceRef, language: &str, encoding: &str) -> bool {
        self.source == *source
            && self.language == content_hash(language.as_bytes())
            && self.encoding == content_hash(encoding.as_bytes())
    }

    /// Checks exact source, language, encoding and caller-limit conservation.
    #[must_use]
    pub fn matches_request(&self, request: &ParseRequest<'_>) -> bool {
        request.included_ranges().is_empty()
            && request.native_vba_module().is_none()
            && self.source == *request.source().source_ref()
            && self.language == content_hash(request.language().as_str().as_bytes())
            && self.encoding == content_hash(request.encoding().as_str().as_bytes())
            && self.limits == *request.limits()
    }

    /// Checks that the original refusal agrees with this native-origin event.
    /// Numeric agreement alone is never sufficient to create a certificate.
    #[must_use]
    pub fn matches_error(&self, error: &AdapterError) -> bool {
        self.native.matches_error(error)
    }

    /// Restores the original typed error when host policy forbids degradation.
    #[must_use]
    pub fn adapter_error(&self) -> AdapterError {
        self.native.error()
    }
}

impl TreeSitterProvider {
    /// Repeats an admitted parse to certify only an intrinsic identity Match limit.
    ///
    /// The existing SDK transaction validates capability, memory admission and
    /// request integrity. Successful parses return `None` and retain no artifact.
    /// All other errors, including caller quotas, remain errors. Included ranges
    /// and native VBA activity require the existing full-analysis path.
    ///
    /// # Errors
    /// Returns unchanged encoding, request, cancellation, memory, parser, sink
    /// and report failures that do not originate at the intrinsic Match ceiling.
    pub fn certify_intrinsic_identity_limit(
        &self,
        request: &ParseRequest<'_>,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<Option<NativeIdentityWorkLimit>, AdapterError> {
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
                    Some(&mut native),
                    None,
                    None,
                )
                .map(RawParseWithPrevious::into_transaction_parts)
            },
        );
        cancellation.check()?;
        match result {
            Err(error) => match native {
                Some(native) if native.matches_error(&error) => Ok(Some(NativeIdentityWorkLimit {
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
