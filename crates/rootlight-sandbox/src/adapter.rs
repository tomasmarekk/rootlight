//! Auditable Windows deep-adapter process isolation and capability evidence.
//!
//! A live report belongs to the exact process that was created, assigned to
//! its Job Object, token-verified, and resumed. A separate conservative probe
//! can verify pre-execution setup, but never claims runtime-only controls.

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::{ProcessCommand, ProcessError, platform};

/// Required isolation control for a deep adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterControl {
    /// Source enters through bounded standard input without repository paths.
    FilesystemView,
    /// Rootlight grants no temporary storage, establishing a zero-byte ceiling.
    TemporaryDirectory,
    /// DNS and network endpoint creation are denied.
    NetworkEgress,
    /// The adapter cannot create an unowned descendant.
    ProcessCreation,
    /// The operating system enforces a hard aggregate memory ceiling.
    Memory,
    /// The operating system enforces a hard aggregate CPU-time ceiling.
    Cpu,
    /// Process creation inherits only the three explicit standard handles.
    Handles,
    /// Dynamic-library search excludes ambient attacker-controlled locations.
    DynamicLibrarySearch,
    /// Every descendant terminates when the owning host scope closes.
    DescendantCleanup,
}

/// Native mechanism contributing to one isolation control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterIsolationMechanism {
    /// Approved inputs are staged with an AppContainer read-only ACL.
    StagedInputAppContainerAcl,
    /// Adapter source crosses only a bounded standard-input pipe.
    SourceViaBoundedStdin,
    /// Repository and user-home paths are withheld from the child contract.
    RepositoryPathsWithheld,
    /// The AppContainer token cannot access the current user's profile.
    UserProfileDenied,
    /// The staged executable is immutable to the AppContainer token.
    StagedExecutableAppContainerAcl,
    /// A writable temporary directory has a private AppContainer ACL.
    PrivateTemporaryDirectoryAcl,
    /// Temporary writes are denied, establishing a hard zero-byte ceiling.
    TemporaryStorageDenied,
    /// The ephemeral AppContainer profile is deleted before user code runs.
    AppContainerProfileStorageRemoved,
    /// A hard temporary-storage byte ceiling is enforced.
    HardTemporaryByteLimit,
    /// The process is an AppContainer with no network capabilities.
    AppContainerWithoutNetworkCapabilities,
    /// A Job Object permits only the initial adapter process.
    JobActiveProcessLimit,
    /// A Job Object enforces aggregate committed memory.
    JobMemoryLimit,
    /// A Job Object enforces aggregate user-mode CPU time.
    JobCpuTimeLimit,
    /// Process creation inherits exactly the three explicit standard handles.
    InheritedHandleAllowlist,
    /// The total number of handles opened at runtime has a hard ceiling.
    RuntimeHandleCountLimit,
    /// Process-creation mitigations reject remote and low-integrity images.
    ImageLoadMitigations,
    /// The child starts in an empty host-owned read-only directory.
    ControlledReadOnlyCurrentDirectory,
    /// The current directory is absent from dynamic-library search.
    CurrentDirectoryDllExclusion,
    /// Closing the owning Job Object terminates every assigned process.
    KillOnCloseJob,
}

/// Observed status of one native mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterMechanismEvidence {
    mechanism: AdapterIsolationMechanism,
    enforced: bool,
    reason_code: &'static str,
}

impl AdapterMechanismEvidence {
    /// Returns the native mechanism being reported.
    #[must_use]
    pub const fn mechanism(self) -> AdapterIsolationMechanism {
        self.mechanism
    }

    /// Returns whether native setup succeeded for the probed process.
    #[must_use]
    pub const fn is_enforced(self) -> bool {
        self.enforced
    }

    /// Returns a stable source-free reason for the status.
    #[must_use]
    pub const fn reason_code(self) -> &'static str {
        self.reason_code
    }

    #[cfg(any(windows, test))]
    pub(crate) const fn enforced(
        mechanism: AdapterIsolationMechanism,
        reason_code: &'static str,
    ) -> Self {
        Self {
            mechanism,
            enforced: true,
            reason_code,
        }
    }

    #[cfg(any(windows, test))]
    pub(crate) const fn unavailable(
        mechanism: AdapterIsolationMechanism,
        reason_code: &'static str,
    ) -> Self {
        Self {
            mechanism,
            enforced: false,
            reason_code,
        }
    }
}

