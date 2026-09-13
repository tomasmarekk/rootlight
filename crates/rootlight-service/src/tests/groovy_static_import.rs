//! Static-import precedence through public indexing and exact source retrieval.
//! Synthetic cases distinguish compiler lookup order from same-spelling guesses.

use super::*;
use rootlight_ir::{
    ContainerRef, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
};

const FIRST: &str = "package sample\nclass First { static def fold(value) { value } }";
const SECOND: &str = "package sample\nclass Second { static def fold(value) { value } }";
const CLIENT: &str = "class Client { def inspect(value) { fold(value) } }";

#[test]
fn fenced_declarations_do_not_compete_with_compilation_units() {
    for example in [
        FIRST,
        "package sample\nclass First { static def fold(value) { value } }\nclass Broken {",
        "class Illustration { def render(value) { value } }",
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("first.groovy"), FIRST).unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            format!("import static sample.First.fold\n{CLIENT}"),
        )
        .unwrap();
        let documentation = format!("# Examples\n\n```groovy\n{example}\n```\n");
        fs::write(fixture.path().join("guide.md"), &documentation).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let native = document
            .files
            .iter()
            .find(|file| file.path == "first.groovy")
            .unwrap();
        let host = document
            .files
            .iter()
            .find(|file| file.path == "guide.md")
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "fold"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == native.id)
            })
            .unwrap();
        let caller = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "inspect")
            .unwrap();
        let call = document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite && site.enclosing == Some(caller.id))
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{example}"
        );
        let response = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([target.id]),
                vec![RelationFamily::CalledBy],
                Some(rootlight_query::RelationDirection::Inbound),
                0,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(
            response
                .data
                .groups
                .iter()
                .flat_map(|group| &group.items)
                .any(|edge| {
                    edge.symbol == caller.id && edge.source_refs.contains(&call.source)
                })
        );
        let embedded = document
            .entities
            .iter()
            .find(|entity| {
                entity.language == "groovy"
                    && entity.kind == rootlight_ir::EntityKind::Class
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == host.id)
            })
            .unwrap();
        let source = embedded.evidence.source.clone().unwrap();
        assert_eq!(source.generation(), receipt.generation);
        let span = source.span();
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            read.data.chunks[0].bytes,
            documentation.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        );
    }
}

#[test]
fn modifier_only_fields_preserve_cross_file_static_import_calls() {
    for modifier in ["public", "protected", "private", "static", "final"] {
        check(
            &format!("import static sample.First.fold\n{CLIENT}"),
            FIRST,
            &format!("class Holder {{ {modifier} handler = {{ value -> value }} }}"),
            Some(("First", "fold")),
            "fold(value)",
        );
    }
}

#[test]
fn imported_assignments_select_authored_setters_and_exact_sources() {
    for (declaration, import, expression, name, expected) in [
        (
            "static void store(value) {}",
            "store as setChosen",
            "chosen = 4",
            "chosen",
            Some("store"),
        ),
        (
            "public static int count",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("count"),
        ),
        (
            "static int count",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("count"),
        ),
        (
            "public static int count; static void setCount(value) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount(value) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount(value) {}",
            "setCount as setChosen",
            "chosen = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount(value) {}",
            "*",
            "count = 4",
            "count",
            Some("setCount"),
        ),
        (
            "static void setCount(value = 1) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount(Object... values) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount(first, second = 1) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount(value) {}",
            "count as chosen",
            "(chosen) = 4",
            "chosen",
            None,
        ),
        (
            "static void setCount(value) {}",
            "count as chosen",
            "chosen /* write */ = 4",
            "chosen",
            Some("setCount"),
        ),
        (
            "static void setCount() {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            None,
        ),
        (
            "static int getCount() { 1 }",
            "count as chosen",
            "chosen = 4",
            "chosen",
            None,
        ),
        (
            "void setCount(value) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            None,
        ),
        (
            "private static void setCount(value) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            None,
        ),
        (
            "static void setCount(int value) {}; static void setCount(String value) {}",
            "count as chosen",
            "chosen = 4",
            "chosen",
            None,
        ),
        (
            "static int getCount() { 1 }; static void setCount(value) {}",
            "count as chosen",
            "chosen += 4",
            "chosen",
            None,
        ),
        (
            "static void setCount(value) {}",
            "count as chosen",
            "chosen++",
            "chosen",
            None,
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\nclass Tools {{ {declaration} }}"),
        )
        .unwrap();
        fs::write(fixture.path().join("client.groovy"), format!("import static sample.Tools.{import}\nclass Client {{ def inspect() {{ {expression} }} }}")).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let site = document
            .occurrences
            .iter()
            .find(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(name.as_bytes())
            })
            .unwrap();
        let calls: Vec<_> = document
            .relations
            .iter()
            .filter(|r| {
                r.predicate == RelationPredicate::Calls
                    && r.evidence.source.as_ref() == Some(&site.source)
            })
            .collect();
        if let Some(expected) = expected {
            let target = document
                .entities
                .iter()
                .find(|e| e.canonical_name == expected)
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{declaration}: {expression}"
            );
            let call_count = usize::from(target.kind == rootlight_ir::EntityKind::Method);
            assert_eq!(calls.len(), call_count, "{declaration}: {expression}");
            for call in &calls {
                assert_eq!(call.object, RelationEndpoint::Entity(target.id));
                assert_eq!(call.provenance, site.provenance);
            }
            assert!(
                document
                    .relations
                    .iter()
                    .any(|r| r.predicate == RelationPredicate::RefersTo
                        && r.object == RelationEndpoint::Entity(target.id)
                        && r.evidence.source.as_ref() == Some(&site.source))
            );
            let caller = document
                .entities
                .iter()
                .find(|e| e.canonical_name == "inspect")
                .unwrap();
            for (seed, family, destination) in [
                (caller.id, RelationFamily::Calls, target.id),
                (target.id, RelationFamily::CalledBy, caller.id),
            ] {
                let result = service
                    .symbol_relationships(
                        receipt.generation,
                        BTreeSet::from([seed]),
                        vec![family],
                        None,
                        0,
                        10,
                        0,
                        &deadline(),
                    )
                    .unwrap();
                let edges: Vec<_> = result.data.groups.iter().flat_map(|g| &g.items).collect();
                assert_eq!(edges.len(), call_count, "{declaration}: {family:?}");
                for edge in edges {
                    assert_eq!(edge.symbol, destination);
                    assert_eq!(edge.source_refs, vec![site.source.clone()]);
                    assert_eq!(edge.source_refs[0].generation(), receipt.generation);
                }
            }
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{declaration}: {expression}: {:?}",
                site.target
            );
            assert!(calls.is_empty(), "{declaration}: {expression}");
        }
        let source = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![site.source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(source.data.chunks.len(), 1);
        assert_eq!(source.data.chunks[0].bytes, name.as_bytes());
    }
}

