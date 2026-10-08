// Grammar-context literal guards leave older scanner tokens and serialized state unchanged.
// This fragment shares the complete-domain FFI owner of the retained scanner suite.

const INTEGER_LITERAL_SUFFIX: usize = 48;
const BASED_LITERAL_SUFFIX: usize = 49;
const FLOAT_LITERAL_SUFFIX: usize = 50;
const NUMERIC_EXPONENT_CONTINUATION: usize = 51;
const NUMERIC_DIGIT_CONTINUATION: usize = 52;
const NUMERIC_GUARDS: [usize; 5] = [
    INTEGER_LITERAL_SUFFIX,
    BASED_LITERAL_SUFFIX,
    FLOAT_LITERAL_SUFFIX,
    NUMERIC_EXPONENT_CONTINUATION,
    NUMERIC_DIGIT_CONTINUATION,
];

#[test]
fn numeric_context_ordinals_append_to_the_complete_legacy_domain() {
    assert_eq!(TOKEN_COUNT, 53);
    assert_eq!(SNAPSHOT_CAPACITY, 1024);
    assert_eq!(NUMERIC_GUARDS, [48, 49, 50, 51, 52]);
    let legacy = [
        "NEWLINE",
        "INDENT",
        "DEDENT",
        "THEN",
        "ELSE",
        "ELIF",
        "PREPROC_IF",
        "PREPROC_ELSE",
        "PREPROC_END",
        "CLASS",
        "BEGIN",
        "STRUCT",
        "INTERFACE",
        "END",
        "AND",
        "WITH",
        "TRIPLE_QUOTE_CONTENT",
        "FORMAT_TRIPLE_QUOTE_CONTENT",
        "BLOCK_COMMENT_CONTENT",
        "INSIDE_STRING",
        "NEWLINE_NO_ALIGNED",
        "TUPLE_MARKER",
        "QUOTED_CLOSE",
        "UNTYPED_QUOTED_CLOSE",
        "MULTI_DOLLAR_TRIPLE_QUOTE_START",
        "MULTI_DOLLAR_TRIPLE_QUOTED_CONTENT",
        "MULTI_DOLLAR_INTERP_START",
        "MULTI_DOLLAR_INTERP_END",
        "MULTI_DOLLAR_TRIPLE_QUOTE_END",
        "TYAPP_OPEN",
        "PAREN_INDENT",
        "TYPE_APP_INDENT",
        "TYPE_DECL_NEWLINE",
        "IN",
        "DO_KEYWORD",
        "TRY_INDENT",
        "PREPROC_INACTIVE",
        "ELEM_SEP",
        "BRACE_INDENT",
        "ERROR_SENTINEL",
        "ENUM_CASE_BAR",
        "DO_BODY_INDENT",
        "DONE_KEYWORD",
        "APPLICATION_ARG_START",
        "DECL_TERM",
        "ROOT_BODY_INDENT",
        "SINGLE_SEMICOLON_EXTRA",
        "QUALIFIED_CALLEE_DOT",
    ];
    assert_eq!(legacy.len(), 48);
    for (name, ordinal) in [
        ("NEWLINE", NEWLINE),
        ("INDENT", INDENT),
        ("DEDENT", DEDENT),
        ("PREPROC_IF", PREPROC_IF),
        ("PREPROC_END", PREPROC_END),
        ("INSIDE_STRING", INSIDE_STRING),
        ("PAREN_INDENT", PAREN_INDENT),
        ("PREPROC_INACTIVE", PREPROC_INACTIVE),
        ("ERROR_SENTINEL", ERROR_SENTINEL),
        ("ENUM_CASE_BAR", ENUM_CASE_BAR),
        ("APPLICATION_ARG_START", APPLICATION_ARGUMENT_START),
    ] {
        assert_eq!(legacy[ordinal], name);
    }
    // Generated-C inspection independently joins every name to the real parser map.
    assert_eq!(legacy[47], "QUALIFIED_CALLEE_DOT");
}

#[test]
fn inert_numeric_guards_alone_preserve_native_state_and_input() {
    for profile in PROFILES {
        for mask in 1_u8..32 {
            for extra in [false, true] {
                let mut scanner = marker_scanner(profile);
                let before = scanner.snapshot();
                assert_ne!(before, Scanner::new(profile).snapshot());
                let mut valid = [false; TOKEN_COUNT];
                for (bit, ordinal) in NUMERIC_GUARDS.into_iter().enumerate() {
                    valid[ordinal] = mask & (1_u8 << bit) != 0;
                }
                valid[PREPROC_INACTIVE] = extra;
                let (found, input) = scanner.scan("u", 12, valid);
                assert!(!found);
                assert_eq!(input.lexer.result_symbol, u16::MAX);
                assert_eq!(input.offset, 0);
                assert_eq!(input.advances, 0);
                assert_eq!(input.marked, None);
                assert_eq!(scanner.snapshot(), before);
            }
        }
    }
}

