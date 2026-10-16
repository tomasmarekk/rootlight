//! Streaming tracked-file inventory for discovery, independent of Git history budgets.
//! Only a bounded stage record and unique regular-file paths are retained; a
//! failed command, incomplete record or cancellation never publishes a prefix.

use std::{
    collections::BTreeSet,
    io::Read,
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;

use super::{GitCollectError, GitCollectLimits, GitCollectOperation, GitRunner, kill_and_reap};

const OPERATION: GitCollectOperation = GitCollectOperation::TrackedFiles;
const MAX_STAGE_METADATA_BYTES: usize = b"100644 ".len() + 64 + b" 0\t".len();
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Path representation bounds and optional caller-requested inventory budgets.
///
/// No aggregate count, text-byte or wall-time budget is imposed by default.
/// Callers supply their filesystem's path bound and may opt into tighter budgets.
/// Retained text bytes exclude collection node overhead, bounded separately by
/// the number of unique retained paths when a count budget is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitTrackedPathLimits {
    max_path_bytes: usize,
    max_paths: Option<usize>,
    max_total_path_bytes: Option<usize>,
    command_timeout: Option<Duration>,
}

impl GitTrackedPathLimits {
    /// Creates an inventory bound matching the consumer's path representation.
    ///
    /// Cancellation remains active without an optional timeout or aggregate budget.
    ///
    /// # Errors
    ///
    /// Returns [`GitCollectError::InvalidLimits`] for a zero path bound or an
    /// unrepresentable stage-record bound.
    pub fn new(max_path_bytes: usize) -> Result<Self, GitCollectError> {
        if max_path_bytes == 0
            || max_path_bytes
                .checked_add(MAX_STAGE_METADATA_BYTES)
                .is_none()
        {
            return Err(GitCollectError::InvalidLimits);
        }
        Ok(Self {
            max_path_bytes,
            max_paths: None,
            max_total_path_bytes: None,
            command_timeout: None,
        })
    }

    /// Adds explicit ceilings for unique regular paths and their UTF-8 text.
    ///
    /// Duplicate conflict stages consume neither budget more than once.
    ///
    /// # Errors
    ///
    /// Returns [`GitCollectError::InvalidLimits`] when either bound is zero.
    pub fn with_retained_limits(
        mut self,
        max_paths: usize,
        max_total_path_bytes: usize,
    ) -> Result<Self, GitCollectError> {
        if max_paths == 0 || max_total_path_bytes == 0 {
            return Err(GitCollectError::InvalidLimits);
        }
        self.max_paths = Some(max_paths);
        self.max_total_path_bytes = Some(max_total_path_bytes);
        Ok(self)
    }

    /// Adds a caller-requested monotonic deadline for command and parsing work.
    ///
    /// # Errors
    ///
    /// Returns [`GitCollectError::InvalidLimits`] when the duration is zero.
    pub fn with_command_timeout(mut self, timeout: Duration) -> Result<Self, GitCollectError> {
        if timeout.is_zero() {
            return Err(GitCollectError::InvalidLimits);
        }
        self.command_timeout = Some(timeout);
        Ok(self)
    }

    /// Returns the UTF-8 byte bound for each index path, including excluded modes.
    #[must_use]
    pub const fn max_path_bytes(self) -> usize {
        self.max_path_bytes
    }

    /// Returns the optional unique regular-path count ceiling.
    #[must_use]
    pub const fn max_paths(self) -> Option<usize> {
        self.max_paths
    }

    /// Returns the optional aggregate retained path-text byte ceiling.
    #[must_use]
    pub const fn max_total_path_bytes(self) -> Option<usize> {
        self.max_total_path_bytes
    }

    /// Returns the optional monotonic command and parsing timeout.
    #[must_use]
    pub const fn command_timeout(self) -> Option<Duration> {
        self.command_timeout
    }
}

/// The inventory resource whose representation or explicit budget was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitTrackedPathLimit {
    /// Unique regular-file paths retained in the inventory.
    Paths,
    /// UTF-8 bytes in one index path.
    PathBytes,
    /// UTF-8 bytes summed across unique regular-file paths.
    TotalPathBytes,
    /// Bytes in one stage record, excluding its NUL terminator.
    RecordBytes,
}

