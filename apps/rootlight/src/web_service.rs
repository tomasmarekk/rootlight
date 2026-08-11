//! Persistent local Web UI process discovery and authenticated lifecycle control.

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Read as _, Write as _},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use rootlight_client::{
    DetachedProcess, DetachedProcessError, spawn_detached_null_stdio_process_in,
};
#[cfg(windows)]
use rootlight_runtime::trusted_windows_system_executable;
use rootlight_runtime::{RuntimeError, RuntimePaths, WEB_UI_PORT, WebDiscoveryRecord};
use serde::{Deserialize, Serialize};

#[cfg(not(windows))]
type DetachedChild = std::process::Child;
#[cfg(windows)]
type DetachedChild = DetachedProcess;

const ORIGIN: &str = "http://127.0.0.1:43127";
const MAX_HTTP_RESPONSE_BYTES: u64 = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
// Graceful shutdown can close the listener before its 202 response reaches
// the controller, so both control delivery and process absence need consensus.
const STOP_CONFIRMATIONS: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WebServiceStatus {
    pub(crate) schema_version: u16,
    pub(crate) registered: bool,
    pub(crate) running: bool,
    pub(crate) origin: &'static str,
    pub(crate) pid: Option<u32>,
}

impl WebServiceStatus {
    fn stopped() -> Self {
        Self {
            schema_version: 1,
            registered: registration_exists(),
            running: false,
            origin: ORIGIN,
            pid: None,
        }
    }

