//! Bounded read-only Git collection through fixed shell-free commands.
//!
//! Repository configuration is hostile input. Every command disables execution
//! hooks and helpers, optional writes, prompts, and lazy network object fetches.

use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    io::Read,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{RepositoryId, content_hash};

use crate::{
    CandidateGroupId, CanonicalGitSnapshot, ChangeSet, CommitRecord, FileChange, FileChangeKind,
    GIT_CONTRACT_VERSION, GitCollection, GitContractError, GitLimits, GitRepositoryState,
    GitSnapshotInput, HeadState, HeadUnavailableReason, HistoryCoverage, HistoryGapReason,
    HistoryState, HistoryTruncation, NonGitReason, ObjectDatabaseId, ObjectFormat, ObjectId,
    RenameCandidate, RenameEvidenceKind, RepositoryState, RevisionSelector, SparseCheckoutState,
    SubmoduleCheckoutState, SubmoduleState, WorktreeState, WorktreeStatus, canonicalize_snapshot,
};

const HARD_MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const HARD_MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const HARD_MAX_FILTER_OVERRIDES: usize = 4_096;
const DEFAULT_COMMAND_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const PRIMARY_WORKTREE: &str = "primary";

/// Bounds for one audited Git command collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitCollectLimits {
    history_commits: usize,
    command_output_bytes: usize,
    command_timeout: Duration,
}

impl GitCollectLimits {
    /// Creates checked command and history limits.
    ///
    /// `history_commits` is checked against [`GitLimits`] when collection
    /// starts because the declarative contract may impose a lower ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`GitCollectError::InvalidLimits`] when a limit is zero or
    /// exceeds the hard command ceiling.
    pub fn new(
        history_commits: usize,
        command_output_bytes: usize,
        command_timeout: Duration,
    ) -> Result<Self, GitCollectError> {
        if history_commits == 0
            || command_output_bytes == 0
            || command_output_bytes > HARD_MAX_COMMAND_OUTPUT_BYTES
            || command_timeout.is_zero()
            || command_timeout > HARD_MAX_COMMAND_TIMEOUT
        {
            return Err(GitCollectError::InvalidLimits);
        }
        Ok(Self {
            history_commits,
            command_output_bytes,
            command_timeout,
        })
    }

    /// Returns the requested commit window.
    #[must_use]
    pub const fn history_commits(self) -> usize {
        self.history_commits
    }

    /// Returns the maximum stdout bytes retained from one Git command.
    #[must_use]
    pub const fn command_output_bytes(self) -> usize {
        self.command_output_bytes
    }

    /// Returns the monotonic timeout applied to each Git command.
    #[must_use]
    pub const fn command_timeout(self) -> Duration {
        self.command_timeout
    }
}

