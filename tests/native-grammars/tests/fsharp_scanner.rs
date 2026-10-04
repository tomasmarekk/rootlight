//! Direct F# scanner qualification without evaluating F# or assuming its wire layout.
//! Native-created indentation and original comment input exercise both retained factories.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

const TOKEN_COUNT: usize = 43;
const ENUM_CASE_BAR: usize = 40;
const INDENT: usize = 1;
const ERROR_SENTINEL: usize = 39;
const NEWLINE: usize = 0;
const DEDENT: usize = 2;
const PREPROC_IF: usize = 6;
const PREPROC_END: usize = 8;
const BLOCK_COMMENT_CONTENT: usize = 18;
const INSIDE_STRING: usize = 19;
const PAREN_INDENT: usize = 30;
const IN: usize = 33;
const PREPROC_INACTIVE: usize = 36;
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
    points: Vec<i32>,
    offset: usize,
    column: u32,
    marked: Option<usize>,
    advances: usize,
    budget: usize,
    advanced_past_end: bool,
    watchdog: bool,
}

unsafe extern "C" fn advance(lexer: *mut Lexer, _skip: bool) {
    // SAFETY: The callback receives the first field of this live exclusive Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.advances = input.advances.saturating_add(1);
    if input.advances > input.budget {
        input.watchdog = true;
        input.lexer.lookahead = 0;
        return;
    }
    if input.offset >= input.points.len() {
        input.advanced_past_end = true;
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
    // SAFETY: No callback outlives the owning Input or retains its address.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = Some(input.offset);
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
        let points: Vec<_> = text
            .chars()
            .map(|ch| i32::try_from(u32::from(ch)).unwrap())
            .collect();
        let budget = points.len().saturating_mul(8).saturating_add(64);
        let mut input = Input {
            lexer: Lexer {
                lookahead: points.first().copied().unwrap_or(0),
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
            column,
            marked: None,
            advances: 0,
            budget,
            advanced_past_end: false,
            watchdog: false,
        };
        // SAFETY: The opaque owner, complete 43-flag array, Input and non-panicking
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
        if let Some(marked) = input.marked {
            assert!(marked <= input.points.len());
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

fn open_paren(scanner: &mut Scanner, column: u32) {
    let (found, input) = scanner.scan("item", column, flags(&[PAREN_INDENT]));
    assert!(found, "{:?}: column {column}", scanner.profile);
    assert_eq!(
        input.lexer.result_symbol,
        u16::try_from(PAREN_INDENT).unwrap()
    );
    assert_eq!(input.marked, Some(0));
    assert_eq!(input.offset, 0);
}

fn assert_sticky_poison(scanner: &mut Scanner, poison: &[u8]) {
    for (text, column, valid) in [
        ("item", 257, flags(&[PAREN_INDENT])),
        ("in", 0, flags(&[IN])),
        ("content*)", 0, flags(&[BLOCK_COMMENT_CONTENT])),
        ("", 0, flags(&[DEDENT])),
        ("item", 0, [true; TOKEN_COUNT]),
    ] {
        let (found, input) = scanner.scan(text, column, valid);
        assert!(
            !found,
            "{:?}: poisoned state emitted a token",
            scanner.profile
        );
        assert_eq!(input.lexer.result_symbol, u16::MAX);
        assert_eq!(input.offset, 0);
        assert_eq!(input.advances, 0);
        assert_eq!(input.marked, None);
        assert_eq!(scanner.snapshot(), poison);
    }
}

fn pop_base(scanner: &mut Scanner) {
    let (found, input) = scanner.scan("in", 0, flags(&[IN]));
    assert!(
        found,
        "{:?}: native IN did not pop its level",
        scanner.profile
    );
    assert_eq!(input.lexer.result_symbol, u16::try_from(IN).unwrap());
    assert_eq!(input.offset, 2);
    assert_eq!(input.marked, Some(2));
}

fn assert_round_trip_behavior(profile: Profile, column: u32, snapshot: &[u8]) {
    for ending in ["\n", "\r\n"] {
        let text = format!(
            "{ending}{}item",
            " ".repeat(usize::try_from(column).unwrap())
        );
        let mut original = Scanner::new(profile);
        open_paren(&mut original, column);
        let mut restored = Scanner::new(profile);
        restored.restore(snapshot);
        for scanner in [&mut original, &mut restored] {
            let (found, input) = scanner.scan(&text, 0, flags(&[NEWLINE]));
            assert!(
                found,
                "{profile:?}: restored aligned column {column}, {ending:?}"
            );
            assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, text.len() - "item".len());
            let (found, input) = scanner.scan(")", column, flags(&[DEDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, 0);
            assert_eq!(scanner.snapshot(), Scanner::new(profile).snapshot());
        }
    }
}

fn open_enum_body(scanner: &mut Scanner, column: u32) {
    let (found, input) = scanner.scan("First", column, flags(&[INDENT]));
    assert!(found, "{:?}: normal enum body at {column}", scanner.profile);
    assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
    assert_eq!(input.marked, Some(0));
    assert_eq!(input.offset, 0);
}

#[test]
fn written_enum_bars_use_bounded_offside_grace_without_changing_the_snapshot() {
    for profile in PROFILES {
        for newline in ["\n", "\r\n"] {
            for column in [4_usize, 5, 6, 8] {
                for next in [
                    " Next = 2",
                    "Next = 2",
                    "``Next`` = 2",
                    "λ = 2",
                    "(* gap *) Next = 2",
                ] {
                    let source = format!("{newline}{}|{next}", " ".repeat(column));
                    let end = source.find('|').unwrap() + 1;
                    let mut original = Scanner::new(profile);
                    open_enum_body(&mut original, 6);
                    let snapshot = original.snapshot();
                    let mut restored = Scanner::new(profile);
                    restored.restore(&snapshot);
                    for scanner in [&mut original, &mut restored] {
                        let (found, input) =
                            scanner.scan(&source, 0, flags(&[ENUM_CASE_BAR, DEDENT]));
                        assert!(found, "{profile:?}: {source:?}");
                        assert_eq!(
                            input.lexer.result_symbol,
                            u16::try_from(ENUM_CASE_BAR).unwrap()
                        );
                        assert_eq!(input.offset, end);
                        assert_eq!(input.marked, Some(end));
                        assert_eq!(scanner.snapshot(), snapshot);
                    }
                }
            }
        }
    }
}

#[test]
fn enum_bar_frontiers_do_not_claim_operator_recovery_or_excessive_offside() {
    for profile in PROFILES {
        for newline in ["\n", "\r\n"] {
            for next in ["> value", "| value", ". value", "+ value"] {
                let source = format!("{newline}    |{next}");
                let mut scanner = Scanner::new(profile);
                open_enum_body(&mut scanner, 6);
                let before = scanner.snapshot();
                let (found, input) = scanner.scan(&source, 0, flags(&[ENUM_CASE_BAR]));
                assert!(!found, "{profile:?}: operator {source:?}");
                assert_ne!(
                    input.lexer.result_symbol,
                    u16::try_from(ENUM_CASE_BAR).unwrap()
                );
                assert_eq!(scanner.snapshot(), before);
            }
            let mut scanner = Scanner::new(profile);
            open_enum_body(&mut scanner, 6);
            let reset = Scanner::new(profile).snapshot();
            let (found, input) = scanner.scan(
                &format!("{newline}   | Next = 2"),
                0,
                flags(&[ENUM_CASE_BAR, DEDENT]),
            );
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(0));
            assert_eq!(scanner.snapshot(), reset);
            for recovery in [false, true] {
                let mut scanner = Scanner::new(profile);
                open_enum_body(&mut scanner, 6);
                let before = scanner.snapshot();
                let tokens = if recovery {
                    flags(&[ENUM_CASE_BAR, ERROR_SENTINEL])
                } else {
                    flags(&[PREPROC_INACTIVE])
                };
                let (found, input) = scanner.scan(&format!("{newline}    | Next = 2"), 0, tokens);
                assert_ne!(
                    input.lexer.result_symbol,
                    u16::try_from(ENUM_CASE_BAR).unwrap()
                );
                if recovery {
                    assert!(found);
                    assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                    assert_eq!(scanner.snapshot(), reset);
                } else {
                    assert!(!found);
                    assert_eq!(scanner.snapshot(), before);
                }
            }
        }
    }
}

#[test]
fn recovery_bar_uses_the_existing_normal_offside_closure() {
    for profile in PROFILES {
        for newline in ["\n", "\r\n"] {
            let mut scanner = Scanner::new(profile);
            open_enum_body(&mut scanner, 6);
            let reset = Scanner::new(profile).snapshot();
            let (found, input) = scanner.scan(
                &format!("{newline}    | Next = 2"),
                0,
                flags(&[ERROR_SENTINEL]),
            );
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(0));
            assert_eq!(scanner.snapshot(), reset);
        }
    }
}

#[test]
fn both_profiles_preserve_distinct_native_column_states_and_restored_tokens() {
    for profile in PROFILES {
        let mut snapshots = Vec::new();
        for column in COLUMNS {
            let mut scanner = Scanner::new(profile);
            open_paren(&mut scanner, column);
            let snapshot = scanner.snapshot();
            assert!(
                !snapshots.iter().any(|prior| prior == &snapshot),
                "{profile:?}: column {column} aliases an earlier native state"
            );
            assert_round_trip_behavior(profile, column, &snapshot);
            snapshots.push(snapshot);
        }
    }
}

#[test]
fn aligned_and_unaligned_snapshot_storage_is_bounded_and_byte_exact() {
    for profile in PROFILES {
        for column in COLUMNS {
            let mut scanner = Scanner::new(profile);
            open_paren(&mut scanner, column);
            let expected = scanner.snapshot();
            for offset in [0, 1, 15] {
                let observed = scanner.snapshot_at(offset);
                assert_eq!(observed, expected);
                let mut restored = Scanner::new(profile);
                restored.restore(&observed);
                assert_eq!(restored.snapshot_at(offset), expected);
            }
        }
    }
}

#[test]
fn zero_length_reset_discards_all_native_indentation_state() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        for column in COLUMNS {
            let mut scanner = Scanner::new(profile);
            open_paren(&mut scanner, column);
            scanner.restore(&[]);
            assert_eq!(scanner.snapshot(), reset);
            let (found, input) = scanner.scan("\nitem", 0, flags(&[NEWLINE, DEDENT]));
            assert!(!found, "{profile:?}: reset fabricated an indentation token");
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(scanner.snapshot(), reset);
        }
    }
}

#[test]
fn truncated_extra_and_hostile_snapshots_reset_instead_of_publishing_partial_state() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut donor = Scanner::new(profile);
        open_paren(&mut donor, 257);
        open_paren(&mut donor, 65_536);
        let complete = donor.snapshot();
        assert!(complete.len() < SNAPSHOT_CAPACITY);
        let mut scanner = Scanner::new(profile);
        for length in 1..complete.len() {
            open_paren(&mut scanner, 257);
            scanner.restore(&complete[..length]);
            assert_eq!(
                scanner.snapshot(),
                reset,
                "{profile:?}: accepted truncated length {length}"
            );
        }
        for suffix in [0, 0xff] {
            let mut extra = complete.clone();
            extra.push(suffix);
            open_paren(&mut scanner, 257);
            scanner.restore(&extra);
            assert_eq!(
                scanner.snapshot(),
                reset,
                "{profile:?}: accepted trailing snapshot data"
            );
        }
        for byte in [0x5a, 0xff] {
            for length in 1..=SNAPSHOT_CAPACITY {
                open_paren(&mut scanner, 257);
                scanner.restore(&vec![byte; length]);
                assert_eq!(
                    scanner.snapshot(),
                    reset,
                    "{profile:?}: accepted hostile byte {byte}, length {length}"
                );
            }
        }
    }
}

