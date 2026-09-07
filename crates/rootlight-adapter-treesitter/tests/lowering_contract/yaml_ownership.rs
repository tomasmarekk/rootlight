//! YAML ownership contracts for the parser-independent lowering boundary.
//! Explicit source spans model reviewed capture roles; these tests do not
//! substitute for native extraction or installed MCP qualification.

use super::*;
use rootlight_ir::{ContainerRef, EntityKind, FactDomain, NormalizedIrDocument};

#[path = "yaml_ownership/bindings.rs"]
mod bindings;

struct Fixture<'a> {
    text: &'a str,
    source: SourceRef,
    snapshot: SourceSnapshot,
    _directory: TempDir,
    facts: Vec<SyntaxFact>,
}

impl<'a> Fixture<'a> {
    fn new(text: &'a str) -> Self {
        let (directory, snapshot, source) =
            source_fixture_for(text, "data.yaml", b"yaml-data-ownership");
        let facts = vec![SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("yaml.file.root"),
        )];
        let mut fixture = Self {
            text,
            source,
            snapshot,
            _directory: directory,
            facts,
        };
        fixture.add(1, "yaml.file.module", text, 0);
        fixture
    }

    fn add(&mut self, parent: u64, syntax: &str, text: &str, ordinal: usize) -> u64 {
        let kind = if syntax.ends_with(".scope") {
            SyntaxFactKind::Scope
        } else if syntax.ends_with(".module") {
            SyntaxFactKind::Module
        } else if syntax.ends_with(".definition") {
            SyntaxFactKind::Occurrence
        } else {
            SyntaxFactKind::Declaration
        };
        let parent = self
            .facts
            .iter()
            .find(|fact| fact.local_id() == parent)
            .unwrap();
        let id = u64::try_from(self.facts.len()).unwrap() + 1;
        self.facts.push(SyntaxFact::new(
            id,
            Some(parent.local_id()),
            kind,
            span_in(self.text, &self.source, text, ordinal),
            parent.depth() + 1,
            label(syntax),
        ));
        id
    }

    fn property(&mut self, parent: u64, pair: &str, key: &str, ordinal: usize) -> u64 {
        let declaration = self.add(parent, "yaml.property.declaration", pair, ordinal);
        let pair_span = self.facts.last().unwrap().span();
        let key_start = pair_span.start_byte() + u64::try_from(pair.find(key).unwrap()).unwrap();
        let id = declaration + 1;
        let depth = self.facts.last().unwrap().depth() + 1;
        self.facts.push(SyntaxFact::new(
            id,
            Some(declaration),
            SyntaxFactKind::Occurrence,
            SourceSpan::new(
                self.source.span().file(),
                key_start,
                key_start + u64::try_from(key.len()).unwrap(),
            )
            .unwrap(),
            depth,
            label("yaml.key.definition"),
        ));
        declaration
    }

    fn analyze(&self) -> NormalizedIrDocument {
        self.analyze_with(self.facts.clone(), IrLimits::default())
            .unwrap()
            .document()
            .clone()
    }

    fn analyze_with(
        &self,
        facts: Vec<SyntaxFact>,
        ir: IrLimits,
    ) -> Result<rootlight_adapter_sdk::AnalysisOutput, AdapterError> {
        analyze_custom(
            &self.snapshot,
            &self.source,
            LanguageId::new("yaml").unwrap(),
            &limits(ir),
            facts,
        )
    }
}

fn properties(document: &NormalizedIrDocument) -> Vec<&rootlight_ir::EntityRecord> {
    let mut properties: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect();
    properties.sort_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte());
    properties
}

