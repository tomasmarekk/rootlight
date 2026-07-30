//! Audited Darwin process-replacement boundary for hard memory enforcement.

#![allow(
    unsafe_code,
    reason = "Darwin exposes fatal physical-footprint spawn attributes only through C FFI"
)]

use std::{
    convert::Infallible,
    ffi::{CString, OsString, c_char, c_int, c_short},
    io,
    os::unix::ffi::OsStrExt as _,
    path::Path,
    ptr,
};

const MEBIBYTE: u64 = 1024 * 1024;
const POSIX_SPAWN_JETSAM_MEMLIMIT_ACTIVE_FATAL: c_short = 0x04;
const POSIX_SPAWN_JETSAM_MEMLIMIT_INACTIVE_FATAL: c_short = 0x08;
const JETSAM_PRIORITY_DEFAULT: c_int = -1;

#[link(name = "System")]
unsafe extern "C" {
    fn posix_spawnattr_setjetsam_ext(
        attributes: *mut libc::posix_spawnattr_t,
        flags: c_short,
        priority: c_int,
        memory_limit_active: c_int,
        memory_limit_inactive: c_int,
    ) -> c_int;
}

struct SpawnAttributes {
    raw: libc::posix_spawnattr_t,
}

impl SpawnAttributes {
    fn new() -> io::Result<Self> {
        let mut raw = ptr::null_mut();
        // SAFETY: `raw` points to writable storage for the opaque attribute
        // handle and is owned by the returned guard after successful init.
        status(unsafe { libc::posix_spawnattr_init(&mut raw) })?;
        Ok(Self { raw })
    }

    fn set_replace_process(&mut self) -> io::Result<()> {
        let flags = c_short::try_from(libc::POSIX_SPAWN_SETEXEC)
            .map_err(|_| io::Error::other("Darwin SETEXEC flag is not representable"))?;
        // SAFETY: the guard owns an initialized attribute handle, and the
        // platform header defines SETEXEC as a valid short spawn flag.
        status(unsafe { libc::posix_spawnattr_setflags(&mut self.raw, flags) })
    }

    fn set_fatal_footprint_limit(&mut self, memory_mebibytes: c_int) -> io::Result<()> {
        let flags =
            POSIX_SPAWN_JETSAM_MEMLIMIT_ACTIVE_FATAL | POSIX_SPAWN_JETSAM_MEMLIMIT_INACTIVE_FATAL;
        // SAFETY: the guard owns an initialized attribute handle. Both limits
        // are positive MiB values accepted by Apple's private spawn extension.
        status(unsafe {
            posix_spawnattr_setjetsam_ext(
                &mut self.raw,
                flags,
                JETSAM_PRIORITY_DEFAULT,
                memory_mebibytes,
                memory_mebibytes,
            )
        })
    }
}

impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        // SAFETY: this guard is the sole owner of an attribute handle that was
        // initialized successfully and has not otherwise been destroyed.
        let _ = unsafe { libc::posix_spawnattr_destroy(&mut self.raw) };
    }
}

pub(super) fn replace_process_with_memory_limit(
    program: &Path,
    arguments: &[OsString],
    memory_bytes: u64,
) -> io::Result<Infallible> {
    let memory_mebibytes = memory_limit_mebibytes(memory_bytes)?;
    let program_c = CString::new(program.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Darwin replacement program contains an embedded NUL",
        )
    })?;
    let argument_c = std::iter::once(program.as_os_str())
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|argument| {
            CString::new(argument.as_bytes()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Darwin replacement argument contains an embedded NUL",
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut argument_pointers = argument_c
        .iter()
        .map(|argument| argument.as_ptr().cast_mut())
        .collect::<Vec<*mut c_char>>();
    argument_pointers.push(ptr::null_mut());
    let environment = [ptr::null_mut::<c_char>()];

    let mut attributes = SpawnAttributes::new()?;
    attributes.set_replace_process()?;
    attributes.set_fatal_footprint_limit(memory_mebibytes)?;
    // SAFETY: every argv pointer refers to a live `CString`, both pointer
    // arrays are null-terminated, the environment is intentionally empty, and
    // the initialized attributes remain alive for the complete call.
    let result = unsafe {
        libc::posix_spawn(
            ptr::null_mut(),
            program_c.as_ptr(),
            ptr::null(),
            &attributes.raw,
            argument_pointers.as_ptr(),
            environment.as_ptr(),
        )
    };
    status(result)?;
    Err(io::Error::other(
        "Darwin SETEXEC spawn returned without replacing the process",
    ))
}

fn memory_limit_mebibytes(memory_bytes: u64) -> io::Result<c_int> {
    let memory_mebibytes = memory_bytes / MEBIBYTE;
    if memory_mebibytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "adapter memory limit is below one mebibyte",
        ));
    }
    c_int::try_from(memory_mebibytes)
        .map_err(|_| io::Error::other("adapter memory limit is not representable on Darwin"))
}

fn status(code: c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_limit_rounds_down_without_exceeding_the_requested_bytes() {
        assert_eq!(
            memory_limit_mebibytes(128 * MEBIBYTE + MEBIBYTE - 1).expect("limit is representable"),
            128
        );
    }

    #[test]
    fn footprint_limit_rejects_sub_mebibyte_and_unrepresentable_values() {
        assert_eq!(
            memory_limit_mebibytes(MEBIBYTE - 1)
                .expect_err("sub-MiB limit is rejected")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(memory_limit_mebibytes(u64::MAX).is_err());
    }
}