impl Default for GitCollectLimits {
    fn default() -> Self {
        Self {
            history_commits: 2_000,
            command_output_bytes: DEFAULT_COMMAND_OUTPUT_BYTES,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

/// Stable source-free operation named by a collection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GitCollectOperation {
    /// Detect repository metadata.
    #[error("repository detection")]
    DetectRepository,
    /// Read the object identifier format.
    #[error("object format")]
    ObjectFormat,
    /// Resolve the shared object database.
    #[error("object database")]
    ObjectDatabase,
    /// Resolve HEAD.
    #[error("head")]
    Head,
    /// Read source-free worktree status.
    #[error("worktree status")]
    Status,
    /// Inspect sparse-checkout configuration.
    #[error("sparse checkout")]
    SparseCheckout,
    /// Inspect executable filter configuration.
    #[error("filter configuration")]
    FilterConfiguration,
    /// Read a bounded commit window.
    #[error("history")]
    History,
    /// Compare two repository states.
    #[error("diff")]
    Diff,
    /// Read staged gitlink entries.
    #[error("submodules")]
    Submodules,
}

/// Stable source-free error family for Git collection callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitCollectErrorCode {
    /// Configured bounds are invalid.
    InvalidLimits,
    /// Git could not be started or inspected.
    CommandIo,
    /// A fixed Git command exceeded its monotonic deadline.
    CommandTimedOut,
    /// A fixed Git command returned too much stdout.
    CommandOutputLimit,
    /// A required fixed Git command failed.
    CommandFailed,
    /// Git returned malformed or unsupported source-free metadata.
    InvalidOutput,
    /// Declarative validation rejected collected evidence.
    Contract,
    /// Cooperative cancellation stopped collection.
    Cancelled,
}

/// Typed source-free failure returned by the read-only Git collector.
#[derive(Debug, thiserror::Error)]
pub enum GitCollectError {
    /// One or more collection limits are zero, excessive, or inconsistent.
    #[error("git collection limits are invalid")]
    InvalidLimits,
    /// Git process creation, polling, reading, or reaping failed.
    #[error("git {operation} command I/O failed")]
    CommandIo {
        /// Source-free operation that failed.
        operation: GitCollectOperation,
        /// Preserved operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A fixed command did not finish within its monotonic budget.
    #[error("git {operation} command timed out")]
    CommandTimedOut {
        /// Source-free operation that timed out.
        operation: GitCollectOperation,
    },
    /// A fixed command produced more stdout than the configured byte ceiling.
    #[error("git {operation} command output exceeds its limit")]
    CommandOutputLimit {
        /// Source-free operation whose output exceeded the limit.
        operation: GitCollectOperation,
        /// Configured retained byte ceiling.
        maximum: usize,
    },
    /// A required fixed command returned a non-success exit status.
    #[error("git {operation} command failed")]
    CommandFailed {
        /// Source-free operation that failed.
        operation: GitCollectOperation,
        /// Numeric process status when the platform exposes one.
        exit_code: Option<i32>,
    },
    /// A fixed command returned malformed or unrepresentable metadata.
    #[error("git {operation} command returned invalid output")]
    InvalidOutput {
        /// Source-free operation whose output was invalid.
        operation: GitCollectOperation,
    },
    /// The declarative contract rejected collected evidence.
    #[error(transparent)]
    Contract(#[from] GitContractError),
    /// Cooperative cancellation stopped collection.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

impl GitCollectError {
    /// Returns the stable error family for programmatic handling.
    #[must_use]
    pub const fn code(&self) -> GitCollectErrorCode {
        match self {
            Self::InvalidLimits => GitCollectErrorCode::InvalidLimits,
            Self::CommandIo { .. } => GitCollectErrorCode::CommandIo,
            Self::CommandTimedOut { .. } => GitCollectErrorCode::CommandTimedOut,
            Self::CommandOutputLimit { .. } => GitCollectErrorCode::CommandOutputLimit,
            Self::CommandFailed { .. } => GitCollectErrorCode::CommandFailed,
            Self::InvalidOutput { .. } => GitCollectErrorCode::InvalidOutput,
            Self::Contract(_) => GitCollectErrorCode::Contract,
            Self::Cancelled(_) => GitCollectErrorCode::Cancelled,
        }
    }
}

/// Collects and validates bounded source-free evidence from one repository.
///
/// Only the selected worktree is inspected. Other linked worktrees remain an
/// importer extension until their dirty-state coverage can be represented
/// without reporting unknown status as clean.
///
/// # Errors
///
/// Returns [`GitCollectError`] for invalid limits, command failure, timeout,
/// excessive or malformed output, declarative contract failure, or
/// cooperative cancellation.
pub fn collect_repository(
    repository_root: &Path,
    repository: RepositoryId,
    contract_limits: &GitLimits,
    collect_limits: GitCollectLimits,
    cancellation: &Cancellation,
) -> Result<CanonicalGitSnapshot, GitCollectError> {
    cancellation.check()?;
    if collect_limits.history_commits > contract_limits.max_commits() {
        return Err(GitCollectError::InvalidLimits);
    }
    let runner = GitRunner::new(repository_root, collect_limits);
    let detected = runner.run(
        GitCollectOperation::DetectRepository,
        ["rev-parse", "--is-inside-work-tree"],
        cancellation,
    )?;
    if !detected.status.success() || trim_ascii(&detected.stdout) != b"true" {
        return canonicalize_snapshot(
            GitSnapshotInput::non_git(repository, NonGitReason::MetadataAbsent),
            contract_limits,
            cancellation,
        )
        .map_err(GitCollectError::from);
    }
    let runner = runner.with_disabled_filters(cancellation)?;

    let format = collect_object_format(&runner, cancellation)?;
    let object_database = collect_object_database(&runner, format, cancellation)?;
    let (head, head_commit) = collect_head(&runner, format, cancellation)?;
    let (status, untracked_paths) = collect_status(&runner, contract_limits, cancellation)?;
    let sparse_checkout = collect_sparse_checkout(&runner, cancellation)?;
    let (commits, history, coverage) = collect_history(
        &runner,
        format,
        collect_limits.history_commits,
        cancellation,
    )?;
    let (change_sets, rename_candidates) = collect_changes(
        &runner,
        head_commit,
        &untracked_paths,
        contract_limits,
        cancellation,
    )?;
    let submodules = collect_submodules(&runner, format, contract_limits, cancellation)?;

    let input = GitSnapshotInput {
        version: GIT_CONTRACT_VERSION,
        repository,
        state: RepositoryState::Git(GitRepositoryState {
            object_format: format,
            object_database,
            history,
            coverage,
        }),
        worktrees: vec![WorktreeState {
            id: PRIMARY_WORKTREE.to_owned(),
            head,
            index_tree: None,
            status,
            sparse_checkout,
        }],
        commits,
        change_sets,
        rename_candidates,
        submodules,
        lineage_candidates: Vec::new(),
    };
    canonicalize_snapshot(input, contract_limits, cancellation).map_err(GitCollectError::from)
}

struct GitRunner<'a> {
    repository_root: &'a Path,
    limits: GitCollectLimits,
    filter_overrides: Vec<OsString>,
}

impl<'a> GitRunner<'a> {
    const fn new(repository_root: &'a Path, limits: GitCollectLimits) -> Self {
        Self {
            repository_root,
            limits,
            filter_overrides: Vec::new(),
        }
    }

    fn with_disabled_filters(
        mut self,
        cancellation: &Cancellation,
    ) -> Result<Self, GitCollectError> {
        let output = self.run(
            GitCollectOperation::FilterConfiguration,
            [
                "config",
                "--includes",
                "--name-only",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process)$",
            ],
            cancellation,
        )?;
        if !output.status.success() {
            if output.status.code() == Some(1) {
                return Ok(self);
            }
            return Err(GitCollectError::CommandFailed {
                operation: GitCollectOperation::FilterConfiguration,
                exit_code: output.status.code(),
            });
        }

        let mut overrides = BTreeSet::new();
        for line in output
            .stdout
            .split(|byte| *byte == b'\n')
            .map(trim_ascii)
            .filter(|line| !line.is_empty())
        {
            cancellation.check()?;
            let key = parse_text(line, GitCollectOperation::FilterConfiguration)?;
            let Some(driver) = filter_driver_prefix(&key) else {
                return Err(GitCollectError::InvalidOutput {
                    operation: GitCollectOperation::FilterConfiguration,
                });
            };
            overrides.insert(format!("{key}="));
            overrides.insert(format!("{driver}.required=false"));
            if overrides.len() > HARD_MAX_FILTER_OVERRIDES {
                return Err(GitCollectError::CommandOutputLimit {
                    operation: GitCollectOperation::FilterConfiguration,
                    maximum: HARD_MAX_FILTER_OVERRIDES,
                });
            }
        }
        self.filter_overrides = overrides.into_iter().map(OsString::from).collect();
        Ok(self)
    }

    fn run<I, S>(
        &self,
        operation: GitCollectOperation,
        arguments: I,
        cancellation: &Cancellation,
    ) -> Result<GitOutput, GitCollectError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        cancellation.check()?;
        let mut command = Command::new("git");
        command
            .current_dir(self.repository_root)
            .arg("--no-pager")
            .arg("--no-optional-locks")
            .arg("--literal-pathspecs")
            .args([
                OsString::from("-c"),
                OsString::from(null_config("core.hooksPath")),
                OsString::from("-c"),
                OsString::from("core.fsmonitor=false"),
                OsString::from("-c"),
                OsString::from("core.untrackedCache=false"),
                OsString::from("-c"),
                OsString::from("diff.external="),
                OsString::from("-c"),
                OsString::from("submodule.recurse=false"),
                OsString::from("-c"),
                OsString::from("fetch.recurseSubmodules=false"),
            ])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device())
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_PROTOCOL_FROM_USER", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for override_value in &self.filter_overrides {
            command.arg("-c").arg(override_value);
        }
        command.args(arguments);
        let child = command
            .spawn()
            .map_err(|source| GitCollectError::CommandIo { operation, source })?;
        wait_for_output(child, operation, self.limits, cancellation)
    }

    fn required<I, S>(
        &self,
        operation: GitCollectOperation,
        arguments: I,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, GitCollectError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(operation, arguments, cancellation)?;
        if !output.status.success() {
            return Err(GitCollectError::CommandFailed {
                operation,
                exit_code: output.status.code(),
            });
        }
        Ok(output.stdout)
    }
}