#[test]
fn documents_and_every_sequence_slot_own_distinct_source_properties() {
    let text = "---\n...\n---\nitems: [null, {name: first}, [false, {name: second}]]\n---\nitems: {name: third}\n";
    let mut fixture = Fixture::new(text);
    fixture.add(2, "yaml.document.scope", "---\n...", 0);
    let first = fixture.add(
        2,
        "yaml.document.scope",
        "---\nitems: [null, {name: first}, [false, {name: second}]]",
        0,
    );
    let mapping = fixture.add(
        first,
        "yaml.mapping.scope",
        "items: [null, {name: first}, [false, {name: second}]]",
        0,
    );
    let items = fixture.property(
        mapping,
        "items: [null, {name: first}, [false, {name: second}]]",
        "items",
        0,
    );
    let sequence = fixture.add(
        items,
        "yaml.sequence.scope",
        "[null, {name: first}, [false, {name: second}]]",
        0,
    );
    fixture.add(sequence, "yaml.sequence_element.scope", "null", 0);
    let element = fixture.add(sequence, "yaml.sequence_element.scope", "{name: first}", 0);
    let mapping = fixture.add(element, "yaml.mapping.scope", "{name: first}", 0);
    fixture.property(mapping, "name: first", "name", 0);
    let nested = fixture.add(
        sequence,
        "yaml.sequence_element.scope",
        "[false, {name: second}]",
        0,
    );
    let sequence = fixture.add(nested, "yaml.sequence.scope", "[false, {name: second}]", 0);
    fixture.add(sequence, "yaml.sequence_element.scope", "false", 0);
    let element = fixture.add(sequence, "yaml.sequence_element.scope", "{name: second}", 0);
    let mapping = fixture.add(element, "yaml.mapping.scope", "{name: second}", 0);
    fixture.property(mapping, "name: second", "name", 0);
    let second = fixture.add(2, "yaml.document.scope", "---\nitems: {name: third}", 0);
    let mapping = fixture.add(second, "yaml.mapping.scope", "items: {name: third}", 0);
    let items = fixture.property(mapping, "items: {name: third}", "items", 0);
    let mapping = fixture.add(items, "yaml.mapping.scope", "{name: third}", 0);
    fixture.property(mapping, "name: third", "name", 0);
    let document = fixture.analyze();
    assert!(
        document.skipped_regions.is_empty(),
        "{:?}",
        document.skipped_regions
    );
    let properties = properties(&document);
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.qualified_name.as_str())
            .collect::<Vec<_>>(),
        [
            "document[1].str:\"items\"",
            "document[1].str:\"items\"[1].str:\"name\"",
            "document[1].str:\"items\"[2][1].str:\"name\"",
            "document[2].str:\"items\"",
            "document[2].str:\"items\".str:\"name\"",
        ]
    );
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    for (child, parent) in [
        (0, module.id),
        (1, properties[0].id),
        (2, properties[0].id),
        (3, module.id),
        (4, properties[3].id),
    ] {
        assert_eq!(
            properties[child].container,
            Some(ContainerRef::Entity(parent))
        );
        assert!(document.relations.iter().any(|relation| relation.predicate
            == RelationPredicate::Contains
            && relation.subject == rootlight_ir::RelationEndpoint::Entity(parent)
            && relation.object == rootlight_ir::RelationEndpoint::Entity(properties[child].id)));
    }
    assert_exact_sources(&fixture, &document);
    let mut reversed = fixture.facts.clone();
    reversed.reverse();
    assert_eq!(
        *fixture
            .analyze_with(reversed, IrLimits::default())
            .unwrap()
            .document(),
        document
    );
}

fn assert_exact_sources(fixture: &Fixture<'_>, document: &NormalizedIrDocument) {
    for entity in properties(document) {
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source.generation(), fixture.source.generation());
        assert_eq!(source.content_hash(), fixture.source.content_hash());
        assert!(
            fixture
                .text
                .get(
                    usize::try_from(source.span().start_byte()).unwrap()
                        ..usize::try_from(source.span().end_byte()).unwrap()
                )
                .is_some()
        );
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == rootlight_ir::OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let span = definition.source.span();
        let raw = &fixture.text[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(definition.syntactic_text_hash, content_hash(raw.as_bytes()));
        assert_eq!(definition.evidence.source, entity.evidence.source);
    }
    let encoded = serde_json::to_vec(document).unwrap();
    assert_eq!(
        decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default()).unwrap(),
        IrDocument::NormalizedV1_1(document.clone())
    );
}

