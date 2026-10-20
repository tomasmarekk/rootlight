//! Exercise Crystal syntax, exact source names and bounded scanner restoration.
//! Native parsing establishes written syntax rather than expanded or inferred meaning.

use std::{
    ffi::{c_char, c_void},
    ptr::NonNull,
};
use tree_sitter::{InputEdit, Parser, Point};
const SOURCE: &str = include_str!("../../tests/fixtures/null_log_handler.cr");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&super::LANGUAGE.into()).unwrap();
    parser
}

#[test]
fn original_class_and_method_native_leaves_retain_authored_offsets() {
    assert_eq!(SOURCE.len(), 313);
    let mut parser = parser();
    let tree = parser.parse(SOURCE, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut cursor = tree.walk();
    let mut found = 0;
    loop {
        let node = cursor.node();
        for (start, end, name, parent) in [
            (144, 158, "NullLogHandler", "class_def"),
            (191, 195, "call", "method_def"),
            (271, 276, "write", "method_def"),
        ] {
            if node.start_byte() == start && node.end_byte() == end && node.is_named() {
                assert_eq!(node.utf8_text(SOURCE.as_bytes()).unwrap(), name);
                assert_eq!(node.parent().unwrap().kind(), parent);
                assert_eq!(
                    node.parent().unwrap().child_by_field_name("name"),
                    Some(node)
                );
                found += 1;
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                assert_eq!(found, 3);
                return;
            }
        }
    }
}

#[test]
fn short_setter_rhs_and_documented_operators_preserve_incremental_trees() {
    let source = "nodes.try(&.prev=(node))\nnodes.try(&.next=(nxt))\nnodes.map(&.+(2))\nnodes.map(&.[0])\nnodes.map(&.==(1))\n";
    let mut parser = parser();
    let mut tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let suffix = "nodes.map(&.method(1))\n";
    let updated = format!("{source}{suffix}");
    let end = tree.root_node().end_position();
    tree.edit(&InputEdit {
        start_byte: source.len(),
        old_end_byte: source.len(),
        new_end_byte: updated.len(),
        start_position: end,
        old_end_position: end,
        new_end_position: Point::new(end.row + 1, 0),
    });
    let incremental = parser.parse(&updated, Some(&tree)).unwrap();
    let fresh = parser.parse(&updated, None).unwrap();
    assert!(!fresh.root_node().has_error());
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert_eq!(incremental.root_node().end_byte(), updated.len());
    for invalid in [
        "nodes.try(&.prev=)\n",
        "nodes.try(&.=(node))\n",
        "nodes.try(&.prev=(node)\n",
    ] {
        assert!(
            parser.parse(invalid, None).unwrap().root_node().has_error(),
            "{invalid}"
        );
    }
}

unsafe extern "C" {
    fn tree_sitter_crystal_external_scanner_create() -> *mut c_void;
    fn tree_sitter_crystal_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_crystal_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> u32;
    fn tree_sitter_crystal_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: u32,
    );
}
struct Scanner(NonNull<c_void>);
impl Scanner {
    fn new() -> Self {
        // SAFETY: The factory creates one scanner uniquely owned by this guard.
        Self(NonNull::new(unsafe { tree_sitter_crystal_external_scanner_create() }).unwrap())
    }
    fn state(&mut self) -> Vec<u8> {
        let mut buffer: [c_char; 1024] = [0; 1024];
        // SAFETY: A live scanner writes into the runtime's full output capacity.
        let size = unsafe {
            tree_sitter_crystal_external_scanner_serialize(self.0.as_ptr(), buffer.as_mut_ptr())
        };
        let size = usize::try_from(size).unwrap();
        assert!(size <= buffer.len());
        buffer[..size]
            .iter()
            .map(|byte| byte.to_ne_bytes()[0])
            .collect()
    }
    fn restore(&mut self, bytes: &[u8]) {
        // SAFETY: The selected scanner validates the exact borrowed extent before
        // copying its state. The guard retains exclusive ownership of payload.
        unsafe {
            tree_sitter_crystal_external_scanner_deserialize(
                self.0.as_ptr(),
                bytes.as_ptr().cast(),
                u32::try_from(bytes.len()).unwrap(),
            )
        };
    }
}
impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: Only this guard owns the factory pointer, which is still live.
        unsafe { tree_sitter_crystal_external_scanner_destroy(self.0.as_ptr()) };
    }
}
#[test]
fn truncated_or_invalid_empty_state_resets_without_partial_restoration() {
    let mut scanner = Scanner::new();
    let initial = scanner.state();
    assert_eq!(initial.len(), 6);
    scanner.restore(&initial);
    assert_eq!(scanner.state(), initial);
    for length in 0..initial.len() {
        scanner.restore(&initial[..length]);
        assert_eq!(scanner.state(), initial);
    }
    for index in 0..4 {
        let mut bad = initial.clone();
        bad[index] = 2;
        scanner.restore(&bad);
        assert_eq!(scanner.state(), initial);
    }
    let mut trailing = initial.clone();
    trailing.push(0);
    scanner.restore(&trailing);
    assert_eq!(scanner.state(), initial);
    let mut count = initial.clone();
    count[4] = 17;
    scanner.restore(&count);
    assert_eq!(scanner.state(), initial);
    count = initial.clone();
    count[5] = 17;
    scanner.restore(&count);
    assert_eq!(scanner.state(), initial);
}
