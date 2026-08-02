//! Minimal platform browser launcher for the trusted local application URL.

use std::{io, process::Command};

#[cfg(target_os = "windows")]
const LAUNCHER: &str = "explorer.exe";
#[cfg(target_os = "macos")]
const LAUNCHER: &str = "open";
#[cfg(all(unix, not(target_os = "macos")))]
const LAUNCHER: &str = "xdg-open";

/// Asks the operating system to open the trusted loopback URL.
///
/// The URL is passed as one process argument so no shell grammar is involved.
///
/// # Errors
///
/// Returns the platform process-spawn error when no browser launcher is
/// available.
#[cfg(any(target_os = "windows", target_os = "macos", unix))]
pub(crate) fn open(url: &str) -> io::Result<()> {
    Command::new(LAUNCHER).arg(url).spawn().map(|_| ())
}

/// Reports an unsupported browser launcher on unrecognized targets.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
pub(crate) fn open(_url: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "browser launching is unsupported on this platform",
    ))
}