#[test]
fn duplicate_typed_keys_keep_occurrences_and_expose_unique_key_gap() {
    let text = "{11: first, 0xB: second, '11': text, 11.0: floating, 1.1e1: other}";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    let mapping = fixture.add(document, "yaml.mapping.scope", text, 0);
    for (pair, key) in [
        ("11: first", "11"),
        ("0xB: second", "0xB"),
        ("'11': text", "'11'"),
        ("11.0: floating", "11.0"),
        ("1.1e1: other", "1.1e1"),
    ] {
        fixture.property(mapping, pair, key, 0);
    }
    let document = fixture.analyze();
    let properties = properties(&document);
    assert_eq!(properties.len(), 5);
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        5
    );
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.qualified_name.as_str())
            .collect::<Vec<_>>(),
        [
            "document[0].int:11",
            "document[0].int:11#occurrence[1]",
            "document[0].str:\"11\"",
            "document[0].float:11e0",
            "document[0].float:11e0#occurrence[1]",
        ]
    );
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|region| region.detail == "yaml-duplicate-mapping-key")
        .collect();
    assert_eq!(gaps.len(), 4);
    for index in [0, 1, 3, 4] {
        assert!(
            gaps.iter()
                .any(|gap| Some(&gap.source) == properties[index].evidence.source.as_ref())
        );
    }
    assert!(
        document
            .coverage_records
            .iter()
            .any(|coverage| coverage.domain == FactDomain::Entities
                && coverage.status == CoverageStatus::Bounded)
    );
    assert_exact_sources(&fixture, &document);
}

#[test]
fn unavailable_parent_key_never_lifts_children_into_outer_mapping() {
    let text = "? [one, two]\n: {name: hidden}\nsafe: {name: visible}\n";
    let mut fixture = Fixture::new(text);
    let mapping = fixture.add(2, "yaml.mapping.scope", text, 0);
    let rejected = fixture.property(mapping, "? [one, two]\n: {name: hidden}", "[one, two]", 0);
    let nested = fixture.add(rejected, "yaml.mapping.scope", "{name: hidden}", 0);
    fixture.property(nested, "name: hidden", "name", 0);
    let accepted = fixture.property(mapping, "safe: {name: visible}", "safe", 0);
    let nested = fixture.add(accepted, "yaml.mapping.scope", "{name: visible}", 0);
    fixture.property(nested, "name: visible", "name", 0);
    let document = fixture.analyze();
    let properties = properties(&document);
    assert_eq!(properties.len(), 2);
    assert_eq!(
        properties[1].container,
        Some(ContainerRef::Entity(properties[0].id))
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|region| region.detail == "declaration-name-unavailable")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|region| region.detail == "stable-scope-identity-unavailable")
    );
    assert_exact_sources(&fixture, &document);
}

fn sequence_fixture<'a>(text: &'a str, prefix: &[&str], member: &str, key: &str) -> Fixture<'a> {
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    let sequence = fixture.add(document, "yaml.sequence.scope", text, 0);
    for prefix in prefix {
        fixture.add(sequence, "yaml.sequence_element.scope", prefix, 0);
    }
    let member = fixture.add(sequence, "yaml.sequence_element.scope", member, 0);
    let mapping = fixture.add(member, "yaml.mapping.scope", key, 0);
    let (name, _) = key.split_once(':').unwrap();
    fixture.property(mapping, key, name, 0);
    fixture
}

#[test]
fn empty_slots_count_but_scalar_bodies_layout_and_capture_ids_do_not() {
    let mut ids = Vec::new();
    for (text, prefix, member, key) in [
        (
            "-\n- 3\n- {name: first}\n",
            ["-\n", "- 3"],
            "- {name: first}",
            "name: first",
        ),
        (
            "-\n- changed\n- {name: another value}\n",
            ["-\n", "- changed"],
            "- {name: another value}",
            "name: another value",
        ),
        (
            "[null, false, {name: last}]",
            ["null", "false"],
            "{name: last}",
            "name: last",
        ),
        (
            "[  null, true, { 'name': 4 } ]",
            ["null", "true"],
            "{ 'name': 4 }",
            "'name': 4",
        ),
    ] {
        let fixture = sequence_fixture(text, &prefix, member, key);
        let document = fixture.analyze();
        assert!(document.skipped_regions.is_empty());
        let properties = properties(&document);
        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].qualified_name, "document[0][2].str:\"name\"");
        ids.push(properties[0].id);
        assert_exact_sources(&fixture, &document);
        let remapped = fixture
            .facts
            .iter()
            .rev()
            .map(|fact| {
                SyntaxFact::new(
                    10_000 - fact.local_id(),
                    fact.parent().map(|parent| 10_000 - parent),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                )
            })
            .collect();
        assert_eq!(
            fixture
                .analyze_with(remapped, IrLimits::default())
                .unwrap()
                .document(),
            &document
        );
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
    let shorter = sequence_fixture(
        "[null, {name: last}]",
        &["null"],
        "{name: last}",
        "name: last",
    )
    .analyze();
    assert_ne!(properties(&shorter)[0].id, ids[0]);
    assert_eq!(
        properties(&shorter)[0].qualified_name,
        "document[0][1].str:\"name\""
    );
}

