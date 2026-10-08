// Numeric-loop header controls extend the retained 53-flag scanner harness.
// Exact owner words must not steal distinct identifiers through plain or comment peeks.
// These synthetic masks qualify token/state behavior, not F# syntax acceptance.

fn assert_header_frontier_replays(profile: Profile, source: &str, admitted: bool) {
    let mut scanner = marker_scanner(profile);
    let before = scanner.snapshot();
    let mut restored = Scanner::new(profile);
    restored.restore(&before);
    let (found, input) = scanner.scan(source, 12, flags(&[APPLICATION_ARGUMENT_START]));
    let (replayed, replay_input) = restored.scan(source, 12, flags(&[APPLICATION_ARGUMENT_START]));
    assert_eq!(found, admitted, "{profile:?}: {source:?}");
    assert_eq!(replayed, found, "{profile:?}: replay {source:?}");
    assert_eq!(
        input.lexer.result_symbol,
        if admitted {
            u16::try_from(APPLICATION_ARGUMENT_START).unwrap()
        } else {
            u16::MAX
        },
        "{profile:?}: {source:?}"
    );
    assert_eq!(replay_input.lexer.result_symbol, input.lexer.result_symbol);
    assert_eq!(input.marked, Some(0), "{profile:?}: {source:?}");
    assert_eq!(replay_input.marked, input.marked);
    assert_eq!(replay_input.offset, input.offset);
    assert_eq!(replay_input.advances, input.advances);
    assert_eq!(scanner.snapshot(), before, "{profile:?}: {source:?}");
    assert_eq!(
        restored.snapshot(),
        before,
        "{profile:?}: replay {source:?}"
    );
}

#[test]
fn numeric_header_owner_words_decline_plain_and_comment_argument_markers() {
    for profile in PROFILES {
        for head in ["to", "to 2", "to(", "downto", "downto 2", "downto("] {
            for prefix in [" ", " (*c*)", " (*outer(*inner*)*) "] {
                let source = format!("{prefix}{head}");
                assert_header_frontier_replays(profile, &source, false);
            }
        }
    }
}

#[test]
fn numeric_header_prefix_unicode_and_backtick_identifiers_keep_argument_markers() {
    for profile in PROFILES {
        for head in [
            "total",
            "toValue",
            "to_1",
            "to'",
            "downtoValue",
            "downto_1",
            "downto'",
            "toλ",
            "downtoλ",
            "to\u{301}",
            "downto\u{301}",
            "λto",
            "``to``",
            "``downto``",
        ] {
            for prefix in [" ", " (*c*)", " (*outer(*inner*)*) "] {
                let source = format!("{prefix}{head}");
                assert_header_frontier_replays(profile, &source, true);
            }
        }
    }
}
