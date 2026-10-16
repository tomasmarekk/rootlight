//! Bounded raw source windows authenticated over one complete private-file pass.
//!
//! The retained private owner protects storage access; callers must separately
//! establish repository and generation membership before supplying an identity.

use std::{ffi::OsStr, fmt, ops::Range};

use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;

use crate::{
    StreamingSourceTextValidator, TextEncoding,
    platform::{PlatformError, PrivateDirectory},
};

/// Expected identity of the complete original source, including its physical encoding.
///
/// These values must come from the caller's authenticated file record. They do
/// not establish repository or generation membership by themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamedSourceIdentity {
    /// Complete original byte length, including any encoding BOM.
    pub byte_length: u64,
    /// BLAKE3 hash of every original byte, including any encoding BOM.
    pub content_hash: ContentHash,
    /// Encoding verified for the complete original source.
    pub encoding: TextEncoding,
}

/// Expected identity of complete original bytes without a text-encoding claim.
///
/// The caller must obtain both values from an authorized immutable file record;
/// they do not independently establish repository or generation membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamedSourceBytesIdentity {
    /// Complete original byte length, including any physical encoding prefix.
    pub byte_length: u64,
    /// BLAKE3 hash of every original byte.
    pub content_hash: ContentHash,
}

impl From<StreamedSourceIdentity> for StreamedSourceBytesIdentity {
    fn from(identity: StreamedSourceIdentity) -> Self {
        Self {
            byte_length: identity.byte_length,
            content_hash: identity.content_hash,
        }
    }
}

/// Explicit caller limits for a response and its full-source verification work.
///
/// Neither limit changes parser or source-snapshot admission limits. Zero limits
/// admit only an empty response or empty complete source, respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRangeReadBudget {
    /// Maximum owned response bytes; working memory also includes fixed buffers.
    pub maximum_response_bytes: usize,
    /// Maximum complete private-file bytes read and verified for this request.
    pub maximum_verified_bytes: u64,
}

/// An owned raw window whose complete source passed private-file and content checks.
///
/// Byte ranges may split encoded scalar values or UTF-16 code units. Text-boundary
/// presentation and repository/generation authorization remain caller obligations.
/// Debug output excludes source bytes.
#[derive(PartialEq, Eq)]
pub struct VerifiedSourceRange {
    identity: StreamedSourceIdentity,
    range: Range<u64>,
    bytes: Vec<u8>,
}

impl VerifiedSourceRange {
    /// Returns the verified identity of the complete original source.
    #[must_use]
    pub const fn identity(&self) -> StreamedSourceIdentity {
        self.identity
    }

    /// Returns the exact half-open original-byte range represented by this window.
    #[must_use]
    pub fn range(&self) -> Range<u64> {
        self.range.clone()
    }

