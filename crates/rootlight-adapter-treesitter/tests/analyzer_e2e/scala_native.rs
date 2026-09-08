//! Scala declarations and source evidence through the production analyzer.
//! Parser success cannot replace exact identity, ownership and coverage assertions.

use super::*;

const SCALA: LanguageCase = LanguageCase {
    name: "scala",
    path: "src/catalog.scala",
    frontend: "tree-sitter-scala-0.26.2",
    source: "object Catalog { def count = 1 }",
    generated: false,
    body_before: "= 1",
    body_after: "= 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SCALA, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, SCALA),
        &request(&fixture.snapshot, &fixture.source, SCALA, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn scala_written_declarations_keep_kinds_companions_and_exact_source() {
    let source = r#"package sample
trait Reader { def read(value: Int): Int }
class Entry(val count: Int)
object Entry { def create(value: Int) = new Entry(value) }
enum Color { case Red, Blue, Green; case Custom(value: Int) }
object Store {
  opaque type Meter = Double
  val (left, right) = (1, 2)
  var active = true
  given ordering: Ordering[Int] = Ordering.Int
  def `odd name`(value: Int) = value
  def /(value: Int) = value
}
"#;
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Reader", EntityKind::Trait),
        ("Entry", EntityKind::Class),
        ("Entry", EntityKind::Namespace),
        ("Color", EntityKind::Enum),
        ("Red", EntityKind::Constant),
        ("Custom", EntityKind::Class),
        ("Blue", EntityKind::Constant),
        ("Green", EntityKind::Constant),
        ("Meter", EntityKind::TypeAlias),
        ("left", EntityKind::Variable),
        ("right", EntityKind::Variable),
        ("active", EntityKind::Variable),
        ("ordering", EntityKind::Variable),
        ("odd name", EntityKind::Function),
        ("/", EntityKind::Function),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.canonical_name == name && e.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}/{kind:?}: {:?}; gaps: {:?}",
            doc.entities,
            doc.skipped_regions
        );
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target
                        == (OccurrenceTarget::Resolved {
                            symbol: entities[0].id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        let written = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(written.trim_matches('`'), name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
}

#[test]
fn scala_package_object_retains_definition_members_and_outer_siblings() {
    let source = "package outer\npackage object utility extends Base { type Name = String; val fallback = \"x\"; def read(value: Name): Name = value }\nobject Outside";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let named = |name: &str| {
        let found: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.canonical_name == name)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {:?}", doc.entities);
        found[0]
    };
    let package = named("utility");
    assert_eq!(package.kind, EntityKind::Namespace);
    assert_eq!(
        package.container,
        Some(rootlight_ir::ContainerRef::Entity(named("outer").id))
    );
    for name in ["Name", "fallback", "read"] {
        assert_eq!(
            named(name).container,
            Some(rootlight_ir::ContainerRef::Entity(package.id))
        );
    }
    assert_eq!(
        named("Outside").container,
        Some(rootlight_ir::ContainerRef::Entity(named("outer").id))
    );
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|o| {
            o.role == OccurrenceRole::Definition
                && o.target == (OccurrenceTarget::Resolved { symbol: package.id })
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let span = definitions[0].source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "utility"
    );
    let identities = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        identities(&result),
        identities(&output(&source.replace("= value }", "= value.toString }")))
    );
}

#[test]
fn scala_package_object_layout_and_quoted_names_keep_exact_source() {
    let header = "package object `utility-kit` extends Base";
    let mut expected = None;
    for source in [
        format!(
            "{header} {{ def read = 1 }}\nobject Outside {{ val text = \"package object Ghost\" }}"
        ),
        format!(
            "{header}:\n  def read = 1\nobject Outside {{ val text = \"package object Ghost\" }}"
        ),
    ] {
        let result = output(&source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        let entity = doc
            .entities
            .iter()
            .find(|e| e.canonical_name == "utility-kit")
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Namespace);
        assert!(!doc.entities.iter().any(|e| e.canonical_name == "Ghost"));
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("`utility-kit`")
        );
        let span = entity.evidence.source.as_ref().unwrap().span();
        let declaration = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(span.start_byte(), 0);
        assert!(declaration.starts_with(header));
        assert!(declaration.contains("def read = 1"));
        assert!(!declaration.contains("object Outside"));
        let identities = doc
            .entities
            .iter()
            .map(|e| (e.id, e.container))
            .collect::<BTreeMap<_, _>>();
        if let Some(ref expected) = expected {
            assert_eq!(&identities, expected);
        } else {
            expected = Some(identities);
        }
    }
}

