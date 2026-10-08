// Direct controls for offside declaration recovery in the retained private scanner harness.
// Include after fsharp_scanner.rs; its factories, native snapshots and bounded callbacks remain authoritative.
// A declined lexical peek commits no token even when it has inspected beyond its original mark.

const OFFSIDE_SINGLE_SEMICOLON_EXTRA: usize = 46;

fn offside_extra_only() -> [bool; TOKEN_COUNT] {
    flags(&[PREPROC_INACTIVE, OFFSIDE_SINGLE_SEMICOLON_EXTRA])
}

fn offside_normal_owner(profile: Profile, column: u32, ending: &str) -> Scanner {
    let mut scanner = Scanner::new(profile);
    let source = if ending.is_empty() {
        "item".to_owned()
    } else {
        format!(
            "{ending}{}item",
            " ".repeat(usize::try_from(column).unwrap())
        )
    };
    let start_column = if ending.is_empty() { column } else { 0 };
    let (found, input) = scanner.scan(&source, start_column, flags(&[INDENT]));
    assert!(found, "{profile:?}: ordinary owner at {column}, {ending:?}");
    assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
    assert_eq!(input.marked, Some(0));
    scanner
}

fn offside_assert_declined(
    scanner: &mut Scanner,
    source: &str,
    column: u32,
    valid: [bool; TOKEN_COUNT],
) {
    let before = scanner.snapshot();
    let (found, input) = scanner.scan(source, column, valid);
    assert!(
        !found,
        "{:?}: unexpected offside token for {source:?}",
        scanner.profile
    );
    assert_eq!(input.lexer.result_symbol, u16::MAX);
    assert_eq!(scanner.snapshot(), before);
}

#[test]
fn extra_only_offside_declarations_close_the_native_ordinary_owner_before_the_keyword() {
    for profile in PROFILES {
        for column in COLUMNS {
            for anchor_ending in ["", "\n", "\r\n"] {
                let mut seed = offside_normal_owner(profile, column, anchor_ending);
                let owned = seed.snapshot();
                for ending in ["\n", "\r\n"] {
                    let mut ordinary = Scanner::new(profile);
                    ordinary.restore(&owned);
                    let (found, input) =
                        ordinary.scan(&format!("{ending}item"), column, flags(&[DEDENT]));
                    assert!(found);
                    assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                    assert_eq!(input.marked, Some(0));
                    let closed = ordinary.snapshot();
                    assert_eq!(closed, Scanner::new(profile).snapshot());

                    for declaration in ["let sentinel = 73", "val sentinel: int"] {
                        let source = format!("{ending}{declaration}");
                        let mut scanner = Scanner::new(profile);
                        scanner.restore(&owned);
                        let (found, input) = scanner.scan(&source, column, offside_extra_only());
                        assert!(
                            found,
                            "{profile:?}: {declaration:?} at {column}, {anchor_ending:?}"
                        );
                        assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                        assert_eq!(input.marked, Some(0));
                        assert_eq!(input.offset, ending.chars().count() + 3);
                        assert_eq!(input.lexer.lookahead, i32::from(b' '));
                        assert_eq!(scanner.snapshot(), closed);
                    }
                }
            }
        }
    }
}

#[test]
fn extra_only_offside_prefixes_and_continuations_do_not_commit_layout_tokens() {
    let prefixes = [
        "letter", "letValue", "val1", "value", "let'", "val'", "let_", "val_", "letΩ", "valΩ",
        "``let``", "``val``", "let!", "val!",
    ];
    for profile in PROFILES {
        for column in COLUMNS {
            let mut seed = offside_normal_owner(profile, column, "");
            let owned = seed.snapshot();
            for ending in ["\n", "\r\n"] {
                for prefix in prefixes {
                    let mut scanner = Scanner::new(profile);
                    scanner.restore(&owned);
                    offside_assert_declined(
                        &mut scanner,
                        &format!("{ending}{prefix} sentinel"),
                        column,
                        offside_extra_only(),
                    );
                }
                for next_column in [column, column.checked_add(1).unwrap()] {
                    for declaration in ["let sentinel = 73", "val sentinel: int"] {
                        let source = format!(
                            "{ending}{}{declaration}",
                            " ".repeat(usize::try_from(next_column).unwrap())
                        );
                        let mut scanner = Scanner::new(profile);
                        scanner.restore(&owned);
                        offside_assert_declined(
                            &mut scanner,
                            &source,
                            column,
                            offside_extra_only(),
                        );
                    }
                }
            }
            for standalone in ["\r", "\x0c"] {
                for declaration in ["let sentinel = 73", "val sentinel: int"] {
                    let mut scanner = Scanner::new(profile);
                    scanner.restore(&owned);
                    offside_assert_declined(
                        &mut scanner,
                        &format!("{standalone}{declaration}"),
                        column,
                        offside_extra_only(),
                    );
                }
            }
            for source in ["let sentinel = 73", " val sentinel: int"] {
                let mut scanner = Scanner::new(profile);
                scanner.restore(&owned);
                offside_assert_declined(&mut scanner, source, column, offside_extra_only());
            }
        }
        let mut nested = offside_normal_owner(profile, 4, "");
        let (found, input) = nested.scan("item", 8, flags(&[INDENT]));
        assert!(found);
        assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
        for ending in ["\n", "\r\n"] {
            for declaration in ["let sentinel = 73", "val sentinel: int"] {
                offside_assert_declined(
                    &mut nested,
                    &format!("{ending}      {declaration}"),
                    8,
                    offside_extra_only(),
                );
            }
        }
    }
}

