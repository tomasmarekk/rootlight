//! Supported JSON Schema content overrides a generic JSON suffix consistently.
//! Explicit configuration and independent conflicting proof remain authoritative,
//! even when the corresponding analyzer is absent from the installation.

use super::*;
use rootlight_discovery::LanguageSignal;

fn schema_input(signals: &[(&str, LanguageEvidence)]) -> ManifestInput {
    ManifestInput {
        file: FileId::from_bytes([37; 20]),
        path: "ordinary.json".to_owned(),
        content_hash: content_hash(b"schema-routing-contract"),
        bytes: 23,
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
fn explicit_supported_schema_content_reaches_both_routes_without_suffix_fallback() {
    let service = FirstSliceService::new(2).unwrap();
    assert!(service.analyzers.contains_key("json-schema"));
    let mut input = schema_input(&[
        ("json", LanguageEvidence::Extension),
        ("json-schema", LanguageEvidence::Content),
    ]);
    for path in ["ordinary.json", "ordinary.data", "extensionless"] {
        input.path = path.to_owned();
        assert_eq!(detected_source_language(&input), Some("json-schema"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("json-schema")
        );
        let mut missing = service.analyzers.clone();
        missing.remove("json-schema");
        assert_eq!(source_language_route(&input, &missing), None);
    }
}

#[test]
fn conflicts_are_unknown_and_configuration_never_falls_through_to_schema() {
    let service = FirstSliceService::new(2).unwrap();
    for evidence in [LanguageEvidence::Content, LanguageEvidence::Shebang] {
        let input = schema_input(&[
            ("json-schema", LanguageEvidence::Content),
            ("python", evidence),
        ]);
        assert_eq!(detected_source_language(&input), None);
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for language in ["json", "python", "missing-language", "json-schema"] {
        let input = schema_input(&[
            ("json-schema", LanguageEvidence::Content),
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
}

#[test]
fn generic_json_is_unchanged_and_schema_encoding_admission_is_explicit() {
    let service = FirstSliceService::new(2).unwrap();
    let input = schema_input(&[("json", LanguageEvidence::Extension)]);
    assert_eq!(detected_source_language(&input), Some("json"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("json")
    );
    let mut input = schema_input(&[("json-schema", LanguageEvidence::Content)]);
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        input.encoding = encoding.to_owned();
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("json-schema")
        );
    }
    input.encoding = "bytes".to_owned();
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
}
