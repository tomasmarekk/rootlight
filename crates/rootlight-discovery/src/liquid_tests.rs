//! A Liquid suffix identifies the source format without advertising a parser.

use super::*;

#[test]
fn liquid_suffix_and_configuration_name_preserve_source_only_capability() {
    for path in [
        "snippet.liquid",
        "unrelated/NESTED.LIQUID",
        "assets/application.js.liquid",
        "assets/theme.scss.liquid",
        "layout/page.html.liquid",
    ] {
        assert_eq!(extension_language(path), Some("liquid"), "{path}");
    }
    for path in ["liquid", "snippet.liquid.bak", "snippet.liquid/notes.txt"] {
        assert_ne!(extension_language(path), Some("liquid"), "{path}");
    }
    assert_eq!(canonical_language("liquid"), Some("liquid"));
    let capability = language_capabilities()
        .iter()
        .find(|entry| entry.language == "liquid")
        .unwrap();
    assert_eq!(capability.suffixes, &[".liquid"]);
    assert_eq!(capability.analyzers, &["source-fallback"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
    assert_eq!(capability.maximum_tier, "tier_d");
}

#[test]
fn exact_liquid_template_gets_filename_evidence_without_host_language_aliases() {
    let source = include_bytes!("../tests/fixtures/liquid_source/blog-sidebar.liquid");
    assert_eq!(source.len(), 888);
    assert_eq!(&source[191..198], b"article");
    assert_eq!(&source[673..676], b"tag");
    let path = RelativePath::parse(Path::new("other/sidebar.LIQUID")).unwrap();
    let (class, signals) = classify(&path, source);
    assert_eq!(class, InputClass::Source);
    assert!(signals.iter().any(
        |signal| signal.language == "liquid" && signal.evidence == LanguageEvidence::Extension
    ));
    assert!(!signals.iter().any(|signal| signal.language == "ruby"));
    let plain = RelativePath::parse(Path::new("sidebar.html")).unwrap();
    assert!(
        !classify(&plain, source)
            .1
            .iter()
            .any(|signal| signal.language == "liquid")
    );
}

#[test]
fn liquid_suffix_keeps_independent_schema_and_racket_content_evidence() {
    for (path, source, content_language) in [
        (
            "page.json.liquid",
            b"{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"properties\":{\"value\":{\"type\":\"string\"}}}\n".as_slice(),
            "json-schema",
        ),
        (
            "script.rkt.liquid",
            b"#lang racket/base\n(define apparent 1)\n".as_slice(),
            "racket",
        ),
    ] {
        let path = RelativePath::parse(Path::new(path)).unwrap();
        let (_, signals) = classify(&path, source);
        assert!(signals.iter().any(|signal| signal.language == "liquid"
            && signal.evidence == LanguageEvidence::Extension));
        assert!(signals.iter().any(|signal| signal.language == content_language
            && signal.evidence == LanguageEvidence::Content));
    }
}
