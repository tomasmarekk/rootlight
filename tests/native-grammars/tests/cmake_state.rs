//! CMake scanner framing cannot retain stale or uninitialized bracket state.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: These signatures match the reviewed scanner. The constructor and
// destructor own one opaque allocation; every supplied buffer outlives its call.
unsafe extern "C" {
    fn tree_sitter_cmake_external_scanner_create() -> *mut c_void;
    fn tree_sitter_cmake_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_cmake_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_cmake_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_cmake::LANGUAGE_CMAKE.into();
        // SAFETY: This owner alone frees the native constructor's live allocation.
        Self(NonNull::new(unsafe { tree_sitter_cmake_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let length = u32::try_from(bytes.len()).unwrap();
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The scanner checks exact six-byte length before reading and
        // resets all state first. The slice supplies the complete advertised input.
        unsafe { tree_sitter_cmake_external_scanner_deserialize(self.0.as_ptr(), pointer, length) };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The scanner writes exactly six bytes to this live writable
        // buffer. Offset one deliberately supplies no integer alignment promise.
        let length = unsafe {
            tree_sitter_cmake_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert_eq!(length, 6);
        assert_eq!(guarded[0], 0xa5);
        assert!(guarded[7..].iter().all(|byte| *byte == 0xa5));
        guarded[1..7].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: The sole owner destroys the still-live allocation exactly once.
        unsafe { tree_sitter_cmake_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn state(token: u8, width: u32) -> Vec<u8> {
    let mut result = vec![1, token];
    result.extend_from_slice(&width.to_le_bytes());
    result
}

#[test]
fn initialized_and_reset_state_is_deterministic() {
    for _ in 0..64 {
        let mut scanner = Scanner::new();
        assert_eq!(scanner.bytes(), state(7, 0));
        scanner.restore(&state(4, 100));
        assert_eq!(scanner.bytes(), state(4, 100));
        scanner.restore(&[]);
        assert_eq!(scanner.bytes(), state(7, 0));
    }
}

#[test]
fn complete_frames_validate_token_width_and_inactive_state() {
    let mut scanner = Scanner::new();
    for token in [0, 1, 3, 4] {
        for width in [0, 1, 128, u32::MAX - 4] {
            let bytes = state(token, width);
            scanner.restore(&bytes);
            assert_eq!(scanner.bytes(), bytes);
        }
    }
    for token in 0..=255 {
        for width in [0, 1, u32::MAX - 3, u32::MAX] {
            scanner.restore(&state(1, 3));
            let bytes = state(token, width);
            scanner.restore(&bytes);
            let valid = (matches!(token, 0 | 1 | 3 | 4) && width <= u32::MAX - 4)
                || token == 7 && width == 0;
            assert_eq!(scanner.bytes(), if valid { bytes } else { state(7, 0) });
        }
    }
}

#[test]
fn arbitrary_lengths_and_versions_clear_previous_state() {
    let mut scanner = Scanner::new();
    for length in 0..=1025 {
        scanner.restore(&state(0, 9));
        let bytes = vec![0x5a; length];
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), state(7, 0));
    }
    for version in 0..=255 {
        let mut bytes = state(1, 3);
        bytes[0] = version;
        scanner.restore(&bytes);
        assert_eq!(
            scanner.bytes(),
            if version == 1 { bytes } else { state(7, 0) }
        );
    }
}
