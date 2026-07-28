//! Fail-closed process entry point for the generic deep-adapter host.
//!
//! Native execution remains unavailable until every required isolation control
//! has an audited backend; this binary never substitutes an unsandboxed launch.

#![forbid(unsafe_code)]

use std::{
    ffi::OsStr,
    io::{self, Write as _},
    process::ExitCode,
};
#[cfg(windows)]
use std::{
    io::Read as _,
    thread,
    time::{Duration, Instant},
};

use rootlight_adapter_host::{
    AdapterActivation, IsolationReport, encode_isolation_report, evaluate_adapter_activation,
};
#[cfg(windows)]
use rootlight_sandbox::{
    AdapterProcessCommand, AdapterSandboxLimits, IsolatedAdapterProcess,
    spawn_windows_isolated_adapter,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next().as_deref() {
        Some(value) if value == OsStr::new("--report") => return report(arguments),
        Some(value) if value == OsStr::new("--isolation-witness") => {
            return isolation_witness(arguments);
        }
        _ => {}
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
    let report = match exact_process_isolation_report() {
        Ok(report) => report,
        Err(()) => {
            eprintln!("error: adapter isolation report could not be observed");
            return ExitCode::FAILURE;
        }
    };
    let encoded = match encode_isolation_report(&report, &source_revision) {
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

fn isolation_witness(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    if arguments.next().is_some() {
        eprintln!("error: adapter isolation witness arguments are invalid");
        return ExitCode::FAILURE;
    }
    if io::stdout()
        .lock()
        .write_all(b"rootlight-isolated\n")
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(windows)]
fn exact_process_isolation_report() -> Result<IsolationReport, ()> {
    let command = AdapterProcessCommand::new(std::env::current_exe().map_err(|_| ())?, 1, 64, 64)
        .map_err(|_| ())?
        .arg("--isolation-witness")
        .map_err(|_| ())?;
    let limits =
        AdapterSandboxLimits::new(128 * 1024 * 1024, Duration::from_secs(5)).map_err(|_| ())?;
    let mut process = spawn_windows_isolated_adapter(command, limits).map_err(|_| ())?;
    drop(process.take_stdin().ok_or(())?);
    let mut stdout = process.take_stdout().ok_or(())?;
    let mut stderr = process.take_stderr().ok_or(())?;
    let mut output = Vec::new();
    stdout.read_to_end(&mut output).map_err(|_| ())?;
    let mut diagnostic = Vec::new();
    stderr.read_to_end(&mut diagnostic).map_err(|_| ())?;
    let status = wait_for_exit(&mut process, Instant::now() + Duration::from_secs(10))?;
    process
        .wait_empty(Instant::now() + Duration::from_secs(2))
        .map_err(|_| ())?;
    if !status.success() || output != b"rootlight-isolated\n" || !diagnostic.is_empty() {
        return Err(());
    }
    Ok(IsolationReport::from_windows_process(process.report()))
}

#[cfg(not(windows))]
fn exact_process_isolation_report() -> Result<IsolationReport, ()> {
    Ok(IsolationReport::current())
}

#[cfg(windows)]
fn wait_for_exit(
    process: &mut IsolatedAdapterProcess,
    deadline: Instant,
) -> Result<std::process::ExitStatus, ()> {
    loop {
        if let Some(status) = process.try_wait().map_err(|_| ())? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            process.terminate().map_err(|_| ())?;
            process
                .wait_empty(Instant::now() + Duration::from_secs(2))
                .map_err(|_| ())?;
            return Err(());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
