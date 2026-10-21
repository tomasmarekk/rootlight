//! Opaque proof that a complete source reached a genuine native scanner refusal.
//! Parser recovery alone and caller-supplied diagnostics cannot issue this proof.

use super::*;
use rootlight_adapter_sdk::AnalysisLimits;
use rootlight_ids::content_hash;

/// Truthful native operation metadata without fabricated observed counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeScannerRefusalDetails {
    /// Complete token, state or uint32 indentation cannot fit the fixed scanner contract.
    IdrisStateCapacity,
    /// Actual accepted external-scanner work; total demand is unknown on refusal.
    IdrisExternalWork {
        /// Accepted logical steps.
        consumed: u64,
        /// Invocation credit.
        limit: u64,
    },
    /// A fixed Lean state or delimiter-nesting bound refused the operation.
    LeanScannerCapacity,
    /// Actual accepted external-scanner steps; complete demand stays unknown.
    LeanExternalWork {
        /// Actual accepted external-scanner logical steps.
        consumed: u64,
        /// Finite invocation credit for those steps.
        limit: u64,
    },
    /// A regex group increment exceeded the reviewed fixed uint32 state width.
    SedGroupCapacity,
    /// Accepted scanner steps before the next charge was declined.
    SedExternalWork {
        /// Actual accepted external-scanner units, not total parser demand.
        consumed: u64,
        /// Finite invocation credit for those units.
        limit: u64,
    },
    /// A nested comment exceeded the reviewed finite depth policy.
    WolframCommentDepth,
    /// Accepted external-scanner units; complete demand remains unknown on refusal.
    WolframExternalWork {
        /// Actual accepted logical scanner steps.
        consumed: u64,
        /// Configured request credit for accepted scanner steps.
        limit: u64,
    },
    /// Complete Pug state or indentation width exceeds its native format.
    PugStateCapacity,
    /// Actual accepted external steps; the refused next step is not counted.
    PugExternalWork {
        /// Accepted native external-scanner logical steps.
        consumed: u64,
        /// Invocation limit for accepted external-scanner logical steps.
        limit: u64,
    },
    /// Complete heredoc state cannot fit the fixed 1024-byte serialized format.
    DockerfileStateCapacity,
    /// A written heredoc marker needs semantics outside the reviewed scanner.
    DockerfileMarkerSemantics,
    /// Complete CoffeeScript state or indentation width exceeds its native format.
    CoffeescriptStateCapacity,
    /// Embedded NUL cannot be treated as native end of input.
    CoffeescriptNulSemantics,
    /// Dedentation did not name an existing scanner indentation column.
    CoffeescriptIndentationSemantics,
    /// Actual accepted external steps; the refused next step is not counted.
    CoffeescriptExternalWork {
        /// Accepted native external-scanner logical steps.
        consumed: u64,
        /// Invocation limit for accepted external-scanner logical steps.
        limit: u64,
    },
}

