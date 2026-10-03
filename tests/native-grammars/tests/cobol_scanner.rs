//! Direct COBOL scanner checks use an EOF-no-op lexer like Tree-sitter's runtime.
//! A callback watchdog records non-progress instead of allowing a native hang.

use std::ffi::c_void;

#[repr(C)]
struct Lexer {
    lookahead: i32,
    result_symbol: u16,
    advance: unsafe extern "C" fn(*mut Lexer, bool),
    mark_end: unsafe extern "C" fn(*mut Lexer),
    get_column: unsafe extern "C" fn(*mut Lexer) -> u32,
    is_at_included_range_start: unsafe extern "C" fn(*const Lexer) -> bool,
    eof: unsafe extern "C" fn(*const Lexer) -> bool,
}

#[repr(C)]
struct Input {
    lexer: Lexer,
    points: Vec<i32>,
    offset: usize,
    column: u32,
    marked: Option<usize>,
    calls: usize,
    past_end: bool,
    watchdog: bool,
}

unsafe extern "C" fn advance(lexer: *mut Lexer, _skip: bool) {
    // SAFETY: Every callback receives the first field of a live exclusive Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.calls = input.calls.saturating_add(1);
    if input.calls > input.points.len().saturating_add(8) {
        input.watchdog = true;
        input.column = 73;
        input.lexer.lookahead = 0;
        return;
    }
    if input.offset >= input.points.len() {
        input.past_end = true;
        return;
    }
    if input.lexer.lookahead == i32::from(b'\n') {
        input.column = 0;
    } else {
        input.column = input.column.saturating_add(1);
    }
    input.offset = input.offset.saturating_add(1);
    input.lexer.lookahead = input.points.get(input.offset).copied().unwrap_or(0);
}

unsafe extern "C" fn mark_end(lexer: *mut Lexer) {
    // SAFETY: The scanner retains the same live first-field address during scan.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = Some(input.offset);
}

unsafe extern "C" fn get_column(lexer: *mut Lexer) -> u32 {
    // SAFETY: The lexer is the first field of the live supplied Input.
    unsafe { (*lexer.cast::<Input>()).column }
}

unsafe extern "C" fn included_start(_lexer: *const Lexer) -> bool {
    false
}

unsafe extern "C" fn eof(lexer: *const Lexer) -> bool {
    // SAFETY: Callbacks only receive the live first field of the supplied Input.
    let input = unsafe { &*lexer.cast::<Input>() };
    input.offset >= input.points.len() || input.watchdog
}

// SAFETY: Signatures and Lexer prefix match the retained scanner and parser.h.
unsafe extern "C" {
    fn tree_sitter_COBOL_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid: *const bool,
    ) -> bool;
    fn tree_sitter_COBOL_free_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid: *const bool,
    ) -> bool;
}

fn scan(text: &str, column: u32, valid: [bool; 6]) -> (bool, Input) {
    scan_profile(text, column, valid, false)
}

fn scan_profile(text: &str, column: u32, valid: [bool; 6], free: bool) -> (bool, Input) {
    let _: tree_sitter::Language = if free {
        tree_sitter_cobol::FREE
    } else {
        tree_sitter_cobol::FIXED
    }
    .into();
    let points: Vec<_> = text
        .chars()
        .map(|c| i32::try_from(u32::from(c)).unwrap())
        .collect();
    let mut input = Input {
        lexer: Lexer {
            lookahead: points.first().copied().unwrap_or(0),
            result_symbol: u16::MAX,
            advance,
            mark_end,
            get_column,
            is_at_included_range_start: included_start,
            eof,
        },
        points,
        offset: 0,
        column,
        marked: None,
        calls: 0,
        past_end: false,
        watchdog: false,
    };
    // SAFETY: The scanner is stateless; six initialized flags and Input stay live,
    // and all callbacks are non-panicking and cannot unwind across the C boundary.
    let found = unsafe {
        if free {
            tree_sitter_COBOL_free_external_scanner_scan(
                std::ptr::null_mut(),
                &mut input.lexer,
                valid.as_ptr(),
            )
        } else {
            tree_sitter_COBOL_external_scanner_scan(
                std::ptr::null_mut(),
                &mut input.lexer,
                valid.as_ptr(),
            )
        }
    };
    assert!(!input.watchdog, "scanner non-progress at {text:?}");
    assert!(!input.past_end, "scanner advance beyond EOF at {text:?}");
    (found, input)
}

#[test]
fn disabled_and_recovery_tokens_leave_input_untouched() {
    for free in [false, true] {
        for valid in [[false; 6], [true; 6]] {
            for text in ["", " ", "123", "\0", "雪", "🦀", "\r\n", "\"closed\""] {
                let (found, input) = scan_profile(text, 0, valid, free);
                assert!(!found);
                assert_eq!(input.offset, 0);
                assert_eq!(input.marked, None);
                assert_eq!(input.lexer.result_symbol, u16::MAX);
            }
        }
    }
}

