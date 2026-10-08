//! Skip-aware direct qualification for the distinct qualified-callee DOT scanner.
//! This companion models contiguous UTF-8 lexer spans for both factories; it augments the retained scanner suite.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

const TOKEN_COUNT: usize = 53;
const INDENT: usize = 1;
const ERROR_SENTINEL: usize = 39;
const NEWLINE: usize = 0;
const DEDENT: usize = 2;
const BLOCK_COMMENT_CONTENT: usize = 18;
const INSIDE_STRING: usize = 19;
const PAREN_INDENT: usize = 30;
const TYPE_APP_INDENT: usize = 31;
const TRY_INDENT: usize = 35;
const BRACE_INDENT: usize = 38;
const DO_BODY_INDENT: usize = 41;
const APPLICATION_ARG_START: usize = 43;
const ROOT_BODY_INDENT: usize = 45;
const QUALIFIED_CALLEE_DOT: usize = 47;
const NEWLINE_NO_ALIGNED: usize = 20;
const SNAPSHOT_CAPACITY: usize = 1024;
const COLUMNS: [u32; 4] = [1, 257, 65_535, 65_536];

#[derive(Clone, Copy, Debug)]
enum Profile {
    Implementation,
    Signature,
}

const PROFILES: [Profile; 2] = [Profile::Implementation, Profile::Signature];

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
    points: Vec<(i32, usize)>,
    offset: usize,
    byte_offset: usize,
    token_start: usize,
    column: u32,
    marked: Option<usize>,
    inspected_end: usize,
    byte_length: usize,
    advances: usize,
    skips: usize,
    budget: usize,
    advanced_past_end: bool,
    watchdog: bool,
}

impl Input {
    fn new(text: &str, column: u32) -> Self {
        let points: Vec<_> = text
            .chars()
            .map(|ch| (i32::try_from(u32::from(ch)).unwrap(), ch.len_utf8()))
            .collect();
        let first = points.first().copied().unwrap_or((0, 0));
        let budget = points.len().saturating_mul(8).saturating_add(64);
        Self {
            lexer: Lexer {
                lookahead: first.0,
                result_symbol: u16::MAX,
                advance,
                mark_end,
                get_column,
                is_at_included_range_start: included_start,
                eof,
                log: None,
            },
            points,
            offset: 0,
            byte_offset: 0,
            token_start: 0,
            column,
            marked: None,
            inspected_end: first.1,
            byte_length: text.len(),
            advances: 0,
            skips: 0,
            budget,
            advanced_past_end: false,
            watchdog: false,
        }
    }

    fn accepted_span(&self, found: bool) -> Option<std::ops::Range<usize>> {
        if !found {
            return None;
        }
        let end = self.marked.unwrap_or(self.byte_offset);
        // Runtime finish moves START backward to a retained END, never END forward.
        Some(self.token_start.min(end)..end)
    }
}

unsafe extern "C" fn advance(lexer: *mut Lexer, skip: bool) {
    // SAFETY: The callback receives the first field of this live exclusive Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.advances = input.advances.saturating_add(1);
    if input.advances > input.budget {
        input.watchdog = true;
        input.lexer.lookahead = 0;
        return;
    }
    let Some((point, width)) = input.points.get(input.offset).copied() else {
        input.advanced_past_end = true;
        return;
    };
    if point == i32::from(b'\n') {
        input.column = 0;
    } else if !(input.byte_offset == 0 && point == 0xFEFF) {
        input.column = input.column.saturating_add(1);
    }
    input.byte_offset = input.byte_offset.saturating_add(width);
    input.offset = input.offset.saturating_add(1);
    if skip {
        input.skips = input.skips.saturating_add(1);
        input.token_start = input.byte_offset;
    }
    let next = input.points.get(input.offset).copied().unwrap_or((0, 0));
    input.lexer.lookahead = next.0;
    input.inspected_end = input
        .inspected_end
        .max(input.byte_offset.saturating_add(next.1));
}

unsafe extern "C" fn mark_end(lexer: *mut Lexer) {
    // SAFETY: No callback outlives the owning Input or retains its address.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = Some(input.byte_offset);
}

unsafe extern "C" fn get_column(lexer: *mut Lexer) -> u32 {
    // SAFETY: This is the same first-field pointer supplied to scan.
    unsafe { (*lexer.cast::<Input>()).column }
}

unsafe extern "C" fn included_start(_lexer: *const Lexer) -> bool {
    false
}