#[test]
fn numeric_continuations_decline_only_the_adjacent_argument_marker() {
    let families: [(usize, &[&str]); 5] = [
        (
            INTEGER_LITERAL_SUFFIX,
            &[
                "y", "uy", "s", "us", "l", "ul", "u", "n", "un", "L", "UL", "uL", "Q", "R", "Z",
                "I", "N", "G", "M", "m",
            ],
        ),
        (
            BASED_LITERAL_SUFFIX,
            &[
                "y", "uy", "s", "us", "l", "ul", "u", "n", "un", "L", "UL", "uL", "lf", "LF",
            ],
        ),
        (FLOAT_LITERAL_SUFFIX, &["f", "F", "M", "m"]),
        (NUMERIC_EXPONENT_CONTINUATION, &["e2", "E-2", "e+2"]),
        (NUMERIC_DIGIT_CONTINUATION, &["0", "12_3", "4_"]),
    ];
    for profile in PROFILES {
        for (guard, suffixes) in families {
            for suffix in suffixes {
                let mut scanner = marker_scanner(profile);
                assert_no_argument_marker(
                    &mut scanner,
                    suffix,
                    12,
                    &[APPLICATION_ARGUMENT_START, guard],
                );
                assert_argument_marker(&mut scanner, suffix, 12, &[APPLICATION_ARGUMENT_START], 0);
                for trivia in [" ", "(*c*)"] {
                    let separated = format!("{trivia}{suffix}");
                    assert_argument_marker(
                        &mut scanner,
                        &separated,
                        12,
                        &[APPLICATION_ARGUMENT_START, guard],
                        0,
                    );
                }
            }
        }
        for guard in [INTEGER_LITERAL_SUFFIX, BASED_LITERAL_SUFFIX] {
            let mut older = marker_scanner(profile);
            let before = older.snapshot();
            let (old_found, old_input) = older.scan("UL", 12, flags(&[APPLICATION_ARGUMENT_START]));
            assert!(old_found);
            assert_eq!(
                old_input.lexer.result_symbol,
                u16::try_from(APPLICATION_ARGUMENT_START).unwrap()
            );
            assert_eq!(older.snapshot(), before);
            let mut guarded = Scanner::new(profile);
            guarded.restore(&before);
            let (found, input) =
                guarded.scan("UL", 12, flags(&[APPLICATION_ARGUMENT_START, guard]));
            assert!(!found);
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(input.marked, old_input.marked);
            assert_eq!(input.offset, old_input.offset);
            assert_eq!(input.advances, old_input.advances);
            assert_eq!(input.advanced_past_end, old_input.advanced_past_end);
            assert_eq!(input.watchdog, old_input.watchdog);
            assert_eq!(guarded.snapshot(), before);
            for non_suffix in ["U", "UX", "Ua", "Uu"] {
                assert_argument_marker(
                    &mut guarded,
                    non_suffix,
                    12,
                    &[APPLICATION_ARGUMENT_START, guard],
                    0,
                );
            }
            for separated in [" UL", "(*c*)UL"] {
                assert_argument_marker(
                    &mut guarded,
                    separated,
                    12,
                    &[APPLICATION_ARGUMENT_START, guard],
                    0,
                );
            }
        }
        for guard in [
            FLOAT_LITERAL_SUFFIX,
            NUMERIC_EXPONENT_CONTINUATION,
            NUMERIC_DIGIT_CONTINUATION,
        ] {
            let mut scanner = marker_scanner(profile);
            assert_argument_marker(
                &mut scanner,
                "UL",
                12,
                &[APPLICATION_ARGUMENT_START, guard],
                0,
            );
        }
        let mut scanner = marker_scanner(profile);
        assert_argument_marker(
            &mut scanner,
            "M",
            12,
            &[APPLICATION_ARGUMENT_START, BASED_LITERAL_SUFFIX],
            0,
        );
        assert_argument_marker(
            &mut scanner,
            "u",
            12,
            &[APPLICATION_ARGUMENT_START, FLOAT_LITERAL_SUFFIX],
            0,
        );
        for source in ["value", "\"text\"", "[|1|]", "{ Value = 1 }"] {
            let mut requested = vec![APPLICATION_ARGUMENT_START];
            requested.extend(NUMERIC_GUARDS);
            assert_argument_marker(&mut scanner, source, 12, &requested, 0);
        }
        let mut co_valid = vec![APPLICATION_ARGUMENT_START, INTEGER_LITERAL_SUFFIX];
        co_valid.push(NUMERIC_EXPONENT_CONTINUATION);
        assert_no_argument_marker(&mut scanner, "u", 12, &co_valid);
        assert_no_argument_marker(&mut scanner, "e+2", 12, &co_valid);
    }
}

#[test]
fn numeric_guards_preserve_older_layout_and_recovery_results() {
    for profile in PROFILES {
        for (source, column, old_tokens) in [
            (
                "\n    u",
                0,
                vec![NEWLINE, DEDENT, APPLICATION_ARGUMENT_START],
            ),
            ("\n", 0, vec![ERROR_SENTINEL, DEDENT]),
            ("end", 12, (0..48).collect()),
            ("#endif", 0, (0..48).collect()),
            ("u", 12, (0..48).collect()),
            ("UL", 12, (0..48).collect()),
            ("e", 12, (0..48).collect()),
            ("0", 12, (0..48).collect()),
        ] {
            let mut older = marker_scanner(profile);
            let state = older.snapshot();
            let (expected_found, expected_input) = older.scan(source, column, flags(&old_tokens));
            let expected_state = older.snapshot();
            for mask in 1_u8..32 {
                let mut candidate = Scanner::new(profile);
                candidate.restore(&state);
                let mut valid = flags(&old_tokens);
                for (bit, ordinal) in NUMERIC_GUARDS.into_iter().enumerate() {
                    valid[ordinal] = mask & (1_u8 << bit) != 0;
                }
                let (found, input) = candidate.scan(source, column, valid);
                assert_eq!(found, expected_found);
                assert_eq!(
                    input.lexer.result_symbol,
                    expected_input.lexer.result_symbol
                );
                assert_eq!(input.marked, expected_input.marked);
                assert_eq!(input.offset, expected_input.offset);
                assert_eq!(input.advances, expected_input.advances);
                assert_eq!(input.advanced_past_end, expected_input.advanced_past_end);
                assert_eq!(input.watchdog, expected_input.watchdog);
                assert_eq!(candidate.snapshot(), expected_state);
                if found {
                    assert!(usize::from(input.lexer.result_symbol) < 48);
                }
            }
        }
    }
}
