//! Bounded local transport for Rootlight's daemon protocol.
//!
//! The codec rejects oversized or truncated protobuf frames before allocation.
//! The endpoint wrapper applies platform access policy and exposes one-request
//! connections so slow peers cannot monopolize an unbounded protocol session.

#![forbid(unsafe_code)]

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName as _};
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, Stream,
    tokio::{Listener as TokioListener, Stream as TokioStream},
    traits::{Listener as _, Stream as _},
};
#[cfg(unix)]
use interprocess::{local_socket::ToFsName as _, os::unix::local_socket::FilesystemUdSocket};
#[cfg(windows)]
use nt_token::OwnedToken;
use prost::Message;
use rootlight_protocol::generated::daemon::v1::{
    ClientHello, RequestEnvelope, ResponseEnvelope, ServerHello,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    time::timeout,
};

// `interprocess` applies the Unix listener mode through process-global `umask`
// during creation. Serialize that short section so parallel binds cannot race.
#[cfg(unix)]
static UNIX_LISTENER_CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Maximum encoded protobuf payload accepted by the local daemon.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Default time allowed for one frame read or write.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(2);
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
    /// Returns [`IpcError::InvalidEndpoint`] when the path is empty, relative,
    /// unrepresentable as a platform Unix socket address, or outside the local
    /// named-pipe namespace on Windows.
    pub fn new(path: PathBuf) -> Result<Self, IpcError> {
        validate_endpoint(&path)?;
        Ok(Self { path })
    }

    /// Returns the platform endpoint path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    #[cfg(unix)]
    fn name(&self) -> Result<interprocess::local_socket::Name<'_>, IpcError> {
        self.path
            .as_path()
            .to_fs_name::<FilesystemUdSocket>()
            .map_err(IpcError::Transport)
    }

    #[cfg(windows)]
    fn name(&self) -> Result<interprocess::local_socket::Name<'_>, IpcError> {
        self.path
            .to_str()
            .and_then(|value| value.get(9..))
            .ok_or(IpcError::InvalidEndpoint)?
            .to_ns_name::<GenericNamespaced>()
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
    pub fn read_message<M: Message + Default, R: io::Read>(
        &self,
        stream: &mut R,
    ) -> Result<M, IpcError> {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        read_exact_bounded(stream, &mut header, self.timeout)?;
        self.decode_message(stream, header)
    }

    fn decode_message<M: Message + Default, R: io::Read>(
        &self,
        stream: &mut R,
        header: [u8; FRAME_HEADER_BYTES],
    ) -> Result<M, IpcError> {
        let declared = self.validate_declared_length(header)?;
        let mut payload = self.allocate_payload(declared)?;
        read_exact_bounded(stream, &mut payload, self.timeout)?;
        decode_payload(&payload)
    }

    /// Encodes and writes one bounded protobuf message asynchronously.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::FrameTooLarge`] before writing when the encoded body
    /// exceeds the configured bound, or a transport/timeout error during I/O.
    pub async fn write_message_async<M: Message, W: AsyncWrite + Unpin>(
        &self,
        stream: &mut W,
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
        let mut frame = Vec::new();
        frame
            .try_reserve_exact(FRAME_HEADER_BYTES + encoded_length)
            .map_err(|_| IpcError::AllocationFailed)?;
        frame.extend_from_slice(&wire_length.to_be_bytes());
        message.encode(&mut frame).map_err(IpcError::Encode)?;
        write_all_async(stream, &frame, self.timeout).await
    }

    /// Reads and decodes one bounded protobuf message asynchronously.
    ///
    /// # Errors
    ///
    /// Returns a typed error for elapsed I/O, EOF, an oversized declared length,
    /// allocation failure, trailing bytes, or malformed protobuf.
    pub async fn read_message_async<M: Message + Default, R: AsyncRead + Unpin>(
        &self,
        stream: &mut R,
    ) -> Result<M, IpcError> {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        read_exact_async(stream, &mut header, self.timeout).await?;
        let declared = self.validate_declared_length(header)?;
        let mut payload = self.allocate_payload(declared)?;
        read_exact_async(stream, &mut payload, self.timeout).await?;
        decode_payload(&payload)
    }

    fn validate_declared_length(
        &self,
        header: [u8; FRAME_HEADER_BYTES],
    ) -> Result<usize, IpcError> {
        let declared = usize::try_from(u32::from_be_bytes(header))
            .map_err(|_| IpcError::InvalidFrameLength)?;
        if declared > self.maximum_bytes {
            return Err(IpcError::FrameTooLarge {
                observed: declared,
                maximum: self.maximum_bytes,
            });
        }
        Ok(declared)
    }

    fn allocate_payload(&self, declared: usize) -> Result<Vec<u8>, IpcError> {
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(declared)
            .map_err(|_| IpcError::AllocationFailed)?;
        payload.resize(declared, 0);
        Ok(payload)
    }
}