/// Source-free failure of a complete streaming tracked-file inventory.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitTrackedPathError {
    /// Existing fixed-command, metadata or cancellation failure.
    #[error(transparent)]
    Collection(#[from] GitCollectError),
    /// A representation bound or an explicit caller budget was exceeded.
    #[error("git tracked inventory {resource:?} exceeds its limit ({observed} > {maximum})")]
    Limit {
        /// Resource whose bound was exceeded.
        resource: GitTrackedPathLimit,
        /// Observed amount when the violation became known.
        observed: usize,
        /// Selected resource ceiling.
        maximum: usize,
    },
    /// A path or record allocation could not be reserved.
    #[error("git tracked inventory allocation failed")]
    AllocationFailure,
    /// Inventory size arithmetic cannot be represented on this platform.
    #[error("git tracked inventory size is unrepresentable")]
    SizeOverflow,
}

/// Collects every tracked regular-file path without buffering complete stdout.
///
/// Missing root-local Git metadata returns an empty set. Symlinks and gitlinks
/// are validated but excluded; conflict stages deduplicate by exact UTF-8 path.
/// Consumers must still validate returned paths against their filesystem boundary.
/// The existing buffered [`super::collect_tracked_paths`] contract is unchanged.
///
/// # Errors
///
/// Returns [`GitTrackedPathError`] for command failure, malformed metadata,
/// incomplete or unrepresentable records, explicit budgets, allocation failure
/// or cancellation. No partial inventory is returned on any failure.
pub fn collect_tracked_paths_streaming(
    repository_root: &Path,
    limits: GitTrackedPathLimits,
    cancellation: &Cancellation,
) -> Result<BTreeSet<String>, GitTrackedPathError> {
    check_cancellation(cancellation)?;
    match std::fs::symlink_metadata(repository_root.join(".git")) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(source) => return Err(command_io(source).into()),
        Ok(metadata) if metadata.file_type().is_symlink() => return Err(invalid_output()),
        Ok(_) => {}
    }
    let runner = GitRunner::new(repository_root, GitCollectLimits::default());
    check_cancellation(cancellation)?;
    let mut child = runner.spawn(
        OPERATION,
        ["ls-files", "--cached", "--stage", "--full-name", "-z"],
    )?;
    wait_for_paths(&mut child, limits, cancellation)
}