#[test]
fn native_stack_capacity_declines_before_emission_and_restores_sticky_poison() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut scanner = Scanner::new(profile);
        let mut last_representable = reset.clone();
        let mut admitted = 0;
        let mut declined = false;
        // Native-created levels must reach the fixed ABI capacity within this
        // bound; neither their private encoding nor a guessed struct is supplied.
        for _ in 0..SNAPSHOT_CAPACITY {
            let (found, input) = scanner.scan("item", 257, flags(&[PAREN_INDENT]));
            assert_eq!(input.offset, 0);
            assert_eq!(input.advances, 0);
            assert_eq!(input.marked, Some(0));
            if !found {
                assert_eq!(input.lexer.result_symbol, u16::MAX);
                declined = true;
                break;
            }
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(PAREN_INDENT).unwrap()
            );
            admitted += 1;
            last_representable = scanner.snapshot();
        }
        assert!(admitted > 0, "{profile:?}: no native level was admitted");
        assert!(
            declined,
            "{profile:?}: capacity never declined within the ABI bound"
        );
        let poison = scanner.snapshot();
        assert!(!poison.is_empty());
        assert_ne!(poison, reset);
        assert_ne!(poison, last_representable);
        assert_eq!(scanner.snapshot_at(1), poison);
        assert_eq!(scanner.snapshot_at(15), poison);
        let mut restored = Scanner::new(profile);
        restored.restore(&poison);
        assert_eq!(restored.snapshot(), poison);
        for current in [&mut scanner, &mut restored] {
            assert_sticky_poison(current, &poison);
            current.restore(&[]);
            assert_eq!(current.snapshot(), reset);
            open_paren(current, 257);
            assert_ne!(current.snapshot(), reset);
        }
    }
}

