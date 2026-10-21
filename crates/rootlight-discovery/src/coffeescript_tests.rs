//! File attribution remains separate from finite native structural support.
use super::*;

#[test]
fn coffeescript_suffix_and_configuration_preserve_exact_language_identity() {
    assert_eq!(
        extension_language("nested/Written.COFFEE"),
        Some("coffeescript")
    );
    assert_eq!(canonical_language("coffeescript"), Some("coffeescript"));
    for path in [
        "written.coffee.bak",
        "written.litcoffee",
        "written.cjsx",
        "written.coffee.md",
    ] {
        assert_ne!(extension_language(path), Some("coffeescript"));
    }
    let capability = language_capabilities()
        .iter()
        .find(|capability| capability.language == "coffeescript")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
}

#[test]
fn unparsed_coffeescript_bytes_remain_source_without_structural_promotion() {
    let path = RelativePath::parse(Path::new("written.coffee")).unwrap();
    let (class, signals) = classify(&path, b"unparsed source");
    assert_eq!(class, InputClass::Source);
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "coffeescript"
                && signal.evidence == LanguageEvidence::Extension)
    );
}
