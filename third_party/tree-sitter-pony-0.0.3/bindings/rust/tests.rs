//! Verify real parser/scanner integration and complete state restoration.
//! Integer-boundary serialization controls do not claim source reachability.

use super::LANGUAGE;
use std::{
    ffi::{c_char, c_int, c_void},
    ptr::NonNull,
};

unsafe extern "C" {
    fn tree_sitter_pony_external_scanner_create() -> *mut c_void;
    fn tree_sitter_pony_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_pony_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> u32;
    fn tree_sitter_pony_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: u32,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        // SAFETY: The native factory allocates one scanner, owned by this guard.
        Self(NonNull::new(unsafe { tree_sitter_pony_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        // SAFETY: The guard owns a live scanner; bytes has exactly the declared
        // length and is either empty or the complete native integer state.
        unsafe {
            tree_sitter_pony_external_scanner_deserialize(
                self.0.as_ptr(),
                bytes.as_ptr().cast(),
                u32::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn state(&mut self) -> Vec<u8> {
        let mut bytes: [c_char; 1024] = [0; 1024];
        // SAFETY: The live scanner serializes into the runtime's full 1024-byte
        // output capacity; its retained state contains one native integer.
        let length = unsafe {
            tree_sitter_pony_external_scanner_serialize(self.0.as_ptr(), bytes.as_mut_ptr())
        };
        let length = usize::try_from(length).unwrap();
        assert!(length <= bytes.len());
        bytes[..length]
            .iter()
            .map(|byte| byte.to_ne_bytes()[0])
            .collect()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This guard uniquely owns the pointer returned by create.
        unsafe { tree_sitter_pony_external_scanner_destroy(self.0.as_ptr()) };
    }
}

#[test]
fn complete_native_integer_roundtrips_and_empty_restore_reset_state() {
    let mut scanner = Scanner::new();
    let counters: [c_int; 2] = [7, c_int::MAX];
    for counter in counters {
        let state = counter.to_ne_bytes();
        scanner.restore(&state);
        assert_eq!(scanner.state(), state);
        let mut restored = Scanner::new();
        restored.restore(&scanner.state());
        assert_eq!(restored.state(), state);
        restored.restore(&[]);
        let zero: c_int = 0;
        assert_eq!(restored.state(), zero.to_ne_bytes());
    }
}

#[test]
fn scanner_forms_incremental_parse_and_fresh_tree_agree() {
    let source = concat!(
        "\"\"\"Module λ🙂\ntext with double \"\" quotes\n\"\"\"\"\"\"\"\n",
        "class Sample\n  fun ref connected(): String =>\n",
        "    /* outer /* nested */ comment */\n    \"\"\"left\nmiddle\nright\"\"\"\n",
        "  fun ref second(): Array[U8] =>\n    Array[U8]\n"
    );
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&LANGUAGE.into()).unwrap();
    let mut tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let start = source.find("middle").unwrap();
    let row = source[..start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let column = start - source[..start].rfind('\n').unwrap() - 1;
    tree.edit(&tree_sitter::InputEdit {
        start_byte: start,
        old_end_byte: start + 6,
        new_end_byte: start + 7,
        start_position: tree_sitter::Point::new(row, column),
        old_end_position: tree_sitter::Point::new(row, column + 6),
        new_end_position: tree_sitter::Point::new(row, column + 7),
    });
    let changed = source.replace("middle", "changed");
    let incremental = parser.parse(&changed, Some(&tree)).unwrap();
    let fresh = parser.parse(&changed, None).unwrap();
    assert!(!incremental.root_node().has_error());
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert_eq!(incremental.root_node().end_byte(), changed.len());
}
