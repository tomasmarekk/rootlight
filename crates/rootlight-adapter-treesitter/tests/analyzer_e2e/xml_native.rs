//! Physical XML source contracts through the real registered public analyzer.
//! Name equality does not establish namespaces, entity expansion or validity.

use super::*;

/// Unexpanded written XML with duplicate physical sibling names.
pub(super) const XML: LanguageCase = LanguageCase {
    name: "xml",
    path: "src/written.xml",
    frontend: "tree-sitter-xml-0.7.0",
    source: "<root><item value=\"plain\"/><item value='other'>text</item></root>",
    generated: false,
    body_before: "text",
    body_after: "changed text",
};

/// Borrow the exact UTF-8 bytes selected by a physical source reference.
///
/// # Panics
/// Panics if the source reference does not delimit a valid slice of the fixture.
pub(super) fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

/// Analyze a written fixture through the registered frontend and validate its source evidence.
///
/// # Panics
/// Panics if analysis or the written-source invariants fail.
pub(super) fn output(case: LanguageCase, source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    assert_sources(&result, case.name, source);
    result
}

/// Require generation-bound physical evidence without expanded or evaluated identities.
///
/// # Panics
/// Panics if source ranges, written roles, containment or partial coverage disagree.
pub(super) fn assert_sources(result: &AnalysisOutput, language: &str, source: &str) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, language);
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(
            document
                .occurrences
                .iter()
                .map(|occurrence| &occurrence.source),
        )
        .chain(
            document
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.evidence.source.as_ref()),
        )
        .chain(document.skipped_regions.iter().map(|gap| &gap.source))
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|extension| extension.evidence.source.as_ref()),
        )
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, reference);
    }
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        let evidence = occurrence.evidence.source.as_ref().unwrap();
        assert!(evidence.span().start_byte() <= occurrence.source.span().start_byte());
        assert!(occurrence.source.span().end_byte() <= evidence.span().end_byte());
        if occurrence.role != OccurrenceRole::Definition {
            assert_eq!(evidence, &occurrence.source);
        }
        assert!(!matches!(
            occurrence.role,
            OccurrenceRole::CallSite | OccurrenceRole::ImportUse
        ));
        if occurrence.role == OccurrenceRole::Reference {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    for coverage in &document.coverage_records {
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

fn signature(result: &AnalysisOutput, source: &str, symbol: SymbolId) -> Vec<String> {
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(symbol))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical
                    .verify_source_bytes(
                        text(source, extension.evidence.source.as_ref().unwrap()).as_bytes(),
                    )
                    .unwrap();
                lexical.text().to_owned()
            })
        })
        .collect()
}

#[test]
fn xml_written_elements_attributes_headers_and_contains_are_exact() {
    let result = output(XML, XML.source);
    let document = result.document();
    assert_eq!(document.entities.len(), 6);
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::MarkupElement)
            .count(),
        3
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::MarkupAttribute)
            .count(),
        2
    );
    for entity in document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        let definitions = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            text(XML.source, &definitions[0].source),
            entity.canonical_name
        );
        assert_eq!(signature(&result, XML.source, entity.id).len(), 1);
        let relation = document
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id)
            })
            .unwrap();
        let RelationEndpoint::Entity(owner) = relation.subject else {
            panic!("physical entity owner");
        };
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.id == owner)
            .unwrap();
        if entity.kind == EntityKind::MarkupAttribute {
            assert_eq!(parent.kind, EntityKind::MarkupElement);
        }
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations
                && gap.detail == "xml-namespace-entity-and-validity-analysis-unavailable")
    );
}

#[test]
fn xml_unicode_qnames_remain_written_not_expanded_or_case_folded() {
    for (source, expected) in [
        (
            "<p:Root xmlns:p=\"urn:example\" p:Value=\"raw\"><p:Child/></p:Root>",
            vec!["p:Root", "xmlns:p", "p:Value", "p:Child"],
        ),
        ("<À·̀ Ϳ‿=\"v\"><𐀀́/></À·̀>", vec!["À·̀", "Ϳ‿", "𐀀́"]),
    ] {
        let result = output(XML, source);
        let mut actual = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind != EntityKind::Module)
            .map(|entity| entity.canonical_name.as_str())
            .collect::<Vec<_>>();
        let mut expected = expected;
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        assert!(!result.document().entities.iter().any(|entity| matches!(
            entity.kind,
            EntityKind::Namespace | EntityKind::TypeAlias | EntityKind::Constant
        )));
    }
}

