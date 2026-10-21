//! Default Structural service preserves written PHP method dependencies and gaps.
//! These relations select source declarations, not unique runtime implementations.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint, RelationPredicate, derive_relation_record_id};
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../rootlight-adapter-treesitter/tests/fixtures/php_constructors/HtmlString.php"
);

fn indexed(bytes: &[u8]) -> (TempDir, FirstSliceService, FirstSliceIndexReceipt, FileId) {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("written.php"), bytes).unwrap();
    fs::write(
        directory.path().join("lib.rs"),
        b"pub fn retained_sibling() {}\n",
    )
    .unwrap();
    let mut service = FirstSliceService::new(3).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let file = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap()
        .document()
        .files
        .iter()
        .find(|file| file.path == "written.php")
        .unwrap()
        .id;
    (directory, service, receipt, file)
}

fn method(
    document: &rootlight_ir::NormalizedIrDocument,
    file: FileId,
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == file
                && site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one written method {name}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition resolves")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Method);
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn written_call(
    document: &rootlight_ir::NormalizedIrDocument,
    receipt: &FirstSliceIndexReceipt,
    caller: SymbolId,
    target: SymbolId,
    span: SourceSpan,
    raw: &[u8],
) -> SourceRef {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.source.span() == span && site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(sites.len(), 1, "one current physical written call");
    let site = sites[0];
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
    assert_eq!(site.enclosing, Some(caller));
    assert_eq!(site.source.repository(), receipt.repository);
    assert_eq!(site.source.generation(), receipt.generation);
    assert_eq!(site.source.content_hash(), content_hash(raw));
    let record = document
        .files
        .iter()
        .find(|record| record.id == span.file())
        .unwrap();
    assert_eq!(site.provenance, record.provenance);
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(caller)
                && edge.object == RelationEndpoint::Entity(target)
                && edge.evidence.source.as_ref() == Some(&site.source)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].provenance, site.provenance);
    assert_eq!(edges[0].id, derive_relation_record_id(edges[0]).unwrap());
    let mut parents = [
        FactRef::Entity(caller),
        FactRef::Fact(site.id),
        FactRef::Entity(target),
    ];
    parents.sort_unstable();
    assert_eq!(edges[0].evidence.derivation, parents);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == IrFactDomain::Relations
                && gap.source == site.source
                && gap.detail
                    == "php-method-runtime-implementation-and-external-semantics-unavailable"
                && gap.provenance == site.provenance)
    );
    site.source.clone()
}

