//! Meson filename routing does not imply project evaluation or native completeness.
use super::*;

#[test]
fn exact_meson_build_filename_installs_the_native_source_route() {
    assert_eq!(filename_language("nested/meson.build"), Some("meson"));
    for path in [
        "not-meson.build",
        "meson.build.bak",
        "meson.options",
        "meson_options.txt",
    ] {
        assert_ne!(filename_language(path), Some("meson"));
    }
    assert_eq!(canonical_language("meson"), Some("meson"));
    let capability = language_capabilities()
        .iter()
        .find(|item| item.language == "meson")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["filename", "configuration"]);
    let path = RelativePath::parse(Path::new("meson.build")).unwrap();
    let (class, signals) = classify(&path, b"items = []\n");
    assert_eq!(class, InputClass::Source);
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "meson"
                && signal.evidence == LanguageEvidence::Filename)
    );
}
