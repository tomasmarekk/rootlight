//! Own Dockerfile cancellation while native scanner callbacks can run.
//! Only a completed native scope can issue a source-preserving refusal witness.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter_dockerfile::{ScannerRefusal, ScannerScope};

use super::{provider_failure, scanner_refusal::NativeScannerRefusalDetails};

pub(super) const SCANNER_CONTRACT_IDENTITY: &str = "dockerfile-scanner-checked-state/2;parser=95a06be957b58f1121ddf523cc8361d8a8c9f256ce0953c62c67bebbe1727612;scanner=76f04e79df9e21d82fbb0b98460dbe549db44ffbcf10373827e3175255d70595;status=07b250b0eff3dbae6bf194084560632321a5bd00d6b04fd0c27f5631a9219ac0;status-unit=2df17e68c0c3e4e718963c4f37bd7e9f38b2394a947b88de47347e49a707ec06";

pub(super) fn begin(
    cancellation: &Cancellation,
) -> Result<ScannerScope<Cancellation>, AdapterError> {
    cancellation.check()?;
    if tree_sitter_dockerfile::SCANNER_CONTRACT_IDENTITY != SCANNER_CONTRACT_IDENTITY {
        return Err(provider_failure("dockerfile-scanner-source-contract"));
    }
    ScannerScope::begin(cancellation.clone(), |context| context.check().is_err())
        .map_err(|refusal| map_refusal(refusal, cancellation))
}

pub(super) fn finish(
    scope: ScannerScope<Cancellation>,
    cancellation: &Cancellation,
    witness: Option<&mut Option<NativeScannerRefusalDetails>>,
) -> Result<(), AdapterError> {
    let result = scope.finish();
    // Host cancellation wins over any earlier sticky scanner refusal. A
    // cancelled transaction must never become a source-preservation certificate.
    cancellation.check()?;
    match result {
        Ok(()) => Ok(()),
        Err(refusal) => {
            let issued = match refusal {
                ScannerRefusal::Capacity => {
                    Some(NativeScannerRefusalDetails::DockerfileStateCapacity)
                }
                ScannerRefusal::UnsupportedMarker => {
                    Some(NativeScannerRefusalDetails::DockerfileMarkerSemantics)
                }
                _ => None,
            };
            if let (Some(slot), Some(issued)) = (witness, issued) {
                *slot = Some(issued);
            }
            Err(map_refusal(refusal, cancellation))
        }
    }
}

fn map_refusal(refusal: ScannerRefusal, cancellation: &Cancellation) -> AdapterError {
    match refusal {
        ScannerRefusal::Allocation => SinkError::AllocationFailed.into(),
        ScannerRefusal::Capacity => NativeScannerRefusalDetails::DockerfileStateCapacity.error(),
        ScannerRefusal::UnsupportedMarker => {
            NativeScannerRefusalDetails::DockerfileMarkerSemantics.error()
        }
        ScannerRefusal::MalformedState => provider_failure("dockerfile-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("dockerfile-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("dockerfile-scanner-scope-conflict"),
        ScannerRefusal::InvalidScalar => provider_failure("dockerfile-scanner-invalid-scalar"),
        ScannerRefusal::Cancelled => cancellation.check().err().map_or_else(
            || provider_failure("dockerfile-scanner-cancelled"),
            AdapterError::from,
        ),
        ScannerRefusal::CallbackPanicked => {
            provider_failure("dockerfile-scanner-callback-panicked")
        }
        _ => provider_failure("dockerfile-scanner-status-abi"),
    }
}