#[test]
fn imported_boolean_getters_require_primitive_returns_and_empty_parameters() {
    for (declaration, expected) in [
        ("static boolean isReady() { true }", "isReady"),
        (
            "static boolean isReady() { true }; static int getReady() { 1 }",
            "isReady",
        ),
        (
            "static Boolean isReady() { true }; static int getReady() { 1 }",
            "getReady",
        ),
        (
            "static def isReady() { true }; static int getReady() { 1 }",
            "getReady",
        ),
        (
            "static boolean[] isReady() { [] }; static int getReady() { 1 }",
            "getReady",
        ),
        (
            "static int isReady() { 1 }; static int getReady() { 2 }",
            "getReady",
        ),
        (
            "static boolean isReady(value = 1) { true }; static int getReady() { 2 }",
            "getReady",
        ),
        (
            "static boolean isReady(Object... values) { true }; static int getReady() { 2 }",
            "getReady",
        ),
        (
            "static boolean isReady(value) { true }; static int getReady() { 2 }",
            "getReady",
        ),
        (
            "static boolean /* type trivia */ isReady() { true }",
            "isReady",
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\nclass Tools {{ {declaration} }}"),
        )
        .unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            "import static sample.Tools.ready as chosen\nclass Client { def inspect() { chosen } }",
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let target = document
            .entities
            .iter()
            .find(|e| e.canonical_name == expected)
            .unwrap();
        let read = document
            .occurrences
            .iter()
            .find(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
            })
            .unwrap();
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{declaration}"
        );
        assert!(
            document
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls
                    && r.object == RelationEndpoint::Entity(target.id)
                    && r.evidence.source.as_ref() == Some(&read.source))
        );
    }
}

#[test]
fn imported_boolean_getter_aliases_do_not_promote_boxed_returns() {
    for (return_type, resolved) in [("boolean", true), ("Boolean", false)] {
        let fixture = TempDir::new().unwrap();
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\nclass Tools {{ static {return_type} isReady() {{ true }} }}"),
        )
        .unwrap();
        fs::write(fixture.path().join("client.groovy"),
            "import static sample.Tools.isReady as isChosen\nclass Client { def inspect() { chosen } }"
        ).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let read = document
            .occurrences
            .iter()
            .find(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
            })
            .unwrap();
        if resolved {
            let getter = document
                .entities
                .iter()
                .find(|e| e.canonical_name == "isReady")
                .unwrap();
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved { symbol: getter.id }
            );
        } else {
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
        }
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|r| r.predicate == RelationPredicate::Calls
                    && r.evidence.source.as_ref() == Some(&read.source))
                .count(),
            usize::from(resolved)
        );
    }
}

#[test]
fn imported_value_reads_select_fields_properties_and_getters() {
    for (member, expected) in [
        ("static boolean isCount() { true }", "isCount"),
        (
            "static Boolean isCount() { true }; static int getCount() { 1 }",
            "getCount",
        ),
        ("public static int count = 1", "count"),
        ("static int count = 1", "count"),
        ("static int getCount() { 1 }", "getCount"),
        (
            "public static int count = 1; static int getCount() { 2 }",
            "getCount",
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\nclass Tools {{ {member} }}"),
        )
        .unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            "import static sample.Tools.count as chosen\nclass Client { def inspect() { chosen } }",
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == expected)
            .unwrap();
        let read = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "groovy.identifier.reference"
                    && site.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
            })
            .unwrap();
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{member}"
        );
        let calls: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Calls
                    && relation.evidence.source.as_ref() == Some(&read.source)
            })
            .collect();
        assert_eq!(
            calls.len(),
            usize::from(target.kind == rootlight_ir::EntityKind::Method),
            "getter invocation, not a fabricated field/property call: {member}"
        );
        if let Some(call) = calls.first() {
            assert_eq!(call.object, RelationEndpoint::Entity(target.id));
            assert_eq!(
                call.subject,
                RelationEndpoint::Entity(read.enclosing.unwrap())
            );
            assert_eq!(call.provenance, read.provenance);
        }
        let caller = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "inspect")
            .unwrap();
        for (seed, family, destination) in [
            (caller.id, RelationFamily::Calls, target.id),
            (target.id, RelationFamily::CalledBy, caller.id),
        ] {
            let result = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([seed]),
                    vec![family],
                    None,
                    0,
                    10,
                    0,
                    &deadline(),
                )
                .unwrap();
            let edges: Vec<_> = result.data.groups.iter().flat_map(|g| &g.items).collect();
            assert_eq!(edges.len(), calls.len(), "{member}: {family:?}");
            for edge in edges {
                assert_eq!(edge.symbol, destination);
                assert_eq!(edge.source_refs, vec![read.source.clone()]);
                assert_eq!(edge.source_refs[0].generation(), receipt.generation);
                let source = service
                    .source_read_with_options_and_budget(
                        receipt.generation,
                        edge.source_refs.clone(),
                        SourceReadOptions::new()
                            .with_context_lines_before(0)
                            .with_context_lines_after(0),
                        FirstSliceBudget::default(),
                        &deadline(),
                    )
                    .unwrap();
                assert_eq!(source.data.chunks.len(), 1);
                assert_eq!(source.data.chunks[0].bytes, b"chosen");
            }
        }
    }
}

#[test]
fn imported_value_reads_follow_explicit_accessor_and_star_precedence() {
    for (imports, value, owner_name, member_name) in [
        (
            "import static sample.First.count as chosen",
            "chosen",
            "First",
            "count",
        ),
        ("import static sample.First.*", "count", "First", "count"),
        (
            "import static sample.First.*\nimport static sample.Second.*",
            "count",
            "First",
            "count",
        ),
        (
            "import static sample.Second.*\nimport static sample.First.*",
            "count",
            "Second",
            "count",
        ),
        (
            "import static sample.First.*\nimport static sample.Second.count",
            "count",
            "Second",
            "count",
        ),
        (
            "import static sample.First.count as chosen\nimport static sample.Second.count as chosen",
            "chosen",
            "Second",
            "count",
        ),
        (
            "import static sample.Accessors.getCount as getChosen",
            "chosen",
            "Accessors",
            "getCount",
        ),
        (
            "import static sample.First.count as chosen\nimport static sample.Accessors.getCount as getChosen",
            "chosen",
            "Accessors",
            "getCount",
        ),
        (
            "import static sample.First.missing as count\nimport static sample.Second.*",
            "count",
            "Second",
            "count",
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("members.groovy"), "package sample\nclass First { public static int count = 1 }\nclass Second { static int count = 2 }\nclass Accessors { static int getCount() { 3 } }").unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            format!("{imports}\nclass Client {{ def inspect() {{ {value} }} }}"),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let owner = document
            .entities
            .iter()
            .find(|e| e.canonical_name == owner_name)
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|e| {
                e.canonical_name == member_name
                    && e.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(value.as_bytes())
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{imports}"
        );
        assert!(
            document
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::RefersTo
                    && r.subject == RelationEndpoint::Occurrence(site.id)
                    && r.object == RelationEndpoint::Entity(target.id))
        );
    }
}