fn decode_payload<M: Message + Default>(payload: &[u8]) -> Result<M, IpcError> {
    let mut bytes = payload;
    let message = M::decode(&mut bytes).map_err(IpcError::Decode)?;
    if !bytes.is_empty() {
        return Err(IpcError::TrailingBytes);
    }
    Ok(message)
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

/// Accepted local stream used by the bounded frame codec.
pub type LocalStream = Stream;

/// One Tokio listener configured for private local daemon access.
#[derive(Debug)]
pub struct AsyncLocalListener {
    listener: TokioListener,
    endpoint: Endpoint,
}

/// Accepted Tokio local stream used by the asynchronous bounded frame codec.
pub type AsyncLocalStream = TokioStream;

impl LocalListener {
    /// Binds a private per-user local endpoint.
    ///
    /// Unix binds mode `0600` inside a private directory and verifies the resulting
    /// socket metadata. Windows applies a protected DACL for the pipe object owner.
    /// Unix stale endpoints must be recovered explicitly before this constructor;
    /// live listeners reclaim only the exact socket name they created when dropped.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError`] for invalid endpoints, access-policy failures, or
    /// transport creation errors.
    pub fn bind(endpoint: Endpoint) -> Result<Self, IpcError> {
        verify_endpoint_parent(&endpoint)?;
        let name = endpoint.name()?;
        let options = ListenerOptions::new()
            .name(name)
            .nonblocking(ListenerNonblockingMode::Both);
        let options = platform_listener_options(options)?;
        #[cfg(unix)]
        let _creation_guard = unix_listener_creation_guard()?;
        let listener = options.create_sync().map_err(IpcError::Transport)?;
        if let Err(error) = verify_bound_endpoint(&endpoint) {
            drop(listener);
            return Err(error);
        }
        Ok(Self { listener, endpoint })
    }

    /// Accepts one client stream within an elapsed deadline.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::TimedOut`] when no client arrives before the deadline,
    /// or [`IpcError::Transport`] for listener failures.
    pub fn accept_timeout(&self, timeout: Duration) -> Result<Stream, IpcError> {
        if timeout.is_zero() {
            return Err(IpcError::InvalidLimit);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(IpcError::InvalidLimit)?;
        loop {
            ensure_before_deadline(deadline)?;
            match self.listener.accept() {
                Ok(stream) => return Ok(stream),
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => wait_for_io(deadline)?,
                Err(source) => return Err(IpcError::Transport(source)),
            }
        }
    }

    /// Returns the bound endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

impl AsyncLocalListener {
    /// Binds a private per-user local endpoint to Tokio.
    ///
    /// The endpoint security checks are identical to [`LocalListener::bind`].
    ///
    /// # Errors
    ///
    /// Returns [`IpcError`] for invalid endpoints, access-policy failures, or
    /// transport creation errors.
    pub fn bind(endpoint: Endpoint) -> Result<Self, IpcError> {
        verify_endpoint_parent(&endpoint)?;
        let name = endpoint.name()?;
        let options = ListenerOptions::new().name(name);
        let options = platform_listener_options(options)?;
        #[cfg(unix)]
        let _creation_guard = unix_listener_creation_guard()?;
        let listener = options.create_tokio().map_err(IpcError::Transport)?;
        if let Err(error) = verify_bound_endpoint(&endpoint) {
            drop(listener);
            return Err(error);
        }
        Ok(Self { listener, endpoint })
    }

    /// Accepts one client stream within an elapsed deadline.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::TimedOut`] when no client arrives before the deadline,
    /// or [`IpcError::Transport`] for listener failures.
    pub async fn accept_timeout(&self, duration: Duration) -> Result<AsyncLocalStream, IpcError> {
        if duration.is_zero() {
            return Err(IpcError::InvalidLimit);
        }
        timeout(duration, self.listener.accept())
            .await
            .map_err(|_| IpcError::TimedOut)?
            .map_err(IpcError::Transport)
    }

    /// Accepts one client stream without adding a transport deadline.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Transport`] when the listener cannot accept a client.
    pub async fn accept(&self) -> Result<AsyncLocalStream, IpcError> {
        self.listener.accept().await.map_err(IpcError::Transport)
    }

    /// Returns the bound endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

/// Verifies that an accepted local peer belongs to the current user.
///
/// On Windows, the protected pipe DACL is the authorization boundary because
/// the current safe Tokio transport does not expose the client access token.
///
/// # Errors
///
/// Returns [`IpcError::PeerUnauthorized`] for a foreign Unix user or a typed
/// transport error when peer credentials cannot be inspected.
pub fn verify_peer(stream: &LocalStream) -> Result<(), IpcError> {
    verify_sync_platform_peer(stream)
}

/// Verifies that an accepted Tokio local peer belongs to the current user.
///
/// Unix credential inspection is synchronous but nonblocking and is completed
/// before the caller starts protocol I/O. On Windows, the protected pipe DACL
/// is the authorization boundary because the current safe Tokio transport does
/// not expose the client access token.
///
/// # Errors
///
/// Returns [`IpcError::PeerUnauthorized`] for a foreign Unix user or a typed
/// transport error when peer credentials cannot be inspected.
pub fn verify_peer_async(stream: &AsyncLocalStream) -> Result<(), IpcError> {
    verify_async_platform_peer(stream)
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

/// Connects to a local daemon endpoint through Tokio.
///
/// # Errors
///
/// Returns [`IpcError::Transport`] when the endpoint cannot be reached.
pub async fn connect_async(endpoint: &Endpoint) -> Result<AsyncLocalStream, IpcError> {
    TokioStream::connect(endpoint.name()?)
        .await
        .map_err(IpcError::Transport)
}

/// Writes the client negotiation frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello cannot be encoded or sent within bounds.
pub fn write_client_hello(
    codec: FrameCodec,
    stream: &mut Stream,
    hello: &ClientHello,
) -> Result<(), IpcError> {
    codec.write_message(stream, hello)
}

/// Reads the client negotiation frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello frame is malformed, oversized, or late.
pub fn read_client_hello(codec: FrameCodec, stream: &mut Stream) -> Result<ClientHello, IpcError> {
    codec.read_message(stream)
}

/// Writes the server negotiation frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello cannot be encoded or sent within bounds.
pub fn write_server_hello(
    codec: FrameCodec,
    stream: &mut Stream,
    hello: &ServerHello,
) -> Result<(), IpcError> {
    codec.write_message(stream, hello)
}

/// Reads the server negotiation frame.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello frame is malformed, oversized, or late.
pub fn read_server_hello(codec: FrameCodec, stream: &mut Stream) -> Result<ServerHello, IpcError> {
    codec.read_message(stream)
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

/// Writes the client negotiation frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello cannot be encoded or sent within bounds.
pub async fn write_client_hello_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
    hello: &ClientHello,
) -> Result<(), IpcError> {
    codec.write_message_async(stream, hello).await
}

/// Verifies the local peer and reads the client negotiation frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when peer authorization fails or when the hello frame is
/// malformed, oversized, or late.
pub async fn read_client_hello_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
) -> Result<ClientHello, IpcError> {
    verify_peer_async(stream)?;
    codec.read_message_async(stream).await
}

/// Writes the server negotiation frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello cannot be encoded or sent within bounds.
pub async fn write_server_hello_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
    hello: &ServerHello,
) -> Result<(), IpcError> {
    codec.write_message_async(stream, hello).await
}

/// Reads the server negotiation frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the hello frame is malformed, oversized, or late.
pub async fn read_server_hello_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
) -> Result<ServerHello, IpcError> {
    codec.read_message_async(stream).await
}

/// Writes one request frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the request cannot be encoded or sent within bounds.
pub async fn write_request_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
    request: &RequestEnvelope,
) -> Result<(), IpcError> {
    codec.write_message_async(stream, request).await
}

/// Reads one request frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the request frame is malformed, oversized, or late.
pub async fn read_request_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
) -> Result<RequestEnvelope, IpcError> {
    codec.read_message_async(stream).await
}

