//! Isolated Fortran scanner frames, rejected-label bookkeeping and ordinary syntax.
//! Frame fixtures retain the pinned scanner's native-endian wire format; no Fortran
//! program is executed, and null tests do not claim allocation-failure injection.

use std::{
    ffi::{c_char, c_uint, c_void},
    mem::{align_of, offset_of, size_of},
    ptr::NonNull,
};
use tree_sitter::{InputEdit, Parser, Point, Tree};

const MAX_LABEL_STACK: usize = 100;
const SERIALIZATION_BUFFER_SIZE: usize = 1024;
const FRAME_BASE_SIZE: usize = 10;
const FRAME_ENTRY_SIZE: usize = 8;
const CANARY: u8 = 0xa5;

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
    marked: usize,
    advance_calls: usize,
}

impl Input {
    fn new(text: &str) -> Self {
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
            marked: 0,
            advance_calls: 0,
        }
    }
}

unsafe extern "C" fn advance(lexer: *mut Lexer, _skip: bool) {
    // SAFETY: The pinned scanner receives the first field of the exclusively live Input.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.advance_calls = input.advance_calls.saturating_add(1);
    input.offset = input.offset.saturating_add(1);
    input.lexer.lookahead = input.points.get(input.offset).copied().unwrap_or(0);
}

unsafe extern "C" fn mark_end(lexer: *mut Lexer) {
    // SAFETY: The repr(C) first field identifies the live Input passed to this scan.
    let input = unsafe { &mut *lexer.cast::<Input>() };
    input.marked = input.offset;
}

unsafe extern "C" fn get_column(lexer: *mut Lexer) -> u32 {
    // SAFETY: The scanner supplies its live Input lexer; this callback only reads it.
    let input = unsafe { &*lexer.cast::<Input>() };
    u32::try_from(input.offset).unwrap_or(u32::MAX)
}

unsafe extern "C" fn is_at_included_range_start(_lexer: *const Lexer) -> bool {
    false
}

unsafe extern "C" fn eof(lexer: *const Lexer) -> bool {
    // SAFETY: The scanner supplies the first field of its still-live Input.
    let input = unsafe { &*lexer.cast::<Input>() };
    input.offset >= input.points.len()
}

