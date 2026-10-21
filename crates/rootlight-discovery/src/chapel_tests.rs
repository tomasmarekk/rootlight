//! Source attribution does not imply Chapel compiler or binding support.
use super::*;
#[test]
fn chapel_suffix_and_configuration_are_distinct_from_unknown_or_backup_files() {
    for path in ["written.chpl", "nested/Written.CHPL"] {
        assert_eq!(extension_language(path), Some("chapel"));
    }
    assert_eq!(canonical_language("chapel"), Some("chapel"));
    let capability = language_capabilities()
        .iter()
        .find(|entry| entry.language == "chapel")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(capability.detectors, &["extension", "configuration"]);
    for path in ["written.chpl.bak", "written.chapel", "written.chpl.in"] {
        assert_ne!(extension_language(path), Some("chapel"));
    }
}
