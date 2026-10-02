//! Durable Haskell source and structural identity through the native grammar route.
//! Exact archived bytes and declaration ownership do not prove import, lexical,
//! type-class or runtime-call semantics; those gaps remain visible after restart.

use super::*;
use rootlight_ir::{EntityKind, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget};
use std::fs;

const SAMPLE: &str = "module Sample (adjust, Label(..)) where\nimport qualified Peer as Other\n-- λ😀 source marker\nadjust :: Int -> Int\nadjust value = helper (value + 1)\nhelper value = Other.helper value\nclass Label a where\n  label :: a -> String\ninstance Label Int where\n  label value = Other.label value\n";
const PEER: &str = "module Peer where\nhelper :: Int -> Int\nhelper value = value\nlabel :: Int -> String\nlabel value = \"value\"\n";

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn index(
    service: &mut FirstSliceService,
    repository: &Path,
    options: FirstSliceIndexOptions,
) -> FirstSliceIndexCommit {
    let cancellation = deadline();
    let prepared = service
        .prepare_repository_with_options(repository, options, &cancellation)
        .unwrap();
    service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .unwrap()
}

fn exact_source(service: &FirstSliceService, reference: &SourceRef, expected: &[u8]) {
    let read = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.generation, reference.generation());
    assert_eq!(read.data.chunks.len(), 1);
    let chunk = &read.data.chunks[0];
    assert_eq!(&chunk.reference, reference);
    assert_eq!(chunk.content_hash, reference.content_hash());
    assert_eq!(chunk.start_byte, reference.span().start_byte());
    assert_eq!(chunk.end_byte, reference.span().end_byte());
    assert_eq!(chunk.language, "haskell");
    assert_eq!(chunk.bytes, expected);
}

fn function<'document>(
    document: &'document NormalizedIrDocument,
    file: FileId,
    name: &str,
) -> &'document rootlight_ir::EntityRecord {
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.language == "haskell"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == name
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file)
        })
        .collect();
    assert_eq!(entities.len(), 1, "one authored function {name}");
    entities[0]
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    sample: &str,
    peer: &str,
) -> (SymbolId, BTreeSet<SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == "haskell")
    );
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "unsupported-language" | "unsupported-encoding"
        )
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
        )
    }));

    let registry = GrammarRegistry::audited().unwrap();
    let descriptor = registry
        .descriptors()
        .iter()
        .find(|descriptor| descriptor.family() == GrammarFamily::Haskell)
        .unwrap();
    let frontend = format!("tree-sitter-haskell-{}", descriptor.grammar_version());
    let native_digest = hash_static_components(&[
        ANALYZER_BINARY_SEED,
        descriptor.language().as_str().as_bytes(),
        descriptor.grammar_version().as_bytes(),
        descriptor.grammar_source_sha256().as_bytes(),
        descriptor.parser_sha256().as_bytes(),
        descriptor.scanner_sha256().unwrap().as_bytes(),
    ])
    .unwrap();
    assert_eq!(grammar_binary_digest(descriptor).unwrap(), native_digest);
    for (path, source) in [("Sample.hs", sample), ("Peer.hs", peer)] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "haskell");
        assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
        let full = file.evidence.source.as_ref().unwrap();
        assert_eq!(full.repository(), receipt.repository);
        assert_eq!(full.generation(), receipt.generation);
        assert_eq!(full.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(full.span().start_byte(), 0);
        assert_eq!(full.span().end_byte(), u64::try_from(source.len()).unwrap());
        exact_source(service, full, source.as_bytes());
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, "haskell");
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        assert_eq!(coverage.reason, None);
        assert_ne!(coverage.status, CoverageStatus::Complete);
        let parser = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(parser.producer_kind, ProducerKind::Parser);
        assert_eq!(parser.language, "haskell");
        assert_eq!(parser.frontend_version.as_deref(), Some(frontend.as_str()));
        assert_eq!(parser.binary_digest, native_digest);

        for site in document
            .occurrences
            .iter()
            .filter(|site| site.file == file.id)
        {
            assert_eq!(site.source.generation(), receipt.generation);
            assert_eq!(site.source.content_hash(), full.content_hash());
            let span = site.source.span();
            let written = source
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap();
            exact_source(service, &site.source, written);
        }
    }

    let sample_file = document
        .files
        .iter()
        .find(|file| file.path == "Sample.hs")
        .unwrap();
    let adjust = function(document, sample_file.id, "adjust");
    let helper = function(document, sample_file.id, "helper");
    for (entity, written) in [(adjust, "adjust value"), (helper, "helper value")] {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == sample_file.id
                    && site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let source = &definitions[0].source;
        let start = u64::try_from(sample.find(written).unwrap()).unwrap();
        assert_eq!(source.span().start_byte(), start);
        assert_eq!(
            source.span().end_byte(),
            start + u64::try_from(entity.canonical_name.len()).unwrap()
        );
        exact_source(service, source, entity.canonical_name.as_bytes());
        let located = service
            .code_locate(
                receipt.generation,
                entity.canonical_name.clone(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.symbol == Some(entity.id) && hit.file == sample_file.id)
        );
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Trait && entity.canonical_name == "Label"
            })
            .count(),
        1
    );

    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::Reference
                    | OccurrenceRole::CallSite
                    | OccurrenceRole::TypeUse
                    | OccurrenceRole::ImportUse,
            )
        })
        .collect();
    assert!(!uses.is_empty());
    assert!(
        uses.iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
    assert!(
        uses.iter()
            .any(|site| site.syntax_kind == "haskell.instance_class_name.reference")
    );
    assert!(
        uses.iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    for written in [
        sample
            .split("adjust value = ")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        "Other.helper value",
        "Other.label value",
    ] {
        let start = u64::try_from(sample.find(written).unwrap()).unwrap();
        let calls: Vec<_> = uses
            .iter()
            .filter(|site| {
                site.file == sample_file.id
                    && site.role == OccurrenceRole::CallSite
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(written.len()).unwrap()
            })
            .collect();
        assert_eq!(calls.len(), 1, "written application {written}");
        exact_source(service, &calls[0].source, written.as_bytes());
    }
    assert!(document.relations.iter().all(|relation| matches!(
        relation.predicate,
        RelationPredicate::Contains | RelationPredicate::Declares | RelationPredicate::DefinesAt,
    )));
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.domain == IrFactDomain::Relations
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
        })
        .collect();
    assert!(
        gaps.iter()
            .any(|gap| gap.detail == "haskell-binding-target-unavailable")
    );
    let class_start =
        u64::try_from(sample.find("instance Label").unwrap() + "instance ".len()).unwrap();
    let class_gap = gaps
        .iter()
        .find(|gap| {
            gap.detail == "haskell-binding-target-unavailable"
                && gap.source.span().file() == sample_file.id
                && gap.source.span().start_byte() == class_start
                && gap.source.span().end_byte()
                    == class_start + u64::try_from("Label".len()).unwrap()
        })
        .unwrap();
    exact_source(service, &class_gap.source, b"Label");
    let import = gaps
        .iter()
        .find(|gap| gap.detail == "unresolved-import-target")
        .unwrap();
    let import_start =
        u64::try_from(sample.find("import qualified Peer as Other").unwrap()).unwrap();
    assert_eq!(import.source.span().file(), sample_file.id);
    assert_eq!(import.source.span().start_byte(), import_start);
    assert_eq!(
        import.source.span().end_byte(),
        import_start + u64::try_from("import qualified Peer as Other".len()).unwrap()
    );
    exact_source(service, &import.source, b"import qualified Peer as Other");
    for gap in gaps {
        assert_eq!(gap.source.generation(), receipt.generation);
        let file = document
            .files
            .iter()
            .find(|file| file.id == gap.source.span().file())
            .unwrap();
        let expected = if file.path == "Sample.hs" {
            sample
        } else {
            peer
        };
        let span = gap.source.span();
        assert_eq!(gap.source.content_hash(), content_hash(expected.as_bytes()));
        exact_source(
            service,
            &gap.source,
            expected
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap(),
        );
    }
    (
        adjust.id,
        document.entities.iter().map(|entity| entity.id).collect(),
        sample_file.evidence.source.clone().unwrap(),
    )
}

