//! Prospective ROOT/DO scanner controls for the private codec-v2 prototype.
//! Append to a separately copied native scanner helper with exactly 53 valid flags.
//! Historical controls and their old unknown-kind expectations remain immutable.

const DECLARATION_TERMINATOR: usize = 44;
const ROOT_BODY_START: usize = 45;
const SINGLE_SEMICOLON_TRIVIA: usize = 46;
const DECLARATION_TRY_INDENT: usize = 35;
const DECLARATION_QUOTED_CLOSE: usize = 22;
const DECLARATION_UNTYPED_QUOTED_CLOSE: usize = 23;

fn declaration_open(scanner: &mut Scanner, token: usize, column: u32) {
    let source = if token == TYPE_APP_INDENT {
        format!("\n{}item", " ".repeat(usize::try_from(column).unwrap()))
    } else {
        "item".to_owned()
    };
    let start_column = if token == TYPE_APP_INDENT { 0 } else { column };
    let (found, input) = scanner.scan(&source, start_column, flags(&[token]));
    assert!(
        found,
        "{:?}: owner token {token} at {column}",
        scanner.profile
    );
    assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
    assert_eq!(input.marked, Some(0));
}

fn declaration_assert_decline(scanner: &mut Scanner, source: &str, tokens: &[usize]) {
    let before = scanner.snapshot();
    let (found, input) = scanner.scan(source, 16, flags(tokens));
    assert!(
        !found,
        "{:?}: forbidden terminator {source:?}, {tokens:?}",
        scanner.profile
    );
    assert_eq!(input.lexer.result_symbol, u16::MAX);
    assert_eq!(scanner.snapshot(), before);
}

#[test]
fn declaration_committed_root_is_distinct_from_normal_at_full_width_columns() {
    assert_eq!(TOKEN_COUNT, 53);
    for profile in PROFILES {
        for column in COLUMNS {
            let mut root = Scanner::new(profile);
            declaration_open(&mut root, ROOT_BODY_START, column);
            let root_state = root.snapshot();
            assert_eq!(root_state[1], 2);
            let mut normal = Scanner::new(profile);
            declaration_open(&mut normal, INDENT, column);
            assert_ne!(root_state, normal.snapshot());
            let mut restored = Scanner::new(profile);
            restored.restore(&root_state);
            assert_eq!(restored.snapshot(), root_state);
            for current in [&mut root, &mut restored] {
                let (found, input) = current.scan(
                    ";; trailing",
                    column,
                    flags(&[DEDENT, DECLARATION_TERMINATOR, NEWLINE]),
                );
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                assert_eq!(input.offset, 1);
                assert_eq!(input.marked, Some(0));
                assert_eq!(current.snapshot(), Scanner::new(profile).snapshot());
            }
            declaration_assert_decline(
                &mut normal,
                ";; trailing",
                &[DEDENT, DECLARATION_TERMINATOR, NEWLINE],
            );
        }
    }
}

#[test]
fn declaration_do_chain_closes_one_retained_owner_per_unread_double_and_replays() {
    for profile in PROFILES {
        let mut original = Scanner::new(profile);
        let mut levels = vec![original.snapshot()];
        declaration_open(&mut original, ROOT_BODY_START, 4);
        levels.push(original.snapshot());
        declaration_open(&mut original, DO_BODY_INDENT, 8);
        levels.push(original.snapshot());
        declaration_open(&mut original, DO_BODY_INDENT, 12);
        levels.push(original.snapshot());
        let mut restored = Scanner::new(profile);
        restored.restore(levels.last().unwrap());
        for next in levels[..levels.len() - 1].iter().rev() {
            for current in [&mut original, &mut restored] {
                let (found, input) = current.scan(
                    ";; // preserved\nfollowing",
                    20,
                    flags(&[DEDENT, DECLARATION_TERMINATOR, NEWLINE]),
                );
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                assert_eq!(input.offset, 1);
                assert_eq!(input.marked, Some(0));
                assert_eq!(current.snapshot(), *next);
            }
        }
        for current in [&mut original, &mut restored] {
            let before = current.snapshot();
            let (found, input) = current.scan(
                ";; // preserved\nfollowing",
                20,
                flags(&[DECLARATION_TERMINATOR, NEWLINE]),
            );
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(DECLARATION_TERMINATOR).unwrap()
            );
            assert_eq!(input.offset, 2);
            assert_eq!(input.marked, Some(2));
            assert_eq!(current.snapshot(), before);
        }
    }
}

