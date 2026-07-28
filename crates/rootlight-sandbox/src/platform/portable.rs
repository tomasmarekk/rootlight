//! Safe standard-library process backend for non-Windows targets.

use std::{
    process::{Child as StdChild, ChildStderr as StdChildStderr, ChildStdin as StdChildStdin},
    process::{ChildStdout as StdChildStdout, Command, ExitStatus, Stdio},
    time::Instant,
};

use crate::{
    AdapterIsolationReport, AdapterProcessCommand, AdapterSandboxLimits, ProcessCommand,
    ProcessError, StdioMode,
};

#[derive(Debug)]
pub(crate) struct ChildProcess {
    child: StdChild,
}

pub(crate) type ChildStdin = StdChildStdin;
pub(crate) type ChildStdout = StdChildStdout;
pub(crate) type ChildStderr = StdChildStderr;

impl ChildProcess {
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        self.child
            .try_wait()
            .map_err(|source| ProcessError::io("query child status", source))
    }

    pub(crate) fn terminate(&mut self) -> Result<(), ProcessError> {
        self.child
            .kill()
            .map_err(|source| ProcessError::io("terminate child", source))
    }
}

#[derive(Debug)]
pub(crate) struct KillOnCloseJob;
#[derive(Debug)]
pub(crate) struct IsolatedAdapterProcess;

impl KillOnCloseJob {
    pub(crate) fn new() -> Result<Self, ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn active_processes(&self) -> Result<u32, ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn terminate(&self, _exit_code: u32) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn wait_empty(&self, _deadline: Instant) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }
}

pub(crate) fn spawn(command: ProcessCommand) -> Result<ChildProcess, ProcessError> {
    validate_command(&command)?;
    let mut process = Command::new(command.program);
    process.args(command.arguments);
    if command.clear_environment {
        process.env_clear();
    }
    process.envs(command.environment);
    if let Some(directory) = command.current_directory {
        process.current_dir(directory);
    }
    process
        .stdin(stdio(command.stdin))
        .stdout(stdio(command.stdout))
        .stderr(stdio(command.stderr));
    process
        .spawn()
        .map(|child| ChildProcess { child })
        .map_err(|source| ProcessError::io("create child", source))
}

pub(crate) fn spawn_in_job(
    _command: ProcessCommand,
    _job: &KillOnCloseJob,
) -> Result<ChildProcess, ProcessError> {
    Err(ProcessError::UnsupportedPlatform)
}

pub(crate) fn probe_windows_adapter_isolation(
    _command: ProcessCommand,
    _limits: AdapterSandboxLimits,
) -> Result<AdapterIsolationReport, ProcessError> {
    Err(ProcessError::UnsupportedPlatform)
}

pub(crate) fn spawn_windows_isolated_adapter(
    command: AdapterProcessCommand,
    _limits: AdapterSandboxLimits,
) -> Result<(IsolatedAdapterProcess, AdapterIsolationReport), ProcessError> {
    let _ = (
        command.program,
        command.arguments,
        command.input_limit,
        command.output_limit,
        command.diagnostic_limit,
    );
    Err(ProcessError::UnsupportedPlatform)
}

impl IsolatedAdapterProcess {
    pub(crate) fn id(&self) -> u32 {
        0
    }

    pub(crate) fn input_limit(&self) -> usize {
        0
    }

    pub(crate) fn output_limit(&self) -> usize {
        0
    }

    pub(crate) fn diagnostic_limit(&self) -> usize {
        0
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        None
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        None
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        None
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn terminate(&self) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn wait_empty(&self, _deadline: Instant) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn fail_closed_cleanup(self) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }
}

fn validate_command(command: &ProcessCommand) -> Result<(), ProcessError> {
    if !command.program.is_absolute() {
        return Err(ProcessError::InvalidInput(
            "the executable path must be absolute".to_owned(),
        ));
    }
    if let Some(directory) = &command.current_directory
        && !directory.is_absolute()
    {
        return Err(ProcessError::InvalidInput(
            "the working directory must be absolute".to_owned(),
        ));
    }
    Ok(())
}

fn stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Null => Stdio::null(),
        StdioMode::Piped => Stdio::piped(),
    }
}
