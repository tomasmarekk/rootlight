//! Fail-closed process entry point for the generic deep-adapter host.
//!
//! The project-session mode accepts only bounded stdin source under native
//! isolation. Other deep-adapter modes remain unavailable until audited.

#![forbid(unsafe_code)]

use std::{
    ffi::OsStr,
    io::{self, Write as _},
    process::ExitCode,
};
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::{
    io::Read as _,
    thread,
    time::{Duration, Instant},
};

use rootlight_adapter_host::{
    AdapterActivation, IsolationReport, encode_isolation_report, evaluate_adapter_activation,
    run_project_session,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rootlight_sandbox::enter_isolated_adapter_launcher;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use rootlight_sandbox::{
    AdapterProcessCommand, AdapterSandboxLimits, IsolatedAdapterProcess, spawn_isolated_adapter,
};

fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let arguments = if arguments.first().map(std::ffi::OsString::as_os_str)
        == Some(OsStr::new("--rootlight-native-isolation-launcher"))
    {
        let mut launcher_arguments = arguments.into_iter();
        launcher_arguments.next();
        match enter_isolated_adapter_launcher(launcher_arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                eprintln!("error: native isolation launcher failed: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        arguments
    };
    dispatch(arguments.into_iter())
}

fn dispatch(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    match arguments.next().as_deref() {
        Some(value) if value == OsStr::new("--report") => return report(arguments),
        Some(value) if value == OsStr::new("--isolation-witness") => {
            return isolation_witness(arguments);
        }
        Some(value) if value == OsStr::new("--isolation-adversary") => {
            return isolation_adversary(arguments);
        }
        Some(value) if value == OsStr::new("--project-session") => {
            return project_session(arguments);
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

fn project_session(arguments: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    if let Err(error) = run_project_session(arguments) {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
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

fn isolation_adversary(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let Some(mode) = arguments.next() else {
        return ExitCode::FAILURE;
    };
    let extra = arguments.next();
    if arguments.next().is_some() {
        return ExitCode::FAILURE;
    }
    let denied = if mode == OsStr::new("write") {
        extra.is_none() && std::fs::write("forbidden-output", b"forbidden").is_err()
    } else if mode == OsStr::new("network") {
        extra
            .and_then(|port| port.into_string().ok())
            .and_then(|port| port.parse::<u16>().ok())
            .is_some_and(|port| {
                std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                    std::time::Duration::from_millis(250),
                )
                .is_err()
            })
    } else if mode == OsStr::new("child") {
        extra.is_none()
            && std::process::Command::new(
                std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("missing")),
            )
            .arg("--isolation-witness")
            .status()
            .is_err()
    } else if mode == OsStr::new("signal-parent") {
        extra.is_none() && parent_signal_is_denied()
    } else if mode == OsStr::new("memory") {
        return if extra.is_none() {
            exhaust_memory_limit()
        } else {
            ExitCode::FAILURE
        };
    } else if mode == OsStr::new("self-exec") {
        #[cfg(target_os = "macos")]
        {
            extra.is_none() && self_exec_is_denied()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    } else if mode == OsStr::new("hold") {
        return if extra.is_none() {
            hold_isolated_process()
        } else {
            ExitCode::FAILURE
        };
    } else {
        return ExitCode::FAILURE;
    };
    if denied && io::stdout().lock().write_all(b"denied\n").is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(target_os = "macos")]
fn self_exec_is_denied() -> bool {
    use std::os::unix::process::CommandExt as _;

    let executable =
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("missing"));
    std::process::Command::new(executable)
        .arg("--isolation-witness")
        .exec()
        .kind()
        == io::ErrorKind::NotFound
}

fn exhaust_memory_limit() -> ExitCode {
    const BLOCK_BYTES: usize = 1024 * 1024;
    const BLOCKS: usize = 512;

    let mut allocations = Vec::with_capacity(BLOCKS);
    for index in 0..BLOCKS {
        // A nonzero fill commits each block instead of reserving only virtual
        // address space, so the native physical/committed-memory limit decides.
        let fill = u8::try_from(index % 251 + 1).unwrap_or(u8::MAX);
        allocations.push(vec![fill; BLOCK_BYTES]);
    }
    std::hint::black_box(&allocations);
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn parent_signal_is_denied() -> bool {
    nix::sys::signal::kill(nix::unistd::getppid(), None).is_err()
}

#[cfg(not(unix))]
const fn parent_signal_is_denied() -> bool {
    false
}

fn hold_isolated_process() -> ExitCode {
    if io::stdout()
        .lock()
        .write_all(b"ready\n")
        .and_then(|()| io::stdout().lock().flush())
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    let mut byte = [0_u8; 1];
    match std::io::Read::read(&mut io::stdin().lock(), &mut byte) {
        Ok(0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn exact_process_isolation_report() -> Result<IsolationReport, ()> {
    let command = AdapterProcessCommand::new(std::env::current_exe().map_err(|_| ())?, 1, 64, 64)
        .map_err(|_| ())?
        .arg("--isolation-witness")
        .map_err(|_| ())?;
    let limits =
        AdapterSandboxLimits::new(128 * 1024 * 1024, Duration::from_secs(5)).map_err(|_| ())?;
    let mut process = spawn_isolated_adapter(command, limits).map_err(|_| ())?;
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
    Ok(IsolationReport::from_process(process.report()))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn exact_process_isolation_report() -> Result<IsolationReport, ()> {
    Ok(IsolationReport::current())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
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
