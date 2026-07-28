//! One-request isolated adapter process transactions.
//!
//! Source bytes cross only the bounded protocol input pipe. The parent drains
//! both untrusted output streams concurrently, owns the complete process tree,
//! and accepts a result only after protocol and immutable-context validation.

use std::{
    io::{Read as _, Write as _},
    path::Path,
    process::ExitStatus,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ir::{ExtensionSupport, NormalizedIrDocument};
use rootlight_protocol::{
    adapter_contract::{
        ADAPTER_FRAME_PREFIX_BYTES, AdapterFrameDecoder, MAX_ADAPTER_FRAME_BYTES,
        NegotiatedSession, encode_adapter_frame, encode_length_delimited_adapter_frame,
    },
    generated::adapter::v1::{AdapterFrame, ProjectAnalysisRequest, adapter_frame},
};
use rootlight_sandbox::{
    AdapterProcessCommand, AdapterSandboxLimits, AdapterStderr, AdapterStdin, AdapterStdout,
    IsolatedAdapterProcess, spawn_windows_isolated_adapter,
};

use crate::{
    AdapterHostError, IsolationReport, prepare_project_analysis, validate_project_analysis_result,
};

const PROJECT_SESSION_ARGUMENT: &str = "--project-session";
const MAX_ADAPTER_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(2);
const PROCESS_REAP_GRACE: Duration = Duration::from_secs(2);

/// Validated project output and native evidence from the exact producing process.
#[derive(Debug)]
pub struct IsolatedProjectAnalysis {
    document: NormalizedIrDocument,
    isolation: IsolationReport,
}

impl IsolatedProjectAnalysis {
    /// Returns the canonical generation-bound project document.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        &self.document
    }

    /// Returns the evidence attached to the exact producing process.
    #[must_use]
    pub const fn isolation(&self) -> &IsolationReport {
        &self.isolation
    }
}

/// Executes one project request inside the native adapter isolation boundary.
///
/// The adapter executable is copied into an operation-owned immutable runtime
/// directory by the sandbox. Repository paths are never passed through
/// arguments or environment variables, and all source content is confined to
/// the quota-limited standard-input packet.
///
/// # Errors
///
/// Returns [`AdapterHostError`] when admission, native isolation, bounded I/O,
/// cancellation, process completion, frame decoding, or hostile-output
/// validation fails. Adapter diagnostics are never copied into the error.
pub fn execute_isolated_project_adapter(
    executable: &Path,
    session: &NegotiatedSession,
    request: &ProjectAnalysisRequest,
    supported_extensions: &ExtensionSupport,
    cancellation: &Cancellation,
) -> Result<IsolatedProjectAnalysis, AdapterHostError> {
    cancellation.check()?;
    let pending = prepare_project_analysis(session, request, cancellation)?;
    let packet = encode_length_delimited_adapter_frame(&AdapterFrame {
        message: Some(adapter_frame::Message::ProjectAnalysisRequest(
            request.clone(),
        )),
    })?;
    let output_limit = MAX_ADAPTER_FRAME_BYTES
        .checked_add(ADAPTER_FRAME_PREFIX_BYTES)
        .ok_or(AdapterHostError::Limit)?;
    let command = AdapterProcessCommand::new(
        executable,
        packet.len(),
        output_limit,
        MAX_ADAPTER_DIAGNOSTIC_BYTES,
    )
    .map_err(|_| AdapterHostError::Process)?
    .arg(PROJECT_SESSION_ARGUMENT)
    .map_err(|_| AdapterHostError::Process)?;
    let memory_bytes =
        usize::try_from(session.limits().memory_bytes).map_err(|_| AdapterHostError::Limit)?;
    let cpu_time = Duration::from_millis(session.limits().cpu_time_ms);
    let sandbox_limits =
        AdapterSandboxLimits::new(memory_bytes, cpu_time).map_err(|_| AdapterHostError::Process)?;
    let mut process = spawn_windows_isolated_adapter(command, sandbox_limits)
        .map_err(|_| AdapterHostError::Process)?;
    let isolation = IsolationReport::from_windows_process(process.report());

    let stdin = process.take_stdin().ok_or(AdapterHostError::ProcessIo)?;
    let stdout = process.take_stdout().ok_or(AdapterHostError::ProcessIo)?;
    let stderr = process.take_stderr().ok_or(AdapterHostError::ProcessIo)?;
    let output_reader = spawn_reader("rootlight-adapter-stdout", stdout, &process)?;
    let diagnostic_reader = spawn_diagnostic_reader(stderr, &process)?;
    let input_writer = spawn_writer(stdin, packet, &process)?;

    let wall_time = Duration::from_millis(session.limits().wall_time_ms);
    let deadline = Instant::now()
        .checked_add(wall_time)
        .ok_or(AdapterHostError::Limit)?;
    let status = match poll_process(&mut process, deadline, cancellation) {
        Ok(status) => status,
        Err(error) => {
            terminate_and_reap(&process);
            drop(process);
            let _ = join_worker(input_writer);
            let _ = join_worker(output_reader);
            let _ = join_worker(diagnostic_reader);
            return Err(error);
        }
    };
    let input_result = join_worker(input_writer);
    let output_result = join_worker(output_reader);
    let diagnostic_result = join_worker(diagnostic_reader);
    input_result?;
    let output = output_result?;
    let diagnostic = diagnostic_result?;
    process
        .wait_empty(reap_deadline())
        .map_err(|_| AdapterHostError::Process)?;
    cancellation.check()?;
    if !status.success() || !diagnostic.is_empty() {
        return Err(AdapterHostError::ProcessFailed);
    }

    let frame = decode_exact_packet(&output)?;
    let encoded = encode_adapter_frame(&frame)?;
    let document = validate_project_analysis_result(
        session,
        pending,
        &encoded,
        supported_extensions,
        cancellation,
    )?;
    Ok(IsolatedProjectAnalysis {
        document,
        isolation,
    })
}