unsafe extern "C" fn eof(lexer: *const Lexer) -> bool {
    // SAFETY: The native scanner only calls this while the Input is live.
    let input = unsafe { &*lexer.cast::<Input>() };
    input.offset >= input.points.len() || input.watchdog
}

// SAFETY: These declarations match both retained scanner shims and parser.h.
unsafe extern "C" {
    fn tree_sitter_fsharp_external_scanner_create() -> *mut c_void;
    fn tree_sitter_fsharp_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_fsharp_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid: *const bool,
    ) -> bool;
    fn tree_sitter_fsharp_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_fsharp_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
    fn tree_sitter_fsharp_signature_external_scanner_create() -> *mut c_void;
    fn tree_sitter_fsharp_signature_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_fsharp_signature_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid: *const bool,
    ) -> bool;
    fn tree_sitter_fsharp_signature_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_fsharp_signature_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner {
    profile: Profile,
    payload: NonNull<c_void>,
}

#[repr(align(16))]
struct GuardedBuffer([u8; 1088]);

impl Scanner {
    fn new(profile: Profile) -> Self {
        let _: tree_sitter::Language = match profile {
            Profile::Implementation => tree_sitter_fsharp::LANGUAGE_FSHARP,
            Profile::Signature => tree_sitter_fsharp::LANGUAGE_SIGNATURE,
        }
        .into();
        // SAFETY: Construction has no arguments; this owner calls the matching destructor once.
        let pointer = unsafe {
            match profile {
                Profile::Implementation => tree_sitter_fsharp_external_scanner_create(),
                Profile::Signature => tree_sitter_fsharp_signature_external_scanner_create(),
            }
        };
        Self {
            profile,
            payload: NonNull::new(pointer).expect("native scanner allocation"),
        }
    }

    fn scan(&mut self, text: &str, column: u32, valid: [bool; TOKEN_COUNT]) -> (bool, Input) {
        let mut input = Input::new(text, column);
        // SAFETY: The opaque owner, complete 53-flag array, Input and non-panicking
        // callbacks are live for this call. The scanner does not retain lexer pointers.
        let found = unsafe {
            match self.profile {
                Profile::Implementation => tree_sitter_fsharp_external_scanner_scan(
                    self.payload.as_ptr(),
                    &mut input.lexer,
                    valid.as_ptr(),
                ),
                Profile::Signature => tree_sitter_fsharp_signature_external_scanner_scan(
                    self.payload.as_ptr(),
                    &mut input.lexer,
                    valid.as_ptr(),
                ),
            }
        };
        assert!(
            !input.watchdog,
            "{:?}: scanner exceeded its input-derived callback bound",
            self.profile
        );
        assert!(
            !input.advanced_past_end,
            "{:?}: native advance attempted beyond true EOF",
            self.profile
        );
        assert!(input.offset <= input.points.len());
        assert!(input.byte_offset <= input.byte_length);
        assert!(input.token_start <= input.byte_length);
        assert!(input.inspected_end <= input.byte_length);
        if let Some(marked) = input.marked {
            assert!(marked <= input.byte_length);
        }
        (found, input)
    }

    fn snapshot_at(&mut self, offset: usize) -> Vec<u8> {
        assert!(offset < 16);
        let mut guarded = GuardedBuffer([0xa5; 1088]);
        let start = 16 + offset;
        assert_eq!((guarded.0[start..].as_ptr() as usize) % 16, offset);
        // SAFETY: Each address has a live writable 1024-byte ABI buffer, including
        // deliberately unaligned addresses; guard bytes are outside that buffer.
        let length = unsafe {
            match self.profile {
                Profile::Implementation => tree_sitter_fsharp_external_scanner_serialize(
                    self.payload.as_ptr(),
                    guarded.0[start..].as_mut_ptr().cast(),
                ),
                Profile::Signature => tree_sitter_fsharp_signature_external_scanner_serialize(
                    self.payload.as_ptr(),
                    guarded.0[start..].as_mut_ptr().cast(),
                ),
            }
        };
        let length = usize::try_from(length).unwrap();
        assert!(
            length <= SNAPSHOT_CAPACITY,
            "{:?}: oversize snapshot",
            self.profile
        );
        assert!(guarded.0[..start].iter().all(|byte| *byte == 0xa5));
        assert!(guarded.0[start + length..].iter().all(|byte| *byte == 0xa5));
        guarded.0[start..start + length].to_vec()
    }

    fn snapshot(&mut self) -> Vec<u8> {
        self.snapshot_at(0)
    }