#[test]
fn declaration_normal_try_bracket_and_directive_barriers_never_drain_root() {
    for profile in PROFILES {
        for token in [
            INDENT,
            DECLARATION_TRY_INDENT,
            PAREN_INDENT,
            TYPE_APP_INDENT,
            BRACE_INDENT,
        ] {
            let mut scanner = Scanner::new(profile);
            declaration_open(&mut scanner, ROOT_BODY_START, 4);
            declaration_open(&mut scanner, token, 8);
            declaration_assert_decline(
                &mut scanner,
                ";; next",
                &[DEDENT, DECLARATION_TERMINATOR, NEWLINE],
            );
        }
        for guard in [
            ERROR_SENTINEL,
            INSIDE_STRING,
            DECLARATION_QUOTED_CLOSE,
            DECLARATION_UNTYPED_QUOTED_CLOSE,
        ] {
            let mut scanner = Scanner::new(profile);
            declaration_open(&mut scanner, ROOT_BODY_START, 4);
            declaration_assert_decline(
                &mut scanner,
                ";; next",
                &[DEDENT, DECLARATION_TERMINATOR, NEWLINE, guard],
            );
        }
        let mut directive = Scanner::new(profile);
        declaration_open(&mut directive, ROOT_BODY_START, 4);
        let (found, input) = directive.scan("#if ready\nitem", 8, flags(&[PREPROC_IF]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(PREPROC_IF).unwrap()
        );
        declaration_assert_decline(
            &mut directive,
            ";; next",
            &[DEDENT, DECLARATION_TERMINATOR, NEWLINE],
        );
    }
}

#[test]
fn declaration_root_admission_requires_native_base_not_a_local_normal_or_do_mask() {
    for profile in PROFILES {
        for token in [INDENT, DO_BODY_INDENT, PAREN_INDENT, BRACE_INDENT] {
            let mut scanner = Scanner::new(profile);
            declaration_open(&mut scanner, token, 4);
            declaration_assert_decline(&mut scanner, "item", &[ROOT_BODY_START]);
            declaration_assert_decline(
                &mut scanner,
                "item",
                &[ROOT_BODY_START, INDENT, DO_BODY_INDENT],
            );
        }
        let mut empty = Scanner::new(profile);
        let (found, _) = empty.scan("in", 0, flags(&[IN]));
        assert!(found);
        declaration_assert_decline(&mut empty, "item", &[ROOT_BODY_START]);
        let mut root = Scanner::new(profile);
        declaration_open(&mut root, ROOT_BODY_START, 4);
        declaration_assert_decline(&mut root, "item", &[ROOT_BODY_START]);
    }
}

#[test]
fn declaration_written_double_is_not_newline_and_single_separator_stays_single() {
    for profile in PROFILES {
        let mut scanner = Scanner::new(profile);
        declaration_open(&mut scanner, ROOT_BODY_START, 4);
        declaration_assert_decline(&mut scanner, ";; next", &[NEWLINE]);
        declaration_assert_decline(&mut scanner, ";; next", &[NEWLINE, DECLARATION_TERMINATOR]);
        let before = scanner.snapshot();
        let (found, input) = scanner.scan(
            "; next",
            12,
            flags(&[NEWLINE, DEDENT, DECLARATION_TERMINATOR]),
        );
        assert!(found);
        assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
        assert_eq!(input.marked, Some(1));
        assert_eq!(scanner.snapshot(), before);
        let mut base = Scanner::new(profile);
        declaration_assert_decline(&mut base, "; next", &[DECLARATION_TERMINATOR]);
        declaration_assert_decline(&mut base, ";; next", &[NEWLINE, DEDENT]);
    }
}

#[test]
fn declaration_historical_codec_keeps_old_domain_and_never_reconstructs_root() {
    for profile in PROFILES {
        for token in [INDENT, DO_BODY_INDENT, PAREN_INDENT, BRACE_INDENT] {
            let mut donor = Scanner::new(profile);
            declaration_open(&mut donor, token, 257);
            let current = donor.snapshot();
            assert_eq!(current[1], 2);
            let mut historical = current.clone();
            historical[1] = 1;
            let mut restored = Scanner::new(profile);
            restored.restore(&historical);
            assert_eq!(restored.snapshot(), current);
            declaration_assert_decline(&mut restored, ";; next", &[DEDENT, DECLARATION_TERMINATOR]);
        }
        let mut root = Scanner::new(profile);
        declaration_open(&mut root, ROOT_BODY_START, 257);
        let mut historical_root = root.snapshot();
        historical_root[1] = 1;
        let reset = Scanner::new(profile).snapshot();
        let mut restored = Scanner::new(profile);
        restored.restore(&historical_root);
        assert_eq!(restored.snapshot(), reset);
        let (found, _) = root.scan("in", 0, flags(&[IN]));
        assert!(found);
        let mut empty = Scanner::new(profile);
        let (found, _) = empty.scan("in", 0, flags(&[IN]));
        assert!(found);
        let current_empty = empty.snapshot();
        let mut historical_empty = current_empty.clone();
        historical_empty[1] = 1;
        restored.restore(&historical_empty);
        assert_eq!(restored.snapshot(), current_empty);
        declaration_assert_decline(&mut restored, "item", &[ROOT_BODY_START]);
    }
}

#[test]
fn declaration_current_codec_rejects_impossible_root_positions_flags_and_extra_bytes() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut root = Scanner::new(profile);
        declaration_open(&mut root, ROOT_BODY_START, 257);
        let state = root.snapshot();
        assert_eq!(state.len(), 22);
        assert_eq!(state[16], 0);
        assert_eq!(state[21], 0x86);
        let mut invalid = Vec::new();
        let mut raw_stranded_only = state.clone();
        raw_stranded_only[21] = 0x46;
        invalid.push(raw_stranded_only);
        let mut unknown_kind = state.clone();
        unknown_kind[21] = 7;
        invalid.push(unknown_kind);
        let mut wrong_base_column = state.clone();
        wrong_base_column[12] = 1;
        invalid.push(wrong_base_column);
        let mut wrong_base_kind = state.clone();
        wrong_base_kind[16] = 6;
        invalid.push(wrong_base_kind);
        let mut unknown_version = state.clone();
        unknown_version[1] = 3;
        invalid.push(unknown_version);
        let mut duplicate = state.clone();
        duplicate[6] = 3;
        duplicate.extend_from_slice(&state[17..22]);
        invalid.push(duplicate);
        let mut extra = state.clone();
        extra.push(0);
        invalid.push(extra);
        for length in 1..state.len() {
            invalid.push(state[..length].to_vec());
        }
        let mut restored = Scanner::new(profile);
        for bytes in invalid {
            restored.restore(&bytes);
            assert_eq!(
                restored.snapshot(),
                reset,
                "{profile:?}: accepted {bytes:?}"
            );
        }
        for offset in [0, 1, 15] {
            let bytes = root.snapshot_at(offset);
            restored.restore(&bytes);
            assert_eq!(restored.snapshot_at(offset), bytes);
        }
    }
}

