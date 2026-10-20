//! A known QML suffix routes independently from MATLAB's function-token hint.
//! Explicit configuration and source encoding keep their existing precedence.

use super::*;
use rootlight_discovery::LanguageSignal;

fn input(signals: &[(&str, LanguageEvidence)]) -> ManifestInput {
    ManifestInput {
        file: FileId::from_bytes([17; 20]),
        path: "Settings.qml".to_owned(),
        content_hash: content_hash(b"qml-routing-contract"),
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
fn qml_known_suffix_routes_both_paths_and_missing_parser_never_chooses_matlab() {
    let service = FirstSliceService::new(2).unwrap();
    assert!(service.analyzers.contains_key("qml"));
    assert!(service.structural_parsers.contains_key("qml"));
    let mut input = input(&[("matlab", LanguageEvidence::Content)]);
    for path in ["Settings.qml", "Settings.QML"] {
        input.path = path.to_owned();
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            input.encoding = encoding.to_owned();
            assert_eq!(detected_source_language(&input), Some("qml"));
            assert_eq!(
                source_language_route(&input, &service.analyzers),
                Some("qml")
            );
            assert_eq!(
                supported_source_language(&input, &service.analyzers),
                Some("qml")
            );
        }
        let mut missing = service.analyzers.clone();
        missing.remove("qml");
        assert_eq!(source_language_route(&input, &missing), None);
    }
    input.encoding = "bytes".to_owned();
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
}

#[test]
fn qml_configuration_precedence_remains_independent_of_parser_availability() {
    let service = FirstSliceService::new(2).unwrap();
    for language in ["python", "unavailable-language", "qml"] {
        let input = input(&[
            ("qml", LanguageEvidence::Extension),
            (language, LanguageEvidence::Configuration),
        ]);
        assert_eq!(detected_source_language(&input), Some(language));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            service.analyzers.contains_key(language).then_some(language)
        );
        let mut missing = service.analyzers.clone();
        missing.remove(language);
        assert_eq!(source_language_route(&input, &missing), None);
    }
    let input = input(&[
        ("qml", LanguageEvidence::Configuration),
        ("python", LanguageEvidence::Configuration),
    ]);
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
}

#[test]
fn genuine_non_qml_shebang_conflict_stays_unresolved_in_both_routes() {
    let service = FirstSliceService::new(2).unwrap();
    for language in ["python", "bash", "racket"] {
        let input = input(&[
            ("qml", LanguageEvidence::Extension),
            (language, LanguageEvidence::Shebang),
        ]);
        assert_eq!(detected_source_language(&input), None);
        assert_eq!(source_language_route(&input, &service.analyzers), None);
        let mut missing = service.analyzers.clone();
        missing.remove(language);
        assert_eq!(source_language_route(&input, &missing), None);
    }
}
