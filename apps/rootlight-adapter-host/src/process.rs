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
use rootlight_ids::content_hash;
use rootlight_ir::{ExtensionSupport, NormalizedIrDocument};
use rootlight_protocol::{
    adapter_contract::{
        ADAPTER_DIGEST_BYTES, AdapterFrameDecoder, MAX_ADAPTER_FRAME_BYTES,
        MAX_ADAPTER_PROJECT_RESULT_CHUNKS, NegotiatedSession, encode_adapter_frame,
        encode_length_delimited_adapter_frame,
    },
    generated::adapter::v1::{
        AdapterFrame, ProjectAnalysisRequest, ProjectAnalysisResult, ProjectAnalysisResultChunk,
        ProjectAnalysisResultEnd, adapter_frame,
    },
};
use rootlight_sandbox::{
    AdapterExecutableDigest, AdapterProcessCommand, AdapterSandboxLimits, AdapterStderr,
    AdapterStdin, AdapterStdout, IsolatedAdapterProcess, spawn_isolated_adapter,
};

use crate::project::{
    FILES_ARGUMENT, INPUT_BYTES_ARGUMENT, MEMORY_BYTES_ARGUMENT, OUTPUT_BYTES_ARGUMENT,
    PROJECT_SESSION_ARGUMENT, WALL_TIME_ARGUMENT,
};
use crate::{
    AdapterHostError, IsolationReport, prepare_project_analysis, validate_project_analysis_result,
    validate_reassembled_project_analysis_result,
};

const MAX_ADAPTER_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const OUTPUT_READ_BUFFER_BYTES: usize = 16 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(2);
const PROCESS_REAP_GRACE: Duration = Duration::from_secs(2);

/// Validated project output and native evidence from the exact producing process.
#[derive(Debug)]
pub struct IsolatedProjectAnalysis {
    document: NormalizedIrDocument,
    isolation: IsolationReport,
}

#[derive(Debug)]
enum ProjectProcessOutput {
    Single(ProjectAnalysisResult),
    Chunked(ProjectAnalysisResult),
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
    let output_limit = usize::try_from(session.limits().output_bytes)
        .map_err(|_| AdapterHostError::Limit)?
        .checked_add(MAX_ADAPTER_FRAME_BYTES)
        .ok_or(AdapterHostError::Limit)?;
    let expected_executable_digest: [u8; ADAPTER_DIGEST_BYTES] = session
        .adapter()
        .source_digest
        .as_slice()
        .try_into()
        .map_err(|_| AdapterHostError::Process)?;
    let command = AdapterProcessCommand::new(
        executable,
        packet.len(),
        output_limit,
        MAX_ADAPTER_DIAGNOSTIC_BYTES,
    )
    .map_err(|_| AdapterHostError::Process)?
    .expected_executable_digest(AdapterExecutableDigest::from_bytes(
        expected_executable_digest,
    ))
    .arg(PROJECT_SESSION_ARGUMENT)
    .and_then(|command| command.arg(WALL_TIME_ARGUMENT))
    .and_then(|command| command.arg(session.limits().wall_time_ms.to_string()))
    .and_then(|command| command.arg(MEMORY_BYTES_ARGUMENT))
    .and_then(|command| command.arg(session.limits().memory_bytes.to_string()))
    .and_then(|command| command.arg(INPUT_BYTES_ARGUMENT))
    .and_then(|command| command.arg(session.limits().input_bytes.to_string()))
    .and_then(|command| command.arg(OUTPUT_BYTES_ARGUMENT))
    .and_then(|command| command.arg(session.limits().output_bytes.to_string()))
    .and_then(|command| command.arg(FILES_ARGUMENT))
    .and_then(|command| command.arg(session.limits().files.to_string()))
    .map_err(|_| AdapterHostError::Process)?;
    let memory_bytes =
        usize::try_from(session.limits().memory_bytes).map_err(|_| AdapterHostError::Limit)?;
    let cpu_time = Duration::from_millis(session.limits().cpu_time_ms);
    let sandbox_limits =
        AdapterSandboxLimits::new(memory_bytes, cpu_time).map_err(|_| AdapterHostError::Process)?;
    let mut process =
        spawn_isolated_adapter(command, sandbox_limits).map_err(|_| AdapterHostError::Process)?;
    let isolation = IsolationReport::from_process(process.report());

