//! Bounded local transport for Rootlight's daemon protocol.
//!
//! The codec rejects oversized or truncated protobuf frames before allocation.
//! The endpoint wrapper applies platform access policy and exposes one-request
//! connections so slow peers cannot monopolize an unbounded protocol session.

#![forbid(unsafe_code)]

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use interprocess::local_socket::{
    GenericFilePath, Listener, ListenerOptions, Stream, ToFsName as _,
    traits::{Listener as _, Stream as _},
};
use prost::Message;
use rootlight_protocol::generated::daemon::v1::{RequestEnvelope, ResponseEnvelope};

/// Maximum encoded protobuf payload accepted by the local daemon.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Default time allowed for one frame read or write.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_HEADER_BYTES: usize = 4;
const RETRY_PAUSE: Duration = Duration::from_millis(1);

/// Platform endpoint selected for the current Rootlight user instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    path: PathBuf,
}

impl Endpoint {
    /// Creates a local endpoint from an absolute Unix socket path or Windows pipe path.
    ///
    /// Windows paths must use the local `\\.\pipe\` namespace. Unix paths must be
    /// absolute so no ambient working directory can redirect the endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::InvalidEndpoint`] when the path is empty, relative, or
    /// outside the local named-pipe namespace on Windows.
    pub fn new(path: PathBuf) -> Result<Self, IpcError> {
        validate_endpoint(&path)?;
        Ok(Self { path })
    }

    /// Returns the platform endpoint path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    fn name(&self) -> Result<interprocess::local_socket::Name<'_>, IpcError> {
        self.path
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .map_err(IpcError::Transport)
    }
}

/// Bounded length-prefixed protobuf codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameCodec {
    maximum_bytes: usize,
    timeout: Duration,
}

impl FrameCodec {
    /// Creates a codec with explicit frame and elapsed-I/O bounds.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::InvalidLimit`] when either bound is zero or when the
    /// frame bound cannot be represented by the four-byte wire header.
    pub fn new(maximum_bytes: usize, timeout: Duration) -> Result<Self, IpcError> {
        if maximum_bytes == 0 || u32::try_from(maximum_bytes).is_err() || timeout.is_zero() {
            return Err(IpcError::InvalidLimit);
        }
        Ok(Self {
            maximum_bytes,
            timeout,
        })
    }

    /// Encodes and writes one bounded protobuf message.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::FrameTooLarge`] before writing when the encoded body
    /// exceeds the configured bound, or a transport/timeout error during I/O.
    pub fn write_message<M: Message>(
        &self,
        stream: &mut Stream,
        message: &M,
    ) -> Result<(), IpcError> {
        let encoded_length = message.encoded_len();
        if encoded_length > self.maximum_bytes {
            return Err(IpcError::FrameTooLarge {
                observed: encoded_length,
                maximum: self.maximum_bytes,
            });
        }
        let wire_length = u32::try_from(encoded_length).map_err(|_| IpcError::FrameTooLarge {
            observed: encoded_length,
            maximum: self.maximum_bytes,
        })?;
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + encoded_length);
        frame.extend_from_slice(&wire_length.to_be_bytes());
        message.encode(&mut frame).map_err(IpcError::Encode)?;
        write_all_bounded(stream, &frame, self.timeout)
    }

    /// Reads and decodes one bounded protobuf message.
    ///
    /// # Errors
    ///
    /// Returns a typed error for elapsed I/O, EOF, an oversized declared length,
    /// allocation failure, trailing bytes, or malformed protobuf.
    pub fn read_message<M: Message + Default>(&self, stream: &mut Stream) -> Result<M, IpcError> {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        read_exact_bounded(stream, &mut header, self.timeout)?;
        let declared = usize::try_from(u32::from_be_bytes(header))
            .map_err(|_| IpcError::InvalidFrameLength)?;
        if declared > self.maximum_bytes {
            return Err(IpcError::FrameTooLarge {
                observed: declared,
                maximum: self.maximum_bytes,
            });
        }
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(declared)
            .map_err(|_| IpcError::AllocationFailed)?;
        payload.resize(declared, 0);
        read_exact_bounded(stream, &mut payload, self.timeout)?;
        let mut bytes = payload.as_slice();
        let message = M::decode(&mut bytes).map_err(IpcError::Decode)?;
        if !bytes.is_empty() {
            return Err(IpcError::TrailingBytes);
        }
        Ok(message)
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self {
            maximum_bytes: MAX_FRAME_BYTES,
            timeout: DEFAULT_IO_TIMEOUT,
        }
    }
}