    fn running(pid: u32) -> Self {
        Self {
            schema_version: 1,
            registered: registration_exists(),
            running: true,
            origin: ORIGIN,
            pid: Some(pid),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WebServiceError {
    #[error("Web UI runtime discovery failed")]
    Runtime(#[from] RuntimeError),
    #[cfg(not(windows))]
    #[error("Web UI process launch failed")]
    Launch(#[source] io::Error),
    #[cfg(windows)]
    #[error("Web UI process launch failed")]
    WindowsLaunch(#[source] DetachedProcessError),
    #[error("Web UI process exited before becoming ready")]
    EarlyExit,
    #[error("Web UI service did not become ready before the startup deadline")]
    StartTimedOut,
    #[error("Web UI service did not stop before the shutdown deadline")]
    StopTimedOut,
    #[error("Web UI service control request failed")]
    Control(#[source] io::Error),
    #[error("Web UI service returned an invalid control response")]
    InvalidResponse,
    #[error("browser launch failed")]
    Browser(#[source] io::Error),
    #[error("Web UI login registration failed")]
    Registration(#[source] io::Error),
    #[error("Web UI service privilege inspection failed")]
    PrivilegeInspection,
    #[error("Web UI service cannot run with elevated privileges")]
    ElevatedExecution,
}

pub(crate) fn status(paths: &RuntimePaths) -> Result<WebServiceStatus, WebServiceError> {
    let Some(record) = discovered_record(paths)? else {
        return Ok(WebServiceStatus::stopped());
    };
    match probe() {
        Ok(pid) if pid == record.pid() => Ok(WebServiceStatus::running(pid)),
        Ok(_) | Err(_) => Ok(WebServiceStatus::stopped()),
    }
}

pub(crate) fn start(
    paths: &RuntimePaths,
    executable: &Path,
) -> Result<WebServiceStatus, WebServiceError> {
    enforce_per_user_service_identity(current_process_is_elevated()?)?;
    start_for_current_user(paths, executable)
}

fn start_for_current_user(
    paths: &RuntimePaths,
    executable: &Path,
) -> Result<WebServiceStatus, WebServiceError> {
    if let Some(record) = live_record(paths)? {
        return Ok(WebServiceStatus::running(record.pid()));
    }
    remove_stale_record(paths)?;
    if registration_exists() && start_registered() {
        return wait_until_started(paths, None);
    }
    paths.prepare_owner()?;
    let mut child = spawn_detached(executable, paths.state_dir())?;
    wait_until_started(paths, Some(&mut child))
}

fn wait_until_started(
    paths: &RuntimePaths,
    mut child: Option<&mut DetachedChild>,
) -> Result<WebServiceStatus, WebServiceError> {
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or(WebServiceError::StartTimedOut)?;
    loop {
        if let Some(record) = live_record(paths)? {
            return Ok(WebServiceStatus::running(record.pid()));
        }
        if let Some(child) = child.as_mut()
            && detached_child_exited(child)?
        {
            return Err(WebServiceError::EarlyExit);
        }
        if Instant::now() >= deadline {
            return Err(WebServiceError::StartTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub(crate) fn stop(paths: &RuntimePaths) -> Result<WebServiceStatus, WebServiceError> {
    let Some(record) = discovered_record(paths)? else {
        return Ok(WebServiceStatus::stopped());
    };
    if !recorded_process_is_live(record.pid()) {
        paths.remove_web_discovery_if_matches(record.instance_nonce())?;
        return Ok(WebServiceStatus::stopped());
    }
    request_shutdown(&record)?;
    let deadline = Instant::now()
        .checked_add(STOP_TIMEOUT)
        .ok_or(WebServiceError::StopTimedOut)?;
    let mut unavailable_observations = 0_u8;
    loop {
        match probe() {
            Ok(pid) if pid == record.pid() => unavailable_observations = 0,
            Ok(_) => return Err(WebServiceError::InvalidResponse),
            Err(_) => {
                unavailable_observations = unavailable_observations.saturating_add(1);
                if unavailable_observations >= STOP_CONFIRMATIONS {
                    paths.remove_web_discovery_if_matches(record.instance_nonce())?;
                    return Ok(WebServiceStatus::stopped());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(WebServiceError::StopTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub(crate) fn restart(
    paths: &RuntimePaths,
    executable: &Path,
) -> Result<WebServiceStatus, WebServiceError> {
    enforce_per_user_service_identity(current_process_is_elevated()?)?;
    stop(paths)?;
    start_for_current_user(paths, executable)
}

pub(crate) fn install(
    paths: &RuntimePaths,
    executable: &Path,
) -> Result<WebServiceStatus, WebServiceError> {
    // Unix login managers may activate a new registration immediately without
    // this process's runtime overrides, so establish the current session first.
    let was_running = status(paths)?.running;
    start(paths, executable)?;
    if let Err(error) = register_autostart(executable) {
        if !was_running {
            let _ = stop(paths);
        }
        return Err(error);
    }
    status(paths)
}

pub(crate) fn uninstall(paths: &RuntimePaths) -> Result<WebServiceStatus, WebServiceError> {
    stop(paths)?;
    unregister_autostart()?;
    Ok(WebServiceStatus::stopped())
}

pub(crate) fn open_browser() -> Result<(), WebServiceError> {
    #[cfg(target_os = "windows")]
    const LAUNCHER: &str = "explorer.exe";
    #[cfg(target_os = "macos")]
    const LAUNCHER: &str = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    const LAUNCHER: &str = "xdg-open";

    Command::new(LAUNCHER)
        .arg(ORIGIN)
        .spawn()
        .map(|_| ())
        .map_err(WebServiceError::Browser)
}

fn enforce_per_user_service_identity(is_elevated: bool) -> Result<(), WebServiceError> {
    // Loopback clients share the service process's filesystem authority, so an
    // elevated service would expose that authority across the local-user boundary.
    if is_elevated {
        return Err(WebServiceError::ElevatedExecution);
    }
    Ok(())
}

#[cfg(unix)]
fn current_process_is_elevated() -> Result<bool, WebServiceError> {
    Ok(rustix::process::geteuid().is_root())
}

#[cfg(windows)]
fn current_process_is_elevated() -> Result<bool, WebServiceError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;

    OwnedToken::from_current_process(TOKEN_QUERY)
        .map_err(|_| WebServiceError::PrivilegeInspection)?
        .is_elevated()
        .map_err(|_| WebServiceError::PrivilegeInspection)
}

#[cfg(all(not(unix), not(windows)))]
fn current_process_is_elevated() -> Result<bool, WebServiceError> {
    Err(WebServiceError::PrivilegeInspection)
}

fn live_record(paths: &RuntimePaths) -> Result<Option<WebDiscoveryRecord>, WebServiceError> {
    let Some(record) = discovered_record(paths)? else {
        return Ok(None);
    };
    Ok((probe().ok() == Some(record.pid())).then_some(record))
}

fn remove_stale_record(paths: &RuntimePaths) -> Result<(), WebServiceError> {
    if let Some(record) = discovered_record(paths)? {
        paths.remove_web_discovery_if_matches(record.instance_nonce())?;
    }
    Ok(())
}

fn discovered_record(paths: &RuntimePaths) -> Result<Option<WebDiscoveryRecord>, WebServiceError> {
    match paths.discover_web() {
        Ok(record) => Ok(Some(record)),
        Err(RuntimeError::Io(source)) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn spawn_detached(
    executable: &Path,
    current_directory: &Path,
) -> Result<DetachedChild, WebServiceError> {
    spawn_detached_null_stdio_process_in(executable, &["--service"], Some(current_directory))
        .map_err(WebServiceError::WindowsLaunch)
}

#[cfg(not(windows))]
fn spawn_detached(
    executable: &Path,
    current_directory: &Path,
) -> Result<DetachedChild, WebServiceError> {
    let mut command = Command::new(executable);
    command
        .arg("--service")
        .current_dir(current_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
    command.spawn().map_err(WebServiceError::Launch)
}

#[cfg(windows)]
fn detached_child_exited(child: &mut DetachedChild) -> Result<bool, WebServiceError> {
    child
        .try_wait()
        .map(|status| status.is_some())
        .map_err(WebServiceError::WindowsLaunch)
}

#[cfg(not(windows))]
fn detached_child_exited(child: &mut DetachedChild) -> Result<bool, WebServiceError> {
    child
        .try_wait()
        .map(|status| status.is_some())
        .map_err(WebServiceError::Launch)
}

fn probe() -> Result<u32, WebServiceError> {
    let response = request("GET", "/api/v1/service/status", None)?;
    if response.status != 200 {
        return Err(WebServiceError::InvalidResponse);
    }
    let status: ProbeResponse =
        serde_json::from_slice(&response.body).map_err(|_| WebServiceError::InvalidResponse)?;
    if !status.ready {
        return Err(WebServiceError::InvalidResponse);
    }
    Ok(status.pid)
}

fn shutdown(record: &WebDiscoveryRecord) -> Result<(), WebServiceError> {
    let response = request(
        "POST",
        "/api/v1/service/shutdown",
        Some(record.control_token()),
    )?;
    if response.status != 202 {
        return Err(WebServiceError::InvalidResponse);
    }
    Ok(())
}

fn request_shutdown(record: &WebDiscoveryRecord) -> Result<(), WebServiceError> {
    let mut last_error = None;
    for attempt in 0..STOP_CONFIRMATIONS {
        match shutdown(record) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < STOP_CONFIRMATIONS {
            thread::sleep(POLL_INTERVAL);
        }
    }
    if recorded_process_is_live(record.pid()) {
        Err(last_error.unwrap_or(WebServiceError::InvalidResponse))
    } else {
        Ok(())
    }
}

fn recorded_process_is_live(pid: u32) -> bool {
    for attempt in 0..STOP_CONFIRMATIONS {
        match probe() {
            Ok(observed_pid) => return observed_pid == pid,
            Err(_) if attempt + 1 < STOP_CONFIRMATIONS => thread::sleep(POLL_INTERVAL),
            Err(_) => return false,
        }
    }
    false
}

fn request(
    method: &str,
    path: &str,
    control_token: Option<&str>,
) -> Result<HttpResponse, WebServiceError> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, WEB_UI_PORT);
    let mut stream = TcpStream::connect_timeout(&address.into(), CONNECT_TIMEOUT)
        .map_err(WebServiceError::Control)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(WebServiceError::Control)?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{WEB_UI_PORT}\r\n\
         Sec-Fetch-Site: same-origin\r\nConnection: close\r\n"
    );
    if let Some(token) = control_token {
        use std::fmt::Write as _;
        write!(
            request,
            "Origin: {ORIGIN}\r\nX-Rootlight-Service-Token: {token}\r\nContent-Length: 0\r\n"
        )
        .map_err(|_| WebServiceError::InvalidResponse)?;
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(WebServiceError::Control)?;
    let mut bytes = Vec::new();
    stream
        .take(MAX_HTTP_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(WebServiceError::Control)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_HTTP_RESPONSE_BYTES) {
        return Err(WebServiceError::InvalidResponse);
    }
    parse_http_response(bytes)
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

fn parse_http_response(mut bytes: Vec<u8>) -> Result<HttpResponse, WebServiceError> {
    let separator = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(WebServiceError::InvalidResponse)?;
    let status_end = bytes
        .windows(2)
        .position(|window| window == b"\r\n")
        .ok_or(WebServiceError::InvalidResponse)?;
    let status_line =
        std::str::from_utf8(&bytes[..status_end]).map_err(|_| WebServiceError::InvalidResponse)?;
    let status = status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|value| value.get(..3))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(WebServiceError::InvalidResponse)?;
    let body_start = separator
        .checked_add(4)
        .ok_or(WebServiceError::InvalidResponse)?;
    let body = bytes.split_off(body_start);
    Ok(HttpResponse { status, body })
}

#[cfg(windows)]
const WINDOWS_TASK_NAME: &str = "Rootlight Web UI";
#[cfg(windows)]
const WINDOWS_STARTUP_FILE: &str = "Rootlight Web UI.vbs";
#[cfg(target_os = "linux")]
const LINUX_UNIT_NAME: &str = "rootlight-web.service";
#[cfg(target_os = "linux")]
const LINUX_DESKTOP_FILE: &str = "rootlight-web.desktop";
#[cfg(target_os = "macos")]
const MACOS_LABEL: &str = "dev.tomasmarekk.rootlight.web";

#[cfg(windows)]
fn register_autostart(executable: &Path) -> Result<(), WebServiceError> {
    const SCRIPT: &str = "$identity=[System.Security.Principal.WindowsIdentity]::GetCurrent().Name;\
        $action=New-ScheduledTaskAction -Execute $env:ROOTLIGHT_SERVICE_EXECUTABLE \
        -Argument '--service';\
        $trigger=New-ScheduledTaskTrigger -AtLogOn -User $identity;\
        $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) \
        -MultipleInstances IgnoreNew -RestartCount 3 \
        -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable \
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries;\
        $principal=New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive \
        -RunLevel Limited;\
        Register-ScheduledTask -TaskName 'Rootlight Web UI' -Action $action -Trigger $trigger \
        -Settings $settings -Principal $principal -Force | Out-Null";
    if powershell(SCRIPT, Some(executable.as_os_str())).is_ok() {
        remove_file_if_present(&windows_startup_path()?)?;
        return Ok(());
    }
    let executable = executable.to_str().ok_or_else(|| {
        WebServiceError::Registration(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service executable path is not Unicode",
        ))
    })?;
    let script = format!(
        "CreateObject(\"Wscript.Shell\").Run Chr(34) & \"{}\" & Chr(34) & \" --service\", 0, False\r\n",
        executable.replace('"', "\"\"")
    );
    write_registration_file(&windows_startup_path()?, script.as_bytes())
}

#[cfg(windows)]
fn start_registered() -> bool {
    // Task Scheduler provides login durability; immediate launches need the
    // explicit child-handle allowlist enforced by `spawn_detached`.
    false
}

#[cfg(windows)]
fn unregister_autostart() -> Result<(), WebServiceError> {
    const SCRIPT: &str = "$task=Get-ScheduledTask -TaskName 'Rootlight Web UI' \
        -ErrorAction SilentlyContinue;if($null -ne $task){\
        Unregister-ScheduledTask -TaskName 'Rootlight Web UI' -Confirm:$false}";
    let _ = powershell(SCRIPT, None);
    remove_file_if_present(&windows_startup_path()?)
}

#[cfg(windows)]
fn registration_exists() -> bool {
    windows_task_exists() || windows_startup_path().is_ok_and(|path| path.is_file())
}

#[cfg(windows)]
fn windows_task_exists() -> bool {
    let Ok(schtasks) = trusted_windows_system_executable(Path::new(r"System32\schtasks.exe"))
    else {
        return false;
    };
    let mut command = Command::new(schtasks);
    command
        .args(["/Query", "/TN", WINDOWS_TASK_NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_windows_command(&mut command);
    command.status().is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn windows_startup_path() -> Result<PathBuf, WebServiceError> {
    let app_data = required_absolute_env_path("APPDATA")?;
    Ok(app_data
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join(WINDOWS_STARTUP_FILE))
}

#[cfg(windows)]
fn powershell(script: &str, executable: Option<&std::ffi::OsStr>) -> Result<(), WebServiceError> {
    let powershell = trusted_windows_system_executable(Path::new(
        r"System32\WindowsPowerShell\v1.0\powershell.exe",
    ))?;
    let mut command = Command::new(powershell);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(executable) = executable {
        command.env("ROOTLIGHT_SERVICE_EXECUTABLE", executable);
    }
    hide_windows_command(&mut command);
    let status = command.status().map_err(WebServiceError::Registration)?;
    if status.success() {
        Ok(())
    } else {
        Err(WebServiceError::Registration(io::Error::other(
            "PowerShell service registration failed",
        )))
    }
}

#[cfg(windows)]
fn hide_windows_command(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    command.creation_flags(CREATE_NO_WINDOW.0);
}

#[cfg(target_os = "linux")]
fn register_autostart(executable: &Path) -> Result<(), WebServiceError> {
    let config = linux_config_home()?;
    let unit_path = config.join("systemd").join("user").join(LINUX_UNIT_NAME);
    let executable = executable.to_str().ok_or_else(|| {
        WebServiceError::Registration(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service executable path is not Unicode",
        ))
    })?;
    let unit = format!(
        "[Unit]\nDescription=Rootlight local Web UI\nAfter=graphical-session.target\n\n\
         [Service]\nType=simple\nExecStart={} --service\nRestart=on-failure\nRestartSec=2\n\n\
         [Install]\nWantedBy=default.target\n",
        systemd_quote(executable)
    );
    write_registration_file(&unit_path, unit.as_bytes())?;
    enable_linux_unit(&unit_path)?;

    let desktop_path = config.join("autostart").join(LINUX_DESKTOP_FILE);
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Rootlight\nComment=Rootlight local Web UI\n\
         Exec={} --service\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_quote(executable)
    );
    write_registration_file(&desktop_path, desktop.as_bytes())?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn start_registered() -> bool {
    // A systemd user manager does not inherit per-install state/runtime
    // overrides. Start this session directly and keep the unit for login.
    false
}

#[cfg(target_os = "linux")]
fn unregister_autostart() -> Result<(), WebServiceError> {
    let config = linux_config_home()?;
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", LINUX_UNIT_NAME])
        .status();
    remove_linux_unit_link(&config)?;
    remove_file_if_present(&config.join("systemd").join("user").join(LINUX_UNIT_NAME))?;
    remove_file_if_present(&config.join("autostart").join(LINUX_DESKTOP_FILE))?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn registration_exists() -> bool {
    linux_config_home().is_ok_and(|config| {
        config
            .join("systemd")
            .join("user")
            .join(LINUX_UNIT_NAME)
            .is_file()
            || config.join("autostart").join(LINUX_DESKTOP_FILE).is_file()
    })
}

#[cfg(target_os = "linux")]
fn linux_config_home() -> Result<PathBuf, WebServiceError> {
    match env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => checked_absolute_path(value),
        _ => Ok(required_absolute_env_path("HOME")?.join(".config")),
    }
}

#[cfg(target_os = "linux")]
fn enable_linux_unit(unit_path: &Path) -> Result<(), WebServiceError> {
    let config = linux_config_home()?;
    let wants = config
        .join("systemd")
        .join("user")
        .join("default.target.wants");
    fs::create_dir_all(&wants).map_err(WebServiceError::Registration)?;
    let link = wants.join(LINUX_UNIT_NAME);
    let expected = Path::new("..").join(LINUX_UNIT_NAME);
    match fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if fs::read_link(&link).map_err(WebServiceError::Registration)? != expected {
                return Err(WebServiceError::Registration(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "foreign systemd unit link exists",
                )));
            }
        }
        Ok(_) => {
            return Err(WebServiceError::Registration(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "foreign systemd unit link exists",
            )));
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            use std::os::unix::fs::symlink;
            symlink(&expected, &link).map_err(WebServiceError::Registration)?;
        }
        Err(source) => return Err(WebServiceError::Registration(source)),
    }
    if !unit_path.is_file() {
        return Err(WebServiceError::Registration(io::Error::new(
            io::ErrorKind::NotFound,
            "systemd unit is unavailable",
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn remove_linux_unit_link(config: &Path) -> Result<(), WebServiceError> {
    let link = config
        .join("systemd")
        .join("user")
        .join("default.target.wants")
        .join(LINUX_UNIT_NAME);
    match fs::symlink_metadata(&link) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                && fs::read_link(&link).map_err(WebServiceError::Registration)?
                    == Path::new("..").join(LINUX_UNIT_NAME) =>
        {
            fs::remove_file(link).map_err(WebServiceError::Registration)
        }
        Ok(_) => Err(WebServiceError::Registration(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "foreign systemd unit link cannot be removed",
        ))),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(WebServiceError::Registration(source)),
    }
}

#[cfg(target_os = "macos")]
fn register_autostart(executable: &Path) -> Result<(), WebServiceError> {
    let executable = executable.to_str().ok_or_else(|| {
        WebServiceError::Registration(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service executable path is not Unicode",
        ))
    })?;
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n<key>Label</key><string>{MACOS_LABEL}</string>\n\
         <key>ProgramArguments</key><array><string>{}</string><string>--service</string></array>\n\
         <key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict>\
         <key>SuccessfulExit</key><false/></dict>\n<key>ThrottleInterval</key><integer>5</integer>\n\
         <key>StandardOutPath</key><string>/dev/null</string>\n\
         <key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        xml_escape(executable)
    );
    let path = macos_launch_agent_path()?;
    write_registration_file(&path, plist.as_bytes())
}

#[cfg(target_os = "macos")]
fn start_registered() -> bool {
    // LaunchAgents provide login durability. The CLI starts the current
    // session directly because headless macOS sessions lack a usable GUI
    // launchd domain even though the registration remains valid for login.
    false
}

#[cfg(target_os = "macos")]
fn unregister_autostart() -> Result<(), WebServiceError> {
    let path = macos_launch_agent_path()?;
    if let Ok(domain) = macos_launch_domain() {
        let _ = Command::new("launchctl")
            .args(["bootout", &domain])
            .arg(&path)
            .status();
    }
    remove_file_if_present(&path)
}

#[cfg(target_os = "macos")]
fn registration_exists() -> bool {
    macos_launch_agent_path().is_ok_and(|path| path.is_file())
}

#[cfg(target_os = "macos")]
fn macos_launch_agent_path() -> Result<PathBuf, WebServiceError> {
    Ok(required_absolute_env_path("HOME")?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MACOS_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn macos_launch_domain() -> Result<String, WebServiceError> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(WebServiceError::Registration)?;
    if !output.status.success() {
        return Err(WebServiceError::Registration(io::Error::other(
            "current user identifier is unavailable",
        )));
    }
    let uid = std::str::from_utf8(&output.stdout)
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| {
            WebServiceError::Registration(io::Error::new(
                io::ErrorKind::InvalidData,
                "current user identifier is invalid",
            ))
        })?;
    Ok(format!("gui/{uid}"))
}

fn write_registration_file(path: &Path, bytes: &[u8]) -> Result<(), WebServiceError> {
    let parent = path.parent().ok_or_else(|| {
        WebServiceError::Registration(io::Error::new(
            io::ErrorKind::InvalidInput,
            "registration path has no parent",
        ))
    })?;
    fs::create_dir_all(parent).map_err(WebServiceError::Registration)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return Err(WebServiceError::Registration(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "registration path is not a regular file",
        )));
    }
    fs::write(path, bytes).map_err(WebServiceError::Registration)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(WebServiceError::Registration)?;
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<(), WebServiceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(path).map_err(WebServiceError::Registration)
        }
        Ok(_) => Err(WebServiceError::Registration(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "foreign registration resource cannot be removed",
        ))),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(WebServiceError::Registration(source)),
    }
}

fn required_absolute_env_path(name: &str) -> Result<PathBuf, WebServiceError> {
    let value = env::var_os(name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            WebServiceError::Registration(io::Error::new(
                io::ErrorKind::NotFound,
                "required user directory is unavailable",
            ))
        })?;
    checked_absolute_path(value)
}

fn checked_absolute_path(value: OsString) -> Result<PathBuf, WebServiceError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(WebServiceError::Registration(io::Error::new(
            io::ErrorKind::InvalidInput,
            "user directory is not absolute",
        )))
    }
}

#[cfg(target_os = "linux")]
fn systemd_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

#[cfg(target_os = "linux")]
fn desktop_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeResponse {
    ready: bool,
    pid: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_parser_is_bounded_and_rejects_malformed_status() {
        let response = parse_http_response(
            b"HTTP/1.1 200 OK\r\ncontent-length: 22\r\n\r\n{\"ready\":true,\"pid\":7}".to_vec(),
        )
        .expect("valid response parses");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{\"ready\":true,\"pid\":7}");
        assert!(parse_http_response(b"HTTP/1.0 200 OK\r\n\r\n{}".to_vec()).is_err());
        assert!(parse_http_response(b"HTTP/1.1 nope\r\n\r\n{}".to_vec()).is_err());
    }

    #[test]
    fn stopped_status_uses_the_stable_plain_origin() {
        assert_eq!(
            WebServiceStatus::stopped(),
            WebServiceStatus {
                schema_version: 1,
                registered: registration_exists(),
                running: false,
                origin: "http://127.0.0.1:43127",
                pid: None,
            }
        );
    }

    #[test]
    fn per_user_service_policy_rejects_elevated_processes() {
        assert!(matches!(
            enforce_per_user_service_identity(true),
            Err(WebServiceError::ElevatedExecution)
        ));
        assert!(enforce_per_user_service_identity(false).is_ok());
    }
}