#[test]
fn imported_accessor_calls_survive_all_application_paths_without_duplicates() {
    use rootlight_resolve::{ResolutionLimits, ResolverFactContext};

    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("tools.groovy"),
        "package sample\nclass Tools { static int getCount() { 1 }; static void setCount(value) {} }",
    )
    .unwrap();
    fs::write(fixture.path().join("client.groovy"),
        "import static sample.Tools.count as chosen\nclass Client { def inspect() { chosen = chosen; chosen = chosen } }").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let original = snapshot.document();
    let engine = ResolutionEngine::default();
    let context =
        ResolverFactContext::new(rootlight_ids::content_hash(b"accessor-call-application"));
    let cancel = rootlight_cancel::Cancellation::new();
    let mut unresolved = original.clone();
    let mut read_sources = Vec::new();
    // Reconstruct the native value inputs so every application path must create
    // the edges; replaying only resolved service output would test a no-op.
    for read in unresolved.occurrences.iter_mut().filter(|s| {
        s.syntax_kind == "groovy.identifier.reference"
            && s.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
    }) {
        read_sources.push(read.source.clone());
        read.target = OccurrenceTarget::Unresolved {
            text_hash: read.syntactic_text_hash,
        };
        read.confidence = rootlight_ir::Confidence::new(0).unwrap();
        read.provenance = unresolved
            .files
            .iter()
            .find(|f| f.id == read.file)
            .unwrap()
            .provenance;
        read.id = rootlight_ir::derive_occurrence_record_id(read).unwrap();
    }
    assert_eq!(read_sources.len(), 4);
    unresolved.relations.retain(|r| {
        !r.evidence
            .source
            .as_ref()
            .is_some_and(|s| read_sources.contains(s))
    });
    rootlight_ir::validate_ir_document(
        &unresolved,
        &rootlight_ir::IrLimits::default(),
        &Default::default(),
    )
    .unwrap();
    let first = engine
        .apply_document(unresolved.clone(), context, &cancel)
        .unwrap();
    let check = |document: &rootlight_ir::NormalizedIrDocument| {
        let caller = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "inspect")
            .unwrap();
        let reads: Vec<_> = document
            .occurrences
            .iter()
            .filter(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
            })
            .collect();
        assert_eq!(reads.len(), 4);
        for read in reads {
            let value = document
                .extensions
                .iter()
                .find(|e| {
                    e.namespace == rootlight_ir::GROOVY_VALUE_LOOKUP_NAMESPACE
                        && e.evidence.source.as_ref() == Some(&read.source)
                })
                .map(|e| rootlight_ir::decode_groovy_value_lookup_envelope(e).unwrap())
                .unwrap();
            let expected = if value.read_only {
                "getCount"
            } else {
                "setCount"
            };
            let accessor = document
                .entities
                .iter()
                .find(|e| e.canonical_name == expected)
                .unwrap();
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: accessor.id
                }
            );
            for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
                let relations: Vec<_> = document
                    .relations
                    .iter()
                    .filter(|r| {
                        r.predicate == predicate && r.evidence.source.as_ref() == Some(&read.source)
                    })
                    .collect();
                assert_eq!(relations.len(), 1, "one exact relation per value and kind");
                let relation = relations[0];
                assert_eq!(relation.object, RelationEndpoint::Entity(accessor.id));
                assert_eq!(
                    relation.subject,
                    if predicate == RelationPredicate::Calls {
                        RelationEndpoint::Entity(caller.id)
                    } else {
                        RelationEndpoint::Occurrence(read.id)
                    }
                );
                assert_eq!(relation.provenance, read.provenance);
                assert!(
                    relation
                        .evidence
                        .derivation
                        .contains(&rootlight_ir::FactRef::Fact(read.id))
                );
            }
        }
    };
    check(original);
    check(&first);
    assert_eq!(
        engine
            .apply(unresolved.clone(), context, &cancel)
            .unwrap()
            .document,
        first
    );
    assert_eq!(
        engine
            .apply_document_bounded(unresolved, context, &cancel)
            .unwrap()
            .0,
        first
    );
    assert_eq!(
        engine
            .apply_document(first.clone(), context, &cancel)
            .unwrap(),
        first
    );
    assert_eq!(
        engine
            .apply(first.clone(), context, &cancel)
            .unwrap()
            .document,
        first
    );
    assert_eq!(
        engine
            .apply_document_bounded(first.clone(), context, &cancel)
            .unwrap()
            .0,
        first
    );
    let narrow = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (partial, work) = narrow
        .apply_document_bounded(first.clone(), context, &cancel)
        .unwrap();
    assert!(!work.fits());
    assert_eq!(partial, first);
    assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
    assert!(matches!(
        engine.apply_document(first, context, &cancel),
        Err(rootlight_resolve::ResolutionError::Cancelled(_))
    ));
}

#[test]
fn imported_value_reads_preserve_locals_and_do_not_guess_write_accessors() {
    for (body, lexical) in [
        ("def inspect(chosen) { chosen }", true),
        ("def inspect() { def chosen = 4; chosen }", true),
        ("def inspect() { chosen = 4 }", false),
        ("def inspect() { (chosen) = 4 }", false),
        ("def inspect() { chosen++ }", false),
        ("def inspect() { ++chosen }", false),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(
            fixture.path().join("tools.groovy"),
            "package sample\nclass Tools { static int getCount() { 1 } }",
        )
        .unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            format!("import static sample.Tools.count as chosen\nclass Client {{ {body} }}"),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let site = document
            .occurrences
            .iter()
            .find(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
            })
            .unwrap();
        if lexical {
            let local = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "chosen"
                        && matches!(
                            e.kind,
                            rootlight_ir::EntityKind::Parameter
                                | rootlight_ir::EntityKind::Variable
                        )
                })
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: local.id },
                "{body}"
            );
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{body}: {:?}",
                site.target
            );
        }
        assert!(
            !document
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls
                    && r.evidence.source.as_ref() == Some(&site.source)),
            "locals and writes must not fabricate getter calls: {body}"
        );
    }
}

