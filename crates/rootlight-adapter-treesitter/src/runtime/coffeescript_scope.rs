//! Keep first-party code safe while a grammar-owned guard retains its callback.
//! Scanner credit is distinct from parser progress and does not enforce heap use.
use super::{provider_failure, scanner_refusal::NativeScannerRefusalDetails};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter_coffeescript::{CancellationProbe, ScannerRefusal, ScannerScope};

pub(super) const SCANNER_CONTRACT_IDENTITY: &str =
    tree_sitter_coffeescript::SCANNER_CONTRACT_IDENTITY;
// This intrinsic external-scanner allowance does not count parser callbacks.
pub(super) const MAX_EXTERNAL_SCANNER_STEPS: usize = 2_000_000;

pub(super) struct OwnedCancellation(Cancellation);
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
        .map_err(|_| provider_failure("coffeescript-scanner-work-range"))?;
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
    // Host cancellation cannot be converted to a source-only disposition.
    cancel.check()?;
    match result {
        Ok(()) => Ok(()),
        Err(status) => {
            let issued = match status {
                ScannerRefusal::Capacity => {
                    Some(NativeScannerRefusalDetails::CoffeescriptStateCapacity)
                }
                ScannerRefusal::UnsupportedNul => {
                    Some(NativeScannerRefusalDetails::CoffeescriptNulSemantics)
                }
                ScannerRefusal::UnsupportedIndentation => {
                    Some(NativeScannerRefusalDetails::CoffeescriptIndentationSemantics)
                }
                ScannerRefusal::WorkLimit => {
                    Some(NativeScannerRefusalDetails::CoffeescriptExternalWork {
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
        ScannerRefusal::Capacity => provider_failure("coffeescript-scanner-capacity"),
        ScannerRefusal::MalformedState => provider_failure("coffeescript-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("coffeescript-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("coffeescript-scanner-scope-conflict"),
        ScannerRefusal::InvalidScalar => provider_failure("coffeescript-scanner-invalid-scalar"),
        ScannerRefusal::Cancelled => cancel.check().err().map_or_else(
            || provider_failure("coffeescript-scanner-cancel-status"),
            Into::into,
        ),
        ScannerRefusal::WorkLimit => provider_failure("coffeescript-scanner-work-limit"),
        ScannerRefusal::UnsupportedNul => provider_failure("coffeescript-scanner-nul-unavailable"),
        ScannerRefusal::UnsupportedIndentation => {
            provider_failure("coffeescript-scanner-indentation-unavailable")
        }
        ScannerRefusal::StatusAbi(_) => provider_failure("coffeescript-scanner-status-abi"),
        ScannerRefusal::CallbackPanicked => {
            provider_failure("coffeescript-scanner-callback-panicked")
        }
        _ => provider_failure("coffeescript-scanner-status-abi"),
    }
}
