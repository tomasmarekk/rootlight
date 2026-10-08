//! Public-boundary regression tests for source-backed Erlang remote calls.
//!
//! Synthetic modules exercise the native provider, structural lowering and
//! shared resolver without treating dynamic dispatch as an exact binding.

use std::{
    collections::BTreeSet,
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{
    CoverageStatus, EntityKind, EntityRecord, NormalizedIrDocument, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate, SourceRef,
};
use rootlight_query::{LocateMode, RepositoryDataTrust, SourceChunkEncoding};
use rootlight_service::{
    FirstSliceBudget, FirstSliceIndexReceipt, FirstSliceService, RelationDirection, RelationFamily,
    SourceEncoding, SourceReadOptions,
};
use tempfile::TempDir;

#[test]
fn static_remote_calls_resolve_by_module_and_arity_with_exact_source() {
    let caller_source = concat!(
        "-module(consumer).\r\n",
        "-export([run/1]).\r\n",
        "run(Value) -> storage:fetch(Value), storage : fetch ( Value , ready ).\r\n",
        "fetch(Value) -> {local, Value}.\r\n",
    );
    let target_source = concat!(
        "-module(storage).\n",
        "-export([fetch/0, fetch/1, fetch/2]).\n",
        "fetch() -> empty.\n",
        "fetch(Value) -> Value.\n",
        "fetch(Value, Tag) -> {Value, Tag}.\n",
    );
    let fixture = IndexedFixture::new(&[
        ("consumer.erl", caller_source),
        ("storage.erl", target_source),
    ]);
    let caller = function(&fixture.document, "consumer::run/1");
    let wrong_module = function(&fixture.document, "consumer::fetch/1");
    let wrong_arity = function(&fixture.document, "storage::fetch/0");
    let targets = [
        ("storage:fetch(Value)", "storage::fetch/1"),
        ("storage : fetch ( Value , ready )", "storage::fetch/2"),
    ];
    let mut expected_targets = BTreeSet::new();

    for (call_text, target_name) in targets {
        let target = function(&fixture.document, target_name);
        let call = call_at(&fixture.document, "consumer.erl", caller_source, call_text);
        assert_eq!(call.syntax_kind, "erlang.function.scoped_call");
        assert_eq!(call.enclosing, Some(caller.id));
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_ne!(target.id, wrong_module.id);
        assert_ne!(target.id, wrong_arity.id);
        assert_ne!(
            call.file,
            target.evidence.source.as_ref().unwrap().span().file()
        );
        assert!(fixture.document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Occurrence(call.id)
                && relation.object == RelationEndpoint::Entity(target.id)
                && relation.evidence.source.as_ref() == Some(&call.source)
        }));
        fixture.assert_source(&call.source, "consumer.erl", caller_source, call_text);

        let located = fixture
            .service
            .code_locate(
                fixture.receipt.generation,
                target_name.to_owned(),
                LocateMode::Exact,
                16,
                0,
                &fixture.cancellation,
            )
            .expect("qualified target is locatable");
        assert!(located.data.hits.iter().any(|hit| {
            hit.symbol == Some(target.id) && hit.path == "storage.erl" && hit.language == "erlang"
        }));
        let explained = fixture
            .service
            .symbol_explain(fixture.receipt.generation, target.id, &fixture.cancellation)
            .expect("cross-file target is explainable");
        assert_eq!(explained.data.entity, *target);
        assert!(explained.data.occurrences.contains(call));
        assert_eq!(
            explained.data.trust,
            RepositoryDataTrust::UntrustedRepositoryData
        );
        expected_targets.insert(target.id);
    }

    let relationships = fixture
        .service
        .symbol_relationships(
            fixture.receipt.generation,
            BTreeSet::from([caller.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &fixture.cancellation,
        )
        .expect("caller relationships are queryable");
    assert!(!relationships.data.truncated);
    assert_eq!(relationships.data.returned_edges, 2);
    assert_eq!(
        relationships
            .data
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .map(|item| item.symbol)
            .collect::<BTreeSet<_>>(),
        expected_targets
    );
}

#[test]
fn duplicate_qualified_targets_do_not_claim_an_exact_remote_call() {
    let caller_source = "-module(consumer).\nrun(Value) -> storage:fetch(Value).\n";
    let fixture = IndexedFixture::new(&[
        ("consumer.erl", caller_source),
        (
            "storage_a.erl",
            "-module(storage).\n-export([fetch/1]).\nfetch(Value) -> Value.\n",
        ),
        (
            "storage_b.erl",
            "-module(storage).\n-export([fetch/1]).\nfetch(Value) -> {other, Value}.\n",
        ),
    ]);
    let expected_targets = fixture
        .document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Function && entity.qualified_name == "storage::fetch/1"
        })
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_targets.len(), 2);
    let call = call_at(
        &fixture.document,
        "consumer.erl",
        caller_source,
        "storage:fetch(Value)",
    );
    assert_eq!(call.syntax_kind, "erlang.function.scoped_call");
    assert_eq!(
        call.enclosing,
        Some(function(&fixture.document, "consumer::run/1").id)
    );
    let OccurrenceTarget::Candidates {
        symbols,
        total_count,
        completeness,
    } = &call.target
    else {
        panic!("duplicate qualified definitions must remain explicit candidates");
    };
    assert_eq!(
        symbols.iter().copied().collect::<BTreeSet<_>>(),
        expected_targets
    );
    assert_eq!(*total_count, 2);
    assert_eq!(*completeness, CoverageStatus::Complete);
    assert!(!fixture.document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.subject == RelationEndpoint::Occurrence(call.id)
    }));
    fixture.assert_source(
        &call.source,
        "consumer.erl",
        caller_source,
        "storage:fetch(Value)",
    );
}