// SAFETY: Signatures and Lexer match the retained scanner.c and tree_sitter/parser.h.
unsafe extern "C" {
    fn tree_sitter_fortran_external_scanner_create() -> *mut c_void;
    fn tree_sitter_fortran_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_fortran_external_scanner_scan(
        payload: *mut c_void,
        lexer: *mut Lexer,
        valid_symbols: *const bool,
    ) -> bool;
    fn tree_sitter_fortran_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_fortran_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum Token {
    Integer = 1,
    EndOfStatement = 6,
    PreprocUnaryOperator = 7,
    DoLabel = 9,
    DoLabelVirtual = 10,
    DoLabelContinue = 11,
}

struct ScanResult {
    found: bool,
    symbol: u16,
    offset: usize,
    marked: usize,
}

struct Scanner(NonNull<c_void>);

#[repr(align(16))]
struct GuardedBuffer([u8; SERIALIZATION_BUFFER_SIZE + 2]);

// This mirror provides integer offsets only; no Rust reference to C state is formed.
#[repr(C)]
struct NativeState {
    in_line_continuation: bool,
    depth: i32,
    labels: [i32; MAX_LABEL_STACK],
    counts: [i32; MAX_LABEL_STACK],
    pending_label_virtual: i32,
    is_pending_eos_virtual: bool,
}

enum IntegerField {
    Depth,
    Count(usize),
    Pending,
}

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_fortran::LANGUAGE.into();
        // SAFETY: This constructor has no arguments; the owner destroys its result once.
        Self(NonNull::new(unsafe { tree_sitter_fortran_external_scanner_create() }).unwrap())
    }

    fn restore(&mut self, bytes: &[u8]) {
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: All advertised bytes are live; the patched decoder accepts null reset.
        unsafe {
            tree_sitter_fortran_external_scanner_deserialize(
                self.0.as_ptr(),
                pointer,
                c_uint::try_from(bytes.len()).unwrap(),
            )
        };
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut buffer = GuardedBuffer([CANARY; SERIALIZATION_BUFFER_SIZE + 2]);
        assert_eq!((buffer.0[1..].as_ptr() as usize) % align_of::<i32>(), 1);
        // SAFETY: The native serializer receives a live writable 1024-byte buffer.
        let length = unsafe {
            tree_sitter_fortran_external_scanner_serialize(
                self.0.as_ptr(),
                buffer.0[1..].as_mut_ptr().cast(),
            )
        };
        let length = usize::try_from(length).unwrap();
        assert!(length <= SERIALIZATION_BUFFER_SIZE);
        assert_eq!(buffer.0[0], CANARY);
        assert!(buffer.0[1 + length..].iter().all(|byte| *byte == CANARY));
        buffer.0[1..1 + length].to_vec()
    }

    fn scan(&mut self, text: &str, tokens: &[Token]) -> ScanResult {
        let mut input = Input::new(text);
        let valid = valid_symbols(tokens);
        // SAFETY: The scanner, Input and all twelve token flags remain live and exclusive.
        let found = unsafe {
            tree_sitter_fortran_external_scanner_scan(
                self.0.as_ptr(),
                &mut input.lexer,
                valid.as_ptr(),
            )
        };
        ScanResult {
            found,
            symbol: input.lexer.result_symbol,
            offset: input.offset,
            marked: input.marked,
        }
    }

    fn corrupt_integer(&mut self, field: IntegerField, value: i32) {
        let offset = match field {
            IntegerField::Depth => offset_of!(NativeState, depth),
            IntegerField::Count(index) => {
                assert!(index < MAX_LABEL_STACK);
                offset_of!(NativeState, counts) + index * size_of::<i32>()
            }
            IntegerField::Pending => offset_of!(NativeState, pending_label_virtual),
        };
        // SAFETY: The pinned C state has this repr(C) layout; only one in-bounds i32
        // is overwritten while its sole owner excludes any simultaneous native call.
        unsafe {
            self.0
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<i32>()
                .write_unaligned(value)
        };
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: The unique owner releases its live allocation through the native allocator.
        unsafe { tree_sitter_fortran_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn valid_symbols(tokens: &[Token]) -> [bool; 12] {
    let mut valid = [false; 12];
    for token in tokens {
        valid[*token as usize] = true;
    }
    valid
}

fn frame(labels: &[i32], counts: &[i32], pending: i32, flags: [u8; 2]) -> Vec<u8> {
    assert_eq!(labels.len(), counts.len());
    let mut bytes = Vec::new();
    bytes.push(flags[0]);
    bytes.extend_from_slice(&i32::try_from(labels.len()).unwrap().to_ne_bytes());
    for label in labels {
        bytes.extend_from_slice(&label.to_ne_bytes());
    }
    for count in counts {
        bytes.extend_from_slice(&count.to_ne_bytes());
    }
    bytes.extend_from_slice(&pending.to_ne_bytes());
    bytes.push(flags[1]);
    assert_eq!(
        bytes.len(),
        FRAME_BASE_SIZE + FRAME_ENTRY_SIZE * labels.len()
    );
    bytes
}

fn reset_frame() -> Vec<u8> {
    frame(&[], &[], 0, [0, 0])
}

fn word(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + size_of::<i32>()].copy_from_slice(&value.to_ne_bytes());
}

fn assert_token(result: ScanResult, token: Token) {
    assert!(result.found);
    assert_eq!(result.symbol, u16::try_from(token as usize).unwrap());
}

#[test]
fn valid_frames_round_trip_every_depth_flags_and_representable_counts() {
    let mut scanner = Scanner::new();
    assert_eq!(scanner.bytes(), reset_frame());
    for depth in 0..=MAX_LABEL_STACK {
        let labels: Vec<_> = (0..depth)
            .map(|index| i32::try_from(index).unwrap())
            .collect();
        let counts: Vec<_> = (0..depth)
            .map(|index| [1, 101, i32::MAX][index % 3])
            .collect();
        for flags in [[0, 0], [0, 1], [1, 0], [1, 1]] {
            for pending in [0, 1, 101, i32::MAX] {
                let expected = frame(&labels, &counts, pending, flags);
                let mut unaligned = vec![CANARY; expected.len() + align_of::<i32>() + 1];
                let offset = (align_of::<i32>() - unaligned.as_ptr() as usize % align_of::<i32>())
                    % align_of::<i32>()
                    + 1;
                unaligned[offset..offset + expected.len()].copy_from_slice(&expected);
                let input = &unaligned[offset..offset + expected.len()];
                assert_eq!(input.as_ptr() as usize % align_of::<i32>(), 1);
                scanner.restore(input);
                assert_eq!(scanner.bytes(), expected, "depth {depth}, flags {flags:?}");
                assert_eq!(scanner.bytes(), expected);
            }
        }
    }
    assert_eq!(frame(&[1; 100], &[1; 100], 0, [0, 0]).len(), 810);
}

#[test]
fn malformed_frames_reset_all_prior_state_without_partial_publication() {
    let mut scanner = Scanner::new();
    let good = frame(&[17, 23], &[2, 3], 4, [1, 1]);
    let mut invalid: Vec<Vec<u8>> = (0..good.len()).map(|end| good[..end].to_vec()).collect();
    invalid.push([good.as_slice(), &[0]].concat());
    invalid.push(vec![0; SERIALIZATION_BUFFER_SIZE + 1]);
    for (offset, values) in [
        (1, &[-1, i32::MIN, 101, i32::MAX][..]),
        (5 + 4 * 2, &[0, -1, i32::MIN][..]),
        (5 + 4 * 2 + 4, &[0, -1, i32::MIN][..]),
        (5 + 8 * 2, &[-1, i32::MIN][..]),
    ] {
        for value in values {
            let mut bytes = good.clone();
            word(&mut bytes, offset, *value);
            invalid.push(bytes);
        }
    }
    for offset in [0, good.len() - 1] {
        for value in [2, 255] {
            let mut bytes = good.clone();
            bytes[offset] = value;
            invalid.push(bytes);
        }
    }
    for bytes in invalid {
        scanner.restore(&good);
        assert_eq!(scanner.bytes(), good);
        scanner.restore(&bytes);
        assert_eq!(scanner.bytes(), reset_frame(), "rejected frame {bytes:?}");
        assert_eq!(scanner.bytes(), reset_frame());
    }
}

#[test]
fn invalid_live_integer_state_is_rejected_before_any_serialization_write() {
    let mut scanner = Scanner::new();
    let good = frame(&[17], &[1], 0, [0, 0]);
    for (field, value) in [
        (IntegerField::Depth, -1),
        (IntegerField::Depth, i32::MIN),
        (IntegerField::Depth, 101),
        (IntegerField::Depth, i32::MAX),
        (IntegerField::Count(0), 0),
        (IntegerField::Count(0), -1),
        (IntegerField::Count(0), i32::MIN),
        (IntegerField::Pending, -1),
        (IntegerField::Pending, i32::MIN),
    ] {
        scanner.restore(&good);
        scanner.corrupt_integer(field, value);
        assert!(scanner.bytes().is_empty());
    }
    scanner.restore(&[]);
    assert_eq!(scanner.bytes(), reset_frame());
}

#[test]
fn null_entrypoints_fail_closed_without_lexer_callbacks_or_output_writes() {
    let mut scanner = Scanner::new();
    let mut input = Input::new("17");
    let valid = valid_symbols(&[Token::DoLabel]);
    let mut output = [CANARY; SERIALIZATION_BUFFER_SIZE];
    // SAFETY: All non-null arguments are live; the patched ABI explicitly rejects null payload.
    let found = unsafe {
        tree_sitter_fortran_external_scanner_scan(
            std::ptr::null_mut(),
            &mut input.lexer,
            valid.as_ptr(),
        )
    };
    assert!(!found);
    assert_eq!(input.advance_calls, 0);
    assert_eq!(input.lexer.result_symbol, u16::MAX);
    // SAFETY: The live scanner rejects a null lexer before invoking any callback.
    let found = unsafe {
        tree_sitter_fortran_external_scanner_scan(
            scanner.0.as_ptr(),
            std::ptr::null_mut(),
            valid.as_ptr(),
        )
    };
    assert!(!found);
    // SAFETY: The live scanner rejects null flags before reading the live lexer.
    let found = unsafe {
        tree_sitter_fortran_external_scanner_scan(
            scanner.0.as_ptr(),
            &mut input.lexer,
            std::ptr::null(),
        )
    };
    assert!(!found);
    assert_eq!(input.advance_calls, 0);
    // SAFETY: The patched serializer rejects null payload without touching the live output.
    let length = unsafe {
        tree_sitter_fortran_external_scanner_serialize(
            std::ptr::null_mut(),
            output.as_mut_ptr().cast(),
        )
    };
    assert_eq!(length, 0);
    assert!(output.iter().all(|byte| *byte == CANARY));
    // SAFETY: The live scanner's serializer rejects a null output without writing.
    let length = unsafe {
        tree_sitter_fortran_external_scanner_serialize(scanner.0.as_ptr(), std::ptr::null_mut())
    };
    assert_eq!(length, 0);
    scanner.restore(&frame(&[17], &[3], 2, [1, 1]));
    // SAFETY: This deliberately malformed null frame is handled before any read.
    unsafe {
        tree_sitter_fortran_external_scanner_deserialize(scanner.0.as_ptr(), std::ptr::null(), 1)
    };
    assert_eq!(scanner.bytes(), reset_frame());
    // SAFETY: The patched decoder and destructor accept null payload as a no-op.
    unsafe {
        tree_sitter_fortran_external_scanner_deserialize(std::ptr::null_mut(), std::ptr::null(), 1);
        tree_sitter_fortran_external_scanner_destroy(std::ptr::null_mut());
    }
}

#[test]
fn distinct_label_capacity_refuses_the_next_token_without_losing_the_stack() {
    let mut scanner = Scanner::new();
    for label in 1..=100 {
        let text = label.to_string();
        let result = scanner.scan(&text, &[Token::DoLabel]);
        assert_eq!(result.offset, text.len());
        assert_eq!(result.marked, text.len());
        assert_token(result, Token::DoLabel);
    }
    let labels: Vec<i32> = (1..=100).collect();
    let full = frame(&labels, &[1; 100], 0, [0, 0]);
    assert_eq!(scanner.bytes(), full);
    for text in ["101", "101+"] {
        let rejected = scanner.scan(
            text,
            &[Token::DoLabel, Token::Integer, Token::PreprocUnaryOperator],
        );
        assert!(!rejected.found);
        assert_eq!(rejected.symbol, u16::MAX);
        assert_eq!(scanner.bytes(), full);
    }
    for label in (1..=100).rev() {
        assert_token(
            scanner.scan(&label.to_string(), &[Token::DoLabelContinue]),
            Token::DoLabelContinue,
        );
    }
    assert_eq!(scanner.bytes(), reset_frame());
    assert_token(scanner.scan("101", &[Token::DoLabel]), Token::DoLabel);
    assert_eq!(scanner.bytes(), frame(&[101], &[1], 0, [0, 0]));
}

#[test]
fn repeated_label_counts_above_stack_capacity_remain_supported() {
    let mut scanner = Scanner::new();
    for _ in 0..101 {
        assert_token(scanner.scan("17", &[Token::DoLabel]), Token::DoLabel);
    }
    assert_eq!(scanner.bytes(), frame(&[17], &[101], 0, [0, 0]));
    assert_token(
        scanner.scan("17", &[Token::DoLabelContinue, Token::DoLabelVirtual]),
        Token::DoLabelVirtual,
    );
    assert_eq!(scanner.bytes(), frame(&[], &[], 100, [0, 1]));
    assert_token(
        scanner.scan("", &[Token::EndOfStatement]),
        Token::EndOfStatement,
    );
    for pending in (1..100).rev() {
        assert_token(
            scanner.scan("", &[Token::DoLabelVirtual]),
            Token::DoLabelVirtual,
        );
        assert_eq!(scanner.bytes(), frame(&[], &[], pending, [0, 1]));
        assert_token(
            scanner.scan("", &[Token::EndOfStatement]),
            Token::EndOfStatement,
        );
    }
    assert_token(
        scanner.scan("", &[Token::DoLabelContinue]),
        Token::DoLabelContinue,
    );
    assert_eq!(scanner.bytes(), reset_frame());
}

#[test]
fn repeated_top_label_remains_admissible_at_full_distinct_depth() {
    let mut scanner = Scanner::new();
    let labels: Vec<i32> = (1..=100).collect();
    scanner.restore(&frame(&labels, &[1; 100], 0, [0, 0]));
    assert_token(scanner.scan("100", &[Token::DoLabel]), Token::DoLabel);
    let mut counts = [1; 100];
    counts[99] = 2;
    let repeated = frame(&labels, &counts, 0, [0, 0]);
    assert_eq!(scanner.bytes(), repeated);
    assert!(!scanner.scan("101", &[Token::DoLabel]).found);
    assert_eq!(scanner.bytes(), repeated);
}

#[test]
fn maximal_top_counter_refuses_increment_but_preserves_valid_prior_state() {
    let mut scanner = Scanner::new();
    scanner.restore(&frame(&[17], &[i32::MAX - 1], 0, [0, 0]));
    assert_token(scanner.scan("17", &[Token::DoLabel]), Token::DoLabel);
    let maximum = frame(&[17], &[i32::MAX], 0, [0, 0]);
    assert_eq!(scanner.bytes(), maximum);
    let rejected = scanner.scan("17+", &[Token::DoLabel, Token::PreprocUnaryOperator]);
    assert!(!rejected.found);
    assert_eq!(rejected.symbol, u16::MAX);
    assert_eq!(scanner.bytes(), maximum);
    assert_token(scanner.scan("23", &[Token::DoLabel]), Token::DoLabel);
    assert_eq!(scanner.bytes(), frame(&[17, 23], &[i32::MAX, 1], 0, [0, 0]));
}

#[test]
fn maximal_pending_counter_performs_one_safe_decrement_without_a_new_limit() {
    let mut scanner = Scanner::new();
    scanner.restore(&frame(&[], &[], i32::MAX, [0, 0]));
    assert_token(
        scanner.scan("", &[Token::DoLabelVirtual]),
        Token::DoLabelVirtual,
    );
    assert_eq!(scanner.bytes(), frame(&[], &[], i32::MAX - 1, [0, 1]));
    assert_token(
        scanner.scan("", &[Token::EndOfStatement]),
        Token::EndOfStatement,
    );
    assert_eq!(scanner.bytes(), frame(&[], &[], i32::MAX - 1, [0, 0]));
}

const DISTINCT_LOOPS: &str = "program loops\n  integer :: i, j\n  do 17 i = 1, 3\n    do 23 j = 1, 2\n      print *, i, j\n23 continue\n17 continue\nend program loops\n";
const SHARED_LOOPS: &str = "program loops\n  integer :: i, j\n  do 17 i = 1, 3\n    do 17 j = 1, 2\n      print *, i, j\n17 continue\nend program loops\n";
const CONTINUATION: &str = "program literals\n  integer :: value\n  character(len=8) :: text\n  value = 1 + &\n    & 2\n  text = 'ab&\n    &cd'\n  print *, text, value\nend program literals\n";

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_fortran::LANGUAGE.into())
        .unwrap();
    parser
}

