//! Windows process creation with explicit handle inheritance and tree ownership.

#![allow(
    unsafe_code,
    reason = "Win32 process, pipe, attribute-list, and Job Object APIs have no safe stable-Rust wrapper"
)]

use std::{
    cmp::Ordering,
    env,
    ffi::{OsStr, c_void},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
        process::ExitStatusExt as _,
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle, GENERIC_READ,
            GENERIC_WRITE, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS,
            SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
        Security::{
            ACL, ACL_REVISION, AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE,
            Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
            FreeSid, GetLengthSid, GetTokenInformation, InitializeAcl,
            InitializeSecurityDescriptor,
            Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile},
            OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
            SECURITY_CAPABILITIES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenIsAppContainer, TokenUser,
        },
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING, SetFileAttributesW,
        },
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_JOB_TIME,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_TERMINATE_AT_END_OF_JOB,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_END_OF_JOB_TIME_INFORMATION,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
                JobObjectEndOfJobTimeInformation, JobObjectExtendedLimitInformation,
                QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
            },
            Pipes::CreatePipe,
            SystemServices::SECURITY_DESCRIPTOR_REVISION,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
                DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
                GetExitCodeProcess, InitializeProcThreadAttributeList,
                LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

use crate::{
    AdapterExecutableDigest, AdapterIsolationReport, AdapterProcessCommand, AdapterSandboxLimits,
    MAX_ADAPTER_EXECUTABLE_BYTES, ProcessCommand, ProcessError, StdioMode,
    adapter::copy_authenticated_executable,
};

const PROCESS_TERMINATION_EXIT_CODE: u32 = 1;
const FAILED_THREAD_RESUME: u32 = u32::MAX;
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
// These values are defined by the Windows SDK as one-bit policy selections
// shifted into the image-load mitigation fields.
const IMAGE_LOAD_NO_REMOTE_ALWAYS_ON: u64 = 1_u64 << 52;
const IMAGE_LOAD_NO_LOW_LABEL_ALWAYS_ON: u64 = 1_u64 << 56;
const IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON: u64 = 1_u64 << 60;
const ADAPTER_IMAGE_LOAD_POLICY: u64 = IMAGE_LOAD_NO_REMOTE_ALWAYS_ON
    | IMAGE_LOAD_NO_LOW_LABEL_ALWAYS_ON
    | IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON;

static SPAWN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static APPCONTAINER_PROFILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct ChildProcess {
    process: OwnedHandle,
    process_id: u32,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

#[derive(Debug)]
pub(crate) struct ChildStdin(File);

#[derive(Debug)]
pub(crate) struct ChildStdout(File);

#[derive(Debug)]
pub(crate) struct ChildStderr(File);

pub(crate) struct IsolatedAdapterProcess {
    job: KillOnCloseJob,
    child: ChildProcess,
    workspace: Option<PrivateAdapterWorkspace>,
    input_limit: usize,
    output_limit: usize,
    diagnostic_limit: usize,
    cleaned: bool,
}

impl std::fmt::Debug for IsolatedAdapterProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IsolatedAdapterProcess")
            .field("process_id", &self.child.process_id)
            .field("input_limit", &self.input_limit)
            .field("output_limit", &self.output_limit)
            .field("diagnostic_limit", &self.diagnostic_limit)
            .field("cleaned", &self.cleaned)
            .finish_non_exhaustive()
    }
}

impl Write for ChildStdin {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Read for ChildStdout {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Read for ChildStderr {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}

impl ChildProcess {
    pub(crate) fn id(&self) -> u32 {
        self.process_id
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        match wait_result(&self.process, 0)? {
            WaitResult::Running => Ok(None),
            WaitResult::Exited => exit_status(&self.process).map(Some),
        }
    }

    pub(crate) fn terminate(&mut self) -> Result<(), ProcessError> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        // SAFETY: the retained handle is the exact live child process created
        // by this object and the exit code has no pointer or lifetime contract.
        unsafe { TerminateProcess(as_handle(&self.process), PROCESS_TERMINATION_EXIT_CODE) }
            .map_err(|source| ProcessError::windows("terminate child", source))
    }
}

impl IsolatedAdapterProcess {
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn input_limit(&self) -> usize {
        self.input_limit
    }

    pub(crate) fn output_limit(&self) -> usize {
        self.output_limit
    }

    pub(crate) fn diagnostic_limit(&self) -> usize {
        self.diagnostic_limit
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.take_stdin()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.take_stdout()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.take_stderr()
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        self.child.try_wait()
    }

    pub(crate) fn terminate(&self) -> Result<(), ProcessError> {
        self.job.terminate(PROCESS_TERMINATION_EXIT_CODE)
    }

    pub(crate) fn wait_empty(&self, deadline: Instant) -> Result<(), ProcessError> {
        self.job.wait_empty(deadline)
    }

    #[cfg(test)]
    pub(crate) fn workspace_root(&self) -> &Path {
        &self
            .workspace
            .as_ref()
            .expect("live adapter retains its workspace")
            .root
    }

    pub(crate) fn fail_closed_cleanup(mut self) -> Result<(), ProcessError> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<(), ProcessError> {
        if self.cleaned {
            return Ok(());
        }
        let child_exited = matches!(wait_result(&self.child.process, 0), Ok(WaitResult::Exited));
        if !child_exited {
            self.job.terminate(PROCESS_TERMINATION_EXIT_CODE)?;
        }
        self.job
            .wait_empty(Instant::now() + Duration::from_secs(5))?;
        if !matches!(wait_result(&self.child.process, 5_000)?, WaitResult::Exited) {
            return Err(ProcessError::Deadline {
                operation: "reap isolated adapter before workspace cleanup",
            });
        }
        if let Some(workspace) = self.workspace.take() {
            workspace.remove()?;
        }
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for IsolatedAdapterProcess {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[derive(Debug)]
pub(crate) struct KillOnCloseJob {
    job: OwnedHandle,
}

impl KillOnCloseJob {
    pub(crate) fn new() -> Result<Self, ProcessError> {
        // SAFETY: no security descriptor or shared name is supplied; the
        // returned handle is immediately adopted by one OwnedHandle.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|source| ProcessError::windows("create Job Object", source))?;
        let job = owned_handle(job);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the information class matches the initialized structure and
        // its storage remains valid for the complete synchronous call.
        unsafe {
            SetInformationJobObject(
                as_handle(&job),
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                structure_size::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()?,
            )
        }
        .map_err(|source| ProcessError::windows("configure Job Object", source))?;
        Ok(Self { job })
    }

    pub(crate) fn active_processes(&self) -> Result<u32, ProcessError> {
        self.accounting()
            .map(|accounting| accounting.ActiveProcesses)
    }

    fn accounting(&self) -> Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, ProcessError> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the information class matches the writable accounting
        // structure and the kernel writes at most the declared byte length.
        unsafe {
            QueryInformationJobObject(
                Some(as_handle(&self.job)),
                JobObjectBasicAccountingInformation,
                ptr::from_mut(&mut accounting).cast(),
                structure_size::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()?,
                None,
            )
        }
        .map_err(|source| ProcessError::windows("query Job Object accounting", source))?;
        Ok(accounting)
    }

    #[cfg(test)]
    fn extended_limits(&self) -> Result<JOBOBJECT_EXTENDED_LIMIT_INFORMATION, ProcessError> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: the information class matches the writable limits structure
        // and the kernel writes at most the declared byte length.
        unsafe {
            QueryInformationJobObject(
                Some(as_handle(&self.job)),
                JobObjectExtendedLimitInformation,
                ptr::from_mut(&mut limits).cast(),
                structure_size::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()?,
                None,
            )
        }
        .map_err(|source| ProcessError::windows("query adapter Job Object limits", source))?;
        Ok(limits)
    }

    pub(crate) fn terminate(&self, exit_code: u32) -> Result<(), ProcessError> {
        // SAFETY: the retained handle names this Job Object and the exit code
        // is copied by value.
        unsafe { TerminateJobObject(as_handle(&self.job), exit_code) }
            .map_err(|source| ProcessError::windows("terminate Job Object", source))
    }

    pub(crate) fn wait_empty(&self, deadline: Instant) -> Result<(), ProcessError> {
        loop {
            if self.active_processes()? == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ProcessError::Deadline {
                    operation: "wait for empty Job Object",
                });
            }
            thread::sleep(WAIT_POLL_INTERVAL);
        }
    }

    pub(crate) fn handoff(self, child: &ChildProcess) -> Result<(), ProcessError> {
        let mut remote_handle = HANDLE::default();
        // SAFETY: both source handles are owned by this process, the target is
        // the exact child created by this module, and the output slot is valid
        // for the complete synchronous duplication call. The returned numeric
        // value belongs to the child process and must not be closed here.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                as_handle(&self.job),
                as_handle(&child.process),
                &mut remote_handle,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(|source| ProcessError::windows("handoff Job Object", source))
    }

