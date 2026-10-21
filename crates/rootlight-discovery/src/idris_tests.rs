//! Ordinary Idris suffix evidence is independent of dependent elaboration.
use super::*;

#[test]
fn idris_source_and_literate_suffixes_advertise_native_source_structure() {
    let capability = language_capabilities()
        .iter()
        .find(|item| item.language == "idris")
        .unwrap();
    assert_eq!(capability.suffixes, &[".idr", ".lidr"]);
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.maximum_tier, "tier_d");
    assert!(capability.aliases.is_empty());
    assert_eq!(canonical_language("idris"), Some("idris"));
    for path in ["Main.idr", "Main.IDR", "Guide.lidr"] {
        assert_eq!(extension_language(path), Some("idris"));
        let path = RelativePath::parse(Path::new(path)).unwrap();
        let (class, signals) = classify(&path, b"unparsed source");
        assert_eq!(class, InputClass::Source);
        assert!(
            signals.iter().any(|signal| signal.language == "idris"
                && signal.evidence == LanguageEvidence::Extension)
        );
    }
    for path in ["Main.idr.bak", "Guide.lidr.md", "Main.idr.js"] {
        assert_ne!(extension_language(path), Some("idris"));
    }
}
