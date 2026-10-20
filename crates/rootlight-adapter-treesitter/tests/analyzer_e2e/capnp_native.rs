//! Cap'n Proto analyzer contracts prove physical names, owners and type edges.
//! These authored cases do not substitute for schema compilation or wire checks.

use super::*;
use rootlight_ir::FactRef;

#[path = "capnp_admission.rs"]
mod admission;
#[path = "capnp_written_values.rs"]
mod written_values;

const CAPNP: LanguageCase = LanguageCase {
    name: "capnp",
    path: "schema.capnp",
    frontend: "tree-sitter-capnp-1.5.0",
    source: "struct Item { value @0 :Text; }\n",
    generated: false,
    body_before: "value @0 :Text",
    body_after: "other @0 :Text",
};

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn output_at(case: LanguageCase, source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let limits = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &limits),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "capnp");
    assert_eq!(
        document.files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
    assert_eq!(document.provenance.len(), 1);
    for occurrence in &document.occurrences {
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_eq!(occurrence.source.generation(), document.generation);
        assert_eq!(occurrence.source.span().file(), document.files[0].id);
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        assert_eq!(occurrence.provenance, document.provenance[0].id);
    }
    for gap in &document.skipped_regions {
        assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, &gap.source);
    }
    result
}

fn output(source: &str) -> AnalysisOutput {
    output_at(CAPNP, source)
}

fn entity<'a>(
    result: &'a AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let mut rows = result
        .document()
        .entities
        .iter()
        .filter(|row| row.canonical_name == name && row.kind == kind);
    let row = rows.next().unwrap_or_else(|| {
        let observed = result
            .document()
            .entities
            .iter()
            .filter(|row| row.canonical_name == name)
            .map(|row| row.kind)
            .collect::<Vec<_>>();
        panic!("missing {kind:?} {name}; observed kinds: {observed:?}");
    });
    assert!(rows.next().is_none(), "ambiguous fixture {name}");
    row
}

fn exact_definition(
    result: &AnalysisOutput,
    source: &str,
    name: &str,
    kind: EntityKind,
    marker: &str,
) {
    let owner = entity(result, name, kind);
    let rows = result
        .document()
        .occurrences
        .iter()
        .filter(|row| {
            row.role == OccurrenceRole::Definition
                && row.target == OccurrenceTarget::Resolved { symbol: owner.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{name}");
    let offset = source.find(marker).unwrap() + marker.find(name).unwrap();
    assert_eq!(
        rows[0].source.span().start_byte(),
        u64::try_from(offset).unwrap()
    );
    assert_eq!(
        rows[0].source.span().end_byte(),
        u64::try_from(offset + name.len()).unwrap()
    );
    assert_eq!(text(source, &rows[0].source), name);
}

fn contains(
    result: &AnalysisOutput,
    parent: &rootlight_ir::EntityRecord,
    child: &rootlight_ir::EntityRecord,
) {
    assert_eq!(
        child.container,
        Some(rootlight_ir::ContainerRef::Entity(parent.id))
    );
    assert!(
        result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id))
    );
}

fn linked_type(
    result: &AnalysisOutput,
    source: &str,
    marker: &str,
    spelling: &str,
    target: SymbolId,
) {
    let offset = source.find(marker).unwrap() + marker.rfind(spelling).unwrap();
    let rows = result
        .document()
        .occurrences
        .iter()
        .filter(|row| {
            row.role == OccurrenceRole::TypeUse
                && row.source.span().start_byte() == u64::try_from(offset).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{marker}");
    let occurrence = rows[0];
    assert_eq!(text(source, &occurrence.source), spelling);
    assert_eq!(
        occurrence.target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert!(
        result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::UsesType
                && relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                && relation.object == RelationEndpoint::Entity(target)
                && relation.evidence.source.as_ref() == Some(&occurrence.source))
    );
}

fn field_type_edge(
    result: &AnalysisOutput,
    source: &str,
    field: &rootlight_ir::EntityRecord,
    target: SymbolId,
    marker: &str,
    spelling: &str,
) {
    linked_type(result, source, marker, spelling, target);
    let offset = source.find(marker).unwrap() + marker.rfind(spelling).unwrap();
    let occurrence = result
        .document()
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::TypeUse
                && row.source.span().start_byte() == u64::try_from(offset).unwrap()
        })
        .unwrap();
    assert_eq!(occurrence.enclosing, Some(field.id));
    let relations = result
        .document()
        .relations
        .iter()
        .filter(|row| {
            row.predicate == RelationPredicate::UsesType
                && row.subject == RelationEndpoint::Entity(field.id)
                && row.object == RelationEndpoint::Entity(target)
                && row.evidence.source.as_ref() == Some(&occurrence.source)
        })
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1, "{marker}");
    assert_eq!(relations[0].provenance, occurrence.provenance);
    assert_eq!(relations[0].confidence, occurrence.confidence);
    let mut expected_derivation = [
        FactRef::Entity(field.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target),
    ];
    expected_derivation.sort_unstable();
    assert_eq!(relations[0].evidence.derivation, expected_derivation);
    assert_eq!(
        relations[0].id,
        rootlight_ir::derive_relation_record_id(relations[0]).unwrap()
    );
}

