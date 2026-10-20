//! Attribution does not certify native syntax or semantic binding.
use super::*;
#[test]
fn unambiguous_sml_wat_suffixes_and_configuration_keep_distinct_profiles() {
    for (path, language) in [
        ("nested/Written.SML", "standard-ml"),
        ("nested/Written.WAT", "webassembly"),
    ] {
        assert_eq!(extension_language(path), Some(language));
        assert_eq!(canonical_language(language), Some(language));
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == language)
            .unwrap();
        assert_eq!(capability.analyzers, &["treesitter"]);
        assert_eq!(capability.detectors, &["extension", "configuration"]);
    }
    for path in [
        "written.wast",
        "written.sml.bak",
        "written.wat.bak",
        "written.ml",
        "written.sig",
    ] {
        assert!(!matches!(
            extension_language(path),
            Some("standard-ml" | "webassembly")
        ));
    }
}
#[test]
fn source_classification_retains_unknown_syntax_without_claiming_structure() {
    for (filename, language) in [
        ("written.sml", "standard-ml"),
        ("written.wat", "webassembly"),
    ] {
        let path = RelativePath::parse(Path::new(filename)).unwrap();
        let (class, signals) = classify(&path, b"unparsed source bytes");
        assert_eq!(class, InputClass::Source);
        assert!(
            signals
                .iter()
                .any(|s| s.language == language && s.evidence == LanguageEvidence::Extension)
        );
    }
}