    fn restore(&mut self, bytes: &[u8]) {
        assert!(bytes.len() <= SNAPSHOT_CAPACITY);
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The input covers exactly the advertised length. A null pointer
        // is supplied only for the scanner's documented zero-length reset.
        unsafe {
            match self.profile {
                Profile::Implementation => tree_sitter_fsharp_external_scanner_deserialize(
                    self.payload.as_ptr(),
                    pointer,
                    c_uint::try_from(bytes.len()).unwrap(),
                ),
                Profile::Signature => tree_sitter_fsharp_signature_external_scanner_deserialize(
                    self.payload.as_ptr(),
                    pointer,
                    c_uint::try_from(bytes.len()).unwrap(),
                ),
            }
        };
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This unique owner frees its allocation through the original profile allocator.
        unsafe {
            match self.profile {
                Profile::Implementation => {
                    tree_sitter_fsharp_external_scanner_destroy(self.payload.as_ptr())
                }
                Profile::Signature => {
                    tree_sitter_fsharp_signature_external_scanner_destroy(self.payload.as_ptr())
                }
            }
        };
    }
}

fn flags(tokens: &[usize]) -> [bool; TOKEN_COUNT] {
    let mut valid = [false; TOKEN_COUNT];
    for token in tokens {
        valid[*token] = true;
    }
    valid
}

fn assert_dot(scanner: &mut Scanner, source: &str, column: u32, tokens: &[usize]) -> Input {
    let before = scanner.snapshot();
    let (found, input) = scanner.scan(source, column, flags(tokens));
    assert!(
        found,
        "{:?}: declined {source:?}, {tokens:?}",
        scanner.profile
    );
    assert_eq!(
        input.lexer.result_symbol,
        u16::try_from(QUALIFIED_CALLEE_DOT).unwrap()
    );
    let dot = source.bytes().position(|byte| byte == b'.').unwrap();
    assert_eq!(input.accepted_span(found), Some(dot..dot + 1), "{source:?}");
    assert_eq!(input.marked, Some(dot + 1), "{source:?}");
    assert_eq!(input.token_start, dot, "{source:?}");
    assert!(
        input.inspected_end > dot + 1,
        "{source:?}: no right lookahead"
    );
    assert_eq!(
        scanner.snapshot(),
        before,
        "{source:?}: stateless DOT changed native state"
    );
    input
}

fn open_scope(scanner: &mut Scanner, token: usize, column: u32) {
    let (found, input) = scanner.scan("item", column, flags(&[token]));
    assert!(found);
    assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
    assert_eq!(input.accepted_span(found), Some(0..0));
}

fn native_donors(profile: Profile) -> Vec<Vec<u8>> {
    let mut scanner = Scanner::new(profile);
    let mut donors = vec![scanner.snapshot()];
    open_scope(&mut scanner, INDENT, 4);
    donors.push(scanner.snapshot());
    // A midline anchor would retain the 8-column continuation instead of closing.
    let (found, input) = scanner.scan("\n            item", 0, flags(&[INDENT]));
    assert!(found);
    assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
    assert_eq!(input.accepted_span(found), Some(0..0));
    let (found, input) = scanner.scan("\n        item", 0, flags(&[DEDENT]));
    assert!(found);
    assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
    assert_eq!(input.accepted_span(found), Some(0..0));
    let stranded_dedent = scanner.snapshot();
    assert_eq!(
        stranded_dedent.get(4),
        Some(&1),
        "native donor must expose pending stranded dedent"
    );
    donors.push(stranded_dedent);
    let (found, input) = scanner.scan("\n        item", 0, flags(&[NEWLINE]));
    assert!(found);
    assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
    assert_eq!(input.accepted_span(found), Some(0..0));
    let stranded_line = scanner.snapshot();
    assert_eq!(
        stranded_line.get(5),
        Some(&1),
        "native donor must expose stranded line"
    );
    donors.push(stranded_line);
    for donor in &donors {
        assert_eq!(donor.get(..2), Some(&b"F\x02"[..]));
    }
    donors
}

