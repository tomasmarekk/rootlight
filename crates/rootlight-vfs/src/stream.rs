//! Stable repository capture without a whole-file memory allocation.

use std::{fmt, io::Write};

use super::{
    ContentHash, FileId, FollowSymlinks, OpenOptions, OpenOptionsFollowExt, Read, RelativePath,
    RepositoryRoot, SNAPSHOT_READ_CHUNK_BYTES, SnapshotMetadata, VfsError, checked_metadata,
    controlled,
};
use rootlight_cancel::Cancellation;

/// Identity and metadata of bytes written by a successful streamed capture.
///
/// This descriptor does not own the captured bytes. The caller must keep the
/// successful sink immutable for every generation that references the source.
/// Parser snapshots retain their separate in-memory size ceiling.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamedSourceCapture {
    file: FileId,
    path: RelativePath,
    content_hash: ContentHash,
    metadata: SnapshotMetadata,
}

impl StreamedSourceCapture {
    /// Returns the stable repository-scoped file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the canonical repository-relative source path.
    #[must_use]
    pub const fn path(&self) -> &RelativePath {
        &self.path
    }

    /// Returns the hash of every original byte written to the sink.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns source metadata agreed upon by both complete captures.
    #[must_use]
    pub const fn metadata(&self) -> SnapshotMetadata {
        self.metadata
    }
}

impl fmt::Debug for StreamedSourceCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamedSourceCapture")
            .field("file", &self.file)
            .field("content_hash", &self.content_hash)
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

/// A streamed source failed verification or could not be retained by its sink.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StreamedSourceCaptureError {
    /// Repository confinement, size, stability or cancellation checks failed.
    #[error(transparent)]
    Source(#[from] VfsError),
    /// The caller's staged sink failed to accept captured bytes.
    #[error("failed to write captured repository source")]
    Write {
        /// Underlying sink error; paths and source bytes are not included.
        #[source]
        source: std::io::Error,
    },
}

impl RepositoryRoot {
    /// Streams two stable captures, retaining the second in a caller-owned sink.
    ///
    /// The first pass hashes the complete source without retaining it. The
    /// second writes original bytes in at most 64-KiB blocks and must agree on
    /// the complete hash and filesystem metadata. `maximum_bytes` bounds disk
    /// input independently of the parser snapshot ceiling. No text encoding or
    /// language claim is inferred from a successful raw-byte capture.
    ///
    /// The sink must be empty and unpublished before this call. On failure it
    /// can contain partial or unverified bytes and must be discarded. Success
    /// does not flush, synchronize, publish or own the sink; those operations
    /// remain the caller's responsibility.
    ///
    /// # Errors
    ///
    /// Returns repository path, file type, size, stability, cancellation and
    /// sink I/O errors. Cancellation observed after an I/O operation takes
    /// precedence over that operation's result.
    pub fn capture_streamed_cancellable(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
        cancellation: &Cancellation,
        sink: &mut impl Write,
    ) -> Result<StreamedSourceCapture, StreamedSourceCaptureError> {
        let mut check = || {
            cancellation
                .check()
                .map_err(|cancelled| VfsError::Cancelled(cancelled.reason()))
        };
        if maximum_bytes == 0 {
            return Err(VfsError::InvalidByteLimit.into());
        }
        check()?;
        let first = self.capture_stream(path, maximum_bytes, &mut std::io::sink(), &mut check)?;
        check()?;
        let second = self.capture_stream(path, maximum_bytes, sink, &mut check)?;
        check()?;
        if first != second {
            return Err(VfsError::UnstableFile.into());
        }
        Ok(StreamedSourceCapture {
            file: self.file_id(path),
            path: path.clone(),
            content_hash: second.0,
            metadata: second.1,
        })
    }