    fn with_adapter_limits(limits: AdapterSandboxLimits) -> Result<Self, ProcessError> {
        // SAFETY: no security descriptor or shared name is supplied; the
        // returned handle is immediately adopted by one OwnedHandle.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|source| ProcessError::windows("create adapter Job Object", source))?;
        let job = owned_handle(job);
        let mut native_limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        native_limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_JOB_TIME;
        native_limits.BasicLimitInformation.ActiveProcessLimit = 1;
        native_limits.BasicLimitInformation.PerJobUserTimeLimit = limits.cpu_ticks();
        native_limits.JobMemoryLimit = limits.memory_bytes();
        // SAFETY: the information class matches the initialized structure and
        // its storage remains valid for the complete synchronous call.
        unsafe {
            SetInformationJobObject(
                as_handle(&job),
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&native_limits).cast(),
                structure_size::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()?,
            )
        }
        .map_err(|source| ProcessError::windows("configure adapter Job Object", source))?;
        let end_action = JOBOBJECT_END_OF_JOB_TIME_INFORMATION {
            EndOfJobTimeAction: JOB_OBJECT_TERMINATE_AT_END_OF_JOB,
        };
        // SAFETY: the information class matches the initialized end-action
        // structure and its storage remains valid for the synchronous call.
        unsafe {
            SetInformationJobObject(
                as_handle(&job),
                JobObjectEndOfJobTimeInformation,
                ptr::from_ref(&end_action).cast(),
                structure_size::<JOBOBJECT_END_OF_JOB_TIME_INFORMATION>()?,
            )
        }
        .map_err(|source| ProcessError::windows("configure adapter CPU end action", source))?;
        Ok(Self { job })
    }
}

pub(crate) fn spawn(command: ProcessCommand) -> Result<ChildProcess, ProcessError> {
    spawn_inner(command, None)
}

pub(crate) fn spawn_in_job(
    command: ProcessCommand,
    job: &KillOnCloseJob,
) -> Result<ChildProcess, ProcessError> {
    spawn_inner(command, Some(job))
}

pub(crate) fn probe_windows_adapter_isolation(
    command: ProcessCommand,
    limits: AdapterSandboxLimits,
) -> Result<AdapterIsolationReport, ProcessError> {
    validate_command(&command)?;
    let application = nul_terminated(command.program.as_os_str(), "executable path")?;
    let mut command_line = command_line(&command)?;
    let environment = environment_block(&command)?;
    let current_directory = command
        .current_directory
        .as_deref()
        .map(|path| nul_terminated(path.as_os_str(), "working directory"))
        .transpose()?;

    let lock = SPAWN_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let streams = PreparedStreams::new(command.stdin, command.stdout, command.stderr)?;
    let handles = streams.child_handles();
    verify_inheritable_handles(&handles)?;
    let mut app_container = AppContainerSid::create_ephemeral()?;
    let security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: app_container.sid,
        Capabilities: ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let attributes = AttributeList::with_adapter_profile(
        &handles,
        &security_capabilities,
        &ADAPTER_IMAGE_LOAD_POLICY,
    )?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = structure_size::<STARTUPINFOEXW>()?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.list;

    let job = KillOnCloseJob::with_adapter_limits(limits)?;
    let flags = CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT
        | CREATE_NO_WINDOW
        | CREATE_SUSPENDED;
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: every pointer references initialized storage that remains alive
    // through the synchronous call. The attribute list contains the exact
    // standard handles, zero-capability AppContainer identity, and immutable
    // image-load policy.
    let creation = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            Some(environment.as_ptr().cast::<c_void>()),
            current_directory
                .as_ref()
                .map_or(PCWSTR::null(), |directory| PCWSTR(directory.as_ptr())),
            ptr::from_ref(&startup.StartupInfo),
            ptr::from_mut(&mut information),
        )
    };
    if let Err(source) = creation {
        app_container.delete()?;
        return Err(ProcessError::windows(
            "create suspended adapter probe",
            source,
        ));
    }

    let process = owned_handle(information.hProcess);
    let thread_handle = owned_handle(information.hThread);
    // SAFETY: both retained handles are live kernel objects. The primary
    // thread remains suspended, so no adapter instruction can run.
    let assignment = unsafe { AssignProcessToJobObject(as_handle(&job.job), as_handle(&process)) };
    if let Err(source) = assignment {
        if let Err(cleanup) = terminate_suspended(&process) {
            std::mem::forget(process);
            std::mem::forget(thread_handle);
            return Err(cleanup);
        }
        return Err(ProcessError::windows(
            "assign suspended adapter probe to Job Object",
            source,
        ));
    }
    verify_appcontainer_token(&process)?;
    job.terminate(PROCESS_TERMINATION_EXIT_CODE)?;
    job.wait_empty(Instant::now() + Duration::from_secs(5))?;
    if !matches!(wait_result(&process, 5_000)?, WaitResult::Exited) {
        return Err(ProcessError::Deadline {
            operation: "reap suspended adapter probe",
        });
    }
    drop(thread_handle);
    drop(process);
    drop(attributes);
    app_container.delete()?;
    Ok(AdapterIsolationReport::windows_suspended_probe())
}

pub(crate) fn spawn_windows_isolated_adapter(
    command: AdapterProcessCommand,
    limits: AdapterSandboxLimits,
) -> Result<(IsolatedAdapterProcess, AdapterIsolationReport), ProcessError> {
    let lock = SPAWN_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app_container = AppContainerSid::create_ephemeral()?;
    let current_user = CurrentUserSid::load()?;
    let workspace = PrivateAdapterWorkspace::create(
        &command.program,
        command.expected_executable_digest,
        &current_user,
        app_container.sid,
    )?;
    let mut process_command = ProcessCommand::new(&workspace.executable)
        .env_clear()
        .current_dir(&workspace.current_directory)
        .stdin(StdioMode::Piped)
        .stdout(StdioMode::Piped)
        .stderr(StdioMode::Piped);
    for argument in command.arguments {
        process_command = process_command.arg(argument);
    }
    for key in ["SystemDrive", "SystemRoot", "WINDIR"] {
        if let Some(value) = env::var_os(key) {
            process_command = process_command.env(key, value);
        }
    }
    for key in ["APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "USERPROFILE"] {
        process_command = process_command.env(key, &workspace.current_directory);
    }
    process_command = process_command.env("ROOTLIGHT_ADAPTER_ISOLATED", "1");
    validate_command(&process_command)?;
    let application = nul_terminated(process_command.program.as_os_str(), "adapter executable")?;
    let mut command_line = command_line(&process_command)?;
    let environment = environment_block(&process_command)?;
    let current_directory = nul_terminated(
        workspace.current_directory.as_os_str(),
        "adapter current directory",
    )?;
    let mut streams = PreparedStreams::new(
        process_command.stdin,
        process_command.stdout,
        process_command.stderr,
    )?;
    let handles = streams.child_handles();
    verify_inheritable_handles(&handles)?;
    let security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: app_container.sid,
        Capabilities: ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let attributes = AttributeList::with_adapter_profile(
        &handles,
        &security_capabilities,
        &ADAPTER_IMAGE_LOAD_POLICY,
    )?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = structure_size::<STARTUPINFOEXW>()?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.list;

    let job = KillOnCloseJob::with_adapter_limits(limits)?;
    let flags = CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT
        | CREATE_NO_WINDOW
        | CREATE_SUSPENDED;
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: every pointer references initialized storage that remains alive
    // through the synchronous call. The child is suspended and receives only
    // the exact pipe handles, zero-capability AppContainer identity, and
    // immutable image-load policy.
    let creation = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            Some(environment.as_ptr().cast::<c_void>()),
            PCWSTR(current_directory.as_ptr()),
            ptr::from_ref(&startup.StartupInfo),
            ptr::from_mut(&mut information),
        )
    };
    if let Err(source) = creation {
        app_container.delete()?;
        workspace.remove()?;
        return Err(ProcessError::windows(
            "create suspended isolated adapter",
            source,
        ));
    }

    let process = owned_handle(information.hProcess);
    let thread_handle = owned_handle(information.hThread);
    // SAFETY: both retained handles are live kernel objects. The primary
    // thread remains suspended, so no adapter instruction can run.
    let assignment = unsafe { AssignProcessToJobObject(as_handle(&job.job), as_handle(&process)) };
    if let Err(source) = assignment {
        if let Err(cleanup) = terminate_suspended(&process) {
            std::mem::forget(process);
            std::mem::forget(thread_handle);
            return Err(cleanup);
        }
        app_container.delete()?;
        workspace.remove()?;
        return Err(ProcessError::windows(
            "assign isolated adapter to Job Object",
            source,
        ));
    }
    if let Err(source) = verify_appcontainer_token(&process) {
        terminate_assigned_suspended(&job, &process)?;
        app_container.delete()?;
        workspace.remove()?;
        return Err(source);
    }
    // Removing the profile before resumption denies the AppContainer its
    // otherwise implicit writable profile storage. The token retains its SID.
    if let Err(source) = app_container.delete() {
        terminate_assigned_suspended(&job, &process)?;
        workspace.remove()?;
        return Err(source);
    }
    if let Err(source) = workspace.clear_current_directory() {
        terminate_assigned_suspended(&job, &process)?;
        workspace.remove()?;
        return Err(source);
    }

    // SAFETY: this is the exact primary thread returned by CreateProcessW with
    // CREATE_SUSPENDED after all native controls and profile removal succeeded.
    if unsafe { ResumeThread(as_handle(&thread_handle)) } == FAILED_THREAD_RESUME {
        let source = windows::core::Error::from_win32();
        job.terminate(PROCESS_TERMINATION_EXIT_CODE)?;
        job.wait_empty(Instant::now() + Duration::from_secs(5))?;
        workspace.remove()?;
        return Err(ProcessError::windows("resume isolated adapter", source));
    }
    drop(thread_handle);
    drop(attributes);
    let (stdin, stdout, stderr) = streams.take_parent_streams();
    let child = ChildProcess {
        process,
        process_id: information.dwProcessId,
        stdin,
        stdout,
        stderr,
    };
    let report = AdapterIsolationReport::windows_isolated_process();
    Ok((
        IsolatedAdapterProcess {
            job,
            child,
            workspace: Some(workspace),
            input_limit: command.input_limit,
            output_limit: command.output_limit,
            diagnostic_limit: command.diagnostic_limit,
            cleaned: false,
        },
        report,
    ))
}

