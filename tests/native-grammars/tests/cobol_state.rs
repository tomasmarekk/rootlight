//! Stateless scanner snapshots and parser reuse retain exact source identities.
//! These native checks do not establish resolver semantics or product support.

use std::{
    ffi::{c_char, c_uint, c_void},
    ops::ControlFlow,
};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

// SAFETY: All declarations match the compiled stateless scanner profile exports.
unsafe extern "C" {
    fn tree_sitter_COBOL_external_scanner_create() -> *mut c_void;
    fn tree_sitter_COBOL_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_COBOL_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_COBOL_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
    fn tree_sitter_COBOL_free_external_scanner_create() -> *mut c_void;
    fn tree_sitter_COBOL_free_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_COBOL_free_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_COBOL_free_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

fn parser(free: bool) -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(
            &if free {
                tree_sitter_cobol::FREE
            } else {
                tree_sitter_cobol::FIXED
            }
            .into(),
        )
        .unwrap();
    parser
}

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn bounded_parse(parser: &mut Parser, source: &[u8], old: Option<&Tree>) -> Tree {
    let mut steps = 0_usize;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        steps = steps.saturating_add(1);
        if steps > 2_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            old,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("native fixture progress exceeded watchdog")
}

fn signature(tree: &Tree, source: &[u8]) -> Vec<(String, usize, usize, bool, bool, bool)> {
    let mut pending: Vec<Node<'_>> = vec![tree.root_node()];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_named(),
            node.is_error(),
            node.is_missing(),
        ));
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result.sort();
    result
}

#[test]
fn both_stateless_profiles_leave_serialization_storage_untouched() {
    let _ = parser(false);
    let _ = parser(true);
    for free in [false, true] {
        // SAFETY: Constructors require no arguments and return their stateless null payload.
        let payload = unsafe {
            if free {
                tree_sitter_COBOL_free_external_scanner_create()
            } else {
                tree_sitter_COBOL_external_scanner_create()
            }
        };
        assert!(payload.is_null());
        for length in 0..=1024 {
            let input = [0x5a_u8; 1024];
            let mut guarded = [0xa5_u8; 1026];
            // SAFETY: Both no-op decoders accept the null payload and declared live input;
            // serialization receives an ABI-sized region with two external canaries.
            let written = unsafe {
                if free {
                    tree_sitter_COBOL_free_external_scanner_deserialize(
                        payload,
                        input.as_ptr().cast(),
                        length,
                    );
                    tree_sitter_COBOL_free_external_scanner_serialize(
                        payload,
                        guarded[1..1025].as_mut_ptr().cast(),
                    )
                } else {
                    tree_sitter_COBOL_external_scanner_deserialize(
                        payload,
                        input.as_ptr().cast(),
                        length,
                    );
                    tree_sitter_COBOL_external_scanner_serialize(
                        payload,
                        guarded[1..1025].as_mut_ptr().cast(),
                    )
                }
            };
            assert_eq!(written, 0);
            assert_eq!(guarded, [0xa5; 1026]);
        }
        // SAFETY: Each original constructor's stateless payload is destroyed once.
        unsafe {
            if free {
                tree_sitter_COBOL_free_external_scanner_destroy(payload)
            } else {
                tree_sitter_COBOL_external_scanner_destroy(payload)
            }
        }
    }
}

#[test]
fn every_byte_damage_and_restore_matches_a_fresh_tree() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            let prefix = if free { "" } else { "000100 " };
            let source = format!("{prefix}identification division.{ending}{prefix}program-id. Primary.{ending}{prefix}procedure division.{ending}{prefix}display \"λ😀\".{ending}{prefix}stop run.{ending}").into_bytes();
            let mut reused = parser(free);
            let original = bounded_parse(&mut reused, &source, None);
            assert!(!original.root_node().has_error());
            let original_signature = signature(&original, &source);
            for offset in 0..source.len() {
                let mut damaged = source.clone();
                damaged[offset] = if source[offset] == b'?' { 0xff } else { b'?' };
                let mut old = original.clone();
                old.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&source, offset),
                    old_end_position: point(&source, offset + 1),
                    new_end_position: point(&damaged, offset + 1),
                });
                let mut updated = bounded_parse(&mut reused, &damaged, Some(&old));
                let fresh = bounded_parse(&mut parser(free), &damaged, None);
                assert_eq!(
                    signature(&updated, &damaged),
                    signature(&fresh, &damaged),
                    "damage at {offset}"
                );
                updated.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&damaged, offset),
                    old_end_position: point(&damaged, offset + 1),
                    new_end_position: point(&source, offset + 1),
                });
                let restored = bounded_parse(&mut reused, &source, Some(&updated));
                assert_eq!(
                    signature(&restored, &source),
                    original_signature,
                    "restore at {offset}"
                );
            }
        }
    }
}

#[test]
fn cancellation_then_reset_preserves_the_next_exact_document() {
    for free in [false, true] {
        let prefix = if free { "" } else { "000100 " };
        let text = format!(
            "{prefix}identification division.\n{prefix}program-id. Primary.\n{prefix}procedure division.\n{}{prefix}stop run.\n",
            format!("{prefix}display 1.\n").repeat(10_000)
        );
        let mut reused = parser(free);
        let mut called = false;
        let mut cancel = |_: &tree_sitter::ParseState| {
            called = true;
            ControlFlow::Break(())
        };
        let tree = reused.parse_with_options(
            &mut |offset, _| text.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut cancel)),
        );
        assert!(called && tree.is_none());
        reused.reset();
        let next = format!("{prefix}identification division.\n{prefix}program-id. Secondary.\n");
        let actual = bounded_parse(&mut reused, next.as_bytes(), None);
        let fresh = bounded_parse(&mut parser(free), next.as_bytes(), None);
        assert!(!actual.root_node().has_error());
        assert_eq!(
            signature(&actual, next.as_bytes()),
            signature(&fresh, next.as_bytes())
        );
    }
}