impl NativeScannerRefusalDetails {
    /// Describe the refused operation; no state size was observed by this ABI.
    #[must_use]
    pub fn diagnostic(self, message: &str) -> String {
        match self {
            Self::LeanScannerCapacity => format!(
                "{message}; origin lean-external-scanner-fixed-capacity; limits state-entries-510-or-comment-depth-1024-or-raw-hashes-1024; observed unknown"
            ),
            Self::LeanExternalWork { consumed, limit } => format!(
                "{message}; origin lean-external-scanner; unit accepted-external-scanner-logical-steps; limit {limit}; consumed {consumed}; demanded unknown; remaining-demand unknown; next charge declined"
            ),
            Self::IdrisStateCapacity => format!(
                "{message}; origin idris-token-state-or-column; unit fixed-native-capacity; limit token-storage-bytes-1024-or-wire-bytes-1024-or-column-u32; observed unknown"
            ),
            Self::IdrisExternalWork { consumed, limit } => format!(
                "{message}; origin idris-external-scanner; unit accepted-external-scanner-logical-steps; limit {limit}; consumed {consumed}; demanded unknown; remaining-demand unknown; next charge declined"
            ),
            Self::SedGroupCapacity => format!(
                "{message}; origin sed-regex-group-state; unit open-regex-groups; limit 4294967295; observed unknown"
            ),
            Self::SedExternalWork { consumed, limit } => format!(
                "{message}; origin sed-external-scanner; unit accepted-external-scanner-logical-steps; limit {limit}; consumed {consumed}; demanded unknown; remaining-demand unknown; next charge declined"
            ),
            Self::WolframCommentDepth => format!(
                "{message}; origin wolfram-nested-comment; unit open-comment-depth; limit 1024; observed unknown"
            ),
            Self::WolframExternalWork { consumed, limit } => format!(
                "{message}; origin wolfram-external-scanner; unit accepted-external-scanner-logical-steps; limit {limit}; consumed {consumed}; demanded unknown; remaining-demand unknown; next charge declined"
            ),
            Self::PugStateCapacity => format!(
                "{message}; origin pug-state-or-column; unit complete-native-state; limit state-bytes-1024-or-column-65535; observed unknown"
            ),
            Self::PugExternalWork { consumed, limit } => format!(
                "{message}; origin pug-external-scanner; unit accepted-external-scanner-logical-steps; limit {limit}; consumed {consumed}; demanded unknown; remaining-demand unknown; next charge declined"
            ),
            Self::CoffeescriptStateCapacity => format!(
                "{message}; origin coffeescript-state-or-column; unit complete-native-state; limit state-bytes-1024-or-column-65535; observed unknown"
            ),
            Self::CoffeescriptNulSemantics => format!(
                "{message}; origin coffeescript-native-scalar; unsupported embedded NUL; observed unknown"
            ),
            Self::CoffeescriptIndentationSemantics => format!(
                "{message}; origin coffeescript-native-indentation; unsupported unmatched dedentation; observed unknown"
            ),
            Self::CoffeescriptExternalWork { consumed, limit } => format!(
                "{message}; origin coffeescript-external-scanner; unit accepted-external-scanner-logical-steps; limit {limit}; consumed {consumed}; demanded unknown; remaining-demand unknown; next charge declined"
            ),
            Self::DockerfileStateCapacity => format!(
                "{message}; origin dockerfile-heredoc-state; unit serialized-state-bytes; limit 1024; observed unknown"
            ),
            Self::DockerfileMarkerSemantics => format!(
                "{message}; origin dockerfile-heredoc-marker; unsupported written marker semantics; observed unknown"
            ),
        }
    }

    pub(super) fn error(self) -> AdapterError {
        provider_failure(match self {
            Self::LeanScannerCapacity => "lean-scanner-capacity",
            Self::LeanExternalWork { .. } => "lean-scanner-work-limit",
            Self::IdrisStateCapacity => "idris-scanner-capacity",
            Self::IdrisExternalWork { .. } => "idris-scanner-work-limit",
            Self::SedGroupCapacity => "sed-scanner-capacity",
            Self::SedExternalWork { .. } => "sed-scanner-work-limit",
            Self::WolframCommentDepth => "wolfram-scanner-capacity",
            Self::WolframExternalWork { .. } => "wolfram-scanner-work-limit",
            Self::PugStateCapacity => "pug-scanner-capacity",
            Self::PugExternalWork { .. } => "pug-scanner-work-limit",
            Self::CoffeescriptStateCapacity => "coffeescript-scanner-capacity",
            Self::CoffeescriptNulSemantics => "coffeescript-scanner-nul-unavailable",
            Self::CoffeescriptIndentationSemantics => {
                "coffeescript-scanner-indentation-unavailable"
            }
            Self::CoffeescriptExternalWork { .. } => "coffeescript-scanner-work-limit",
            Self::DockerfileStateCapacity => "dockerfile-scanner-capacity",
            Self::DockerfileMarkerSemantics => "dockerfile-scanner-unsupported-marker",
        })
    }

