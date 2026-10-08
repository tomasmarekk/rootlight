//! Prospective single-semicolon module-list scanner controls over the retained 53-flag helper.
//! Signature masks are synthetic protocol checks, not proof of a reachable signature element list.

const MODULE_SINGLE_ELEMENT_SEPARATOR: usize = 37;
const MODULE_SINGLE_APPLICATION_START: usize = 43;
const MODULE_SINGLE_EXTRA: usize = 46;

fn module_single_open(scanner: &mut Scanner, token: usize) {
    let source = if token == 31 {
        "\n        item"
    } else {
        "item"
    };
    let column = if token == 31 { 0 } else { 8 };
    let (found, input) = scanner.scan(source, column, flags(&[token]));
    assert!(found, "{:?}: owner {token}", scanner.profile);
    assert_eq!(input.lexer.result_symbol, u16::try_from(token).unwrap());
    assert_eq!(input.marked, Some(0));
}

fn module_single_assert_token(
    scanner: &mut Scanner,
    source: &str,
    tokens: &[usize],
    expected: usize,
    end: usize,
) {
    let before = scanner.snapshot();
    let (found, input) = scanner.scan(source, 12, flags(tokens));
    assert!(found, "{:?}: {source:?}, {tokens:?}", scanner.profile);
    assert_eq!(input.lexer.result_symbol, u16::try_from(expected).unwrap());
    assert_eq!(input.marked, Some(end));
    assert_eq!(scanner.snapshot(), before);
}

#[test]
fn written_single_module_separator_consumes_only_its_byte_and_replays_at_native_base() {
    assert_eq!(TOKEN_COUNT, 53);
    for profile in PROFILES {
        for tokens in [
            vec![MODULE_SINGLE_ELEMENT_SEPARATOR, MODULE_SINGLE_EXTRA],
            vec![
                PREPROC_INACTIVE,
                MODULE_SINGLE_ELEMENT_SEPARATOR,
                MODULE_SINGLE_EXTRA,
            ],
            vec![
                PREPROC_INACTIVE,
                MODULE_SINGLE_ELEMENT_SEPARATOR,
                MODULE_SINGLE_EXTRA,
                MODULE_SINGLE_APPLICATION_START,
            ],
        ] {
            for source in [
                "; ()",
                " ; ()",
                "\t; ()",
                "\n; ()",
                "\r\n; ()",
                "; // c\n()",
                "; (* c *) ()",
                ";",
            ] {
                let mut seed = Scanner::new(profile);
                let before = seed.snapshot();
                let expected_end = source.find(';').unwrap() + 1;
                let mut restored = Scanner::new(profile);
                restored.restore(&before);
                for scanner in [&mut seed, &mut restored] {
                    module_single_assert_token(
                        scanner,
                        source,
                        &tokens,
                        MODULE_SINGLE_ELEMENT_SEPARATOR,
                        expected_end,
                    );
                }
                assert!(expected_end > 0);
            }
        }
    }
}

#[test]
fn written_single_module_separator_keeps_newline_and_owed_dedent_priority() {
    for profile in PROFILES {
        let tokens = [
            NEWLINE,
            MODULE_SINGLE_ELEMENT_SEPARATOR,
            MODULE_SINGLE_EXTRA,
        ];
        let mut base = Scanner::new(profile);
        module_single_assert_token(&mut base, "; next", &tokens, NEWLINE, 1);

        let mut root = Scanner::new(profile);
        module_single_open(&mut root, 45);
        let before = root.snapshot();
        let mut replay = Scanner::new(profile);
        replay.restore(&before);
        for scanner in [&mut root, &mut replay] {
            module_single_assert_token(
                scanner,
                "; next",
                &[
                    NEWLINE,
                    DEDENT,
                    MODULE_SINGLE_ELEMENT_SEPARATOR,
                    MODULE_SINGLE_EXTRA,
                ],
                NEWLINE,
                1,
            );
            let (found, input) = scanner.scan(
                ";\nnext",
                12,
                flags(&[
                    NEWLINE,
                    DEDENT,
                    MODULE_SINGLE_ELEMENT_SEPARATOR,
                    MODULE_SINGLE_EXTRA,
                ]),
            );
            assert!(found);
            assert_eq!(input.lexer.result_symbol, u16::try_from(DEDENT).unwrap());
            assert_eq!(input.marked, Some(1));
            assert_eq!(scanner.snapshot(), Scanner::new(profile).snapshot());
        }
    }
}

