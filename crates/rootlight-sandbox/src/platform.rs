//! Operating-system process backend selection.

#[cfg(windows)]
mod os;
#[cfg(all(not(windows), not(any(target_os = "linux", target_os = "macos"))))]
mod portable;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;

#[cfg(windows)]
pub(crate) use os::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, IsolatedAdapterProcess, KillOnCloseJob,
    probe_windows_adapter_isolation, spawn, spawn_in_job,
    spawn_windows_isolated_adapter as spawn_isolated_adapter,
};
#[cfg(all(not(windows), not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) use portable::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, IsolatedAdapterProcess, KillOnCloseJob,
    probe_windows_adapter_isolation, spawn, spawn_in_job,
    spawn_windows_isolated_adapter as spawn_isolated_adapter,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use unix::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, IsolatedAdapterProcess, KillOnCloseJob,
    enter_isolated_adapter_launcher, probe_windows_adapter_isolation, spawn, spawn_in_job,
    spawn_isolated_adapter,
};