/// Composite enforcement status for one required control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterControlEvidence {
    control: AdapterControl,
    enforced: bool,
    reason_code: &'static str,
}

impl AdapterControlEvidence {
    /// Returns the required control being reported.
    #[must_use]
    pub const fn control(self) -> AdapterControl {
        self.control
    }

    /// Returns whether every mechanism required by the control is enforced.
    #[must_use]
    pub const fn is_enforced(self) -> bool {
        self.enforced
    }

    /// Returns a stable source-free reason for the composite status.
    #[must_use]
    pub const fn reason_code(self) -> &'static str {
        self.reason_code
    }
}

/// Native-isolation evidence for a probe or an exact live adapter process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterIsolationReport {
    mechanisms: Vec<AdapterMechanismEvidence>,
}

impl AdapterIsolationReport {
    /// Returns evidence for every independently audited native mechanism.
    #[must_use]
    pub fn mechanisms(&self) -> &[AdapterMechanismEvidence] {
        &self.mechanisms
    }

    /// Returns evidence for one required composite control.
    #[must_use]
    pub fn control(&self, control: AdapterControl) -> AdapterControlEvidence {
        let required = required_mechanisms(control);
        let enforced = required.iter().all(|required_mechanism| {
            self.mechanisms
                .iter()
                .any(|evidence| evidence.mechanism == *required_mechanism && evidence.enforced)
        });
        AdapterControlEvidence {
            control,
            enforced,
            reason_code: if enforced {
                "all_required_native_mechanisms_enforced"
            } else {
                unavailable_reason(control)
            },
        }
    }

    /// Returns whether all nine controls in the narrowed process contract hold.
    ///
    /// Independently reported properties outside that contract, such as a
    /// runtime handle-count quota, remain unavailable and must not be inferred
    /// from this result.
    #[must_use]
    pub fn permits_deep_adapter(&self) -> bool {
        required_controls()
            .iter()
            .all(|control| self.control(*control).is_enforced())
    }

    #[cfg(any(windows, test))]
    pub(crate) fn windows_isolated_process() -> Self {
        use AdapterIsolationMechanism::{
            AppContainerProfileStorageRemoved, AppContainerWithoutNetworkCapabilities,
            ControlledReadOnlyCurrentDirectory, CurrentDirectoryDllExclusion,
            HardTemporaryByteLimit, ImageLoadMitigations, InheritedHandleAllowlist,
            JobActiveProcessLimit, JobCpuTimeLimit, JobMemoryLimit, KillOnCloseJob,
            PrivateTemporaryDirectoryAcl, RepositoryPathsWithheld, RuntimeHandleCountLimit,
            SourceViaBoundedStdin, StagedExecutableAppContainerAcl, StagedInputAppContainerAcl,
            TemporaryStorageDenied, UserProfileDenied,
        };

        Self {
            mechanisms: vec![
                AdapterMechanismEvidence::unavailable(
                    StagedInputAppContainerAcl,
                    "source_files_are_not_exposed_to_the_adapter",
                ),
                AdapterMechanismEvidence::enforced(
                    SourceViaBoundedStdin,
                    "source_crosses_quota_limited_anonymous_pipe",
                ),
                AdapterMechanismEvidence::enforced(
                    RepositoryPathsWithheld,
                    "command_contract_has_no_source_or_repository_path",
                ),
                AdapterMechanismEvidence::enforced(
                    UserProfileDenied,
                    "verified_appcontainer_token_and_empty_environment",
                ),
                AdapterMechanismEvidence::enforced(
                    StagedExecutableAppContainerAcl,
                    "private_runtime_copy_is_appcontainer_read_execute_only",
                ),
                AdapterMechanismEvidence::unavailable(
                    PrivateTemporaryDirectoryAcl,
                    "zero_write_profile_has_no_temporary_directory",
                ),
                AdapterMechanismEvidence::enforced(
                    TemporaryStorageDenied,
                    "no_writable_path_or_temporary_environment_is_granted",
                ),
                AdapterMechanismEvidence::enforced(
                    AppContainerProfileStorageRemoved,
                    "ephemeral_profile_deleted_before_primary_thread_resume",
                ),
                AdapterMechanismEvidence::enforced(
                    HardTemporaryByteLimit,
                    "zero_write_profile_has_zero_byte_limit",
                ),
                AdapterMechanismEvidence::enforced(
                    AppContainerWithoutNetworkCapabilities,
                    "security_capabilities_attribute_has_zero_capabilities",
                ),
                AdapterMechanismEvidence::enforced(
                    JobActiveProcessLimit,
                    "job_active_process_limit_is_one",
                ),
                AdapterMechanismEvidence::enforced(JobMemoryLimit, "job_memory_limit_configured"),
                AdapterMechanismEvidence::enforced(
                    JobCpuTimeLimit,
                    "job_cpu_time_limit_configured",
                ),
                AdapterMechanismEvidence::enforced(
                    InheritedHandleAllowlist,
                    "process_handle_list_contains_three_standard_handles",
                ),
                AdapterMechanismEvidence::unavailable(
                    RuntimeHandleCountLimit,
                    "windows_job_has_no_runtime_handle_count_limit",
                ),
                AdapterMechanismEvidence::enforced(
                    ImageLoadMitigations,
                    "remote_and_low_integrity_images_denied_and_system32_preferred",
                ),
                AdapterMechanismEvidence::unavailable(
                    CurrentDirectoryDllExclusion,
                    "windows_loader_still_considers_the_controlled_current_directory",
                ),
                AdapterMechanismEvidence::enforced(
                    ControlledReadOnlyCurrentDirectory,
                    "current_directory_is_private_empty_and_appcontainer_read_only",
                ),
                AdapterMechanismEvidence::enforced(KillOnCloseJob, "job_kill_on_close_configured"),
            ],
        }
    }