/// Writes one response frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the response cannot be encoded or sent within bounds.
pub async fn write_response_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
    response: &ResponseEnvelope,
) -> Result<(), IpcError> {
    codec.write_message_async(stream, response).await
}

/// Reads one response frame asynchronously.
///
/// # Errors
///
/// Returns [`IpcError`] when the response frame is malformed, oversized, or late.
pub async fn read_response_async(
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
) -> Result<ResponseEnvelope, IpcError> {
    codec.read_message_async(stream).await
}

/// Waits for an accepted asynchronous peer to close its receive direction.
///
/// The single-request protocol permits no further client bytes while daemon
/// work is running. This operation reads at most one byte and adds no timeout;
/// the caller owns the surrounding deadline or cancellation selection. Dropping
/// the future before completion does not consume a byte.
///
/// # Errors
///
/// Returns [`IpcError::UnexpectedPeerData`] when the peer sends another byte,
/// or [`IpcError::Transport`] for a read failure that does not signal closure.
pub async fn wait_for_peer_close_async(stream: &mut AsyncLocalStream) -> Result<(), IpcError> {
    let mut byte = [0_u8; 1];
    match stream.read(&mut byte).await {
        Ok(0) => Ok(()),
        Ok(_) => Err(IpcError::UnexpectedPeerData),
        Err(source) if is_peer_close_error(&source) => Ok(()),
        Err(source) => Err(IpcError::Transport(source)),
    }
}

