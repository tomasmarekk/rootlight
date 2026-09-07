//! Exercises the patched Bash scanner through its bounded native state contract.
//! Parse fixtures are never executed; incremental trees are compared to fresh trees.

use std::ffi::{c_char, c_uint, c_void};
use std::ptr::NonNull;
use tree_sitter::{InputEdit, Node, Parser, Point};

// SAFETY: These signatures match the pinned scanner.c. State buffers stay live
// for each call, and the native constructor/destructor own the opaque allocation.
unsafe extern "C" {
    fn tree_sitter_bash_external_scanner_create() -> *mut c_void;
    fn tree_sitter_bash_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_bash_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_bash_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
        // SAFETY: The constructor has no preconditions; this owner alone destroys its result.
        Self(
            NonNull::new(unsafe { tree_sitter_bash_external_scanner_create() })
                .expect("native allocation"),
        )
    }

    fn restore(&mut self, bytes: &[u8]) {
        let length = u32::try_from(bytes.len()).expect("bounded test state");
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The patched deserializer validates lengths before every read or
        // allocation. The slice provides length initialized bytes, or null/zero reset.
        unsafe { tree_sitter_bash_external_scanner_deserialize(self.0.as_ptr(), pointer, length) };
    }

    fn serialized(&mut self) -> Vec<u8> {
        let mut buffer = [0_u8; 1024];
        // SAFETY: The native serializer checks the runtime's 1024-byte bound before
        // writing each entry. The live payload is exclusively owned during this call.
        let length = unsafe {
            tree_sitter_bash_external_scanner_serialize(self.0.as_ptr(), buffer.as_mut_ptr().cast())
        };
        buffer
            .get(..usize::try_from(length).expect("native length"))
            .expect("serializer bound")
            .to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: The sole owner destroys its live allocation once with the native allocator.
        unsafe { tree_sitter_bash_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn state(depth: u8, delimiters: &[&[u8]]) -> Vec<u8> {
    let mut bytes = vec![
        depth,
        1,
        1,
        u8::try_from(delimiters.len()).expect("bounded count"),
    ];
    for delimiter in delimiters {
        bytes.extend_from_slice(&[1, 0, 1]);
        bytes.extend_from_slice(
            &u32::try_from(delimiter.len())
                .expect("bounded delimiter")
                .to_ne_bytes(),
        );
        bytes.extend_from_slice(delimiter);
    }
    bytes
}

#[test]
fn reset_and_shorter_restores_erase_prior_context() {
    let mut scanner = Scanner::new();
    let empty = [0; 4];
    assert_eq!(scanner.serialized(), empty);
    for depth in 0..=255 {
        let one = state(depth, &[b"END\0"]);
        let two = state(depth, &[b"FIRST\0", b"LAST\0"]);
        for bytes in [&two[..], &one, &empty, &two, &[]] {
            scanner.restore(bytes);
            assert_eq!(
                scanner.serialized(),
                if bytes.is_empty() { &empty[..] } else { bytes }
            );
        }
    }
}

#[test]
fn group_markers_survive_restore_and_reuse() {
    let mut scanner = Scanner::new();
    for flags in [[0, 2, 0, 2], [1, 2, 1, 3], [0, 3, 0, 3]] {
        let mut bytes = state(128, &[b"A\0", b"B\0", b"C\0", b"D\0"]);
        for (index, flag) in flags.into_iter().enumerate() {
            bytes[4 + index * 9 + 1] = flag;
        }
        scanner.restore(&bytes);
        assert_eq!(scanner.serialized(), bytes);
        scanner.restore(&state(0, &[b"OTHER\0"]));
        scanner.restore(&bytes);
        assert_eq!(scanner.serialized(), bytes);
        scanner.restore(&[]);
        assert_eq!(scanner.serialized(), [0; 4]);
    }
}

#[test]
fn malformed_states_reset_without_retaining_partial_entries() {
    let valid = state(128, &[b"ONE\0", b"TWO\0"]);
    let mut scanner = Scanner::new();
    for length in 0..valid.len() {
        scanner.restore(&valid);
        scanner.restore(&valid[..length]);
        assert_eq!(scanner.serialized(), [0; 4], "truncation {length}");
    }
    let mut missing_terminator = valid.clone();
    *missing_terminator.last_mut().expect("nonempty") = 1;
    let mut huge_length = valid.clone();
    huge_length[7..11].copy_from_slice(&u32::MAX.to_ne_bytes());
    let mut trailing = valid.clone();
    trailing.push(0);
    for invalid in [missing_terminator, huge_length, trailing, vec![0; 1025]] {
        scanner.restore(&valid);
        scanner.restore(&invalid);
        assert_eq!(scanner.serialized(), [0; 4]);
    }
}

#[test]
fn serialized_state_keeps_the_existing_buffer_boundary() {
    let mut scanner = Scanner::new();
    for size in [0, 1, 127, 128, 255, 256, 1012, 1013] {
        let mut delimiter = vec![b'x'; size];
        if let Some(last) = delimiter.last_mut() {
            *last = 0;
        }
        let bytes = state(255, &[&delimiter]);
        scanner.restore(&bytes);
        if bytes.len() < 1024 {
            assert_eq!(scanner.serialized(), bytes);
        } else {
            assert!(scanner.serialized().is_empty());
        }
        scanner.restore(&[]);
        assert_eq!(scanner.serialized(), [0; 4]);
    }
}

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("compatible grammar");
    parser
}

fn assert_after_function(node: Node<'_>, source: &str) {
    let mut cursor = node.walk();
    let functions: Vec<_> = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "function_definition")
        .collect();
    assert_eq!(functions.len(), 1, "{}", node.to_sexp());
    let function = functions[0];
    assert_eq!(
        function.utf8_text(source.as_bytes()).expect("source bytes"),
        "after() { :; }"
    );
    assert_eq!(
        function
            .child_by_field_name("name")
            .expect("name")
            .utf8_text(source.as_bytes())
            .expect("name bytes"),
        "after"
    );
}

