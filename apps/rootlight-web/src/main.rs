//! Process entry point for the Rootlight loopback web host.

#![forbid(unsafe_code)]

use std::{env, process::ExitCode};

#[tokio::main]
async fn main() -> ExitCode {
    match rootlight_web::run(env::args_os().skip(1)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rootlight-web: {error}");
            ExitCode::FAILURE
        }
    }
}