fn verify_appcontainer_token(process: &OwnedHandle) -> Result<(), ProcessError> {
    let mut token = HANDLE::default();
    // SAFETY: the retained process handle names the suspended probe. The token
    // output pointer references writable storage and requests query-only access.
    unsafe { OpenProcessToken(as_handle(process), TOKEN_QUERY, &mut token) }
        .map_err(|source| ProcessError::windows("open adapter probe token", source))?;
    let token = owned_handle(token);
    let mut is_app_container = 0_u32;
    let mut returned_bytes = 0_u32;
    // SAFETY: TokenIsAppContainer returns one u32 into initialized writable
    // storage, bounded by the exact declared structure length.
    unsafe {
        GetTokenInformation(
            as_handle(&token),
            TokenIsAppContainer,
            Some(ptr::from_mut(&mut is_app_container).cast()),
            structure_size::<u32>()?,
            ptr::from_mut(&mut returned_bytes),
        )
    }
    .map_err(|source| ProcessError::windows("query adapter AppContainer token", source))?;
    if returned_bytes != structure_size::<u32>()? || is_app_container == 0 {
        return Err(ProcessError::InvalidInput(
            "adapter probe token is not an AppContainer token".to_owned(),
        ));
    }
    Ok(())
}

fn spawn_inner(
    command: ProcessCommand,
    job: Option<&KillOnCloseJob>,
) -> Result<ChildProcess, ProcessError> {
    validate_command(&command)?;
    let application = nul_terminated(command.program.as_os_str(), "executable path")?;
    let mut command_line = command_line(&command)?;
    let environment = environment_block(&command)?;
    let current_directory = command
        .current_directory
        .as_deref()
        .map(|path| nul_terminated(path.as_os_str(), "working directory"))
        .transpose()?;

    let lock = SPAWN_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut streams = PreparedStreams::new(command.stdin, command.stdout, command.stderr)?;
    let handles = streams.child_handles();
    verify_inheritable_handles(&handles)?;
    let attributes = AttributeList::with_handle_list(&handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = structure_size::<STARTUPINFOEXW>()?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.list;

    let mut flags = CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW;
    if job.is_some() {
        flags |= CREATE_SUSPENDED;
    }
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: every pointer references initialized storage that remains alive
    // through the synchronous call. Only the three verified inheritable
    // standard handles are present in the process attribute allowlist.
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            Some(environment.as_ptr().cast::<c_void>()),
            current_directory
                .as_ref()
                .map_or(PCWSTR::null(), |directory| PCWSTR(directory.as_ptr())),
            ptr::from_ref(&startup.StartupInfo),
            ptr::from_mut(&mut information),
        )
    }
    .map_err(|source| ProcessError::windows("create child", source))?;

    let process = owned_handle(information.hProcess);
    let thread_handle = owned_handle(information.hThread);
    if let Some(job) = job {
        // SAFETY: both retained handles are live kernel objects. The primary
        // thread remains suspended, so no child code can run before assignment.
        let assignment =
            unsafe { AssignProcessToJobObject(as_handle(&job.job), as_handle(&process)) };
        if let Err(source) = assignment {
            if let Err(cleanup) = terminate_suspended(&process) {
                // There is no safe execution path for this uncontained,
                // suspended child. Deliberately retain its exact kernel
                // handles when cleanup is unproven instead of erasing the
                // final ownership record while the parent process remains up.
                std::mem::forget(process);
                std::mem::forget(thread_handle);
                return Err(cleanup);
            }
            return Err(ProcessError::windows("assign child to Job Object", source));
        }
        // SAFETY: this is the exact primary thread returned by CreateProcessW
        // with CREATE_SUSPENDED and it has been assigned to the Job Object.
        if unsafe { ResumeThread(as_handle(&thread_handle)) } == FAILED_THREAD_RESUME {
            let source = windows::core::Error::from_win32();
            job.terminate(PROCESS_TERMINATION_EXIT_CODE)?;
            job.wait_empty(Instant::now() + Duration::from_secs(5))?;
            return Err(ProcessError::windows("resume contained child", source));
        }
    }
    drop(thread_handle);
    let (stdin, stdout, stderr) = streams.take_parent_streams();
    Ok(ChildProcess {
        process,
        process_id: information.dwProcessId,
        stdin,
        stdout,
        stderr,
    })
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
    if command
        .program
        .as_os_str()
        .encode_wide()
        .any(|unit| unit == 0)
    {
        return Err(ProcessError::InvalidInput(
            "the executable path contains NUL".to_owned(),
        ));
    }
    if command
        .program
        .as_os_str()
        .encode_wide()
        .any(|unit| unit == u16::from(b'"'))
    {
        return Err(ProcessError::InvalidInput(
            "the executable path contains a quote".to_owned(),
        ));
    }
    Ok(())
}

fn command_line(command: &ProcessCommand) -> Result<Vec<u16>, ProcessError> {
    let mut encoded = Vec::new();
    append_argument(
        &mut encoded,
        command.program.as_os_str(),
        true,
        "executable path",
    )?;
    for argument in &command.arguments {
        encoded.push(u16::from(b' '));
        append_argument(&mut encoded, argument, false, "argument")?;
    }
    encoded.push(0);
    Ok(encoded)
}

fn append_argument(
    target: &mut Vec<u16>,
    argument: &OsStr,
    force_quotes: bool,
    label: &'static str,
) -> Result<(), ProcessError> {
    let units = argument.encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(ProcessError::InvalidInput(format!("{label} contains NUL")));
    }
    let quote = force_quotes
        || units.is_empty()
        || units.iter().any(|unit| {
            *unit == u16::from(b' ') || *unit == u16::from(b'\t') || *unit == u16::from(b'"')
        });
    if !quote {
        target.extend_from_slice(&units);
        return Ok(());
    }

    target.push(u16::from(b'"'));
    let mut backslashes = 0_usize;
    for unit in units {
        if unit == u16::from(b'\\') {
            backslashes = backslashes.saturating_add(1);
        } else if unit == u16::from(b'"') {
            target.extend(std::iter::repeat_n(
                u16::from(b'\\'),
                backslashes.saturating_mul(2).saturating_add(1),
            ));
            target.push(unit);
            backslashes = 0;
        } else {
            target.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
            target.push(unit);
            backslashes = 0;
        }
    }
    target.extend(std::iter::repeat_n(
        u16::from(b'\\'),
        backslashes.saturating_mul(2),
    ));
    target.push(u16::from(b'"'));
    Ok(())
}

fn environment_block(command: &ProcessCommand) -> Result<Vec<u16>, ProcessError> {
    let mut entries = if command.clear_environment {
        Vec::new()
    } else {
        // Windows exposes drive-relative current-directory state through
        // pseudo-variables such as `=C:`. They are not ordinary environment
        // keys and must not cross this explicit child boundary.
        env::vars_os()
            .filter(|(key, _)| !is_drive_current_directory_key(key))
            .collect::<Vec<_>>()
    };
    for (key, value) in &command.environment {
        validate_environment_entry(key, value)?;
        if let Some(entry) = entries
            .iter_mut()
            .find(|(existing, _)| windows_ordinal_cmp(existing, key) == Ordering::Equal)
        {
            *entry = (key.clone(), value.clone());
        } else {
            entries.push((key.clone(), value.clone()));
        }
    }
    for (key, value) in &entries {
        validate_environment_entry(key, value)?;
    }
    entries.sort_by(|(left, _), (right, _)| windows_ordinal_cmp(left, right));

    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn is_drive_current_directory_key(key: &OsStr) -> bool {
    key.encode_wide()
        .next()
        .is_some_and(|unit| unit == u16::from(b'='))
}

fn validate_environment_entry(key: &OsStr, value: &OsStr) -> Result<(), ProcessError> {
    let key_units = key.encode_wide().collect::<Vec<_>>();
    if key_units.is_empty() || key_units.contains(&0) || key_units.contains(&u16::from(b'=')) {
        return Err(ProcessError::InvalidInput(
            "an environment key is empty or contains '=' or NUL".to_owned(),
        ));
    }
    if value.encode_wide().any(|unit| unit == 0) {
        return Err(ProcessError::InvalidInput(
            "an environment value contains NUL".to_owned(),
        ));
    }
    Ok(())
}

fn windows_ordinal_cmp(left: &OsStr, right: &OsStr) -> Ordering {
    let left = left.encode_wide().collect::<Vec<_>>();
    let right = right.encode_wide().collect::<Vec<_>>();
    // SAFETY: both slices contain initialized UTF-16 code units and are alive
    // for the complete comparison; the API accepts explicit slice lengths.
    match unsafe { CompareStringOrdinal(&left, &right, true) } {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        _ => left.cmp(&right),
    }
}

fn nul_terminated(value: &OsStr, label: &'static str) -> Result<Vec<u16>, ProcessError> {
    let mut encoded = value.encode_wide().collect::<Vec<_>>();
    if encoded.contains(&0) {
        return Err(ProcessError::InvalidInput(format!("{label} contains NUL")));
    }
    encoded.push(0);
    Ok(encoded)
}

struct PreparedStreams {
    stdin_child: OwnedHandle,
    stdout_child: OwnedHandle,
    stderr_child: OwnedHandle,
    stdin_parent: Option<File>,
    stdout_parent: Option<File>,
    stderr_parent: Option<File>,
}

impl PreparedStreams {
    fn new(stdin: StdioMode, stdout: StdioMode, stderr: StdioMode) -> Result<Self, ProcessError> {
        let (stdin_child, stdin_parent) = prepare_stdin(stdin)?;
        let (stdout_child, stdout_parent) = prepare_stdout(stdout)?;
        let (stderr_child, stderr_parent) = prepare_stderr(stderr)?;
        Ok(Self {
            stdin_child,
            stdout_child,
            stderr_child,
            stdin_parent,
            stdout_parent,
            stderr_parent,
        })
    }

    fn child_handles(&self) -> [HANDLE; 3] {
        [
            as_handle(&self.stdin_child),
            as_handle(&self.stdout_child),
            as_handle(&self.stderr_child),
        ]
    }

    fn take_parent_streams(
        &mut self,
    ) -> (Option<ChildStdin>, Option<ChildStdout>, Option<ChildStderr>) {
        (
            self.stdin_parent.take().map(ChildStdin),
            self.stdout_parent.take().map(ChildStdout),
            self.stderr_parent.take().map(ChildStderr),
        )
    }
}

fn prepare_stdin(mode: StdioMode) -> Result<(OwnedHandle, Option<File>), ProcessError> {
    match mode {
        StdioMode::Null => open_null(GENERIC_READ.0).map(|handle| (handle, None)),
        StdioMode::Piped => {
            let (reader, writer) = inheritable_pipe()?;
            clear_inherit(&writer)?;
            Ok((reader, Some(File::from(writer))))
        }
    }
}

fn prepare_stdout(mode: StdioMode) -> Result<(OwnedHandle, Option<File>), ProcessError> {
    match mode {
        StdioMode::Null => open_null(GENERIC_WRITE.0).map(|handle| (handle, None)),
        StdioMode::Piped => {
            let (reader, writer) = inheritable_pipe()?;
            clear_inherit(&reader)?;
            Ok((writer, Some(File::from(reader))))
        }
    }
}

fn prepare_stderr(mode: StdioMode) -> Result<(OwnedHandle, Option<File>), ProcessError> {
    prepare_stdout(mode)
}

fn inheritable_pipe() -> Result<(OwnedHandle, OwnedHandle), ProcessError> {
    let attributes = inheritable_security_attributes();
    let mut reader = HANDLE::default();
    let mut writer = HANDLE::default();
    // SAFETY: output pointers reference writable HANDLE storage and the
    // initialized security descriptor remains alive for the synchronous call.
    unsafe {
        CreatePipe(
            &mut reader,
            &mut writer,
            Some(ptr::from_ref(&attributes)),
            0,
        )
    }
    .map_err(|source| ProcessError::windows("create anonymous pipe", source))?;
    Ok((owned_handle(reader), owned_handle(writer)))
}

fn open_null(access: u32) -> Result<OwnedHandle, ProcessError> {
    let attributes = inheritable_security_attributes();
    let name = [u16::from(b'N'), u16::from(b'U'), u16::from(b'L'), 0];
    // SAFETY: the NUL path is terminated, the security descriptor is valid for
    // the synchronous call, and the returned handle is immediately owned.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(ptr::from_ref(&attributes)),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|source| ProcessError::windows("open NUL device", source))?;
    Ok(owned_handle(handle))
}

fn inheritable_security_attributes() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES size fits u32"),
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: true.into(),
    }
}

