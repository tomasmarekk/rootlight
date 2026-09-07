//! Pins the HTML scanner's complete serialized state and hostile-input boundary.
//! These FFI checks stay outside production crates' forbid-unsafe policy.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: Signatures match the pinned scanner.c; Scanner owns the opaque allocation.
unsafe extern "C" {
    fn tree_sitter_html_external_scanner_create() -> *mut c_void;
    fn tree_sitter_html_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_html_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_html_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_html::LANGUAGE.into();
        // SAFETY: The native constructor has no preconditions; this owner frees its allocation.
        Self(NonNull::new(unsafe { tree_sitter_html_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: Every advertised byte is live. The patched decoder validates the whole
        // frame before reading payloads or allocating; zero length permits a null buffer.
        unsafe {
            tree_sitter_html_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                u32::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5u8; 1026];
        // SAFETY: Only states fitting the 1024-byte ABI buffer can enter the scanner;
        // byte encoding does not require integer alignment at this offset.
        let count = unsafe {
            tree_sitter_html_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[1025], 0xa5);
        let count = usize::try_from(count).unwrap();
        assert!(count <= 1024);
        guarded[1..1 + count].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This sole owner destroys the still-live native allocation once.
        unsafe { tree_sitter_html_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn names_state(names: &[&[u8]]) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend_from_slice(&u16::try_from(names.len()).unwrap().to_le_bytes());
    for name in names {
        bytes.push(126); // CUSTOM in the pinned tag.h.
        bytes.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(name);
    }
    bytes
}

#[test]
fn html_full_state_round_trips_without_truncating_names_or_stack() {
    let mut scanner = Scanner::new();
    assert_eq!(scanner.bytes(), [1, 0, 0]);
    for length in [1, 127, 254, 255, 256, 700, 1018] {
        let name = vec![b'A'; length];
        let state = names_state(&[&name]);
        scanner.restore(&state);
        assert_eq!(scanner.bytes(), state);
    }
    for count in [1u16, 255, 256, 512, 1021] {
        let mut bytes = vec![1];
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend(std::iter::repeat_n(48, usize::from(count))); // DIV in tag.h.
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), bytes);
    }
    let bytes = names_state(&["X-ž".as_bytes(), "X-Ж".as_bytes(), "X-💡".as_bytes()]);
    scanner.restore(&bytes);
    assert_eq!(scanner.bytes(), bytes);
}

#[test]
fn html_malformed_state_discards_all_prior_context() {
    let mut scanner = Scanner::new();
    let good = names_state(&[b"X-FIRST", b"X-SECOND"]);
    let mut malformed = (0..good.len())
        .map(|end| good[..end].to_vec())
        .collect::<Vec<_>>();
    malformed.extend([
        vec![2, 0, 0],
        vec![1, 1, 0, 127],
        vec![1, 1, 0, 23],
        vec![1, 255, 255],
        vec![1, 1, 0, 126, 0, 0],
        vec![1, 1, 0, 126, 255, 255],
        vec![1; 1025],
    ]);
    malformed.push([good.as_slice(), &[0]].concat());
    for bytes in malformed {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), [1, 0, 0], "{bytes:?}");
    }
}

#[test]
fn html_adversarial_state_is_bounded_reusable_and_canonical() {
    let mut scanner = Scanner::new();
    for length in 0..=1025 {
        for byte in [0, 1, 23, 48, 126, 127, 255] {
            scanner.restore(&vec![byte; length]);
            let once = scanner.bytes();
            scanner.restore(&once);
            assert_eq!(scanner.bytes(), once);
        }
    }
    scanner.restore(&[]);
    assert_eq!(scanner.bytes(), [1, 0, 0]);
}
