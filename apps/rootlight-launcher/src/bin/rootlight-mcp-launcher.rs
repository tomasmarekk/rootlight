//! Stable Rootlight MCP launcher process entry point.
//!
//! Windows uses the GUI subsystem so short-lived stdio MCP sessions do not
//! allocate a console host. Redirected standard handles remain available to
//! the launched MCP process.

#![forbid(unsafe_code)]
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match rootlight_launcher::run() {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("rootlight MCP launcher: {error}");
            ExitCode::FAILURE
        }
    }
}
