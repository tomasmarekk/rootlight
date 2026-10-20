//! Filename attribution does not certify native Mojo structural or semantic facts.
use super::*;

#[test]
fn mojo_suffixes_and_configuration_name_keep_one_canonical_identity() {
    for path in [
        "source.mojo",
        "nested/Other.MOJO",
        "unrelated/Source.MoJo",
        "nested/source.🔥",
    ] {
        assert_eq!(extension_language(path), Some("mojo"));
    }
    for path in [
        "mojo",
        "source.moj",
        "source.mojo.bak",
        "source.mojo/notes.txt",
        "source.mo",
        "source.py",
    ] {
        assert_ne!(extension_language(path), Some("mojo"));
    }
    assert_eq!(canonical_language("mojo"), Some("mojo"));
    let capability = language_capabilities()
        .iter()
        .find(|entry| entry.language == "mojo")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
}

#[test]
fn arbitrary_mojo_source_keeps_extension_evidence_without_syntax_promotion() {
    for filename in ["unrelated/Source.MOJO", "unrelated/Source.🔥"] {
        let path = RelativePath::parse(Path::new(filename)).unwrap();
        let (class, signals) = classify(&path, b"struct Written:\n    var value: Int\n");
        assert_eq!(class, InputClass::Source);
        assert!(
            signals.iter().any(|signal| signal.language == "mojo"
                && signal.evidence == LanguageEvidence::Extension)
        );
    }
}
