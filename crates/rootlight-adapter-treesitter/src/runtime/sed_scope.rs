//! Retain the qualified owned callback through one static Sed native operation.
//! Scanner accepted steps are separate from total parser work and heap usage.

use super::{provider_failure, scanner_refusal::NativeScannerRefusalDetails};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter_gsed::{CancellationProbe, ScannerRefusal, ScannerScope};

pub(super) const SCANNER_CONTRACT_IDENTITY: &str = tree_sitter_gsed::SCANNER_CONTRACT_IDENTITY;
pub(super) const FRONTEND_POLICY: &str = "rootlight.sed.static-file-text/1;dialect=gnu-bre;paired-crlf=lf;lone-cr=preserve;coordinates=physical-boundaries;runtime-interpretation=unqualified";
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
        .map_err(|_| provider_failure("sed-scanner-work-range"))?;
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
    cancel.check()?;
    match result {
        Ok(()) => Ok(()),
        Err(status) => {
            let issued = match status {
                ScannerRefusal::Capacity => Some(NativeScannerRefusalDetails::SedGroupCapacity),
                ScannerRefusal::WorkLimit => Some(NativeScannerRefusalDetails::SedExternalWork {
                    consumed,
                    limit: scope.limit,
                }),
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
        ScannerRefusal::Capacity => provider_failure("sed-scanner-capacity"),
        ScannerRefusal::MalformedState => provider_failure("sed-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("sed-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("sed-scanner-scope-conflict"),
        ScannerRefusal::InvalidScalar => provider_failure("sed-scanner-invalid-scalar"),
        ScannerRefusal::Cancelled => cancel
            .check()
            .err()
            .map_or_else(|| provider_failure("sed-scanner-cancel-status"), Into::into),
        ScannerRefusal::WorkLimit => provider_failure("sed-scanner-work-limit"),
        ScannerRefusal::CallbackPanicked => provider_failure("sed-scanner-callback-panicked"),
        ScannerRefusal::StatusAbi(_) => provider_failure("sed-scanner-status-abi"),
        _ => provider_failure("sed-scanner-status-abi"),
    }
}