#[test]
fn fixed_prefix_never_crosses_a_physical_line() {
    for ending in ["\n", "\r\n", "\r"] {
        for prefix in ["1", "123", "12345"] {
            let text = format!("{prefix}{ending}IDENTIFICATION DIVISION.");
            let (found, input) = scan(&text, 0, [false, true, false, false, false, false]);
            assert!(found);
            assert_eq!(input.marked, Some(prefix.len()));
            assert_eq!(input.offset, prefix.len());
        }
    }
}

#[test]
fn free_profile_never_discards_fixed_prefix_suffix_or_indicator_bytes() {
    for (text, column, symbol) in [
        ("IDENTIFICATION", 0, 1),
        ("MOVE value", 73, 2),
        ("* text", 6, 3),
    ] {
        let mut valid = [false; 6];
        valid[symbol] = true;
        let (found, input) = scan_profile(text, column, valid, true);
        assert!(!found);
        assert_eq!(input.offset, 0);
        assert_eq!(input.marked, None);
    }
}

#[test]
fn comment_keywords_are_bounded_case_insensitive_words() {
    let valid = [false, false, false, false, true, false];
    for free in [false, true] {
        for keyword in [
            "author",
            "installation",
            "date-written",
            "date-compiled",
            "security",
            "identification division",
            "environment division",
            "data division",
            "procedure division",
            "PROCEDURE\t  DIVISION",
            "ENVIRONMENT    DIVISION",
        ] {
            for ending in ["", ".", "\n", "\r\n", ". trailing"] {
                let (found, _) = scan_profile(&format!("{keyword}{ending}"), 8, valid, free);
                assert!(!found, "reserved keyword consumed as comment: {keyword:?}");
            }
        }
        for text in [
            "authoritarian text",
            "ordinary trailing text",
            "procedure divisi",
            "雪\0🦀 trailing",
        ] {
            let (found, input) = scan_profile(text, 8, valid, free);
            assert!(found, "{text:?}");
            assert_eq!(input.marked, Some(text.chars().count()));
        }
    }
}

#[test]
fn unicode_and_embedded_nul_are_not_eof_or_layout_whitespace() {
    for free in [false, true] {
        for text in ["\0", "雪", "🦀", "\u{a0}"] {
            let (found, input) =
                scan_profile(text, 8, [true, false, false, false, false, false], free);
            assert!(!found);
            assert_eq!(input.offset, 0);
        }
        let (found, input) =
            scan_profile(" \0x", 8, [true, false, false, false, false, false], free);
        assert!(found);
        assert_eq!(input.marked, Some(1));
    }
}

#[test]
fn fixed_literal_requires_authored_closure_and_continuation_indicator() {
    let valid = [false, false, false, false, false, true];
    for quote in ['\'', '"'] {
        for text in [
            format!("{quote}closed{quote}"),
            format!("{quote}雪🦀{quote}"),
            format!("{quote}a{quote}{quote}b{quote}"),
        ] {
            let (found, input) = scan(&text, 8, valid);
            assert!(found);
            assert_eq!(input.marked, Some(text.chars().count()));
        }
        for ending in ["\n", "\r\n"] {
            let text = format!("{quote}first{ending}      -    {quote}second{quote}");
            let (found, input) = scan(&text, 8, valid);
            assert!(found);
            assert_eq!(input.marked, Some(text.chars().count()));
            for text in [
                format!("{quote}unterminated{ending}       unrelated{quote}"),
                format!("{quote}first{ending}      -    {quote}unterminated"),
            ] {
                assert!(!scan(&text, 8, valid).0);
            }
        }
        assert!(!scan(&format!("{quote}unterminated"), 8, valid).0);
        assert!(!scan(&format!("{quote}a{quote}{quote}unterminated"), 8, valid).0);
        assert!(
            !scan(
                &format!("{quote}first\rX\n      - {quote}second{quote}"),
                8,
                valid
            )
            .0
        );
        assert!(
            !scan(
                &format!(
                    "{quote}first\n      -{}{quote}second{quote}",
                    " ".repeat(65)
                ),
                8,
                valid
            )
            .0
        );
        let text = format!("{quote}{}{quote}", "x".repeat(63));
        assert!(
            !scan(&text, 8, valid).0,
            "column 73 quote must not close a fixed literal"
        );
        assert!(!scan_profile(&format!("{quote}literal{quote}"), 0, valid, true).0);
    }
}

#[test]
fn fixed_sequence_prefix_stops_at_eof_without_advancing_past_it() {
    for text in ["1", "12", "123", "1234", "12345", "123456"] {
        let (found, input) = scan(text, 0, [false, true, false, false, false, false]);
        assert!(!input.watchdog, "non-progress at {text:?}");
        assert!(!input.past_end, "advance past EOF at {text:?}");
        assert!(found);
        assert_eq!(input.offset, text.len());
        assert_eq!(input.marked, Some(text.len()));
    }
}