#[test]
fn imported_values_recompute_dependencies_and_restore_exact_sources() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::write(fixture.path().join("client.groovy"), "import static sample.Tools.count as chosen\nclass Client { def inspect() { [chosen, chosen]; chosen = 1; chosen = 2 } }").unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    for (member, read_target, write_target) in [
        ("public static int count = 1", Some("count"), Some("count")),
        ("static int getCount() { 2 }", Some("getCount"), None),
        ("static boolean isCount() { true }", Some("isCount"), None),
        (
            "static Boolean isCount() { true }; static int getCount() { 2 }",
            Some("getCount"),
            None,
        ),
        ("private static int count = 3", None, None),
        ("static int count = 4", Some("count"), Some("count")),
        ("static void setCount(value) {}", None, Some("setCount")),
        (
            "static int getCount() { 2 }; static void setCount(value) {}",
            Some("getCount"),
            Some("setCount"),
        ),
        (
            "static int getCount() { 2 }; private static void setCount(value) {}",
            Some("getCount"),
            None,
        ),
        (
            "static int getCount() { 2 }; static void setCount(first, second) {}",
            Some("getCount"),
            None,
        ),
    ] {
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\nclass Tools {{ {member} }}"),
        )
        .unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let reads: Vec<_> = document
            .occurrences
            .iter()
            .filter(|s| {
                s.syntax_kind == "groovy.identifier.reference"
                    && s.syntactic_text_hash == rootlight_ids::content_hash(b"chosen")
            })
            .collect();
        assert_eq!(reads.len(), 4);
        for read in &reads {
            let value = document
                .extensions
                .iter()
                .find(|e| {
                    e.namespace == rootlight_ir::GROOVY_VALUE_LOOKUP_NAMESPACE
                        && e.evidence.source.as_ref() == Some(&read.source)
                })
                .map(|e| rootlight_ir::decode_groovy_value_lookup_envelope(e).unwrap())
                .unwrap();
            let expected = if value.read_only {
                read_target
            } else {
                write_target
            };
            let calls: Vec<_> = document
                .relations
                .iter()
                .filter(|r| {
                    r.predicate == RelationPredicate::Calls
                        && r.evidence.source.as_ref() == Some(&read.source)
                })
                .collect();
            assert_eq!(
                calls.len(),
                usize::from(matches!(
                    expected,
                    Some("getCount" | "isCount" | "setCount")
                )),
                "{member}"
            );
            if let Some(name) = expected {
                let target = document
                    .entities
                    .iter()
                    .find(|e| e.canonical_name == name)
                    .unwrap();
                assert_eq!(
                    read.target,
                    OccurrenceTarget::Resolved { symbol: target.id },
                    "{member}"
                );
                let provenance = document
                    .provenance
                    .iter()
                    .find(|p| p.id == read.provenance)
                    .unwrap();
                assert!(provenance.input_sources.contains(&read.source));
                assert!(
                    provenance
                        .input_sources
                        .contains(target.evidence.source.as_ref().unwrap())
                );
            } else {
                assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            }
        }
        let source = service
            .source_read_with_options_and_budget(
                receipt.generation,
                reads.iter().map(|r| r.source.clone()).collect(),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(source.data.chunks.len(), 4);
        for chunk in &source.data.chunks {
            assert_eq!(chunk.bytes, b"chosen");
        }
        let original = document.clone();
        drop(snapshot);
        assert_eq!(
            service
                .index_repository(fixture.path(), &deadline())
                .unwrap()
                .generation,
            receipt.generation
        );
        let clean = service
            .index_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert_eq!(clean.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
            &original
        );
    }
}

#[test]
fn static_field_and_property_imports_keep_authored_targets() {
    for (declaration, kind) in [
        (
            "class Tools { public static int count = 1 }",
            rootlight_ir::EntityKind::Field,
        ),
        (
            "class Tools { static int count = 1 }",
            rootlight_ir::EntityKind::Property,
        ),
        (
            "class Tools { static final int count = 1 }",
            rootlight_ir::EntityKind::Property,
        ),
        (
            "interface Tools { int count = 1 }",
            rootlight_ir::EntityKind::Field,
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\n{declaration}\n"),
        )
        .unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            "import static sample.Tools.count as chosen\nclass Client {}\n",
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "count")
            .unwrap();
        assert_eq!(target.kind, kind, "{declaration}");
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "groovy.static_import.reference")
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{declaration}"
        );
        assert_eq!(target.visibility, rootlight_ir::EntityVisibility::Public);
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Imports
                    && relation.subject == RelationEndpoint::Occurrence(site.id)
                    && relation.object == RelationEndpoint::Entity(target.id))
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "groovy-import-build-context-unverified"
                    && gap.provenance == site.provenance)
        );
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "getCount")
        );
    }
}

#[test]
fn static_members_reject_foreign_native_owner_bindings() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("tools.groovy"),
        "package sample\nclass Outer { static class Inner { public static int count = 1 } }",
    )
    .unwrap();
    fs::write(
        fixture.path().join("client.groovy"),
        "import static sample.Outer.count\nclass Client {}",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let mut document = snapshot.document().clone();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "groovy.static_import.reference")
        .unwrap();
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    let site_id = site.id;
    let outer = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Outer")
        .unwrap()
        .id;
    let field = document
        .entities
        .iter_mut()
        .find(|entity| entity.canonical_name == "count")
        .unwrap();
    // Keep native metadata unchanged: a forged common-IR container must not
    // move the declaration into an enclosing class's static-import namespace.
    field.container = Some(ContainerRef::Entity(outer));
    let batch = ResolutionEngine::default()
        .resolve(&document, &rootlight_cancel::Cancellation::new())
        .unwrap();
    let decision = batch
        .decisions
        .iter()
        .find(|decision| decision.occurrence == site_id)
        .unwrap();
    assert!(matches!(
        decision.outcome,
        rootlight_resolve::ResolutionOutcome::Unresolved { .. }
    ));
}