fn parse(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    tree
}

#[test]
fn ordinary_distinct_shared_do_and_literal_continuations_remain_error_free() {
    for source in [DISTINCT_LOOPS, SHARED_LOOPS] {
        let tree = parse(source);
        assert_eq!(tree.root_node().to_sexp().matches("(do_loop").count(), 2);
    }
    parse(CONTINUATION);
}

#[test]
fn labeled_do_incremental_reparse_matches_a_clean_parse() {
    let mut parser = parser();
    let mut tree = parser.parse(SHARED_LOOPS, None).unwrap();
    assert!(!tree.root_node().has_error());
    let start = SHARED_LOOPS.find("1, 3").unwrap();
    let prefix = &SHARED_LOOPS[..start];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix.rsplit('\n').next().unwrap().len();
    let mut updated = SHARED_LOOPS.as_bytes().to_vec();
    updated[start] = b'7';
    let updated = String::from_utf8(updated).unwrap();
    tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 1,
        start_position: Point::new(row, column),
        old_end_position: Point::new(row, column + 1),
        new_end_position: Point::new(row, column + 1),
    });
    let incremental = parser.parse(&updated, Some(&tree)).unwrap();
    let clean = parse(&updated);
    assert!(!incremental.root_node().has_error());
    assert_eq!(
        incremental.root_node().to_sexp(),
        clean.root_node().to_sexp()
    );
}
