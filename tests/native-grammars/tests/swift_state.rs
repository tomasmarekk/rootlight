//! Checks the compiled Swift scanner's complete serialized-state boundary.
//! Opaque native allocations never cross into the production Rust API.

use std::ffi::{c_char, c_uint, c_void};
use std::ptr::NonNull;

// SAFETY: These signatures match the pinned scanner.c. Its serializer writes
// four bytes; the matching native constructor/destructor own the allocation.
unsafe extern "C" {
    fn tree_sitter_swift_external_scanner_create() -> *mut c_void;
    fn tree_sitter_swift_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_swift_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_swift_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _language: tree_sitter::Language = tree_sitter_swift::LANGUAGE.into();
        // SAFETY: The native constructor has no preconditions and returns an owned allocation.
        Self(
            NonNull::new(unsafe { tree_sitter_swift_external_scanner_create() })
                .expect("scanner allocation succeeds"),
        )
    }

    fn restore(&mut self, bytes: &[u8; 4], length: u32) {
        assert!(length <= 4);
        let buffer = if length == 0 {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The payload is exclusively owned and live. The nonempty buffer
        // has at least length initialized bytes; null/zero matches parser reset.
        unsafe { tree_sitter_swift_external_scanner_deserialize(self.0.as_ptr(), buffer, length) };
    }

    fn state(&mut self) -> u32 {
        let mut bytes = [0_u8; 4];
        // SAFETY: The payload is live and exclusively owned; the audited native
        // serializer writes exactly four bytes to this writable allocation.
        let written = unsafe {
            tree_sitter_swift_external_scanner_serialize(self.0.as_ptr(), bytes.as_mut_ptr().cast())
        };
        assert_eq!(written, 4);
        u32::from_be_bytes(bytes)
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This sole owner frees the constructor's allocation exactly once
        // using the matching native destructor, never Rust's allocator.
        unsafe { tree_sitter_swift_external_scanner_destroy(self.0.as_ptr()) };
    }
}

#[test]
fn every_unsigned_byte_position_survives_serialization() {
    let mut scanner = Scanner::new();
    assert_eq!(scanner.state(), 0);
    for position in 0..4 {
        for byte in 0..=255_u8 {
            let mut input = [0_u8; 4];
            input[position] = byte;
            scanner.restore(&input, 4);
            assert_eq!(
                scanner.state(),
                u32::from_be_bytes(input),
                "position {position}, byte {byte}"
            );
        }
    }
}

#[test]
fn empty_and_truncated_state_erase_prior_raw_string_context() {
    let mut scanner = Scanner::new();
    for length in 0..4 {
        scanner.restore(&42_u32.to_be_bytes(), 4);
        assert_eq!(scanner.state(), 42);
        scanner.restore(&[0; 4], length);
        assert_eq!(scanner.state(), 0, "state length {length}");
    }
}

#[test]
fn mixed_high_bytes_preserve_the_full_counter() {
    let mut scanner = Scanner::new();
    for value in [
        0,
        u32::MAX,
        0x8080_8080,
        0xff00_80ff,
        0x007f_80ff,
        0x8000_0001,
    ] {
        scanner.restore(&value.to_be_bytes(), 4);
        assert_eq!(scanner.state(), value);
    }
}