fn wait_for_paths(
    child: &mut Child,
    limits: GitTrackedPathLimits,
    cancellation: &Cancellation,
) -> Result<BTreeSet<String>, GitTrackedPathError> {
    let started = Instant::now();
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(child);
        return Err(command_io(std::io::Error::other("git stdout pipe is unavailable")).into());
    };
    let reader_cancellation = cancellation.clone();
    let reader =
        thread::Builder::new().spawn(move || read_paths(stdout, limits, &reader_cancellation));
    let mut reader = match reader {
        Ok(reader) => Some(reader),
        Err(source) => {
            kill_and_reap(child);
            return Err(command_io(source).into());
        }
    };
    let result = (|| {
        let mut paths = None;
        let mut status = None;
        loop {
            check_cancellation(cancellation)?;
            if limits
                .command_timeout
                .is_some_and(|timeout| started.elapsed() >= timeout)
            {
                return Err(GitCollectError::CommandTimedOut {
                    operation: OPERATION,
                }
                .into());
            }
            if reader.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                let completed = reader.take().ok_or_else(invalid_output)?;
                paths = Some(completed.join().map_err(|_| {
                    command_io(std::io::Error::other("git stdout reader panicked"))
                })??);
            }
            if status.is_none() {
                status = child.try_wait().map_err(command_io)?;
            }
            if let Some(status) = status {
                if !status.success() {
                    return Err(GitCollectError::CommandFailed {
                        operation: OPERATION,
                        exit_code: status.code(),
                    }
                    .into());
                }
                if let Some(paths) = paths {
                    check_cancellation(cancellation)?;
                    return Ok(paths);
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() {
        kill_and_reap(child);
    }
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    result
}

fn read_paths(
    mut reader: impl Read,
    limits: GitTrackedPathLimits,
    cancellation: &Cancellation,
) -> Result<BTreeSet<String>, GitTrackedPathError> {
    let mut chunk = [0; READ_CHUNK_BYTES];
    let mut record = Vec::new();
    let mut paths = BTreeSet::new();
    let mut total_path_bytes = 0;
    let record_limit = limits
        .max_path_bytes
        .checked_add(MAX_STAGE_METADATA_BYTES)
        .ok_or(GitTrackedPathError::SizeOverflow)?;
    loop {
        check_cancellation(cancellation)?;
        let count = match reader.read(&mut chunk) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(command_io(error).into()),
        };
        if count == 0 {
            check_cancellation(cancellation)?;
            return if record.is_empty() {
                Ok(paths)
            } else {
                Err(invalid_output())
            };
        }
        for segment in chunk
            .get(..count)
            .ok_or_else(invalid_output)?
            .split_inclusive(|byte| *byte == 0)
        {
            check_cancellation(cancellation)?;
            let terminated = segment.last() == Some(&0);
            let bytes = segment.strip_suffix(&[0]).unwrap_or(segment);
            let observed = record
                .len()
                .checked_add(bytes.len())
                .ok_or(GitTrackedPathError::SizeOverflow)?;
            enforce_limit(GitTrackedPathLimit::RecordBytes, observed, record_limit)?;
            record
                .try_reserve_exact(bytes.len())
                .map_err(|_| GitTrackedPathError::AllocationFailure)?;
            record.extend_from_slice(bytes);
            if terminated {
                retain_record(&record, &mut paths, &mut total_path_bytes, limits)?;
                record.clear();
            }
        }
    }
}

fn retain_record(
    record: &[u8],
    paths: &mut BTreeSet<String>,
    total_path_bytes: &mut usize,
    limits: GitTrackedPathLimits,
) -> Result<(), GitTrackedPathError> {
    let mut record_fields = record.splitn(2, |byte| *byte == b'\t');
    let metadata = record_fields.next().ok_or_else(invalid_output)?;
    let path = record_fields.next().ok_or_else(invalid_output)?;
    let mut fields = metadata.split(|byte| *byte == b' ');
    let mode = fields.next().ok_or_else(invalid_output)?;
    let object = fields.next().ok_or_else(invalid_output)?;
    let stage = fields.next().ok_or_else(invalid_output)?;
    if fields.next().is_some()
        || !matches!(mode, b"100644" | b"100755" | b"120000" | b"160000")
        || !matches!(stage, b"0" | b"1" | b"2" | b"3")
        || !matches!(object.len(), 40 | 64)
        || !object.iter().all(u8::is_ascii_hexdigit)
        || path.is_empty()
    {
        return Err(invalid_output());
    }
    enforce_limit(
        GitTrackedPathLimit::PathBytes,
        path.len(),
        limits.max_path_bytes,
    )?;
    let path = std::str::from_utf8(path).map_err(|_| invalid_output())?;
    if !matches!(mode, b"100644" | b"100755") || paths.contains(path) {
        return Ok(());
    }
    let count = paths
        .len()
        .checked_add(1)
        .ok_or(GitTrackedPathError::SizeOverflow)?;
    let text_bytes = total_path_bytes
        .checked_add(path.len())
        .ok_or(GitTrackedPathError::SizeOverflow)?;
    if let Some(maximum) = limits.max_paths {
        enforce_limit(GitTrackedPathLimit::Paths, count, maximum)?;
    }
    if let Some(maximum) = limits.max_total_path_bytes {
        enforce_limit(GitTrackedPathLimit::TotalPathBytes, text_bytes, maximum)?;
    }
    let mut owned = String::new();
    owned
        .try_reserve_exact(path.len())
        .map_err(|_| GitTrackedPathError::AllocationFailure)?;
    owned.push_str(path);
    paths.insert(owned);
    *total_path_bytes = text_bytes;
    Ok(())
}

fn enforce_limit(
    resource: GitTrackedPathLimit,
    observed: usize,
    maximum: usize,
) -> Result<(), GitTrackedPathError> {
    if observed > maximum {
        Err(GitTrackedPathError::Limit {
            resource,
            observed,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn check_cancellation(cancellation: &Cancellation) -> Result<(), GitTrackedPathError> {
    cancellation.check().map_err(GitCollectError::from)?;
    Ok(())
}

fn invalid_output() -> GitTrackedPathError {
    GitCollectError::InvalidOutput {
        operation: OPERATION,
    }
    .into()
}

fn command_io(source: std::io::Error) -> GitCollectError {
    GitCollectError::CommandIo {
        operation: OPERATION,
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, Write},
        process::{Command, Stdio},
    };

    use rootlight_cancel::CancellationReason;

    use super::*;

    fn limits() -> GitTrackedPathLimits {
        GitTrackedPathLimits::new(256).unwrap()
    }

    fn record(mode: &str, stage: u8, object_bytes: usize, path: &str) -> String {
        format!("{mode} {} {stage}\t{path}\0", "a".repeat(object_bytes))
    }

    #[test]
    fn inventory_limits_are_opt_in_and_reject_unrepresentable_settings() {
        let limits = limits();
        assert_eq!(limits.max_path_bytes(), 256);
        assert_eq!(limits.max_paths(), None);
        assert_eq!(limits.max_total_path_bytes(), None);
        assert_eq!(limits.command_timeout(), None);
        assert!(GitTrackedPathLimits::new(0).is_err());
        assert!(GitTrackedPathLimits::new(usize::MAX).is_err());
        assert!(limits.with_retained_limits(0, 1).is_err());
        assert!(limits.with_retained_limits(1, 0).is_err());
        assert!(limits.with_command_timeout(Duration::ZERO).is_err());
        assert!(limits.with_retained_limits(usize::MAX, usize::MAX).is_ok());
        assert_eq!(
            limits
                .with_command_timeout(Duration::from_secs(600))
                .unwrap()
                .command_timeout(),
            Some(Duration::from_secs(600)),
        );
    }

    #[test]
    fn every_byte_split_preserves_utf8_metadata_nuls_and_conflict_identity() {
        let records = [
            record("100644", 0, 40, "space name.rs"),
            record("100755", 0, 64, "unicode-λ.rs"),
            record("100644", 1, 40, "conflict.rs"),
            record("100644", 2, 40, "conflict.rs"),
            record("100644", 3, 40, "conflict.rs"),
            record("120000", 0, 40, "link.rs"),
            record("160000", 0, 40, "module"),
            record("100644", 0, 40, "tab\tand\nline.rs"),
        ]
        .concat();
        let expected: BTreeSet<_> = [
            "space name.rs",
            "unicode-λ.rs",
            "conflict.rs",
            "tab\tand\nline.rs",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for split in 0..=records.len() {
            let (left, right) = records.as_bytes().split_at(split);
            let reader = Cursor::new(left).chain(Cursor::new(right));
            assert_eq!(
                read_paths(reader, limits(), &Cancellation::new()).unwrap(),
                expected,
                "split={split}"
            );
        }
    }

    #[test]
    fn invalid_or_incomplete_record_after_valid_path_never_returns_a_prefix() {
        let first = record("100644", 0, 40, "valid.rs");
        let mut malformed = vec![
            record("100644", 0, 40, "last.rs")
                .trim_end_matches('\0')
                .to_owned()
                .into_bytes(),
            vec![0],
            record("100644", 4, 40, "bad.rs").into_bytes(),
            record("100000", 0, 40, "bad.rs").into_bytes(),
            record("100644", 0, 39, "bad.rs").into_bytes(),
            record("100644", 0, 40, "").into_bytes(),
            format!("100644 {} 0 extra\tbad.rs\0", "a".repeat(40)).into_bytes(),
            format!("100644 {} 0\tbad.rs\0", "z".repeat(40)).into_bytes(),
        ];
        let mut non_utf8 = format!("120000 {} 0\t", "a".repeat(40)).into_bytes();
        non_utf8.extend_from_slice(&[255, 0]);
        malformed.push(non_utf8);
        for suffix in malformed {
            let input = Cursor::new(first.as_bytes()).chain(Cursor::new(suffix));
            assert!(matches!(
                read_paths(input, limits(), &Cancellation::new()),
                Err(GitTrackedPathError::Collection(
                    GitCollectError::InvalidOutput { .. }
                ))
            ));
        }
        assert!(
            read_paths(Cursor::new([]), limits(), &Cancellation::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn retained_budgets_count_unique_regular_paths_and_exact_text_bytes() {
        let input = [
            record("100644", 1, 40, "a.rs"),
            record("100644", 2, 40, "a.rs"),
            record("120000", 0, 40, "ignored.rs"),
            record("100755", 0, 64, "b.rs"),
        ]
        .concat();
        let limits = limits().with_retained_limits(2, 8).unwrap();
        assert_eq!(
            read_paths(Cursor::new(input.as_bytes()), limits, &Cancellation::new())
                .unwrap()
                .len(),
            2
        );
        for (limits, expected_resource, expected_observed, expected_maximum) in [
            (
                limits.with_retained_limits(1, 8).unwrap(),
                GitTrackedPathLimit::Paths,
                2,
                1,
            ),
            (
                limits.with_retained_limits(2, 7).unwrap(),
                GitTrackedPathLimit::TotalPathBytes,
                8,
                7,
            ),
        ] {
            assert!(
                matches!(read_paths(Cursor::new(input.as_bytes()), limits, &Cancellation::new()),
                Err(GitTrackedPathError::Limit { resource, observed, maximum })
                if resource == expected_resource && observed == expected_observed && maximum == expected_maximum)
            );
        }
    }

    #[test]
    fn path_and_stage_record_limits_fail_before_unbounded_growth() {
        let limits = GitTrackedPathLimits::new(4).unwrap();
        let valid = record("100644", 0, 64, "a.rs");
        assert_eq!(
            read_paths(Cursor::new(valid), limits, &Cancellation::new())
                .unwrap()
                .len(),
            1
        );
        let long_path = record("100644", 0, 40, "ab.rs");
        assert!(matches!(
            read_paths(Cursor::new(long_path), limits, &Cancellation::new()),
            Err(GitTrackedPathError::Limit {
                resource: GitTrackedPathLimit::PathBytes,
                observed: 5,
                maximum: 4
            })
        ));
        let record_maximum = 4 + MAX_STAGE_METADATA_BYTES;
        assert!(
            matches!(read_paths(Cursor::new(vec![b'a'; record_maximum + 1]), limits, &Cancellation::new()),
            Err(GitTrackedPathError::Limit { resource: GitTrackedPathLimit::RecordBytes, observed, maximum })
            if observed == record_maximum + 1 && maximum == record_maximum)
        );
    }

    struct CancelAfterRead<'a> {
        source: Cursor<Vec<u8>>,
        cancellation: &'a Cancellation,
    }

    impl Read for CancelAfterRead<'_> {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            let count = self.source.read(target)?;
            if count == 0 {
                self.cancellation.cancel(CancellationReason::ClientRequest);
            }
            Ok(count)
        }
    }

    struct FailedRead;

    impl Read for FailedRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture read failure"))
        }
    }

    #[test]
    fn cancellation_or_io_failure_after_valid_records_returns_no_inventory() {
        let input = record("100644", 0, 40, "valid.rs").into_bytes();
        let cancellation = Cancellation::new();
        let reader = CancelAfterRead {
            source: Cursor::new(input.clone()),
            cancellation: &cancellation,
        };
        assert!(matches!(
            read_paths(reader, limits(), &cancellation),
            Err(GitTrackedPathError::Collection(GitCollectError::Cancelled(
                _
            )))
        ));
        assert!(matches!(
            read_paths(
                Cursor::new(input).chain(FailedRead),
                limits(),
                &Cancellation::new()
            ),
            Err(GitTrackedPathError::Collection(
                GitCollectError::CommandIo { .. }
            ))
        ));
    }

    fn blocked_child() -> Child {
        Command::new("git")
            .args(["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("fixture child starts")
    }

    #[test]
    fn cancellation_and_timeout_reap_a_child_waiting_for_input() {
        let cancellation = Cancellation::new();
        let mut child = blocked_child();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            wait_for_paths(&mut child, limits(), &cancellation),
            Err(GitTrackedPathError::Collection(GitCollectError::Cancelled(
                _
            )))
        ));
        assert!(child.try_wait().unwrap().is_some());
        let mut child = blocked_child();
        let limits = limits()
            .with_command_timeout(Duration::from_millis(1))
            .unwrap();
        assert!(matches!(
            wait_for_paths(&mut child, limits, &Cancellation::new()),
            Err(GitTrackedPathError::Collection(
                GitCollectError::CommandTimedOut { .. }
            ))
        ));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn successful_parser_eof_cannot_hide_a_failed_command() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new("git")
            .args(["--git-dir=missing", "rev-parse", "--verify", "HEAD"])
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(matches!(
            wait_for_paths(&mut child, limits(), &Cancellation::new()),
            Err(GitTrackedPathError::Collection(
                GitCollectError::CommandFailed { .. }
            ))
        ));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn malformed_output_reaps_a_child_before_it_closes_stdout() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let mut writer = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(b"malformed\0")
            .unwrap();
        let object = writer.wait_with_output().unwrap();
        assert!(object.status.success());
        let mut child = Command::new("git")
            .args(["cat-file", "--batch"])
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&object.stdout)
            .unwrap();
        child.stdin.as_mut().unwrap().flush().unwrap();
        assert!(matches!(
            wait_for_paths(
                &mut child,
                limits()
                    .with_command_timeout(Duration::from_secs(10))
                    .unwrap(),
                &Cancellation::new()
            ),
            Err(GitTrackedPathError::Collection(
                GitCollectError::InvalidOutput { .. }
            ))
        ));
        assert!(child.try_wait().unwrap().is_some());
    }
}