#[test]
fn scala_package_prefixes_match_explicit_nested_ownership() {
    let declarations = "class Entry\nobject Entry { def read(value: Int) = value }";
    let implicit = output(&format!("package outer\npackage inner\n{declarations}"));
    let explicit = output(&format!(
        "package outer {{ package inner {{ {declarations} }} }}"
    ));
    for result in [&implicit, &explicit] {
        let doc = result.document();
        let package = |name: &str| {
            doc.entities
                .iter()
                .find(|e| e.canonical_name == name && e.kind == EntityKind::Namespace)
                .unwrap()
        };
        let outer = package("outer");
        let inner = package("inner");
        assert_eq!(
            inner.container,
            Some(rootlight_ir::ContainerRef::Entity(outer.id)),
            "{:?}",
            doc.entities
        );
        for entry in doc.entities.iter().filter(|e| e.canonical_name == "Entry") {
            assert_eq!(
                entry.container,
                Some(rootlight_ir::ContainerRef::Entity(inner.id)),
                "{:?}",
                doc.entities
            );
        }
        assert_eq!(
            doc.entities
                .iter()
                .filter(|e| e.canonical_name == "Entry")
                .count(),
            2
        );
    }
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&implicit), identities(&explicit));
}

#[test]
fn scala_package_prefix_preserves_explicit_siblings_and_definition_bytes() {
    let source = "// prefix\npackage root\n/* between */\npackage left { class Entry }\npackage right { class Entry }\nclass Outside";
    let result = output(source);
    let doc = result.document();
    let owner = |name: &str| {
        doc.entities
            .iter()
            .find(|e| e.canonical_name == name)
            .unwrap()
    };
    for (name, parent) in [("left", "root"), ("right", "root"), ("Outside", "root")] {
        assert_eq!(
            owner(name).container,
            Some(rootlight_ir::ContainerRef::Entity(owner(parent).id))
        );
    }
    let entries: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| e.canonical_name == "Entry")
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries.iter().map(|e| e.container).collect::<BTreeSet<_>>(),
        ["left", "right"]
            .into_iter()
            .map(|name| Some(rootlight_ir::ContainerRef::Entity(owner(name).id)))
            .collect()
    );
    for name in ["root", "left", "right"] {
        let entity = owner(name);
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let written = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(written, name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
    let span = owner("left").evidence.source.as_ref().unwrap().span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "package left { class Entry }"
    );
}

#[test]
fn scala_nonleading_unbraced_package_never_claims_following_declarations() {
    let source = "object Before {}\npackage late\nclass After";
    let result = output(source);
    let doc = result.document();
    let after = doc
        .entities
        .iter()
        .find(|e| e.canonical_name == "After")
        .unwrap();
    let file_module = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(
        after.container,
        Some(rootlight_ir::ContainerRef::Entity(file_module.id))
    );
    let gap = doc
        .skipped_regions
        .iter()
        .find(|g| g.detail == "scala-nonleading-unbraced-package-scope-unavailable")
        .unwrap();
    let span = gap.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "package late"
    );
}