/// One platform listener configured for private local daemon access.
#[derive(Debug)]
pub struct LocalListener {
    listener: Listener,
    endpoint: Endpoint,
}

impl LocalListener {
    /// Binds a private per-user local endpoint.
    ///
    /// Unix binds mode `0600` and verifies the resulting socket type, owner, and
    /// permissions. Windows applies a protected DACL for the current logon user.
    /// Existing endpoints are never removed by this constructor.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError`] for invalid endpoints, access-policy failures, or
    /// transport creation errors.
    pub fn bind(endpoint: Endpoint) -> Result<Self, IpcError> {
        let name = endpoint.name()?;
        let options = ListenerOptions::new().name(name).reclaim_name(false);
        let options = platform_listener_options(options)?;
        let listener = options.create_sync().map_err(IpcError::Transport)?;
        verify_bound_endpoint(&endpoint)?;
        Ok(Self { listener, endpoint })
    }

    /// Accepts one client stream and enables bounded nonblocking I/O.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Transport`] when accept or stream configuration fails.
    pub fn accept(&self) -> Result<Stream, IpcError> {
        let stream = self.listener.accept().map_err(IpcError::Transport)?;
        stream.set_nonblocking(true).map_err(IpcError::Transport)?;
        Ok(stream)
    }

    /// Returns the bound endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

impl Drop for LocalListener {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(self.endpoint.as_path());
        }
    }
}

/// Connects to a local daemon endpoint with bounded nonblocking I/O.
///
/// # Errors
///
/// Returns [`IpcError::Transport`] when the endpoint cannot be reached or the
/// connected stream cannot be configured.
pub fn connect(endpoint: &Endpoint) -> Result<Stream, IpcError> {
    let stream = Stream::connect(endpoint.name()?).map_err(IpcError::Transport)?;
    stream.set_nonblocking(true).map_err(IpcError::Transport)?;
    Ok(stream)
}

/// Writes one request frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the request cannot be encoded or sent within bounds.
pub fn write_request(
    codec: FrameCodec,
    stream: &mut Stream,
    request: &RequestEnvelope,
) -> Result<(), IpcError> {
    codec.write_message(stream, request)
}

/// Reads one request frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the request frame is malformed, oversized, or late.
pub fn read_request(codec: FrameCodec, stream: &mut Stream) -> Result<RequestEnvelope, IpcError> {
    codec.read_message(stream)
}

/// Writes one response frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the response cannot be encoded or sent within bounds.
pub fn write_response(
    codec: FrameCodec,
    stream: &mut Stream,
    response: &ResponseEnvelope,
) -> Result<(), IpcError> {
    codec.write_message(stream, response)
}

/// Reads one response frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the response frame is malformed, oversized, or late.
pub fn read_response(codec: FrameCodec, stream: &mut Stream) -> Result<ResponseEnvelope, IpcError> {
    codec.read_message(stream)
}

fn read_exact_bounded(
    stream: &mut Stream,
    buffer: &mut [u8],
    timeout: Duration,
) -> Result<(), IpcError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(IpcError::InvalidLimit)?;
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) if filled == 0 => wait_for_io(deadline)?,
            Ok(0) => return Err(IpcError::UnexpectedEof),
            Ok(read) => {
                filled = filled
                    .checked_add(read)
                    .ok_or(IpcError::InvalidFrameLength)?
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => wait_for_io(deadline)?,
            Err(source) => return Err(IpcError::Transport(source)),
        }
    }
    Ok(())
}

