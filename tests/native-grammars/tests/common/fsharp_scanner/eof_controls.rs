// Prospective EOF-single scanner controls appended to the frozen 53-flag helper.
// Shared masks test protocol admission and replay, not signature grammar reachability.
// ROOT eligibility never grants closure to NORMAL, TRY or protected delimiter owners.

const EOF_SINGLE_ROOT: usize = 45;
const EOF_SINGLE_EXTRA: usize = 46;
const EOF_SINGLE_DECL_TERM: usize = 44;

fn eof_single_open(scanner: &mut Scanner, token: usize, column: u32) {
    let source = if token == TYPE_APP_INDENT {
        format!("\n{}item", " ".repeat(usize::try_from(column).unwrap()))
    } else {
        "item".to_owned()
    };
    let start = if token == TYPE_APP_INDENT { 0 } else { column };
    let (found, input) = scanner.scan(&source, start, flags(&[token]));
    assert!(found, "{:?}: owner token {token}", scanner.profile);
    assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
    assert_eq!(input.marked, Some(0));
}

fn eof_single_token(scanner: &mut Scanner, source: &str, tokens: &[usize], expected: usize) {
    let before = scanner.snapshot();
    let (found, input) = scanner.scan(source, 22, flags(tokens));
    assert!(found, "{:?}: {source:?}", scanner.profile);
    assert_eq!(input.lexer.result_symbol, u16::try_from(expected).unwrap());
    assert_eq!(input.marked, Some(source.find(';').unwrap() + 1));
    assert_eq!(
        scanner.snapshot(),
        before,
        "a single extra must never drain an owner"
    );
}

#[test]
fn eof_single_extra_retains_root_do_snapshots_then_ordinary_eof_closes_one_owner() {
    assert_eq!(TOKEN_COUNT, 53);
    for profile in PROFILES {
        for do_depth in 0..=2 {
            let mut original = Scanner::new(profile);
            let mut levels = vec![original.snapshot()];
            eof_single_open(&mut original, EOF_SINGLE_ROOT, 4);
            levels.push(original.snapshot());
            for level in 0..do_depth {
                eof_single_open(&mut original, DO_BODY_INDENT, 8 + level * 4);
                levels.push(original.snapshot());
            }
            let mut restored = Scanner::new(profile);
            restored.restore(levels.last().unwrap());
            for current in [&mut original, &mut restored] {
                eof_single_token(
                    current,
                    ";",
                    &[NEWLINE, DEDENT, EOF_SINGLE_EXTRA],
                    EOF_SINGLE_EXTRA,
                );
            }
            for previous in levels[..levels.len() - 1].iter().rev() {
                for current in [&mut original, &mut restored] {
                    let (found, input) = current.scan("", 0, flags(&[DEDENT]));
                    assert!(found);
                    assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                    assert_eq!(input.marked, Some(0));
                    assert_eq!(current.snapshot(), *previous);
                }
            }
        }
    }
}

#[test]
fn eof_single_extra_preserves_its_byte_through_padding_and_line_comment_peeks() {
    for profile in PROFILES {
        for source in [";", " ;", "; ", ";\n", ";\r\n", "; // retained"] {
            let mut original = Scanner::new(profile);
            eof_single_open(&mut original, EOF_SINGLE_ROOT, 4);
            let before = original.snapshot();
            let mut restored = Scanner::new(profile);
            restored.restore(&before);
            for current in [&mut original, &mut restored] {
                eof_single_token(
                    current,
                    source,
                    &[NEWLINE, DEDENT, EOF_SINGLE_EXTRA],
                    EOF_SINGLE_EXTRA,
                );
            }
        }
    }
}

#[test]
fn eof_single_ordinary_and_protected_owner_barriers_keep_old_separator_not_root_extra() {
    for profile in PROFILES {
        for token in [INDENT, 35, PAREN_INDENT, TYPE_APP_INDENT, BRACE_INDENT] {
            let mut original = Scanner::new(profile);
            eof_single_open(&mut original, EOF_SINGLE_ROOT, 4);
            eof_single_open(&mut original, token, 8);
            let before = original.snapshot();
            let mut restored = Scanner::new(profile);
            restored.restore(&before);
            for current in [&mut original, &mut restored] {
                eof_single_token(current, ";", &[NEWLINE, DEDENT, EOF_SINGLE_EXTRA], NEWLINE);
            }
        }
    }
}