    /// Borrows the exact original bytes in the requested range.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Transfers ownership of the exact original bytes in the requested range.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl fmt::Debug for VerifiedSourceRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedSourceRange")
            .field("identity", &self.identity)
            .field("range", &self.range)
            .field("byte_count", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Failures that prevent an authenticated raw source window from being returned.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceRangeReadError {
    /// The range is reversed or extends beyond the expected original length.
    #[error("source byte range is outside the expected source")]
    InvalidRange,
    /// The requested window exceeds the response limit or addressable storage.
    #[error("source byte range exceeds the response limit of {maximum} bytes")]
    ResponseLimit {
        /// Caller-supplied maximum response size.
        maximum: usize,
    },
    /// The expected complete source exceeds the caller's verification limit.
    #[error("complete source exceeds the verification limit of {maximum} bytes")]
    VerificationLimit {
        /// Caller-supplied maximum complete-source verification size.
        maximum: u64,
    },
    /// The bounded response allocation could not be reserved.
    #[error("unable to reserve the bounded source response")]
    Allocation(#[source] std::collections::TryReserveError),
    /// Private storage rejected a name, file, traversal, cancellation or I/O operation.
    #[error(transparent)]
    Private(#[from] PlatformError),
    /// The fully traversed file length differs from the expected source length.
    #[error("complete source length differs from the expected length")]
    LengthMismatch {
        /// Expected complete original byte length.
        expected: u64,
        /// Actual complete traversed byte length.
        actual: u64,
    },
    /// The complete original-byte hash differs from the expected content hash.
    #[error("complete source content hash differs from the expected hash")]
    ContentHashMismatch,
    /// The complete source is not valid UTF-8 or strictly admitted BOM-backed UTF-16.
    #[error("complete source has an invalid or unsupported text encoding")]
    InvalidEncoding,
    /// The complete source is valid but uses a different physical encoding.
    #[error("complete source encoding differs from the expected encoding")]
    EncodingMismatch {
        /// Encoding recorded with the expected source.
        expected: TextEncoding,
        /// Encoding verified over every original byte.
        actual: TextEncoding,
    },
}

/// Reads an exact raw window while authenticating the complete private source once.
///
/// The range and both budgets are checked before file I/O. One guarded private
/// traversal hashes and strictly validates every byte, staging only the requested
/// overlap. Success requires EOF, stable private-file identity and length, the
/// expected complete length/hash/encoding, and no cancellation. Even an empty
/// window verifies the complete source. No source-sized allocation or second read
/// is performed; working memory is the bounded response plus fixed buffers.
///
/// The caller must resolve the expected identity from its authorized file record
/// and retain the generation's private owner and any required lease throughout
/// this call. This function does not prove that the private file belongs to that
/// repository or generation. Raw ranges may split Unicode encoding units.
///
/// # Errors
/// Returns a typed range, budget, allocation, private-storage, cancellation,
/// length, content-hash or encoding error. No staged bytes escape on failure.
pub fn read_verified_source_range_cancellable(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    expected: StreamedSourceIdentity,
    range: Range<u64>,
    budget: SourceRangeReadBudget,
    cancellation: &Cancellation,
) -> Result<VerifiedSourceRange, SourceRangeReadError> {
    verify_source_range(expected, range, budget, cancellation, |visitor| {
        directory.read_file_streamed_cancellable(
            name,
            budget.maximum_verified_bytes,
            cancellation,
            visitor,
        )
    })
}

/// Visits ordered original bytes while authenticating the complete private source.
///
/// Each callback is provisional. The visitor must stage its bounded response or
/// matches and publish nothing until this function returns successfully. Success
/// requires stable private metadata, EOF, the exact complete length and hash,
/// strict physical encoding, and no cancellation. No complete source is retained.
/// Repository/generation membership and the private owner's lifetime remain the
/// caller's responsibility, as with [`read_verified_source_range_cancellable`].
///
/// # Errors
/// Returns a typed verification-limit, private-storage, callback, cancellation,
/// length, content-hash or encoding failure. Callback failures stop traversal;
/// previously visited bytes do not establish a verified successful prefix.
pub fn visit_verified_source_cancellable(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    expected: StreamedSourceIdentity,
    maximum_verified_bytes: u64,
    cancellation: &Cancellation,
    visitor: impl FnMut(u64, &[u8]) -> Result<(), PlatformError>,
) -> Result<u64, SourceRangeReadError> {
    verify_source_traversal(
        expected,
        maximum_verified_bytes,
        cancellation,
        |visitor| {
            directory.read_file_streamed_cancellable(
                name,
                maximum_verified_bytes,
                cancellation,
                visitor,
            )
        },
        visitor,
    )
}

/// Visits original bytes while authenticating their complete private identity.
///
/// Callbacks are provisional until successful return. No text-encoding support
/// is inferred; a text consumer must additionally validate the complete stream
/// before publishing staged output. This permits exact raw availability for
/// source encodings that a text consumer cannot interpret safely.
///
/// # Errors
/// Returns typed verification-limit, private-file, callback, cancellation,
/// length or full content-hash failures. No successful-prefix claim is returned.
pub fn visit_verified_source_bytes_cancellable(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    expected: StreamedSourceBytesIdentity,
    maximum_verified_bytes: u64,
    cancellation: &Cancellation,
    visitor: impl FnMut(u64, &[u8]) -> Result<(), PlatformError>,
) -> Result<u64, SourceRangeReadError> {
    verify_source_bytes_traversal(
        expected,
        maximum_verified_bytes,
        cancellation,
        |visitor| {
            directory.read_file_streamed_cancellable(
                name,
                maximum_verified_bytes,
                cancellation,
                visitor,
            )
        },
        visitor,
    )
}

fn verify_source_range(
    expected: StreamedSourceIdentity,
    range: Range<u64>,
    budget: SourceRangeReadBudget,
    cancellation: &Cancellation,
    read: impl FnOnce(
        &mut dyn FnMut(u64, &[u8]) -> Result<(), PlatformError>,
    ) -> Result<u64, PlatformError>,
) -> Result<VerifiedSourceRange, SourceRangeReadError> {
    if range.start > range.end || range.end > expected.byte_length {
        return Err(SourceRangeReadError::InvalidRange);
    }
    let response_length = usize::try_from(range.end - range.start)
        .ok()
        .filter(|length| *length <= budget.maximum_response_bytes)
        .ok_or(SourceRangeReadError::ResponseLimit {
            maximum: budget.maximum_response_bytes,
        })?;
    if expected.byte_length > budget.maximum_verified_bytes {
        return Err(SourceRangeReadError::VerificationLimit {
            maximum: budget.maximum_verified_bytes,
        });
    }
    check_cancelled(cancellation)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(response_length)
        .map_err(SourceRangeReadError::Allocation)?;
    verify_source_traversal(
        expected,
        budget.maximum_verified_bytes,
        cancellation,
        read,
        |offset, chunk| {
            let next = offset
                .checked_add(u64::try_from(chunk.len()).map_err(|_| PlatformError::ResourceLimit)?)
                .ok_or(PlatformError::ResourceLimit)?;
            let start = range.start.max(offset);
            let end = range.end.min(next);
            if start < end {
                let start =
                    usize::try_from(start - offset).map_err(|_| PlatformError::ResourceLimit)?;
                let end =
                    usize::try_from(end - offset).map_err(|_| PlatformError::ResourceLimit)?;
                let new_length = bytes
                    .len()
                    .checked_add(end - start)
                    .filter(|length| *length <= response_length)
                    .ok_or(PlatformError::ResourceLimit)?;
                bytes.extend_from_slice(&chunk[start..end]);
                debug_assert_eq!(bytes.len(), new_length);
            }
            Ok(())
        },
    )?;
    if bytes.len() != response_length {
        return Err(PlatformError::SecurityPolicy.into());
    }
    Ok(VerifiedSourceRange {
        identity: expected,
        range,
        bytes,
    })
}

fn verify_source_traversal(
    expected: StreamedSourceIdentity,
    maximum_verified_bytes: u64,
    cancellation: &Cancellation,
    read: impl FnOnce(
        &mut dyn FnMut(u64, &[u8]) -> Result<(), PlatformError>,
    ) -> Result<u64, PlatformError>,
    mut visitor: impl FnMut(u64, &[u8]) -> Result<(), PlatformError>,
) -> Result<u64, SourceRangeReadError> {
    let mut text = StreamingSourceTextValidator::new();
    let length = verify_source_bytes_traversal(
        expected.into(),
        maximum_verified_bytes,
        cancellation,
        read,
        |offset, chunk| {
            text.push(chunk, cancellation)
                .map_err(|cancelled| PlatformError::Cancelled(cancelled.reason()))?;
            visitor(offset, chunk)
        },
    )?;
    let encoding = text.finish().ok_or(SourceRangeReadError::InvalidEncoding)?;
    if encoding != expected.encoding {
        return Err(SourceRangeReadError::EncodingMismatch {
            expected: expected.encoding,
            actual: encoding,
        });
    }
    check_cancelled(cancellation)?;
    Ok(length)
}

fn verify_source_bytes_traversal(
    expected: StreamedSourceBytesIdentity,
    maximum_verified_bytes: u64,
    cancellation: &Cancellation,
    read: impl FnOnce(
        &mut dyn FnMut(u64, &[u8]) -> Result<(), PlatformError>,
    ) -> Result<u64, PlatformError>,
    mut visitor: impl FnMut(u64, &[u8]) -> Result<(), PlatformError>,
) -> Result<u64, SourceRangeReadError> {
    if expected.byte_length > maximum_verified_bytes {
        return Err(SourceRangeReadError::VerificationLimit {
            maximum: maximum_verified_bytes,
        });
    }
    check_cancelled(cancellation)?;
    let mut hash = blake3::Hasher::new();
    let mut consumed = 0_u64;
    let length = read(&mut |offset, chunk| {
        if offset != consumed {
            return Err(PlatformError::SecurityPolicy);
        }
        let next = offset
            .checked_add(u64::try_from(chunk.len()).map_err(|_| PlatformError::ResourceLimit)?)
            .ok_or(PlatformError::ResourceLimit)?;
        hash.update(chunk);
        visitor(offset, chunk)?;
        consumed = next;
        Ok(())
    })?;
    check_cancelled(cancellation)?;
    if length != expected.byte_length {
        return Err(SourceRangeReadError::LengthMismatch {
            expected: expected.byte_length,
            actual: length,
        });
    }
    if consumed != length {
        return Err(PlatformError::SecurityPolicy.into());
    }
    if ContentHash::from_bytes(*hash.finalize().as_bytes()) != expected.content_hash {
        return Err(SourceRangeReadError::ContentHashMismatch);
    }
    check_cancelled(cancellation)?;
    Ok(length)
}

fn check_cancelled(cancellation: &Cancellation) -> Result<(), PlatformError> {
    cancellation
        .check()
        .map_err(|cancelled| PlatformError::Cancelled(cancelled.reason()))
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::{fs, io, io::Write as _, path::Path};

    use cap_std::{ambient_authority, fs::Dir};
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{content_hash, derive_repository};

    use crate::{MAX_SNAPSHOT_BYTES, RelativePath, RepositoryRoot};

    const CHUNK: usize = 64 * 1024;

    fn fixture() -> (tempfile::TempDir, PrivateDirectory<'static>) {
        let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        #[cfg(windows)]
        {
            use nt_token::OwnedToken;
            use windows::Win32::Security::TOKEN_QUERY;
            use windows_permissions::{
                LocalBox, SecurityDescriptor,
                constants::{SeObjectType, SecurityInformation},
                wrappers::SetNamedSecurityInfo,
            };

            let sid = OwnedToken::from_current_process(TOKEN_QUERY)
                .unwrap()
                .user()
                .unwrap()
                .to_string()
                .unwrap();
            let descriptor: LocalBox<SecurityDescriptor> =
                format!("D:P(A;;FA;;;{sid})").parse().unwrap();
            SetNamedSecurityInfo(
                temporary.path().as_os_str(),
                SeObjectType::SE_FILE_OBJECT,
                SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
                None,
                None,
                descriptor.dacl(),
                None,
            )
            .unwrap();
        }
        let mut parent = Dir::open_ambient_dir(temporary.path(), ambient_authority()).unwrap();
        PrivateDirectory::secure_new_parent(&mut parent).unwrap();
        let directory = PrivateDirectory::create(&parent, OsStr::new("private")).unwrap();
        (temporary, directory)
    }

    fn store(directory: &PrivateDirectory<'_>, name: &OsStr, bytes: &[u8]) {
        let mut file = directory.create_file(name).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn identity(bytes: &[u8], encoding: TextEncoding) -> StreamedSourceIdentity {
        StreamedSourceIdentity {
            byte_length: bytes.len() as u64,
            content_hash: content_hash(bytes),
            encoding,
        }
    }

    fn budget(length: u64, maximum_response_bytes: usize) -> SourceRangeReadBudget {
        SourceRangeReadBudget {
            maximum_response_bytes,
            maximum_verified_bytes: length,
        }
    }

    fn encode(text: &str, encoding: TextEncoding) -> Vec<u8> {
        match encoding {
            TextEncoding::Utf8 => text.as_bytes().to_vec(),
            TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                let mut bytes = match encoding {
                    TextEncoding::Utf16Le => vec![0xff, 0xfe],
                    TextEncoding::Utf16Be => vec![0xfe, 0xff],
                    TextEncoding::Utf8 => unreachable!(),
                };
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&match encoding {
                        TextEncoding::Utf16Le => unit.to_le_bytes(),
                        TextEncoding::Utf16Be => unit.to_be_bytes(),
                        TextEncoding::Utf8 => unreachable!(),
                    });
                }
                bytes
            }
        }
    }

    #[test]
    fn verified_visitors_preserve_offsets_and_raw_only_encodings() {
        let (_temporary, directory) = fixture();
        for (index, encoding) in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = encode(&format!("{}λ😀tail", "x".repeat(CHUNK + 3)), encoding);
            let expected = identity(&bytes, encoding);
            let name = format!("visitor-{index}");
            store(&directory, OsStr::new(&name), &bytes);
            let mut consumed = 0;
            let mut callbacks = 0;
            let length = visit_verified_source_cancellable(
                &directory,
                OsStr::new(&name),
                expected,
                expected.byte_length,
                &Cancellation::new(),
                |offset, chunk| {
                    assert_eq!(offset, consumed);
                    let start = usize::try_from(offset).unwrap();
                    assert_eq!(chunk, &bytes[start..start + chunk.len()]);
                    consumed += u64::try_from(chunk.len()).unwrap();
                    callbacks += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(length, expected.byte_length);
            assert_eq!(consumed, length);
            assert!(callbacks > 1);
        }
        let bytes = b"raw Latin-1 spelling: caf\xe9";
        store(&directory, OsStr::new("raw-only"), bytes);
        let expected = StreamedSourceBytesIdentity {
            byte_length: u64::try_from(bytes.len()).unwrap(),
            content_hash: content_hash(bytes),
        };
        let mut staged = Vec::new();
        assert_eq!(
            visit_verified_source_bytes_cancellable(
                &directory,
                OsStr::new("raw-only"),
                expected,
                expected.byte_length,
                &Cancellation::new(),
                |_, chunk| {
                    staged.extend_from_slice(chunk);
                    Ok(())
                },
            )
            .unwrap(),
            expected.byte_length
        );
        assert_eq!(staged, bytes);
        assert!(matches!(
            visit_verified_source_cancellable(
                &directory,
                OsStr::new("raw-only"),
                StreamedSourceIdentity {
                    byte_length: expected.byte_length,
                    content_hash: expected.content_hash,
                    encoding: TextEncoding::Utf8,
                },
                expected.byte_length,
                &Cancellation::new(),
                |_, _| Ok(()),
            ),
            Err(SourceRangeReadError::InvalidEncoding)
        ));
    }

    #[test]
    fn verified_visitors_reject_limits_hash_callback_and_cancellation() {
        let (_temporary, directory) = fixture();
        let bytes = vec![b'x'; CHUNK + 7];
        let expected = identity(&bytes, TextEncoding::Utf8);
        store(&directory, OsStr::new("visitor"), &bytes);
        assert!(matches!(
            visit_verified_source_bytes_cancellable(
                &directory,
                OsStr::new("missing"),
                expected.into(),
                expected.byte_length - 1,
                &Cancellation::new(),
                |_, _| panic!("verification limit precedes I/O"),
            ),
            Err(SourceRangeReadError::VerificationLimit { .. })
        ));
        assert!(matches!(
            visit_verified_source_bytes_cancellable(
                &directory,
                OsStr::new("visitor"),
                StreamedSourceBytesIdentity {
                    content_hash: content_hash(b"other"),
                    ..expected.into()
                },
                expected.byte_length,
                &Cancellation::new(),
                |_, _| Ok(()),
            ),
            Err(SourceRangeReadError::ContentHashMismatch)
        ));
        assert!(matches!(
            visit_verified_source_bytes_cancellable(
                &directory,
                OsStr::new("visitor"),
                expected.into(),
                expected.byte_length,
                &Cancellation::new(),
                |_, _| Err(PlatformError::ResourceLimit),
            ),
            Err(SourceRangeReadError::Private(PlatformError::ResourceLimit))
        ));
        let cancellation = Cancellation::new();
        assert!(matches!(
            visit_verified_source_bytes_cancellable(
                &directory,
                OsStr::new("visitor"),
                expected.into(),
                expected.byte_length,
                &cancellation,
                |_, _| {
                    cancellation.cancel(CancellationReason::ClientRequest);
                    Ok(())
                },
            ),
            Err(SourceRangeReadError::Private(PlatformError::Cancelled(
                CancellationReason::ClientRequest
            )))
        ));
    }

    #[test]
    fn exact_raw_windows_cross_chunks_and_encoding_units() {
        let (_temporary, directory) = fixture();
        for (index, encoding) in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ]
        .into_iter()
        .enumerate()
        {
            let text = format!("{}λ😀\r\n{}tail", "a".repeat(CHUNK - 2), "b".repeat(CHUNK));
            let bytes = encode(&text, encoding);
            let expected = identity(&bytes, encoding);
            let name = format!("source-{index}");
            store(&directory, OsStr::new(&name), &bytes);
            let length = expected.byte_length;
            let unicode_offset = match encoding {
                TextEncoding::Utf8 => (CHUNK - 2) as u64,
                TextEncoding::Utf16Le | TextEncoding::Utf16Be => (2 + (CHUNK - 2) * 2) as u64,
            };
            for range in [
                0..7,
                (CHUNK as u64 - 3)..(CHUNK as u64 + 9),
                1..2,
                (unicode_offset + 3)..(unicode_offset + 5),
                (length - 9)..length,
                length..length,
                0..0,
            ] {
                let result = read_verified_source_range_cancellable(
                    &directory,
                    OsStr::new(&name),
                    expected,
                    range.clone(),
                    budget(length, 12),
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(result.identity(), expected);
                assert_eq!(result.range(), range);
                let selected =
                    usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap();
                assert_eq!(result.bytes(), &bytes[selected.clone()]);
                assert!(result.bytes.capacity() <= 12);
                assert!(!format!("{result:?}").contains("tail"));
                assert_eq!(result.into_bytes(), bytes[selected]);
            }
        }
        store(&directory, OsStr::new("empty"), b"");
        let empty = read_verified_source_range_cancellable(
            &directory,
            OsStr::new("empty"),
            identity(b"", TextEncoding::Utf8),
            0..0,
            budget(0, 0),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(empty.bytes().is_empty());
    }

    #[test]
    fn unselected_malformed_tail_and_wrong_encoding_never_return_a_window() {
        let (_temporary, directory) = fixture();
        for (index, encoding) in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ]
        .into_iter()
        .enumerate()
        {
            let mut bytes = encode(&"a".repeat(CHUNK + 16), encoding);
            bytes.push(0xff);
            let name = format!("malformed-{index}");
            store(&directory, OsStr::new(&name), &bytes);
            let expected = identity(&bytes, encoding);
            for range in [0..4, expected.byte_length..expected.byte_length] {
                assert!(matches!(
                    read_verified_source_range_cancellable(
                        &directory,
                        OsStr::new(&name),
                        expected,
                        range,
                        budget(expected.byte_length, 4),
                        &Cancellation::new(),
                    ),
                    Err(SourceRangeReadError::InvalidEncoding)
                ));
            }
            let valid = encode("exact λ😀", encoding);
            let name = format!("wrong-encoding-{index}");
            store(&directory, OsStr::new(&name), &valid);
            let wrong = if encoding == TextEncoding::Utf8 {
                TextEncoding::Utf16Le
            } else {
                TextEncoding::Utf8
            };
            assert!(matches!(
                read_verified_source_range_cancellable(
                    &directory, OsStr::new(&name), identity(&valid, wrong), 0..1,
                    budget(valid.len() as u64, 1), &Cancellation::new(),
                ),
                Err(SourceRangeReadError::EncodingMismatch { expected, actual })
                    if expected == wrong && actual == encoding
            ));
        }
    }

    #[test]
    fn whole_hash_and_length_are_checked_outside_the_requested_window() {
        let (_temporary, directory) = fixture();
        let original = vec![b'a'; CHUNK * 2 + 3];
        let mut changed = original.clone();
        *changed.last_mut().unwrap() = b'b';
        store(&directory, OsStr::new("changed"), &changed);
        let expected = identity(&original, TextEncoding::Utf8);
        assert!(matches!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("changed"),
                expected,
                0..4,
                budget(expected.byte_length, 4),
                &Cancellation::new(),
            ),
            Err(SourceRangeReadError::ContentHashMismatch)
        ));
        for (name, actual) in [("short", b"abc".as_slice()), ("long", b"abcde".as_slice())] {
            store(&directory, OsStr::new(name), actual);
            assert!(matches!(
                read_verified_source_range_cancellable(
                    &directory, OsStr::new(name), identity(b"abcd", TextEncoding::Utf8), 0..1,
                    budget(5, 1), &Cancellation::new(),
                ),
                Err(SourceRangeReadError::LengthMismatch { expected: 4, actual: length })
                    if length == actual.len() as u64
            ));
        }
    }

    #[test]
    fn range_and_budget_failures_precede_file_io() {
        let (_temporary, directory) = fixture();
        let expected = identity(b"four", TextEncoding::Utf8);
        let read = |range, limits| {
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("missing"),
                expected,
                range,
                limits,
                &Cancellation::new(),
            )
        };
        assert!(matches!(
            read(Range { start: 3, end: 2 }, budget(4, 4)),
            Err(SourceRangeReadError::InvalidRange)
        ));
        assert!(matches!(
            read(0..5, budget(4, 5)),
            Err(SourceRangeReadError::InvalidRange)
        ));
        assert!(matches!(
            read(0..4, budget(4, 3)),
            Err(SourceRangeReadError::ResponseLimit { maximum: 3 })
        ));
        assert!(matches!(
            read(0..1, budget(3, 1)),
            Err(SourceRangeReadError::VerificationLimit { maximum: 3 })
        ));
        let huge = StreamedSourceIdentity {
            byte_length: u64::MAX,
            ..expected
        };
        assert!(matches!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("missing"),
                huge,
                0..u64::MAX,
                budget(u64::MAX, usize::MAX),
                &Cancellation::new(),
            ),
            Err(SourceRangeReadError::Allocation(_) | SourceRangeReadError::ResponseLimit { .. })
        ));
        store(&directory, OsStr::new("too-long"), b"five!");
        assert!(matches!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("too-long"),
                expected,
                0..1,
                budget(4, 1),
                &Cancellation::new(),
            ),
            Err(SourceRangeReadError::Private(PlatformError::ResourceLimit))
        ));
    }

    #[test]
    fn cancellation_discards_staged_bytes_before_and_during_private_traversal() {
        let (_temporary, directory) = fixture();
        let bytes = vec![b'a'; CHUNK * 3];
        store(&directory, OsStr::new("source"), &bytes);
        let expected = identity(&bytes, TextEncoding::Utf8);
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("source"),
                expected,
                0..4,
                budget(expected.byte_length, 4),
                &cancelled,
            ),
            Err(SourceRangeReadError::Private(PlatformError::Cancelled(
                CancellationReason::ClientRequest
            )))
        ));
        for after_last in [false, true] {
            let cancellation = Cancellation::new();
            let mut visited = 0;
            let result = verify_source_range(
                expected,
                0..4,
                budget(expected.byte_length, 4),
                &cancellation,
                |visitor| {
                    directory.read_file_streamed_cancellable(
                        OsStr::new("source"),
                        expected.byte_length,
                        &cancellation,
                        |offset, chunk| {
                            visitor(offset, chunk)?;
                            visited += chunk.len();
                            if !after_last || visited == bytes.len() {
                                cancellation.cancel(CancellationReason::ClientRequest);
                            }
                            Ok(())
                        },
                    )
                },
            );
            assert!(matches!(
                result,
                Err(SourceRangeReadError::Private(PlatformError::Cancelled(
                    CancellationReason::ClientRequest
                )))
            ));
            assert_eq!(visited, if after_last { bytes.len() } else { CHUNK });
        }
    }

    #[test]
    fn private_link_and_regular_file_guards_are_inherited() {
        let (_temporary, directory) = fixture();
        store(&directory, OsStr::new("source"), b"source");
        directory
            .capability()
            .hard_link("source", directory.capability(), "alias")
            .unwrap();
        let expected = identity(b"source", TextEncoding::Utf8);
        assert!(matches!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("source"),
                expected,
                0..1,
                budget(expected.byte_length, 1),
                &Cancellation::new(),
            ),
            Err(SourceRangeReadError::Private(PlatformError::SecurityPolicy))
        ));
        let _child = directory.create_directory(OsStr::new("directory")).unwrap();
        assert!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("directory"),
                expected,
                0..1,
                budget(expected.byte_length, 1),
                &Cancellation::new(),
            )
            .is_err()
        );
        assert!(matches!(
            read_verified_source_range_cancellable(
                &directory,
                OsStr::new("../source"),
                expected,
                0..1,
                budget(expected.byte_length, 1),
                &Cancellation::new(),
            ),
            Err(SourceRangeReadError::Private(PlatformError::InvalidName))
        ));
    }

    #[test]
    fn captured_source_above_snapshot_cap_remains_exact_after_live_source_changes() {
        let (temporary, directory) = fixture();
        let root = RepositoryRoot::open(
            derive_repository(b"verified-streamed-range-test").id(),
            temporary.path(),
        )
        .unwrap();
        let path = RelativePath::parse(Path::new("source.rs")).unwrap();
        let absolute = temporary.path().join("source.rs");
        let mut source = fs::File::create(&absolute).unwrap();
        let block = [b'a'; CHUNK];
        for _ in 0..MAX_SNAPSHOT_BYTES / CHUNK as u64 {
            source.write_all(&block).unwrap();
        }
        source.write_all(b"z").unwrap();
        drop(source);
        assert!(matches!(
            root.snapshot(&path, u64::MAX),
            Err(crate::VfsError::FileTooLarge {
                maximum: MAX_SNAPSHOT_BYTES
            })
        ));
        struct Tee<'a, W> {
            file: W,
            text: StreamingSourceTextValidator,
            cancellation: &'a Cancellation,
            largest_chunk: usize,
        }
        impl<W: io::Write> io::Write for Tee<'_, W> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let written = self.file.write(bytes)?;
                self.text
                    .push(&bytes[..written], self.cancellation)
                    .map_err(io::Error::other)?;
                self.largest_chunk = self.largest_chunk.max(written);
                Ok(written)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.file.flush()
            }
        }
        let cancellation = Cancellation::new();
        let mut sink = Tee {
            file: directory.create_file(OsStr::new("captured")).unwrap(),
            text: StreamingSourceTextValidator::new(),
            cancellation: &cancellation,
            largest_chunk: 0,
        };
        let captured = root
            .capture_streamed_cancellable(&path, MAX_SNAPSHOT_BYTES + 1, &cancellation, &mut sink)
            .unwrap();
        sink.file.sync_all().unwrap();
        assert!(sink.largest_chunk <= CHUNK);
        let encoding = sink.text.finish().unwrap();
        drop(sink.file);
        let expected = StreamedSourceIdentity {
            byte_length: captured.metadata().length,
            content_hash: captured.content_hash(),
            encoding,
        };
        assert_eq!(expected.byte_length, MAX_SNAPSHOT_BYTES + 1);
        fs::write(&absolute, b"changed live source").unwrap();
        let result = read_verified_source_range_cancellable(
            &directory,
            OsStr::new("captured"),
            expected,
            (MAX_SNAPSHOT_BYTES - 3)..(MAX_SNAPSHOT_BYTES + 1),
            budget(expected.byte_length, 4),
            &cancellation,
        )
        .unwrap();
        assert_eq!(result.bytes(), b"aaaz");
        assert!(result.bytes.capacity() <= 4);
        assert_eq!(fs::read(&absolute).unwrap(), b"changed live source");
    }
}