#[test]
fn static_member_imports_recompute_modifiers_and_survive_restart() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::write(
        fixture.path().join("client.groovy"),
        "import static sample.Tools.count as chosen\nclass Client {}\n",
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    for (member, resolved) in [
        ("public static int count = 1", true),
        ("private static int count = 1", false),
        ("static int count = 1", true),
        ("int count = 1", false),
        ("@Deprecated static int count = 1", false),
        ("public static int count = 2", true),
    ] {
        fs::write(
            fixture.path().join("tools.groovy"),
            format!("package sample\nclass Tools {{ {member} }}\n"),
        )
        .unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "groovy.static_import.reference")
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            resolved,
            "{member}"
        );
        let field = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "count")
            .unwrap();
        let reads = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![site.source.clone(), field.evidence.source.clone().unwrap()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(reads.data.chunks[0].bytes, b"sample.Tools.count");
        assert_eq!(
            reads.data.chunks[1].bytes,
            if member.ends_with('2') {
                b"count = 2"
            } else {
                b"count = 1"
            }
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == site.file
                    && gap.detail
                        == if resolved {
                            "groovy-import-build-context-unverified"
                        } else {
                            "unresolved-import-target"
                        })
        );
        let expected = document.clone();
        drop(snapshot);
        assert_eq!(
            service
                .index_repository(fixture.path(), &deadline())
                .unwrap()
                .generation,
            receipt.generation
        );
        let clean = service
            .index_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(clean.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
            &expected
        );
    }
}

#[test]
fn overloaded_imports_preserve_candidates_across_bounded_and_repeated_application() {
    use rootlight_ir::CoverageStatus;
    use rootlight_resolve::{ResolutionLimits, ResolverFactContext};

    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("first.groovy"),
        "package sample\nclass First { static def fold() { 1 }; static def fold(value) { value }; static def fold(left, right) { left } }",
    )
    .unwrap();
    fs::write(
        fixture.path().join("client.groovy"),
        "import static sample.First.fold as chosen\nclass Client {}",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let original = snapshot.document();
    let mut expected: Vec<_> = original
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "fold")
        .map(|entity| entity.id)
        .collect();
    expected.sort_unstable();
    assert_eq!(expected.len(), 3);
    let check = |document: &rootlight_ir::NormalizedIrDocument, limit: usize| {
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "groovy.static_import.reference")
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Candidates {
                symbols: expected.iter().copied().take(limit).collect(),
                total_count: 3,
                completeness: if limit < 3 {
                    CoverageStatus::Bounded
                } else {
                    CoverageStatus::Complete
                },
            }
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Imports
                    && relation.subject == RelationEndpoint::Occurrence(site.id))
        );
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        for symbol in expected.iter().take(limit) {
            assert!(
                proof
                    .derivation_parents
                    .contains(&rootlight_ir::FactRef::Entity(*symbol))
            );
        }
        assert!(proof.evidence_sources.contains(&site.source));
        let gap = document
            .skipped_regions
            .iter()
            .find(|gap| {
                gap.source.span().file() == site.file
                    && gap.detail == "groovy-import-candidates-build-context-unverified"
            })
            .unwrap();
        assert_eq!(
            gap.reason,
            rootlight_ir::SkippedRegionReason::MissingBuildContext
        );
        assert_eq!(gap.provenance, site.provenance);
    };
    check(original, 3);
    let context =
        ResolverFactContext::new(rootlight_ids::content_hash(b"static-import-candidates"));
    let cancel = rootlight_cancel::Cancellation::new();
    let narrow = ResolutionEngine::new(ResolutionLimits::new(1).unwrap());
    let batch = narrow.resolve(original, &cancel).unwrap();
    let decision = batch
        .decisions
        .iter()
        .find(|decision| {
            decision.explanation.rule == rootlight_resolve::ResolutionRule::GroovyStaticImport
        })
        .unwrap();
    assert_eq!(decision.explanation.candidates.len(), 1);
    assert_eq!(decision.explanation.candidates[0].symbol, expected[0]);
    assert!(decision.explanation.rejected_candidates.is_empty());
    assert_eq!(decision.explanation.rejected_total, 0);
    let first = narrow
        .apply_document(original.clone(), context, &cancel)
        .unwrap();
    check(&first, 1);
    let estimate = narrow.estimate_work(&first, &cancel).unwrap();
    assert!(estimate.required > 1);
    let insufficient = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (partial, pending) = insufficient
        .apply_document_bounded(first.clone(), context, &cancel)
        .unwrap();
    assert_eq!(pending.required, estimate.required);
    assert!(!pending.fits());
    assert_eq!(partial, first);
    assert!(matches!(
        insufficient.resolve(&first, &cancel),
        Err(rootlight_resolve::ResolutionError::WorkLimit { .. })
    ));
    assert_eq!(
        narrow
            .apply_document(first.clone(), context, &cancel)
            .unwrap(),
        first
    );
    assert_eq!(
        narrow
            .apply(first.clone(), context, &cancel)
            .unwrap()
            .document,
        first
    );
    assert_eq!(
        narrow
            .apply_document_bounded(first.clone(), context, &cancel)
            .unwrap()
            .0,
        first
    );
    let wide = ResolutionEngine::default();
    let expanded = wide.apply_document(first, context, &cancel).unwrap();
    check(&expanded, 3);
    assert_eq!(
        wide.apply_document(expanded.clone(), context, &cancel)
            .unwrap(),
        expanded
    );
    assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
    assert!(matches!(
        wide.resolve(&expanded, &cancel),
        Err(rootlight_resolve::ResolutionError::Cancelled(_))
    ));
}

#[test]
fn static_imports_link_exact_members_or_namespaces_without_hiding_unknown_targets() {
    use rootlight_ir::SkippedRegionReason;

    for (import_text, definition, expected_name) in [
        ("import static sample.First.fold", FIRST, Some("fold")),
        (
            "import static sample.First.fold as chosen",
            FIRST,
            Some("fold"),
        ),
        ("import static sample.First.*", FIRST, Some("First")),
        (
            "import static sample.First.fold",
            "package sample\nclass First { static def fold() { 1 }; static def fold(value) { value } }",
            None,
        ),
        ("import static sample.First.absent", FIRST, None),
        (
            "import static sample.First.fold",
            "package sample\nclass First { private static def fold(value) { value } }",
            None,
        ),
        (
            "import static sample.First.fold",
            "package sample\nclass First { def fold(value) { value } }",
            None,
        ),
        (
            "import static sample.First.fold",
            "package sample\nclass First { static def fold = 1 }",
            Some("fold"),
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("first.groovy"), definition).unwrap();
        fs::write(
            fixture.path().join("client.groovy"),
            format!("{import_text}\n{CLIENT}\n"),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                matches!(
                    site.syntax_kind.as_str(),
                    "groovy.static_import.reference" | "groovy.static_star_import.reference"
                )
            })
            .unwrap();
        assert_eq!(site.role, OccurrenceRole::ImportUse);
        if let Some(name) = expected_name {
            let target = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{import_text}"
            );
            assert!(document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Imports
                    && relation.subject == RelationEndpoint::Occurrence(site.id)
                    && relation.object == RelationEndpoint::Entity(target.id)
                    && relation.provenance == site.provenance
            }));
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.source.span().file() == site.file
                    && gap.detail == "groovy-import-build-context-unverified"
                    && gap.reason == SkippedRegionReason::MissingBuildContext
                    && gap.provenance == site.provenance
            }));
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span().file() == site.file
                        && gap.detail == "unresolved-import-target")
            );
        } else {
            assert!(
                !matches!(site.target, OccurrenceTarget::Resolved { .. }),
                "{definition}"
            );
            let detail = if let OccurrenceTarget::Candidates {
                symbols,
                total_count,
                completeness,
            } = &site.target
            {
                assert_eq!(symbols.len(), 2);
                assert_eq!(*total_count, 2);
                assert_eq!(*completeness, rootlight_ir::CoverageStatus::Complete);
                "groovy-import-candidates-build-context-unverified"
            } else {
                "unresolved-import-target"
            };
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span().file() == site.file && gap.detail == detail)
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Imports
                        && relation.subject == RelationEndpoint::Occurrence(site.id))
            );
        }
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![site.source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let expected = import_text
            .strip_prefix("import static ")
            .unwrap()
            .split(" as ")
            .next()
            .unwrap()
            .trim_end_matches(".*");
        assert_eq!(read.data.chunks[0].bytes, expected.as_bytes());
    }
}