fn write_all_bounded(
    stream: &mut Stream,
    buffer: &[u8],
    timeout: Duration,
) -> Result<(), IpcError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(IpcError::InvalidLimit)?;
    let mut written = 0;
    while written < buffer.len() {
        match stream.write(&buffer[written..]) {
            Ok(0) => return Err(IpcError::WriteZero),
            Ok(count) => {
                written = written
                    .checked_add(count)
                    .ok_or(IpcError::InvalidFrameLength)?
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => wait_for_io(deadline)?,
            Err(source) => return Err(IpcError::Transport(source)),
        }
    }
    Ok(())
}

fn wait_for_io(deadline: Instant) -> Result<(), IpcError> {
    if Instant::now() >= deadline {
        return Err(IpcError::TimedOut);
    }
    std::thread::sleep(RETRY_PAUSE);
    Ok(())
}

#[cfg(unix)]
fn validate_endpoint(path: &Path) -> Result<(), IpcError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(IpcError::InvalidEndpoint);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_endpoint(path: &Path) -> Result<(), IpcError> {
    let value = path.to_str().ok_or(IpcError::InvalidEndpoint)?;
    if value.len() <= 9
        || !value
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"\\.\pipe\"))
    {
        return Err(IpcError::InvalidEndpoint);
    }
    Ok(())
}

#[cfg(unix)]
fn platform_listener_options(
    options: ListenerOptions<'_>,
) -> Result<ListenerOptions<'_>, IpcError> {
    use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
    Ok(options.mode(0o600))
}

#[cfg(windows)]
fn platform_listener_options(
    options: ListenerOptions<'_>,
) -> Result<ListenerOptions<'_>, IpcError> {
    use interprocess::os::windows::{
        local_socket::ListenerOptionsExt as _, security_descriptor::SecurityDescriptor,
    };
    use widestring::U16CString;

    // `OW` scopes this protected DACL to the owner assigned by Windows for the
    // new pipe object. The instance nonce remains mandatory protocol defense.
    let sddl = U16CString::from_str("D:P(A;;GA;;;OW)").map_err(|_| IpcError::SecurityPolicy)?;
    let descriptor = SecurityDescriptor::deserialize(&sddl).map_err(IpcError::Transport)?;
    Ok(options.security_descriptor(descriptor))
}

#[cfg(unix)]
fn verify_bound_endpoint(endpoint: &Endpoint) -> Result<(), IpcError> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let parent = endpoint
        .as_path()
        .parent()
        .ok_or(IpcError::InvalidEndpoint)?;
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(IpcError::Transport)?;
    let metadata = std::fs::symlink_metadata(endpoint.as_path()).map_err(IpcError::Transport)?;
    if !parent_metadata.file_type().is_dir()
        || parent_metadata.mode() & 0o077 != 0
        || !metadata.file_type().is_socket()
        || metadata.uid() != parent_metadata.uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(IpcError::InsecureEndpoint);
    }
    Ok(())
}

#[cfg(windows)]
fn verify_bound_endpoint(_endpoint: &Endpoint) -> Result<(), IpcError> {
    Ok(())
}