    fn matches_error(self, error: &AdapterError) -> bool {
        self.error() == *error
    }
}

/// Source-bound evidence issued only after an admitted native operation refused.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeScannerRefusal {
    provider_id: u64,
    source: SourceRef,
    language: ContentHash,
    encoding: ContentHash,
    limits: AnalysisLimits,
    native: NativeScannerRefusalDetails,
}

impl std::fmt::Debug for NativeScannerRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeScannerRefusal")
            .field("source", &self.source)
            .field("details", &self.native)
            .finish_non_exhaustive()
    }
}

impl NativeScannerRefusal {
    /// Recognize candidates for a probe without certifying their native origin.
    #[must_use]
    pub fn recognizes_error(error: &AdapterError) -> bool {
        matches!(error, AdapterError::ProviderFailed { code } if matches!(
            code.as_str(), "idris-scanner-capacity" | "idris-scanner-work-limit" | "lean-scanner-capacity" | "lean-scanner-work-limit" | "sed-scanner-capacity" | "sed-scanner-work-limit" | "wolfram-scanner-capacity" | "wolfram-scanner-work-limit" | "pug-scanner-capacity" | "pug-scanner-work-limit" | "dockerfile-scanner-capacity" | "dockerfile-scanner-unsupported-marker"
                | "coffeescript-scanner-capacity" | "coffeescript-scanner-nul-unavailable"
                | "coffeescript-scanner-indentation-unavailable" | "coffeescript-scanner-work-limit"))
    }

    /// Check the exact process-local provider that issued this proof.
    #[must_use]
    pub fn was_issued_by(&self, provider: &TreeSitterProvider) -> bool {
        self.provider_id == provider.provider_id
    }

    /// Return the complete physical source identity admitted by the transaction.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Return descriptive metadata without granting syntax authority.
    #[must_use]
    pub const fn details(&self) -> NativeScannerRefusalDetails {
        self.native
    }

    /// Check exact source, language and encoding before source-only publication.
    #[must_use]
    pub fn matches_source(&self, source: &SourceRef, language: &str, encoding: &str) -> bool {
        self.source == *source
            && self.language == content_hash(language.as_bytes())
            && self.encoding == content_hash(encoding.as_bytes())
    }

    /// Check request and quota conservation while excluding partial native input.
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

    /// Check the exact refusal after the native witness was authenticated.
    #[must_use]
    pub fn matches_error(&self, error: &AdapterError) -> bool {
        self.native.matches_error(error)
    }

    /// Restore the typed refusal when host policy requires strict failure.
    #[must_use]
    pub fn adapter_error(&self) -> AdapterError {
        self.native.error()
    }
}

impl TreeSitterProvider {
    /// Repeat the ordinary admitted full-source path to authenticate a scanner refusal.
    /// Healthy parsing returns no proof. Recovery trees alone, allocation,
    /// cancellation and lifecycle failures cannot create a scanner witness.
    /// The host separately admits source preservation only under its default
    /// quotas; this proof authenticates and retains the exact requested quotas.
    ///
    /// # Errors
    /// Returns unchanged transaction errors without an exact private native witness.
    pub fn certify_native_scanner_refusal(
        &self,
        request: &ParseRequest<'_>,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<Option<NativeScannerRefusal>, AdapterError> {
        cancellation.check()?;
        if !matches!(
            request.language().as_str(),
            "dockerfile" | "coffeescript" | "pug" | "wolfram" | "sed" | "idris" | "lean"
        ) || !request.included_ranges().is_empty()
            || request.native_vba_module().is_some()
        {
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
                    None,
                    Some(&mut native),
                )
                .map(RawParseWithPrevious::into_transaction_parts)
            },
        );
        cancellation.check()?;
        match result {
            Err(error) => match native {
                Some(native) if native.matches_error(&error) => Ok(Some(NativeScannerRefusal {
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