fn assert_old_priority(
    profile: Profile,
    donor: &[u8],
    source: &str,
    column: u32,
    ordinary: &[usize],
    expected: usize,
) {
    let mut old = Scanner::new(profile);
    old.restore(donor);
    let (old_found, old_input) = old.scan(source, column, flags(ordinary));
    assert!(old_found);
    assert_eq!(
        old_input.lexer.result_symbol,
        u16::try_from(expected).unwrap()
    );
    let expected_state = old.snapshot();
    let mut with_dot = ordinary.to_vec();
    with_dot.push(QUALIFIED_CALLEE_DOT);
    let mut mixed = Scanner::new(profile);
    mixed.restore(donor);
    let (mixed_found, mixed_input) = mixed.scan(source, column, flags(&with_dot));
    assert_eq!(
        mixed_found, old_found,
        "{profile:?}: {source:?}, {ordinary:?}"
    );
    assert_eq!(
        mixed_input.lexer.result_symbol,
        old_input.lexer.result_symbol
    );
    assert_ne!(
        mixed_input.lexer.result_symbol,
        u16::try_from(QUALIFIED_CALLEE_DOT).unwrap()
    );
    assert_eq!(
        mixed_input.marked, old_input.marked,
        "{source:?}: older mark moved"
    );
    assert_eq!(
        mixed_input.accepted_span(mixed_found),
        old_input.accepted_span(old_found)
    );
    assert_eq!(
        mixed.snapshot(),
        expected_state,
        "{source:?}: older transition changed"
    );
}

#[test]
fn skip_aware_model_preserves_utf8_width_and_start_backward_finish() {
    let mut input = Input::new(" \u{200b}.x", 0);
    // SAFETY: These local calls supply the first field of the live exclusive Input.
    unsafe {
        advance(&mut input.lexer, true);
        advance(&mut input.lexer, false);
        mark_end(&mut input.lexer);
        advance(&mut input.lexer, true);
    }
    assert_eq!(input.byte_offset, 5);
    assert_eq!(input.token_start, 5);
    assert_eq!(input.marked, Some(4));
    assert_eq!(input.accepted_span(true), Some(4..4));
    assert_eq!(input.accepted_span(false), None);
    assert_eq!(input.inspected_end, 6);
    let mut dot = Input::new(".(+)", 0);
    // SAFETY: The callback pointers cannot outlive these local owned Inputs.
    unsafe {
        advance(&mut dot.lexer, false);
        mark_end(&mut dot.lexer);
        advance(&mut dot.lexer, false);
    }
    assert_eq!(dot.accepted_span(true), Some(0..1));
    assert_eq!(dot.byte_offset, 2);
    assert_eq!(dot.skips, 0);
}

#[test]
fn q_only_keeps_one_byte_dot_at_native_columns_and_after_padding() {
    for profile in PROFILES {
        for column in COLUMNS {
            for padding in ["", " ", "\t", "\n    ", "\r    ", "\r\n    "] {
                let mut scanner = Scanner::new(profile);
                let source = format!("{padding}.(+)");
                let input = assert_dot(&mut scanner, &source, column, &[QUALIFIED_CALLEE_DOT]);
                assert_eq!(input.skips, padding.chars().count(), "{source:?}");
                assert!(input.byte_offset > padding.len() + 1);
            }
        }
    }
}

#[test]
fn right_trivia_is_inspected_but_not_consumed_by_dot() {
    for profile in PROFILES {
        for source in [
            ". // c\n(+)",
            ". // c\r(+)",
            ". // c\r\n(+)",
            ". /// documentation\r(+)",
            ". //// comment\r\n(+)",
            ".(* comment *)(+)",
            ".(* outer (* inner *) c *)(|Hit|Miss|)",
            ".(* c *) // line\r\n(* next *)(+)",
            ".\u{200b}(+)",
            ".\u{2060}(+)",
            ".\u{feff}(+)",
            ".\t\u{b}\u{c}\r\n(+)",
        ] {
            for tokens in [
                vec![QUALIFIED_CALLEE_DOT],
                vec![QUALIFIED_CALLEE_DOT, APPLICATION_ARG_START, NEWLINE, DEDENT],
            ] {
                let mut scanner = Scanner::new(profile);
                let input = assert_dot(&mut scanner, source, 64, &tokens);
                assert_eq!(input.skips, 0, "{source:?}: right trivia moved token start");
            }
        }
    }
}

#[test]
fn complete_operators_outrank_overlapping_block_comment_spellings() {
    for profile in PROFILES {
        for source in [
            ".(+)",
            ".(*)",
            ".(**)",
            ".( ** )",
            ".( * )",
            ".(?)",
            ".($)",
            ".($+)",
            ".(%&&)",
            ".(<=>)",
            ".(.+)",
            ".(.. ..)",
            ".( .. .. )",
            ".(\t+\t)",
            ".(\n+\n)",
            ".(\r\n+\r\n)",
            ".(\u{b}+\u{c})",
        ] {
            for tokens in [
                vec![QUALIFIED_CALLEE_DOT],
                vec![QUALIFIED_CALLEE_DOT, APPLICATION_ARG_START],
            ] {
                assert_dot(&mut Scanner::new(profile), source, 64, &tokens);
            }
        }
    }
}

