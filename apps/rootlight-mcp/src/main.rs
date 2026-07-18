//! Standard-stream entry point for Rootlight's MCP bridge.

#![forbid(unsafe_code)]

use std::{process::ExitCode, sync::Arc};

use rootlight_mcp::{NoopRequestHandler, Session, StdioLimits, serve};
use tokio::io::{BufReader, BufWriter};

#[tokio::main]
async fn main() -> ExitCode {
    let input = BufReader::new(tokio::io::stdin());
    let output = BufWriter::new(tokio::io::stdout());
    let mut session = Session::rootlight();

    match serve(
        input,
        output,
        &mut session,
        Arc::new(NoopRequestHandler),
        StdioLimits::default(),
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rootlight-mcp terminated: {}", error.category());
            ExitCode::FAILURE
        }
    }
}
