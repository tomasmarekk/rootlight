//! Guards the scanner's bounded, version-free five-byte continuation state encoding.
//! Owning FFI handles are freed on unwinding as well as successful test completion.

use std::ffi::{c_char, c_uint, c_void};

// SAFETY: These signatures match the candidate scanner.c C exports.
unsafe extern "C" {
    fn tree_sitter_groovy_external_scanner_create() -> *mut c_void;
    fn tree_sitter_groovy_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_groovy_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_groovy_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(*mut c_void);
impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = dekobon_tree_sitter_groovy::LANGUAGE.into();
        // SAFETY: The input-free C constructor returns a uniquely owned allocation.
        let value = unsafe { tree_sitter_groovy_external_scanner_create() };
        assert!(!value.is_null());
        Self(value)
    }
    fn decode(&mut self, bytes: &[u8]) {
        // SAFETY: The handle is live and uniquely borrowed; all declared input bytes live.
        unsafe {
            tree_sitter_groovy_external_scanner_deserialize(
                self.0,
                bytes.as_ptr().cast(),
                c_uint::try_from(bytes.len()).unwrap(),
            )
        };
    }
    fn encode(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: A live unique handle receives exactly the ABI's 1024-byte writable region.
        let count = unsafe {
            tree_sitter_groovy_external_scanner_serialize(
                self.0,
                guarded[1..1025].as_mut_ptr().cast(),
            )
        } as usize;
        assert!(count == 0 || count == 5);
        assert_eq!(guarded[0], 0xa5);
        assert!(guarded[count + 1..].iter().all(|b| *b == 0xa5));
        guarded[1..count + 1].to_vec()
    }
}
impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns the constructor's handle and destroys it exactly once.
        unsafe { tree_sitter_groovy_external_scanner_destroy(self.0) };
    }
}

#[test]
fn scanner_roundtrips_both_decisions_and_canonicalizes_empty_state() {
    let mut scanner = Scanner::new();
    assert!(scanner.encode().is_empty());
    for decision in [1, 2] {
        for remaining in [1_u32, 127, 65536, u32::MAX] {
            let mut bytes = remaining.to_le_bytes().to_vec();
            bytes.push(decision);
            scanner.decode(&bytes);
            assert_eq!(scanner.encode(), bytes);
        }
        scanner.decode(&[0, 0, 0, 0, decision]);
        assert!(scanner.encode().is_empty());
    }
}

#[test]
fn scanner_rejects_truncated_and_invalid_serialized_decisions() {
    let mut scanner = Scanner::new();
    for length in 0..=1024 {
        scanner.decode(&[7, 0, 0, 0, 1]);
        scanner.decode(&vec![0xff; length]);
        assert!(scanner.encode().is_empty(), "length={length}");
    }
}
