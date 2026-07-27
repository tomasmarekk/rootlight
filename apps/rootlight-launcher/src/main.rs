//! Stable Rootlight launcher process entry point.
//!
//! All state selection and child-process behavior lives in the library crate.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match rootlight_launcher::run() {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("rootlight launcher: {error}");
            ExitCode::FAILURE
        }
    }
}