fn is_peer_close_error(source: &io::Error) -> bool {
    matches!(
        source.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

async fn read_exact_async<R: AsyncRead + Unpin>(
    stream: &mut R,
    buffer: &mut [u8],
    duration: Duration,
) -> Result<(), IpcError> {
    timeout(duration, stream.read_exact(buffer))
        .await
        .map_err(|_| IpcError::TimedOut)?
        .map(|_| ())
        .map_err(|source| {
            if source.kind() == io::ErrorKind::UnexpectedEof {
                IpcError::UnexpectedEof
            } else {
                IpcError::Transport(source)
            }
        })
}

async fn write_all_async<W: AsyncWrite + Unpin>(
    stream: &mut W,
    buffer: &[u8],
    duration: Duration,
) -> Result<(), IpcError> {
    timeout(duration, stream.write_all(buffer))
        .await
        .map_err(|_| IpcError::TimedOut)?
        .map_err(|source| {
            if source.kind() == io::ErrorKind::WriteZero {
                IpcError::WriteZero
            } else {
                IpcError::Transport(source)
            }
        })
}

fn read_exact_bounded(
    stream: &mut impl io::Read,
    buffer: &mut [u8],
    timeout: Duration,
) -> Result<(), IpcError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(IpcError::InvalidLimit)?;
    let mut filled = 0;
    while filled < buffer.len() {
        ensure_before_deadline(deadline)?;
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
        ensure_before_deadline(deadline)?;
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

fn ensure_before_deadline(deadline: Instant) -> Result<(), IpcError> {
    if Instant::now() >= deadline {
        return Err(IpcError::TimedOut);
    }
    Ok(())
}

fn wait_for_io(deadline: Instant) -> Result<(), IpcError> {
    ensure_before_deadline(deadline)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    std::thread::sleep(RETRY_PAUSE.min(remaining));
    Ok(())
}

#[cfg(unix)]
fn validate_endpoint(path: &Path) -> Result<(), IpcError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(IpcError::InvalidEndpoint);
    }
    std::os::unix::net::SocketAddr::from_pathname(path).map_err(|_| IpcError::InvalidEndpoint)?;
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
fn verify_sync_platform_peer(stream: &LocalStream) -> Result<(), IpcError> {
    let Stream::UdSocket(socket) = stream;
    verify_unix_peer(socket)
}

#[cfg(unix)]
fn verify_async_platform_peer(stream: &AsyncLocalStream) -> Result<(), IpcError> {
    let TokioStream::UdSocket(socket) = stream;
    verify_unix_peer(socket)
}

#[cfg(target_os = "linux")]
fn verify_unix_peer(socket: &impl std::os::fd::AsFd) -> Result<(), IpcError> {
    let credentials =
        nix::sys::socket::getsockopt(socket, nix::sys::socket::sockopt::PeerCredentials).map_err(
            |source| IpcError::PeerCredentials(io::Error::from_raw_os_error(source as i32)),
        )?;
    if credentials.uid() != nix::unistd::geteuid().as_raw() {
        return Err(IpcError::PeerUnauthorized);
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn verify_unix_peer(socket: &impl std::os::fd::AsFd) -> Result<(), IpcError> {
    let (user, _) = nix::unistd::getpeereid(socket)
        .map_err(|source| IpcError::PeerCredentials(io::Error::from_raw_os_error(source as i32)))?;
    if user != nix::unistd::geteuid() {
        return Err(IpcError::PeerUnauthorized);
    }
    Ok(())
}

#[cfg(windows)]
fn verify_sync_platform_peer(_stream: &LocalStream) -> Result<(), IpcError> {
    Ok(())
}

#[cfg(windows)]
fn verify_async_platform_peer(_stream: &AsyncLocalStream) -> Result<(), IpcError> {
    Ok(())
}

#[cfg(unix)]
fn verify_endpoint_parent(endpoint: &Endpoint) -> Result<(), IpcError> {
    use std::os::unix::fs::MetadataExt as _;

    let parent = endpoint
        .as_path()
        .parent()
        .ok_or(IpcError::InvalidEndpoint)?;
    let metadata = std::fs::symlink_metadata(parent).map_err(IpcError::Transport)?;
    if !metadata.file_type().is_dir() || metadata.mode() & 0o077 != 0 {
        return Err(IpcError::InsecureEndpoint);
    }
    Ok(())
}

#[cfg(windows)]
fn verify_endpoint_parent(_endpoint: &Endpoint) -> Result<(), IpcError> {
    Ok(())
}

#[cfg(unix)]
fn platform_listener_options(
    options: ListenerOptions<'_>,
) -> Result<ListenerOptions<'_>, IpcError> {
    use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
    Ok(options.mode(0o600))
}

#[cfg(unix)]
fn unix_listener_creation_guard() -> Result<std::sync::MutexGuard<'static, ()>, IpcError> {
    UNIX_LISTENER_CREATION
        .lock()
        .map_err(|_| IpcError::SecurityPolicy)
}

#[cfg(windows)]
fn platform_listener_options(
    options: ListenerOptions<'_>,
) -> Result<ListenerOptions<'_>, IpcError> {
    use interprocess::os::windows::{
        local_socket::ListenerOptionsExt as _, security_descriptor::SecurityDescriptor,
    };
    use widestring::U16CString;

    let sddl = windows_pipe_sddl()?;
    let sddl = U16CString::from_str(sddl).map_err(|_| IpcError::SecurityPolicy)?;
    let descriptor = SecurityDescriptor::deserialize(&sddl).map_err(IpcError::Transport)?;
    Ok(options.security_descriptor(descriptor))
}

#[cfg(windows)]
fn windows_pipe_sddl() -> Result<String, IpcError> {
    let token = OwnedToken::from_current_process(windows::Win32::Security::TOKEN_QUERY)
        .map_err(|_| IpcError::SecurityPolicy)?;
    let mut allowed_sids = token
        .logon_sid()
        .map_err(|_| IpcError::SecurityPolicy)?
        .into_iter()
        .map(|group| {
            group
                .sid()
                .to_string()
                .map_err(|_| IpcError::SecurityPolicy)
        })
        .collect::<Result<Vec<_>, _>>()?;
    allowed_sids.sort_unstable();
    allowed_sids.dedup();
    if allowed_sids.is_empty() {
        allowed_sids.push(
            token
                .user()
                .and_then(|sid| sid.to_string())
                .map_err(|_| IpcError::SecurityPolicy)?,
        );
    }

    let mut sddl = String::from("D:P");
    for sid in allowed_sids {
        use std::fmt::Write as _;
        write!(&mut sddl, "(A;;GRGW;;;{sid})").map_err(|_| IpcError::SecurityPolicy)?;
    }
    Ok(sddl)
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
    /// The peer sent data where closure was the only valid protocol event.
    #[error("local IPC peer sent unexpected trailing data")]
    UnexpectedPeerData,
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
    /// The accepted peer belongs to another local user.
    #[error("local IPC peer is not authorized")]
    PeerUnauthorized,
    /// Platform peer credentials could not be inspected.
    #[error("local IPC peer credentials are unavailable")]
    PeerCredentials(#[source] io::Error),
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
    use std::{io::Cursor, sync::mpsc, thread};
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
            timeout_ms: None,
            request: Some(request_envelope::Request::Health(HealthRequest {})),
        }
    }

    fn encoded_frame(message: &impl Message) -> Vec<u8> {
        let encoded_length = message.encoded_len();
        let wire_length = u32::try_from(encoded_length).expect("test frame length fits u32");
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + encoded_length);
        frame.extend_from_slice(&wire_length.to_be_bytes());
        message.encode(&mut frame).expect("test frame encodes");
        frame
    }

    #[cfg(unix)]
    fn async_test_endpoint(temporary: &TempDir, label: &str) -> Endpoint {
        Endpoint::new(temporary.path().join(format!("{label}.sock"))).expect("endpoint is valid")
    }

    #[cfg(windows)]
    fn async_test_endpoint(_temporary: &TempDir, label: &str) -> Endpoint {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_PIPE: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_PIPE.fetch_add(1, Ordering::Relaxed);
        Endpoint::new(PathBuf::from(format!(
            r"\\.\pipe\rootlight-{label}-{}-{sequence}",
            std::process::id()
        )))
        .expect("endpoint is valid")
    }

    async fn connected_async_streams(label: &str) -> (TempDir, AsyncLocalStream, AsyncLocalStream) {
        let temporary = private_tempdir();
        let endpoint = async_test_endpoint(&temporary, label);
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("listener binds");
        let (client, server) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(connect_async(&endpoint), listener.accept())
        })
        .await
        .expect("connection setup completes");
        (
            temporary,
            client.expect("client connects"),
            server.expect("connection accepts"),
        )
    }

    #[test]
    fn endpoint_rejects_relative_paths() {
        assert!(matches!(
            Endpoint::new(PathBuf::from("rootlight.sock")),
            Err(IpcError::InvalidEndpoint)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_rejects_unrepresentable_unix_paths() {
        let path = Path::new("/").join("x".repeat(4096));
        assert!(matches!(
            Endpoint::new(path),
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
            let mut stream = listener
                .accept_timeout(Duration::from_secs(1))
                .expect("connection accepts");
            verify_peer(&stream).expect("peer is authorized");
            read_request(FrameCodec::default(), &mut stream).expect("request decodes")
        });
        ready_rx.recv().expect("server is ready");

        let mut stream = connect(&endpoint).expect("client connects");
        write_request(FrameCodec::default(), &mut stream, &request(41)).expect("request writes");

        assert_eq!(server.join().expect("server thread joins"), request(41));
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_policy_prefers_logon_sid_and_generic_io_only() {
        let token = OwnedToken::from_current_process(windows::Win32::Security::TOKEN_QUERY)
            .expect("current process token opens");
        let logon_sids = token
            .logon_sid()
            .expect("logon SID is available")
            .into_iter()
            .map(|group| group.sid().to_string().expect("logon SID formats"))
            .collect::<Vec<_>>();
        assert!(!logon_sids.is_empty(), "interactive token has a logon SID");

        let user_sid = token
            .user()
            .and_then(|sid| sid.to_string())
            .expect("user SID formats");
        let sddl = windows_pipe_sddl().expect("pipe security policy builds");

        assert!(sddl.starts_with("D:P"));
        assert!(!sddl.contains("GA"));
        assert!(!sddl.contains(&user_sid));
        for sid in logon_sids {
            assert!(sddl.contains(&format!("(A;;GRGW;;;{sid})")));
        }
    }

    #[tokio::test]
    async fn async_round_trip_preserves_one_bounded_request() {
        let temporary = private_tempdir();
        #[cfg(unix)]
        let endpoint =
            Endpoint::new(temporary.path().join("async.sock")).expect("endpoint is valid");
        #[cfg(windows)]
        let endpoint = Endpoint::new(PathBuf::from(format!(
            r"\\.\pipe\rootlight-async-{}-{}",
            std::process::id(),
            temporary.path().display().to_string().len()
        )))
        .expect("endpoint is valid");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("listener binds");
        let server = tokio::spawn(async move {
            let mut stream = listener
                .accept_timeout(Duration::from_secs(1))
                .await
                .expect("connection accepts");
            verify_peer_async(&stream).expect("peer is authorized");
            read_request_async(FrameCodec::default(), &mut stream)
                .await
                .expect("request decodes")
        });

        let mut stream = connect_async(&endpoint).await.expect("client connects");
        write_request_async(FrameCodec::default(), &mut stream, &request(73))
            .await
            .expect("request writes");

        assert_eq!(server.await.expect("server task joins"), request(73));
    }

    #[tokio::test]
    async fn peer_close_monitor_accepts_eof() {
        let (_temporary, client, mut server) = connected_async_streams("peer-close-eof").await;
        drop(client);

        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_peer_close_async(&mut server),
        )
        .await
        .expect("peer-close monitor completes")
        .expect("peer close is accepted");
    }

    #[tokio::test]
    async fn peer_close_monitor_rejects_unexpected_byte_without_a_source() {
        let (_temporary, mut client, mut server) = connected_async_streams("peer-close-byte").await;
        client
            .write_all(&[0x5a])
            .await
            .expect("unexpected byte writes");

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_peer_close_async(&mut server),
        )
        .await
        .expect("peer-close monitor completes")
        .expect_err("unexpected byte is rejected");

        assert!(matches!(error, IpcError::UnexpectedPeerData));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[tokio::test]
    async fn async_codec_accepts_fragmented_header_and_payload() {
        let expected = request(87);
        let frame = encoded_frame(&expected);
        let (mut writer, mut reader) = tokio::io::duplex(frame.len());
        let sender = tokio::spawn(async move {
            for byte in frame {
                writer.write_all(&[byte]).await.expect("fragment writes");
                tokio::task::yield_now().await;
            }
        });

        let observed = FrameCodec::default()
            .read_message_async::<RequestEnvelope, _>(&mut reader)
            .await
            .expect("fragmented frame decodes");
        sender.await.expect("fragment writer joins");

        assert_eq!(observed, expected);
    }

    #[tokio::test]
    async fn async_codec_rejects_truncated_fragmented_payload() {
        let expected = request(88);
        let mut frame = encoded_frame(&expected);
        frame.pop().expect("encoded request has a payload byte");
        let (mut writer, mut reader) = tokio::io::duplex(frame.len());
        let sender = tokio::spawn(async move {
            for fragment in frame.chunks(2) {
                writer.write_all(fragment).await.expect("fragment writes");
                tokio::task::yield_now().await;
            }
        });

        let result = FrameCodec::default()
            .read_message_async::<RequestEnvelope, _>(&mut reader)
            .await;
        sender.await.expect("fragment writer joins");

        assert!(matches!(result, Err(IpcError::UnexpectedEof)));
    }

    #[test]
    fn codec_accepts_fragmented_synchronous_reads() {
        struct FragmentedReader {
            inner: Cursor<Vec<u8>>,
            maximum_chunk: usize,
        }

        impl io::Read for FragmentedReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let limit = buffer.len().min(self.maximum_chunk);
                io::Read::read(&mut self.inner, &mut buffer[..limit])
            }
        }

        let expected = request(89);
        let frame = encoded_frame(&expected);
        let mut reader = FragmentedReader {
            inner: Cursor::new(frame),
            maximum_chunk: 1,
        };
        let observed = FrameCodec::default()
            .read_message::<RequestEnvelope, _>(&mut reader)
            .expect("fragmented frame decodes");

        assert_eq!(observed, expected);
    }

    #[tokio::test]
    async fn stalled_async_client_does_not_block_a_second_accept() {
        let temporary = private_tempdir();
        #[cfg(unix)]
        let endpoint =
            Endpoint::new(temporary.path().join("concurrent.sock")).expect("endpoint is valid");
        #[cfg(windows)]
        let endpoint = Endpoint::new(PathBuf::from(format!(
            r"\\.\pipe\rootlight-concurrent-{}-{}",
            std::process::id(),
            temporary.path().display().to_string().len()
        )))
        .expect("endpoint is valid");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("listener binds");
        let stalled_client = connect_async(&endpoint)
            .await
            .expect("stalled client connects");
        let mut stalled = listener
            .accept_timeout(Duration::from_secs(1))
            .await
            .expect("stalled connection accepts");
        let stalled_read =
            tokio::spawn(
                async move { read_request_async(FrameCodec::default(), &mut stalled).await },
            );

        let mut fast_client = connect_async(&endpoint)
            .await
            .expect("fast client connects");
        let fast_server = listener
            .accept_timeout(Duration::from_secs(1))
            .await
            .expect("fast connection accepts");
        let fast = tokio::spawn(async move {
            let mut fast_server = fast_server;
            read_request_async(FrameCodec::default(), &mut fast_server)
                .await
                .expect("fast request decodes")
        });
        write_request_async(FrameCodec::default(), &mut fast_client, &request(91))
            .await
            .expect("fast request writes");

        assert_eq!(fast.await.expect("fast task joins"), request(91));
        drop(stalled_client);
        assert!(stalled_read.await.expect("stalled task joins").is_err());
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
            let mut stream = listener
                .accept_timeout(Duration::from_secs(1))
                .expect("connection accepts");
            let codec = FrameCodec::new(16, Duration::from_secs(1)).expect("limits are valid");
            codec.read_message::<RequestEnvelope, _>(&mut stream)
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
