//! Retain the qualified owned callback through one static CUDA native operation.
//! Scanner accepted steps are separate from total parser work and heap usage.

use super::{provider_failure, scanner_refusal::NativeScannerRefusalDetails};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter_cuda::{CancellationProbe, ScannerRefusal, ScannerScope};

pub(crate) const SCANNER_CONTRACT_IDENTITY: &str = tree_sitter_cuda::SCANNER_CONTRACT_IDENTITY;
pub(crate) const FRONTEND_POLICY: &str = "rootlight.cuda.written-source/1;source=preserve;native=cuda;cpp-cuh=cuda;physical-edits=fresh;included-ranges=refused;runtime=unqualified";
pub(crate) const MAX_EXTERNAL_SCANNER_STEPS: usize = 2_000_000;

struct OwnedCancellation(Cancellation);

impl CancellationProbe for OwnedCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.check().is_err()
    }
}

pub(crate) struct Scope {
    native: ScannerScope<OwnedCancellation>,
    limit: u64,
}

/// Own one thread-affine scanner callback with finite external-step credit.
pub(crate) fn begin(cancel: &Cancellation, credit: usize) -> Result<Scope, AdapterError> {
    cancel.check()?;
    let credit = u64::try_from(credit.min(MAX_EXTERNAL_SCANNER_STEPS))
        .map_err(|_| provider_failure("cuda-scanner-work-range"))?;
    let native = ScannerScope::begin(OwnedCancellation(cancel.clone()), credit)
        .map_err(|status| map(status, cancel))?;
    Ok(Scope {
        native,
        limit: credit,
    })
}

/// Detach the callback and reject a refused native result before admitting facts.
pub(crate) fn finish(
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
                ScannerRefusal::Capacity => Some(NativeScannerRefusalDetails::CudaStateCapacity),
                ScannerRefusal::WorkLimit => Some(NativeScannerRefusalDetails::CudaExternalWork {
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
        ScannerRefusal::Capacity => provider_failure("cuda-scanner-capacity"),
        ScannerRefusal::MalformedState => provider_failure("cuda-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("cuda-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("cuda-scanner-scope-conflict"),
        ScannerRefusal::InvalidScalar => provider_failure("cuda-scanner-invalid-scalar"),
        ScannerRefusal::Cancelled => cancel.check().err().map_or_else(
            || provider_failure("cuda-scanner-cancel-status"),
            Into::into,
        ),
        ScannerRefusal::WorkLimit => provider_failure("cuda-scanner-work-limit"),
        ScannerRefusal::CallbackPanicked => provider_failure("cuda-scanner-callback-panicked"),
        ScannerRefusal::StatusAbi(_) => provider_failure("cuda-scanner-status-abi"),
        _ => provider_failure("cuda-scanner-status-abi"),
    }
}
