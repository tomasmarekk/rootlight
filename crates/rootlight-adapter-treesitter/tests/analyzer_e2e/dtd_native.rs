//! DTD written names and parents are usable source evidence, not resolved kinds.
//! Clean native parsing does not remove the explicit structural declaration-kind gap.

use super::xml_native::{assert_sources, output, text};
use super::*;

/// Minimal written declaration whose body edit does not create an evaluated entity.
pub(super) const DTD: LanguageCase = LanguageCase {
    name: "dtd",
    path: "src/written.dtd",
    frontend: "tree-sitter-xml-dtd-0.7.0",
    source: "<!ELEMENT root EMPTY>",
    generated: false,
    body_before: "EMPTY",
    body_after: "ANY",
};

#[test]
fn dtd_written_declarations_keep_names_literals_and_explicit_kind_gap() {
    for (source, name) in [
        (DTD.source, "root"),
        ("<!ELEMENT À̀ EMPTY>\n<!ATTLIST À̀ 𐀀 CDATA #IMPLIED>", "𐀀"),
    ] {
        let result = output(DTD, source);
        assert_eq!(result.document().entities.len(), 1);
        assert_eq!(result.document().entities[0].kind, EntityKind::Module);
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                    && text(source, &occurrence.source) == name
                    && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(
                    |occurrence| occurrence.role == OccurrenceRole::StringEvidence
                        && text(source, &occurrence.source).starts_with("<!ELEMENT")
                )
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.detail == "dtd-structural-declaration-kind-unavailable")
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.detail == "dtd-entity-expansion-and-validity-analysis-unavailable")
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition)
        );
    }
}

#[test]
fn dtd_pi_parent_and_required_context_survive_exact_source_replay() {
    let source = "<?go a?b\nc?><!ELEMENT root EMPTY><?after?>";
    let result = output(DTD, source);
    for expected in ["<?go a?b\nc?>", "<?after?>", "<!ELEMENT root EMPTY>"] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(
                    |occurrence| occurrence.role == OccurrenceRole::StringEvidence
                        && text(source, &occurrence.source) == expected
                )
        );
    }
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, DTD);
    let fixture = Fixture::new(DTD, source.as_bytes());
    let budget = limits();
    let demand = provider
        .required_syntax_fact_count(
            &request(&fixture.snapshot, &fixture.source, DTD, &budget).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    let bounded = limits_with_syntax_records(demand);
    let retained = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, DTD, &bounded),
        &ExtensionSupport::default(),
    );
    assert_sources(&retained, "dtd", source);
    assert!(
        retained
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Entities
                && gap.detail == "dtd-structural-declaration-kind-unavailable")
    );
}