fn filter_driver_prefix(key: &str) -> Option<&str> {
    let (driver, command) = key.rsplit_once('.')?;
    if !driver
        .get(.."filter.".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("filter."))
        || driver.len() == "filter.".len()
        || !["clean", "smudge", "process"]
            .iter()
            .any(|candidate| command.eq_ignore_ascii_case(candidate))
    {
        return None;
    }
    Some(driver)
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn wait_for_output(
    mut child: Child,
    operation: GitCollectOperation,
    limits: GitCollectLimits,
    cancellation: &Cancellation,
) -> Result<GitOutput, GitCollectError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| GitCollectError::CommandIo {
            operation,
            source: std::io::Error::other("git stdout pipe is unavailable"),
        })?;
    let read_limit = limits
        .command_output_bytes
        .checked_add(1)
        .ok_or(GitCollectError::InvalidLimits)?;
    let read_limit_u64 = u64::try_from(read_limit).map_err(|_| GitCollectError::InvalidLimits)?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(read_limit_u64).read_to_end(&mut bytes)?;
        Ok::<Vec<u8>, std::io::Error>(bytes)
    });
    let started = Instant::now();

    loop {
        if let Err(cancelled) = cancellation.check() {
            kill_and_reap(&mut child);
            let _ = reader.join();
            return Err(GitCollectError::Cancelled(cancelled));
        }
        if started.elapsed() >= limits.command_timeout {
            kill_and_reap(&mut child);
            let _ = reader.join();
            return Err(GitCollectError::CommandTimedOut { operation });
        }
        match child
            .try_wait()
            .map_err(|source| GitCollectError::CommandIo { operation, source })?
        {
            Some(status) => {
                let bytes = reader
                    .join()
                    .map_err(|_| GitCollectError::CommandIo {
                        operation,
                        source: std::io::Error::other("git stdout reader panicked"),
                    })?
                    .map_err(|source| GitCollectError::CommandIo { operation, source })?;
                if bytes.len() > limits.command_output_bytes {
                    return Err(GitCollectError::CommandOutputLimit {
                        operation,
                        maximum: limits.command_output_bytes,
                    });
                }
                return Ok(GitOutput {
                    status,
                    stdout: bytes,
                });
            }
            None => {
                // Polling keeps cancellation and the monotonic deadline observable
                // while the reader drains the bounded pipe concurrently.
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn null_config(key: &str) -> String {
    format!("{key}={}", null_device().to_string_lossy())
}

#[cfg(windows)]
fn null_device() -> &'static OsStr {
    OsStr::new("NUL")
}

#[cfg(not(windows))]
fn null_device() -> &'static OsStr {
    OsStr::new("/dev/null")
}

fn collect_object_format(
    runner: &GitRunner<'_>,
    cancellation: &Cancellation,
) -> Result<ObjectFormat, GitCollectError> {
    let output = runner.required(
        GitCollectOperation::ObjectFormat,
        ["rev-parse", "--show-object-format"],
        cancellation,
    )?;
    match trim_ascii(&output) {
        b"sha1" => Ok(ObjectFormat::Sha1),
        b"sha256" => Ok(ObjectFormat::Sha256),
        _ => Err(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::ObjectFormat,
        }),
    }
}

