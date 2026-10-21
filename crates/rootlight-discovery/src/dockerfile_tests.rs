//! Dockerfile routing advertises only the installed finite native source route.
use super::*;

#[test]
fn dockerfile_basename_route_does_not_match_backup_or_directory_names() {
    for path in ["Dockerfile", "nested/Dockerfile", "nested\\dockerfile"] {
        assert_eq!(filename_language(path), Some("dockerfile"));
        let relative = RelativePath::parse(Path::new(&path.replace('\\', "/"))).unwrap();
        let (class, signals) = classify(&relative, b"FROM scratch\n");
        assert_eq!(class, InputClass::Source);
        assert!(signals.iter().any(|signal| signal.language == "dockerfile"
            && signal.evidence == LanguageEvidence::Filename));
    }
    for path in [
        "Dockerfile.bak",
        "not-Dockerfile",
        "Dockerfile/notes.txt",
        "Containerfile",
    ] {
        assert_ne!(filename_language(path), Some("dockerfile"));
    }
    let capability = language_capabilities()
        .iter()
        .find(|capability| capability.language == "dockerfile")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["filename", "configuration"]);
    assert_eq!(canonical_language("dockerfile"), Some("dockerfile"));
}
