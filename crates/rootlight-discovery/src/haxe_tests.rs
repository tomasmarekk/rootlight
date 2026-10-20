//! Haxe extension evidence preserves real conflicts and safe source encodings.

use super::*;

#[test]
fn haxe_suffix_is_canonical_without_mining_literal_or_macro_text() {
    for name in ["Written.hx", "nested/Written.HX"] {
        let path = RelativePath::parse(Path::new(name)).unwrap();
        let (_, signals) = classify(&path, b"class Written { static macro function measure() { var value = 1; consume(value); return macro {}; } }");
        assert!(
            signals.iter().any(|signal| signal.language == "haxe"
                && signal.evidence == LanguageEvidence::Extension)
        );
        assert!(!signals.iter().any(|signal| signal.language == "matlab"));
    }
    assert_eq!(extension_language("build.hxml"), None);
    assert_eq!(extension_language("Written.hx.backup"), None);
}

#[test]
fn haxe_does_not_erase_shebang_proof_or_invent_content_from_invalid_encoding() {
    let path = RelativePath::parse(Path::new("Written.hx")).unwrap();
    let (_, signals) = classify(&path, b"#!/usr/bin/env python\nclass Written {}\n");
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "python"
                && signal.evidence == LanguageEvidence::Shebang)
    );
    let (_, signals) = classify(&path, b"\xff\xfe\0\xff");
    assert!(
        !signals
            .iter()
            .any(|signal| signal.evidence == LanguageEvidence::Content)
    );
}