#[test]
fn absent_module_does_not_bind_same_named_local_or_foreign_functions() {
    let caller_source = concat!(
        "-module(consumer).\n",
        "run(Value) -> missing:fetch(Value), storage:fetch(Value).\n",
        "fetch(Value) -> {local, Value}.\n",
    );
    let fixture = IndexedFixture::new(&[
        ("consumer.erl", caller_source),
        (
            "storage.erl",
            "-module(storage).\n-export([fetch/1]).\nfetch(Value) -> Value.\n",
        ),
    ]);
    let caller = function(&fixture.document, "consumer::run/1");
    let local_decoy = function(&fixture.document, "consumer::fetch/1");
    let foreign_decoy = function(&fixture.document, "storage::fetch/1");
    let missing_call = call_at(
        &fixture.document,
        "consumer.erl",
        caller_source,
        "missing:fetch(Value)",
    );
    assert_eq!(missing_call.syntax_kind, "erlang.function.scoped_call");
    assert_eq!(missing_call.enclosing, Some(caller.id));
    assert_eq!(
        missing_call.syntactic_text_hash,
        content_hash(b"missing::fetch/1")
    );
    assert!(matches!(
        missing_call.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!fixture.document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.subject == RelationEndpoint::Occurrence(missing_call.id)
    }));
    fixture.assert_source(
        &missing_call.source,
        "consumer.erl",
        caller_source,
        "missing:fetch(Value)",
    );

    let present_call = call_at(
        &fixture.document,
        "consumer.erl",
        caller_source,
        "storage:fetch(Value)",
    );
    assert_eq!(
        present_call.target,
        OccurrenceTarget::Resolved {
            symbol: foreign_decoy.id
        }
    );
    assert_ne!(foreign_decoy.id, local_decoy.id);
}

#[test]
fn block_body_commas_do_not_prove_a_wrong_remote_overload() {
    let block_call = "storage:fetch(fun() -> Value, Value end)";
    let caller_source = concat!(
        "-module(consumer).\n",
        "run(Value) -> storage:fetch(fun() -> Value, Value end), storage:fetch(Value).\n",
    );
    let target_source = concat!(
        "-module(storage).\n",
        "-export([fetch/1, fetch/2]).\n",
        "fetch(Value) -> Value.\n",
        "fetch(Left, Right) -> {Left, Right}.\n",
    );
    let fixture = IndexedFixture::new(&[
        ("consumer.erl", caller_source),
        ("storage.erl", target_source),
    ]);
    let caller = function(&fixture.document, "consumer::run/1");
    let correct_target = function(&fixture.document, "storage::fetch/1");
    let wrong_target = function(&fixture.document, "storage::fetch/2");
    assert_ne!(correct_target.id, wrong_target.id);
    let ordinary_call = call_at(
        &fixture.document,
        "consumer.erl",
        caller_source,
        "storage:fetch(Value)",
    );
    assert_eq!(ordinary_call.enclosing, Some(caller.id));
    assert_eq!(ordinary_call.syntax_kind, "erlang.function.scoped_call");
    assert_eq!(
        ordinary_call.target,
        OccurrenceTarget::Resolved {
            symbol: correct_target.id
        }
    );
    fixture.assert_source(
        &ordinary_call.source,
        "consumer.erl",
        caller_source,
        "storage:fetch(Value)",
    );

    let start = u64::try_from(caller_source.find(block_call).unwrap()).unwrap();
    let end = start + u64::try_from(block_call.len()).unwrap();
    assert!(!fixture.document.occurrences.iter().any(|occurrence| {
        occurrence.file == ordinary_call.file
            && occurrence.role == OccurrenceRole::CallSite
            && occurrence.syntax_kind == "erlang.function.scoped_call"
            && occurrence.source.span().start_byte() >= start
            && occurrence.source.span().end_byte() <= end
            && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
    }));
    assert!(!fixture.document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.evidence.source.as_ref().is_some_and(|source| {
                source.span().file() == ordinary_call.file
                    && source.span().start_byte() >= start
                    && source.span().end_byte() <= end
            })
    }));
    assert!(fixture.document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == ordinary_call.file
            && gap.source.repository() == fixture.receipt.repository
            && gap.source.generation() == fixture.receipt.generation
            && gap.source.content_hash() == content_hash(caller_source.as_bytes())
            && gap.source.span().start_byte() <= start
            && gap.source.span().end_byte() >= end
            && gap.detail == "erlang-macro-and-expression-semantics-partial"
    }));
}