#[test]
fn resolver_rejects_contradictory_native_parent_evidence() {
    use rootlight_ids::FactId;
    use rootlight_ir::{CoverageScope, FactDomain, SourceRef, SourceSpan};

    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("client.groovy"),
        format!("import static sample.Tools.fold\n{CLIENT}\n"),
    )
    .unwrap();
    fs::write(fixture.path().join("tools.groovy"), "package sample\nclass Parent {}\nclass Tools extends Parent { static def fold(value) { value } }\nclass Other {}\n").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let mut original = snapshot.document().clone();
    let call = original
        .occurrences
        .iter_mut()
        .find(|site| site.role == OccurrenceRole::CallSite)
        .unwrap();
    let call_id = call.id;
    let expected = call.target.clone();
    assert!(matches!(expected, OccurrenceTarget::Resolved { .. }));
    call.target = OccurrenceTarget::Unresolved {
        text_hash: call.syntactic_text_hash,
    };
    call.provenance = original
        .files
        .iter()
        .find(|file| file.id == call.file)
        .unwrap()
        .provenance;
    let header_index = original
        .occurrences
        .iter()
        .position(|site| site.role == OccurrenceRole::InheritanceUse)
        .unwrap();
    let header_file = original.occurrences[header_index].file;
    let owner = original.occurrences[header_index].enclosing.unwrap();
    let method = original
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "fold")
        .unwrap()
        .id;
    let other = original
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Other")
        .unwrap()
        .id;
    let engine = ResolutionEngine::default();
    let resolved = |document: &rootlight_ir::NormalizedIrDocument| {
        rootlight_ir::validate_ir_document(
            document,
            &rootlight_ir::IrLimits::default(),
            &Default::default(),
        )
        .unwrap();
        engine
            .resolve(document, &rootlight_cancel::Cancellation::new())
            .unwrap()
            .decisions
            .into_iter()
            .find(|decision| decision.occurrence == call_id)
            .map(|decision| decision.outcome)
    };
    assert!(matches!(
        resolved(&original),
        Some(rootlight_resolve::ResolutionOutcome::Resolved { .. })
    ));
    // Preserve IDs and derivation links to isolate resolver admission from
    // canonical storage identity checks; each mutation still passes IR validation.
    let mut accepted = Vec::new();
    for mutation in 0..8 {
        let mut document = original.clone();
        let header = &mut document.occurrences[header_index];
        match mutation {
            0 => header.role = OccurrenceRole::Reference,
            1 => header.syntax_kind = "groovy.identifier.reference".into(),
            2 => header.enclosing = None,
            3 => header.enclosing = Some(method),
            4 | 5 => {
                let mut extra = header.clone();
                extra.id = FactId::from_bytes([207; 20]);
                if mutation == 5 {
                    let span = extra.source.span();
                    extra.source = SourceRef::new(
                        extra.repository,
                        extra.generation,
                        SourceSpan::new(extra.file, span.end_byte() + 1, span.end_byte() + 6)
                            .unwrap(),
                        extra.source.content_hash(),
                        None,
                    );
                    extra.evidence.source = Some(extra.source.clone());
                    extra.target = OccurrenceTarget::Resolved { symbol: other };
                    extra.syntactic_text_hash = rootlight_ids::content_hash(b"Other");
                }
                document.occurrences.push(extra);
                let coverage = document
                    .coverage_records
                    .iter_mut()
                    .find(|record| {
                        record.scope == CoverageScope::File(header_file)
                            && record.domain == FactDomain::Occurrences
                    })
                    .unwrap();
                coverage.discovered += 1;
                coverage.indexed += 1;
            }
            6 => header.target = OccurrenceTarget::Resolved { symbol: method },
            7 => header.target = OccurrenceTarget::Resolved { symbol: owner },
            _ => unreachable!(),
        }
        if matches!(
            resolved(&document),
            Some(rootlight_resolve::ResolutionOutcome::Resolved { .. })
        ) {
            accepted.push(mutation);
        }
    }
    assert!(
        accepted.is_empty(),
        "contradictory parent evidence was accepted: {accepted:?}"
    );
}

#[test]
fn static_import_targets_and_scoped_gaps_survive_dependency_edits_and_restart() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::write(
        fixture.path().join("client.groovy"),
        format!("import static sample.First.fold\nimport static sample.Missing.ghost\n{CLIENT}\n"),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    for (definition, import_resolved, call_resolved) in [
        (FIRST, true, true),
        (
            "package sample\nclass First { static def fold() { 1 }; static def fold(value) { value } }",
            false,
            true,
        ),
        (
            "package sample\nclass First { static def other(value) { value } }",
            false,
            false,
        ),
        (FIRST, true, true),
    ] {
        fs::write(fixture.path().join("first.groovy"), definition).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        for (name, resolved) in [
            ("sample.First.fold", import_resolved),
            ("sample.Missing.ghost", false),
        ] {
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "groovy.static_import.reference"
                        && site.syntactic_text_hash == rootlight_ids::content_hash(name.as_bytes())
                })
                .unwrap();
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                resolved,
                "{definition}: {name}"
            );
            let gaps: Vec<_> = document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    let span = gap.source.span();
                    span.file() == site.file
                        && span.start_byte() <= site.source.span().start_byte()
                        && span.end_byte() >= site.source.span().end_byte()
                        && matches!(
                            gap.detail.as_str(),
                            "unresolved-import-target"
                                | "groovy-import-build-context-unverified"
                                | "groovy-import-candidates-build-context-unverified"
                        )
                })
                .collect();
            assert_eq!(gaps.len(), 1, "{definition}: {name}");
            assert_eq!(
                gaps[0].detail,
                if resolved {
                    "groovy-import-build-context-unverified"
                } else if matches!(site.target, OccurrenceTarget::Candidates { .. }) {
                    "groovy-import-candidates-build-context-unverified"
                } else {
                    "unresolved-import-target"
                }
            );
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| {
                        relation.predicate == RelationPredicate::Imports
                            && relation.subject == RelationEndpoint::Occurrence(site.id)
                    })
                    .count(),
                usize::from(resolved)
            );
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![site.source.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks[0].bytes, name.as_bytes());
        }
        let call = document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap();
        assert_eq!(
            matches!(call.target, OccurrenceTarget::Resolved { .. }),
            call_resolved,
            "{definition}"
        );
        let expected = document.clone();
        drop(snapshot);
        assert_eq!(
            service
                .index_repository(fixture.path(), &deadline())
                .unwrap()
                .generation,
            receipt.generation
        );
        let clean = service
            .index_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(clean.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
            &expected
        );
    }
}