#[test]
fn capnp_field_type_relations_keep_exact_owners_and_occurrence_evidence() {
    let source = "struct A { struct Item {} first @0 :Item; }\nstruct B { struct Item {} second @0 :List(Item); }\n";
    let result = output(source);
    for (name, marker) in [
        ("first", "first @0 :Item"),
        ("second", "second @0 :List(Item)"),
    ] {
        let field = entity(&result, name, EntityKind::Field);
        let target = result
            .document()
            .entities
            .iter()
            .find(|row| {
                row.kind == EntityKind::Struct
                    && row.canonical_name == "Item"
                    && row.container == field.container
            })
            .unwrap();
        field_type_edge(&result, source, field, target.id, marker, "Item");
    }
    assert_eq!(
        result
            .document()
            .relations
            .iter()
            .filter(|row| row.predicate == RelationPredicate::UsesType
                && matches!(row.subject, RelationEndpoint::Entity(_)))
            .count(),
        2
    );
}

#[test]
fn capnp_field_type_relations_do_not_guess_unresolved_or_nonfield_owners() {
    for source in [
        "struct Item {} struct Item {} struct Host { value @0 :Item; }\n",
        "struct Item {} struct Host { value @0 :item; }\n",
        "using Remote = import \"remote.capnp\"; struct Item {} struct Host { value @0 :Item; }\n",
        "struct Item {} interface Api { fetch @0 (input: Item) -> (output: Item); }\n",
    ] {
        let result = output(source);
        assert!(
            !result.document().relations.iter().any(|row| {
                row.predicate == RelationPredicate::UsesType
                    && matches!(row.subject, RelationEndpoint::Entity(_))
            }),
            "{source}"
        );
        if source.contains("interface Api") {
            let target = entity(&result, "Item", EntityKind::Struct).id;
            linked_type(&result, source, "input: Item", "Item", target);
            linked_type(&result, source, "output: Item", "Item", target);
        }
    }
}

#[test]
fn capnp_written_declarations_have_exact_leaf_spans_and_nested_owners() {
    let source = "# λ😀 physical header\nstruct Envelope { struct Detail {} enum Tone { light @0; } value @0 :Detail; }\ninterface Service { get @0 (input: Envelope) -> (output: Envelope); }\nconst seed :Envelope = ();\nannotation tag(struct) :Text;\n";
    let result = output(source);
    for (name, kind, marker) in [
        ("Envelope", EntityKind::Struct, "struct Envelope"),
        ("Detail", EntityKind::Struct, "struct Detail"),
        ("Tone", EntityKind::Enum, "enum Tone"),
        ("light", EntityKind::Constant, "light @0"),
        ("value", EntityKind::Field, "value @0"),
        ("Service", EntityKind::Interface, "interface Service"),
        ("get", EntityKind::Method, "get @0"),
        ("input", EntityKind::Parameter, "input: Envelope"),
        ("output", EntityKind::Parameter, "output: Envelope"),
        ("seed", EntityKind::Constant, "const seed"),
        ("tag", EntityKind::Function, "annotation tag"),
    ] {
        exact_definition(&result, source, name, kind, marker);
    }
    contains(
        &result,
        entity(&result, "Envelope", EntityKind::Struct),
        entity(&result, "Detail", EntityKind::Struct),
    );
    contains(
        &result,
        entity(&result, "Tone", EntityKind::Enum),
        entity(&result, "light", EntityKind::Constant),
    );
    contains(
        &result,
        entity(&result, "Service", EntityKind::Interface),
        entity(&result, "get", EntityKind::Method),
    );
}