    #[cfg(any(windows, test))]
    pub(crate) fn windows_suspended_probe() -> Self {
        use AdapterIsolationMechanism::{
            AppContainerProfileStorageRemoved, ControlledReadOnlyCurrentDirectory,
            HardTemporaryByteLimit, RepositoryPathsWithheld, SourceViaBoundedStdin,
            StagedExecutableAppContainerAcl, TemporaryStorageDenied, UserProfileDenied,
        };

        let mut report = Self::windows_isolated_process();
        for evidence in &mut report.mechanisms {
            if matches!(
                evidence.mechanism,
                SourceViaBoundedStdin
                    | RepositoryPathsWithheld
                    | UserProfileDenied
                    | StagedExecutableAppContainerAcl
                    | TemporaryStorageDenied
                    | AppContainerProfileStorageRemoved
                    | HardTemporaryByteLimit
                    | ControlledReadOnlyCurrentDirectory
            ) {
                evidence.enforced = false;
                evidence.reason_code = "suspended_probe_does_not_establish_runtime_contract";
            }
        }
        report
    }
}

/// Executable and bounded stream contract for one isolated adapter process.
#[derive(Debug, Clone)]
pub struct AdapterProcessCommand {
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) input_limit: usize,
    pub(crate) output_limit: usize,
    pub(crate) diagnostic_limit: usize,
}

impl AdapterProcessCommand {
    /// Creates an adapter command with explicit nonzero stream quotas.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidInput`] when a stream quota is zero.
    pub fn new(
        program: impl Into<PathBuf>,
        input_limit: usize,
        output_limit: usize,
        diagnostic_limit: usize,
    ) -> Result<Self, ProcessError> {
        if input_limit == 0 || output_limit == 0 || diagnostic_limit == 0 {
            return Err(ProcessError::InvalidInput(
                "adapter stream limits must be nonzero".to_owned(),
            ));
        }
        Ok(Self {
            program: program.into(),
            arguments: Vec::new(),
            input_limit,
            output_limit,
            diagnostic_limit,
        })
    }

    /// Appends one non-path literal argument.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidInput`] when the argument contains path
    /// separators, Windows drive syntax, a root, or a parent-directory
    /// component. Source and repository paths must cross only inside the
    /// bounded protocol payload.
    pub fn arg(mut self, argument: impl AsRef<OsStr>) -> Result<Self, ProcessError> {
        let argument = argument.as_ref();
        let candidate = std::path::Path::new(argument);
        if contains_path_syntax(argument)
            || candidate.has_root()
            || candidate.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::Prefix(_) | std::path::Component::ParentDir
                )
            })
        {
            return Err(ProcessError::InvalidInput(
                "adapter arguments cannot contain path syntax".to_owned(),
            ));
        }
        self.arguments.push(argument.to_owned());
        Ok(self)
    }
}