fn clear_inherit(handle: &OwnedHandle) -> Result<(), ProcessError> {
    // SAFETY: the retained handle is live and only its inheritance flag is
    // changed; no ownership transfer occurs.
    unsafe { SetHandleInformation(as_handle(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|source| ProcessError::windows("clear handle inheritance", source))
}

fn verify_inheritable_handles(handles: &[HANDLE]) -> Result<(), ProcessError> {
    for (index, handle) in handles.iter().enumerate() {
        let mut flags = 0_u32;
        // SAFETY: each handle remains owned by PreparedStreams and the output
        // flag pointer references initialized writable storage.
        unsafe { GetHandleInformation(*handle, &mut flags) }
            .map_err(|source| ProcessError::windows("query handle inheritance", source))?;
        if flags & HANDLE_FLAG_INHERIT.0 == 0 {
            return Err(ProcessError::InvalidInput(format!(
                "child standard handle {index} is not inheritable"
            )));
        }
    }
    for (index, left) in handles.iter().enumerate() {
        for right in handles.iter().skip(index + 1) {
            // SAFETY: both values are verified live handles retained by
            // PreparedStreams for the complete comparison.
            if unsafe { CompareObjectHandles(*left, *right) }.as_bool() {
                return Err(ProcessError::InvalidInput(
                    "child standard handles must be distinct".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

struct AppContainerSid {
    sid: PSID,
    profile_name: Vec<u16>,
    deleted: bool,
}

impl AppContainerSid {
    fn create_ephemeral() -> Result<Self, ProcessError> {
        let sequence = APPCONTAINER_PROFILE_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        let mut profile_name =
            format!("rootlight.deep-adapter.{}.{}", std::process::id(), sequence)
                .encode_utf16()
                .collect::<Vec<_>>();
        profile_name.push(0);
        let name = PCWSTR(profile_name.as_ptr());
        // SAFETY: the unique name is NUL-terminated and alive for the complete
        // call. No capabilities are granted. The returned SID has one owner.
        let sid =
            unsafe { CreateAppContainerProfile(name, name, name, None) }.map_err(|source| {
                ProcessError::windows("create adapter AppContainer profile", source)
            })?;
        Ok(Self {
            sid,
            profile_name,
            deleted: false,
        })
    }

    fn delete(&mut self) -> Result<(), ProcessError> {
        if self.deleted {
            return Ok(());
        }
        // SAFETY: the profile name is NUL-terminated and names the profile
        // created by this object.
        unsafe { DeleteAppContainerProfile(PCWSTR(self.profile_name.as_ptr())) }.map_err(
            |source| ProcessError::windows("delete adapter AppContainer profile", source),
        )?;
        self.deleted = true;
        Ok(())
    }
}

impl Drop for AppContainerSid {
    fn drop(&mut self) {
        if !self.deleted {
            // SAFETY: the profile name is NUL-terminated and names the profile
            // created by this object. Drop cannot report best-effort cleanup.
            let _ = unsafe { DeleteAppContainerProfile(PCWSTR(self.profile_name.as_ptr())) };
        }
        // SAFETY: this SID was allocated by
        // CreateAppContainerProfile and has not been freed.
        unsafe {
            FreeSid(self.sid);
        }
    }
}

struct CurrentUserSid {
    _storage: Vec<usize>,
    sid: PSID,
}

impl CurrentUserSid {
    fn load() -> Result<Self, ProcessError> {
        let mut token = HANDLE::default();
        // SAFETY: GetCurrentProcess returns a process pseudo-handle valid for
        // this call. The output pointer requests a query-only token handle.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(|source| ProcessError::windows("open current process token", source))?;
        let token = owned_handle(token);
        let mut bytes = 0_u32;
        // SAFETY: this documented sizing call supplies no output buffer and a
        // valid writable size pointer.
        let sizing = unsafe {
            GetTokenInformation(
                as_handle(&token),
                TokenUser,
                None,
                0,
                ptr::from_mut(&mut bytes),
            )
        };
        if bytes == 0 {
            return sizing
                .map(|()| unreachable!("token-user sizing returned no size"))
                .map_err(|source| ProcessError::windows("size current user token", source));
        }
        let words = usize::try_from(bytes)
            .map_err(|_| ProcessError::InvalidInput("token-user size exceeds usize".to_owned()))?
            .div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        // SAFETY: storage is pointer-aligned and contains at least the byte
        // capacity requested by the kernel sizing call.
        unsafe {
            GetTokenInformation(
                as_handle(&token),
                TokenUser,
                Some(storage.as_mut_ptr().cast()),
                bytes,
                ptr::from_mut(&mut bytes),
            )
        }
        .map_err(|source| ProcessError::windows("read current user token", source))?;
        // SAFETY: successful TokenUser retrieval initialized a TOKEN_USER at
        // the aligned start of storage and its SID remains inside that buffer.
        let sid = unsafe { storage.as_ptr().cast::<TOKEN_USER>().read().User.Sid };
        // SAFETY: TokenUser guarantees a valid SID pointer for the lifetime of
        // the returned buffer.
        if unsafe { GetLengthSid(sid) } == 0 {
            return Err(ProcessError::InvalidInput(
                "current user token contains an empty SID".to_owned(),
            ));
        }
        Ok(Self {
            _storage: storage,
            sid,
        })
    }
}

struct DirectorySecurity {
    _acl: Vec<usize>,
    descriptor: SECURITY_DESCRIPTOR,
}

impl DirectorySecurity {
    fn appcontainer_read_only(
        current_user: PSID,
        app_container: PSID,
    ) -> Result<Self, ProcessError> {
        let current_sid_bytes = sid_length(current_user)?;
        let app_sid_bytes = sid_length(app_container)?;
        let required_bytes = size_of::<ACL>()
            .checked_add(current_sid_bytes)
            .and_then(|value| value.checked_add(app_sid_bytes))
            .and_then(|value| value.checked_add(256))
            .ok_or_else(|| ProcessError::InvalidInput("directory ACL size overflow".to_owned()))?;
        let words = required_bytes.div_ceil(size_of::<usize>());
        let mut acl = vec![0_usize; words];
        let acl_bytes = u32::try_from(acl.len().saturating_mul(size_of::<usize>()))
            .map_err(|_| ProcessError::InvalidInput("directory ACL exceeds u32".to_owned()))?;
        let acl_pointer = acl.as_mut_ptr().cast::<ACL>();
        // SAFETY: acl_pointer references aligned writable storage of exactly
        // acl_bytes and remains owned by DirectorySecurity.
        unsafe { InitializeAcl(acl_pointer, acl_bytes, ACL_REVISION) }
            .map_err(|source| ProcessError::windows("initialize adapter directory ACL", source))?;
        let inheritance = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
        // SAFETY: both SIDs remain live for this synchronous ACL construction,
        // and the ACL has capacity for both access entries.
        unsafe {
            AddAccessAllowedAceEx(
                acl_pointer,
                ACL_REVISION,
                inheritance,
                FILE_ALL_ACCESS.0,
                current_user,
            )
        }
        .map_err(|source| ProcessError::windows("grant owner adapter directory access", source))?;
        // SAFETY: the AppContainer SID remains live and receives only
        // read/execute access inherited by child directories and files.
        unsafe {
            AddAccessAllowedAceEx(
                acl_pointer,
                ACL_REVISION,
                inheritance,
                FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0,
                app_container,
            )
        }
        .map_err(|source| {
            ProcessError::windows("grant AppContainer read-only directory access", source)
        })?;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        // SAFETY: descriptor is initialized writable storage and uses the
        // current self-relative descriptor revision.
        unsafe {
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(ptr::from_mut(&mut descriptor).cast()),
                SECURITY_DESCRIPTOR_REVISION,
            )
        }
        .map_err(|source| {
            ProcessError::windows("initialize adapter directory security descriptor", source)
        })?;
        // SAFETY: the initialized ACL remains owned beside the descriptor for
        // every CreateDirectoryW call using this object.
        unsafe {
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(ptr::from_mut(&mut descriptor).cast()),
                true,
                Some(acl_pointer),
                false,
            )
        }
        .map_err(|source| {
            ProcessError::windows("set adapter directory security descriptor", source)
        })?;
        // SAFETY: the descriptor is initialized and remains writable here.
        // Protecting its DACL prevents the parent temp directory from adding
        // inherited principals beyond the two explicit access entries.
        unsafe {
            SetSecurityDescriptorControl(
                PSECURITY_DESCRIPTOR(ptr::from_mut(&mut descriptor).cast()),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
        }
        .map_err(|source| {
            ProcessError::windows("protect adapter directory security descriptor", source)
        })?;
        Ok(Self {
            _acl: acl,
            descriptor,
        })
    }

    fn attributes(&mut self) -> Result<SECURITY_ATTRIBUTES, ProcessError> {
        Ok(SECURITY_ATTRIBUTES {
            nLength: structure_size::<SECURITY_ATTRIBUTES>()?,
            lpSecurityDescriptor: ptr::from_mut(&mut self.descriptor).cast(),
            bInheritHandle: false.into(),
        })
    }
}

struct PrivateAdapterWorkspace {
    root: PathBuf,
    executable: PathBuf,
    current_directory: PathBuf,
    removed: bool,
}

impl PrivateAdapterWorkspace {
    fn create(
        source_executable: &Path,
        expected_digest: Option<AdapterExecutableDigest>,
        current_user: &CurrentUserSid,
        app_container: PSID,
    ) -> Result<Self, ProcessError> {
        let (mut source, source_bytes) = open_adapter_executable(source_executable)?;
        let mut security =
            DirectorySecurity::appcontainer_read_only(current_user.sid, app_container)?;
        let attributes = security.attributes()?;
        let mut random = [0_u8; 16];
        // SAFETY: the output slice is initialized writable storage and the
        // system-preferred provider requires no explicit algorithm handle.
        let random_status =
            unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if random_status.0 < 0 {
            return Err(ProcessError::Windows {
                operation: "generate adapter workspace name",
                code: random_status.0.cast_unsigned(),
                message: "BCryptGenRandom returned a failing NTSTATUS".to_owned(),
            });
        }
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = env::temp_dir().join(format!(".rootlight-adapter-{suffix}"));
        create_directory(&root, &attributes)?;
        let runtime = root.join("runtime");
        let current_directory = root.join("work");
        if let Err(error) = create_directory(&runtime, &attributes)
            .and_then(|()| create_directory(&current_directory, &attributes))
        {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        let executable = runtime.join("adapter.exe");
        let mut staged = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)
        {
            Ok(staged) => staged,
            Err(source) => {
                let _ = fs::remove_dir_all(&root);
                return Err(ProcessError::io("create staged adapter executable", source));
            }
        };
        if let Err(error) =
            copy_authenticated_executable(&mut source, source_bytes, expected_digest, &mut staged)
                .and_then(|_| {
                    staged.sync_all().map_err(|source| {
                        ProcessError::io("sync staged adapter executable", source)
                    })
                })
        {
            drop(staged);
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        drop(staged);
        if let Err(error) = set_file_attributes(&executable, FILE_ATTRIBUTE_READONLY) {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        Ok(Self {
            root,
            executable,
            current_directory,
            removed: false,
        })
    }

    fn remove(mut self) -> Result<(), ProcessError> {
        self.remove_inner()
    }

    fn clear_current_directory(&self) -> Result<(), ProcessError> {
        for entry in fs::read_dir(&self.current_directory)
            .map_err(|source| ProcessError::io("inspect adapter current directory", source))?
        {
            let path = entry
                .map_err(|source| ProcessError::io("inspect adapter current entry", source))?
                .path();
            if path.is_dir() {
                fs::remove_dir_all(&path)
                    .map_err(|source| ProcessError::io("remove adapter profile overlay", source))?;
            } else {
                fs::remove_file(&path)
                    .map_err(|source| ProcessError::io("remove adapter current file", source))?;
            }
        }
        if fs::read_dir(&self.current_directory)
            .map_err(|source| ProcessError::io("verify adapter current directory", source))?
            .next()
            .is_some()
        {
            return Err(ProcessError::InvalidInput(
                "adapter current directory is not empty".to_owned(),
            ));
        }
        Ok(())
    }

    fn remove_inner(&mut self) -> Result<(), ProcessError> {
        if self.removed {
            return Ok(());
        }
        if fs::metadata(&self.executable).is_ok() {
            set_file_attributes(&self.executable, FILE_ATTRIBUTE_NORMAL)?;
        }
        fs::remove_dir_all(&self.root)
            .map_err(|source| ProcessError::io("remove adapter workspace", source))?;
        self.removed = true;
        Ok(())
    }
}

fn open_adapter_executable(source: &Path) -> Result<(File, u64), ProcessError> {
    if !source.is_absolute() {
        return Err(ProcessError::InvalidInput(
            "the adapter executable path must be absolute".to_owned(),
        ));
    }
    let input = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(source)
        .map_err(|error| ProcessError::io("open adapter executable without reparse", error))?;
    let metadata = input
        .metadata()
        .map_err(|error| ProcessError::io("inspect opened adapter executable", error))?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(ProcessError::InvalidInput(
            "the adapter executable must be a regular non-reparse file".to_owned(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_ADAPTER_EXECUTABLE_BYTES {
        return Err(ProcessError::InvalidInput(
            "the adapter executable size is outside the hard limit".to_owned(),
        ));
    }
    Ok((input, metadata.len()))
}

impl Drop for PrivateAdapterWorkspace {
    fn drop(&mut self) {
        let _ = self.remove_inner();
    }
}

fn sid_length(sid: PSID) -> Result<usize, ProcessError> {
    // SAFETY: callers pass SIDs retained by their owning token/profile objects.
    let bytes = unsafe { GetLengthSid(sid) };
    if bytes == 0 {
        return Err(ProcessError::InvalidInput(
            "security principal contains an empty SID".to_owned(),
        ));
    }
    usize::try_from(bytes)
        .map_err(|_| ProcessError::InvalidInput("SID length exceeds usize".to_owned()))
}

fn create_directory(path: &Path, attributes: &SECURITY_ATTRIBUTES) -> Result<(), ProcessError> {
    let encoded = nul_terminated(path.as_os_str(), "adapter workspace path")?;
    // SAFETY: the path is NUL-terminated and the security attributes reference
    // a live descriptor and ACL for the complete synchronous call.
    unsafe { CreateDirectoryW(PCWSTR(encoded.as_ptr()), Some(ptr::from_ref(attributes))) }
        .map_err(|source| ProcessError::windows("create private adapter directory", source))
}

fn set_file_attributes(
    path: &Path,
    attributes: windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
) -> Result<(), ProcessError> {
    let encoded = nul_terminated(path.as_os_str(), "adapter runtime file")?;
    // SAFETY: the path is NUL-terminated and attributes are copied by value.
    unsafe { SetFileAttributesW(PCWSTR(encoded.as_ptr()), attributes) }
        .map_err(|source| ProcessError::windows("set adapter runtime attributes", source))
}

struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl AttributeList {
    fn with_handle_list(handles: &[HANDLE]) -> Result<Self, ProcessError> {
        let mut attributes = Self::new(1)?;
        attributes.update_raw(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            size_of_val(handles),
            "set process handle allowlist",
        )?;
        Ok(attributes)
    }

    fn with_adapter_profile(
        handles: &[HANDLE],
        security_capabilities: &SECURITY_CAPABILITIES,
        image_load_policy: &u64,
    ) -> Result<Self, ProcessError> {
        let mut attributes = Self::new(3)?;
        attributes.update_raw(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            size_of_val(handles),
            "set adapter process handle allowlist",
        )?;
        attributes.update_raw(
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            ptr::from_ref(security_capabilities).cast(),
            size_of::<SECURITY_CAPABILITIES>(),
            "set adapter AppContainer capabilities",
        )?;
        attributes.update_raw(
            PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize,
            ptr::from_ref(image_load_policy).cast(),
            size_of::<u64>(),
            "set adapter image-load mitigations",
        )?;
        Ok(attributes)
    }

    fn new(attribute_count: u32) -> Result<Self, ProcessError> {
        let mut bytes = 0_usize;
        // SAFETY: this documented sizing call supplies no destination and a
        // valid writable size pointer.
        let sizing = unsafe {
            InitializeProcThreadAttributeList(
                None,
                attribute_count,
                None,
                ptr::from_mut(&mut bytes),
            )
        };
        if bytes == 0 {
            return sizing
                .map(|()| unreachable!("attribute-list sizing returned no size"))
                .map_err(|source| ProcessError::windows("size process attribute list", source));
        }
        let words = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: storage is pointer-aligned, has at least the requested byte
        // capacity, and remains owned by AttributeList until deletion.
        unsafe {
            InitializeProcThreadAttributeList(
                Some(list),
                attribute_count,
                None,
                ptr::from_mut(&mut bytes),
            )
        }
        .map_err(|source| ProcessError::windows("initialize process attribute list", source))?;
        Ok(Self {
            list,
            _storage: storage,
        })
    }

    fn update_raw(
        &mut self,
        attribute: usize,
        value: *const c_void,
        bytes: usize,
        operation: &'static str,
    ) -> Result<(), ProcessError> {
        // SAFETY: the list is initialized with enough attribute slots. The
        // caller supplies initialized storage that outlives process creation
        // and the byte length exactly matches that storage.
        unsafe {
            UpdateProcThreadAttribute(self.list, 0, attribute, Some(value), bytes, None, None)
        }
        .map_err(|source| ProcessError::windows(operation, source))
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: this list was initialized exactly once, has not been deleted,
        // and its backing storage remains alive until this Drop returns.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

enum WaitResult {
    Running,
    Exited,
}

fn wait_result(process: &OwnedHandle, milliseconds: u32) -> Result<WaitResult, ProcessError> {
    // SAFETY: the retained handle names the exact child process and remains
    // alive for the complete wait.
    match unsafe { WaitForSingleObject(as_handle(process), milliseconds) } {
        WAIT_TIMEOUT => Ok(WaitResult::Running),
        WAIT_OBJECT_0 => Ok(WaitResult::Exited),
        WAIT_FAILED => Err(ProcessError::windows(
            "wait for child",
            windows::core::Error::from_win32(),
        )),
        other => Err(ProcessError::io(
            "wait for child",
            std::io::Error::other(format!("unexpected wait result {}", other.0)),
        )),
    }
}

fn exit_status(process: &OwnedHandle) -> Result<ExitStatus, ProcessError> {
    let mut exit_code = 0_u32;
    // SAFETY: the retained process handle is signaled and the output pointer
    // references initialized writable storage.
    unsafe { GetExitCodeProcess(as_handle(process), &mut exit_code) }
        .map_err(|source| ProcessError::windows("read child exit status", source))?;
    Ok(ExitStatus::from_raw(exit_code))
}

fn terminate_suspended(process: &OwnedHandle) -> Result<(), ProcessError> {
    // SAFETY: the retained process handle is the exact suspended child and no
    // child instruction has executed.
    unsafe { TerminateProcess(as_handle(process), PROCESS_TERMINATION_EXIT_CODE) }
        .map_err(|source| ProcessError::windows("terminate unassigned child", source))?;
    if !matches!(wait_result(process, 5_000)?, WaitResult::Exited) {
        return Err(ProcessError::Deadline {
            operation: "reap unassigned child",
        });
    }
    Ok(())
}

fn terminate_assigned_suspended(
    job: &KillOnCloseJob,
    process: &OwnedHandle,
) -> Result<(), ProcessError> {
    job.terminate(PROCESS_TERMINATION_EXIT_CODE)?;
    job.wait_empty(Instant::now() + Duration::from_secs(5))?;
    if !matches!(wait_result(process, 5_000)?, WaitResult::Exited) {
        return Err(ProcessError::Deadline {
            operation: "reap assigned suspended adapter",
        });
    }
    Ok(())
}

fn structure_size<T>() -> Result<u32, ProcessError> {
    u32::try_from(size_of::<T>())
        .map_err(|_| ProcessError::InvalidInput("a Win32 structure size exceeds u32".to_owned()))
}

fn as_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

fn owned_handle(handle: HANDLE) -> OwnedHandle {
    debug_assert!(!handle.is_invalid());
    // SAFETY: every caller passes one newly returned live Win32 handle and
    // transfers its sole Rust ownership into OwnedHandle exactly once.
    unsafe { OwnedHandle::from_raw_handle(handle.0 as RawHandle) }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        io::Read as _,
        os::windows::ffi::OsStringExt as _,
        path::{Path, PathBuf},
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn command_line_quotes_empty_whitespace_quotes_and_trailing_backslashes() {
        let command = ProcessCommand::new(PathBuf::from(r"C:\Program Files\rootlight.exe"))
            .arg("")
            .arg("plain")
            .arg("two words")
            .arg(r#"quote"inside"#)
            .arg(r"C:\tail\");
        let encoded = command_line(&command).expect("command line encodes");
        let rendered = String::from_utf16(&encoded[..encoded.len() - 1])
            .expect("command line is valid UTF-16");
        assert_eq!(
            rendered,
            r#""C:\Program Files\rootlight.exe" "" plain "two words" "quote\"inside" C:\tail\"#
        );
    }

    #[test]
    fn environment_overrides_are_case_insensitive_sorted_and_double_terminated() {
        let command = ProcessCommand::new(PathBuf::from(r"C:\rootlight.exe"))
            .env_clear()
            .env("zeta", "last")
            .env("Path", "first")
            .env("PATH", "replacement")
            .env("alpha", "start");
        let block = environment_block(&command).expect("environment block encodes");
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let entries = block[..block.len() - 1]
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(|entry| String::from_utf16(entry).expect("entry is UTF-16"))
            .collect::<Vec<_>>();
        assert_eq!(entries, ["alpha=start", "PATH=replacement", "zeta=last"]);
    }

    #[test]
    fn drive_current_directory_keys_are_never_inherited_as_environment() {
        assert!(is_drive_current_directory_key(OsStr::new("=C:")));
        assert!(is_drive_current_directory_key(OsStr::new("=D:")));
        assert!(!is_drive_current_directory_key(OsStr::new("PATH")));
        assert!(validate_environment_entry(OsStr::new("=C:"), OsStr::new(r"C:\work")).is_err());
    }

    #[test]
    fn embedded_nul_is_rejected_before_process_creation() {
        let command = ProcessCommand::new(PathBuf::from(r"C:\rootlight.exe"))
            .arg(OsString::from_wide(&[b'a'.into(), 0, b'b'.into()]));
        assert!(matches!(
            command_line(&command),
            Err(ProcessError::InvalidInput(_))
        ));
    }

    #[test]
    fn opened_executable_handle_blocks_path_replacement_until_copy_finishes() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter.exe");
        let original = b"authenticated executable";
        fs::write(&source, original).expect("fixture executable writes");
        let expected = AdapterExecutableDigest::from_bytes(*blake3::hash(original).as_bytes());
        let (mut opened, declared_bytes) =
            open_adapter_executable(&source).expect("fixture executable opens");

        assert!(
            OpenOptions::new().write(true).open(&source).is_err(),
            "opened executable allowed concurrent mutation"
        );
        assert!(
            fs::remove_file(&source).is_err(),
            "opened executable allowed path removal"
        );
        let mut staged = Vec::new();
        let observed =
            copy_authenticated_executable(&mut opened, declared_bytes, Some(expected), &mut staged)
                .expect("opened executable stages");

        assert_eq!(observed, expected);
        assert_eq!(staged, original);
    }

    #[test]
    fn executable_reparse_point_is_rejected_when_symlinks_are_available() {
        use std::os::windows::fs::symlink_file;

        let directory = tempfile::tempdir().expect("fixture directory opens");
        let target = directory.path().join("target.exe");
        let link = directory.path().join("adapter.exe");
        fs::write(&target, b"executable").expect("fixture target writes");
        if symlink_file(&target, &link).is_err() {
            return;
        }

        assert!(matches!(
            open_adapter_executable(&link),
            Err(ProcessError::InvalidInput(_))
        ));
    }

    #[test]
    fn device_and_sparse_oversize_executables_are_rejected() {
        assert!(open_adapter_executable(Path::new(r"\\.\NUL")).is_err());

        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter.exe");
        let file = File::create(&source).expect("fixture executable creates");
        file.set_len(MAX_ADAPTER_EXECUTABLE_BYTES + 1)
            .expect("fixture executable becomes sparse");
        drop(file);
        assert!(matches!(
            open_adapter_executable(&source),
            Err(ProcessError::InvalidInput(_))
        ));
    }

    #[test]
    fn staged_executable_must_match_negotiated_digest() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter.exe");
        fs::write(&source, b"unexpected executable").expect("fixture executable writes");
        let expected =
            AdapterExecutableDigest::from_bytes(*blake3::hash(b"negotiated executable").as_bytes());
        let mut app_container =
            AppContainerSid::create_ephemeral().expect("fixture AppContainer opens");
        let owner = CurrentUserSid::load().expect("current user SID reads");

        let result =
            PrivateAdapterWorkspace::create(&source, Some(expected), &owner, app_container.sid);
        app_container
            .delete()
            .expect("fixture AppContainer deletes");
        assert!(matches!(result, Err(ProcessError::InvalidInput(_))));
    }

    #[test]
    fn adapter_job_records_hard_process_memory_and_cpu_limits() {
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_millis(750))
            .expect("adapter limits validate");
        let job = KillOnCloseJob::with_adapter_limits(limits).expect("adapter Job Object opens");
        let observed = job.extended_limits().expect("adapter limits read back");
        assert_eq!(observed.BasicLimitInformation.ActiveProcessLimit, 1);
        assert_eq!(
            observed.BasicLimitInformation.PerJobUserTimeLimit,
            7_500_000
        );
        assert_eq!(observed.JobMemoryLimit, 256 * 1024 * 1024);
        assert!(
            observed
                .BasicLimitInformation
                .LimitFlags
                .contains(JOB_OBJECT_LIMIT_ACTIVE_PROCESS)
        );
        assert!(
            observed
                .BasicLimitInformation
                .LimitFlags
                .contains(JOB_OBJECT_LIMIT_JOB_MEMORY)
        );
        assert!(
            observed
                .BasicLimitInformation
                .LimitFlags
                .contains(JOB_OBJECT_LIMIT_JOB_TIME)
        );
        assert!(
            observed
                .BasicLimitInformation
                .LimitFlags
                .contains(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
        );
    }

    #[test]
    fn adapter_job_terminates_a_process_at_the_hard_memory_limit() {
        let limits = AdapterSandboxLimits::new(96 * 1024 * 1024, Duration::from_secs(5))
            .expect("adapter limits validate");
        let job = KillOnCloseJob::with_adapter_limits(limits).expect("adapter Job Object opens");
        let mut child = spawn_in_job(
            ProcessCommand::new(env::current_exe().expect("test executable path resolves"))
                .arg("--exact")
                .arg("platform::os::tests::adapter_memory_limit_helper")
                .arg("--nocapture")
                .env("ROOTLIGHT_SANDBOX_MEMORY_LIMIT_HELPER", "1"),
            &job,
        )
        .expect("memory-bound helper starts");
        let status = wait_for_exit_status(&mut child, Instant::now() + Duration::from_secs(5));
        assert!(!status.success());
        job.wait_empty(Instant::now() + Duration::from_secs(2))
            .expect("memory-limited Job Object empties");
    }

    #[test]
    fn adapter_memory_limit_helper() {
        if env::var_os("ROOTLIGHT_SANDBOX_MEMORY_LIMIT_HELPER").as_deref() != Some(OsStr::new("1"))
        {
            return;
        }
        let mut retained = Vec::new();
        loop {
            let mut page = vec![0_u8; 4 * 1024 * 1024];
            page.fill(0xa5);
            retained.push(std::hint::black_box(page));
        }
    }

    #[test]
    fn adapter_probe_applies_native_attributes_without_resuming_user_code() {
        let command = ProcessCommand::new(system_binary("cmd.exe"));
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(1))
            .expect("adapter limits validate");
        let report =
            probe_windows_adapter_isolation(command, limits).expect("adapter probe completes");
        assert!(
            report
                .control(crate::AdapterControl::NetworkEgress)
                .is_enforced()
        );
        assert!(
            report
                .control(crate::AdapterControl::ProcessCreation)
                .is_enforced()
        );
        assert!(!report.permits_deep_adapter());
    }

    #[test]
    fn isolated_adapter_executes_after_verified_native_setup() {
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(2))
            .expect("adapter limits validate");
        let (status, output, _) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_smoke_helper",
            &[],
            limits,
        );
        assert!(
            status.success(),
            "isolated helper failed: {}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn isolated_adapter_smoke_helper() {
        if !is_isolated_helper() {
            return;
        }
        assert_eq!(env::var("ROOTLIGHT_ADAPTER_ISOLATED").as_deref(), Ok("1"));
    }

    #[test]
    fn isolated_adapter_denies_network_endpoints() {
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("parent listener binds");
        listener
            .set_nonblocking(true)
            .expect("parent listener becomes nonblocking");
        let address = listener
            .local_addr()
            .expect("parent listener address reads");
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(2))
            .expect("adapter limits validate");
        let (status, stdout, stderr) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_network_helper",
            address.to_string().as_bytes(),
            limits,
        );
        assert!(
            status.success(),
            "network helper failed: stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        assert!(listener.accept().is_err());
    }

    #[test]
    fn isolated_adapter_network_helper() {
        if !is_isolated_helper() {
            return;
        }
        let mut address = String::new();
        std::io::stdin()
            .read_to_string(&mut address)
            .expect("network target reads");
        let address = address
            .parse::<std::net::SocketAddr>()
            .expect("network target parses");
        assert!(
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_err()
        );
    }

    #[test]
    fn isolated_adapter_denies_descendant_creation() {
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(2))
            .expect("adapter limits validate");
        let (status, stdout, stderr) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_child_process_helper",
            &[],
            limits,
        );
        let stdout = String::from_utf8_lossy(&stdout);
        assert!(stdout.contains("child-attempted"));
        assert!(
            status.success() || !stdout.contains("child-created"),
            "child creation escaped the Job limit: stdout={stdout} stderr={}",
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn isolated_adapter_child_process_helper() {
        if !is_isolated_helper() {
            return;
        }
        println!("child-attempted");
        let result = std::process::Command::new(system_binary("cmd.exe"))
            .args(["/D", "/Q", "/C", "exit", "0"])
            .status();
        match result {
            Err(_) => println!("child-denied"),
            Ok(status) if !status.success() => println!("child-denied"),
            Ok(_) => {
                println!("child-created");
                panic!("isolated adapter created a successful descendant");
            }
        }
    }

    #[test]
    fn isolated_adapter_denies_user_files_and_all_writes() {
        let mut foreign_profile =
            AppContainerSid::create_ephemeral().expect("foreign AppContainer profile opens");
        let owner = CurrentUserSid::load().expect("current user SID reads");
        let workspace = PrivateAdapterWorkspace::create(
            &env::current_exe().expect("test executable path resolves"),
            None,
            &owner,
            foreign_profile.sid,
        )
        .expect("foreign private workspace opens");
        let sentinel = workspace.root.join("ambient-secret.txt");
        fs::write(&sentinel, b"ambient secret").expect("ambient sentinel writes");
        let home_sentinel =
            Path::new(&env::var_os("USERPROFILE").expect("parent user profile path exists")).join(
                format!(".rootlight-adapter-denial-{}.txt", std::process::id()),
            );
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&home_sentinel)
            .and_then(|mut file| file.write_all(b"user home secret"))
            .expect("user-home sentinel writes");
        foreign_profile
            .delete()
            .expect("foreign AppContainer profile deletes");
        let input = format!(
            "{}\n{}",
            sentinel.to_string_lossy(),
            home_sentinel.to_string_lossy()
        );
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(2))
            .expect("adapter limits validate");
        let (status, stdout, stderr) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_filesystem_helper",
            input.as_bytes(),
            limits,
        );
        let succeeded = status.success();
        fs::remove_file(&home_sentinel).expect("user-home sentinel removes");
        workspace.remove().expect("foreign workspace removes");
        assert!(
            succeeded,
            "filesystem helper failed: stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn isolated_adapter_filesystem_helper() {
        if !is_isolated_helper() {
            return;
        }
        let mut requested_path = String::new();
        std::io::stdin()
            .read_to_string(&mut requested_path)
            .expect("bounded test input reads");
        for path in requested_path.lines() {
            assert!(fs::read(path).is_err(), "unexpected access to {path}");
        }
        let current = env::current_dir().expect("controlled current directory resolves");
        assert!(fs::write(current.join("forbidden.tmp"), b"no").is_err());
        for key in ["APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "USERPROFILE"] {
            let path = PathBuf::from(env::var_os(key).expect("restricted path variable exists"));
            assert!(path.starts_with(&current));
            assert!(fs::write(path.join(format!("{key}.tmp")), b"no").is_err());
        }
    }

    #[test]
    fn isolated_adapter_current_directory_cannot_supply_a_dll() {
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(2))
            .expect("adapter limits validate");
        let (status, stdout, stderr) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_dll_helper",
            &[],
            limits,
        );
        assert!(
            status.success(),
            "DLL helper failed: stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn isolated_adapter_dll_helper() {
        if !is_isolated_helper() {
            return;
        }
        let current = env::current_dir().expect("controlled current directory resolves");
        let entries = fs::read_dir(&current)
            .expect("controlled current directory reads")
            .collect::<Result<Vec<_>, _>>()
            .expect("controlled current entries read");
        assert!(entries.is_empty());
        assert!(fs::write(current.join("rootlight-attack.dll"), b"not a DLL").is_err());
        let library = nul_terminated(OsStr::new("rootlight-attack.dll"), "test library")
            .expect("name encodes");
        // SAFETY: the test name is NUL-terminated. The expected failure returns
        // no module handle and therefore transfers no ownership.
        let loaded = unsafe {
            windows::Win32::System::LibraryLoader::LoadLibraryW(PCWSTR(library.as_ptr()))
        };
        assert!(loaded.is_err());
    }

    #[test]
    fn isolated_adapter_streams_fail_closed_at_their_byte_quotas() {
        let executable = env::current_exe().expect("test executable path resolves");
        let executable_bytes = fs::read(&executable).expect("test executable reads");
        let executable_digest =
            AdapterExecutableDigest::from_bytes(*blake3::hash(&executable_bytes).as_bytes());
        let command = AdapterProcessCommand::new(executable, 1, 64, 64 * 1024)
            .expect("adapter command validates")
            .expected_executable_digest(executable_digest)
            .arg("--exact")
            .expect("test selector is a literal")
            .arg("platform::os::tests::isolated_adapter_output_quota_helper")
            .expect("test name is a literal")
            .arg("--nocapture")
            .expect("test flag is a literal");
        let limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_secs(2))
            .expect("adapter limits validate");
        let mut adapter =
            crate::spawn_isolated_adapter(command, limits).expect("isolated adapter starts");
        let workspace_root = adapter.workspace_root().to_path_buf();
        let mut stdin = adapter.take_stdin().expect("adapter stdin is retained");
        assert!(stdin.write_all(b"too large").is_err());
        drop(stdin);
        let mut stdout = adapter.take_stdout().expect("adapter stdout is retained");
        assert_eq!(stdout.read(&mut []).expect("empty output read succeeds"), 0);
        let mut output = Vec::new();
        assert!(stdout.read_to_end(&mut output).is_err());
        adapter
            .terminate()
            .expect("quota-violating adapter terminates");
        adapter
            .wait_empty(Instant::now() + Duration::from_secs(2))
            .expect("quota-violating adapter Job Object empties");
        drop(stdout);
        drop(adapter);
        assert!(
            !workspace_root.exists(),
            "isolated adapter workspace survived exact child cleanup"
        );
    }

    #[test]
    fn isolated_adapter_output_quota_helper() {
        if !is_isolated_helper() {
            return;
        }
        println!("{}", "x".repeat(4 * 1024));
    }

    #[test]
    fn isolated_adapter_cpu_and_memory_limits_terminate_hostile_work() {
        let cpu_limits = AdapterSandboxLimits::new(256 * 1024 * 1024, Duration::from_millis(100))
            .expect("adapter CPU limits validate");
        let (cpu_status, _, _) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_cpu_helper",
            &[],
            cpu_limits,
        );
        assert!(!cpu_status.success());

        let memory_limits = AdapterSandboxLimits::new(96 * 1024 * 1024, Duration::from_secs(5))
            .expect("adapter memory limits validate");
        let (memory_status, _, _) = run_isolated_helper(
            "platform::os::tests::isolated_adapter_memory_helper",
            &[],
            memory_limits,
        );
        assert!(!memory_status.success());
    }

    #[test]
    fn isolated_adapter_cpu_helper() {
        if !is_isolated_helper() {
            return;
        }
        let mut value = 1_u64;
        loop {
            value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }

    #[test]
    fn isolated_adapter_memory_helper() {
        if !is_isolated_helper() {
            return;
        }
        let mut retained = Vec::new();
        loop {
            let mut page = vec![0_u8; 4 * 1024 * 1024];
            page.fill(0x5a);
            retained.push(std::hint::black_box(page));
        }
    }

    #[test]
    fn ephemeral_appcontainer_profile_cleanup_is_idempotent() {
        let mut profile = AppContainerSid::create_ephemeral().expect("AppContainer profile opens");
        profile.delete().expect("AppContainer profile deletes");
        assert!(profile.deleted);
        profile
            .delete()
            .expect("repeated profile cleanup remains successful");
    }

    #[test]
    fn explicit_handle_list_excludes_an_ambient_inheritable_writer() {
        let (reader, writer) = inheritable_pipe().expect("sentinel pipe opens");
        clear_inherit(&reader).expect("sentinel reader is parent-only");
        let mut reader = File::from(reader);
        let (sender, receiver) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut byte = [0_u8; 1];
            sender.send(reader.read(&mut byte)).ok();
        });

        let mut child = spawn(
            ProcessCommand::new(command_interpreter())
                .arg("/D")
                .arg("/Q")
                .arg("/C")
                .arg("set /p value=")
                .stdin(StdioMode::Piped),
        )
        .expect("blocking child starts");
        let input = child.take_stdin().expect("control stdin is retained");
        assert!(child.try_wait().expect("child status reads").is_none());
        drop(writer);
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("sentinel reaches EOF")
                .expect("sentinel read succeeds"),
            0
        );
        assert!(child.try_wait().expect("child remains live").is_none());
        drop(input);
        wait_for_exit(&mut child, Instant::now() + Duration::from_secs(2));
        reader_thread.join().expect("sentinel reader joins");
    }

    #[test]
    fn dropping_kill_on_close_job_terminates_its_exact_child() {
        let job = KillOnCloseJob::new().expect("Job Object opens");
        let mut child = spawn_in_job(
            ProcessCommand::new(command_interpreter())
                .arg("/D")
                .arg("/Q")
                .arg("/C")
                .arg("set /p value=")
                .stdin(StdioMode::Piped),
            &job,
        )
        .expect("contained child starts");
        let _input = child.take_stdin().expect("control stdin is retained");
        assert_eq!(job.active_processes().expect("accounting reads"), 1);
        drop(job);
        wait_for_exit(&mut child, Instant::now() + Duration::from_secs(2));
    }

    #[test]
    fn handed_off_job_stays_live_until_its_exact_child_exits() {
        let job = KillOnCloseJob::new().expect("Job Object opens");
        let mut child = spawn_in_job(
            ProcessCommand::new(command_interpreter())
                .arg("/D")
                .arg("/Q")
                .arg("/C")
                .arg("set /p value=")
                .stdin(StdioMode::Piped),
            &job,
        )
        .expect("contained child starts");
        let input = child.take_stdin().expect("control stdin is retained");

        job.handoff(&child)
            .expect("containment lease transfers to the exact child");
        assert!(
            child
                .try_wait()
                .expect("handed-off child status reads")
                .is_none()
        );

        drop(input);
        wait_for_exit(&mut child, Instant::now() + Duration::from_secs(2));
    }

    #[test]
    fn suspended_assignment_contains_a_descendant_started_immediately() {
        let job = KillOnCloseJob::new().expect("Job Object opens");
        let mut child = spawn_in_job(
            ProcessCommand::new(env::current_exe().expect("test executable path resolves"))
                .arg("--exact")
                .arg("platform::os::tests::job_helper_spawns_descendant")
                .arg("--nocapture")
                .env("ROOTLIGHT_SANDBOX_JOB_HELPER", "1"),
            &job,
        )
        .expect("contained helper starts");
        wait_for_exit(&mut child, Instant::now() + Duration::from_secs(2));
        wait_for_descendant(&job, Instant::now() + Duration::from_secs(2));
        job.terminate(PROCESS_TERMINATION_EXIT_CODE)
            .expect("contained tree terminates");
        job.wait_empty(Instant::now() + Duration::from_secs(2))
            .expect("contained tree empties");
    }

    #[test]
    fn job_helper_spawns_descendant() {
        if env::var_os("ROOTLIGHT_SANDBOX_JOB_HELPER").as_deref() != Some(OsStr::new("1")) {
            return;
        }
        let descendant = std::process::Command::new(system_binary("ping.exe"))
            .args(["-n", "30", "127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("helper descendant starts");
        // The parent test owns the enclosing Job Object and proves that this
        // deliberately detached fixture cannot outlive that exact tree.
        std::mem::forget(descendant);
    }

    fn command_interpreter() -> PathBuf {
        system_binary("cmd.exe")
    }

    fn system_binary(name: &str) -> PathBuf {
        Path::new(&env::var_os("SystemRoot").expect("Windows system root exists"))
            .join("System32")
            .join(name)
    }

    fn wait_for_exit(child: &mut ChildProcess, deadline: Instant) {
        let _ = wait_for_exit_status(child, deadline);
    }

    fn wait_for_exit_status(child: &mut ChildProcess, deadline: Instant) -> ExitStatus {
        loop {
            if let Some(status) = child.try_wait().expect("child status reads") {
                return status;
            }
            if Instant::now() >= deadline {
                child.terminate().expect("timed-out child terminates");
                panic!("child did not exit before the deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_descendant(job: &KillOnCloseJob, deadline: Instant) {
        loop {
            let accounting = job.accounting().expect("Job Object accounting reads");
            if accounting.TotalProcesses >= 2 && accounting.ActiveProcesses >= 1 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected a live descendant, observed {} total and {} active processes",
                accounting.TotalProcesses,
                accounting.ActiveProcesses
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_isolated_exit(child: &mut IsolatedAdapterProcess, deadline: Instant) -> ExitStatus {
        loop {
            if let Some(status) = child.try_wait().expect("isolated child status reads") {
                return status;
            }
            if Instant::now() >= deadline {
                child.terminate().expect("timed-out adapter terminates");
                panic!("isolated adapter did not exit before the deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn run_isolated_helper(
        test_name: &str,
        input: &[u8],
        limits: AdapterSandboxLimits,
    ) -> (ExitStatus, Vec<u8>, Vec<u8>) {
        let command = AdapterProcessCommand::new(
            env::current_exe().expect("test executable path resolves"),
            input.len().max(1),
            64 * 1024,
            64 * 1024,
        )
        .expect("adapter command validates")
        .arg("--exact")
        .expect("test selector is a literal")
        .arg(test_name)
        .expect("test name is a literal")
        .arg("--nocapture")
        .expect("test flag is a literal");
        let (mut adapter, report) =
            spawn_windows_isolated_adapter(command, limits).expect("isolated adapter starts");
        assert!(report.permits_deep_adapter());
        let mut stdin = adapter.take_stdin().expect("adapter stdin is retained");
        stdin
            .write_all(input)
            .expect("bounded adapter input writes");
        stdin.flush().expect("bounded adapter input flushes");
        drop(stdin);
        let mut stdout = adapter.take_stdout().expect("adapter stdout is retained");
        let mut stderr = adapter.take_stderr().expect("adapter stderr is retained");
        let mut output = Vec::new();
        let mut diagnostic = Vec::new();
        stdout
            .read_to_end(&mut output)
            .expect("bounded adapter output reads");
        stderr
            .read_to_end(&mut diagnostic)
            .expect("bounded adapter diagnostics read");
        let status = wait_for_isolated_exit(&mut adapter, Instant::now() + Duration::from_secs(8));
        adapter
            .wait_empty(Instant::now() + Duration::from_secs(2))
            .expect("isolated adapter Job Object empties");
        (status, output, diagnostic)
    }

    fn is_isolated_helper() -> bool {
        env::var_os("ROOTLIGHT_ADAPTER_ISOLATED").as_deref() == Some(OsStr::new("1"))
    }
}