#[test]
fn declaration_root_stack_preserves_fixed_snapshot_capacity_and_sticky_poison() {
    for profile in PROFILES {
        let reset = Scanner::new(profile).snapshot();
        let mut scanner = Scanner::new(profile);
        declaration_open(&mut scanner, ROOT_BODY_START, 4);
        let mut declined = false;
        for _ in 0..SNAPSHOT_CAPACITY {
            let (found, input) = scanner.scan("item", 257, flags(&[PAREN_INDENT]));
            assert_eq!(input.offset, 0);
            assert_eq!(input.marked, Some(0));
            if !found {
                declined = true;
                break;
            }
            assert!(scanner.snapshot().len() <= SNAPSHOT_CAPACITY);
        }
        assert!(declined);
        let poison = scanner.snapshot();
        assert_eq!(poison.len(), 12);
        assert_eq!(poison[1], 2);
        assert_eq!(poison[2], 1);
        assert_ne!(poison, reset);
        let mut restored = Scanner::new(profile);
        restored.restore(&poison);
        assert_eq!(restored.snapshot(), poison);
        declaration_assert_decline(&mut restored, ";; next", &[DEDENT, DECLARATION_TERMINATOR]);
        let mut historical = poison.clone();
        historical[1] = 1;
        restored.restore(&historical);
        assert_eq!(restored.snapshot(), poison);
        restored.restore(&[]);
        assert_eq!(restored.snapshot(), reset);
    }
}

#[test]
fn declaration_single_extra_never_consumes_a_double_and_keeps_newline_priority() {
    assert_eq!(TOKEN_COUNT, 53);
    for profile in PROFILES {
        for source in [";", "; next", "; // untouched\nfollowing", "; (* c *)"] {
            let mut scanner = Scanner::new(profile);
            let before = scanner.snapshot();
            let (found, input) = scanner.scan(source, 0, flags(&[SINGLE_SEMICOLON_TRIVIA]));
            assert!(found);
            assert_eq!(
                input.lexer.result_symbol,
                u16::try_from(SINGLE_SEMICOLON_TRIVIA).unwrap()
            );
            assert_eq!(input.offset, 1);
            assert_eq!(input.marked, Some(1));
            assert_eq!(scanner.snapshot(), before);
        }
        for source in [";;", ";; next", ";;; next"] {
            let mut scanner = Scanner::new(profile);
            let before = scanner.snapshot();
            let (found, input) = scanner.scan(source, 0, flags(&[SINGLE_SEMICOLON_TRIVIA]));
            assert!(!found);
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(input.offset, 1);
            assert_eq!(input.marked, None);
            assert_eq!(scanner.snapshot(), before);
        }
        let mut root = Scanner::new(profile);
        declaration_open(&mut root, ROOT_BODY_START, 4);
        let before = root.snapshot();
        let (found, input) = root.scan(
            "; next",
            12,
            flags(&[NEWLINE, DEDENT, SINGLE_SEMICOLON_TRIVIA]),
        );
        assert!(found);
        assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
        assert_eq!(input.marked, Some(1));
        assert_eq!(root.snapshot(), before);
        declaration_assert_decline(&mut root, ";; next", &[NEWLINE, SINGLE_SEMICOLON_TRIVIA]);
        let mut base = Scanner::new(profile);
        let (found, input) = base.scan(";", 0, flags(&[NEWLINE, SINGLE_SEMICOLON_TRIVIA]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(SINGLE_SEMICOLON_TRIVIA).unwrap()
        );
        assert_eq!(input.marked, Some(1));
        assert_eq!(input.offset, 1);
    }
}

use super::*;