fn contains_path_syntax(argument: &OsStr) -> bool {
    let rendered = argument.to_string_lossy();
    let bytes = rendered.as_bytes();
    rendered.contains(['/', '\\'])
        || bytes.windows(2).enumerate().any(|(index, pair)| {
            pair[0].is_ascii_alphabetic()
                && pair[1] == b':'
                && (index == 0
                    || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_'))
        })
}

/// Live Windows adapter process held inside its exact native isolation scope.
#[derive(Debug)]
pub struct IsolatedAdapterProcess {
    inner: platform::IsolatedAdapterProcess,
    report: AdapterIsolationReport,
}

impl IsolatedAdapterProcess {
    /// Returns the verified isolation evidence for this exact process.
    #[must_use]
    pub const fn report(&self) -> &AdapterIsolationReport {
        &self.report
    }

    /// Returns the exact operating-system process identifier.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.inner.id()
    }

    /// Takes the quota-limited input writer.
    pub fn take_stdin(&mut self) -> Option<AdapterStdin> {
        let limit = self.inner.input_limit();
        self.inner
            .take_stdin()
            .map(|inner| AdapterStdin::new(inner, limit))
    }

    /// Takes the quota-limited output reader.
    pub fn take_stdout(&mut self) -> Option<AdapterStdout> {
        let limit = self.inner.output_limit();
        self.inner
            .take_stdout()
            .map(|inner| AdapterStdout::new(inner, limit))
    }

    /// Takes the quota-limited diagnostic reader.
    pub fn take_stderr(&mut self) -> Option<AdapterStderr> {
        let limit = self.inner.diagnostic_limit();
        self.inner
            .take_stderr()
            .map(|inner| AdapterStderr::new(inner, limit))
    }

    /// Returns the exit status when the exact adapter has terminated.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact process handle cannot be queried.
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, ProcessError> {
        self.inner.try_wait()
    }

    /// Terminates the complete adapter Job Object.
    ///
    /// # Errors
    ///
    /// Returns an error when native Job termination fails.
    pub fn terminate(&self) -> Result<(), ProcessError> {
        self.inner.terminate()
    }

    /// Waits until every process in the adapter Job Object has exited.
    ///
    /// # Errors
    ///
    /// Returns an error when accounting fails or the deadline expires.
    pub fn wait_empty(&self, deadline: Instant) -> Result<(), ProcessError> {
        self.inner.wait_empty(deadline)
    }

    #[cfg(all(test, windows))]
    pub(crate) fn workspace_root(&self) -> &std::path::Path {
        self.inner.workspace_root()
    }
}

/// Quota-limited adapter input pipe.
#[derive(Debug)]
pub struct AdapterStdin {
    inner: platform::ChildStdin,
    remaining: usize,
}

impl AdapterStdin {
    fn new(inner: platform::ChildStdin, remaining: usize) -> Self {
        Self { inner, remaining }
    }
}

impl Write for AdapterStdin {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "adapter input exceeds its hard byte limit",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.remaining = self.remaining.saturating_sub(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

macro_rules! bounded_reader {
    ($name:ident, $inner:ty) => {
        #[doc = concat!("Quota-limited adapter ", stringify!($name), " pipe.")]
        #[derive(Debug)]
        pub struct $name {
            inner: $inner,
            remaining: usize,
            limit_reached: bool,
        }

        impl $name {
            fn new(inner: $inner, remaining: usize) -> Self {
                Self {
                    inner,
                    remaining,
                    limit_reached: false,
                }
            }
        }

        impl Read for $name {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if buffer.is_empty() {
                    return Ok(0);
                }
                if self.limit_reached {
                    return Err(io::Error::other(
                        "adapter output exceeds its hard byte limit",
                    ));
                }
                if self.remaining == 0 {
                    let mut probe = [0_u8; 1];
                    if self.inner.read(&mut probe)? == 0 {
                        return Ok(0);
                    }
                    self.limit_reached = true;
                    return Err(io::Error::other(
                        "adapter output exceeds its hard byte limit",
                    ));
                }
                let requested = buffer.len().min(self.remaining);
                let read = self.inner.read(&mut buffer[..requested])?;
                self.remaining = self.remaining.saturating_sub(read);
                Ok(read)
            }
        }
    };
}

bounded_reader!(AdapterStdout, platform::ChildStdout);
bounded_reader!(AdapterStderr, platform::ChildStderr);

/// Starts an adapter inside the verified Windows native isolation scope.
///
/// The executable is copied into an operation-owned immutable runtime
/// directory. The child receives no repository path or user environment, and
/// Rootlight grants it no writable filesystem location. Source and results
/// cross only bounded pipes.
///
/// # Errors
///
/// Returns [`ProcessError`] when policy preparation, staging, suspended
/// creation, native verification, profile removal, Job assignment, or primary
/// thread resumption fails. No adapter code runs before every step succeeds.
pub fn spawn_windows_isolated_adapter(
    command: AdapterProcessCommand,
    limits: AdapterSandboxLimits,
) -> Result<IsolatedAdapterProcess, ProcessError> {
    let (inner, report) = platform::spawn_windows_isolated_adapter(command, limits)?;
    if !report.permits_deep_adapter() {
        inner.fail_closed_cleanup()?;
        return Err(ProcessError::InvalidInput(
            "Windows adapter isolation remains incomplete".to_owned(),
        ));
    }
    Ok(IsolatedAdapterProcess { inner, report })
}

/// Hard limits applied to a Windows adapter probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterSandboxLimits {
    memory_bytes: usize,
    cpu_ticks: i64,
}

