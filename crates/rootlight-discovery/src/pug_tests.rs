//! File attribution does not imply complete template or embedded semantics.
//! The installed capability names only the admitted native structural provider.

use super::*;

#[test]
fn pug_suffix_and_configuration_preserve_exact_language_identity() {
    assert_eq!(extension_language("nested/Written.PUG"), Some("pug"));
    assert_eq!(canonical_language("pug"), Some("pug"));
    for path in ["written.pug.bak", "written.jade", "written.pug.md"] {
        assert_ne!(extension_language(path), Some("pug"));
    }
    let capability = language_capabilities()
        .iter()
        .find(|capability| capability.language == "pug")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
    assert_eq!(capability.maximum_tier, "tier_d");
}

#[test]
fn unparsed_pug_bytes_remain_source_without_structural_promotion() {
    let path = RelativePath::parse(Path::new("written.pug")).unwrap();
    let (class, signals) = classify(&path, b"unparsed source");
    assert_eq!(class, InputClass::Source);
    assert!(
        signals.iter().any(
            |signal| signal.language == "pug" && signal.evidence == LanguageEvidence::Extension
        )
    );
}
