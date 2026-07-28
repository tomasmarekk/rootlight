//! Windows process creation with explicit handle inheritance and tree ownership.

#![allow(
    unsafe_code,
    reason = "Win32 process, pipe, attribute-list, and Job Object APIs have no safe stable-Rust wrapper"
)]

use std::{
    cmp::Ordering,
    env,
    ffi::{OsStr, c_void},
    fs::File,
    io::{Read, Write},
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
        process::ExitStatusExt as _,
    },
    process::ExitStatus,
    ptr,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            CompareObjectHandles, GENERIC_READ, GENERIC_WRITE, GetHandleInformation, HANDLE,
            HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
                QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
            },
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
                DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
                InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

use crate::{ProcessCommand, ProcessError, StdioMode};

const PROCESS_TERMINATION_EXIT_CODE: u32 = 1;
const FAILED_THREAD_RESUME: u32 = u32::MAX;
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

static SPAWN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
        env::vars_os().collect::<Vec<_>>()
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

struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl AttributeList {
    fn with_handle_list(handles: &[HANDLE]) -> Result<Self, ProcessError> {
        let mut bytes = 0_usize;
        // SAFETY: this documented sizing call supplies no destination and a
        // valid writable size pointer.
        let sizing =
            unsafe { InitializeProcThreadAttributeList(None, 1, None, ptr::from_mut(&mut bytes)) };
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
            InitializeProcThreadAttributeList(Some(list), 1, None, ptr::from_mut(&mut bytes))
        }
        .map_err(|source| ProcessError::windows("initialize process attribute list", source))?;
        // SAFETY: the initialized list and exact handle slice remain alive
        // through process creation, and cbSize matches the complete slice.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(|source| ProcessError::windows("set process handle allowlist", source))?;
        Ok(Self {
            list,
            _storage: storage,
        })
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
    fn embedded_nul_is_rejected_before_process_creation() {
        let command = ProcessCommand::new(PathBuf::from(r"C:\rootlight.exe"))
            .arg(OsString::from_wide(&[b'a'.into(), 0, b'b'.into()]));
        assert!(matches!(
            command_line(&command),
            Err(ProcessError::InvalidInput(_))
        ));
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
        loop {
            if child.try_wait().expect("child status reads").is_some() {
                return;
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
}