impl AdapterSandboxLimits {
    /// Validates hard aggregate memory and CPU-time ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidInput`] when either limit is zero or the
    /// CPU duration cannot be represented by the Windows 100-nanosecond unit.
    pub fn new(memory_bytes: usize, cpu_time: Duration) -> Result<Self, ProcessError> {
        if memory_bytes == 0 {
            return Err(ProcessError::InvalidInput(
                "adapter memory limit must be nonzero".to_owned(),
            ));
        }
        if cpu_time.is_zero() {
            return Err(ProcessError::InvalidInput(
                "adapter CPU-time limit must be nonzero".to_owned(),
            ));
        }
        let ticks = cpu_time.as_nanos().div_ceil(100);
        let cpu_ticks = i64::try_from(ticks).map_err(|_| {
            ProcessError::InvalidInput(
                "adapter CPU-time limit exceeds the Windows representation".to_owned(),
            )
        })?;
        Ok(Self {
            memory_bytes,
            cpu_ticks,
        })
    }

    #[cfg(windows)]
    pub(crate) const fn memory_bytes(self) -> usize {
        self.memory_bytes
    }

    #[cfg(windows)]
    pub(crate) const fn cpu_ticks(self) -> i64 {
        self.cpu_ticks
    }
}

/// Probes Windows deep-adapter isolation without executing adapter code.
///
/// The process is created suspended with the explicit standard-handle list,
/// zero-capability AppContainer token, image-load mitigations, and hard Job
/// Object limits. It is then terminated and reaped before its primary thread
/// is resumed. The report distinguishes independently enforced mechanisms
/// from unavailable parts of composite controls.
///
/// # Errors
///
/// Returns [`ProcessError`] when command validation, native policy setup,
/// suspended process creation, Job assignment, termination, or reaping fails.
pub fn probe_windows_adapter_isolation(
    command: ProcessCommand,
    limits: AdapterSandboxLimits,
) -> Result<AdapterIsolationReport, ProcessError> {
    platform::probe_windows_adapter_isolation(command, limits)
}

const fn required_controls() -> [AdapterControl; 9] {
    [
        AdapterControl::FilesystemView,
        AdapterControl::TemporaryDirectory,
        AdapterControl::NetworkEgress,
        AdapterControl::ProcessCreation,
        AdapterControl::Memory,
        AdapterControl::Cpu,
        AdapterControl::Handles,
        AdapterControl::DynamicLibrarySearch,
        AdapterControl::DescendantCleanup,
    ]
}

