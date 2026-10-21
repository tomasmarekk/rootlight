//! Retain an owned cancellation probe through one native Wolfram operation.
//! External-scanner accepted steps are separate from parser work and heap use.

use super::{provider_failure, scanner_refusal::NativeScannerRefusalDetails};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter_wolfram::{CancellationProbe, ScannerRefusal, ScannerScope};

pub(super) const SCANNER_CONTRACT_IDENTITY: &str = tree_sitter_wolfram::SCANNER_CONTRACT_IDENTITY;
pub(super) const MAX_EXTERNAL_SCANNER_STEPS: usize = 2_000_000;

struct OwnedCancellation(Cancellation);

impl CancellationProbe for OwnedCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.check().is_err()
    }
}

pub(super) struct Scope {
    native: ScannerScope<OwnedCancellation>,
    limit: u64,
}

pub(super) fn begin(cancel: &Cancellation, credit: usize) -> Result<Scope, AdapterError> {
    cancel.check()?;
    let credit = u64::try_from(credit.min(MAX_EXTERNAL_SCANNER_STEPS))
        .map_err(|_| provider_failure("wolfram-scanner-work-range"))?;
    let native = ScannerScope::begin(OwnedCancellation(cancel.clone()), credit)
        .map_err(|status| map(status, cancel))?;
    Ok(Scope {
        native,
        limit: credit,
    })
}

pub(super) fn finish(
    scope: Scope,
    cancel: &Cancellation,
    witness: Option<&mut Option<NativeScannerRefusalDetails>>,
) -> Result<(), AdapterError> {
    let consumed = scope.native.work();
    let result = scope.native.finish();
    // Cancellation after a soft native decline must remain a strict failure.
    cancel.check()?;
    match result {
        Ok(()) => Ok(()),
        Err(status) => {
            let issued = match status {
                ScannerRefusal::Capacity => Some(NativeScannerRefusalDetails::WolframCommentDepth),
                ScannerRefusal::WorkLimit => {
                    Some(NativeScannerRefusalDetails::WolframExternalWork {
                        consumed,
                        limit: scope.limit,
                    })
                }
                _ => None,
            };
            if let (Some(slot), Some(issued)) = (witness, issued) {
                *slot = Some(issued);
            }
            Err(map(status, cancel))
        }
    }
}

fn map(status: ScannerRefusal, cancel: &Cancellation) -> AdapterError {
    match status {
        ScannerRefusal::OwnerAllocation | ScannerRefusal::NativeAllocationUnavailable => {
            SinkError::AllocationFailed.into()
        }
        ScannerRefusal::Capacity => provider_failure("wolfram-scanner-capacity"),
        ScannerRefusal::MalformedState => provider_failure("wolfram-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("wolfram-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("wolfram-scanner-scope-conflict"),
        ScannerRefusal::InvalidScalar => provider_failure("wolfram-scanner-invalid-scalar"),
        ScannerRefusal::Cancelled => cancel.check().err().map_or_else(
            || provider_failure("wolfram-scanner-cancel-status"),
            Into::into,
        ),
        ScannerRefusal::WorkLimit => provider_failure("wolfram-scanner-work-limit"),
        ScannerRefusal::UnsupportedNul => provider_failure("wolfram-scanner-nul-unavailable"),
        ScannerRefusal::CallbackPanicked => provider_failure("wolfram-scanner-callback-panicked"),
        ScannerRefusal::StatusAbi(_) => provider_failure("wolfram-scanner-status-abi"),
        _ => provider_failure("wolfram-scanner-status-abi"),
    }
}
