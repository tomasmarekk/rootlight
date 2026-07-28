//! Operating-system process backend selection.

#[cfg(windows)]
mod os;
#[cfg(not(windows))]
mod portable;

#[cfg(windows)]
pub(crate) use os::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, KillOnCloseJob, spawn, spawn_in_job,
};
#[cfg(not(windows))]
pub(crate) use portable::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, KillOnCloseJob, spawn, spawn_in_job,
};