#[test]
fn full_original_default_service_preserves_both_written_calls_and_encoded_source_on_noop() {
    assert_eq!(ORIGINAL.len(), 1145);
    assert!(ORIGINAL.is_ascii());
    let text = std::str::from_utf8(ORIGINAL).unwrap();
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let (raw, prefix, unit): (Vec<u8>, u64, u64) = match encoding {
            "utf-8" => (ORIGINAL.to_vec(), 0, 1),
            "utf-8-bom" => ([b"\xef\xbb\xbf".as_slice(), ORIGINAL].concat(), 3, 1),
            _ => (
                std::iter::once(0xfeff_u16)
                    .chain(text.encode_utf16())
                    .flat_map(|value| {
                        if encoding == "utf-16le" {
                            value.to_le_bytes()
                        } else {
                            value.to_be_bytes()
                        }
                    })
                    .collect(),
                2,
                2,
            ),
        };
        let (directory, mut service, receipt, file_id) = indexed(&raw);
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.id == file_id)
            .unwrap();
        assert_eq!(file.language, "php");
        let source = file.evidence.source.as_ref().unwrap();
        assert_eq!(
            source.span(),
            SourceSpan::new(file_id, 0, u64::try_from(raw.len()).unwrap()).unwrap()
        );
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.content_hash(), content_hash(&raw));
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, *source);
        assert_eq!(read.data.chunks[0].bytes, raw);
        let physical = |start, end| (prefix + unit * start, prefix + unit * end);
        let method_at = |name, start, end| {
            let (start, end) = physical(start, end);
            method(document, file_id, name, start, end)
        };
        let calls = [
            (
                method_at("isNotEmpty", 911, 921),
                method_at("isEmpty", 716, 723),
                956,
                963,
            ),
            (
                method_at("__toString", 1075, 1085),
                method_at("toHtml", 541, 547),
                1118,
                1124,
            ),
        ];
        for (caller, target, start, end) in calls {
            let (start, end) = physical(start, end);
            let site = written_call(
                document,
                &receipt,
                caller,
                target,
                SourceSpan::new(file_id, start, end).unwrap(),
                &raw,
            );
            let exact = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![site.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(exact.data.chunks[0].reference, site);
            assert_eq!(
                exact.data.chunks[0].bytes,
                raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
            );
        }
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls
                    && edge
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|site| site.span().file() == file_id))
                .count(),
            2
        );
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::DispatchCandidate
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|site| site.span().file() == file_id)
        }));
        let coverage = service
            .source_file_coverage_until(receipt.generation, file_id, &deadline())
            .unwrap();
        assert_eq!(coverage.status, CoverageStatus::Bounded);
        assert_eq!(coverage.tier, rootlight_ir::AnalysisTier::TierD);
        assert_eq!(coverage.reason, None);
        assert!(
            document
                .coverage_records
                .iter()
                .any(|coverage| coverage.scope == CoverageScope::File(file_id)
                    && coverage.domain == IrFactDomain::Relations
                    && coverage.status == CoverageStatus::Bounded
                    && coverage.skipped > 0)
        );
        for (query, count) in [("__toString", 1), ("__toString_absent_source_control__", 0)] {
            let page = service
                .find_literal(
                    receipt.generation,
                    file_id,
                    query,
                    None,
                    SourceFindBudget::new(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(page.stop, SourceFindStop::EndOfSource);
            assert!(page.continuation.is_none());
            assert_eq!(page.matches.len(), count);
            if count == 1 {
                let (start, end) = physical(1075, 1085);
                assert_eq!(page.matches[0].span().start_byte(), start);
                assert_eq!(page.matches[0].span().end_byte(), end);
            }
        }
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "retained_sibling")
        );
        let ids: BTreeSet<_> = document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .map(|edge| edge.id)
            .collect();
        let no_op = service
            .index_repository(directory.path(), &deadline())
            .unwrap();
        assert_eq!(no_op, receipt);
        let repeated = service
            .loaded_generation_snapshot(no_op.generation)
            .unwrap();
        assert_eq!(
            repeated
                .document()
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .map(|edge| edge.id)
                .collect::<BTreeSet<_>>(),
            ids
        );
    }
}

#[test]
fn callable_acquisition_stays_unknown_but_ordinary_unpacking_keeps_written_call() {
    for arguments in ["...", " /* acquisition */ ... /* closure */ ", "...$args"] {
        let text = format!(
            "<?php class Owner {{ function target(...$values) {{}} function caller($args) {{ return $this->target({arguments}); }} }}"
        );
        let (_directory, service, receipt, file) = indexed(text.as_bytes());
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let start = u64::try_from(text.find("$this->target").unwrap() + "$this->".len()).unwrap();
        if arguments == "...$args" {
            let target_start =
                u64::try_from(text.find("function target").unwrap() + "function ".len()).unwrap();
            let caller_start =
                u64::try_from(text.find("function caller").unwrap() + "function ".len()).unwrap();
            let target = method(document, file, "target", target_start, target_start + 6);
            let caller = method(document, file, "caller", caller_start, caller_start + 6);
            written_call(
                document,
                &receipt,
                caller,
                target,
                SourceSpan::new(file, start, start + 6).unwrap(),
                text.as_bytes(),
            );
        } else {
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file)
            }));
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| site.source.span().file() == file
                        && site.source.span().start_byte() == start
                        && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            );
            assert!(document.skipped_regions.iter().any(|gap| gap.domain
                == IrFactDomain::Relations
                && gap.source.span().file() == file
                && gap.source.span().start_byte() == start
                && gap.source.span().end_byte() == start + 6
                && gap.source.content_hash() == content_hash(text.as_bytes())
                && gap.repository == receipt.repository
                && gap.generation == receipt.generation
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                && gap.detail
                    == "php-method-runtime-implementation-and-external-semantics-unavailable"));
        }
    }
}

