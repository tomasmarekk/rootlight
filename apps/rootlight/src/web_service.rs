//! Persistent local Web UI process discovery and authenticated lifecycle control.

use std::{
    io::{self, Read as _, Write as _},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_runtime::{RuntimeError, RuntimePaths, WEB_UI_PORT, WebDiscoveryRecord};
use serde::{Deserialize, Serialize};

const ORIGIN: &str = "http://127.0.0.1:43127";
const MAX_HTTP_RESPONSE_BYTES: u64 = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WebServiceStatus {
    pub(crate) schema_version: u16,
    pub(crate) running: bool,
    pub(crate) origin: &'static str,
    pub(crate) pid: Option<u32>,
}

impl WebServiceStatus {
    fn stopped() -> Self {
        Self {
            schema_version: 1,
            running: false,
            origin: ORIGIN,
            pid: None,
        }
    }

    fn running(pid: u32) -> Self {
        Self {
            schema_version: 1,
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
    #[error("Web UI process launch failed")]
    Launch(#[source] io::Error),
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
    if let Some(record) = live_record(paths)? {
        return Ok(WebServiceStatus::running(record.pid()));
    }
    remove_stale_record(paths)?;
    let mut child = spawn_detached(executable)?;
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or(WebServiceError::StartTimedOut)?;
    loop {
        if let Some(record) = live_record(paths)? {
            return Ok(WebServiceStatus::running(record.pid()));
        }
        if child.try_wait().map_err(WebServiceError::Launch)?.is_some() {
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
    if probe().ok() != Some(record.pid()) {
        paths.remove_web_discovery_if_matches(record.instance_nonce())?;
        return Ok(WebServiceStatus::stopped());
    }
    shutdown(&record)?;
    let deadline = Instant::now()
        .checked_add(STOP_TIMEOUT)
        .ok_or(WebServiceError::StopTimedOut)?;
    loop {
        if probe().is_err() {
            paths.remove_web_discovery_if_matches(record.instance_nonce())?;
            return Ok(WebServiceStatus::stopped());
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
    stop(paths)?;
    start(paths, executable)
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

fn spawn_detached(executable: &Path) -> Result<Child, WebServiceError> {
    let mut command = Command::new(executable);
    command
        .arg("--service")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        use windows::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        };
        command.creation_flags((CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | DETACHED_PROCESS).0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn().map_err(WebServiceError::Launch)
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
                running: false,
                origin: "http://127.0.0.1:43127",
                pid: None,
            }
        );
    }
}
