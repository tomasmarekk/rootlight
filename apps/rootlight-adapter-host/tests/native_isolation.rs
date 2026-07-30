//! Exact-process checks for the native deep-adapter sandbox.

#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use std::{
    io::Read as _,
    path::PathBuf,
    time::{Duration, Instant},
};

use rootlight_sandbox::{AdapterProcessCommand, AdapterSandboxLimits, spawn_isolated_adapter};

#[test]
fn exact_native_process_reports_every_required_control() {
    let command = AdapterProcessCommand::new(adapter_executable(), 1, 64, 4_096)
        .expect("stream limits validate")
        .arg("--isolation-witness")
        .expect("witness argument validates");
    let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(5))
        .expect("native limits validate");
    let mut process =
        spawn_isolated_adapter(command, limits).expect("native isolation is established");
    drop(process.take_stdin().expect("witness stdin is present"));
    let mut stdout = process.take_stdout().expect("witness stdout is present");
    let mut stderr = process.take_stderr().expect("witness stderr is present");
    let mut output = Vec::new();
    stdout
        .read_to_end(&mut output)
        .expect("witness output is readable");
    let mut diagnostics = Vec::new();
    stderr
        .read_to_end(&mut diagnostics)
        .expect("witness diagnostics are readable");
    let status = wait_for_exit(&mut process);

    assert!(
        status.success(),
        "native witness failed with {status:?}: {}",
        String::from_utf8_lossy(&diagnostics)
    );
    assert_eq!(output, b"rootlight-isolated\n");
    assert!(diagnostics.is_empty());
    assert!(process.report().permits_deep_adapter());
    process
        .wait_empty(Instant::now() + Duration::from_secs(2))
        .expect("native process scope is empty");
}

#[test]
fn native_profile_denies_filesystem_network_and_process_creation() {
    for mode in ["write", "child"] {
        let (status, output, diagnostics) = run_adversary(mode);
        assert!(
            status.success(),
            "{mode} adversary failed: {}",
            String::from_utf8_lossy(&diagnostics)
        );
        assert_eq!(output, b"denied\n", "{mode} was not denied");
        assert!(diagnostics.is_empty(), "{mode} emitted diagnostics");
    }

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("parent listener binds");
    listener
        .set_nonblocking(true)
        .expect("parent listener becomes nonblocking");
    let port = listener
        .local_addr()
        .expect("parent listener address reads")
        .port()
        .to_string();
    let command = adversary_command("network")
        .arg(port)
        .expect("network target argument validates");
    let (status, output, diagnostics) = run_adversary_command(command);
    assert!(
        status.success(),
        "network adversary failed: {}",
        String::from_utf8_lossy(&diagnostics)
    );
    assert_eq!(output, b"denied\n");
    assert!(diagnostics.is_empty());
    assert!(
        listener.accept().is_err(),
        "sandbox reached parent listener"
    );
}

#[cfg(unix)]
#[test]
fn native_profile_denies_signalling_the_parent() {
    let (status, output, diagnostics) = run_adversary("signal-parent");
    assert!(
        status.success(),
        "signal adversary failed: {}",
        String::from_utf8_lossy(&diagnostics)
    );
    assert_eq!(output, b"denied\n");
    assert!(diagnostics.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn native_launcher_closes_ambient_inherited_descriptors() {
    use std::fs::File;
    use std::os::fd::AsFd as _;

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    let ambient = File::open("/dev/null").expect("ambient descriptor opens");
    fcntl(ambient.as_fd(), FcntlArg::F_SETFD(FdFlag::empty()))
        .expect("ambient descriptor is intentionally inheritable");
    let command = adversary_command("hold");
    let limits = sandbox_limits();
    let mut process =
        spawn_isolated_adapter(command, limits).expect("native isolation is established");
    let stdin = process.take_stdin().expect("hold stdin is present");
    let mut stdout = process.take_stdout().expect("hold stdout is present");
    let mut stderr = process.take_stderr().expect("hold stderr is present");
    let mut ready = [0_u8; 6];
    stdout
        .read_exact(&mut ready)
        .expect("hold readiness is readable");
    assert_eq!(&ready, b"ready\n");

    let descriptors = std::fs::read_dir(format!("/proc/{}/fd", process.id()))
        .expect("live child descriptors are visible")
        .map(|entry| {
            entry
                .expect("descriptor entry is readable")
                .file_name()
                .into_string()
                .expect("descriptor name is numeric")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        descriptors.len(),
        3,
        "unexpected descriptors: {descriptors:?}"
    );
    assert!(
        ["0", "1", "2"]
            .iter()
            .all(|fd| descriptors.iter().any(|entry| entry == fd))
    );

    drop(stdin);
    let status = wait_for_exit(&mut process);
    let mut diagnostics = Vec::new();
    stderr
        .read_to_end(&mut diagnostics)
        .expect("hold diagnostics are readable");
    assert!(status.success());
    assert!(diagnostics.is_empty());
    drop(ambient);
}

fn run_adversary(mode: &str) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let command = adversary_command(mode);
    run_adversary_command(command)
}

fn run_adversary_command(
    command: AdapterProcessCommand,
) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let mut process =
        spawn_isolated_adapter(command, sandbox_limits()).expect("native isolation is established");
    drop(process.take_stdin().expect("adversary stdin is present"));
    let mut stdout = process.take_stdout().expect("adversary stdout is present");
    let mut stderr = process.take_stderr().expect("adversary stderr is present");
    let mut output = Vec::new();
    stdout
        .read_to_end(&mut output)
        .expect("adversary output is readable");
    let mut diagnostics = Vec::new();
    stderr
        .read_to_end(&mut diagnostics)
        .expect("adversary diagnostics are readable");
    (wait_for_exit(&mut process), output, diagnostics)
}

fn adversary_command(mode: &str) -> AdapterProcessCommand {
    AdapterProcessCommand::new(adapter_executable(), 1, 64, 4_096)
        .expect("stream limits validate")
        .arg("--isolation-adversary")
        .and_then(|command| command.arg(mode))
        .expect("adversary arguments validate")
}

fn sandbox_limits() -> AdapterSandboxLimits {
    AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(5))
        .expect("native limits validate")
}

fn adapter_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rootlight-adapter-host"))
}

fn wait_for_exit(
    process: &mut rootlight_sandbox::IsolatedAdapterProcess,
) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = process.try_wait().expect("process status is readable") {
            return status;
        }
        assert!(Instant::now() < deadline, "native witness timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