fn fixtures() -> Vec<String> {
    [
        "END",
        "ΤΕΛΟΣ",
        "終端",
        "𐐀",
        "é",
        "e\u{301}",
        "\u{7f}",
        "\u{80}",
        "\u{7ff}",
        "\u{800}",
        "\u{d7ff}",
        "\u{e000}",
        "\u{ffff}",
        "\u{10000}",
        "\u{10ffff}",
    ]
    .into_iter()
    .flat_map(|delimiter| {
        [
            delimiter.to_owned(),
            format!("'{delimiter}'"),
            format!("\"{delimiter}\""),
        ]
        .into_iter()
        .map(move |start| {
            format!("cat <<{start}\nbody\n{delimiter}suffix\n{delimiter}\nafter() {{ :; }}\n")
        })
    })
    .collect()
}

#[test]
fn unicode_and_prefix_lines_preserve_exact_following_definitions() {
    for source in fixtures() {
        let tree = parser().parse(&source, None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "{source:?}: {}",
            tree.root_node().to_sexp()
        );
        assert_after_function(tree.root_node(), &source);
        let mut cursor = tree.root_node().walk();
        let redirect = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "redirected_statement")
            .expect("redirect");
        assert!(redirect.end_byte() <= source.find("after()").expect("following function"));
        assert!(redirect.end_byte() > source.find("suffix").expect("prefix line"));
    }
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = source.get(..byte).expect("character boundary");
    Point {
        row: prefix.bytes().filter(|byte| *byte == b'\n').count(),
        column: prefix.rsplit('\n').next().expect("line").len(),
    }
}

fn fingerprint(node: Node<'_>) -> Vec<(String, usize, usize, bool, bool)> {
    let mut pending = vec![node];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_error(),
            node.is_missing(),
        ));
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

#[test]
fn incremental_edits_and_parser_reuse_match_fresh_trees() {
    let mut incremental_parser = parser();
    for original in fixtures() {
        let initial = incremental_parser
            .parse(&original, None)
            .expect("initial parse");
        for needle in ["body", "suffix", "cat", "after"] {
            let start = original.find(needle).expect("edit target");
            for replacement in ["", "changed", "\n", "𐐀"] {
                let end = start + needle.len();
                let mut changed = original.clone();
                changed.replace_range(start..end, replacement);
                let mut edited = initial.clone();
                edited.edit(&InputEdit {
                    start_byte: start,
                    old_end_byte: end,
                    new_end_byte: start + replacement.len(),
                    start_position: point(&original, start),
                    old_end_position: point(&original, end),
                    new_end_position: point(&changed, start + replacement.len()),
                });
                let incremental = incremental_parser
                    .parse(&changed, Some(&edited))
                    .expect("incremental parse");
                let fresh = parser().parse(&changed, None).expect("fresh parse");
                assert_eq!(
                    fingerprint(incremental.root_node()),
                    fingerprint(fresh.root_node()),
                    "edit {needle} -> {replacement:?}: {original:?}"
                );
            }
        }
        incremental_parser.reset();
        let restored = incremental_parser
            .parse(&original, None)
            .expect("reset parse");
        assert_eq!(
            fingerprint(restored.root_node()),
            fingerprint(initial.root_node())
        );
    }
}
