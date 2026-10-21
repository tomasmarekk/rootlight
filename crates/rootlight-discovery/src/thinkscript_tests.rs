//! ThinkScript recognition requires written distinctive syntax in ambiguous text.
//! Literal/comment examples and unrelated suffixes cannot donate a source route.

use super::*;

fn signals(path: &str, bytes: &[u8]) -> Vec<String> {
    classify(&RelativePath::parse(Path::new(path)).unwrap(), bytes)
        .1
        .into_iter()
        .filter(|signal| signal.evidence == LanguageEvidence::Content)
        .map(|signal| signal.language)
        .collect()
}

#[test]
fn complete_original_and_renamed_script_input_syntax_route_shared_txt_only() {
    let original =
        include_bytes!("../../rootlight-adapters/tests/fixtures/thinkscript/VIX_FGMR.txt");
    assert_eq!(original.len(), 2565);
    assert_eq!(signals("ordinary.txt", original), ["thinkscript"]);
    assert_eq!(
        signals(
            "RENAMED.TXT",
            b"# comment\r\nscript Written { input width=2; plot value=width; }"
        ),
        ["thinkscript"]
    );
    assert_eq!(extension_language("ordinary.txt"), None);
    assert_eq!(canonical_language("thinkscript"), Some("thinkscript"));
    let row = language_capabilities()
        .iter()
        .find(|row| row.language == "thinkscript")
        .unwrap();
    assert!(row.suffixes.is_empty());
    assert_eq!(row.detectors, ["content", "configuration"]);
    assert_eq!(row.analyzers, ["thinkscript-source"]);
}

#[test]
fn prose_comments_strings_incomplete_and_foreign_suffixes_do_not_gain_thinkscript() {
    for source in [
        b"plain text with no code".as_slice(),
        b"# script Written { input width=2; plot value=width; }",
        b"\"script Written { input width=2; plot value=width; }\"",
        b"example: script Written { input width=2; plot value=width; }",
        b"script Written { input width=2; plot value=width;",
        b"script Written { input width=\"unterminated; }",
    ] {
        assert!(
            !signals("ordinary.txt", source)
                .iter()
                .any(|language| language == "thinkscript")
        );
    }
    for path in [
        "ordinary.rs",
        "ordinary.json",
        "ordinary.ts",
        "ordinary.cpp",
        "ordinary.txt.bak",
    ] {
        assert!(
            !signals(path, b"script Written { input width=2; plot value=width; }")
                .iter()
                .any(|language| language == "thinkscript")
        );
    }
}