#[test]
fn stars_keep_source_order() {
    check(
        &format!("import static sample.First.*\nimport static sample.Second.*\n{CLIENT}"),
        FIRST,
        SECOND,
        Some(("First", "fold")),
        "fold(value)",
    );
    check(
        &format!("import static sample.Second.*\nimport static sample.First.*\n{CLIENT}"),
        FIRST,
        SECOND,
        Some(("Second", "fold")),
        "fold(value)",
    );
    check(
        &format!(
            "import static sample.First.*\nimport static sample.Second.*\nimport static sample.First.*\n{CLIENT}"
        ),
        FIRST,
        SECOND,
        Some(("First", "fold")),
        "fold(value)",
    );
}

#[test]
fn last_explicit_import_supersedes_the_same_binding() {
    check(
        &format!("import static sample.First.fold\nimport static sample.Second.fold\n{CLIENT}"),
        FIRST,
        SECOND,
        Some(("Second", "fold")),
        "fold(value)",
    );
    check(
        &format!("import static sample.Second.fold\nimport static sample.First.fold\n{CLIENT}"),
        FIRST,
        SECOND,
        Some(("First", "fold")),
        "fold(value)",
    );
    check(
        "import static sample.First.fold as chosen\nimport static sample.Second.fold as chosen\nclass Client { def inspect(value) { chosen(value) } }",
        FIRST,
        SECOND,
        Some(("Second", "fold")),
        "chosen(value)",
    );
}

#[test]
fn explicit_import_precedes_stars_but_ineligible_arity_falls_through() {
    check(
        &format!("import static sample.First.*\nimport static sample.Second.fold\n{CLIENT}"),
        FIRST,
        SECOND,
        Some(("Second", "fold")),
        "fold(value)",
    );
    check(
        &format!("import static sample.First.fold\nimport static sample.Second.*\n{CLIENT}"),
        "package sample\nclass First { static def fold() { 1 } }",
        SECOND,
        Some(("Second", "fold")),
        "fold(value)",
    );
}

#[test]
fn static_context_ignores_current_instance_method_but_not_outer_method() {
    check(
        "import static sample.First.fold\nclass Client { def fold(value) { value }; static def inspect(value) { fold(value) } }",
        FIRST,
        SECOND,
        Some(("First", "fold")),
        "fold(value)",
    );
    check(
        "import static sample.First.fold\nclass Outer { def fold(value) { value }; static class Inner { static def inspect(value) { fold(value) } } }",
        FIRST,
        SECOND,
        None,
        "fold(value)",
    );
}

#[test]
fn enum_self_star_blocks_other_stars_but_not_explicit_imports() {
    check(
        "package sample\nimport static sample.First.*\nenum Choice { ONLY; def inspect(value) { fold(value) } }",
        FIRST,
        SECOND,
        Some(("First", "fold")),
        "fold(value)",
    );
    check(
        "package sample\nimport static sample.First.*\nimport static sample.Choice.*\nenum Choice { ONLY; def inspect(value) { fold(value) } }",
        FIRST,
        SECOND,
        None,
        "fold(value)",
    );
    check(
        "package sample\nimport static sample.First.fold\nimport static sample.Choice.*\nenum Choice { ONLY; def inspect(value) { fold(value) } }",
        FIRST,
        SECOND,
        Some(("First", "fold")),
        "fold(value)",
    );
}

#[test]
fn property_alias_accessor_precedes_star_method() {
    check(
        "import static sample.First.value as selected\nimport static sample.Second.*\nclass Client { def inspect() { getSelected() } }",
        "package sample\nclass First { static def getValue() { 1 } }",
        "package sample\nclass Second { static def getSelected() { 2 } }",
        Some(("First", "getValue")),
        "getSelected()",
    );
}

fn check(
    client: &str,
    first: &str,
    second: &str,
    expected: Option<(&str, &str)>,
    invocation: &str,
) {
    let fixture = TempDir::new().unwrap();
    for (path, source) in [
        ("client.groovy", client),
        ("first.groovy", first),
        ("second.groovy", second),
    ] {
        fs::write(fixture.path().join(path), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 3);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(
        document.diagnostics.is_empty(),
        "{client}: {:?}",
        document.diagnostics
    );
    let caller = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "inspect")
        .unwrap();
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite && site.enclosing == Some(caller.id))
        .collect();
    assert_eq!(calls.len(), 1, "{client}");
    let call = calls[0];
    if let Some((owner, name)) = expected {
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{client}"
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls
                    && relation.subject == RelationEndpoint::Entity(caller.id)
                    && relation.object == RelationEndpoint::Entity(target.id)),
            "{client}"
        );
        for (seed, family, direction, destination) in [
            (
                caller.id,
                RelationFamily::Calls,
                rootlight_query::RelationDirection::Outbound,
                target.id,
            ),
            (
                target.id,
                RelationFamily::CalledBy,
                rootlight_query::RelationDirection::Inbound,
                caller.id,
            ),
        ] {
            let response = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([seed]),
                    vec![family],
                    Some(direction),
                    0,
                    10,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert!(
                response
                    .data
                    .groups
                    .iter()
                    .flat_map(|group| &group.items)
                    .any(|edge| edge.symbol == destination
                        && edge.source_refs.contains(&call.source)),
                "{client}: {family:?}"
            );
        }
    } else {
        // These cases prove that an import cannot override compiler-owned local lookup.
        let client_file = document
            .files
            .iter()
            .find(|file| file.path == "client.groovy")
            .unwrap();
        let imported: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() != client_file.id)
            })
            .map(|entity| entity.id)
            .collect();
        assert!(
            !matches!(call.target, OccurrenceTarget::Resolved { symbol } if imported.contains(&symbol)),
            "{client}: {:?}",
            call.target
        );
    }
    let source = call.source.clone();
    assert_eq!(source.generation(), receipt.generation);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, invocation.as_bytes());
}

