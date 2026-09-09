//! Checks the candidate scanner's complete private snapshot contract through FFI.
//! Guard bytes and canonical replay expose truncation without relying on crashes.

use std::{
    ffi::{c_char, c_void},
    ptr::NonNull,
};

// SAFETY: These signatures match the pinned scanner exports; Scanner owns the allocation.
unsafe extern "C" {
    fn tree_sitter_astro_external_scanner_create() -> *mut c_void;
    fn tree_sitter_astro_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_astro_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> u32;
    fn tree_sitter_astro_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: u32,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_astro_next::LANGUAGE.into();
        // SAFETY: The constructor has no preconditions; this owner frees the allocation.
        Self(NonNull::new(unsafe { tree_sitter_astro_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: All advertised bytes are initialized and live for this call.
        // The decoder validates the complete frame; zero length allows a null pointer.
        unsafe {
            tree_sitter_astro_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                u32::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The live scanner admits only states fitting the 1024-byte ABI
        // buffer, which is initialized here. Encoding requires no integer alignment.
        let count = unsafe {
            tree_sitter_astro_external_scanner_serialize(
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
        // SAFETY: The sole owner releases the still-live allocation exactly once.
        unsafe { tree_sitter_astro_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn names_state(names: &[&[u8]]) -> Vec<u8> {
    let mut bytes = vec![2];
    bytes.extend_from_slice(&u16::try_from(names.len()).unwrap().to_le_bytes());
    for name in names {
        bytes.push(126); // CUSTOM in the pinned candidate tag.h.
        bytes.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(name);
    }
    bytes
}

#[test]
fn complete_names_and_maximum_stack_round_trip() {
    let mut scanner = Scanner::new();
    assert_eq!(scanner.bytes(), [2, 0, 0]);
    for length in [1, 254, 255, 256, 700, 1018] {
        let name = vec![b'X'; length];
        let bytes = names_state(&[&name]);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), bytes);
    }
    for count in [1_u16, 255, 256, 512, 1021] {
        let mut bytes = vec![2];
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend(std::iter::repeat_n(46, usize::from(count))); // DIV in tag.h.
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), bytes);
    }
    let bytes = names_state(&["X-é".as_bytes(), "X-ǩ".as_bytes(), "X-💡".as_bytes()]);
    scanner.restore(&bytes);
    assert_eq!(scanner.bytes(), bytes);
}

#[test]
fn malformed_frames_reset_all_prior_context() {
    let mut scanner = Scanner::new();
    let good = names_state(&[b"Card", b"UI.Detail"]);
    let mut malformed: Vec<_> = (0..good.len()).map(|end| good[..end].to_vec()).collect();
    malformed.extend([
        vec![1, 0, 0],
        vec![2, 1, 0, 127],
        vec![2, 1, 0, 21],
        vec![2, 255, 255],
        vec![2, 1, 0, 126, 0, 0],
        vec![2, 1, 0, 126, 255, 255],
        vec![1; 1025],
    ]);
    malformed.push([good.as_slice(), &[0]].concat());
    for bytes in malformed {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), [2, 0, 0], "{bytes:?}");
    }
}

#[test]
fn adversarial_state_is_bounded_and_canonical() {
    let mut scanner = Scanner::new();
    for length in 0..=1025 {
        for byte in [0, 1, 2, 21, 46, 124, 125, 126, 127, 255] {
            scanner.restore(&vec![byte; length]);
            let once = scanner.bytes();
            scanner.restore(&once);
            assert_eq!(scanner.bytes(), once);
        }
    }
}

fn interpolation_state(parens: &[u8], braces: &[u8]) -> Vec<u8> {
    let length = 7 + parens.len() + braces.len();
    let mut bytes = vec![2, 1, 0, 124]; // INTERPOLATION in the candidate tag.h.
    bytes.extend_from_slice(&u16::try_from(length).unwrap().to_le_bytes());
    bytes.extend_from_slice(&[15, 3, 2]);
    bytes.extend_from_slice(&u16::try_from(parens.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(braces.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(parens);
    bytes.extend_from_slice(braces);
    bytes
}

#[test]
fn interpolation_context_round_trips_at_full_native_capacity() {
    let mut scanner = Scanner::new();
    for count in [0, 1, 255, 1000, 1011] {
        let bytes = interpolation_state(&vec![1; count], &[]);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), bytes);
        let bytes = interpolation_state(&[], &vec![1; count]);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), bytes);
    }
    let mixed = interpolation_state(&[0, 1, 3, 4], &[0, 1]);
    scanner.restore(&mixed);
    assert_eq!(scanner.bytes(), mixed);
}

#[test]
fn malformed_interpolation_payload_never_restores_partial_context() {
    let mut scanner = Scanner::new();
    let good = interpolation_state(&[0, 1, 3, 4], &[0, 1]);
    let mut malformed: Vec<_> = (0..good.len()).map(|end| good[..end].to_vec()).collect();
    for (position, value) in [
        (6, 16),
        (7, 2),
        (8, 3),
        (9, 255),
        (11, 255),
        (13, 2),
        (17, 2),
    ] {
        let mut bytes = good.clone();
        bytes[position] = value;
        malformed.push(bytes);
    }
    for bytes in malformed {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), [2, 0, 0], "{bytes:?}");
    }
}