fn collect_object_database(
    runner: &GitRunner<'_>,
    format: ObjectFormat,
    cancellation: &Cancellation,
) -> Result<ObjectDatabaseId, GitCollectError> {
    let output = runner.required(
        GitCollectOperation::ObjectDatabase,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancellation,
    )?;
    let common_dir = trim_ascii(&output);
    if common_dir.is_empty() {
        return Err(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::ObjectDatabase,
        });
    }
    let mut identity = Vec::with_capacity(common_dir.len().saturating_add(16));
    identity.extend_from_slice(b"rootlight.git.odb.v1\0");
    identity.push(match format {
        ObjectFormat::Sha1 => 1,
        ObjectFormat::Sha256 => 2,
    });
    identity.extend_from_slice(common_dir);
    Ok(ObjectDatabaseId::from_bytes(
        *content_hash(&identity).as_bytes(),
    ))
}

fn collect_head(
    runner: &GitRunner<'_>,
    format: ObjectFormat,
    cancellation: &Cancellation,
) -> Result<(HeadState, Option<ObjectId>), GitCollectError> {
    let reference_output = runner.run(
        GitCollectOperation::Head,
        ["symbolic-ref", "--quiet", "HEAD"],
        cancellation,
    )?;
    let reference = if reference_output.status.success() {
        Some(parse_text(
            trim_ascii(&reference_output.stdout),
            GitCollectOperation::Head,
        )?)
    } else {
        None
    };
    let commit_output = runner.run(
        GitCollectOperation::Head,
        ["rev-parse", "--verify", "HEAD^{commit}"],
        cancellation,
    )?;
    let commit = if commit_output.status.success() {
        Some(parse_object_id(
            trim_ascii(&commit_output.stdout),
            format,
            GitCollectOperation::Head,
        )?)
    } else {
        None
    };
    let head = match (reference, commit) {
        (Some(reference), Some(commit)) => HeadState::Branch { reference, commit },
        (Some(reference), None) => HeadState::Unborn { reference },
        (None, Some(commit)) => HeadState::Detached { commit },
        (None, None) => HeadState::Unavailable {
            reason: HeadUnavailableReason::InvalidMetadata,
        },
    };
    Ok((head, commit))
}