#[test]
fn native_empty_stack_and_nondefault_first_level_round_trip_without_a_base() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut donor = Scanner::new(profile);
        pop_base(&mut donor);
        let empty = donor.snapshot();
        assert!(!empty.is_empty());
        assert_ne!(empty, reset);
        let mut restored_empty = Scanner::new(profile);
        restored_empty.restore(&empty);
        assert_eq!(restored_empty.snapshot(), empty);
        open_paren(&mut donor, 257);
        open_paren(&mut restored_empty, 257);
        let first_level = donor.snapshot();
        assert_eq!(restored_empty.snapshot(), first_level);
        assert_ne!(first_level, empty);
        assert_ne!(first_level, reset);
        let mut ordinary = Scanner::new(profile);
        open_paren(&mut ordinary, 257);
        assert_ne!(first_level, ordinary.snapshot());
        for ending in ["\n", "\r\n"] {
            let text = format!("{ending}{}item", " ".repeat(257));
            let mut original = Scanner::new(profile);
            pop_base(&mut original);
            open_paren(&mut original, 257);
            let mut restored = Scanner::new(profile);
            restored.restore(&first_level);
            for scanner in [&mut original, &mut restored] {
                let (found, input) = scanner.scan(&text, 0, flags(&[NEWLINE]));
                assert!(found, "{profile:?}: first-level newline, {ending:?}");
                assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
                assert_eq!(input.marked, Some(0));
                assert_eq!(input.offset, text.len() - "item".len());
                let (found, input) = scanner.scan(")", 257, flags(&[DEDENT]));
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                assert_eq!(input.marked, Some(0));
                assert_eq!(input.offset, 0);
                assert_eq!(scanner.snapshot(), empty);
                scanner.restore(&[]);
                assert_eq!(scanner.snapshot(), reset);
            }
        }
    }
}

