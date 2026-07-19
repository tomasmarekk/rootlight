//! Fail-closed process entry point for the generic deep-adapter host.
//!
//! Native execution remains unavailable until every required isolation control
//! has an audited backend; this binary never substitutes an unsandboxed launch.

use std::{
    ffi::OsStr,
    io::{self, Write as _},
    process::ExitCode,
};

use rootlight_adapter_host::{
    AdapterActivation, IsolationReport, encode_isolation_report, evaluate_adapter_activation,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(OsStr::new("--report")) {
        return report(arguments);
    }
    let report = IsolationReport::current();
    let message = if evaluate_adapter_activation(&report) == AdapterActivation::StructuralFallback {
        "error: deep adapter isolation backend is unavailable"
    } else {
        "error: deep adapter execution backend is unavailable"
    };
    eprintln!("{message}");
    ExitCode::FAILURE
}

fn report(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    if arguments.next().as_deref() != Some(OsStr::new("--source-revision")) {
        eprintln!("error: adapter isolation report arguments are invalid");
        return ExitCode::FAILURE;
    }
    let Some(source_revision) = arguments.next().and_then(|value| value.into_string().ok()) else {
        eprintln!("error: adapter isolation report arguments are invalid");
        return ExitCode::FAILURE;
    };
    if arguments.next().is_some() {
        eprintln!("error: adapter isolation report arguments are invalid");
        return ExitCode::FAILURE;
    }
    let encoded = match encode_isolation_report(&IsolationReport::current(), &source_revision) {
        Ok(encoded) => encoded,
        Err(_) => {
            eprintln!("error: adapter isolation report could not be encoded");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = io::stdout().lock();
    if stdout
        .write_all(&encoded)
        .and_then(|()| stdout.write_all(b"\n"))
        .is_err()
    {
        eprintln!("error: adapter isolation report could not be written");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
