//! Scala scanner snapshot framing at the native serialization boundary.
//! Malformed frames must clear prior layout state instead of being truncated.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: These signatures match the pinned scanner's exported C functions.
unsafe extern "C" {
    fn tree_sitter_scala_external_scanner_create() -> *mut c_void;
    fn tree_sitter_scala_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_scala_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
    fn tree_sitter_scala_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_scala::LANGUAGE.into();
        // SAFETY: The constructor needs no input; this unique owner frees the allocation.
        Self(NonNull::new(unsafe { tree_sitter_scala_external_scanner_create() }).unwrap())
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The live scanner receives a writable ABI-sized 1024-byte buffer.
        let length = unsafe {
            tree_sitter_scala_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert!(length <= 1024);
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[1025], 0xa5);
        guarded[1..1 + usize::try_from(length).unwrap()].to_vec()
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The patched decoder validates framing before reading; all advertised bytes live.
        unsafe {
            tree_sitter_scala_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                c_uint::try_from(bytes.len()).unwrap(),
            );
        }
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This unique owner frees its live allocation through the original allocator.
        unsafe { tree_sitter_scala_external_scanner_destroy(self.0.as_ptr()) };
    }
}

#[test]
fn scala_partial_indent_word_resets_instead_of_accepting_a_prefix() {
    let mut scanner = Scanner::new();
    let empty = scanner.bytes();
    let frame = [2_i16, 1, 2, i16::from(b'x'), 0, 4];
    // The complete header is readable and aligned even in the upstream decoder.
    // Only the final indent word is cut short, isolating framing from memory faults.
    // SAFETY: All 12 backing bytes are live and int16-aligned; the decoder receives 11.
    unsafe {
        tree_sitter_scala_external_scanner_deserialize(
            scanner.0.as_ptr(),
            frame.as_ptr().cast(),
            11,
        );
    }
    assert_eq!(scanner.bytes(), empty);
}

fn frame(indent_count: usize) -> Vec<u8> {
    let mut bytes = vec![1, 7];
    for value in [65_536_u32, 2, u32::MAX, u32::from('😀')] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for index in 0..indent_count {
        let width = [0, 16_384, 32_768, 65_536, u32::MAX][index % 5];
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.push(u8::try_from(index % 2).unwrap());
    }
    bytes
}

#[test]
fn scala_complete_frames_round_trip_at_every_abi_length_without_mutating_state() {
    let mut scanner = Scanner::new();
    for words in 0..=201 {
        let expected = frame(words);
        let mut unaligned = vec![0xa5];
        unaligned.extend_from_slice(&expected);
        scanner.restore(&unaligned[1..]);
        assert_eq!(scanner.bytes(), expected);
        assert_eq!(scanner.bytes(), expected);
    }
}

#[test]
fn scala_invalid_lengths_clear_prior_context_before_any_word_reads() {
    let mut scanner = Scanner::new();
    let empty = scanner.bytes();
    let good = frame(12);
    for length in
        (0..=1025).filter(|length| *length < 18 || *length > 1024 || (length - 18) % 5 != 0)
    {
        scanner.restore(&good);
        assert_eq!(scanner.bytes(), good);
        scanner.restore(&vec![0xff; length]);
        assert_eq!(scanner.bytes(), empty, "length {length}");
    }
}

#[test]
fn scala_invalid_field_domains_never_restore_a_partial_layout() {
    let mut scanner = Scanner::new();
    let empty = scanner.bytes();
    let good = frame(3);
    let mut invalid = Vec::new();
    for (index, value) in [(0, 0), (0, 2), (1, 8), (6, 3), (22, 2), (27, 255)] {
        let mut bytes = good.clone();
        bytes[index] = value;
        invalid.push(bytes);
    }
    for character in [0xD800_u32, 0xDFFF, 0x11_0000, u32::MAX] {
        let mut bytes = good.clone();
        bytes[14..18].copy_from_slice(&character.to_le_bytes());
        invalid.push(bytes);
    }
    for absent in [1, 2] {
        let mut bytes = good.clone();
        bytes[1] &= !absent;
        invalid.push(bytes);
    }
    for bytes in invalid {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), empty, "{bytes:?}");
    }
}
