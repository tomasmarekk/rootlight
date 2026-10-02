//! Pinned Haskell layout syntax and scanner snapshots, without executing Haskell.
//! Snapshot fixtures preserve the scanner's process-local native wire layout.

use std::{
    ffi::{c_char, c_uint, c_void},
    mem::{align_of, offset_of, size_of},
    ops::ControlFlow,
    ptr::NonNull,
};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

// SAFETY: These signatures match the pinned C scanner; Scanner owns the allocation.
unsafe extern "C" {
    fn tree_sitter_haskell_external_scanner_create() -> *mut c_void;
    fn tree_sitter_haskell_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_haskell_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_haskell_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

#[repr(align(16))]
struct GuardedBuffer([u8; 1026]);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_haskell::LANGUAGE.into();
        // SAFETY: The constructor has no preconditions; this owner frees its result once.
        Self(NonNull::new(unsafe { tree_sitter_haskell_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: All advertised bytes are live; the bounded decoder accepts a null reset.
        unsafe {
            tree_sitter_haskell_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                c_uint::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut buffer = GuardedBuffer([0xa5_u8; 1026]);
        let guarded = &mut buffer.0;
        assert_eq!(
            (guarded[1..].as_ptr() as usize) % align_of::<PersistFrame>(),
            1
        );
        // SAFETY: The live scanner receives a writable 1024-byte serialization buffer.
        let length = unsafe {
            tree_sitter_haskell_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        let length = usize::try_from(length).unwrap();
        assert!(length <= 1024);
        assert_eq!(guarded[0], 0xa5);
        assert!(guarded[1 + length..].iter().all(|byte| *byte == 0xa5));
        guarded[1..1 + length].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This unique owner frees its live allocation through the original allocator.
        unsafe { tree_sitter_haskell_external_scanner_destroy(self.0.as_ptr()) };
    }
}

// These mirrors provide offsets only: no Rust struct representation crosses FFI.
#[repr(C)]
struct NewlineFrame {
    state: c_uint,
    end: c_uint,
    indent: u32,
    flags: [u8; 4],
}

#[repr(C)]
struct PersistFrame {
    contexts: c_uint,
    newline: NewlineFrame,
}

#[repr(C)]
struct ContextFrame {
    sort: c_uint,
    indent: u32,
}

fn word(bytes: &mut [u8], offset: usize, value: c_uint) {
    bytes[offset..offset + size_of::<c_uint>()].copy_from_slice(&value.to_ne_bytes());
}

fn frame(state: c_uint, end: c_uint, flags: [u8; 4], contexts: &[(c_uint, u32)]) -> Vec<u8> {
    let mut bytes = vec![0; size_of::<PersistFrame>() + std::mem::size_of_val(contexts)];
    assert_eq!(size_of::<ContextFrame>(), size_of::<(c_uint, u32)>());
    word(
        &mut bytes,
        offset_of!(PersistFrame, contexts),
        c_uint::try_from(contexts.len()).unwrap(),
    );
    let newline = offset_of!(PersistFrame, newline);
    word(&mut bytes, newline + offset_of!(NewlineFrame, state), state);
    word(&mut bytes, newline + offset_of!(NewlineFrame, end), end);
    word(
        &mut bytes,
        newline + offset_of!(NewlineFrame, indent),
        u32::MAX,
    );
    let start = newline + offset_of!(NewlineFrame, flags);
    bytes[start..start + flags.len()].copy_from_slice(&flags);
    for (index, (sort, indent)) in contexts.iter().enumerate() {
        let start = size_of::<PersistFrame>() + index * size_of::<ContextFrame>();
        word(&mut bytes, start + offset_of!(ContextFrame, sort), *sort);
        word(
            &mut bytes,
            start + offset_of!(ContextFrame, indent),
            *indent,
        );
    }
    bytes
}

fn reset_frame() -> Vec<u8> {
    let mut bytes = frame(3, 0, [0; 4], &[]);
    word(
        &mut bytes,
        offset_of!(PersistFrame, newline) + offset_of!(NewlineFrame, indent),
        0,
    );
    bytes
}

#[test]
fn snapshots_round_trip_every_context_capacity_and_field_domain_without_mutation() {
    let mut scanner = Scanner::new();
    assert_eq!(scanner.bytes().len(), size_of::<PersistFrame>());
    let maximum = (1024 - size_of::<PersistFrame>()) / size_of::<ContextFrame>();
    for count in 0..=maximum {
        let contexts: Vec<_> = (0..count)
            .map(|index| {
                (
                    c_uint::try_from(index % 10).unwrap(),
                    u32::try_from(index).unwrap(),
                )
            })
            .collect();
        let expected = frame(3, 36, [1; 4], &contexts);
        let alignment = align_of::<PersistFrame>();
        let mut unaligned = vec![0xa5; expected.len() + alignment];
        let offset = (alignment - (unaligned.as_ptr() as usize % alignment)) % alignment + 1;
        unaligned[offset..offset + expected.len()].copy_from_slice(&expected);
        let bytes = &unaligned[offset..offset + expected.len()];
        assert_eq!((bytes.as_ptr() as usize) % alignment, 1);
        scanner.restore(bytes);
        assert_eq!(scanner.bytes(), expected, "context count {count}");
        assert_eq!(scanner.bytes(), expected);
    }
    for state in 0..=3 {
        for end in 0..=36 {
            for mask in 0..16_u8 {
                let expected = frame(
                    state,
                    end,
                    std::array::from_fn(|index| (mask >> index) & 1),
                    &[(0, u32::MAX)],
                );
                scanner.restore(&expected);
                assert_eq!(scanner.bytes(), expected);
            }
        }
    }
    scanner.restore(&[]);
    assert_eq!(scanner.bytes(), reset_frame());
}

#[test]
fn invalid_snapshot_lengths_counts_enums_and_booleans_reset_all_prior_state() {
    let mut scanner = Scanner::new();
    let good = frame(2, 36, [1; 4], &[(0, 1), (9, u32::MAX)]);
    let mut invalid: Vec<_> = (0..good.len()).map(|end| good[..end].to_vec()).collect();
    invalid.push([good.as_slice(), &[0]].concat());
    invalid.push(vec![0; 1025]);
    for (offset, values) in [
        (offset_of!(PersistFrame, contexts), &[3, c_uint::MAX][..]),
        (
            offset_of!(PersistFrame, newline) + offset_of!(NewlineFrame, state),
            &[4, c_uint::MAX][..],
        ),
        (
            offset_of!(PersistFrame, newline) + offset_of!(NewlineFrame, end),
            &[37, c_uint::MAX][..],
        ),
        (
            size_of::<PersistFrame>() + offset_of!(ContextFrame, sort),
            &[10, c_uint::MAX][..],
        ),
        (
            size_of::<PersistFrame>() + size_of::<ContextFrame>() + offset_of!(ContextFrame, sort),
            &[10, c_uint::MAX][..],
        ),
    ] {
        for value in values {
            let mut bytes = good.clone();
            word(&mut bytes, offset, *value);
            invalid.push(bytes);
        }
    }
    for offset in 0..4 {
        for value in [2, 255] {
            let mut bytes = good.clone();
            bytes[offset_of!(PersistFrame, newline) + offset_of!(NewlineFrame, flags) + offset] =
                value;
            invalid.push(bytes);
        }
    }
    for bytes in invalid {
        scanner.restore(&good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), reset_frame(), "invalid frame {bytes:?}");
        scanner.restore(&good);
        assert_eq!(scanner.bytes(), good);
    }
    // SAFETY: The live decoder explicitly rejects null nonempty frames before reading bytes.
    unsafe {
        tree_sitter_haskell_external_scanner_deserialize(scanner.0.as_ptr(), std::ptr::null(), 1)
    };
    assert_eq!(scanner.bytes(), reset_frame());
}

#[test]
fn hostile_snapshots_at_every_abi_length_are_bounded_and_canonical() {
    let mut scanner = Scanner::new();
    let good = frame(1, 8, [0, 1, 0, 1], &[(1, 8)]);
    let mut seed = 1_u32;
    for length in 0..=1025 {
        for _ in 0..4 {
            let mut bytes = vec![0; length];
            for byte in &mut bytes {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = seed.to_le_bytes()[3];
            }
            scanner.restore(&good);
            scanner.restore(&bytes);
            let canonical = scanner.bytes();
            assert_eq!(scanner.bytes(), canonical);
            scanner.restore(&canonical);
            assert_eq!(scanner.bytes(), canonical);
        }
    }
}

const SOURCE: &str = "module Sample (adjust) where\nimport qualified Data.List as List\n-- λ😀 source marker\nadjust :: Int -> Int\nadjust value =\n  let next = value + 1\n      helper n = n * 2\n  in helper next\nclass Label a where\n  label :: a -> String\ninstance Label Int where\n  label value = \"value\"\n";
const OPERATORS: &str = "module Operators where\ninfixl 6 <+>\n(<+>) :: Int -> Int -> Int\nleft <+> right = left + right\n";
const LAYOUT: &str = "module Layout where\nchoose value = case value of\n  Just next -> next\n  Nothing -> 0\naction value = do\n  let next = value + 1\n  pure next\nexplicit value = let { next = value + 1; other = next * 2 } in other\n";

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_haskell::LANGUAGE.into())
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
            .map_or(prefix.len(), |index| prefix.len() - index - 1),
    )
}

