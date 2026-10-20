//! Filename attribution is separate from native Hare analysis.
use super::*;

#[test]
fn hare_suffix_and_configuration_name_are_general() {
    for path in ["written.ha", "nested/Other.HA", "unrelated/Source.Ha"] {
        assert_eq!(extension_language(path), Some("hare"));
    }
    for path in [
        "hare",
        "written.hare",
        "written.ha.bak",
        "written.ha/readme.txt",
    ] {
        assert_ne!(extension_language(path), Some("hare"));
    }
    assert_eq!(canonical_language("hare"), Some("hare"));
    let capability = language_capabilities()
        .iter()
        .find(|entry| entry.language == "hare")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
}

#[test]
fn arbitrary_hare_file_keeps_extension_evidence() {
    let path = RelativePath::parse(Path::new("unrelated/Read.HA")).unwrap();
    let (class, signals) = classify(&path, b"fn target() void = {};\n");
    assert_eq!(class, InputClass::Source);
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "hare"
                && signal.evidence == LanguageEvidence::Extension)
    );
}