#[test]
fn mapping_order_is_not_identity_and_nested_duplicate_owners_do_not_merge() {
    let mut by_spelling = std::collections::BTreeMap::new();
    for text in [
        "{a: {name: first}, b: {name: second}}",
        "{b: {name: second}, a: {name: first}}",
    ] {
        let mut fixture = Fixture::new(text);
        let document = fixture.add(2, "yaml.document.scope", text, 0);
        let mapping = fixture.add(document, "yaml.mapping.scope", text, 0);
        for (pair, key, value) in [
            ("a: {name: first}", "a", "name: first"),
            ("b: {name: second}", "b", "name: second"),
        ] {
            let property = fixture.property(mapping, pair, key, 0);
            let nested = fixture.add(property, "yaml.mapping.scope", value, 0);
            fixture.property(nested, value, "name", 0);
        }
        let document = fixture.analyze();
        assert!(document.skipped_regions.is_empty());
        for property in properties(&document) {
            let previous = by_spelling.insert(property.qualified_name.clone(), property.id);
            assert!(previous.is_none_or(|id| id == property.id));
        }
    }
    assert_eq!(by_spelling.len(), 4);

    let text = "{a: {name: first}, 'a': {name: second}}";
    let mut fixture = Fixture::new(text);
    let mapping = fixture.add(2, "yaml.mapping.scope", text, 0);
    for (pair, key, value) in [
        ("a: {name: first}", "a", "name: first"),
        ("'a': {name: second}", "'a'", "name: second"),
    ] {
        let parent = fixture.property(mapping, pair, key, 0);
        let nested = fixture.add(parent, "yaml.mapping.scope", value, 0);
        fixture.property(nested, value, "name", 0);
    }
    let document = fixture.analyze();
    let properties = properties(&document);
    assert_eq!(properties.len(), 4);
    assert_ne!(properties[0].id, properties[2].id);
    assert_ne!(properties[1].id, properties[3].id);
    assert_eq!(
        properties[1].container,
        Some(ContainerRef::Entity(properties[0].id))
    );
    assert_eq!(
        properties[3].container,
        Some(ContainerRef::Entity(properties[2].id))
    );
    assert_eq!(document.skipped_regions.len(), 2);
    assert_exact_sources(&fixture, &document);
}

#[test]
fn unknown_scope_is_an_explicit_local_gap_without_poisoning_siblings() {
    let text = "{bad: {name: hidden}, good: {name: visible}}";
    let mut fixture = Fixture::new(text);
    let mapping = fixture.add(2, "yaml.mapping.scope", text, 0);
    for (pair, key, value, syntax) in [
        (
            "bad: {name: hidden}",
            "bad",
            "name: hidden",
            "yaml.unreviewed.scope",
        ),
        (
            "good: {name: visible}",
            "good",
            "name: visible",
            "yaml.mapping.scope",
        ),
    ] {
        let parent = fixture.property(mapping, pair, key, 0);
        let nested = fixture.add(parent, syntax, value, 0);
        fixture.property(nested, value, "name", 0);
    }
    let document = fixture.analyze();
    assert_eq!(properties(&document).len(), 3);
    assert_eq!(document.skipped_regions.len(), 1);
    assert_eq!(
        document.skipped_regions[0].detail,
        "stable-scope-identity-unavailable"
    );
    assert_eq!(
        document.skipped_regions[0].source.span(),
        span_in(text, &fixture.source, "name: hidden", 0)
    );
    assert_exact_sources(&fixture, &document);
}

