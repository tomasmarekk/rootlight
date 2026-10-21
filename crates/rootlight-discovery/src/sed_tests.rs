//! Ordinary Sed file routing is separate from static parsing or execution.
use super::*;

const ORIGINAL: &[u8] =
    include_bytes!("../../rootlight-adapter-treesitter/tests/fixtures/sed/mario.sed");

#[test]
fn sed_suffix_advertises_native_static_structure_without_dialect_inference() {
    assert_eq!(extension_language("nested/written.sed"), Some("sed"));
    assert_eq!(extension_language("nested/written.SED"), Some("sed"));
    assert_eq!(canonical_language("sed"), Some("sed"));
    for path in [
        "written.sed.bak",
        "written.sed.md",
        "written.sed.ere",
        "written.sh",
    ] {
        assert_ne!(extension_language(path), Some("sed"), "{path}");
    }
    let capability = language_capabilities()
        .iter()
        .find(|item| item.language == "sed")
        .unwrap();
    assert_eq!(capability.suffixes, &[".sed"]);
    assert!(capability.aliases.is_empty());
    assert_eq!(capability.detectors, &["extension"]);
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.maximum_tier, "tier_d");
}

#[test]
fn complete_original_and_unparsed_sed_bytes_keep_ordinary_extension_evidence() {
    assert_eq!(ORIGINAL.len(), 8893);
    assert_eq!(
        content_hash(ORIGINAL).to_string(),
        "b3_untefh2tstnrgqj74ikoxuxayelmoallhirnayo3r7apw6nowpdr7ohqvm"
    );
    let path = RelativePath::parse(Path::new("written.sed")).unwrap();
    for bytes in [ORIGINAL, b"unparsed source".as_slice()] {
        let (class, signals) = classify(&path, bytes);
        assert_eq!(class, InputClass::Source);
        assert!(signals.iter().any(
            |signal| signal.language == "sed" && signal.evidence == LanguageEvidence::Extension
        ));
    }
}