/// Local transport and framing failures.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// Endpoint syntax or platform namespace was invalid.
    #[error("local endpoint is invalid")]
    InvalidEndpoint,
    /// A frame or timeout bound was zero or unrepresentable.
    #[error("local IPC limit is invalid")]
    InvalidLimit,
    /// A declared or encoded frame exceeded the configured maximum.
    #[error("local IPC frame exceeds {maximum} bytes")]
    FrameTooLarge {
        /// Observed encoded or declared bytes.
        observed: usize,
        /// Configured maximum bytes.
        maximum: usize,
    },
    /// The peer closed the stream before one complete frame arrived.
    #[error("local IPC frame ended unexpectedly")]
    UnexpectedEof,
    /// A write reported no progress before the frame completed.
    #[error("local IPC write made no progress")]
    WriteZero,
    /// The declared frame length could not be represented safely.
    #[error("local IPC frame length is invalid")]
    InvalidFrameLength,
    /// Allocation for a validated frame failed.
    #[error("local IPC frame allocation failed")]
    AllocationFailed,
    /// The frame exceeded its elapsed I/O deadline.
    #[error("local IPC frame timed out")]
    TimedOut,
    /// Decoding left bytes outside the protobuf message.
    #[error("local IPC frame has trailing bytes")]
    TrailingBytes,
    /// Bound endpoint metadata did not satisfy the private-user policy.
    #[error("local IPC endpoint permissions are insecure")]
    InsecureEndpoint,
    /// Platform access policy could not be constructed safely.
    #[error("local IPC security policy is unavailable")]
    SecurityPolicy,
    /// Protobuf encoding failed.
    #[error("local IPC protobuf encoding failed")]
    Encode(#[source] prost::EncodeError),
    /// Protobuf decoding failed.
    #[error("local IPC protobuf decoding failed")]
    Decode(#[source] prost::DecodeError),
    /// Platform transport operation failed.
    #[error("local IPC transport failed")]
    Transport(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_protocol::generated::daemon::v1::{HealthRequest, request_envelope};
    use std::{sync::mpsc, thread};
    use tempfile::{TempDir, tempdir};

    fn private_tempdir() -> TempDir {
        let temporary = tempdir().expect("temporary directory is available");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
                .expect("temporary directory becomes private");
        }
        temporary
    }

    fn request(request_id: u64) -> RequestEnvelope {
        RequestEnvelope {
            request_id,
            instance_nonce: vec![7; 16],
            request: Some(request_envelope::Request::Health(HealthRequest {})),
        }
    }

    #[test]
    fn endpoint_rejects_relative_paths() {
        assert!(matches!(
            Endpoint::new(PathBuf::from("rootlight.sock")),
            Err(IpcError::InvalidEndpoint)
        ));
    }

    #[test]
    fn local_round_trip_preserves_one_bounded_request() {
        let temporary = private_tempdir();
        #[cfg(unix)]
        let endpoint =
            Endpoint::new(temporary.path().join("rootlight.sock")).expect("endpoint is valid");
        #[cfg(windows)]
        let endpoint = Endpoint::new(PathBuf::from(format!(
            r"\\.\pipe\rootlight-test-{}-{}",
            std::process::id(),
            temporary.path().display().to_string().len()
        )))
        .expect("endpoint is valid");
        let listener = LocalListener::bind(endpoint.clone()).expect("listener binds");
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let server = thread::spawn(move || {
            ready_tx.send(()).expect("test synchronization succeeds");
            let mut stream = listener.accept().expect("connection accepts");
            read_request(FrameCodec::default(), &mut stream).expect("request decodes")
        });
        ready_rx.recv().expect("server is ready");

        let mut stream = connect(&endpoint).expect("client connects");
        write_request(FrameCodec::default(), &mut stream, &request(41)).expect("request writes");

        assert_eq!(server.join().expect("server thread joins"), request(41));
    }

    #[test]
    fn oversized_declared_length_is_rejected_before_payload_read() {
        let temporary = private_tempdir();
        #[cfg(unix)]
        let endpoint =
            Endpoint::new(temporary.path().join("oversized.sock")).expect("endpoint is valid");
        #[cfg(windows)]
        let endpoint = Endpoint::new(PathBuf::from(format!(
            r"\\.\pipe\rootlight-oversized-{}-{}",
            std::process::id(),
            temporary.path().display().to_string().len()
        )))
        .expect("endpoint is valid");
        let listener = LocalListener::bind(endpoint.clone()).expect("listener binds");
        let server = thread::spawn(move || {
            let mut stream = listener.accept().expect("connection accepts");
            let codec = FrameCodec::new(16, Duration::from_secs(1)).expect("limits are valid");
            codec.read_message::<RequestEnvelope>(&mut stream)
        });
        let mut stream = connect(&endpoint).expect("client connects");
        write_all_bounded(&mut stream, &17_u32.to_be_bytes(), Duration::from_secs(1))
            .expect("header writes");

        assert!(matches!(
            server.join().expect("server thread joins"),
            Err(IpcError::FrameTooLarge {
                observed: 17,
                maximum: 16
            })
        ));
    }
}