#[test]
fn haskell_native_sources_and_symbols_survive_noop_edit_reopen_and_clean_rebuild() {
    for newline in ["\n", "\r\n"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let sample = SAMPLE.replace('\n', newline);
        let peer = PEER.replace('\n', newline);
        fs::write(repository.path().join("Sample.hs"), &sample).unwrap();
        fs::write(repository.path().join("Peer.hs"), &peer).unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let first = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(first.receipt().discovered_inputs, 2);
        assert_eq!(first.receipt().indexed_files, 2);
        assert!(first.receipt().discovery_complete);
        assert_eq!(first.evidence().rebuilt_files, 2);
        let initial = first.receipt().clone();
        let (original_symbol, original_entities, original_source) =
            assert_generation(&service, &initial, &sample, &peer);
        let noop = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(
            noop.evidence().strategy,
            FirstSliceIndexOperationStrategy::RetainedGeneration
        );
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        assert_eq!(noop.evidence().changed_inputs, 0);
        assert_eq!(noop.evidence().changed_files, 0);

        assert_eq!(sample.matches("value + 1").count(), 1);
        let changed = sample.replace("value + 1", "value + 2");
        assert_ne!(
            content_hash(sample.as_bytes()),
            content_hash(changed.as_bytes())
        );
        fs::write(repository.path().join("Sample.hs"), &changed).unwrap();
        let mutation = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let updated = mutation.receipt().clone();
        assert_ne!(updated.generation, initial.generation);
        assert_eq!(updated.parent, Some(initial.generation));
        assert_eq!(mutation.evidence().changed_files, 1);
        assert!(mutation.evidence().rebuilt_files > 0);
        let (updated_symbol, updated_entities, updated_source) =
            assert_generation(&service, &updated, &changed, &peer);
        assert_eq!(updated_symbol, original_symbol);
        assert_ne!(
            updated_source.content_hash(),
            original_source.content_hash()
        );
        exact_source(&service, &original_source, sample.as_bytes());
        assert!(
            service
                .source_read_with_options_and_budget(
                    updated.generation,
                    vec![original_source.clone()],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .is_err()
        );
        assert_eq!(
            assert_generation(&service, &initial, &sample, &peer).1,
            original_entities
        );
        drop(service);

        let mut reopened =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            assert_generation(&reopened, &initial, &sample, &peer).1,
            original_entities
        );
        assert_eq!(
            assert_generation(&reopened, &updated, &changed, &peer).1,
            updated_entities
        );
        let noop = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &updated);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        let clean = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert!(updated.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, updated.logical_snapshot);
        let (clean_symbol, clean_entities, _) =
            assert_generation(&reopened, clean.receipt(), &changed, &peer);
        assert_eq!(clean_symbol, original_symbol);
        assert_eq!(clean_entities, updated_entities);
        drop(reopened);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            assert_generation(&restored, &initial, &sample, &peer).1,
            original_entities
        );
        assert_eq!(
            assert_generation(&restored, &updated, &changed, &peer).1,
            updated_entities
        );
        assert_eq!(
            assert_generation(&restored, clean.receipt(), &changed, &peer).1,
            clean_entities
        );
    }
}