#[test]
fn nonreference_and_unclosed_tails_have_no_committed_external_token() {
    for profile in PROFILES {
        for source in [
            ".",
            ".Name",
            "..(+)",
            ".()",
            ".(value)",
            ".(,)",
            ".(:)",
            ".(+$)",
            ".(+",
            ".( *",
            ".(* unfinished",
            ".(* c *)(value)",
            ".// c",
            ".#if X\n(+)",
        ] {
            for tokens in [
                vec![QUALIFIED_CALLEE_DOT],
                vec![QUALIFIED_CALLEE_DOT, APPLICATION_ARG_START, NEWLINE],
            ] {
                let mut scanner = Scanner::new(profile);
                let before = scanner.snapshot();
                let (found, input) = scanner.scan(source, 64, flags(&tokens));
                assert!(!found, "{profile:?}: accepted {source:?}, {tokens:?}");
                assert_ne!(
                    input.lexer.result_symbol,
                    u16::try_from(QUALIFIED_CALLEE_DOT).unwrap()
                );
                assert_eq!(input.accepted_span(found), None);
                assert!(
                    input.byte_offset > 0,
                    "{source:?}: failed peek was not exercised"
                );
                assert_eq!(
                    scanner.snapshot(),
                    before,
                    "{source:?}: failed Q peek changed state"
                );
            }
        }
    }
}

#[test]
fn active_pattern_head_classification_does_not_assert_syntax_completeness() {
    for profile in PROFILES {
        for source in [".(|Hit|Miss|)", ".(|Hit|_|)", ".(|", ".(|Hit"] {
            assert_dot(
                &mut Scanner::new(profile),
                source,
                64,
                &[QUALIFIED_CALLEE_DOT],
            );
        }
    }
}

#[test]
fn mixed_layout_and_application_requests_retain_stateless_dot() {
    for profile in PROFILES {
        for tokens in [
            vec![QUALIFIED_CALLEE_DOT, NEWLINE],
            vec![QUALIFIED_CALLEE_DOT, DEDENT],
            vec![QUALIFIED_CALLEE_DOT, APPLICATION_ARG_START],
            vec![QUALIFIED_CALLEE_DOT, NEWLINE, DEDENT, APPLICATION_ARG_START],
        ] {
            for padding in ["", "  ", "\n    ", "\r\n    "] {
                let source = format!("{padding}.(+)");
                let input = assert_dot(&mut Scanner::new(profile), &source, 64, &tokens);
                assert_eq!(input.skips, padding.chars().count());
            }
        }
    }
}

#[test]
fn q_never_wins_in_recovery_inside_string_or_type_app_contexts() {
    for profile in PROFILES {
        for source in [".(+)", ".(*)", ".(|Hit|Miss|)", ".// c\r(+)"] {
            for valid in [
                flags(&[QUALIFIED_CALLEE_DOT, ERROR_SENTINEL]),
                flags(&[QUALIFIED_CALLEE_DOT, INSIDE_STRING]),
                flags(&[QUALIFIED_CALLEE_DOT, TYPE_APP_INDENT]),
                flags(&[NEWLINE]),
                [true; TOKEN_COUNT],
            ] {
                let mut scanner = Scanner::new(profile);
                let (found, input) = scanner.scan(source, 64, valid);
                assert_ne!(
                    input.lexer.result_symbol,
                    u16::try_from(QUALIFIED_CALLEE_DOT).unwrap()
                );
                if !found {
                    assert_eq!(input.accepted_span(found), None);
                }
            }
        }
    }
}

#[test]
fn all_valid_recovery_keeps_native_eof_dedent_with_q_enabled() {
    for profile in PROFILES {
        let mut scanner = Scanner::new(profile);
        open_scope(&mut scanner, PAREN_INDENT, 8);
        let before = scanner.snapshot();
        let (found, input) = scanner.scan("", 0, [true; TOKEN_COUNT]);
        assert!(found);
        assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
        assert_eq!(input.accepted_span(found), Some(0..0));
        assert_ne!(scanner.snapshot(), before);
    }
}

