//! Modelica parser admission requires written evidence for the shared `.mo` suffix.
//! Routing honors explicit configuration and retains uncertainty independently
//! of which native analyzers happen to be installed.

use super::*;
use rootlight_discovery::LanguageSignal;

fn input(signals: &[(&str, LanguageEvidence)]) -> ManifestInput {
    ManifestInput {
        file: FileId::from_bytes([17; 20]),
        path: "source.MO".to_owned(),
        content_hash: content_hash(b"within; model Example end Example;"),
        bytes: 34,
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
fn modelica_suffix_alone_and_conflicting_proofs_never_choose_an_installed_parser() {
    let service = FirstSliceService::new(2).unwrap();
    assert!(service.analyzers.contains_key("modelica"));
    assert!(service.structural_parsers.contains_key("modelica"));
    for signals in [
        Vec::new(),
        vec![("modelica", LanguageEvidence::Extension)],
        vec![("modelica", LanguageEvidence::Context)],
        vec![("python", LanguageEvidence::Content)],
        vec![
            ("modelica", LanguageEvidence::Content),
            ("rust", LanguageEvidence::Content),
        ],
        vec![
            ("modelica", LanguageEvidence::Content),
            ("python", LanguageEvidence::Shebang),
        ],
        vec![
            ("modelica", LanguageEvidence::Content),
            ("racket", LanguageEvidence::Content),
        ],
    ] {
        let input = input(&signals);
        assert_eq!(detected_source_language(&input), None, "{signals:?}");
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            None,
            "{signals:?}"
        );
        let mut partial = service.analyzers.clone();
        partial.remove("rust");
        partial.remove("python");
        partial.remove("racket");
        assert_eq!(source_language_route(&input, &partial), None, "{signals:?}");
    }
}

#[test]
fn modelica_written_content_routes_supported_encodings_without_extension_fallback() {
    let service = FirstSliceService::new(2).unwrap();
    let mut input = input(&[("modelica", LanguageEvidence::Content)]);
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        input.encoding = encoding.to_owned();
        assert_eq!(detected_source_language(&input), Some("modelica"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("modelica")
        );
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("modelica")
        );
    }
    input.encoding = "bytes".to_owned();
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
    let mut missing = service.analyzers.clone();
    missing.remove("modelica");
    assert_eq!(source_language_route(&input, &missing), None);
}

#[test]
fn modelica_configuration_precedence_does_not_depend_on_parser_availability() {
    let service = FirstSliceService::new(2).unwrap();
    for language in ["python", "unavailable-language", "modelica"] {
        let input = input(&[
            ("modelica", LanguageEvidence::Content),
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
        ("python", LanguageEvidence::Configuration),
        ("modelica", LanguageEvidence::Configuration),
    ]);
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
}