#[test]
fn native_preprocessor_kinds_restore_independently_of_numeric_levels() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut stray = Scanner::new(profile);
        let source = "#if condition\nitem";
        let (found, input) = stray.scan(source, 0, flags(&[PREPROC_INACTIVE]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(PREPROC_INACTIVE).unwrap()
        );
        assert_eq!(input.offset, source.find('\n').unwrap());
        assert_eq!(input.marked, Some(input.offset));
        let stray_state = stray.snapshot();
        assert_ne!(stray_state, reset);
        let mut restored_stray = Scanner::new(profile);
        restored_stray.restore(&stray_state);
        assert_eq!(restored_stray.snapshot(), stray_state);
        for scanner in [&mut stray, &mut restored_stray] {
            let (found, input) = scanner.scan("#endif", 0, flags(&[PREPROC_INACTIVE]));
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(PREPROC_INACTIVE).unwrap()
            );
            assert_eq!(input.offset, "#endif".len());
            assert_eq!(input.marked, Some("#endif".len()));
            assert_eq!(scanner.snapshot(), reset);
        }
        let mut structured = Scanner::new(profile);
        pop_base(&mut structured);
        let empty = structured.snapshot();
        let (found, input) = structured.scan("#if condition", 0, flags(&[PREPROC_IF]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(PREPROC_IF).unwrap()
        );
        assert_eq!(input.offset, "#if".len());
        assert_eq!(input.marked, Some("#if".len()));
        let structured_state = structured.snapshot();
        assert_ne!(structured_state, empty);
        assert_ne!(structured_state, stray_state);
        let mut restored_structured = Scanner::new(profile);
        restored_structured.restore(&structured_state);
        assert_eq!(restored_structured.snapshot(), structured_state);
        for scanner in [&mut structured, &mut restored_structured] {
            let (found, input) = scanner.scan("#endif", 0, flags(&[PREPROC_END]));
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(PREPROC_END).unwrap()
            );
            assert_eq!(input.offset, "#endif".len());
            assert_eq!(input.marked, Some("#endif".len()));
            assert_eq!(scanner.snapshot(), empty);
        }
    }
}