#[test]
fn older_indent_tokens_keep_priority_state_and_zero_width_spans() {
    for profile in PROFILES {
        let donor = Scanner::new(profile).snapshot();
        for token in [
            INDENT,
            PAREN_INDENT,
            TRY_INDENT,
            BRACE_INDENT,
            DO_BODY_INDENT,
            ROOT_BODY_INDENT,
        ] {
            for source in [".(+)", " \t.(+)", "\n    .(+)", "\r\n    .(+)"] {
                assert_old_priority(profile, &donor, source, 0, &[token], token);
                let mut scanner = Scanner::new(profile);
                scanner.restore(&donor);
                let (found, input) = scanner.scan(source, 0, flags(&[token, QUALIFIED_CALLEE_DOT]));
                assert_eq!(input.marked, Some(0));
                assert_eq!(input.accepted_span(found), Some(0..0));
                assert!(input.token_start >= input.marked.unwrap());
            }
        }
    }
}

#[test]
fn older_unaligned_newline_retains_its_frontier_before_pending_dot() {
    for profile in PROFILES {
        let donor = Scanner::new(profile).snapshot();
        for source in ["\n    .(+)", "\r\n    .(+)"] {
            assert_old_priority(
                profile,
                &donor,
                source,
                0,
                &[NEWLINE_NO_ALIGNED],
                NEWLINE_NO_ALIGNED,
            );
            let mut scanner = Scanner::new(profile);
            let (found, input) = scanner.scan(
                source,
                0,
                flags(&[NEWLINE_NO_ALIGNED, QUALIFIED_CALLEE_DOT]),
            );
            assert_eq!(input.accepted_span(found), Some(0..0));
        }
    }
}

#[test]
fn older_comment_content_and_closing_layout_keep_priority() {
    for profile in PROFILES {
        let donor = Scanner::new(profile).snapshot();
        assert_old_priority(
            profile,
            &donor,
            "body*)",
            0,
            &[BLOCK_COMMENT_CONTENT],
            BLOCK_COMMENT_CONTENT,
        );
        let mut scoped = Scanner::new(profile);
        open_scope(&mut scoped, PAREN_INDENT, 8);
        assert_old_priority(profile, &scoped.snapshot(), ")", 0, &[DEDENT], DEDENT);
    }
}

#[test]
fn q_true_and_false_preserve_native_stranded_flags_and_codec() {
    for profile in PROFILES {
        for donor in native_donors(profile) {
            for tokens in [
                vec![QUALIFIED_CALLEE_DOT],
                vec![QUALIFIED_CALLEE_DOT, APPLICATION_ARG_START],
            ] {
                for source in [".(+)", ".Name", ".// c\r(+)"] {
                    let mut scanner = Scanner::new(profile);
                    scanner.restore(&donor);
                    assert_eq!(scanner.snapshot_at(1), donor);
                    let (found, input) = scanner.scan(source, 257, flags(&tokens));
                    assert_eq!(found, source != ".Name");
                    assert_eq!(
                        scanner.snapshot(),
                        donor,
                        "{profile:?}: {source:?}, {tokens:?}"
                    );
                    assert_eq!(
                        input.accepted_span(found),
                        if found { Some(0..1) } else { None }
                    );
                }
            }
        }
    }
}

#[test]
fn q_decisions_replay_identically_after_cross_factory_snapshot_restore() {
    for donor_profile in PROFILES {
        for donor in native_donors(donor_profile) {
            for source in [".(**)", ".(|Hit|Miss|)", ".(* c *)// next\r(+)", ".(value)"] {
                let mut reference = Scanner::new(donor_profile);
                reference.restore(&donor);
                let (expected, first) = reference.scan(source, 257, flags(&[QUALIFIED_CALLEE_DOT]));
                assert_eq!(reference.snapshot(), donor);
                for profile in PROFILES {
                    let mut restored = Scanner::new(profile);
                    restored.restore(&donor);
                    assert_eq!(restored.snapshot_at(1), donor);
                    let (found, input) = restored.scan(source, 257, flags(&[QUALIFIED_CALLEE_DOT]));
                    assert_eq!(found, expected);
                    assert_eq!(input.lexer.result_symbol, first.lexer.result_symbol);
                    assert_eq!(input.accepted_span(found), first.accepted_span(expected));
                    assert_eq!(input.byte_offset, first.byte_offset);
                    assert_eq!(input.token_start, first.token_start);
                    assert_eq!(input.marked, first.marked);
                    assert_eq!(input.inspected_end, first.inspected_end);
                    assert_eq!(input.skips, first.skips);
                    assert_eq!(restored.snapshot(), donor);
                }
            }
        }
    }
}
