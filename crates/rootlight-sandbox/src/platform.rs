//! Operating-system process backend selection.

#[cfg(windows)]
mod os;
#[cfg(not(windows))]
mod portable;

#[cfg(windows)]
pub(crate) use os::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, IsolatedAdapterProcess, KillOnCloseJob,
    probe_windows_adapter_isolation, spawn, spawn_in_job, spawn_windows_isolated_adapter,
};
#[cfg(not(windows))]
pub(crate) use portable::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, IsolatedAdapterProcess, KillOnCloseJob,
    probe_windows_adapter_isolation, spawn, spawn_in_job, spawn_windows_isolated_adapter,
};