#[test]
fn full_width_columns_round_trip_and_checked_whitespace_overflow_poisons() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut scanner = Scanner::new(profile);
        open_paren(&mut scanner, u32::MAX);
        let widest = scanner.snapshot();
        for column in COLUMNS {
            let mut narrower = Scanner::new(profile);
            open_paren(&mut narrower, column);
            assert_ne!(widest, narrower.snapshot());
        }
        let mut restored = Scanner::new(profile);
        restored.restore(&widest);
        assert_eq!(restored.snapshot_at(1), widest);
        for current in [&mut scanner, &mut restored] {
            // Closing the native level needs no allocation proportional to its column.
            let (found, input) = current.scan(")", u32::MAX, flags(&[DEDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.offset, 0);
            assert_eq!(input.marked, Some(0));
            assert_eq!(current.snapshot(), reset);
        }
        for (text, column) in [(" item", u32::MAX), ("\titem", u32::MAX - 7)] {
            let mut overflowing = Scanner::new(profile);
            let (found, input) = overflowing.scan(text, column, flags(&[PAREN_INDENT]));
            assert!(!found, "{profile:?}: overflow emitted an indentation token");
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(input.offset, 0);
            assert_eq!(input.advances, 0);
            assert_eq!(input.marked, Some(0));
            let poison = overflowing.snapshot();
            assert!(!poison.is_empty());
            assert_ne!(poison, reset);
            assert_ne!(poison, widest);
            let mut restored = Scanner::new(profile);
            restored.restore(&poison);
            assert_eq!(restored.snapshot(), poison);
            for current in [&mut overflowing, &mut restored] {
                assert_sticky_poison(current, &poison);
                current.restore(&[]);
                assert_eq!(current.snapshot(), reset);
                open_paren(current, 257);
            }
        }
    }
}

#[test]
fn nested_comment_content_stops_at_the_outer_delimiter_without_losing_source() {
    // The lexer starts after the grammar has shifted the outer `(*` opener.
    for profile in PROFILES {
        for ending in ["\n", "\r\n"] {
            for content in [
                "plain",
                "(* inner *)",
                "(* inner *) tail",
                "(* one (* two *) three *)",
                "parenthesis ( ordinary",
                "asterisk * ordinary",
                "(*a*)(*b*)",
            ] {
                let content = format!("{content}{ending}tail");
                let source = format!("{content}*)after");
                let mut scanner = Scanner::new(profile);
                let (found, input) = scanner.scan(&source, 0, flags(&[BLOCK_COMMENT_CONTENT]));
                assert!(found, "{profile:?}: {content:?}");
                assert_eq!(
                    input.lexer.result_symbol,
                    u16::try_from(BLOCK_COMMENT_CONTENT).unwrap()
                );
                assert_eq!(input.marked, Some(content.len()));
                assert_eq!(input.offset, content.len() + 1);
                assert_eq!(input.lexer.lookahead, i32::from(b')'));
            }
        }
        for content in ["", "(* inner *)", "(* one (* two *) three *)", "(*a*)(*b*)"] {
            let source = format!("{content}*)after");
            let mut scanner = Scanner::new(profile);
            let (found, input) = scanner.scan(&source, 0, flags(&[BLOCK_COMMENT_CONTENT]));
            assert!(found);
            assert_eq!(input.marked, Some(content.len()));
            assert_eq!(input.offset, content.len() + 1);
            assert_eq!(input.lexer.lookahead, i32::from(b')'));
        }
    }
}

