//! Extension admission is ordinary metadata, separate from deep analysis.
use super::*;

#[test]
fn flux_extension_routes_generically_without_matching_content_names() {
    for path in ["sample.flux", "nested/another.FLUX"] {
        assert_eq!(extension_language(path), Some("flux"));
    }
    assert_ne!(extension_language("sample.fluxish"), Some("flux"));
    assert_ne!(extension_language("sample.txt"), Some("flux"));
    let row = LANGUAGE_CAPABILITIES
        .iter()
        .find(|row| row.language == "flux")
        .unwrap();
    assert_eq!(row.maximum_tier, "tier_d");
    assert_eq!(row.analyzers, &["native"]);
}