#[test]
fn scala_package_ownership_replays_exactly_with_retained_generation_evidence() {
    for source in [
        "package outer\npackage inner\nobject Store { def read(value: Int) = value }",
        "package outer\npackage object utility { def read(value: Int) = value }",
    ] {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, SCALA);
        let fixture = Fixture::new(SCALA, source.as_bytes());
        let initial_limits = limits();
        let initial_request = request(&fixture.snapshot, &fixture.source, SCALA, &initial_limits);
        let (initial, artifact) = analyzer
            .analyze_and_capture(
                &initial_request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let successor = fixture.next_generation();
        let reduced_limits =
            limits_with_syntax_records(artifact.syntax_fact_count().checked_add(1).unwrap());
        let successor_request = request(
            &successor.snapshot,
            &successor.source,
            SCALA,
            &reduced_limits,
        );
        let reused = analyzer
            .analyze_from_artifact(
                &successor_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let fresh = analyze(&analyzer, &successor_request, &ExtensionSupport::default());
        assert_eq!(reused.document(), fresh.document());
        assert_eq!(reused.report(), fresh.report());
        let identities = |result: &AnalysisOutput| {
            result
                .document()
                .entities
                .iter()
                .map(|e| (e.id, e.container))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(identities(&initial), identities(&reused));
        assert!(reused.document().entities.iter().all(|e| {
            e.evidence
                .source
                .as_ref()
                .is_some_and(|r| r.generation() == successor.source.generation())
        }));
    }
}

#[test]
fn scala_overloads_and_disjoint_blocks_preserve_unique_stable_owners() {
    let source = "object Store { def read(value: Int): Int = value; def read(value: String): String = value; def run = { { val local = 1 }; { val local = 2 } } }";
    let initial = output(source);
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        identities(&initial).len(),
        initial.document().entities.len()
    );
    for (name, expected) in [("read", 2), ("value", 2), ("local", 2)] {
        let ids: BTreeSet<_> = initial
            .document()
            .entities
            .iter()
            .filter(|e| e.canonical_name == name)
            .map(|e| e.id)
            .collect();
        assert_eq!(
            ids.len(),
            expected,
            "{name}: {:?}",
            initial.document().entities
        );
    }
    for changed in [
        format!("// moved\n{source}"),
        source.replace("local = 1", "local = 100"),
        source.replace("Int = value", "Int = value + 1"),
    ] {
        assert_eq!(identities(&initial), identities(&output(&changed)));
    }
}

#[test]
fn scala_pattern_bindings_do_not_promote_extractors_or_literal_text() {
    let source = "object Store { /* def Ghost = 0 */ val text = \"class Fake\"; def read(input: Option[Int]) = input match { case Some(value) => value; case None => 0 } }";
    let result = output(source);
    let names: Vec<_> = result
        .document()
        .entities
        .iter()
        .map(|e| e.canonical_name.as_str())
        .collect();
    assert!(names.contains(&"value"), "{names:?}");
    for absent in ["Some", "None", "Ghost", "Fake"] {
        assert!(!names.contains(&absent), "{names:?}");
    }
    assert!(
        result.document().skipped_regions.iter().any(
            |g| g.detail == "scala-import-inheritance-implicit-dispatch-resolution-unavailable"
        )
    );
}

#[test]
fn scala_extensions_generators_and_closures_keep_local_owners() {
    let source = r#"object Store {
  extension (value: String) { def sized = value.length }
  given Ordering[Int] with { def compare(left: Int, right: Int) = left - right }
  def run = {
    val first = List(1).map(item => item + 1)
    val second = List(2).map(item => item + 2)
    for (entry <- List(1); copy = entry) yield copy
  }
}"#;
    let initial = output(source);
    assert!(
        initial.document().diagnostics.is_empty(),
        "{:?}",
        initial.document().diagnostics
    );
    for (name, count) in [
        ("value", 1),
        ("sized", 1),
        ("compare", 1),
        ("left", 1),
        ("right", 1),
        ("item", 2),
        ("entry", 1),
        ("copy", 1),
    ] {
        let entities: Vec<_> = initial
            .document()
            .entities
            .iter()
            .filter(|e| e.canonical_name == name)
            .collect();
        assert_eq!(
            entities.len(),
            count,
            "{name}: {:?}; {:?}",
            initial.document().entities,
            initial.document().skipped_regions
        );
        assert_eq!(
            entities.iter().map(|e| e.id).collect::<BTreeSet<_>>().len(),
            count
        );
    }
    let ids = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|e| e.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        ids(&initial),
        ids(&output(&source.replace("item + 1", "item + 100")))
    );
    assert!(
        initial
            .document()
            .skipped_regions
            .iter()
            .any(|g| g.detail == "scala-anonymous-runtime-declaration-identity-unavailable")
    );
}

#[test]
fn scala_unicode_patterns_distinguish_binders_from_stable_names() {
    let result = output(
        "object Store { def read(input: Int) = input match { case λ => λ; case ⅰ => 0; case `Existing` => 1; case _ => 2 } }",
    );
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    let names: Vec<_> = result
        .document()
        .entities
        .iter()
        .map(|e| e.canonical_name.as_str())
        .collect();
    assert!(names.contains(&"λ"), "{names:?}");
    for absent in ["ⅰ", "Existing", "_"] {
        assert!(!names.contains(&absent), "{names:?}");
    }
}

#[test]
fn scala_recovery_preserves_healthy_declarations_with_a_source_gap() {
    let source = "object Store { def healthy = 1; def broken( }";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|e| e.canonical_name == "healthy")
    );
    let gap = result
        .document()
        .skipped_regions
        .iter()
        .find(|g| g.reason == SkippedRegionReason::ParseError)
        .expect("malformed syntax retains a source-bound gap");
    let span = gap.evidence.source.as_ref().unwrap().span();
    let start = usize::try_from(span.start_byte()).unwrap();
    let end = usize::try_from(span.end_byte()).unwrap();
    assert!(start <= end && end <= source.len());
    // The native recovery identifies the unmatched delimiter, not the healthy name.
    assert_eq!(&source[start..end], "(");
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}