fn collect_status(
    runner: &GitRunner<'_>,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<(WorktreeStatus, BTreeSet<String>), GitCollectError> {
    let output = runner.required(
        GitCollectOperation::Status,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--ignore-submodules=all",
        ],
        cancellation,
    )?;
    let mut status = WorktreeStatus::default();
    let mut untracked = BTreeSet::new();
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        cancellation.check()?;
        let [index, worktree, b' ', path @ ..] = record else {
            return Err(GitCollectError::InvalidOutput {
                operation: GitCollectOperation::Status,
            });
        };
        match (*index, *worktree) {
            (b'?', b'?') => {
                status.untracked_paths = status.untracked_paths.checked_add(1).ok_or(
                    GitCollectError::InvalidOutput {
                        operation: GitCollectOperation::Status,
                    },
                )?;
                untracked.insert(parse_text(path, GitCollectOperation::Status)?);
                if untracked.len() > limits.max_changes() {
                    return Err(GitContractError::CollectionLimit {
                        collection: GitCollection::Changes,
                        maximum: limits.max_changes(),
                    }
                    .into());
                }
            }
            (b'!', b'!') => {}
            pair => {
                status.tracked_changes = status.tracked_changes.checked_add(1).ok_or(
                    GitCollectError::InvalidOutput {
                        operation: GitCollectOperation::Status,
                    },
                )?;
                if is_conflict_status(pair) {
                    status.conflicts =
                        status
                            .conflicts
                            .checked_add(1)
                            .ok_or(GitCollectError::InvalidOutput {
                                operation: GitCollectOperation::Status,
                            })?;
                }
                let _ = parse_text(path, GitCollectOperation::Status)?;
            }
        }
    }
    Ok((status, untracked))
}

fn is_conflict_status(status: (u8, u8)) -> bool {
    matches!(
        status,
        (b'D', b'D')
            | (b'A', b'U')
            | (b'U', b'D')
            | (b'U', b'A')
            | (b'D', b'U')
            | (b'A', b'A')
            | (b'U', b'U')
    )
}

fn collect_sparse_checkout(
    runner: &GitRunner<'_>,
    cancellation: &Cancellation,
) -> Result<SparseCheckoutState, GitCollectError> {
    let output = runner.run(
        GitCollectOperation::SparseCheckout,
        ["config", "--bool", "--get", "core.sparseCheckout"],
        cancellation,
    )?;
    if !output.status.success() {
        return Ok(SparseCheckoutState::Disabled);
    }
    match trim_ascii(&output.stdout) {
        b"false" => Ok(SparseCheckoutState::Disabled),
        b"true" => Ok(SparseCheckoutState::Unknown),
        _ => Err(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::SparseCheckout,
        }),
    }
}

fn collect_history(
    runner: &GitRunner<'_>,
    format: ObjectFormat,
    history_limit: usize,
    cancellation: &Cancellation,
) -> Result<(Vec<CommitRecord>, HistoryState, HistoryCoverage), GitCollectError> {
    let requested = u32::try_from(history_limit).map_err(|_| GitCollectError::InvalidLimits)?;
    let max_count = history_limit
        .checked_add(1)
        .ok_or(GitCollectError::InvalidLimits)?;
    let max_count_arg = format!("--max-count={max_count}");
    let output = runner.run(
        GitCollectOperation::History,
        [
            OsString::from("log"),
            OsString::from("--no-show-signature"),
            OsString::from("--no-notes"),
            OsString::from("--format=%H%x1f%P%x1f%T%x1f%at%x1e"),
            OsString::from(max_count_arg),
            OsString::from("HEAD"),
        ],
        cancellation,
    )?;
    let shallow_output = runner.run(
        GitCollectOperation::History,
        ["rev-parse", "--is-shallow-repository"],
        cancellation,
    )?;
    let is_shallow =
        shallow_output.status.success() && trim_ascii(&shallow_output.stdout) == b"true";

    if !output.status.success() {
        return Ok((
            Vec::new(),
            HistoryState::Incomplete {
                reason: HistoryGapReason::ImporterIncomplete,
                missing_objects: Vec::new(),
            },
            HistoryCoverage {
                imported_commits: 0,
                requested_commit_limit: requested,
                oldest_imported_time_unix_seconds: None,
                truncation: if is_shallow {
                    vec![HistoryTruncation::ShallowBoundary]
                } else {
                    Vec::new()
                },
            },
        ));
    }

    let mut commits = parse_commits(&output.stdout, format, cancellation)?;
    let truncated = commits.len() > history_limit;
    commits.truncate(history_limit);
    let imported_commits =
        u32::try_from(commits.len()).map_err(|_| GitCollectError::InvalidLimits)?;
    let oldest = commits
        .iter()
        .map(|commit| commit.author_time_unix_seconds)
        .min();
    let mut truncation = Vec::new();
    if truncated {
        truncation.push(HistoryTruncation::CommitLimit);
    }
    if is_shallow {
        truncation.push(HistoryTruncation::ShallowBoundary);
    }
    let history = if is_shallow {
        HistoryState::Incomplete {
            reason: HistoryGapReason::ImporterIncomplete,
            missing_objects: Vec::new(),
        }
    } else {
        HistoryState::Complete
    };
    Ok((
        commits,
        history,
        HistoryCoverage {
            imported_commits,
            requested_commit_limit: requested,
            oldest_imported_time_unix_seconds: oldest,
            truncation,
        },
    ))
}

