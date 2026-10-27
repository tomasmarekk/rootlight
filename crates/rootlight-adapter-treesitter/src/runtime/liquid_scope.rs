//! Retain an owned cancellation probe through one native Liquid operation.
//! External-scanner accepted steps are separate from parser work and heap use.

use super::{provider_failure, scanner_refusal::NativeScannerRefusalDetails};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter_liquid::{CancellationProbe, ScannerRefusal, ScannerScope};

pub(super) const SCANNER_CONTRACT_IDENTITY: &str = tree_sitter_liquid::SCANNER_CONTRACT_IDENTITY;
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
        .map_err(|_| provider_failure("liquid-scanner-work-range"))?;
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
                ScannerRefusal::WorkLimit => {
                    Some(NativeScannerRefusalDetails::LiquidExternalWork {
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
        ScannerRefusal::Capacity => provider_failure("liquid-scanner-capacity"),
        ScannerRefusal::MalformedState => provider_failure("liquid-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("liquid-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("liquid-scanner-scope-conflict"),
        ScannerRefusal::InvalidScalar => provider_failure("liquid-scanner-invalid-scalar"),
        ScannerRefusal::Cancelled => cancel.check().err().map_or_else(
            || provider_failure("liquid-scanner-cancel-status"),
            Into::into,
        ),
        ScannerRefusal::WorkLimit => provider_failure("liquid-scanner-work-limit"),
        ScannerRefusal::UnsupportedNul => provider_failure("liquid-scanner-nul-unavailable"),
        ScannerRefusal::UnsupportedIndentation => {
            provider_failure("liquid-scanner-indentation-unavailable")
        }
        ScannerRefusal::CallbackPanicked => provider_failure("liquid-scanner-callback-panicked"),
        ScannerRefusal::StatusAbi(_) => provider_failure("liquid-scanner-status-abi"),
        _ => provider_failure("liquid-scanner-status-abi"),
    }
}