    let stdin = process.take_stdin().ok_or(AdapterHostError::ProcessIo)?;
    let stdout = process.take_stdout().ok_or(AdapterHostError::ProcessIo)?;
    let stderr = process.take_stderr().ok_or(AdapterHostError::ProcessIo)?;
    let output_reader = spawn_project_output_reader(stdout, session.clone(), &process)?;
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
            let _ = join_adapter_worker(output_reader);
            let _ = join_worker(diagnostic_reader);
            return Err(error);
        }
    };
    let input_result = join_worker(input_writer);
    let output_result = join_adapter_worker(output_reader);
    let diagnostic_result = join_worker(diagnostic_reader);
    let diagnostic = diagnostic_result?;
    process
        .wait_empty(reap_deadline())
        .map_err(|_| AdapterHostError::Process)?;
    cancellation.check()?;
    if !status.success() || !diagnostic.is_empty() {
        return Err(classify_process_failure(&diagnostic));
    }
    input_result?;
    let output = output_result?;

    let document = match output {
        ProjectProcessOutput::Single(result) => {
            let frame = AdapterFrame {
                message: Some(adapter_frame::Message::ProjectAnalysisResult(result)),
            };
            let encoded = encode_adapter_frame(&frame)?;
            validate_project_analysis_result(
                session,
                pending,
                &encoded,
                supported_extensions,
                cancellation,
            )?
        }
        ProjectProcessOutput::Chunked(result) => validate_reassembled_project_analysis_result(
            session,
            pending,
            result,
            supported_extensions,
            cancellation,
        )?,
    };
    Ok(IsolatedProjectAnalysis {
        document,
        isolation,
    })
}

fn classify_process_failure(diagnostic: &[u8]) -> AdapterHostError {
    let diagnostic = diagnostic
        .strip_suffix(b"\r\n")
        .or_else(|| diagnostic.strip_suffix(b"\n"))
        .unwrap_or(diagnostic);
    match diagnostic {
        b"error: adapter project input limit exceeded" => AdapterHostError::ProjectInputLimit,
        b"error: adapter project output limit exceeded" => AdapterHostError::ProjectOutputLimit,
        b"error: adapter project memory limit exceeded" => AdapterHostError::ProjectMemoryLimit,
        _ => AdapterHostError::ProcessFailed,
    }
}

fn spawn_project_output_reader(
    mut reader: AdapterStdout,
    session: NegotiatedSession,
    process: &IsolatedAdapterProcess,
) -> Result<JoinHandle<Result<ProjectProcessOutput, AdapterHostError>>, AdapterHostError> {
    thread::Builder::new()
        .name("rootlight-adapter-stdout".to_owned())
        .spawn(move || decode_project_output(&mut reader, &session))
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

fn join_adapter_worker<T>(
    worker: JoinHandle<Result<T, AdapterHostError>>,
) -> Result<T, AdapterHostError> {
    worker.join().map_err(|_| AdapterHostError::ProcessIo)?
}

#[cfg(test)]
fn decode_exact_packet(packet: &[u8]) -> Result<AdapterFrame, AdapterHostError> {
    let mut frames = decode_packets(packet)?;
    if frames.len() != 1 {
        return Err(AdapterHostError::UnexpectedFrame);
    }
    frames.pop().ok_or(AdapterHostError::UnexpectedFrame)
}

#[cfg(test)]
fn decode_packets(packet: &[u8]) -> Result<Vec<AdapterFrame>, AdapterHostError> {
    let mut decoder = AdapterFrameDecoder::new();
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < packet.len() {
        let (consumed, frame) = decoder.push(&packet[offset..])?;
        if consumed == 0 {
            return Err(AdapterHostError::UnexpectedFrame);
        }
        offset = offset
            .checked_add(consumed)
            .ok_or(AdapterHostError::Limit)?;
        if let Some(frame) = frame {
            frames.try_reserve(1).map_err(|_| AdapterHostError::Limit)?;
            frames.push(frame);
        } else if offset == packet.len() {
            return Err(AdapterHostError::UnexpectedFrame);
        }
    }
    if frames.is_empty() {
        return Err(AdapterHostError::UnexpectedFrame);
    }
    Ok(frames)
}

fn decode_project_output(
    reader: &mut impl std::io::Read,
    session: &NegotiatedSession,
) -> Result<ProjectProcessOutput, AdapterHostError> {
    let mut decoder = AdapterFrameDecoder::new();
    let mut accumulator = ProjectOutputAccumulator::default();
    let mut buffer = [0_u8; OUTPUT_READ_BUFFER_BYTES];
    let mut incomplete_frame = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| AdapterHostError::ProcessIo)?;
        if read == 0 {
            break;
        }
        let mut offset = 0usize;
        while offset < read {
            let (consumed, frame) = decoder.push(&buffer[offset..read])?;
            if consumed == 0 {
                return Err(AdapterHostError::UnexpectedFrame);
            }
            offset = offset
                .checked_add(consumed)
                .ok_or(AdapterHostError::Limit)?;
            incomplete_frame = frame.is_none();
            if let Some(frame) = frame {
                accumulator.accept(session, frame)?;
            }
        }
    }
    if incomplete_frame {
        return Err(AdapterHostError::UnexpectedFrame);
    }
    accumulator.finish()
}