#[test]
fn capnp_type_lookup_uses_typed_ancestry_and_full_qualified_spans() {
    let source = "struct A { struct Inner {} local @0 :Inner; }\nstruct B { struct Inner {} local @0 :Inner; cross @1 :A.Inner; }\n";
    let result = output(source);
    let a = entity(&result, "A", EntityKind::Struct);
    let b = entity(&result, "B", EntityKind::Struct);
    let modules = result
        .document()
        .entities
        .iter()
        .filter(|row| row.kind == EntityKind::Module)
        .collect::<Vec<_>>();
    assert_eq!(modules.len(), 1);
    contains(&result, modules[0], a);
    contains(&result, modules[0], b);
    let nested = |parent: SymbolId| {
        result
            .document()
            .entities
            .iter()
            .find(|row| {
                row.canonical_name == "Inner"
                    && row.kind == EntityKind::Struct
                    && row.container == Some(rootlight_ir::ContainerRef::Entity(parent))
            })
            .unwrap()
    };
    let inner_a = nested(a.id);
    let inner_b = nested(b.id);
    assert_ne!(inner_a.id, inner_b.id);
    linked_type(
        &result,
        source,
        "Inner {} local @0 :Inner",
        "Inner",
        inner_a.id,
    );
    linked_type(
        &result,
        source,
        "struct B { struct Inner {} local @0 :Inner",
        "Inner",
        inner_b.id,
    );
    linked_type(&result, source, "cross @1 :A.Inner", "A.Inner", inner_a.id);
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == OccurrenceRole::TypeUse)
            .count(),
        3
    );
}

#[test]
fn capnp_nested_type_leaf_is_unresolved_outside_its_lexical_namespace() {
    let source =
        "struct A { struct Inner {} }\nstruct Host { missing @0 :Inner; exact @1 :A.Inner; }\n";
    let result = output(source);
    let inner = entity(&result, "Inner", EntityKind::Struct);
    linked_type(&result, source, "exact @1 :A.Inner", "A.Inner", inner.id);
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::TypeUse && text(source, &row.source) == "Inner")
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn capnp_multiple_named_returns_are_separate_parameters_with_written_type_edges() {
    let source = "struct Input {} struct Output {}\ninterface Service { get @0 (input: Input) -> (left: Output, right: Input); }\n";
    let result = output(source);
    let method = entity(&result, "get", EntityKind::Method);
    for name in ["input", "left", "right"] {
        exact_definition(
            &result,
            source,
            name,
            EntityKind::Parameter,
            match name {
                "input" => "input: Input",
                "left" => "left: Output",
                _ => "right: Input",
            },
        );
        contains(
            &result,
            method,
            entity(&result, name, EntityKind::Parameter),
        );
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|row| row.kind == EntityKind::Parameter)
            .count(),
        3
    );
    linked_type(
        &result,
        source,
        "input: Input",
        "Input",
        entity(&result, "Input", EntityKind::Struct).id,
    );
    linked_type(
        &result,
        source,
        "left: Output",
        "Output",
        entity(&result, "Output", EntityKind::Struct).id,
    );
    linked_type(
        &result,
        source,
        "right: Input",
        "Input",
        entity(&result, "Input", EntityKind::Struct).id,
    );
}

#[test]
fn capnp_single_extends_and_unnamed_method_types_are_written_uses_not_dispatch() {
    let source = "struct Request {} struct Response {} interface Base {}\ninterface Service extends(Base) { get @0 Request -> Response; }\n";
    let result = output(source);
    for (marker, name, kind) in [
        ("extends(Base)", "Base", EntityKind::Interface),
        ("get @0 Request", "Request", EntityKind::Struct),
        ("-> Response", "Response", EntityKind::Struct),
    ] {
        linked_type(
            &result,
            source,
            marker,
            name,
            entity(&result, name, kind).id,
        );
    }
    assert!(!result.document().relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::Extends | RelationPredicate::Implements
    )));
}

