// Direct controls for ordinary slash-led INDENT admission in the retained scanner harness.
// Include after fsharp_scanner.rs so both factories and its bounded FFI helpers remain authoritative.
// Native snapshots are compared with ordinary written anchors, never authored wire frames.

#[test]
fn slash_led_ordinary_indent_matches_the_native_item_anchor() {
    for profile in PROFILES {
        for column in COLUMNS {
            for newline in ["", "\n", "\r\n"] {
                let padding = if newline.is_empty() {
                    String::new()
                } else {
                    " ".repeat(usize::try_from(column).unwrap())
                };
                let prefix = format!("{newline}{padding}");
                let start_column = if newline.is_empty() { column } else { 0 };
                let mut ordinary = Scanner::new(profile);
                let before = ordinary.snapshot();
                let (found, input) =
                    ordinary.scan(&format!("{prefix}item"), start_column, flags(&[INDENT]));
                assert!(
                    found,
                    "{profile:?}: ordinary anchor at {column}, {newline:?}"
                );
                assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
                assert_eq!(input.marked, Some(0));
                let expected = ordinary.snapshot();
                assert_ne!(expected, before);

                for head in ["/", "/unit"] {
                    let source = format!("{prefix}{head}");
                    let mut scanner = Scanner::new(profile);
                    let (found, input) = scanner.scan(&source, start_column, flags(&[INDENT]));
                    assert!(
                        found,
                        "{profile:?}: slash anchor {head:?} at {column}, {newline:?}"
                    );
                    assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
                    assert_eq!(input.marked, Some(0));
                    assert_eq!(input.offset, prefix.chars().count() + 1);
                    assert_eq!(scanner.snapshot(), expected);
                }
            }
        }
    }
}

#[test]
fn slash_line_comments_keep_the_original_indent_decline_and_frontier() {
    for profile in PROFILES {
        for column in COLUMNS {
            for newline in ["", "\n", "\r\n"] {
                let padding = if newline.is_empty() {
                    String::new()
                } else {
                    " ".repeat(usize::try_from(column).unwrap())
                };
                let prefix = format!("{newline}{padding}");
                let start_column = if newline.is_empty() { column } else { 0 };
                for comment in ["//", "// unit", "// unit\n/unit"] {
                    let mut scanner = Scanner::new(profile);
                    let before = scanner.snapshot();
                    let (found, input) = scanner.scan(
                        &format!("{prefix}{comment}"),
                        start_column,
                        flags(&[INDENT]),
                    );
                    assert!(
                        !found,
                        "{profile:?}: comment {comment:?} at {column}, {newline:?}"
                    );
                    assert_eq!(input.lexer.result_symbol, u16::MAX);
                    assert_eq!(input.marked, Some(0));
                    assert_eq!(input.offset, prefix.chars().count() + 1);
                    assert_eq!(input.lexer.lookahead, i32::from(b'/'));
                    assert_eq!(scanner.snapshot(), before);
                }
            }
        }
    }
}