#[derive(Debug, Default)]
struct ProjectOutputAccumulator {
    single: Option<ProjectAnalysisResult>,
    session_id: Option<Vec<u8>>,
    request_id: Option<Vec<u8>>,
    next_chunk_index: u32,
    normalized_ir: Vec<u8>,
    completed: Option<ProjectAnalysisResult>,
}

impl ProjectOutputAccumulator {
    fn accept(
        &mut self,
        session: &NegotiatedSession,
        frame: AdapterFrame,
    ) -> Result<(), AdapterHostError> {
        if self.single.is_some() || self.completed.is_some() {
            return Err(AdapterHostError::UnexpectedFrame);
        }
        match frame.message {
            Some(adapter_frame::Message::ProjectAnalysisResult(result))
                if self.next_chunk_index == 0 =>
            {
                self.single = Some(result);
                Ok(())
            }
            Some(adapter_frame::Message::ProjectAnalysisResultChunk(chunk)) => {
                self.accept_chunk(session, chunk)
            }
            Some(adapter_frame::Message::ProjectAnalysisResultEnd(end)) => {
                self.accept_end(session, end)
            }
            _ => Err(AdapterHostError::UnexpectedFrame),
        }
    }

    fn accept_chunk(
        &mut self,
        session: &NegotiatedSession,
        chunk: ProjectAnalysisResultChunk,
    ) -> Result<(), AdapterHostError> {
        session.validate_project_analysis_result_chunk(&chunk)?;
        if chunk.chunk_index != self.next_chunk_index
            || self
                .session_id
                .as_ref()
                .is_some_and(|expected| expected != &chunk.session_id)
            || self
                .request_id
                .as_ref()
                .is_some_and(|expected| expected != &chunk.request_id)
        {
            return Err(AdapterHostError::RequestMismatch);
        }
        if self.session_id.is_none() {
            self.session_id = Some(chunk.session_id.clone());
            self.request_id = Some(chunk.request_id.clone());
        }
        let complete_bytes = self
            .normalized_ir
            .len()
            .checked_add(chunk.normalized_ir_chunk.len())
            .ok_or(AdapterHostError::Limit)?;
        if u64::try_from(complete_bytes).map_err(|_| AdapterHostError::Limit)?
            > session.limits().output_bytes
        {
            return Err(AdapterHostError::ProjectOutputLimit);
        }
        self.normalized_ir
            .try_reserve(chunk.normalized_ir_chunk.len())
            .map_err(|_| AdapterHostError::Limit)?;
        self.normalized_ir
            .extend_from_slice(&chunk.normalized_ir_chunk);
        self.next_chunk_index = self
            .next_chunk_index
            .checked_add(1)
            .filter(|index| *index <= MAX_ADAPTER_PROJECT_RESULT_CHUNKS)
            .ok_or(AdapterHostError::ProjectOutputLimit)?;
        Ok(())
    }