#[test]
fn written_single_module_separator_never_consumes_an_unrequested_double() {
    for profile in PROFILES {
        for tokens in [
            vec![
                PREPROC_INACTIVE,
                MODULE_SINGLE_ELEMENT_SEPARATOR,
                MODULE_SINGLE_EXTRA,
            ],
            vec![
                MODULE_SINGLE_ELEMENT_SEPARATOR,
                MODULE_SINGLE_EXTRA,
                MODULE_SINGLE_APPLICATION_START,
            ],
        ] {
            for source in [";; next", " ;; next", "\r\n;; next", ";;; next"] {
                let mut seed = Scanner::new(profile);
                let before = seed.snapshot();
                let mut replay = Scanner::new(profile);
                replay.restore(&before);
                for scanner in [&mut seed, &mut replay] {
                    let (found, input) = scanner.scan(source, 12, flags(&tokens));
                    assert!(!found);
                    assert_eq!(input.lexer.result_symbol, u16::MAX);
                    assert_eq!(input.lexer.lookahead, i32::from(b';'));
                    assert!(
                        input
                            .marked
                            .is_none_or(|end| end <= source.find(';').unwrap())
                    );
                    assert_eq!(scanner.snapshot(), before);
                }
            }
        }
    }
}

#[test]
fn written_single_module_separator_cannot_escape_owners_directives_or_lexical_guards() {
    for profile in PROFILES {
        let tokens = [
            PREPROC_INACTIVE,
            MODULE_SINGLE_ELEMENT_SEPARATOR,
            MODULE_SINGLE_EXTRA,
        ];
        for owner in [INDENT, PAREN_INDENT, 31, 35, 38, 41, 45] {
            let mut seed = Scanner::new(profile);
            module_single_open(&mut seed, owner);
            let before = seed.snapshot();
            let mut replay = Scanner::new(profile);
            replay.restore(&before);
            for scanner in [&mut seed, &mut replay] {
                module_single_assert_token(scanner, "; next", &tokens, MODULE_SINGLE_EXTRA, 1);
                let (found, input) = scanner.scan("\nnext", 12, flags(&tokens));
                assert!(
                    !found,
                    "an ELEM-only mask must not enter general offside closure"
                );
                assert_eq!(input.lexer.result_symbol, u16::MAX);
                assert_eq!(scanner.snapshot(), before);
            }
        }

        let mut directive = Scanner::new(profile);
        let (found, input) = directive.scan("#if ready\nitem", 0, flags(&[PREPROC_IF]));
        assert!(found);
        assert_eq!(
            input.lexer.result_symbol,
            u16::try_from(PREPROC_IF).unwrap()
        );
        module_single_assert_token(&mut directive, "; next", &tokens, MODULE_SINGLE_EXTRA, 1);
        let before = directive.snapshot();
        let mut restored = Scanner::new(profile);
        restored.restore(&before);
        module_single_assert_token(&mut restored, "; next", &tokens, MODULE_SINGLE_EXTRA, 1);

        let mut dollars = Scanner::new(profile);
        let (found, input) = dollars.scan("$$\"\"\"neutral", 0, flags(&[24]));
        assert!(found);
        assert_eq!(input.lexer.result_symbol, 24);
        module_single_assert_token(&mut dollars, "; next", &tokens, MODULE_SINGLE_EXTRA, 1);
        let before = dollars.snapshot();
        let mut restored = Scanner::new(profile);
        restored.restore(&before);
        module_single_assert_token(&mut restored, "; next", &tokens, MODULE_SINGLE_EXTRA, 1);

        for guard in [ERROR_SENTINEL, INSIDE_STRING, 22, 23] {
            let mut scanner = Scanner::new(profile);
            let before = scanner.snapshot();
            let (found, input) = scanner.scan(
                "; next",
                12,
                flags(&[MODULE_SINGLE_ELEMENT_SEPARATOR, MODULE_SINGLE_EXTRA, guard]),
            );
            assert_ne!(
                input.lexer.result_symbol,
                u16::try_from(MODULE_SINGLE_ELEMENT_SEPARATOR).unwrap()
            );
            if found {
                assert_eq!(
                    input.lexer.result_symbol,
                    u16::try_from(MODULE_SINGLE_EXTRA).unwrap()
                );
                assert_eq!(input.marked, Some(1));
            } else {
                assert_eq!(input.lexer.result_symbol, u16::MAX);
            }
            assert_eq!(scanner.snapshot(), before);
        }
    }
}

use super::*;