#[test]
fn actual_qualified_address_and_duplicate_gap_fit_their_existing_quotas() {
    let key = "🌍".repeat(15);
    let text = format!("{{{key}: 1, {key}: 2}}");
    let mut fixture = Fixture::new(&text);
    let document = fixture.add(2, "yaml.document.scope", &text, 0);
    let mapping = fixture.add(document, "yaml.mapping.scope", &text, 0);
    fixture.property(mapping, &format!("{key}: 1"), &key, 0);
    fixture.property(mapping, &format!("{key}: 2"), &key, 0);
    let normal = fixture.analyze();
    let maximum = normal
        .entities
        .iter()
        .map(|entity| entity.qualified_name.len())
        .max()
        .unwrap();
    let mut ir = IrLimits::default();
    ir.max_string_bytes = maximum;
    let exact = fixture
        .analyze_with(fixture.facts.clone(), ir.clone())
        .unwrap();
    assert_eq!(exact.document(), &normal);
    ir.max_string_bytes -= 1;
    assert!(matches!(
        fixture.analyze_with(fixture.facts.clone(), ir),
        Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
    ));
    for coverage in &normal.coverage_records {
        assert_eq!(coverage.discovered, coverage.indexed + coverage.skipped);
    }
}

#[test]
fn cached_identity_partition_includes_empty_and_scalar_sequence_slots() {
    let text = "-\n- 3\n- {name: first}\n";
    let fixture = sequence_fixture(text, &["-\n", "- 3"], "- {name: first}", "name: first");
    let language = LanguageId::new("yaml").unwrap();
    let analyzer = custom_analyzer(&fixture.snapshot, language.clone(), fixture.facts.clone());
    let limits = limits(IrLimits::default());
    let request_for = |source| {
        AnalysisRequest::new(
            GenerationBoundSnapshot::new(&fixture.snapshot, source).unwrap(),
            language.clone(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false)
    };
    let request = request_for(&fixture.source);
    let memory = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
    let (initial, artifact) = analyzer
        .analyze_and_capture(&request, ExtensionSupport::default(), memory, &deadline())
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        fixture.facts.len()
    );
    assert_eq!(artifact.syntax_fact_count(), fixture.facts.len());
    let source = SourceRef::new(
        fixture.source.repository(),
        GenerationId::from_bytes([8; 20]),
        fixture.source.span(),
        fixture.source.content_hash(),
        None,
    );
    let successor = request_for(&source);
    let reused = analyzer
        .analyze_from_artifact(
            &successor,
            &artifact,
            ExtensionSupport::default(),
            memory,
            &deadline(),
        )
        .unwrap();
    let fresh = execute_analysis(
        &analyzer,
        &successor,
        ExtensionSupport::default(),
        memory,
        &deadline(),
    )
    .unwrap();
    assert_eq!(reused.document(), fresh.document());
    assert_eq!(reused.report(), fresh.report());
    assert_eq!(
        properties(reused.document())[0].id,
        properties(initial.document())[0].id
    );
    assert!(
        reused
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.source.generation() == source.generation())
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(artifact.required_syntax_fact_count(&cancelled).is_err());
}

#[test]
fn equivalent_collection_wrappers_do_not_change_data_addresses() {
    let fixture = sequence_fixture(
        "[null, {name: last}]",
        &["null"],
        "{name: last}",
        "name: last",
    );
    let expected = fixture.analyze();
    for remove_mapping in [false, true] {
        for remove_sequence in [false, true] {
            let removed: std::collections::BTreeSet<_> = fixture
                .facts
                .iter()
                .filter(|fact| {
                    (remove_mapping && fact.syntax_kind().as_str() == "yaml.mapping.scope")
                        || (remove_sequence && fact.syntax_kind().as_str() == "yaml.sequence.scope")
                })
                .map(SyntaxFact::local_id)
                .collect();
            let facts = fixture
                .facts
                .iter()
                .filter(|fact| !removed.contains(&fact.local_id()))
                .map(|fact| {
                    let mut parent = fact.parent();
                    while let Some(removed_parent) =
                        parent.filter(|parent| removed.contains(parent))
                    {
                        parent = fixture
                            .facts
                            .iter()
                            .find(|fact| fact.local_id() == removed_parent)
                            .unwrap()
                            .parent();
                    }
                    SyntaxFact::new(
                        fact.local_id(),
                        parent,
                        fact.kind(),
                        fact.span(),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    )
                })
                .collect();
            let actual = fixture.analyze_with(facts, IrLimits::default()).unwrap();
            assert_eq!(actual.document(), &expected);
        }
    }
}