    fn accept_end(
        &mut self,
        session: &NegotiatedSession,
        end: ProjectAnalysisResultEnd,
    ) -> Result<(), AdapterHostError> {
        session.validate_project_analysis_result_end(&end)?;
        let observed_digest = content_hash(&self.normalized_ir);
        if self.session_id.as_ref() != Some(&end.session_id)
            || self.request_id.as_ref() != Some(&end.request_id)
            || self.next_chunk_index != end.chunk_count
            || u64::try_from(self.normalized_ir.len()).map_err(|_| AdapterHostError::Limit)?
                != end.total_output_bytes
        {
            return Err(AdapterHostError::RequestMismatch);
        }
        if end
            .output_digest
            .as_ref()
            .is_none_or(|digest| digest.value.as_slice() != observed_digest.as_bytes())
        {
            return Err(AdapterHostError::DigestMismatch);
        }
        self.completed = Some(ProjectAnalysisResult {
            session_id: end.session_id,
            request_id: end.request_id,
            normalized_ir: std::mem::take(&mut self.normalized_ir),
            output_digest: end.output_digest,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<ProjectProcessOutput, AdapterHostError> {
        if let Some(result) = self.single.take() {
            return Ok(ProjectProcessOutput::Single(result));
        }
        if let Some(result) = self.completed.take() {
            return Ok(ProjectProcessOutput::Chunked(result));
        }
        Err(AdapterHostError::UnexpectedFrame)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rootlight_ids::content_hash;
    use rootlight_protocol::{
        adapter_contract::{ADAPTER_NONCE_BYTES, encode_length_delimited_adapter_frame},
        generated::{
            adapter::v1::{
                AdapterFrame, ProjectAnalysisResult, ProjectAnalysisResultChunk,
                ProjectAnalysisResultEnd, adapter_frame,
            },
            common::v1::ContentHash,
        },
    };

    use crate::{PROJECT_ADAPTER_HARD_LIMITS, negotiate_project_adapter_session};

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

    #[test]
    fn process_failures_preserve_closed_resource_limit_diagnostics() {
        for (diagnostic, expected) in [
            (
                b"error: adapter project input limit exceeded\n".as_slice(),
                AdapterHostError::ProjectInputLimit,
            ),
            (
                b"error: adapter project output limit exceeded\r\n".as_slice(),
                AdapterHostError::ProjectOutputLimit,
            ),
            (
                b"error: adapter project memory limit exceeded\n".as_slice(),
                AdapterHostError::ProjectMemoryLimit,
            ),
        ] {
            assert_eq!(
                std::mem::discriminant(&classify_process_failure(diagnostic)),
                std::mem::discriminant(&expected)
            );
        }
        assert!(matches!(
            classify_process_failure(b"error: unrecognized adapter failure\n"),
            AdapterHostError::ProcessFailed
        ));
    }

    #[test]
    fn chunked_decoder_requires_ordered_authenticated_commit() {
        let session = project_session();
        let request_id = vec![9; ADAPTER_NONCE_BYTES];
        let first = b"first".to_vec();
        let second = b"second".to_vec();
        let complete = [first.as_slice(), second.as_slice()].concat();
        let frames = vec![
            chunk_frame(&session, &request_id, 0, first),
            chunk_frame(&session, &request_id, 1, second),
            end_frame(&session, &request_id, 2, &complete),
        ];
        let encoded = encode_frames(&frames);

        let output = decode_project_output(&mut Cursor::new(encoded), &session)
            .expect("a complete chunk stream commits");
        let ProjectProcessOutput::Chunked(result) = output else {
            panic!("chunked output expected");
        };
        assert_eq!(result.normalized_ir, complete);
    }

    #[test]
    fn chunked_decoder_rejects_missing_duplicate_and_trailing_frames() {
        let session = project_session();
        let request_id = vec![10; ADAPTER_NONCE_BYTES];
        let first = b"first".to_vec();
        let second = b"second".to_vec();
        let complete = [first.as_slice(), second.as_slice()].concat();
        let first_frame = chunk_frame(&session, &request_id, 0, first);
        let second_frame = chunk_frame(&session, &request_id, 1, second);
        let end = end_frame(&session, &request_id, 2, &complete);

        let missing_end = encode_frames(&[first_frame.clone(), second_frame.clone()]);
        assert!(matches!(
            decode_project_output(&mut Cursor::new(missing_end), &session),
            Err(AdapterHostError::UnexpectedFrame)
        ));

        let duplicate = encode_frames(&[first_frame.clone(), first_frame.clone(), end.clone()]);
        assert!(matches!(
            decode_project_output(&mut Cursor::new(duplicate), &session),
            Err(AdapterHostError::RequestMismatch)
        ));

        let trailing = encode_frames(&[first_frame, second_frame, end.clone(), end]);
        assert!(matches!(
            decode_project_output(&mut Cursor::new(trailing), &session),
            Err(AdapterHostError::UnexpectedFrame)
        ));
    }

    #[test]
    fn chunked_decoder_rejects_chunk_and_result_digest_substitution() {
        let session = project_session();
        let request_id = vec![11; ADAPTER_NONCE_BYTES];
        let first = b"first".to_vec();
        let second = b"second".to_vec();
        let complete = [first.as_slice(), second.as_slice()].concat();

        let mut corrupted_chunk = chunk_frame(&session, &request_id, 0, first.clone());
        let Some(adapter_frame::Message::ProjectAnalysisResultChunk(chunk)) =
            corrupted_chunk.message.as_mut()
        else {
            panic!("chunk fixture expected");
        };
        chunk.chunk_digest = Some(ContentHash {
            value: vec![0; ADAPTER_DIGEST_BYTES],
        });
        let invalid_chunk = encode_frames(&[
            corrupted_chunk,
            chunk_frame(&session, &request_id, 1, second.clone()),
            end_frame(&session, &request_id, 2, &complete),
        ]);
        assert!(matches!(
            decode_project_output(&mut Cursor::new(invalid_chunk), &session),
            Err(AdapterHostError::Protocol(_))
        ));

        let mut corrupted_end = end_frame(&session, &request_id, 2, &complete);
        let Some(adapter_frame::Message::ProjectAnalysisResultEnd(end)) =
            corrupted_end.message.as_mut()
        else {
            panic!("end fixture expected");
        };
        end.output_digest = Some(ContentHash {
            value: vec![0; ADAPTER_DIGEST_BYTES],
        });
        let invalid_result = encode_frames(&[
            chunk_frame(&session, &request_id, 0, first),
            chunk_frame(&session, &request_id, 1, second),
            corrupted_end,
        ]);
        assert!(matches!(
            decode_project_output(&mut Cursor::new(invalid_result), &session),
            Err(AdapterHostError::DigestMismatch)
        ));
    }

    fn project_session() -> NegotiatedSession {
        let executable = std::env::current_exe().expect("test executable path is available");
        negotiate_project_adapter_session(
            &executable,
            [7; ADAPTER_NONCE_BYTES],
            PROJECT_ADAPTER_HARD_LIMITS,
        )
        .expect("project session negotiates")
    }

    fn chunk_frame(
        session: &NegotiatedSession,
        request_id: &[u8],
        chunk_index: u32,
        normalized_ir_chunk: Vec<u8>,
    ) -> AdapterFrame {
        let chunk_digest = content_hash(&normalized_ir_chunk);
        AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisResultChunk(
                ProjectAnalysisResultChunk {
                    session_id: session.session_id().to_vec(),
                    request_id: request_id.to_vec(),
                    chunk_index,
                    normalized_ir_chunk,
                    chunk_digest: Some(ContentHash {
                        value: chunk_digest.as_bytes().to_vec(),
                    }),
                },
            )),
        }
    }

    fn end_frame(
        session: &NegotiatedSession,
        request_id: &[u8],
        chunk_count: u32,
        normalized_ir: &[u8],
    ) -> AdapterFrame {
        AdapterFrame {
            message: Some(adapter_frame::Message::ProjectAnalysisResultEnd(
                ProjectAnalysisResultEnd {
                    session_id: session.session_id().to_vec(),
                    request_id: request_id.to_vec(),
                    chunk_count,
                    total_output_bytes: u64::try_from(normalized_ir.len())
                        .expect("fixture length is representable"),
                    output_digest: Some(ContentHash {
                        value: content_hash(normalized_ir).as_bytes().to_vec(),
                    }),
                },
            )),
        }
    }

    fn encode_frames(frames: &[AdapterFrame]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|frame| {
                encode_length_delimited_adapter_frame(frame)
                    .expect("fixture frame encodes")
                    .into_iter()
            })
            .collect()
    }
}