#[test]
fn dynamic_and_macro_dispatch_do_not_invent_static_remote_targets() {
    let caller_source = concat!(
        "-module(consumer).\n",
        "-define(ARGUMENTS, one, two).\n",
        "run(Module, Fun) -> storage:fetch(ok), Module:fetch(ok), storage:Fun(ok),\n",
        "    ?REMOTE:fetch(ok), storage:?FETCH(ok), ?CALL(ok), storage:fetch(?ARGUMENTS).\n",
        "fetch(Value) -> {local, Value}.\n",
    );
    let fixture = IndexedFixture::new(&[
        ("consumer.erl", caller_source),
        (
            "storage.erl",
            "-module(storage).\n-export([fetch/1]).\nfetch(Value) -> Value.\n",
        ),
    ]);
    let static_call = call_at(
        &fixture.document,
        "consumer.erl",
        caller_source,
        "storage:fetch(ok)",
    );
    assert_eq!(
        static_call.target,
        OccurrenceTarget::Resolved {
            symbol: function(&fixture.document, "storage::fetch/1").id
        }
    );

    for dispatch in [
        "Module:fetch(ok)",
        "storage:Fun(ok)",
        "?REMOTE:fetch(ok)",
        "storage:?FETCH(ok)",
        "?CALL(ok)",
        "storage:fetch(?ARGUMENTS)",
    ] {
        let start = u64::try_from(caller_source.find(dispatch).unwrap()).unwrap();
        let end = start + u64::try_from(dispatch.len()).unwrap();
        for occurrence in fixture.document.occurrences.iter().filter(|occurrence| {
            occurrence.file == static_call.file
                && occurrence.role == OccurrenceRole::CallSite
                && occurrence.source.span().start_byte() < end
                && occurrence.source.span().end_byte() > start
        }) {
            assert_ne!(
                occurrence.syntax_kind, "erlang.function.scoped_call",
                "{dispatch}"
            );
            assert!(
                !matches!(occurrence.target, OccurrenceTarget::Resolved { .. }),
                "{dispatch}"
            );
        }
    }
    assert!(fixture.document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == static_call.file
            && gap.source.generation() == fixture.receipt.generation
            && gap.detail == "erlang-macro-and-expression-semantics-partial"
    }));
}

struct IndexedFixture {
    _directory: TempDir,
    service: FirstSliceService,
    receipt: FirstSliceIndexReceipt,
    cancellation: Cancellation,
    document: NormalizedIrDocument,
}

impl IndexedFixture {
    fn new(sources: &[(&str, &str)]) -> Self {
        let directory = TempDir::new().expect("fixture directory exists");
        for (path, source) in sources {
            fs::write(directory.path().join(path), source).expect("synthetic Erlang source writes");
        }
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .expect("test deadline is representable"),
        );
        let mut service = FirstSliceService::new(2).expect("service initializes");
        let receipt = service
            .index_repository(directory.path(), &cancellation)
            .expect("Erlang fixture indexes");
        assert_eq!(receipt.indexed_files, u64::try_from(sources.len()).unwrap());
        let document = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                |snapshot| snapshot.document().clone(),
            )
            .expect("published generation is inspectable");
        Self {
            _directory: directory,
            service,
            receipt,
            cancellation,
            document,
        }
    }

    fn assert_source(&self, reference: &SourceRef, path: &str, source: &str, selected: &str) {
        assert_eq!(reference.repository(), self.receipt.repository);
        assert_eq!(reference.generation(), self.receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let result = self
            .service
            .source_read_with_options_and_budget(
                self.receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &self.cancellation,
            )
            .expect("exact call bytes are readable");
        assert_eq!(result.data.chunks.len(), 1);
        let chunk = &result.data.chunks[0];
        assert_eq!(&chunk.reference, reference);
        assert_eq!(chunk.start_byte, reference.span().start_byte());
        assert_eq!(chunk.end_byte, reference.span().end_byte());
        assert_eq!(chunk.bytes, selected.as_bytes());
        assert_eq!(chunk.path, path);
        assert_eq!(chunk.language, "erlang");
        assert_eq!(chunk.encoding, SourceChunkEncoding::Bytes);
        assert_eq!(chunk.trust, RepositoryDataTrust::UntrustedRepositoryData);
        assert!(chunk.extracted_from.is_none());
    }
}

fn function<'a>(document: &'a NormalizedIrDocument, qualified_name: &str) -> &'a EntityRecord {
    let matches = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Function && entity.qualified_name == qualified_name
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{qualified_name}");
    matches[0]
}

fn call_at<'a>(
    document: &'a NormalizedIrDocument,
    path: &str,
    source: &str,
    text: &str,
) -> &'a OccurrenceRecord {
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .expect("caller file is indexed");
    let start = u64::try_from(source.find(text).expect("call text is source-authored")).unwrap();
    let end = start + u64::try_from(text.len()).unwrap();
    let calls = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.file == file.id
                && occurrence.role == OccurrenceRole::CallSite
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == end
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1, "{path}: {text}");
    calls[0]
}