#[test]
fn eof_single_recovery_string_quote_and_preprocessor_masks_do_not_gain_root_extra() {
    for profile in PROFILES {
        for guard in [ERROR_SENTINEL, INSIDE_STRING, 22, 23] {
            let mut scanner = Scanner::new(profile);
            eof_single_open(&mut scanner, EOF_SINGLE_ROOT, 4);
            let before = scanner.snapshot();
            let (found, input) =
                scanner.scan(";", 22, flags(&[NEWLINE, DEDENT, EOF_SINGLE_EXTRA, guard]));
            if guard == INSIDE_STRING {
                assert!(!found);
                assert_eq!(input.lexer.result_symbol, u16::MAX);
            } else {
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
                assert_eq!(input.marked, Some(1));
            }
            assert_eq!(scanner.snapshot(), before);
        }
        let mut directive = Scanner::new(profile);
        eof_single_open(&mut directive, EOF_SINGLE_ROOT, 4);
        let (found, input) = directive.scan("#if flag\nitem", 8, flags(&[PREPROC_IF]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(PREPROC_IF).unwrap()
        );
        eof_single_token(
            &mut directive,
            ";",
            &[NEWLINE, DEDENT, EOF_SINGLE_EXTRA],
            NEWLINE,
        );
    }
}

#[test]
fn eof_single_missing_extra_mask_and_non_eof_continuations_stay_existing_separators() {
    for profile in PROFILES {
        // Declining the block opener preserves the owner while its trivia remains.
        let assert_block_decline = |scanner: &mut Scanner| {
            let before = scanner.snapshot();
            let (found, input) = scanner.scan(
                "; (* retained *)",
                22,
                flags(&[NEWLINE, DEDENT, EOF_SINGLE_EXTRA]),
            );
            assert!(!found);
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(input.offset, 4);
            assert_eq!(input.marked, Some(1));
            assert_eq!(input.column, 26);
            assert_eq!(input.lexer.lookahead, i32::from(b' '));
            assert_eq!(input.advances, 4);
            assert_eq!(input.budget, 192);
            assert!(!input.watchdog);
            assert!(!input.advanced_past_end);
            assert_eq!(scanner.snapshot(), before);
        };

        let mut fresh = Scanner::new(profile);
        eof_single_open(&mut fresh, EOF_SINGLE_ROOT, 4);
        let fresh_state = fresh.snapshot();
        let mut fresh_restored = Scanner::new(profile);
        fresh_restored.restore(&fresh_state);
        for current in [&mut fresh, &mut fresh_restored] {
            assert_eq!(current.snapshot(), fresh_state);
            assert_block_decline(current);
        }
        assert_eq!(fresh.snapshot(), fresh_restored.snapshot());

        let mut original = Scanner::new(profile);
        eof_single_open(&mut original, EOF_SINGLE_ROOT, 4);
        let retained = original.snapshot();
        let mut restored = Scanner::new(profile);
        restored.restore(&retained);
        for current in [&mut original, &mut restored] {
            eof_single_token(current, ";", &[NEWLINE, DEDENT], NEWLINE);
        }
        assert_eq!(original.snapshot(), restored.snapshot());
        for current in [&mut original, &mut restored] {
            eof_single_token(
                current,
                "; next",
                &[NEWLINE, DEDENT, EOF_SINGLE_EXTRA],
                NEWLINE,
            );
        }
        assert_eq!(original.snapshot(), restored.snapshot());
        for current in [&mut original, &mut restored] {
            assert_block_decline(current);
        }
        assert_eq!(original.snapshot(), restored.snapshot());

        for current in [&mut original, &mut restored] {
            let before = current.snapshot();
            let (found, input) = current.scan(
                ";;",
                22,
                flags(&[NEWLINE, EOF_SINGLE_EXTRA, EOF_SINGLE_DECL_TERM]),
            );
            assert!(!found);
            assert_eq!(input.lexer.result_symbol, u16::MAX);
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, 1);
            assert_eq!(input.column, 23);
            assert_eq!(input.lexer.lookahead, i32::from(b';'));
            assert_eq!(input.advances, 1);
            assert_eq!(current.snapshot(), before);
        }
        assert_eq!(original.snapshot(), restored.snapshot());
        for current in [&mut original, &mut restored] {
            let (found, input) = current.scan(
                ";;",
                22,
                flags(&[NEWLINE, DEDENT, EOF_SINGLE_EXTRA, EOF_SINGLE_DECL_TERM]),
            );
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(0));
            assert_eq!(input.offset, 1);
            assert_eq!(input.column, 23);
            assert_eq!(input.lexer.lookahead, i32::from(b';'));
            assert_eq!(input.advances, 1);
            assert_eq!(current.snapshot(), Scanner::new(profile).snapshot());
        }
        assert_eq!(original.snapshot(), restored.snapshot());
    }
}

#[test]
fn eof_single_native_base_remains_existing_extra_without_root_admission() {
    for profile in PROFILES {
        for source in [";", ";\n", ";\r\n", "; // retained"] {
            let mut original = Scanner::new(profile);
            let before = original.snapshot();
            let mut restored = Scanner::new(profile);
            restored.restore(&before);
            for current in [&mut original, &mut restored] {
                eof_single_token(
                    current,
                    source,
                    &[NEWLINE, EOF_SINGLE_EXTRA],
                    EOF_SINGLE_EXTRA,
                );
            }
        }
    }
}
