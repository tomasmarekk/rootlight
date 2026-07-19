//! Fail-closed process entry point for the generic deep-adapter host.
//!
//! Native execution remains unavailable until every required isolation control
//! has an audited backend; this binary never substitutes an unsandboxed launch.

use std::process::ExitCode;

use rootlight_adapter_host::{AdapterActivation, IsolationReport, evaluate_adapter_activation};

fn main() -> ExitCode {
    let report = IsolationReport::current();
    let message = if evaluate_adapter_activation(&report) == AdapterActivation::StructuralFallback {
        "error: deep adapter isolation backend is unavailable"
    } else {
        "error: deep adapter execution backend is unavailable"
    };
    eprintln!("{message}");
    ExitCode::FAILURE
}
