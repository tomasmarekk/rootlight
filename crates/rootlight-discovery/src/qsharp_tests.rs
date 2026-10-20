//! Source attribution does not itself certify Q# syntax or binding.
use super::*;
#[test]
fn qsharp_suffix_and_configuration_preserve_one_identity() {
    assert_eq!(extension_language("nested/Written.QS"), Some("qsharp"));
    assert_eq!(canonical_language("qsharp"), Some("qsharp"));
    for name in ["written.qs.bak", "written.cs", "written.qsharp"] {
        assert_ne!(extension_language(name), Some("qsharp"));
    }
    let capability = language_capabilities()
        .iter()
        .find(|c| c.language == "qsharp")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
}
#[test]
fn unparsed_qsharp_bytes_remain_source_without_semantic_promotion() {
    let path = RelativePath::parse(Path::new("written.qs")).unwrap();
    let (class, signals) = classify(&path, b"unparsed source");
    assert_eq!(class, InputClass::Source);
    assert!(
        signals
            .iter()
            .any(|s| s.language == "qsharp" && s.evidence == LanguageEvidence::Extension)
    );
}