fn required_mechanisms(control: AdapterControl) -> &'static [AdapterIsolationMechanism] {
    use AdapterIsolationMechanism::{
        AppContainerProfileStorageRemoved, AppContainerWithoutNetworkCapabilities,
        ControlledReadOnlyCurrentDirectory, HardTemporaryByteLimit, ImageLoadMitigations,
        InheritedHandleAllowlist, JobActiveProcessLimit, JobCpuTimeLimit, JobMemoryLimit,
        KillOnCloseJob, RepositoryPathsWithheld, SourceViaBoundedStdin,
        StagedExecutableAppContainerAcl, TemporaryStorageDenied, UserProfileDenied,
    };

    match control {
        AdapterControl::FilesystemView => &[
            SourceViaBoundedStdin,
            RepositoryPathsWithheld,
            UserProfileDenied,
            StagedExecutableAppContainerAcl,
        ],
        AdapterControl::TemporaryDirectory => &[
            TemporaryStorageDenied,
            AppContainerProfileStorageRemoved,
            HardTemporaryByteLimit,
        ],
        AdapterControl::NetworkEgress => &[AppContainerWithoutNetworkCapabilities],
        AdapterControl::ProcessCreation => &[JobActiveProcessLimit],
        AdapterControl::Memory => &[JobMemoryLimit],
        AdapterControl::Cpu => &[JobCpuTimeLimit],
        AdapterControl::Handles => &[InheritedHandleAllowlist],
        AdapterControl::DynamicLibrarySearch => {
            &[ImageLoadMitigations, ControlledReadOnlyCurrentDirectory]
        }
        AdapterControl::DescendantCleanup => &[KillOnCloseJob],
    }
}

const fn unavailable_reason(control: AdapterControl) -> &'static str {
    match control {
        AdapterControl::FilesystemView => "approved_immutable_input_view_unavailable",
        AdapterControl::TemporaryDirectory => "private_bounded_temporary_directory_unavailable",
        AdapterControl::NetworkEgress => "network_egress_denial_unavailable",
        AdapterControl::ProcessCreation => "process_creation_limit_unavailable",
        AdapterControl::Memory => "hard_memory_limit_unavailable",
        AdapterControl::Cpu => "hard_cpu_limit_unavailable",
        AdapterControl::Handles => "inherited_handle_allowlist_unavailable",
        AdapterControl::DynamicLibrarySearch => "secure_dynamic_library_search_unavailable",
        AdapterControl::DescendantCleanup => "descendant_cleanup_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_limits_are_rejected() {
        assert!(AdapterSandboxLimits::new(0, Duration::from_secs(1)).is_err());
        assert!(AdapterSandboxLimits::new(1, Duration::ZERO).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn adapter_arguments_reject_windows_path_escape_forms() {
        let command = || {
            AdapterProcessCommand::new("adapter.exe", 1, 1, 1)
                .expect("nonzero stream quotas validate")
        };

        assert!(command().arg("--protocol-v1").is_ok());
        assert!(command().arg("platform::os::tests::adapter_helper").is_ok());
        assert!(command().arg(r"C:\repository\source.rs").is_err());
        assert!(command().arg(r"C:repository\source.rs").is_err());
        assert!(command().arg(r"\repository\source.rs").is_err());
        assert!(command().arg(r"..\source.rs").is_err());
        assert!(command().arg(r"--source=C:\repository\source.rs").is_err());
        assert!(command().arg("--source=C:source.rs").is_err());
    }

    #[test]
    fn suspended_probe_does_not_overclaim_runtime_controls() {
        let report = AdapterIsolationReport::windows_suspended_probe();
        assert!(report.control(AdapterControl::NetworkEgress).is_enforced());
        assert!(report.control(AdapterControl::Memory).is_enforced());
        assert!(report.control(AdapterControl::Handles).is_enforced());
        assert!(
            !report
                .control(AdapterControl::TemporaryDirectory)
                .is_enforced()
        );
        assert!(
            !report
                .control(AdapterControl::DynamicLibrarySearch)
                .is_enforced()
        );
        assert!(!report.permits_deep_adapter());
    }

    #[test]
    fn isolated_process_uses_the_explicitly_narrowed_windows_contract() {
        let report = AdapterIsolationReport::windows_isolated_process();
        assert!(report.permits_deep_adapter());
        let runtime_handles = report
            .mechanisms()
            .iter()
            .find(|evidence| {
                evidence.mechanism() == AdapterIsolationMechanism::RuntimeHandleCountLimit
            })
            .expect("runtime-handle evidence exists");
        assert!(!runtime_handles.is_enforced());
        let cwd_exclusion = report
            .mechanisms()
            .iter()
            .find(|evidence| {
                evidence.mechanism() == AdapterIsolationMechanism::CurrentDirectoryDllExclusion
            })
            .expect("current-directory evidence exists");
        assert!(!cwd_exclusion.is_enforced());
    }
}