fn spawn_reader(
    name: &str,
    mut reader: AdapterStdout,
    process: &IsolatedAdapterProcess,
) -> Result<JoinHandle<std::io::Result<Vec<u8>>>, AdapterHostError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut output = Vec::new();
            reader.read_to_end(&mut output)?;
            Ok(output)
        })
        .map_err(|_| {
            terminate_and_reap(process);
            AdapterHostError::ProcessIo
        })
}

fn spawn_diagnostic_reader(
    mut reader: AdapterStderr,
    process: &IsolatedAdapterProcess,
) -> Result<JoinHandle<std::io::Result<Vec<u8>>>, AdapterHostError> {
    thread::Builder::new()
        .name("rootlight-adapter-stderr".to_owned())
        .spawn(move || {
            let mut output = Vec::new();
            reader.read_to_end(&mut output)?;
            Ok(output)
        })
        .map_err(|_| {
            terminate_and_reap(process);
            AdapterHostError::ProcessIo
        })
}

fn spawn_writer(
    mut writer: AdapterStdin,
    packet: Vec<u8>,
    process: &IsolatedAdapterProcess,
) -> Result<JoinHandle<std::io::Result<()>>, AdapterHostError> {
    thread::Builder::new()
        .name("rootlight-adapter-stdin".to_owned())
        .spawn(move || {
            writer.write_all(&packet)?;
            writer.flush()
        })
        .map_err(|_| {
            terminate_and_reap(process);
            AdapterHostError::ProcessIo
        })
}

fn poll_process(
    process: &mut IsolatedAdapterProcess,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<ExitStatus, AdapterHostError> {
    loop {
        if let Err(cancelled) = cancellation.check() {
            return Err(cancelled.into());
        }
        if Instant::now() >= deadline {
            return Err(AdapterHostError::ProcessTimeout);
        }
        if let Some(status) = process.try_wait().map_err(|_| AdapterHostError::Process)? {
            return Ok(status);
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn terminate_and_reap(process: &IsolatedAdapterProcess) {
    let _ = process.terminate();
    let _ = process.wait_empty(reap_deadline());
}

fn reap_deadline() -> Instant {
    Instant::now()
        .checked_add(PROCESS_REAP_GRACE)
        .unwrap_or_else(Instant::now)
}

fn join_worker<T>(worker: JoinHandle<std::io::Result<T>>) -> Result<T, AdapterHostError> {
    worker
        .join()
        .map_err(|_| AdapterHostError::ProcessIo)?
        .map_err(|_| AdapterHostError::ProcessIo)
}

fn decode_exact_packet(packet: &[u8]) -> Result<AdapterFrame, AdapterHostError> {
    let mut decoder = AdapterFrameDecoder::new();
    let (consumed, frame) = decoder.push(packet)?;
    if consumed != packet.len() {
        return Err(AdapterHostError::UnexpectedFrame);
    }
    frame.ok_or(AdapterHostError::UnexpectedFrame)
}

#[cfg(test)]
mod tests {
    use rootlight_protocol::{
        adapter_contract::encode_length_delimited_adapter_frame,
        generated::adapter::v1::{AdapterFrame, ProjectAnalysisResult, adapter_frame},
    };

    use super::*;

    #[test]
    fn exact_packet_decoder_rejects_partial_and_trailing_frames() {
        let frame = AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisResult(
                ProjectAnalysisResult {
                    session_id: vec![1; 16],
                    request_id: vec![2; 16],
                    normalized_ir: vec![3; 8],
                    output_digest: None,
                },
            )),
        };
        let packet = encode_length_delimited_adapter_frame(&frame).expect("fixture frame encodes");
        assert_eq!(
            decode_exact_packet(&packet).expect("one exact packet decodes"),
            frame
        );
        assert!(matches!(
            decode_exact_packet(&packet[..packet.len() - 1]),
            Err(AdapterHostError::UnexpectedFrame)
        ));
        let mut trailing = packet;
        trailing.push(0);
        assert!(matches!(
            decode_exact_packet(&trailing),
            Err(AdapterHostError::UnexpectedFrame)
        ));
    }
}
