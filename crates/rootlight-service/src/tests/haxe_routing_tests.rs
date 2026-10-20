//! Native Haxe routing preserves configuration, conflicts and parser availability.

use super::*;
use rootlight_discovery::LanguageSignal;

fn input(signals: &[(&str, LanguageEvidence)]) -> ManifestInput {
    ManifestInput {
        file: FileId::from_bytes([17; 20]),
        path: "Written.hx".to_owned(),
        content_hash: content_hash(b"haxe-routing-source"),
        bytes: 20,
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: signals
            .iter()
            .map(|(language, evidence)| LanguageSignal {
                language: (*language).to_owned(),
                evidence: *evidence,
            })
            .collect(),
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    }
}

#[test]
fn haxe_uses_both_native_provider_maps_and_missing_parser_is_not_another_language() {
    let service = FirstSliceService::new(2).unwrap();
    assert!(service.analyzers.contains_key("haxe"));
    assert!(service.structural_parsers.contains_key("haxe"));
    let mut input = input(&[("haxe", LanguageEvidence::Extension)]);
    for path in ["Written.hx", "Written.HX"] {
        input.path = path.to_owned();
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            input.encoding = encoding.to_owned();
            assert_eq!(detected_source_language(&input), Some("haxe"));
            assert_eq!(
                supported_source_language(&input, &service.analyzers),
                Some("haxe")
            );
        }
        let mut missing = service.analyzers.clone();
        missing.remove("haxe");
        assert_eq!(source_language_route(&input, &missing), None);
    }
    input.encoding = "bytes".to_owned();
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
}

#[test]
fn haxe_genuine_content_and_shebang_conflicts_remain_unknown_without_configuration() {
    let service = FirstSliceService::new(2).unwrap();
    for evidence in [LanguageEvidence::Content, LanguageEvidence::Shebang] {
        let input = input(&[("haxe", LanguageEvidence::Extension), ("python", evidence)]);
        assert_eq!(detected_source_language(&input), None);
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for language in ["haxe", "python", "unavailable-language"] {
        let input = input(&[
            ("haxe", LanguageEvidence::Extension),
            ("python", LanguageEvidence::Shebang),
            (language, LanguageEvidence::Configuration),
        ]);
        assert_eq!(detected_source_language(&input), Some(language));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            service.analyzers.contains_key(language).then_some(language)
        );
    }
    let input = input(&[
        ("haxe", LanguageEvidence::Configuration),
        ("python", LanguageEvidence::Configuration),
    ]);
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
}
