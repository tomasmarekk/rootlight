//! Original Apex source distinguishes static private calls from virtual dispatch.
//! Exact private targets must survive merging while virtual receiver and
//! cross-file override qualification remain separate bounded requirements.

use super::*;
use rootlight_ir::RelationEndpoint;

fn original_apex_project() -> (TempDir, FirstSliceService, FirstSliceIndexReceipt) {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("TriggerHandler.cls"),
        include_bytes!(
            "../../../rootlight-adapter-treesitter/tests/fixtures/apex_calls/TriggerHandler.cls"
        ),
    )
    .unwrap();
    fs::write(fixture.path().join("TriggerHandler_Test.cls"), include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/apex_calls/TriggerHandler_Test.cls")).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    (fixture, service, receipt)
}

fn apex_definition_at(
    document: &rootlight_ir::NormalizedIrDocument,
    path: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    let rows: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.file == file.id
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(rows.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = rows[0].target else {
        panic!("source-authored definition is missing")
    };
    symbol
}

#[test]
fn original_private_run_calls_have_exact_targets_and_readable_source() {
    let (_fixture, service, receipt) = original_apex_project();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let caller = apex_definition_at(document, "TriggerHandler.cls", 856, 859);
    let file = document
        .files
        .iter()
        .find(|file| file.path == "TriggerHandler.cls")
        .unwrap();
    for (name, definition, leaf, invocation) in [
        ("validateRun", (4836, 4847), (875, 886), (875, 888)),
        ("addToLoopCount", (4297, 4311), (921, 935), (921, 937)),
    ] {
        let target = apex_definition_at(document, "TriggerHandler.cls", definition.0, definition.1);
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::CallSite
                    && site.enclosing == Some(caller)
                    && [leaf, invocation].into_iter().any(|(a, b)| {
                        site.source.span().start_byte() == a && site.source.span().end_byte() == b
                    })
            })
            .collect();
        assert_eq!(sites.len(), 1, "required original private call {name}");
        let site = sites[0];
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Entity(caller)
                    && edge.object == RelationEndpoint::Entity(target)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
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
        let physical = include_bytes!(
            "../../../rootlight-adapter-treesitter/tests/fixtures/apex_calls/TriggerHandler.cls"
        );
        let bytes = &physical[usize::try_from(site.source.span().start_byte()).unwrap()
            ..usize::try_from(site.source.span().end_byte()).unwrap()];
        assert_eq!(read.data.chunks[0].bytes, bytes);
    }
}

#[test]
fn virtual_static_declaration_and_bounded_dispatch_survive_structural_and_deep_resolution() {
    let source = b"public virtual class Handler { public void run() { this.beforeInsert(); } protected virtual void beforeInsert() {} }";
    for mode in [FirstSliceIndexMode::Structural, FirstSliceIndexMode::Deep] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("Handler.cls"), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository_with_mode(fixture.path(), mode, &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let caller = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "run")
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Method && entity.canonical_name == "beforeInsert"
            })
            .unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.enclosing == Some(caller.id)
            })
            .collect();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert_eq!(
            site.target,
            OccurrenceTarget::Candidates {
                symbols: vec![target.id],
                total_count: 1,
                completeness: rootlight_ir::CoverageStatus::Bounded
            }
        );
        assert!(document.relations.iter().any(|edge| edge.predicate
            == RelationPredicate::DispatchCandidate
            && edge.subject == RelationEndpoint::Occurrence(site.id)
            && edge.object == RelationEndpoint::Entity(target.id)));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Entity(caller.id)
                        && edge.object == RelationEndpoint::Entity(target.id)
                        && edge.evidence.source.as_ref() == Some(&site.source)
                })
                .count(),
            1
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "apex-runtime-receiver-and-project-overrides-unavailable")
        );
        assert_eq!(site.source.content_hash(), content_hash(source));
    }
}

#[test]
fn original_virtual_run_call_keeps_static_declaration_and_bounded_runtime_dispatch() {
    let (_fixture, service, receipt) = original_apex_project();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    // These are independently authored definition-name coordinates, not query output.
    let _class = apex_definition_at(document, "TriggerHandler.cls", 21, 35);
    let _constructor = apex_definition_at(document, "TriggerHandler.cls", 608, 622);
    let caller = apex_definition_at(document, "TriggerHandler.cls", 856, 859);
    let base = apex_definition_at(document, "TriggerHandler.cls", 5405, 5417);
    let known_override = apex_definition_at(document, "TriggerHandler_Test.cls", 7687, 7699);
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/apex_calls/TriggerHandler.cls"
    );
    let file = document
        .files
        .iter()
        .find(|file| file.path == "TriggerHandler.cls")
        .unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.file == file.id
                && site.enclosing == Some(caller)
                && site.source.span().start_byte() <= 1060
                && 1072 <= site.source.span().end_byte()
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.syntax_kind, "apex.virtual_instance_call.call");
    assert_eq!(site.source.content_hash(), content_hash(source));
    assert_eq!(site.source.repository(), document.repository);
    assert_eq!(site.source.generation(), document.generation);
    let OccurrenceTarget::Candidates {
        symbols,
        total_count,
        completeness,
    } = &site.target
    else {
        panic!("the runtime receiver is not a uniquely resolved implementation")
    };
    assert!(symbols.contains(&base));
    assert!(*total_count >= u64::try_from(symbols.len()).unwrap());
    assert_eq!(*completeness, rootlight_ir::CoverageStatus::Bounded);
    let static_calls: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(caller)
                && edge.object == RelationEndpoint::Entity(base)
                && edge.evidence.source.as_ref() == Some(&site.source)
        })
        .collect();
    assert_eq!(
        static_calls.len(),
        1,
        "the original static declared-call obligation remains required"
    );
    let mut proof = [
        rootlight_ir::FactRef::Entity(caller),
        rootlight_ir::FactRef::Fact(site.id),
        rootlight_ir::FactRef::Entity(base),
    ];
    proof.sort_unstable();
    assert_eq!(static_calls[0].evidence.derivation, proof);
    assert_eq!(
        static_calls[0].id,
        rootlight_ir::derive_relation_record_id(static_calls[0]).unwrap()
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(
                |edge| edge.predicate == RelationPredicate::DispatchCandidate
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(base)
                    && edge.evidence.source.as_ref() == Some(&site.source)
            )
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && [
                RelationEndpoint::Entity(caller),
                RelationEndpoint::Occurrence(site.id),
            ]
            .contains(&edge.subject)
            && edge.object == RelationEndpoint::Entity(known_override)
    }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "apex-runtime-receiver-and-project-overrides-unavailable")
    );
    // Joining known runtime overrides is still separately required; this checks the static declaration.
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
    assert_eq!(
        read.data.chunks[0].bytes,
        source[usize::try_from(site.source.span().start_byte()).unwrap()
            ..usize::try_from(site.source.span().end_byte()).unwrap()]
    );
}
