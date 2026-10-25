//! Source-written named Rust struct fields retain exact definitions and owners.
//! Native parser negatives distinguish declarations from types, uses and recovery.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/rust_named_fields/event_loop.rs");

fn case() -> LanguageCase {
    let mut case = *CASES.iter().find(|case| case.name == "rust").unwrap();
    case.path = "libs/core/event_loop.rs";
    case.generated = false;
    case
}

fn output(bytes: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case(), bytes);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case()),
        &request(&fixture.snapshot, &fixture.source, case(), &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_frame(result.document(), &fixture.source);
    result
}

fn assert_frame(document: &rootlight_ir::NormalizedIrDocument, expected: &SourceRef) {
    for source in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(document.occurrences.iter().map(|site| &site.source))
        .chain(
            document
                .relations
                .iter()
                .filter_map(|edge| edge.evidence.source.as_ref()),
        )
    {
        assert_eq!(source.repository(), expected.repository());
        assert_eq!(source.generation(), expected.generation());
        assert_eq!(source.content_hash(), expected.content_hash());
        assert_eq!(source.span().file(), expected.span().file());
    }
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(definitions.len(), 1, "one exact definition for {name}");
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must target its emitted entity: {name}");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.language, "rust");
    assert!(entity.evidence.source.is_some());
    symbol
}

fn owned_field(
    document: &rootlight_ir::NormalizedIrDocument,
    owner_name: &str,
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let symbol = definition(document, name, EntityKind::Field, start, end);
    let field = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    let owners: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == owner_name)
        .collect();
    assert_eq!(owners.len(), 1);
    assert_eq!(
        field.container,
        Some(rootlight_ir::ContainerRef::Entity(owners[0].id))
    );
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owners[0].id)
                && edge.object == RelationEndpoint::Entity(symbol)
        })
        .collect();
    assert_eq!(
        edges.len(),
        1,
        "one direct ownership edge for {owner_name}::{name}"
    );
    let evidence = edges[0].evidence.source.as_ref().unwrap();
    assert!(evidence.span().start_byte() <= start && evidence.span().end_byte() >= end);
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol }
        })
        .unwrap();
    assert_eq!(evidence.repository(), site.source.repository());
    assert_eq!(evidence.generation(), site.source.generation());
    assert_eq!(evidence.content_hash(), site.source.content_hash());
    assert_eq!(evidence.span().file(), site.source.span().file());
    symbol
}

fn site(text: &str, anchor: &str, name: &str) -> (u64, u64) {
    let start = text.find(anchor).unwrap() + anchor.find(name).unwrap();
    (
        u64::try_from(start).unwrap(),
        u64::try_from(start + name.len()).unwrap(),
    )
}

fn no_definition(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64) {
    assert!(document.occurrences.iter().all(|site| {
        site.role != OccurrenceRole::Definition
            || site.source.span().start_byte() != start
            || site.source.span().end_byte() != end
    }));
}

#[test]
fn original_deno_named_field_retains_exact_source_definition_and_struct_owner() {
    assert_eq!(ORIGINAL.len(), 1724);
    for (start, end, name) in [
        (1104, 1119, "EventLoopPhases"),
        (1160, 1175, "close_callbacks"),
        (1392, 1411, "run_close_callbacks"),
        (1608, 1632, "drain_v8_close_callbacks"),
        (1455, 1470, "close_callbacks"),
    ] {
        assert_eq!(&ORIGINAL[start..end], name.as_bytes());
    }
    let result = output(ORIGINAL);
    let document = result.document();
    definition(document, "EventLoopPhases", EntityKind::Struct, 1104, 1119);
    owned_field(document, "EventLoopPhases", "close_callbacks", 1160, 1175);
    definition(
        document,
        "run_close_callbacks",
        EntityKind::Method,
        1392,
        1411,
    );
    definition(
        document,
        "drain_v8_close_callbacks",
        EntityKind::Method,
        1608,
        1632,
    );
    no_definition(document, 1455, 1470);
}