    fn capture_stream(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
        sink: &mut impl Write,
        check: &mut impl FnMut() -> Result<(), VfsError>,
    ) -> Result<(ContentHash, SnapshotMetadata), StreamedSourceCaptureError> {
        let parent = self.open_parent(path, check)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = controlled(check, || {
            parent
                .open_with(path.file_name(), &options)
                .map_err(|source| VfsError::OpenFile { source })
        })?;
        let before = controlled(check, || checked_metadata(&file, maximum_bytes))?;
        let mut hasher = blake3::Hasher::new();
        let mut consumed = 0_u64;
        let read_ceiling = maximum_bytes.saturating_add(1);
        let mut buffer = [0_u8; SNAPSHOT_READ_CHUNK_BYTES];
        loop {
            check()?;
            let remaining = read_ceiling.saturating_sub(consumed);
            if remaining == 0 {
                break;
            }
            let admitted = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = controlled(check, || {
                file.read(&mut buffer[..admitted])
                    .map_err(|source| VfsError::ReadFile { source })
            })?;
            if read == 0 {
                break;
            }
            consumed = consumed
                .checked_add(u64::try_from(read).map_err(|_| VfsError::FileTooLarge {
                    maximum: maximum_bytes,
                })?)
                .filter(|bytes| *bytes <= maximum_bytes)
                .ok_or(VfsError::FileTooLarge {
                    maximum: maximum_bytes,
                })?;
            check()?;
            let written = sink.write_all(&buffer[..read]);
            check()?;
            written.map_err(|source| StreamedSourceCaptureError::Write { source })?;
            hasher.update(&buffer[..read]);
            check()?;
        }
        let after = controlled(check, || checked_metadata(&file, maximum_bytes))?;
        if before != after || after.length != consumed {
            return Err(VfsError::UnstableFile.into());
        }
        Ok((
            ContentHash::from_bytes(*hasher.finalize().as_bytes()),
            after,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_SNAPSHOT_BYTES;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{content_hash, derive_repository};
    use std::{fs, io, path::Path};

    fn fixture() -> (tempfile::TempDir, RepositoryRoot, RelativePath) {
        let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let root = RepositoryRoot::open(
            derive_repository(b"streamed-capture-test").id(),
            temporary.path(),
        )
        .unwrap();
        let path = RelativePath::parse(Path::new("source.rs")).unwrap();
        (temporary, root, path)
    }

    #[test]
    fn streamed_capture_preserves_exact_bytes_identity_and_empty_sources() {
        let (temporary, root, path) = fixture();
        for bytes in [
            b"\xef\xbb\xbffn main() { /* physical\r\n bytes */ }".as_slice(),
            b"",
        ] {
            fs::write(temporary.path().join("source.rs"), bytes).unwrap();
            let mut sink = Vec::new();
            let captured = root
                .capture_streamed_cancellable(&path, 1024, &Cancellation::new(), &mut sink)
                .unwrap();
            assert_eq!(sink, bytes);
            assert_eq!(captured.file(), root.file_id(&path));
            assert_eq!(captured.path(), &path);
            assert_eq!(captured.metadata().length, bytes.len() as u64);
            assert_eq!(captured.content_hash(), content_hash(bytes));
        }
    }

    #[test]
    fn streamed_capture_crosses_snapshot_ceiling_with_bounded_sink_blocks() {
        let (temporary, root, path) = fixture();
        let mut source = fs::File::create(temporary.path().join("source.rs")).unwrap();
        let block = [b' '; SNAPSHOT_READ_CHUNK_BYTES];
        let mut expected = blake3::Hasher::new();
        for _ in 0..MAX_SNAPSHOT_BYTES / block.len() as u64 {
            source.write_all(&block).unwrap();
            expected.update(&block);
        }
        source.write_all(b"\n").unwrap();
        expected.update(b"\n");
        drop(source);
        struct DigestSink {
            hash: blake3::Hasher,
            bytes: u64,
            largest_block: usize,
        }
        impl Write for DigestSink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.hash.update(bytes);
                self.bytes += bytes.len() as u64;
                self.largest_block = self.largest_block.max(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut sink = DigestSink {
            hash: blake3::Hasher::new(),
            bytes: 0,
            largest_block: 0,
        };
        let captured = root
            .capture_streamed_cancellable(
                &path,
                MAX_SNAPSHOT_BYTES + 1,
                &Cancellation::new(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(sink.bytes, MAX_SNAPSHOT_BYTES + 1);
        assert!(sink.largest_block <= SNAPSHOT_READ_CHUNK_BYTES);
        assert_eq!(sink.hash.finalize(), expected.finalize());
        assert_eq!(
            captured.content_hash().as_bytes(),
            expected.finalize().as_bytes()
        );
        assert_eq!(captured.metadata().length, MAX_SNAPSHOT_BYTES + 1);
        assert!(matches!(
            root.snapshot(&path, u64::MAX),
            Err(VfsError::FileTooLarge {
                maximum: MAX_SNAPSHOT_BYTES
            })
        ));
    }

    #[test]
    fn streamed_capture_enforces_caller_limits_before_writing() {
        let (temporary, root, path) = fixture();
        fs::write(temporary.path().join("source.rs"), b"four").unwrap();
        let mut sink = Vec::new();
        assert!(matches!(
            root.capture_streamed_cancellable(&path, 3, &Cancellation::new(), &mut sink,),
            Err(StreamedSourceCaptureError::Source(VfsError::FileTooLarge {
                maximum: 3
            }))
        ));
        assert!(sink.is_empty());
        assert!(matches!(
            root.capture_streamed_cancellable(&path, 0, &Cancellation::new(), &mut sink,),
            Err(StreamedSourceCaptureError::Source(
                VfsError::InvalidByteLimit
            ))
        ));
    }

    #[test]
    fn streamed_capture_preserves_sink_error_and_post_write_cancellation() {
        let (temporary, root, path) = fixture();
        fs::write(temporary.path().join("source.rs"), b"source").unwrap();
        struct FailingSink<'a> {
            cancellation: Option<&'a Cancellation>,
        }
        impl Write for FailingSink<'_> {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                if let Some(cancellation) = self.cancellation {
                    cancellation.cancel(CancellationReason::ClientRequest);
                }
                Err(io::Error::other("staged sink failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let cancellation = Cancellation::new();
        assert!(matches!(root.capture_streamed_cancellable(
            &path, 1024, &cancellation, &mut FailingSink { cancellation: None },
        ), Err(StreamedSourceCaptureError::Write { source }) if source.kind() == io::ErrorKind::Other));
        assert!(matches!(
            root.capture_streamed_cancellable(
                &path,
                1024,
                &cancellation,
                &mut FailingSink {
                    cancellation: Some(&cancellation)
                },
            ),
            Err(StreamedSourceCaptureError::Source(VfsError::Cancelled(
                CancellationReason::ClientRequest
            )))
        ));
    }

    #[test]
    fn streamed_capture_rejects_second_pass_mutation_and_non_regular_sources() {
        let (temporary, root, path) = fixture();
        let absolute = temporary.path().join("source.rs");
        fs::write(&absolute, [b'a'; SNAPSHOT_READ_CHUNK_BYTES * 3]).unwrap();
        struct MutatingSink<'a> {
            source: &'a Path,
            mutated: bool,
        }
        impl Write for MutatingSink<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.mutated {
                    fs::write(self.source, [b'b'; SNAPSHOT_READ_CHUNK_BYTES * 3])?;
                    self.mutated = true;
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(matches!(
            root.capture_streamed_cancellable(
                &path,
                MAX_SNAPSHOT_BYTES,
                &Cancellation::new(),
                &mut MutatingSink {
                    source: &absolute,
                    mutated: false
                },
            ),
            Err(StreamedSourceCaptureError::Source(VfsError::UnstableFile))
        ));
        let directory = RelativePath::parse(Path::new("directory")).unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        assert!(
            root.capture_streamed_cancellable(
                &directory,
                1024,
                &Cancellation::new(),
                &mut Vec::new(),
            )
            .is_err()
        );
    }
}