#[test]
fn actual_php_artifacts_rebuild_for_parser_299_and_reuse_for_resolver_128() {
    let (_directory, service, receipt, php) = indexed(ORIGINAL);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let caller = method(document, php, "__toString", 1075, 1085);
    let target = method(document, php, "toHtml", 541, 547);
    written_call(
        document,
        &receipt,
        caller,
        target,
        SourceSpan::new(php, 1118, 1124).unwrap(),
        ORIGINAL,
    );
    let cancellation = Cancellation::new();
    let limits = PlanningLimits::default();
    let current = service.incremental_inputs.get(&receipt.generation).unwrap();
    let retained = service
        .structural_artifacts
        .generation(receipt.generation)
        .unwrap();
    let files: BTreeSet<_> = retained.iter().map(|(file, _)| *file).collect();
    assert_eq!(files.len(), 2);
    assert!(files.contains(&php));
    let provider = derive_fact("incremental-provider", INCREMENTAL_PROVIDER_SEED).id();
    let adapter_key = InputKey::AdapterVersion(provider);
    let grammar_key = InputKey::GrammarVersion(
        derive_fact("rootlight.first-slice.grammar-input", GRAMMAR_REVISION_SEED).id(),
    );
    let grammar = first_slice_grammar_revision_hash().unwrap();
    assert_eq!(current.value(grammar_key), Some(grammar));
    let keys = incremental_input_keys(current, current, &cancellation).unwrap();
    let passes = first_slice_passes().unwrap();
    let (nodes, edges) =
        first_slice_dependency_parts(receipt.repository, &files, &keys, &passes, &cancellation)
            .unwrap();
    let graph_limits = GraphLimits::new(5, nodes.len(), edges.len()).unwrap();
    let registry =
        DependencyRegistry::new(passes.declarations().unwrap(), graph_limits, &cancellation)
            .unwrap();
    verify_first_slice_observations(&registry, &passes).unwrap();
    let graph = DependencyGraph::new(nodes, edges, &registry, graph_limits, &cancellation).unwrap();
    let summary = |inputs: InputSnapshot| {
        let artifacts =
            parent_artifact_summaries(&inputs, Some(retained), limits, &cancellation).unwrap();
        assert_eq!(artifacts.len(), 2);
        GenerationSummary::new(inputs, artifacts, limits, &cancellation).unwrap()
    };
    let unchanged = plan_invalidation(
        &summary(current.clone()),
        current,
        &graph,
        limits,
        &cancellation,
    )
    .unwrap();
    assert!(unchanged.changes().changes().is_empty());
    assert_eq!(unchanged.invalidated_nodes().count(), 0);
    assert!(
        unchanged
            .artifact_decisions()
            .iter()
            .all(|choice| choice.kind() == ArtifactDecisionKind::Reuse)
    );
    let previous_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/299",
        grammar.as_bytes(),
    ])
    .unwrap();
    for (changed_key, old, decision) in [
        (adapter_key, previous_parser, ArtifactDecisionKind::Rebuild),
        (
            InputKey::ResolverVersion,
            content_hash(b"rootlight.first-slice.resolve/128"),
            ArtifactDecisionKind::Reuse,
        ),
    ] {
        let previous = InputSnapshot::new(
            current.iter().map(|input| {
                if input.key() == changed_key {
                    InputFingerprint::new(changed_key, old)
                } else {
                    input
                }
            }),
            limits,
            &cancellation,
        )
        .unwrap();
        assert_eq!(previous.value(grammar_key), current.value(grammar_key));
        let plan =
            plan_invalidation(&summary(previous), current, &graph, limits, &cancellation).unwrap();
        assert_eq!(
            plan.changes()
                .changes()
                .iter()
                .map(|change| change.key())
                .collect::<Vec<_>>(),
            vec![changed_key]
        );
        assert!(plan.fallback().is_none());
        assert_eq!(plan.artifact_decisions().len(), 2);
        for file in &files {
            let choice = plan
                .artifact_decisions()
                .iter()
                .find(|choice| choice.artifact() == parser_artifact_id(*file))
                .unwrap();
            assert_eq!(choice.kind(), decision);
            if changed_key == adapter_key {
                assert!(plan.invalidated_nodes().any(
                    |node| node == FactNode::new(file_analysis_unit(*file), FactDomain::Syntax)
                ));
            }
        }
        if changed_key == InputKey::ResolverVersion {
            assert_eq!(
                plan.invalidated_nodes()
                    .map(|node| node.domain())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    FactDomain::Resolution,
                    FactDomain::DerivedGraph,
                    FactDomain::Search
                ])
            );
        }
    }
}