#[test]
fn comment_eof_is_not_an_embedded_nul_and_string_controls_do_not_scan() {
    for profile in PROFILES {
        for content in ["a\0b", "\0", "(*a\0b*) tail"] {
            let source = format!("{content}*)after");
            let mut scanner = Scanner::new(profile);
            let (found, input) = scanner.scan(&source, 0, flags(&[BLOCK_COMMENT_CONTENT]));
            assert!(found);
            assert_eq!(input.marked, Some(content.len()));
            assert_eq!(input.offset, content.len() + 1);
            assert_eq!(input.lexer.lookahead, i32::from(b')'));
        }
        for text in [
            "",
            "unfinished",
            "(* unfinished",
            "(* one (* unfinished",
            "(* closed *)",
        ] {
            let mut scanner = Scanner::new(profile);
            let (found, input) = scanner.scan(text, 0, flags(&[BLOCK_COMMENT_CONTENT]));
            assert_eq!(
                input.offset,
                text.len(),
                "{profile:?}: truncated content did not reach true EOF"
            );
            assert_eq!(input.lexer.lookahead, 0);
            if found {
                // A content token is not proof of a closing `*)`; any published
                // partial content must nevertheless cover the original consumed bytes.
                assert_eq!(
                    input.lexer.result_symbol,
                    u16::try_from(BLOCK_COMMENT_CONTENT).unwrap()
                );
                assert_eq!(input.marked, Some(text.len()));
            }
        }
        for tokens in [
            vec![INSIDE_STRING],
            vec![INSIDE_STRING, BLOCK_COMMENT_CONTENT],
        ] {
            let mut scanner = Scanner::new(profile);
            let before = scanner.snapshot();
            let (found, input) = scanner.scan("(* nested *)", 0, flags(&tokens));
            assert!(!found);
            assert_eq!(input.offset, 0);
            assert_eq!(input.marked, None);
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(scanner.snapshot(), before);
        }
    }
}

const DO_BODY_INDENT: usize = 41;
const DONE_KEYWORD: usize = 42;
const BRACE_INDENT: usize = 38;

fn open_do_body(scanner: &mut Scanner, column: u32) {
    let (found, input) = scanner.scan("item", column, flags(&[DO_BODY_INDENT]));
    assert!(found, "{:?}: do body at {column}", scanner.profile);
    assert_eq!(
        input.lexer.result_symbol,
        u16::try_from(DO_BODY_INDENT).unwrap()
    );
    assert_eq!(input.marked, Some(0));
    assert_eq!(input.offset, 0);
}

fn expect_done_dedent(scanner: &mut Scanner, expected: &[u8]) {
    let (found, input) = scanner.scan("done", 16, flags(&[DEDENT]));
    assert!(
        found,
        "{:?}: nearest do body did not close",
        scanner.profile
    );
    assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
    assert_eq!(input.marked, Some(0));
    // The lexical peek advances, but DEDENT still ends at the untouched delimiter.
    assert_eq!(input.offset, 4);
    assert_eq!(scanner.snapshot(), expected);
}

#[test]
fn native_do_body_tags_round_trip_and_unknown_tags_reset() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        for column in [8_u32, 257] {
            for newline in ["", "\n", "\r\n"] {
                let source = if newline.is_empty() {
                    "item".to_owned()
                } else {
                    format!(
                        "{newline}{}item",
                        " ".repeat(usize::try_from(column).unwrap())
                    )
                };
                let start_column = if newline.is_empty() { column } else { 0 };
                let mut normal = Scanner::new(profile);
                let (found, input) = normal.scan(&source, start_column, flags(&[INDENT]));
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
                assert_eq!(input.marked, Some(0));
                let normal_state = normal.snapshot();
                let mut owned = Scanner::new(profile);
                let (found, input) = owned.scan(&source, start_column, flags(&[DO_BODY_INDENT]));
                assert!(found, "{profile:?}: {source:?}");
                assert_eq!(
                    input.lexer.result_symbol,
                    u16::try_from(DO_BODY_INDENT).unwrap()
                );
                assert_eq!(input.marked, Some(0));
                assert_eq!(input.offset, source.chars().count() - "item".len());
                let state = owned.snapshot();
                assert_ne!(state, reset);
                assert_eq!(state.len(), normal_state.len());
                let different: Vec<_> = state
                    .iter()
                    .zip(&normal_state)
                    .enumerate()
                    .filter_map(|(index, (owned, normal))| (owned != normal).then_some(index))
                    .collect();
                assert_eq!(
                    different.len(),
                    1,
                    "do ownership changed more than its native kind"
                );
                let mut restored = Scanner::new(profile);
                restored.restore(&state);
                assert_eq!(restored.snapshot_at(1), state);
                for scanner in [&mut owned, &mut restored] {
                    expect_done_dedent(scanner, &reset);
                }
                // Locate the native kind by comparison rather than authoring a wire frame.
                let mut unknown = state.clone();
                unknown[different[0]] = 0x3f;
                restored.restore(&unknown);
                assert_eq!(
                    restored.snapshot(),
                    reset,
                    "{profile:?}: unknown kind was admitted"
                );
            }
        }
    }
}

