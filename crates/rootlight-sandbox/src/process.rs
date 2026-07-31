//! Safe process command and ownership facade.

use std::{
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::PathBuf,
    process::ExitStatus,
    time::Instant,
};

use crate::{ProcessError, platform};

/// Explicit stream policy for one child standard handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioMode {
    /// Connect the child stream to the operating system's null device.
    Null,
    /// Create a private anonymous pipe retained by the parent.
    Piped,
}

/// Fully explicit process-creation request.
#[derive(Debug, Clone)]
pub struct ProcessCommand {
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
    pub(crate) clear_environment: bool,
    pub(crate) current_directory: Option<PathBuf>,
    pub(crate) stdin: StdioMode,
    pub(crate) stdout: StdioMode,
    pub(crate) stderr: StdioMode,
}

impl ProcessCommand {
    /// Creates a command for an absolute executable path.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: Vec::new(),
            clear_environment: false,
            current_directory: None,
            stdin: StdioMode::Null,
            stdout: StdioMode::Null,
            stderr: StdioMode::Null,
        }
    }

    /// Appends one literal argument.
    #[must_use]
    pub fn arg(mut self, argument: impl AsRef<OsStr>) -> Self {
        self.arguments.push(argument.as_ref().to_owned());
        self
    }

    /// Sets one child-only environment variable.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.environment
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Starts the child from an empty environment before applying overrides.
    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.clear_environment = true;
        self
    }

    /// Selects the child working directory.
    #[must_use]
    pub fn current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.current_directory = Some(directory.into());
        self
    }

    /// Selects the child standard-input policy.
    #[must_use]
    pub fn stdin(mut self, mode: StdioMode) -> Self {
        self.stdin = mode;
        self
    }

    /// Selects the child standard-output policy.
    #[must_use]
    pub fn stdout(mut self, mode: StdioMode) -> Self {
        self.stdout = mode;
        self
    }

    /// Selects the child standard-error policy.
    #[must_use]
    pub fn stderr(mut self, mode: StdioMode) -> Self {
        self.stderr = mode;
        self
    }

    /// Starts the process without tree-scoped termination semantics.
    ///
    /// # Errors
    ///
    /// Returns an error when command validation or operating-system process
    /// creation fails.
    pub fn spawn(self) -> Result<ChildProcess, ProcessError> {
        platform::spawn(self).map(ChildProcess::from_inner)
    }
}

/// Exact owned child process and its selected parent-side streams.
#[derive(Debug)]
pub struct ChildProcess {
    inner: platform::ChildProcess,
}

impl ChildProcess {
    pub(crate) fn from_inner(inner: platform::ChildProcess) -> Self {
        Self { inner }
    }

    /// Returns the operating-system process identifier captured at creation.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.inner.id()
    }

    /// Takes the parent writer for piped standard input.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.inner.take_stdin().map(ChildStdin::from_inner)
    }

    /// Takes the parent reader for piped standard output.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.inner.take_stdout().map(ChildStdout::from_inner)
    }

    /// Takes the parent reader for piped standard error.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.inner.take_stderr().map(ChildStderr::from_inner)
    }

    /// Returns the exit status when the exact child has terminated.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact process handle cannot be queried.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        self.inner.try_wait()
    }

    /// Terminates the exact child process.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact process handle cannot be terminated.
    pub fn terminate(&mut self) -> Result<(), ProcessError> {
        self.inner.terminate()
    }
}

/// Parent writer connected only to one child's standard input.
#[derive(Debug)]
pub struct ChildStdin {
    inner: platform::ChildStdin,
}

impl ChildStdin {
    fn from_inner(inner: platform::ChildStdin) -> Self {
        Self { inner }
    }
}

impl Write for ChildStdin {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Parent reader connected only to one child's standard output.
#[derive(Debug)]
pub struct ChildStdout {
    inner: platform::ChildStdout,
}

impl ChildStdout {
    fn from_inner(inner: platform::ChildStdout) -> Self {
        Self { inner }
    }
}

impl Read for ChildStdout {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

/// Parent reader connected only to one child's standard error.
#[derive(Debug)]
pub struct ChildStderr {
    inner: platform::ChildStderr,
}

impl ChildStderr {
    fn from_inner(inner: platform::ChildStderr) -> Self {
        Self { inner }
    }
}

impl Read for ChildStderr {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

/// Owned kill-on-close process tree.
#[derive(Debug)]
pub struct KillOnCloseJob {
    inner: platform::KillOnCloseJob,
}

impl KillOnCloseJob {
    /// Creates an empty process tree that terminates when its final handle closes.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform cannot create kill-on-close
    /// containment.
    pub fn new() -> Result<Self, ProcessError> {
        platform::KillOnCloseJob::new().map(|inner| Self { inner })
    }

    /// Starts a suspended process, assigns it to this tree, and then resumes it.
    ///
    /// # Errors
    ///
    /// Returns an error when command validation, process creation, tree
    /// assignment, or thread resumption fails. A child that cannot be assigned
    /// is terminated before executing user code.
    pub fn spawn(&self, command: ProcessCommand) -> Result<ChildProcess, ProcessError> {
        platform::spawn_in_job(command, &self.inner).map(ChildProcess::from_inner)
    }

    /// Returns the number of live processes assigned to this tree.
    ///
    /// # Errors
    ///
    /// Returns an error when Job Object accounting cannot be queried.
    pub fn active_processes(&self) -> Result<u32, ProcessError> {
        self.inner.active_processes()
    }

    /// Terminates every process assigned to this tree.
    ///
    /// # Errors
    ///
    /// Returns an error when tree termination fails.
    pub fn terminate(&self, exit_code: u32) -> Result<(), ProcessError> {
        self.inner.terminate(exit_code)
    }

    /// Waits until Job Object accounting reports no active process.
    ///
    /// # Errors
    ///
    /// Returns an error when accounting fails or the deadline expires.
    pub fn wait_empty(&self, deadline: Instant) -> Result<(), ProcessError> {
        self.inner.wait_empty(deadline)
    }

    /// Transfers the final kill-on-close lease into the contained child.
    ///
    /// This is intended only for an authenticated detached-process handoff.
    /// The parent-side job handle closes after the duplicate is installed in
    /// the child, so failed handoff still terminates the contained tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform cannot duplicate the containment
    /// lease into the exact child process.
    pub fn handoff(self, child: &ChildProcess) -> Result<(), ProcessError> {
        self.inner.handoff(&child.inner)
    }
}
