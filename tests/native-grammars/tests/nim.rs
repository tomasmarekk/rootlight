//! Exercise the retained Nim tables and scanner through the genuine shared runtime.
//! Malformed serialized state controls are separate from ordinary parser input.

use std::ffi::c_void;
use tree_sitter::{InputEdit, Parser, Point};

const SOURCE: &str =
    include_str!("../../../crates/rootlight-adapter-treesitter/tests/fixtures/nim/patterns.nim");

unsafe extern "C" {
    fn tree_sitter_nim_external_scanner_create() -> *mut c_void;
    fn tree_sitter_nim_external_scanner_destroy(state: *mut c_void);
    fn tree_sitter_nim_external_scanner_serialize(state: *mut c_void, buffer: *mut u8) -> u32;
    fn tree_sitter_nim_external_scanner_deserialize(
        state: *mut c_void,
        buffer: *const u8,
        length: u32,
    );
}

#[test]
fn retained_nim_parser_accepts_original_source_and_incremental_reparse() {
    let mut parser = Parser::new();
    let language = tree_sitter_nim::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    assert_eq!(language.abi_version(), 14);
    let mut original = parser.parse(SOURCE, None).unwrap();
    assert!(
        !original.root_node().has_error(),
        "{}",
        original.root_node().to_sexp()
    );
    assert_eq!(original.root_node().kind(), "source_file");
    let replacement = SOURCE.replacen("NodeType", "NodeKind", 1);
    let start = SOURCE.find("NodeType").unwrap();
    let prefix = &SOURCE[..start];
    let point = Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix.rsplit('\n').next().unwrap().len(),
    );
    let end = Point::new(point.row, point.column + "NodeType".len());
    original.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 8,
        new_end_byte: start + 8,
        start_position: point,
        old_end_position: end,
        new_end_position: end,
    });
    // Equal-length token replacement retains all physical offsets.
    let incremental = parser.parse(&replacement, Some(&original)).unwrap();
    let fresh = parser.parse(&replacement, None).unwrap();
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert!(!fresh.root_node().has_error());
    for script in [
        include_str!("../../../crates/rootlight-adapter-treesitter/tests/fixtures/nim/config.nims"),
        include_str!(
            "../../../crates/rootlight-adapter-treesitter/tests/fixtures/nim/jester.nimble"
        ),
    ] {
        let tree = parser.parse(script, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
    let malformed = parser
        .parse("proc broken( =\n type Bad = object\n", None)
        .unwrap();
    assert!(malformed.root_node().has_error());
}

#[test]
fn scanner_rejects_foreign_state_without_writing_past_the_runtime_buffer() {
    // SAFETY: the factory owns a live scanner until the matching destroy below.
    let state = unsafe { tree_sitter_nim_external_scanner_create() };
    assert!(!state.is_null());
    let invalid = [1u8; 1025];
    let mut output = [0xa5u8; 2048];
    // SAFETY: state is live and both buffers cover the declared input/output spans.
    unsafe {
        tree_sitter_nim_external_scanner_deserialize(state, invalid.as_ptr(), 1025);
        assert_eq!(
            tree_sitter_nim_external_scanner_serialize(state, output.as_mut_ptr()),
            0
        );
    }
    assert!(output.iter().all(|byte| *byte == 0xa5));
    let valid: Vec<u8> = (0..=254).collect();
    // SAFETY: the valid input contains 255 increasing byte indents, and output
    // reserves twice the runtime's maximum 1024-byte serialization capacity.
    unsafe {
        tree_sitter_nim_external_scanner_deserialize(state, valid.as_ptr(), 255);
        assert_eq!(
            tree_sitter_nim_external_scanner_serialize(state, output.as_mut_ptr()),
            255
        );
    }
    assert_eq!(&output[..255], valid.as_slice());
    assert!(output[255..].iter().all(|byte| *byte == 0xa5));
    // SAFETY: zero length is the scanner ABI's reset operation; no input is read.
    unsafe {
        tree_sitter_nim_external_scanner_deserialize(state, std::ptr::null(), 0);
        assert_eq!(
            tree_sitter_nim_external_scanner_serialize(state, output.as_mut_ptr()),
            0
        );
        tree_sitter_nim_external_scanner_destroy(state);
    }
}