#[test]
fn nested_done_closes_body_frames_before_its_written_tail_without_popping_outer_owners() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut donor = Scanner::new(profile);
        open_do_body(&mut donor, 8);
        let outer = donor.snapshot();
        open_do_body(&mut donor, 12);
        let inner = donor.snapshot();
        open_enum_body(&mut donor, 16);
        let nested = donor.snapshot();
        let mut restored = Scanner::new(profile);
        restored.restore(&nested);
        for scanner in [&mut donor, &mut restored] {
            expect_done_dedent(scanner, &inner);
            expect_done_dedent(scanner, &outer);
            let (found, input) = scanner.scan("done next", 16, flags(&[DONE_KEYWORD, DEDENT]));
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(DONE_KEYWORD).unwrap()
            );
            assert_eq!(input.offset, 4);
            assert_eq!(input.marked, Some(4));
            assert_eq!(input.lexer.lookahead, i32::from(b' '));
            assert_eq!(
                scanner.snapshot(),
                outer,
                "written inner done popped its outer body"
            );
            expect_done_dedent(scanner, &reset);
            let (found, input) = scanner.scan("done", 16, flags(&[DONE_KEYWORD, DEDENT]));
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(DONE_KEYWORD).unwrap()
            );
            assert_eq!(input.offset, 4);
            assert_eq!(input.marked, Some(4));
            assert_eq!(scanner.snapshot(), reset, "written done popped the base");
        }
    }
}

#[test]
fn done_prefixes_stray_scopes_and_delimited_barriers_do_not_mutate_owner_state() {
    for profile in PROFILES {
        for text in [
            "doneValue",
            "undone",
            "done_1",
            "done'",
            "doneλ",
            "done\u{301}",
            "``done``",
        ] {
            let mut scanner = Scanner::new(profile);
            open_do_body(&mut scanner, 8);
            let before = scanner.snapshot();
            let (found, input) = scanner.scan(text, 16, flags(&[DONE_KEYWORD, DEDENT]));
            assert!(!found, "{profile:?}: delimiter claimed identifier {text:?}");
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(scanner.snapshot(), before);
        }
        for scope in [None, Some(INDENT), Some(PAREN_INDENT), Some(BRACE_INDENT)] {
            let mut scanner = Scanner::new(profile);
            if matches!(scope, Some(PAREN_INDENT | BRACE_INDENT)) {
                open_do_body(&mut scanner, 8);
            }
            if let Some(token) = scope {
                let (found, input) = scanner.scan("item", 16, flags(&[token]));
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
                assert_eq!(input.offset, 0);
                assert_eq!(input.marked, Some(0));
            }
            let before = scanner.snapshot();
            let (found, input) = scanner.scan("done", 16, flags(&[DEDENT]));
            assert!(!found, "{profile:?}: stray done crossed scope {scope:?}");
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(scanner.snapshot(), before);
        }
        for tokens in [
            vec![DONE_KEYWORD, DEDENT, INSIDE_STRING],
            vec![DONE_KEYWORD, DEDENT, ERROR_SENTINEL],
        ] {
            let mut scanner = Scanner::new(profile);
            open_do_body(&mut scanner, 8);
            let before = scanner.snapshot();
            let (found, input) = scanner.scan("done", 16, flags(&tokens));
            assert!(!found, "{profile:?}: guarded done emitted a token");
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(scanner.snapshot(), before);
        }
    }
}