fn parse_commits(
    output: &[u8],
    format: ObjectFormat,
    cancellation: &Cancellation,
) -> Result<Vec<CommitRecord>, GitCollectError> {
    let mut commits = Vec::new();
    for record in output
        .split(|byte| *byte == 0x1e)
        .map(trim_ascii)
        .filter(|record| !record.is_empty())
    {
        cancellation.check()?;
        let mut fields = record.split(|byte| *byte == 0x1f);
        let id = required_field(&mut fields, GitCollectOperation::History)?;
        let parents = required_field(&mut fields, GitCollectOperation::History)?;
        let tree = required_field(&mut fields, GitCollectOperation::History)?;
        let author_time = required_field(&mut fields, GitCollectOperation::History)?;
        if fields.next().is_some() {
            return Err(GitCollectError::InvalidOutput {
                operation: GitCollectOperation::History,
            });
        }
        let mut parent_ids = Vec::new();
        for parent in parents
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|parent| !parent.is_empty())
        {
            parent_ids.push(parse_object_id(
                parent,
                format,
                GitCollectOperation::History,
            )?);
        }
        let author_time_unix_seconds = parse_text(author_time, GitCollectOperation::History)?
            .parse::<i64>()
            .map_err(|_| GitCollectError::InvalidOutput {
                operation: GitCollectOperation::History,
            })?;
        commits.push(CommitRecord {
            id: parse_object_id(id, format, GitCollectOperation::History)?,
            parents: parent_ids,
            tree: parse_object_id(tree, format, GitCollectOperation::History)?,
            author_time_unix_seconds,
        });
    }
    Ok(commits)
}

fn required_field<'a>(
    fields: &mut impl Iterator<Item = &'a [u8]>,
    operation: GitCollectOperation,
) -> Result<&'a [u8], GitCollectError> {
    fields
        .next()
        .ok_or(GitCollectError::InvalidOutput { operation })
}

