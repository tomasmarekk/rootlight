//! Thin process entry point for the opt-in semantic stdio host.
//!
//! The binary exposes no ambient repository or persistence capability; all
//! semantic input and artifact bytes must arrive through the bounded protocol.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match rootlight_semantic_host::serve_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
