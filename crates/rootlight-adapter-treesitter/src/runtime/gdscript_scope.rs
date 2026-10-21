//! Admit GDScript native trees only after the exact checked scanner scope closes.
//! The source identity also invalidates trees cached under an older scanner contract.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use tree_sitter_gdscript::{ScannerRefusal, ScannerScope};

use super::provider_failure;

pub(super) const SCANNER_CONTRACT_IDENTITY: &str = concat!(
    "gdscript-scanner-checked-state/1;parser=",
    "cc18c78720d6dfc8951e3132e1d3abeeb241b1c16c3ecdd54ff8551ad2017f04",
    ";scanner=",
    "9cd19de7e118dee560deb187c4114440da8ab9285121a96dd5c6f22f19b4fee1",
    ";status=",
    "899a5c5e9eec6b2579415af9b3329fe43409cff4b1e71540e4769373bbe47c63",
    ";status-unit=",
    "a95ea4ceec487e2700227ce257b92d5de8eb0bf411211c9da22f15d9eedc8e29",
    ";controls=",
    "c88665cb015b37b064c3e43352e83aee652ebb55fdc5a47a2967b9612afdd0f4",
);

pub(super) fn begin() -> Result<ScannerScope, AdapterError> {
    if tree_sitter_gdscript::SCANNER_CONTRACT_IDENTITY != SCANNER_CONTRACT_IDENTITY {
        return Err(provider_failure("gdscript-scanner-source-contract"));
    }
    ScannerScope::begin().map_err(map_refusal)
}

pub(super) fn finish(scope: ScannerScope) -> Result<(), AdapterError> {
    scope.finish().map_err(map_refusal)
}

fn map_refusal(status: ScannerRefusal) -> AdapterError {
    match status {
        ScannerRefusal::Allocation => SinkError::AllocationFailed.into(),
        ScannerRefusal::Capacity => provider_failure("gdscript-scanner-capacity"),
        ScannerRefusal::MalformedState => provider_failure("gdscript-scanner-malformed-state"),
        ScannerRefusal::ScopeMissing => provider_failure("gdscript-scanner-scope-missing"),
        ScannerRefusal::ScopeConflict => provider_failure("gdscript-scanner-scope-conflict"),
        _ => provider_failure("gdscript-scanner-status-abi"),
    }
}

pub(super) const CACHE_CONTRACT_IDENTITY: &str = "gdscript-scanner-checked-state/1;source=95c9697b0bc33a08774a189231a75cc139500e4bdb415bcbd3fe988532bd184f;parser=cc18c78720d6dfc8951e3132e1d3abeeb241b1c16c3ecdd54ff8551ad2017f04;scanner=9cd19de7e118dee560deb187c4114440da8ab9285121a96dd5c6f22f19b4fee1;status=899a5c5e9eec6b2579415af9b3329fe43409cff4b1e71540e4769373bbe47c63;status-unit=a95ea4ceec487e2700227ce257b92d5de8eb0bf411211c9da22f15d9eedc8e29;controls=c88665cb015b37b064c3e43352e83aee652ebb55fdc5a47a2967b9612afdd0f4";