fn collect_changes(
    runner: &GitRunner<'_>,
    head_commit: Option<ObjectId>,
    untracked_paths: &BTreeSet<String>,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<(Vec<ChangeSet>, Vec<RenameCandidate>), GitCollectError> {
    let mut change_sets = Vec::new();
    let mut candidates = Vec::new();
    let index = RevisionSelector::Index {
        worktree: PRIMARY_WORKTREE.to_owned(),
    };
    let working_tree = RevisionSelector::WorkingTree {
        worktree: PRIMARY_WORKTREE.to_owned(),
    };

    if let Some(commit) = head_commit {
        let base = RevisionSelector::Commit(commit);
        let staged = collect_diff(
            runner,
            ["diff", "--cached"],
            base.clone(),
            index.clone(),
            limits,
            cancellation,
        )?;
        candidates.extend(collect_rename_candidates(
            runner,
            ["diff", "--cached"],
            base.clone(),
            index.clone(),
            cancellation,
        )?);
        if !staged.changes.is_empty() {
            change_sets.push(staged);
        }

        let mut combined = collect_diff(
            runner,
            ["diff", "HEAD"],
            base.clone(),
            working_tree.clone(),
            limits,
            cancellation,
        )?;
        add_untracked(&mut combined.changes, untracked_paths, limits)?;
        candidates.extend(collect_rename_candidates(
            runner,
            ["diff", "HEAD"],
            base,
            working_tree.clone(),
            cancellation,
        )?);
        if !combined.changes.is_empty() {
            change_sets.push(combined);
        }
    }

    let mut unstaged = collect_diff(runner, ["diff"], index, working_tree, limits, cancellation)?;
    add_untracked(&mut unstaged.changes, untracked_paths, limits)?;
    if !unstaged.changes.is_empty() {
        change_sets.push(unstaged);
    }
    Ok((change_sets, candidates))
}

fn collect_diff<const N: usize>(
    runner: &GitRunner<'_>,
    prefix: [&str; N],
    base: RevisionSelector,
    head: RevisionSelector,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<ChangeSet, GitCollectError> {
    let mut arguments = prefix.into_iter().map(OsString::from).collect::<Vec<_>>();
    arguments.extend([
        OsString::from("--name-status"),
        OsString::from("-z"),
        OsString::from("--no-renames"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--ignore-submodules=all"),
    ]);
    let output = runner.required(GitCollectOperation::Diff, arguments, cancellation)?;
    let changes = parse_name_status(&output, limits, cancellation)?;
    Ok(ChangeSet {
        base,
        head,
        changes,
    })
}

fn parse_name_status(
    output: &[u8],
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<Vec<FileChange>, GitCollectError> {
    let mut tokens = output
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = tokens.next() {
        cancellation.check()?;
        let path = tokens.next().ok_or(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::Diff,
        })?;
        let path = parse_text(path, GitCollectOperation::Diff)?;
        let (kind, before_path, after_path) = match status {
            b"A" => (FileChangeKind::Added, None, Some(path)),
            b"D" => (FileChangeKind::Deleted, Some(path), None),
            b"M" => (FileChangeKind::Modified, Some(path.clone()), Some(path)),
            b"T" => (FileChangeKind::TypeChanged, Some(path.clone()), Some(path)),
            _ => {
                return Err(GitCollectError::InvalidOutput {
                    operation: GitCollectOperation::Diff,
                });
            }
        };
        changes.push(FileChange {
            kind,
            before_path,
            after_path,
            spans: Vec::new(),
        });
        if changes.len() > limits.max_changes() {
            return Err(GitContractError::CollectionLimit {
                collection: GitCollection::Changes,
                maximum: limits.max_changes(),
            }
            .into());
        }
    }
    Ok(changes)
}

fn add_untracked(
    changes: &mut Vec<FileChange>,
    paths: &BTreeSet<String>,
    limits: &GitLimits,
) -> Result<(), GitCollectError> {
    let mut present = changes
        .iter()
        .filter_map(|change| change.after_path.clone())
        .collect::<BTreeSet<_>>();
    for path in paths {
        if present.insert(path.clone()) {
            changes.push(FileChange {
                kind: FileChangeKind::Added,
                before_path: None,
                after_path: Some(path.clone()),
                spans: Vec::new(),
            });
        }
        if changes.len() > limits.max_changes() {
            return Err(GitContractError::CollectionLimit {
                collection: GitCollection::Changes,
                maximum: limits.max_changes(),
            }
            .into());
        }
    }
    Ok(())
}

fn collect_rename_candidates<const N: usize>(
    runner: &GitRunner<'_>,
    prefix: [&str; N],
    base: RevisionSelector,
    head: RevisionSelector,
    cancellation: &Cancellation,
) -> Result<Vec<RenameCandidate>, GitCollectError> {
    let mut arguments = prefix.into_iter().map(OsString::from).collect::<Vec<_>>();
    arguments.extend([
        OsString::from("--name-status"),
        OsString::from("-z"),
        OsString::from("--find-renames=50%"),
        OsString::from("-l1000"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--ignore-submodules=all"),
    ]);
    let output = runner.required(GitCollectOperation::Diff, arguments, cancellation)?;
    let mut tokens = output
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty());
    let mut candidates = Vec::new();
    while let Some(status) = tokens.next() {
        cancellation.check()?;
        if let Some(score) = status.strip_prefix(b"R") {
            let before = tokens.next().ok_or(GitCollectError::InvalidOutput {
                operation: GitCollectOperation::Diff,
            })?;
            let after = tokens.next().ok_or(GitCollectError::InvalidOutput {
                operation: GitCollectOperation::Diff,
            })?;
            let before_path = parse_text(before, GitCollectOperation::Diff)?;
            let after_path = parse_text(after, GitCollectOperation::Diff)?;
            let similarity = parse_similarity(score)?;
            let mut evidence = vec![RenameEvidenceKind::ImporterSignal];
            if similarity == 100 {
                evidence.push(RenameEvidenceKind::ExactContent);
            } else {
                evidence.push(RenameEvidenceKind::Similarity);
            }
            candidates.push(RenameCandidate {
                base: base.clone(),
                head: head.clone(),
                before_path: before_path.clone(),
                after_path: after_path.clone(),
                group: rename_group(&base, &head, &before_path, &after_path)?,
                confidence_bps: similarity.saturating_mul(100),
                evidence,
            });
        } else {
            let _ = tokens.next().ok_or(GitCollectError::InvalidOutput {
                operation: GitCollectOperation::Diff,
            })?;
        }
    }
    Ok(candidates)
}

fn parse_similarity(score: &[u8]) -> Result<u16, GitCollectError> {
    let value = parse_text(score, GitCollectOperation::Diff)?
        .parse::<u16>()
        .map_err(|_| GitCollectError::InvalidOutput {
            operation: GitCollectOperation::Diff,
        })?;
    if value > 100 {
        return Err(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::Diff,
        });
    }
    Ok(value)
}

fn rename_group(
    base: &RevisionSelector,
    head: &RevisionSelector,
    before_path: &str,
    after_path: &str,
) -> Result<CandidateGroupId, GitCollectError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"rootlight.git.rename.v1\0");
    encode_selector(&mut bytes, base)?;
    encode_selector(&mut bytes, head)?;
    encode_length_prefixed(&mut bytes, before_path.as_bytes())?;
    encode_length_prefixed(&mut bytes, after_path.as_bytes())?;
    let digest = content_hash(&bytes);
    let mut group = [0_u8; 16];
    let prefix = digest
        .as_bytes()
        .get(..group.len())
        .ok_or(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::Diff,
        })?;
    group.copy_from_slice(prefix);
    Ok(CandidateGroupId::from_bytes(group))
}

