//! Project facts retain native const-reference identity and truthful binding gaps.
//! Project name indexes cannot replace the selected-function certificate.

use super::*;

#[test]
fn cpp_const_formal_context_preserves_header_identity_and_exact_source_limit() {
    let sources = [
        "void Owned(const Value &value) { External(value[0]); }\n",
        "void Owned(const Value &value) { External(value[1]); }\n",
        "void Owned(const Value &value, const Value &other) { External(value[0]); }\n",
    ];
    let mut identities = Vec::new();
    for text in sources {
        let fixture = ProjectFixture::new(["written.cpp"], [text], SemanticProjectLanguage::Cpp);
        assert_real_parser_symbol_identity(
            &fixture,
            "cpp",
            &[
                (EntityKind::Function, "Owned"),
                (EntityKind::Parameter, "value"),
            ],
        );
        let project = analyze_with_real_parser(&fixture);
        let owner = project
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Owned")
            .unwrap();
        identities.push(owner.id);
        let gaps: Vec<_> = project.document().skipped_regions.iter().filter(|gap| gap.domain == FactDomain::Relations
            && gap.detail == "cpp-const-reference-formal-proof-is-limited-to-subscript-receivers-in-a-clean-function-body").collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].source.repository(), fixture.sources[0].repository());
        assert_eq!(gaps[0].source.generation(), fixture.sources[0].generation());
        assert_eq!(
            gaps[0].source.content_hash(),
            fixture.sources[0].content_hash()
        );
        assert_eq!(
            gaps[0].source.span().file(),
            fixture.sources[0].span().file()
        );
        assert_eq!(gaps[0].source.span().start_byte(), 0);
        assert_eq!(
            gaps[0].source.span().end_byte(),
            u64::try_from(text.trim_end().len()).unwrap()
        );
        assert!(
            project
                .document()
                .coverage_records
                .iter()
                .any(|record| record.domain == FactDomain::Relations
                    && record.status != CoverageStatus::Complete
                    && record.skipped > 0)
        );
    }
    assert_eq!(identities[0], identities[1]);
    assert_ne!(identities[0], identities[2]);
}

#[test]
fn cpp_native_formal_reference_cannot_adopt_a_project_spelling_target() {
    let text = "void value() {} void Owned(const Value &value) { External(value[0]); }\n";
    let fixture = ProjectFixture::new(["written.cpp"], [text], SemanticProjectLanguage::Cpp);
    let project = analyze_with_real_parser(&fixture);
    let at = u64::try_from(text.rfind("value[").unwrap()).unwrap();
    let sites: Vec<_> = project
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == at
                && site.source.span().end_byte() == at + 5
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].syntax_kind, "cpp.const_formal_receiver.reference");
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(
        sites[0].source.content_hash(),
        content_hash(text.as_bytes())
    );
    assert_eq!(
        sites[0].source.repository(),
        fixture.sources[0].repository()
    );
    assert_eq!(
        sites[0].source.generation(),
        fixture.sources[0].generation()
    );
    assert!(
        !project
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(sites[0].id))
    );
}
