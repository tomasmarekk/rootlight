//! R scanner snapshots retain complete raw-string and nesting context.
//! The isolated FFI tests reject hostile frames without executing R code.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: Signatures match the pinned scanner; Scanner owns its native allocation.
unsafe extern "C" {
    fn tree_sitter_r_external_scanner_create() -> *mut c_void;
    fn tree_sitter_r_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_r_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_r_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_r::LANGUAGE.into();
        // SAFETY: The constructor has no preconditions; this owner frees its allocation once.
        Self(NonNull::new(unsafe { tree_sitter_r_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: Every advertised byte is live; zero length permits a null reset buffer.
        unsafe {
            tree_sitter_r_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                c_uint::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The live scanner receives the ABI's writable 1024-byte buffer.
        let length = unsafe {
            tree_sitter_r_external_scanner_serialize(
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
        // SAFETY: The unique owner frees its live allocation through its original allocator.
        unsafe { tree_sitter_r_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn frame(raw: [u8; 3], scopes: &[u8]) -> Vec<u8> {
    let mut bytes = raw.to_vec();
    // Scanner snapshots are process-local and preserve the upstream native unsigned layout.
    bytes.extend_from_slice(&c_uint::try_from(scopes.len()).unwrap().to_ne_bytes());
    bytes.extend_from_slice(scopes);
    bytes
}

#[test]
fn r_complete_context_round_trips_without_consuming_or_truncating_state() {
    let mut scanner = Scanner::new();
    let maximum = 1024 - 3 - size_of::<c_uint>();
    for length in [0, 1, 255, 256, maximum] {
        let scopes: Vec<_> = (0..length)
            .map(|index| u8::try_from(index % 4 + 1).unwrap())
            .collect();
        for raw in [
            [0, 0, 0],
            [b')', 0, b'"'],
            [b']', 255, b'\''],
            [b'}', 3, b'"'],
        ] {
            let expected = frame(raw, &scopes);
            scanner.restore(&expected);
            assert_eq!(scanner.bytes(), expected);
            assert_eq!(scanner.bytes(), expected);
        }
    }
}

#[test]
fn r_invalid_snapshot_fields_reset_all_prior_context() {
    let mut scanner = Scanner::new();
    let good = frame([b')', 2, b'"'], &[1, 2, 3, 4]);
    let empty = frame([0, 0, 0], &[]);
    let mut invalid: Vec<_> = (0..good.len()).map(|end| good[..end].to_vec()).collect();
    invalid.extend([
        frame([b'(', 2, b'"'], &[1]),
        frame([b')', 2, b'x'], &[1]),
        frame([0, 1, 0], &[]),
        frame([0, 0, b'"'], &[]),
        frame([b')', 2, b'"'], &[0]),
        frame([b')', 2, b'"'], &[5]),
        frame([b')', 2, b'"'], &[255]),
        [good.as_slice(), &[0]].concat(),
    ]);
    for bytes in invalid {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), empty, "{bytes:?}");
    }
}

#[test]
fn r_oversized_and_hostile_snapshots_are_bounded_and_canonical() {
    let mut scanner = Scanner::new();
    let good = frame([b'}', 3, b'\''], &[1, 4]);
    let maximum = 1024 - 3 - size_of::<c_uint>();
    let oversized = frame([b')', 0, b'"'], &vec![1; maximum + 1]);
    scanner.restore(&oversized);
    assert_eq!(scanner.bytes(), frame([0, 0, 0], &[]));
    let mut seed = 1_u32;
    for length in 0..=1025 {
        for _ in 0..16 {
            let bytes: Vec<_> = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed.to_be_bytes()[0]
                })
                .collect();
            scanner.restore(&good);
            scanner.restore(&bytes);
            let once = scanner.bytes();
            assert_eq!(scanner.bytes(), once);
            scanner.restore(&once);
            assert_eq!(scanner.bytes(), once);
        }
    }
}
