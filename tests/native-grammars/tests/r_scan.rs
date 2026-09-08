//! Direct R lexer callbacks pin rejected-token rollback and code-point handling.
//! The C ABI mirror is isolated here; callbacks never unwind into the native scanner.

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
    get_column: Option<unsafe extern "C" fn(*mut Lexer) -> u32>,
    is_at_included_range_start: Option<unsafe extern "C" fn(*const Lexer) -> bool>,
    eof: unsafe extern "C" fn(*const Lexer) -> bool,
    log: Option<unsafe extern "C" fn(*const Lexer, *const c_char, ...)>,
}

#[repr(C)]
struct Input {
    lexer: Lexer,
    points: Vec<i32>,
    offset: usize,
    marked: usize,
    advanced_past_end: bool,
}

unsafe extern "C" fn advance(lexer: *mut Lexer, _skip: bool) {
    // SAFETY: Each callback receives the first field of a live, uniquely borrowed Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.advanced_past_end |= input.offset >= input.points.len();
    input.offset = input.offset.saturating_add(1);
    input.lexer.lookahead = input.points.get(input.offset).copied().unwrap_or(0);
}

unsafe extern "C" fn mark(lexer: *mut Lexer) {
    // SAFETY: The repr(C) first-field address is the owning live Input address.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = input.offset;
}

unsafe extern "C" fn eof(lexer: *const Lexer) -> bool {
    // SAFETY: The scanner only invokes this callback with its live Input lexer.
    let input = unsafe { &*lexer.cast::<Input>() };
    input.offset >= input.points.len()
}

// SAFETY: Signatures and Lexer layout match src/tree_sitter/parser.h in the pinned crate.
unsafe extern "C" {
    fn tree_sitter_r_external_scanner_create() -> *mut c_void;
    fn tree_sitter_r_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_r_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid: *const bool,
    ) -> bool;
    fn tree_sitter_r_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum Token {
    Semicolon = 2,
    RawOpen = 3,
    RawClose = 5,
    CloseParen = 8,
    OpenBrace = 9,
    CloseBrace = 10,
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_r::LANGUAGE.into();
        // SAFETY: No constructor arguments; the unique owner destroys the allocation once.
        Self(NonNull::new(unsafe { tree_sitter_r_external_scanner_create() }).unwrap())
    }

    fn scan(&mut self, text: &str, token: Token) -> bool {
        let points: Vec<_> = text
            .chars()
            .map(|ch| i32::try_from(u32::from(ch)).unwrap())
            .collect();
        let mut input = Input {
            lexer: Lexer {
                lookahead: points.first().copied().unwrap_or(0),
                result_symbol: u16::MAX,
                advance,
                mark_end: mark,
                get_column: None,
                is_at_included_range_start: None,
                eof,
                log: None,
            },
            points,
            offset: 0,
            marked: 0,
            advanced_past_end: false,
        };
        let mut valid = [false; 16];
        valid[token as usize] = true;
        // SAFETY: Scanner and Input are live and exclusive, all 16 flags are initialized,
        // and the pinned scanner calls only advance, mark_end and eof in these paths.
        let found = unsafe {
            tree_sitter_r_external_scanner_scan(self.0.as_ptr(), &mut input.lexer, valid.as_ptr())
        };
        assert!(!input.advanced_past_end, "{text:?}");
        if found {
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(token as usize).unwrap()
            );
            assert_eq!(input.marked, input.points.len(), "{text:?}");
        }
        found
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The scanner receives a live writable 1024-byte ABI buffer.
        let length = unsafe {
            tree_sitter_r_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert!(length <= 1024);
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[1025], 0xa5);
        guarded[1..1 + usize::try_from(length).unwrap()].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: The sole owner frees its still-live allocation with the original allocator.
        unsafe { tree_sitter_r_external_scanner_destroy(self.0.as_ptr()) };
    }
}

#[test]
fn r_rejected_close_preserves_the_open_scope() {
    let mut scanner = Scanner::new();
    let empty = scanner.bytes();
    assert!(scanner.scan("{", Token::OpenBrace));
    let before = scanner.bytes();
    assert!(!scanner.scan(")", Token::CloseParen));
    assert_eq!(scanner.bytes(), before);
    assert!(scanner.scan("}", Token::CloseBrace));
    assert_eq!(scanner.bytes(), empty);
}

#[test]
fn r_raw_close_revalidates_source_after_restoring_context() {
    let mut scanner = Scanner::new();
    assert!(scanner.scan("r\"--(", Token::RawOpen));
    let before = scanner.bytes();
    for source in ["", ")", ")-", ")--", ")--'", "]--\""] {
        assert!(!scanner.scan(source, Token::RawClose));
        assert_eq!(scanner.bytes(), before);
    }
    assert!(scanner.scan(")--\"", Token::RawClose));
}

#[test]
fn r_scope_capacity_rejects_tokens_without_truncation() {
    let mut scanner = Scanner::new();
    let empty = scanner.bytes();
    let capacity = 1024 - 3 - size_of::<c_uint>();
    for _ in 0..capacity {
        assert!(scanner.scan("{", Token::OpenBrace));
    }
    let full = scanner.bytes();
    assert_eq!(full.len(), 1024);
    assert!(!scanner.scan("{", Token::OpenBrace));
    assert_eq!(scanner.bytes(), full);
    for _ in 0..capacity {
        assert!(scanner.scan("}", Token::CloseBrace));
    }
    assert_eq!(scanner.bytes(), empty);
}

#[test]
fn r_codepoint_aliases_never_enter_raw_or_whitespace_state() {
    let mut scanner = Scanner::new();
    let empty = scanner.bytes();
    for text in ["r\u{122}(", "r\"\u{128}", "r\"\u{15b}", "r\"\u{17b}"] {
        assert!(!scanner.scan(text, Token::RawOpen));
        assert_eq!(scanner.bytes(), empty);
    }
    assert!(!scanner.scan("\u{10020};", Token::Semicolon));
    assert_eq!(scanner.bytes(), empty);
}
