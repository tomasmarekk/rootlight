//! PowerShell's stateless scanner emits zero-width statement boundaries.
//! Direct ABI checks keep lookahead, disabled-token and serialization contracts explicit.

use std::ffi::{c_char, c_uint, c_void};

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
    marked: Option<usize>,
    advanced_past_end: bool,
    non_skip_advance: bool,
}

unsafe extern "C" fn advance(lexer: *mut Lexer, skip: bool) {
    // SAFETY: The scanner receives the first field of a live, exclusive repr(C) Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.advanced_past_end |= input.offset >= input.points.len();
    input.non_skip_advance |= !skip;
    input.offset = input.offset.saturating_add(1);
    input.lexer.lookahead = input.points.get(input.offset).copied().unwrap_or(0);
}

unsafe extern "C" fn mark_end(lexer: *mut Lexer) {
    // SAFETY: The first-field address is the owning live Input address throughout scan.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = Some(input.offset);
}

unsafe extern "C" fn eof(lexer: *const Lexer) -> bool {
    // SAFETY: The callback receives the live Input supplied to the scanner.
    let input = unsafe { &*lexer.cast::<Input>() };
    input.offset >= input.points.len()
}

// SAFETY: Signatures and Lexer layout match the pinned crate's scanner.c and parser.h.
unsafe extern "C" {
    fn tree_sitter_powershell_external_scanner_create() -> *mut c_void;
    fn tree_sitter_powershell_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_powershell_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid_symbols: *const bool,
    ) -> bool;
    fn tree_sitter_powershell_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_powershell_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

fn scan(text: &str, valid: bool) -> (bool, Input) {
    let _: tree_sitter::Language = tree_sitter_powershell::LANGUAGE.into();
    let points: Vec<_> = text
        .chars()
        .map(|ch| i32::try_from(u32::from(ch)).unwrap())
        .collect();
    let mut input = Input {
        lexer: Lexer {
            lookahead: points.first().copied().unwrap_or(0),
            result_symbol: u16::MAX,
            advance,
            mark_end,
            get_column: None,
            is_at_included_range_start: None,
            eof,
            log: None,
        },
        points,
        offset: 0,
        marked: None,
        advanced_past_end: false,
        non_skip_advance: false,
    };
    // SAFETY: The pinned scanner is stateless (null payload); Input and its one valid
    // symbol are live, and the only used callbacks are non-panicking advance/mark_end.
    let found = unsafe {
        tree_sitter_powershell_external_scanner_scan(std::ptr::null_mut(), &mut input.lexer, &valid)
    };
    assert!(!input.advanced_past_end, "{text:?}");
    assert!(!input.non_skip_advance, "{text:?}");
    (found, input)
}

#[test]
fn powershell_terminators_are_zero_width_and_never_consume_the_delimiter() {
    for prefix in ["", " ", "\t\r ", "\u{000b}\u{000c}"] {
        for delimiter in ["", "}", ";", ")", "\n"] {
            let source = format!("{prefix}{delimiter}");
            let (found, input) = scan(&source, true);
            assert!(found, "{source:?}");
            assert_eq!(input.lexer.result_symbol, 0);
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, prefix.chars().count(), "{source:?}");
        }
    }
}

#[test]
fn powershell_disabled_terminators_leave_all_lexer_state_untouched() {
    for source in ["", " ", "\r\n", ";", "}", ")", "\t$next", "雪"] {
        let (found, input) = scan(source, false);
        assert!(!found);
        assert_eq!(input.offset, 0);
        assert_eq!(input.marked, None);
        assert_eq!(input.lexer.result_symbol, u16::MAX);
    }
}

#[test]
fn powershell_non_terminators_are_rejected_after_only_whitespace_lookahead() {
    for suffix in ["$value", "command", "{", "(", "雪", "🦀"] {
        let source = format!(" \t{suffix}");
        let (found, input) = scan(&source, true);
        assert!(!found, "{source:?}");
        assert_eq!(input.offset, 2);
        assert_eq!(input.marked, Some(0));
    }
}

#[test]
fn powershell_stateless_roundtrips_never_touch_serialization_storage() {
    let _: tree_sitter::Language = tree_sitter_powershell::LANGUAGE.into();
    // SAFETY: The constructor takes no arguments and returns the stateless null payload.
    let payload = unsafe { tree_sitter_powershell_external_scanner_create() };
    assert!(payload.is_null());
    for length in 0..=1024 {
        let input = [0x5a_u8; 1024];
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: Input covers the declared length; the writable region covers the
        // 1024-byte serialization ABI maximum. The pinned scanner accepts null payload.
        let written = unsafe {
            tree_sitter_powershell_external_scanner_deserialize(
                payload,
                input.as_ptr().cast(),
                length,
            );
            tree_sitter_powershell_external_scanner_serialize(
                payload,
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert_eq!(written, 0);
        assert_eq!(guarded, [0xa5; 1026]);
    }
    // SAFETY: This is the payload from the constructor, destroyed exactly once.
    unsafe { tree_sitter_powershell_external_scanner_destroy(payload) };
}
