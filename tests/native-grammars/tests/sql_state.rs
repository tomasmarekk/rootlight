//! SQL scanner snapshots must be complete, bounded and observational.
//! Contextual batch tokens prove physical lines without changing dollar state;
//! this isolated FFI suite never executes SQL or utility batch counts.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

#[repr(C)]
struct Lexer {
    lookahead: i32,
    result_symbol: u16,
    advance: unsafe extern "C" fn(*mut Lexer, bool),
    mark_end: unsafe extern "C" fn(*mut Lexer),
    get_column: unsafe extern "C" fn(*mut Lexer) -> u32,
    is_at_included_range_start: unsafe extern "C" fn(*const Lexer) -> bool,
    eof: unsafe extern "C" fn(*const Lexer) -> bool,
    log: Option<unsafe extern "C" fn(*const Lexer, *const c_char, ...)>,
}

#[repr(C)]
struct Input {
    lexer: Lexer,
    points: Vec<i32>,
    offset: usize,
    token_start: usize,
    marked: usize,
    column: u32,
}

impl Input {
    fn new(text: &str, column: u32) -> Self {
        let points: Vec<_> = text
            .chars()
            .map(|ch| i32::try_from(u32::from(ch)).unwrap())
            .collect();
        Self {
            lexer: Lexer {
                lookahead: points.first().copied().unwrap_or(0),
                result_symbol: u16::MAX,
                advance,
                mark_end,
                get_column,
                is_at_included_range_start,
                eof,
                log: None,
            },
            points,
            offset: 0,
            token_start: 0,
            marked: 0,
            column,
        }
    }

    fn token(&self) -> String {
        self.points[self.token_start..self.marked]
            .iter()
            .map(|point| char::from_u32(u32::try_from(*point).unwrap()).unwrap())
            .collect()
    }
}

unsafe extern "C" fn advance(lexer: *mut Lexer, skip: bool) {
    // SAFETY: The scanner receives the repr(C) first field of this exclusively live Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.column = if input.lexer.lookahead == i32::from(b'\n') {
        0
    } else {
        input.column.saturating_add(1)
    };
    input.offset = input.offset.saturating_add(1);
    if skip {
        input.token_start = input.offset;
    }
    input.lexer.lookahead = input.points.get(input.offset).copied().unwrap_or(0);
}

unsafe extern "C" fn mark_end(lexer: *mut Lexer) {
    // SAFETY: The scanner supplies the first field of its still-live Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = input.offset;
}

unsafe extern "C" fn get_column(lexer: *mut Lexer) -> u32 {
    // SAFETY: The callback only reads the exclusively borrowed live Input.
    unsafe { &*lexer.cast::<Input>() }.column
}

unsafe extern "C" fn is_at_included_range_start(_lexer: *const Lexer) -> bool {
    false
}

unsafe extern "C" fn eof(lexer: *const Lexer) -> bool {
    // SAFETY: The scanner supplies the first field of its live Input; all points stay allocated.
    let input = unsafe { &*lexer.cast::<Input>() };
    input.offset >= input.points.len()
}

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
    fn tree_sitter_sql_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid_symbols: *const bool,
    ) -> bool;
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

    fn scan(&mut self, input: &mut Input, valid: [bool; 4]) -> bool {
        // SAFETY: Scanner is uniquely live, Lexer exactly matches parser.h, all callback
        // storage outlives the call, and four initialized bools match all external ordinals.
        unsafe {
            tree_sitter_sql_external_scanner_scan(
                self.0.as_ptr(),
                std::ptr::from_mut(input).cast::<Lexer>(),
                valid.as_ptr(),
            )
        }
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

#[test]
fn sql_go_tokens_prove_physical_lines_and_preserve_stateless_snapshots() {
    let mut scanner = Scanner::new();
    for (source, column, expected) in [
        ("GO", 0, "GO"),
        ("GO\n", 0, "GO"),
        ("\r\nGO\r\n", 17, "GO"),
        ("\n  gO\n", 13, "gO"),
        ("\tGO 2 -- repeated batch\r\n", 0, "GO 2"),
        ("GO 0002 -- EOF comment", 0, "GO 0002"),
    ] {
        let mut input = Input::new(source, column);
        assert!(
            scanner.scan(&mut input, [false, false, false, true]),
            "{source:?}"
        );
        assert_eq!(input.lexer.result_symbol, 3);
        assert_eq!(input.token(), expected);
        assert!(scanner.bytes().is_empty());
        assert!(input.offset <= input.points.len());
    }
}

#[test]
fn sql_go_rejects_invalid_context_counts_prefixes_and_block_comment_lines() {
    let mut scanner = Scanner::new();
    for (source, column) in [
        (" GO\n", 8),
        ("GO\n", 9),
        ("GONE\n", 0),
        ("GO2\n", 0),
        ("GO 0\n", 0),
        ("GO 000\n", 0),
        ("GO -2\n", 0),
        ("GO +2\n", 0),
        ("GO;\n", 0),
        ("GO 2 SELECT 1\n", 0),
        ("GO /* comment */\n", 0),
        ("/* comment */ GO\n", 0),
        ("GO - comment\n", 0),
        ("\u{c}GO\n", 0),
        ("\u{b}GO\n", 0),
    ] {
        let mut input = Input::new(source, column);
        assert!(
            !scanner.scan(&mut input, [false, false, false, true]),
            "{source:?}"
        );
        assert!(scanner.bytes().is_empty());
        assert!(input.offset <= input.points.len());
    }
    let mut input = Input::new("GO\n", 0);
    assert!(!scanner.scan(&mut input, [true, true, true, false]));
    assert!(scanner.bytes().is_empty());
}

#[test]
fn sql_batch_scanning_never_consumes_or_rewrites_live_dollar_state() {
    let mut scanner = Scanner::new();
    let mut start = Input::new("$outer$", 0);
    assert!(scanner.scan(&mut start, [true, false, false, false]));
    assert_eq!(start.lexer.result_symbol, 0);
    let retained = scanner.bytes();
    assert_eq!(retained, frame("$outer$"));
    let mut go = Input::new("\nGO\n", 18);
    assert!(!scanner.scan(&mut go, [false, false, false, true]));
    assert_eq!(scanner.bytes(), retained);
    assert_eq!(scanner.bytes(), retained);
    let mut end = Input::new("$outer$", 0);
    assert!(scanner.scan(&mut end, [false, true, false, false]));
    assert_eq!(end.lexer.result_symbol, 1);
    assert!(scanner.bytes().is_empty());
    let mut literal = Input::new("$text$\nGO\n$text$", 0);
    assert!(scanner.scan(&mut literal, [false, false, true, false]));
    assert_eq!(literal.lexer.result_symbol, 2);
    assert_eq!(literal.token(), "$text$\nGO\n$text$");
    assert!(scanner.bytes().is_empty());
}
