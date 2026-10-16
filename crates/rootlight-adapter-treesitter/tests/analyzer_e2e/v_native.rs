//! V product fixtures keep written tokens, physical ownership and binding gaps.
//! Native syntax recovery never justifies promoting an unresolved target.

use super::*;

const V: LanguageCase = LanguageCase {
    name: "v",
    path: "src/written.v",
    frontend: "tree-sitter-v-0.0.6",
    source: "module written\nimport os\ntype Count = int\nstruct Box[T] {\n value T\n}\ninterface Reader {\n read(input int) int\n}\nenum State {\n idle\n ready\n}\nconst limit = 3\n__global ( counter int )\nfn (b Box[int]) get(value int) int {\n local := value\n return local\n}\nfn helper(input int) int { return input + 1 }\n",
    generated: false,
    body_before: "input + 1",
    body_after: "input + 2",
};

fn output(source: &str) -> AnalysisOutput {
    output_for(V, source)
}

fn output_for(case: LanguageCase, source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(occurrence.syntactic_text_hash, content_hash(bytes));
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_eq!(occurrence.source.generation(), document.generation);
        if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
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
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    name: &str,
    kind: EntityKind,
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == name && entity.kind == kind)
        .unwrap_or_else(|| {
            panic!(
                "missing {name} {kind:?}: {:?}",
                document
                    .entities
                    .iter()
                    .map(|entity| (&entity.canonical_name, entity.kind))
                    .collect::<Vec<_>>()
            )
        });
    let occurrence = document.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition
        && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)).unwrap();
    let span = occurrence.source.span();
    let written = &source
        [usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()];
    assert_eq!(written, name);
}

fn contains(document: &rootlight_ir::NormalizedIrDocument, owner: &str, child: &str) {
    let parent = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == owner)
        .unwrap();
    let child = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == child)
        .unwrap();
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id)),
        "missing {owner} Contains {}",
        child.canonical_name
    );
}

#[test]
fn v_written_declarations_preserve_exact_names_and_containment() {
    let result = output(V.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("written", EntityKind::Module),
        ("Count", EntityKind::TypeAlias),
        ("Box", EntityKind::Struct),
        ("T", EntityKind::TypeParameter),
        ("value", EntityKind::Field),
        ("Reader", EntityKind::Interface),
        ("read", EntityKind::Method),
        ("State", EntityKind::Enum),
        ("idle", EntityKind::Constant),
        ("ready", EntityKind::Constant),
        ("limit", EntityKind::Constant),
        ("counter", EntityKind::Variable),
        ("get", EntityKind::Method),
        ("b", EntityKind::Parameter),
        ("local", EntityKind::Variable),
        ("helper", EntityKind::Function),
    ] {
        definition(document, V.source, name, kind);
    }
    for (parent, child) in [
        ("Box", "T"),
        ("Box", "value"),
        ("Reader", "read"),
        ("State", "idle"),
        ("get", "b"),
        ("get", "local"),
    ] {
        contains(document, parent, child);
    }
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Import && entity.canonical_name == "os")
    );
}

#[test]
fn v_calls_and_type_references_keep_source_bound_unresolved_targets() {
    let source = "module written\nstruct Box {}\nfn helper(value int) int { return value }\nfn use(box Box) int { return helper(2) }\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let call = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .expect("written call");
    let span = call.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "helper"
    );
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert!(!document.skipped_regions.is_empty());
}

#[test]
fn v_incomplete_owners_and_inert_source_do_not_invent_definitions() {
    for source in [
        "module written\nstruct Broken {\n hidden int\n",
        "module written\n// fn hidden() {}\nfn kept() { text := 'fn invented() {}' }\n",
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "hidden" | "invented"))
        );
    }
}

#[test]
fn v_inline_globals_semicolons_and_defer_mode_preserve_written_functions() {
    let source = "module written\n__global ( enabled bool count int )\nfn written() { defer(fn) { println(count) }; local := 1; println(local) }\n";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    for (name, kind) in [
        ("enabled", EntityKind::Variable),
        ("count", EntityKind::Variable),
        ("written", EntityKind::Function),
        ("local", EntityKind::Variable),
    ] {
        definition(result.document(), source, name, kind);
    }
}

#[test]
fn v_union_and_foreign_headers_preserve_authored_identity() {
    let source = "module written\npub union Number {\n value f64\n bits u64\n}\nstruct Box { value int }\nfn (left Box) + (right Box) Box { return left }\nfn C.puts(&char) i32\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    for (name, kind) in [
        ("Number", EntityKind::Union),
        ("Box", EntityKind::Struct),
        ("+", EntityKind::Method),
        ("C.puts", EntityKind::Function),
    ] {
        definition(result.document(), source, name, kind);
    }
    contains(result.document(), "Number", "bits");
}

#[test]
fn v_unsupported_anonymous_owners_do_not_promote_nested_bindings() {
    for (source, retained, kind) in [
        (
            "module written\nfn outer() { callback := fn () { hidden := 1; println(hidden) }; println(callback) }\n",
            "callback",
            EntityKind::Variable,
        ),
        (
            "module written\nstruct Outer {\n inner struct {\n  hidden int\n }\n}\n",
            "inner",
            EntityKind::Field,
        ),
    ] {
        let result = output(source);
        let document = result.document();
        assert!(document.diagnostics.is_empty());
        definition(document, source, retained, kind);
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "hidden"),
            "an unsupported anonymous owner cannot be replaced by a surrounding owner"
        );
        let start = u64::try_from(source.find("hidden").unwrap()).unwrap();
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.domain == domain
                    && gap.detail == "v-written-construct-analysis-unavailable"
                    && gap.source.span().start_byte() <= start
                    && start + 6 <= gap.source.span().end_byte()
            }));
        }
    }
}

#[test]
fn v_discard_bindings_do_not_define_a_named_variable() {
    let source = "module written\nfn pair() (int, int) { return 1, 2 }\nfn written() { _, value := pair(); println(value); for _, item in [1, 2] { println(item) } }\n";
    let result = output(source);
    let document = result.document();
    assert!(document.diagnostics.is_empty());
    definition(document, source, "value", EntityKind::Variable);
    definition(document, source, "item", EntityKind::Variable);
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "_")
    );
    for occurrence in &document.occurrences {
        if occurrence.role == OccurrenceRole::Definition {
            let span = occurrence.source.span();
            assert_ne!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                "_"
            );
        }
    }
}