fn encode_selector(
    bytes: &mut Vec<u8>,
    selector: &RevisionSelector,
) -> Result<(), GitCollectError> {
    match selector {
        RevisionSelector::Commit(object) => {
            bytes.push(1);
            encode_object(bytes, *object);
        }
        RevisionSelector::Reference { name, target } => {
            bytes.push(2);
            encode_length_prefixed(bytes, name.as_bytes())?;
            encode_object(bytes, *target);
        }
        RevisionSelector::Head { worktree } => {
            bytes.push(3);
            encode_length_prefixed(bytes, worktree.as_bytes())?;
        }
        RevisionSelector::Index { worktree } => {
            bytes.push(4);
            encode_length_prefixed(bytes, worktree.as_bytes())?;
        }
        RevisionSelector::WorkingTree { worktree } => {
            bytes.push(5);
            encode_length_prefixed(bytes, worktree.as_bytes())?;
        }
    }
    Ok(())
}

fn encode_object(bytes: &mut Vec<u8>, object: ObjectId) {
    match object {
        ObjectId::Sha1(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value);
        }
        ObjectId::Sha256(value) => {
            bytes.push(2);
            bytes.extend_from_slice(&value);
        }
    }
}

fn encode_length_prefixed(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), GitCollectError> {
    let length = u64::try_from(value.len()).map_err(|_| GitCollectError::CommandOutputLimit {
        operation: GitCollectOperation::Diff,
        maximum: HARD_MAX_COMMAND_OUTPUT_BYTES,
    })?;
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

fn collect_submodules(
    runner: &GitRunner<'_>,
    format: ObjectFormat,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<Vec<SubmoduleState>, GitCollectError> {
    let output = runner.required(
        GitCollectOperation::Submodules,
        ["ls-files", "--stage", "-z"],
        cancellation,
    )?;
    let mut submodules = Vec::new();
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        cancellation.check()?;
        let mut fields = record.splitn(2, |byte| *byte == b'\t');
        let metadata = fields.next().ok_or(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::Submodules,
        })?;
        let path = fields.next().ok_or(GitCollectError::InvalidOutput {
            operation: GitCollectOperation::Submodules,
        })?;
        let mut metadata_fields = metadata.split(|byte| *byte == b' ');
        let mode = required_field(&mut metadata_fields, GitCollectOperation::Submodules)?;
        let object = required_field(&mut metadata_fields, GitCollectOperation::Submodules)?;
        let stage = required_field(&mut metadata_fields, GitCollectOperation::Submodules)?;
        if metadata_fields.next().is_some() {
            return Err(GitCollectError::InvalidOutput {
                operation: GitCollectOperation::Submodules,
            });
        }
        if mode == b"160000" && stage == b"0" {
            submodules.push(SubmoduleState {
                worktree: PRIMARY_WORKTREE.to_owned(),
                path: parse_text(path, GitCollectOperation::Submodules)?,
                recorded_commit: parse_object_id(object, format, GitCollectOperation::Submodules)?,
                checkout: SubmoduleCheckoutState::Unknown,
            });
            if submodules.len() > limits.max_changes() {
                return Err(GitContractError::CollectionLimit {
                    collection: GitCollection::Submodules,
                    maximum: limits.max_changes(),
                }
                .into());
            }
        }
    }
    Ok(submodules)
}

fn parse_object_id(
    value: &[u8],
    format: ObjectFormat,
    operation: GitCollectOperation,
) -> Result<ObjectId, GitCollectError> {
    match format {
        ObjectFormat::Sha1 => {
            let bytes = parse_hex::<20>(value, operation)?;
            Ok(ObjectId::sha1(bytes))
        }
        ObjectFormat::Sha256 => {
            let bytes = parse_hex::<32>(value, operation)?;
            Ok(ObjectId::sha256(bytes))
        }
    }
}

fn parse_hex<const N: usize>(
    value: &[u8],
    operation: GitCollectOperation,
) -> Result<[u8; N], GitCollectError> {
    let expected = N
        .checked_mul(2)
        .ok_or(GitCollectError::InvalidOutput { operation })?;
    if value.len() != expected {
        return Err(GitCollectError::InvalidOutput { operation });
    }
    let mut parsed = [0_u8; N];
    for (target, pair) in parsed.iter_mut().zip(value.chunks_exact(2)) {
        let [high, low] = pair else {
            return Err(GitCollectError::InvalidOutput { operation });
        };
        let high = hex_nibble(*high, operation)?;
        let low = hex_nibble(*low, operation)?;
        *target = high
            .checked_mul(16)
            .and_then(|high| high.checked_add(low))
            .ok_or(GitCollectError::InvalidOutput { operation })?;
    }
    Ok(parsed)
}

fn hex_nibble(value: u8, operation: GitCollectOperation) -> Result<u8, GitCollectError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(GitCollectError::InvalidOutput { operation }),
    }
}

fn parse_text(value: &[u8], operation: GitCollectOperation) -> Result<String, GitCollectError> {
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|_| GitCollectError::InvalidOutput { operation })
}

fn trim_ascii(value: &[u8]) -> &[u8] {
    value.trim_ascii()
}