#[test]
fn extra_only_offside_recovery_respects_native_owner_string_and_preprocessor_barriers() {
    const OFFSIDE_TRY_INDENT: usize = 35;
    const OFFSIDE_ROOT_BODY_INDENT: usize = 45;

    let protected = [
        PAREN_INDENT,
        BRACE_INDENT,
        TYPE_APP_INDENT,
        DO_BODY_INDENT,
        OFFSIDE_TRY_INDENT,
        OFFSIDE_ROOT_BODY_INDENT,
    ];
    for profile in PROFILES {
        for column in COLUMNS {
            for ending in ["\n", "\r\n"] {
                for declaration in ["let sentinel = 73", "val sentinel: int"] {
                    let source = format!("{ending}{declaration}");
                    for empty_stack in [false, true] {
                        let mut scanner = Scanner::new(profile);
                        if empty_stack {
                            pop_base(&mut scanner);
                        }
                        offside_assert_declined(
                            &mut scanner,
                            &source,
                            column,
                            offside_extra_only(),
                        );
                    }
                    let mut first_level = Scanner::new(profile);
                    pop_base(&mut first_level);
                    open_enum_body(&mut first_level, column);
                    offside_assert_declined(
                        &mut first_level,
                        &source,
                        column,
                        offside_extra_only(),
                    );
                    for token in protected {
                        let mut scanner = Scanner::new(profile);
                        let (anchor, start_column) = if token == TYPE_APP_INDENT {
                            (
                                format!(
                                    "{ending}{}item",
                                    " ".repeat(usize::try_from(column).unwrap())
                                ),
                                0,
                            )
                        } else {
                            ("item".to_owned(), column)
                        };
                        let (found, input) = scanner.scan(&anchor, start_column, flags(&[token]));
                        assert!(found, "{profile:?}: protected owner token {token}");
                        assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
                        offside_assert_declined(
                            &mut scanner,
                            &source,
                            column,
                            offside_extra_only(),
                        );
                    }
                    let mut string = offside_normal_owner(profile, column, "");
                    offside_assert_declined(
                        &mut string,
                        &source,
                        column,
                        flags(&[
                            PREPROC_INACTIVE,
                            OFFSIDE_SINGLE_SEMICOLON_EXTRA,
                            INSIDE_STRING,
                        ]),
                    );
                }
            }
        }
        for token in [PAREN_INDENT, BRACE_INDENT, TYPE_APP_INDENT] {
            let mut scanner = Scanner::new(profile);
            let (anchor, column) = if token == TYPE_APP_INDENT {
                ("\n    item", 0)
            } else {
                ("item", 4)
            };
            let (found, input) = scanner.scan(anchor, column, flags(&[token]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
            let (found, input) = scanner.scan("item", 8, flags(&[INDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
            for ending in ["\n", "\r\n"] {
                offside_assert_declined(
                    &mut scanner,
                    &format!("{ending}let sentinel = 73"),
                    8,
                    offside_extra_only(),
                );
            }
        }
        let mut conditional = offside_normal_owner(profile, 4, "");
        let (found, input) = conditional.scan("#if condition\n    item", 4, flags(&[PREPROC_IF]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(PREPROC_IF).unwrap()
        );
        let (found, input) = conditional.scan("item", 8, flags(&[INDENT]));
        assert!(found);
        assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
        for ending in ["\n", "\r\n"] {
            for declaration in ["let sentinel = 73", "val sentinel: int"] {
                offside_assert_declined(
                    &mut conditional,
                    &format!("{ending}  {declaration}"),
                    8,
                    offside_extra_only(),
                );
            }
        }
    }
}

#[test]
fn extra_only_offside_declaration_closure_replays_from_native_snapshots() {
    for profile in PROFILES {
        for column in COLUMNS {
            let mut seed = offside_normal_owner(profile, column, "");
            let outer = seed.snapshot();
            let inner_column = column.checked_add(1).unwrap();
            let (found, input) = seed.scan("item", inner_column, flags(&[INDENT]));
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
            let owned = seed.snapshot();
            assert_eq!(seed.snapshot_at(1), owned);
            for ending in ["\n", "\r\n"] {
                let padding = " ".repeat(usize::try_from(column).unwrap());
                let ordinary_source = format!("{ending}{padding}item");
                let mut ordinary = Scanner::new(profile);
                ordinary.restore(&owned);
                let (found, input) =
                    ordinary.scan(&ordinary_source, inner_column, flags(&[DEDENT]));
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                assert_eq!(input.marked, Some(0));
                assert_eq!(ordinary.snapshot(), outer);

                for declaration in ["let sentinel = 73", "val sentinel: int"] {
                    let source = format!("{ending}{padding}{declaration}");
                    let mut original = Scanner::new(profile);
                    original.restore(&owned);
                    let mut restored = Scanner::new(profile);
                    restored.restore(&original.snapshot_at(1));
                    for scanner in [&mut original, &mut restored] {
                        let (found, input) =
                            scanner.scan(&source, inner_column, offside_extra_only());
                        assert!(found);
                        assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                        assert_eq!(input.marked, Some(0));
                        assert_eq!(input.offset, ending.chars().count() + padding.len() + 3);
                        assert_eq!(scanner.snapshot(), outer);
                    }
                    let mut baseline = Scanner::new(profile);
                    baseline.restore(&outer);
                    let (found, input) =
                        baseline.scan(&ordinary_source, inner_column, flags(&[NEWLINE]));
                    assert!(found);
                    assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
                    let replayed = baseline.snapshot();
                    for scanner in [&mut original, &mut restored] {
                        let (actual_found, actual_input) =
                            scanner.scan(&ordinary_source, inner_column, flags(&[NEWLINE]));
                        assert_eq!(actual_found, found);
                        assert_eq!(actual_input.lexer.result_symbol, input.lexer.result_symbol);
                        assert_eq!(actual_input.marked, input.marked);
                        assert_eq!(actual_input.offset, input.offset);
                        assert_eq!(actual_input.advances, input.advances);
                        assert_eq!(scanner.snapshot(), replayed);
                    }
                }
            }
        }
        let donor = native_do_anchor_state(profile, INDENT, "stranded");
        for ending in ["\n", "\r\n"] {
            for declaration in ["let sentinel = 73", "val sentinel: int"] {
                let source = format!("{ending}    {declaration}");
                let mut ordinary = Scanner::new(profile);
                ordinary.restore(&donor);
                let (found, input) = ordinary.scan(&source, 20, flags(&[DEDENT]));
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
                assert_eq!(input.marked, Some(0));
                let closed = ordinary.snapshot();

                let mut recovered = Scanner::new(profile);
                recovered.restore(&donor);
                assert_eq!(recovered.snapshot_at(1), donor);
                let (actual_found, actual_input) =
                    recovered.scan(&source, 20, offside_extra_only());
                assert_eq!(actual_found, found);
                assert_eq!(actual_input.lexer.result_symbol, input.lexer.result_symbol);
                assert_eq!(actual_input.marked, Some(0));
                assert_eq!(recovered.snapshot(), closed);

                let mut restored = Scanner::new(profile);
                restored.restore(&recovered.snapshot_at(1));
                let replay = format!("{ending}    item");
                let (found, input) = ordinary.scan(&replay, 20, flags(&[NEWLINE]));
                assert!(found);
                assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
                let replayed = ordinary.snapshot();
                for scanner in [&mut recovered, &mut restored] {
                    let (actual_found, actual_input) = scanner.scan(&replay, 20, flags(&[NEWLINE]));
                    assert_eq!(actual_found, found);
                    assert_eq!(actual_input.lexer.result_symbol, input.lexer.result_symbol);
                    assert_eq!(actual_input.marked, input.marked);
                    assert_eq!(actual_input.offset, input.offset);
                    assert_eq!(actual_input.advances, input.advances);
                    assert_eq!(scanner.snapshot(), replayed);
                }
            }
        }
    }
}