#[test]
fn slash_indent_does_not_escape_missing_recovery_string_or_owner_masks() {
    const TRY_BODY_INDENT: usize = 35;
    const ROOT_BODY_INDENT: usize = 45;

    let cases = [
        ("no tokens", flags(&[]), None),
        ("extra only", flags(&[PREPROC_INACTIVE]), None),
        ("layout without indent", flags(&[NEWLINE, DEDENT]), Some(0)),
        ("recovery only", flags(&[ERROR_SENTINEL]), Some(0)),
        ("indent recovery", flags(&[INDENT, ERROR_SENTINEL]), Some(0)),
        ("inside string", flags(&[INDENT, INSIDE_STRING]), None),
        ("try owner", flags(&[INDENT, TRY_BODY_INDENT]), Some(0)),
        ("do owner", flags(&[INDENT, DO_BODY_INDENT]), Some(0)),
        ("root owner", flags(&[INDENT, ROOT_BODY_INDENT]), Some(0)),
        ("all-valid recovery", [true; TOKEN_COUNT], Some(0)),
    ];
    for profile in PROFILES {
        for column in COLUMNS {
            for newline in ["", "\n", "\r\n"] {
                let padding = if newline.is_empty() {
                    String::new()
                } else {
                    " ".repeat(usize::try_from(column).unwrap())
                };
                let prefix = format!("{newline}{padding}");
                let start_column = if newline.is_empty() { column } else { 0 };
                for head in ["/", "/unit"] {
                    let source = format!("{prefix}{head}");
                    for (label, valid, marked) in cases {
                        let mut scanner = Scanner::new(profile);
                        let before = scanner.snapshot();
                        let (found, input) = scanner.scan(&source, start_column, valid);
                        assert!(
                            !found,
                            "{profile:?}: {label} admitted {head:?} at {column}, {newline:?}"
                        );
                        assert_eq!(input.lexer.result_symbol, u16::MAX);
                        assert_eq!(input.marked, marked);
                        assert_eq!(scanner.snapshot(), before);
                    }
                }
            }
        }
    }
}

#[test]
fn slash_indent_native_restore_and_replay_match_the_ordinary_anchor() {
    for profile in PROFILES {
        let mut seed = Scanner::new(profile);
        open_enum_body(&mut seed, 1);
        let before = seed.snapshot();
        for column in COLUMNS {
            for newline in ["", "\n", "\r\n"] {
                let padding = if newline.is_empty() {
                    String::new()
                } else {
                    " ".repeat(usize::try_from(column).unwrap())
                };
                let prefix = format!("{newline}{padding}");
                let start_column = if newline.is_empty() { column } else { 0 };
                for head in ["/", "/unit"] {
                    let mut ordinary = Scanner::new(profile);
                    ordinary.restore(&before);
                    let (found, input) =
                        ordinary.scan(&format!("{prefix}item"), start_column, flags(&[INDENT]));
                    assert!(found);
                    assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
                    assert_eq!(input.marked, Some(0));
                    let expected = ordinary.snapshot();

                    let mut original = Scanner::new(profile);
                    original.restore(&before);
                    let source = format!("{prefix}{head}");
                    let (found, input) = original.scan(&source, start_column, flags(&[INDENT]));
                    assert!(
                        found,
                        "{profile:?}: restored slash {head:?} at {column}, {newline:?}"
                    );
                    assert_eq!(input.lexer.result_symbol, u16::try_from(INDENT).unwrap());
                    assert_eq!(input.marked, Some(0));
                    assert_eq!(original.snapshot(), expected);

                    let mut restored = Scanner::new(profile);
                    restored.restore(&original.snapshot());
                    assert_eq!(restored.snapshot_at(1), expected);
                    for ending in ["\n", "\r\n"] {
                        let replay = format!(
                            "{ending}{}item",
                            " ".repeat(usize::try_from(column).unwrap())
                        );
                        let (found, input) = ordinary.scan(&replay, 0, flags(&[NEWLINE]));
                        assert!(found, "{profile:?}: ordinary aligned replay at {column}");
                        assert_eq!(input.lexer.result_symbol, u16::try_from(NEWLINE).unwrap());
                        assert_eq!(input.marked, Some(0));
                        let after_replay = ordinary.snapshot();
                        for scanner in [&mut original, &mut restored] {
                            let (actual_found, actual_input) =
                                scanner.scan(&replay, 0, flags(&[NEWLINE]));
                            assert_eq!(actual_found, found);
                            assert_eq!(actual_input.lexer.result_symbol, input.lexer.result_symbol);
                            assert_eq!(actual_input.marked, input.marked);
                            assert_eq!(actual_input.offset, input.offset);
                            assert_eq!(actual_input.advances, input.advances);
                            assert_eq!(scanner.snapshot(), after_replay);
                        }
                    }
                }
            }
        }
    }
}
