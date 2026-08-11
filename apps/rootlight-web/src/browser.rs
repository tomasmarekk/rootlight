//! Minimal platform browser launcher for the trusted local application URL.

use std::io;

#[cfg(any(windows, unix))]
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(windows)]
const WINDOWS_LAUNCHER_RELATIVE_PATH: &str = "explorer.exe";
#[cfg(target_os = "macos")]
const UNIX_LAUNCHER_PATH: &str = "/usr/bin/open";
#[cfg(all(unix, not(target_os = "macos")))]
const UNIX_LAUNCHER_PATH: &str = "/usr/bin/xdg-open";

/// Asks the operating system to open the trusted loopback URL.
///
/// The URL is passed as one process argument so no shell grammar is involved.
///
/// # Errors
///
/// Returns an I/O error when the trusted platform launcher cannot be resolved
/// or spawned.
#[cfg(any(windows, unix))]
pub(crate) fn open(url: &str) -> io::Result<()> {
    browser_command(url)?.spawn().map(|_| ())
}

#[cfg(any(windows, unix))]
fn browser_command(url: &str) -> io::Result<Command> {
    let mut command = Command::new(browser_launcher()?);
    command.arg(url);
    Ok(command)
}

#[cfg(windows)]
fn browser_launcher() -> io::Result<PathBuf> {
    rootlight_runtime::trusted_windows_system_executable(Path::new(WINDOWS_LAUNCHER_RELATIVE_PATH))
        .map_err(io::Error::other)
}

#[cfg(unix)]
fn browser_launcher() -> io::Result<PathBuf> {
    Ok(PathBuf::from(UNIX_LAUNCHER_PATH))
}

/// Reports an unsupported browser launcher on unrecognized targets.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(any(windows, unix)))]
pub(crate) fn open(_url: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "browser launching is unsupported on this platform",
    ))
}

#[cfg(all(test, any(windows, unix)))]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn browser_command_uses_an_absolute_launcher_and_exact_url_argument() {
        let url = "http://127.0.0.1:43127/#bootstrap=browser-test";
        let command = browser_command(url).expect("trusted browser launcher resolves");

        let program = Path::new(command.get_program());
        assert!(program.is_absolute());
        #[cfg(windows)]
        assert_eq!(program.file_name(), Some(OsStr::new("explorer.exe")));
        #[cfg(target_os = "macos")]
        assert_eq!(program, Path::new("/usr/bin/open"));
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(program, Path::new("/usr/bin/xdg-open"));
        let mut arguments = command.get_args();
        assert_eq!(arguments.next(), Some(OsStr::new(url)));
        assert_eq!(arguments.next(), None);
    }
}
