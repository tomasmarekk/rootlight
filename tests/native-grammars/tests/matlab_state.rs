//! MATLAB scanner frames must restore independently of previously scanned text.
//! Native ownership and hostile-frame checks stay outside production crates.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: These signatures match the pinned MATLAB scanner's exported C ABI.
unsafe extern "C" {
    fn tree_sitter_matlab_external_scanner_create() -> *mut c_void;
    fn tree_sitter_matlab_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_matlab_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_matlab_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_matlab::LANGUAGE.into();
        // SAFETY: The constructor needs no inputs; this unique owner frees the result once.
        Self(NonNull::new(unsafe { tree_sitter_matlab_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The scanner is live, and all advertised bytes are readable for this call.
        // The ABI permits a null buffer for the zero-length reset frame.
        unsafe {
            tree_sitter_matlab_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                c_uint::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> [u8; 5] {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The live scanner receives the ABI's writable 1024-byte buffer.
        let length = unsafe {
            tree_sitter_matlab_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..1025].as_mut_ptr().cast(),
            )
        };
        assert_eq!(length, 5);
        assert_eq!(guarded[0], 0xa5);
        assert!(guarded[6..].iter().all(|byte| *byte == 0xa5));
        guarded[1..6].try_into().unwrap()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: The unique owner frees its live allocation through its original allocator.
        unsafe { tree_sitter_matlab_external_scanner_destroy(self.0.as_ptr()) };
    }
}

#[test]
fn matlab_complete_frames_round_trip_without_consuming_state() {
    let mut scanner = Scanner::new();
    assert_eq!(scanner.bytes(), [0; 5]);
    for flags in 0_u8..16 {
        for delimiter in [0, b'\'', b'"'] {
            let frame = [
                flags & 1,
                (flags >> 1) & 1,
                (flags >> 2) & 1,
                delimiter,
                (flags >> 3) & 1,
            ];
            scanner.restore(&frame);
            assert_eq!(scanner.bytes(), frame);
            assert_eq!(scanner.bytes(), frame);
        }
    }
}

#[test]
fn matlab_empty_and_malformed_frames_reset_all_previous_context() {
    let mut scanner = Scanner::new();
    let good = [1, 1, 1, b'"', 1];
    let mut invalid: Vec<_> = (0..5).map(|end| good[..end].to_vec()).collect();
    invalid.extend([vec![0; 6], vec![0xff; 1024]]);
    for index in 0..5 {
        for byte in 0..=255 {
            let valid = if index == 3 {
                matches!(byte, 0 | b'\'' | b'"')
            } else {
                byte <= 1
            };
            if !valid {
                let mut frame = good;
                frame[index] = byte;
                invalid.push(frame.to_vec());
            }
        }
    }
    for frame in invalid {
        scanner.restore(&good);
        scanner.restore(&frame);
        assert_eq!(scanner.bytes(), [0; 5], "{frame:?}");
    }
}