fn bounded_parse(parser: &mut Parser, source: &[u8], old: Option<&Tree>) -> Tree {
    let mut steps = 0;
    let mut progress = |_: &tree_sitter::ParseState| {
        steps += 1;
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
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .expect("bounded fixture exceeded native progress watchdog")
}

fn signature(tree: &Tree, source: &[u8]) -> Vec<(String, usize, usize, bool, bool, bool)> {
    let mut pending = vec![tree.root_node()];
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
    result
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn exact(node: Node<'_>, source: &str, expected: &str, start: usize) {
    assert_eq!(node.utf8_text(source.as_bytes()).unwrap(), expected);
    assert_eq!(node.byte_range(), start..start + expected.len());
    assert_eq!(node.start_position(), point(source.as_bytes(), start));
    assert_eq!(
        node.end_position(),
        point(source.as_bytes(), start + expected.len())
    );
}

#[test]
fn declarations_imports_signatures_and_operator_heads_keep_exact_source_ranges() {
    for newline in ["\n", "\r\n"] {
        let source = SOURCE.replace('\n', newline);
        let tree = bounded_parse(&mut parser(), source.as_bytes(), None);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(tree.root_node().kind(), "haskell");
        signature(&tree, source.as_bytes());
        let headers = nodes(tree.root_node(), "header");
        assert_eq!(headers.len(), 1);
        exact(
            headers[0].child_by_field_name("module").unwrap(),
            &source,
            "Sample",
            source.find("Sample").unwrap(),
        );
        let imports = nodes(tree.root_node(), "import");
        assert_eq!(imports.len(), 1);
        exact(
            imports[0].child_by_field_name("module").unwrap(),
            &source,
            "Data.List",
            source.find("Data.List").unwrap(),
        );
        exact(
            imports[0].child_by_field_name("alias").unwrap(),
            &source,
            "List",
            source.find("as List").unwrap() + 3,
        );
        let functions = nodes(tree.root_node(), "function");
        let named: Vec<_> = functions
            .iter()
            .filter_map(|node| node.child_by_field_name("name"))
            .collect();
        assert_eq!(named.len(), 3);
        for (name, written) in named
            .iter()
            .zip(["adjust value", "helper n", "label value"])
        {
            let expected = written.split(' ').next().unwrap();
            exact(*name, &source, expected, source.find(written).unwrap());
        }
        for kind in ["class", "instance"] {
            let declarations = nodes(tree.root_node(), kind);
            assert_eq!(declarations.len(), 1);
            exact(
                declarations[0].child_by_field_name("name").unwrap(),
                &source,
                "Label",
                source.find(&format!("{kind} Label")).unwrap() + kind.len() + 1,
            );
        }
        let signatures = nodes(tree.root_node(), "signature");
        assert_eq!(signatures.len(), 2);
        for (node, written) in signatures.iter().zip(["adjust ::", "label ::"]) {
            let expected = written.split(' ').next().unwrap();
            exact(
                node.child_by_field_name("name").unwrap(),
                &source,
                expected,
                source.find(written).unwrap(),
            );
        }
        let source = OPERATORS.replace('\n', newline);
        let tree = bounded_parse(&mut parser(), source.as_bytes(), None);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        signature(&tree, source.as_bytes());
        let functions = nodes(tree.root_node(), "function");
        let declaration = functions
            .iter()
            .find(|node| node.child_by_field_name("match").is_some())
            .unwrap();
        let head = nodes(*declaration, "infix")
            .into_iter()
            .find(|node| node.start_byte() == source.find("left <+>").unwrap())
            .unwrap();
        exact(
            head.child_by_field_name("operator").unwrap(),
            &source,
            "<+>",
            source.find("left <+>").unwrap() + 5,
        );
    }
}

#[test]
fn string_gaps_keep_exact_source_ranges_with_lf_and_crlf() {
    for newline in ["\n", "\r\n"] {
        let literal = format!("\"left\\{newline}    \\right\"");
        let source = format!(
            "module StringGap where{newline}message = {literal}{newline}control = \"plain\"{newline}"
        );
        let tree = bounded_parse(&mut parser(), source.as_bytes(), None);
        assert!(
            !tree.root_node().has_error(),
            "newline {newline:?}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(tree.root_node().kind(), "haskell");
        let inventory = signature(&tree, source.as_bytes());
        assert!(inventory.iter().all(|node| !node.4 && !node.5));
        let strings = nodes(tree.root_node(), "string");
        assert_eq!(strings.len(), 2, "newline {newline:?}");
        exact(
            strings[0],
            &source,
            &literal,
            source.find(&literal).unwrap(),
        );
        exact(
            strings[1],
            &source,
            "\"plain\"",
            source.find("\"plain\"").unwrap(),
        );
    }
}

#[test]
fn layout_braces_recovery_and_parser_reuse_do_not_leak_prior_document_state() {
    let mut reused = parser();
    for newline in ["\n", "\r\n"] {
        for fixture in [SOURCE, OPERATORS, LAYOUT] {
            let source = fixture.replace('\n', newline);
            let tree = bounded_parse(&mut reused, source.as_bytes(), None);
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            let fresh = bounded_parse(&mut parser(), source.as_bytes(), None);
            assert_eq!(
                signature(&tree, source.as_bytes()),
                signature(&fresh, source.as_bytes())
            );
        }
        for fixture in [
            "module Broken (\n",
            "module Broken where\nvalue = let next =\n",
            "module Broken where\nvalue = (1 +\n",
        ] {
            let source = fixture.replace('\n', newline);
            let tree = bounded_parse(&mut reused, source.as_bytes(), None);
            assert!(tree.root_node().has_error(), "{source:?}");
            signature(&tree, source.as_bytes());
            let next = bounded_parse(&mut reused, SOURCE.as_bytes(), None);
            let fresh = bounded_parse(&mut parser(), SOURCE.as_bytes(), None);
            assert_eq!(
                signature(&next, SOURCE.as_bytes()),
                signature(&fresh, SOURCE.as_bytes())
            );
        }
    }
}

#[test]
fn incremental_byte_damage_and_restore_match_fresh_layout_trees() {
    for fixture in [SOURCE, LAYOUT] {
        for newline in ["\n", "\r\n"] {
            let source = fixture.replace('\n', newline).into_bytes();
            let mut reused = parser();
            let original = bounded_parse(&mut reused, &source, None);
            assert!(!original.root_node().has_error());
            let expected = signature(&original, &source);
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
                let mut incremental = bounded_parse(&mut reused, &damaged, Some(&old));
                let fresh = bounded_parse(&mut parser(), &damaged, None);
                assert_eq!(
                    signature(&incremental, &damaged),
                    signature(&fresh, &damaged),
                    "damage at {offset}"
                );
                incremental.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&damaged, offset),
                    old_end_position: point(&damaged, offset + 1),
                    new_end_position: point(&source, offset + 1),
                });
                let restored = bounded_parse(&mut reused, &source, Some(&incremental));
                assert_eq!(
                    signature(&restored, &source),
                    expected,
                    "restore at {offset}"
                );
            }
        }
    }
}

#[test]
fn cancellation_then_reset_preserves_the_next_complete_document() {
    let source = format!(
        "module Work where\n{}",
        "value x = let next = x + 1 in next\n".repeat(10_000)
    );
    let mut reused = parser();
    let mut called = false;
    let mut cancel = |_: &tree_sitter::ParseState| {
        called = true;
        ControlFlow::Break(())
    };
    let tree = reused.parse_with_options(
        &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut cancel)),
    );
    assert!(called && tree.is_none());
    reused.reset();
    let next = bounded_parse(&mut reused, SOURCE.as_bytes(), None);
    let fresh = bounded_parse(&mut parser(), SOURCE.as_bytes(), None);
    assert_eq!(
        signature(&next, SOURCE.as_bytes()),
        signature(&fresh, SOURCE.as_bytes())
    );
}