#[test]
fn named_fields_keep_visibility_raw_names_generic_types_and_distinct_struct_owners() {
    let text = "struct First<T> { pub(crate) value: Option<T>, callback: fn(T) -> T, r#type: Vec<T> }\nstruct Second { value: usize }\n";
    let result = output(text.as_bytes());
    for (owner, anchor, written, canonical) in [
        ("First", "value: Option<T>", "value", "value"),
        ("First", "callback: fn(T)", "callback", "callback"),
        ("First", "r#type: Vec<T>", "r#type", "type"),
        ("Second", "value: usize", "value", "value"),
    ] {
        let (start, end) = site(text, anchor, written);
        owned_field(result.document(), owner, canonical, start, end);
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Field)
            .count(),
        4
    );
}

#[test]
fn types_field_uses_initializers_comments_strings_and_nonstruct_members_are_not_named_fields() {
    let text = "// comment_only: Fake\nstruct Owner { value: Option<u64> }\nunion Other { union_only: u64 }\nenum Choice { Record { variant_only: u64 } }\nstruct Tuple(u64, u64);\nfn use_fields(item: Owner) { let _ = item.value; let _ = Owner { value: None }; let _ = \"string_only: Fake\"; }\n";
    let result = output(text.as_bytes());
    let (start, end) = site(text, "value: Option<u64>", "value");
    owned_field(result.document(), "Owner", "value", start, end);
    for (anchor, name) in [
        ("comment_only: Fake", "comment_only"),
        ("value: Option<u64>", "Option"),
        ("value: Option<u64>", "u64"),
        ("union_only: u64", "union_only"),
        ("variant_only: u64", "variant_only"),
        ("item.value", "value"),
        ("Owner { value: None }", "value"),
        ("string_only: Fake", "string_only"),
    ] {
        let (start, end) = site(text, anchor, name);
        no_definition(result.document(), start, end);
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Field)
            .count(),
        1
    );
}

#[test]
fn malformed_named_field_cannot_borrow_a_later_healthy_structs_definition() {
    let text = "struct Broken { missing: }\nstruct Healthy { safe: u8 }\n";
    let result = output(text.as_bytes());
    let (start, end) = site(text, "safe: u8", "safe");
    owned_field(result.document(), "Healthy", "safe", start, end);
    assert!(
        result.document().entities.iter().all(|entity| {
            entity.kind != EntityKind::Field || entity.canonical_name != "missing"
        })
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}

#[test]
fn rewritten_named_fields_and_next_generation_use_only_current_physical_source() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case());
    let budget = limits();
    let original = "struct Owner { before: u8 }\n";
    let fixture = Fixture::new(case(), original.as_bytes());
    let initial = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, case(), &budget),
        &ExtensionSupport::default(),
    );
    let (start, end) = site(original, "before: u8", "before");
    owned_field(initial.document(), "Owner", "before", start, end);
    let text = "struct Owner { after: u8 }\n";
    let changed = fixture.rewrite(text.as_bytes());
    let next = changed.next_generation();
    for current in [&changed, &next] {
        let result = analyze(
            &analyzer,
            &request(&current.snapshot, &current.source, case(), &budget),
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let (start, end) = site(text, "after: u8", "after");
        owned_field(result.document(), "Owner", "after", start, end);
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.canonical_name != "before")
        );
        assert_ne!(current.source.content_hash(), fixture.source.content_hash());
        assert_ne!(current.source.generation(), fixture.source.generation());
        assert_frame(result.document(), &current.source);
    }
}

#[test]
fn a_recovered_named_field_does_not_remove_healthy_siblings_or_later_struct_fields() {
    let text = "struct Owner { broken: , healthy: u8 }\nstruct Later { next: u16 }\n";
    let result = output(text.as_bytes());
    for (owner, anchor, name) in [
        ("Owner", "healthy: u8", "healthy"),
        ("Later", "next: u16", "next"),
    ] {
        let (start, end) = site(text, anchor, name);
        owned_field(result.document(), owner, name, start, end);
    }
    let (start, end) = site(text, "broken:", "broken");
    no_definition(result.document(), start, end);
    assert!(
        result.document().entities.iter().all(|entity| {
            entity.kind != EntityKind::Field || entity.canonical_name != "broken"
        })
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}