#[test]
fn capnp_named_group_union_and_anonymous_union_preserve_lexical_containment() {
    let source = "struct Owner { info :group { value @0 :Text; } choice :union { first @1 :Text; } union { second @2 :Text; } }\n";
    let result = output(source);
    let owner = entity(&result, "Owner", EntityKind::Struct);
    let group = entity(&result, "info", EntityKind::Field);
    let union = entity(&result, "choice", EntityKind::Union);
    contains(&result, owner, group);
    contains(&result, group, entity(&result, "value", EntityKind::Field));
    contains(&result, owner, union);
    contains(&result, union, entity(&result, "first", EntityKind::Field));
    contains(&result, owner, entity(&result, "second", EntityKind::Field));
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|row| row.kind == EntityKind::Union)
            .count(),
        1
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|row| row.canonical_name == "union")
    );
}

#[test]
fn capnp_duplicate_type_owners_never_select_an_arbitrary_target() {
    let source = "struct Item {} struct Item {} struct Host { value @0 :Item; }\n";
    let result = output(source);
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::TypeUse)
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::UsesType)
    );
}

#[test]
fn capnp_alias_import_and_generic_gaps_disable_document_target_inference() {
    for source in [
        "using Alias = Item; struct Item {} struct Host { value @0 :Item; }\n",
        "using Remote = import \"remote.capnp\"; struct Item {} struct Host { value @0 :Item; }\n",
        "struct Box(T) { value @0 :T; }\n",
    ] {
        let result = output(source);
        let uses = result
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == OccurrenceRole::TypeUse)
            .collect::<Vec<_>>();
        assert!(!uses.is_empty());
        assert!(
            uses.iter()
                .all(|row| matches!(row.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(
                    |gap| gap.detail == "capnp-import-alias-or-generic-binding-unavailable"
                        && !text(source, &gap.source).is_empty()
                )
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|row| row.canonical_name == "Alias" || row.canonical_name == "Remote")
        );
    }
}

#[test]
fn capnp_recovery_and_multiple_inheritance_are_explicit_ranged_gaps() {
    for source in [
        "struct Bad { value @0 : ; }\n",
        "interface A {} interface B {} interface Bad extends(A, B) {}\n",
    ] {
        let result = output(source);
        let gaps = result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "capnp-declaration-name-owner-or-grammar-unavailable")
            .collect::<Vec<_>>();
        assert!(!gaps.is_empty());
        assert!(gaps.iter().all(|gap| !text(source, &gap.source).is_empty()));
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|row| row.canonical_name == "Bad")
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::Extends)
        );
    }
}

#[test]
fn capnp_physical_paths_keep_independent_file_and_type_identities() {
    let source = "struct Item {} struct Host { value @0 :Item; }\n";
    let first = output_at(
        LanguageCase {
            path: "first.capnp",
            ..CAPNP
        },
        source,
    );
    let second = output_at(
        LanguageCase {
            path: "nested/second.capnp",
            ..CAPNP
        },
        source,
    );
    assert_ne!(first.document().files[0].id, second.document().files[0].id);
    let first_item = entity(&first, "Item", EntityKind::Struct);
    let second_item = entity(&second, "Item", EntityKind::Struct);
    assert_ne!(first_item.id, second_item.id);
    linked_type(&first, source, "value @0 :Item", "Item", first_item.id);
    linked_type(&second, source, "value @0 :Item", "Item", second_item.id);
}

#[test]
fn capnp_list_type_preserves_the_complete_nested_custom_type_use() {
    let source = "struct A { struct Inner {} } struct Host { values @0 :List(List(A.Inner)); }\n";
    let result = output(source);
    linked_type(
        &result,
        source,
        "values @0 :List(List(A.Inner))",
        "A.Inner",
        entity(&result, "Inner", EntityKind::Struct).id,
    );
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == OccurrenceRole::TypeUse)
            .count(),
        1
    );
}

#[test]
fn capnp_unproven_qualified_spelling_never_binds_an_individual_prefix() {
    for spelling in ["A . Inner", "A..Inner"] {
        let source =
            format!("struct A {{ struct Inner {{}} }} struct Host {{ value @0 :{spelling}; }}\n");
        let result = output(&source);
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::UsesType)
        );
        let start = source.find(spelling).unwrap();
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(
                    |gap| gap.detail == "capnp-compound-type-spelling-unavailable"
                        && gap.source.span().start_byte() == u64::try_from(start).unwrap()
                        && gap.source.span().end_byte()
                            == u64::try_from(start + spelling.len()).unwrap()
                        && text(&source, &gap.source) == spelling
                )
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|row| row.role == OccurrenceRole::TypeUse)
                .all(|row| matches!(row.target, OccurrenceTarget::Unresolved { .. }))
        );
    }
}
