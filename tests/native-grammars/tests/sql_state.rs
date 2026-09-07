//! SQL scanner snapshots must be complete, bounded and observational.
//! This isolated FFI suite exercises hostile frames without executing SQL.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: Signatures match the pinned scanner; Scanner uniquely owns its allocation.
unsafe extern "C" {
    fn tree_sitter_sql_external_scanner_create() -> *mut c_void;
    fn tree_sitter_sql_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_sql_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_sql_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_sequel::LANGUAGE.into();
        // SAFETY: The constructor takes no arguments; this owner destroys the result once.
        Self(NonNull::new(unsafe { tree_sitter_sql_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        // SAFETY: All advertised bytes are initialized and live for this call.
        // The decoder must validate the complete frame before retaining a tag.
        unsafe {
            tree_sitter_sql_external_scanner_deserialize(
                self.0.as_ptr(),
                bytes.as_ptr().cast(),
                u32::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The scanner is live and the ABI provides a writable 1024-byte buffer.
        let length = unsafe {
            tree_sitter_sql_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[1025], 0xa5);
        assert!(length <= 1024);
        guarded[1..1 + usize::try_from(length).unwrap()].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This unique owner frees its still-live allocation through its native allocator.
        unsafe { tree_sitter_sql_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn frame(tag: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend_from_slice(&u16::try_from(tag.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

#[test]
fn sql_serialization_does_not_consume_live_context() {
    let mut scanner = Scanner::new();
    scanner.restore(&frame("$outer$"));
    let first = scanner.bytes();
    assert!(!first.is_empty());
    assert_eq!(scanner.bytes(), first);
    assert_eq!(scanner.bytes(), first);
}

#[test]
fn sql_complete_tags_round_trip_through_the_abi_boundary() {
    let mut scanner = Scanner::new();
    for tag in [
        "$$".to_owned(),
        "$Name_42$".to_owned(),
        "$9.tag-$".to_owned(),
        "$πЖ$".to_owned(),
        format!("${}$", "a".repeat(1019)),
    ] {
        let expected = frame(&tag);
        scanner.restore(&expected);
        assert_eq!(scanner.bytes(), expected);
        assert_eq!(scanner.bytes(), expected);
    }
}

#[test]
fn sql_malformed_frames_erase_prior_context_without_partial_tags() {
    let mut scanner = Scanner::new();
    let good = frame("$outer$");
    let mut invalid = (0..good.len())
        .map(|end| good[..end].to_vec())
        .collect::<Vec<_>>();
    invalid.extend([
        vec![2, 2, 0, b'$', b'$'],
        vec![1; 1025],
        frame("$bad name$"),
        frame("$bad\ntag$"),
        frame("$a\0b$"),
        frame("tag"),
        frame("$a$b$"),
    ]);
    invalid.push([good.as_slice(), &[0]].concat());
    for encoded in [
        &[0xc0, 0x80][..],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0x80],
        &[0xe2, 0x82],
    ] {
        let tag = [&b"$"[..], encoded, b"$"].concat();
        let mut bytes = vec![1];
        bytes.extend_from_slice(&u16::try_from(tag.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&tag);
        invalid.push(bytes);
    }
    for bytes in invalid {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert!(scanner.bytes().is_empty(), "{bytes:?}");
    }
}

#[test]
fn sql_hostile_snapshots_are_bounded_and_deterministic() {
    let mut scanner = Scanner::new();
    let mut seed = 1_u32;
    for length in 0..=1025 {
        for _ in 0..16 {
            let bytes = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed.to_be_bytes()[0]
                })
                .collect::<Vec<_>>();
            scanner.restore(&frame("$previous$"));
            scanner.restore(&bytes);
            let snapshot = scanner.bytes();
            assert_eq!(scanner.bytes(), snapshot);
            scanner.restore(&snapshot);
            assert_eq!(scanner.bytes(), snapshot);
        }
    }
}