#[test]
fn xml_parent_source_keeps_attribute_pi_cdata_and_external_literal_bytes() {
    for (source, expected) in [
        (XML.source, "\"plain\""),
        ("<?go data?><root/><?after tail?>", "<?after tail?>"),
        (
            "<root><?go first?second\nthird???></root>",
            "<?go first?second\nthird???>",
        ),
        ("<?go?><root/>", "<?go?>"),
        ("<?go   ?><root/>", "<?go   ?>"),
        (
            "<?xml-stylesheet href=\"local.xsl\"?><root/>",
            "<?xml-stylesheet href=\"local.xsl\"?>",
        ),
        (
            "<?xml-model href=\"local.xsd\"?><root/>",
            "<?xml-model href=\"local.xsd\"?>",
        ),
        ("<root><![CDATA[]]></root>", "<![CDATA[]]>"),
        (
            "<!DOCTYPE root SYSTEM \"https://example.invalid/resource\"><root/>",
            "\"https://example.invalid/resource\"",
        ),
    ] {
        let result = output(XML, source);
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(
                    |occurrence| occurrence.role == OccurrenceRole::StringEvidence
                        && text(source, &occurrence.source) == expected
                ),
            "{expected}"
        );
    }
}

#[test]
fn xml_unequal_and_truncated_owners_keep_source_and_scoped_gaps() {
    for (source, domain, reason, detail) in [
        (
            "<Upper></upper>",
            FactDomain::Entities,
            SkippedRegionReason::UnsupportedConstruct,
            "xml-written-owner-and-tag-agreement-unavailable",
        ),
        (
            "<À></ǀ>",
            FactDomain::Entities,
            SkippedRegionReason::UnsupportedConstruct,
            "xml-written-owner-and-tag-agreement-unavailable",
        ),
        (
            "<À></À>",
            FactDomain::Entities,
            SkippedRegionReason::UnsupportedConstruct,
            "xml-written-owner-and-tag-agreement-unavailable",
        ),
        (
            "<root/",
            FactDomain::Diagnostics,
            SkippedRegionReason::ParseError,
            "syntax-error-recovery",
        ),
    ] {
        let result = output(XML, source);
        assert!(!result.document().entities.iter().any(|entity| matches!(
            entity.kind,
            EntityKind::MarkupElement | EntityKind::MarkupAttribute
        )));
        let file_source = result.document().files[0].evidence.source.as_ref().unwrap();
        assert_eq!(text(source, file_source), source);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain
                    && gap.reason == reason
                    && gap.detail == detail
                    && text(source, &gap.source) == source),
            "{source}"
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
fn xml_body_value_changes_preserve_physical_ids_and_required_fact_groups() {
    let before = "<root><item value=\"old\">body</item><item/></root>";
    let after = "<root><item value=\"new\">updated body</item><item/></root>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, XML);
    let first_fixture = Fixture::new(XML, before.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&first_fixture.snapshot, &first_fixture.source, XML, &budget),
        &ExtensionSupport::default(),
    );
    let second_fixture = first_fixture.rewrite(after.as_bytes());
    let second = analyze(
        &analyzer,
        &request(
            &second_fixture.snapshot,
            &second_fixture.source,
            XML,
            &budget,
        ),
        &ExtensionSupport::default(),
    );
    assert_eq!(
        first
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        second
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    assert_ne!(
        first.document().files[0].content_hash,
        second.document().files[0].content_hash
    );
    assert_ne!(first.document().generation, second.document().generation);
    let restored = second_fixture.rewrite(before.as_bytes());
    let next = restored.next_generation();
    let demand = provider
        .required_syntax_fact_count(
            &request(&next.snapshot, &next.source, XML, &budget).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    let bounded = limits_with_syntax_records(demand);
    let retained = analyze(
        &analyzer,
        &request(&next.snapshot, &next.source, XML, &bounded),
        &ExtensionSupport::default(),
    );
    assert_sources(&retained, "xml", before);
    assert_eq!(
        first
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        retained
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    for entity in retained
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        assert_eq!(signature(&retained, before, entity.id).len(), 1);
    }
}