#[test]
fn property_lookup_preserves_the_authored_target_without_asserting_runtime_arity() {
    check(
        "import static sample.First.value as selected\nimport static sample.Second.*\nclass Client { def inspect(value) { getSelected(value) } }",
        "package sample\nclass First { static def getValue() { 1 } }",
        "package sample\nclass Second { static def getSelected(value) { value } }",
        Some(("First", "getValue")),
        "getSelected(value)",
    );
    check(
        "import static sample.First.value as selected\nimport static sample.Second.*\nclass Client { def inspect() { getselected() } }",
        "package sample\nclass First { static def getValue() { 1 } }",
        "package sample\nclass Second { static def getselected() { 2 } }",
        Some(("First", "getValue")),
        "getselected()",
    );
    check(
        "import static sample.First.pNAME as selected\nimport static sample.Second.*\nclass Client { def inspect() { getSelected() } }",
        "package sample\nclass First { static def getpNAME() { 1 } }",
        "package sample\nclass Second { static def getSelected() { 2 } }",
        Some(("First", "getpNAME")),
        "getSelected()",
    );
}

#[test]
fn import_order_edits_match_clean_rebuild_and_keep_exact_generation_sources() {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("first.groovy"), FIRST).unwrap();
    fs::write(fixture.path().join("second.groovy"), SECOND).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    for (imports, selected) in [
        (
            "import static sample.First.fold\nimport static sample.Second.fold",
            "Second",
        ),
        (
            "import static sample.Second.fold\nimport static sample.First.fold",
            "First",
        ),
        (
            "import static sample.First.*\nimport static sample.Second.*",
            "First",
        ),
        (
            "import static sample.Second.*\nimport static sample.First.*",
            "Second",
        ),
    ] {
        fs::write(
            fixture.path().join("client.groovy"),
            format!("{imports}\n{CLIENT}\n"),
        )
        .unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == selected)
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "fold"
                    && entity.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        let call = document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{imports}"
        );
        let source = call.source.clone();
        assert_eq!(source.generation(), receipt.generation);
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, b"fold(value)");
        drop(snapshot);
        assert_eq!(
            service
                .index_repository(fixture.path(), &deadline())
                .unwrap()
                .generation,
            receipt.generation
        );
        let clean = service
            .index_repository_with_options(
                fixture.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(
            clean.logical_snapshot, receipt.logical_snapshot,
            "{imports}"
        );
    }
}

#[test]
fn inherited_calls_retain_parent_import_sources_across_dependency_edits() {
    use rootlight_ir::FactRef;

    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("first.groovy"), FIRST).unwrap();
    fs::write(fixture.path().join("second.groovy"), SECOND).unwrap();
    fs::write(
        fixture.path().join("client.groovy"),
        format!("import static sample.Tools.fold\n{CLIENT}\n"),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    for form in ["alias", "direct", "qualified"] {
        for (parent, selected) in [
            ("sample.First", "First"),
            ("sample.Second", "Second"),
            ("sample.First", "First"),
        ] {
            let header_text = match form {
                "alias" => {
                    format!("import {parent} as ParentType\nclass Tools extends ParentType {{}}")
                }
                "direct" => format!("import {parent}\nclass Tools extends {selected} {{}}"),
                "qualified" => format!("class Tools extends {parent} {{}}"),
                _ => unreachable!(),
            };
            fs::write(
                fixture.path().join("tools.groovy"),
                format!("package sample\n{header_text}\n"),
            )
            .unwrap();
            let receipt = service
                .index_repository(fixture.path(), &deadline())
                .unwrap();
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let owner = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == selected)
                .unwrap();
            let target = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "fold"
                        && entity.container == Some(ContainerRef::Entity(owner.id))
                })
                .unwrap();
            let call = document
                .occurrences
                .iter()
                .find(|site| site.role == OccurrenceRole::CallSite)
                .unwrap();
            assert_eq!(
                call.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{parent}"
            );
            let tools = document
                .files
                .iter()
                .find(|file| file.path == "tools.groovy")
                .unwrap();
            let import = document.occurrences.iter().find(|site| {
                site.file == tools.id && site.syntax_kind == "groovy.type_import.reference"
            });
            assert_eq!(import.is_some(), form != "qualified");
            let header = document
                .occurrences
                .iter()
                .find(|site| site.file == tools.id && site.role == OccurrenceRole::InheritanceUse)
                .unwrap();
            let binding = document
                .extensions
                .iter()
                .find(|envelope| {
                    envelope.namespace
                        == if form == "qualified" {
                            rootlight_ir::GROOVY_TYPE_LOOKUP_NAMESPACE
                        } else {
                            rootlight_ir::GROOVY_BINDING_NAMESPACE
                        }
                        && envelope.evidence.source.as_ref() == Some(&header.source)
                })
                .unwrap();
            let provenance = document
                .provenance
                .iter()
                .find(|record| record.id == call.provenance)
                .unwrap();
            assert_eq!(
                provenance.rule.as_deref(),
                Some("groovy-static-import-v1.indexed_snapshot")
            );
            assert!(
                provenance
                    .derivation_parents
                    .contains(&FactRef::Fact(binding.id))
            );
            assert!(
                provenance
                    .derivation_parents
                    .contains(&FactRef::Entity(owner.id))
            );
            for source in [&call.source, &header.source]
                .into_iter()
                .chain(import.map(|site| &site.source))
            {
                assert!(
                    provenance.input_sources.contains(source),
                    "missing input: {parent} {source:?}"
                );
                assert!(
                    provenance.evidence_sources.contains(source),
                    "missing evidence: {parent} {source:?}"
                );
                assert_eq!(source.generation(), receipt.generation);
            }
            let relation = document
                .relations
                .iter()
                .find(|relation| {
                    relation.predicate == RelationPredicate::Calls
                        && relation.object == RelationEndpoint::Entity(target.id)
                })
                .unwrap();
            assert_eq!(relation.provenance, call.provenance);
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![import.unwrap_or(header).source.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks[0].bytes, parent.as_bytes());
            drop(snapshot);
            assert_eq!(
                service
                    .index_repository(fixture.path(), &deadline())
                    .unwrap()
                    .generation,
                receipt.generation
            );
            let clean = service
                .index_repository_with_options(
                    fixture.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(clean.logical_snapshot, receipt.logical_snapshot, "{parent}");
        }
    }
}