#[test]
fn combined_indent_admission_commits_the_do_body_owner_not_an_ordinary_scope() {
    for profile in PROFILES {
        for column in [8, 257] {
            let mut normal = Scanner::new(profile);
            open_enum_body(&mut normal, column);
            let normal_state = normal.snapshot();
            let mut owned = Scanner::new(profile);
            open_do_body(&mut owned, column);
            let owned_state = owned.snapshot();
            assert_ne!(owned_state, normal_state);
            let mut combined = Scanner::new(profile);
            let (found, input) = combined.scan(
                "item",
                column,
                flags(&[INDENT, DO_BODY_INDENT, PREPROC_INACTIVE]),
            );
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(DO_BODY_INDENT).unwrap()
            );
            assert_eq!(input.offset, 0);
            assert_eq!(input.marked, Some(0));
            assert_eq!(combined.snapshot(), owned_state);
            assert_ne!(combined.snapshot(), normal_state);
        }
    }
}

fn native_do_anchor_state(profile: Profile, token: usize, anchor: &str) -> Vec<u8> {
    let mut scanner = Scanner::new(profile);
    let (source, column) = match anchor {
        "scoped" => ("\n        value", 0),
        "midline" => ("value", 8),
        "stranded" => {
            open_enum_body(&mut scanner, 4);
            let (found, input) = scanner.scan("\n            value", 0, flags(&[INDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
            let source = "\n        value";
            let (found, input) = scanner.scan(source, 0, flags(&[DEDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(0));
            let (found, input) = scanner.scan(source, 0, flags(&[NEWLINE]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
            assert_eq!(input.marked, Some(0));
            // The parser consumes the line's prefix before the same-line body marker.
            ("value", 20)
        }
        other => panic!("unexpected anchor {other}"),
    };
    let (found, input) = scanner.scan(source, column, flags(&[token]));
    assert!(found, "{profile:?}: {anchor} marker {token}");
    assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
    assert_eq!(input.marked, Some(0));
    assert_eq!(input.offset, source.chars().count() - "value".len());
    scanner.snapshot()
}

#[test]
fn native_stranded_do_state_round_trips_and_every_unknown_raw_kind_resets() {
    let domain = [0_u8, 1, 2, 3, 4, 5, 128, 133, 192, 197];
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        for (anchor, expected_kind) in [("scoped", 5), ("midline", 133), ("stranded", 197)] {
            let normal = native_do_anchor_state(profile, INDENT, anchor);
            let owned = native_do_anchor_state(profile, DO_BODY_INDENT, anchor);
            assert_eq!(normal.len(), owned.len());
            let different: Vec<_> = owned
                .iter()
                .zip(&normal)
                .enumerate()
                .filter_map(|(index, (owned, normal))| (owned != normal).then_some(index))
                .collect();
            assert_eq!(
                different.len(),
                1,
                "{profile:?}: {anchor} owner tag difference"
            );
            let kind_index = different[0];
            assert_eq!(
                owned[kind_index], expected_kind,
                "{profile:?}: native {anchor}"
            );
            let mut restored = Scanner::new(profile);
            restored.restore(&owned);
            assert_eq!(restored.snapshot_at(1), owned);
            for unknown in u8::MIN..=u8::MAX {
                if domain.contains(&unknown) {
                    continue;
                }
                // Only rejection inputs alter an observed native kind; accepted states are genuine.
                let mut invalid = owned.clone();
                invalid[kind_index] = unknown;
                restored.restore(&owned);
                restored.restore(&invalid);
                assert_eq!(
                    restored.snapshot(),
                    reset,
                    "{profile:?}: {anchor} accepted raw kind {unknown}"
                );
            }
        }
    }
}

const TYPE_APP_INDENT: usize = 31;

#[test]
fn type_application_barrier_blocks_done_until_its_own_written_closer() {
    for profile in PROFILES {
        for newline in ["\n", "\r\n"] {
            let mut scanner = Scanner::new(profile);
            open_do_body(&mut scanner, 8);
            let outer = scanner.snapshot();
            let source = format!("{newline}                Type");
            let (found, input) = scanner.scan(&source, 0, flags(&[TYPE_APP_INDENT]));
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(TYPE_APP_INDENT).unwrap()
            );
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, source.chars().count() - "Type".len());
            let barrier = scanner.snapshot();
            assert_ne!(barrier, outer);
            let (found, input) = scanner.scan("done", 16, flags(&[DEDENT]));
            assert!(!found, "{profile:?}: done crossed a type application");
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(scanner.snapshot(), barrier);
            let (found, input) = scanner.scan(">", 16, flags(&[DEDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, 0);
            assert_eq!(scanner.snapshot(), outer);
        }
    }
}
