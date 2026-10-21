//! File attribution remains separate from finite native structural support.
use super::*;

#[test]
fn gdscript_suffix_and_configuration_preserve_exact_language_identity() {
    assert_eq!(extension_language("nested/Written.GD"), Some("gdscript"));
    assert_eq!(canonical_language("gdscript"), Some("gdscript"));
    for path in [
        "written.gd.bak",
        "written.gdshader",
        "written.gdextension",
        "written.gds",
    ] {
        assert_ne!(extension_language(path), Some("gdscript"));
    }
    let capability = language_capabilities()
        .iter()
        .find(|capability| capability.language == "gdscript")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
}

#[test]
fn unparsed_gdscript_bytes_remain_source_without_structural_promotion() {
    let path = RelativePath::parse(Path::new("written.gd")).unwrap();
    let (class, signals) = classify(&path, b"unparsed source");
    assert_eq!(class, InputClass::Source);
    assert!(
        signals.iter().any(|signal| signal.language == "gdscript"
            && signal.evidence == LanguageEvidence::Extension)
    );
}
