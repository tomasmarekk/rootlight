//! Durable source-format selection for explicitly configured native COBOL analysis.
//! Fixed and free routes retain authored bytes and structural declarations; generic
//! COBOL remains source-only, and nondefinition targets are not inferred.

use super::*;
use rootlight_ir::OccurrenceTarget;
use std::fs;

const SAMPLE: &str = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. SAMPLE.\n       DATA DIVISION.\n       WORKING-STORAGE SECTION.\n       01 STORED PIC 9 VALUE 1.\n       COPY MISSING.\n       PROCEDURE DIVISION.\n       MAIN.\n           MOVE STORED TO STORED.\n           PERFORM MAIN.\n           CALL \"EXTERNAL\".\n           STOP RUN.\n";

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

fn policy(language: &str) -> FirstSliceStoragePolicy {
    // All eight published generations must stay within this fixture's retention window.
    let configuration = format!(
        "version='1.3'\n[storage]\nretained_generations=8\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\ncbl='{language}'\n"
    );
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(&configuration), 8).unwrap();
    assert_eq!(policy.retained_generations(), 8);
    policy
}

fn exact_source(
    service: &FirstSliceService,
    reference: &SourceRef,
    language: &str,
    expected: &[u8],
) {
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
    assert_eq!(chunk.language, language);
    assert_eq!(chunk.bytes, expected);
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    language: &str,
    source: &str,
) -> (BTreeSet<SymbolId>, SourceRef) {
    let retained = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = retained.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.path, "sample.cbl");
    assert_eq!(file.language, language);
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(full.repository(), receipt.repository);
    assert_eq!(full.generation(), receipt.generation);
    assert_eq!(full.content_hash(), file.content_hash);
    assert_eq!(full.span().file(), file.id);
    assert_eq!(full.span().start_byte(), 0);
    assert_eq!(full.span().end_byte(), file.byte_length);
    exact_source(service, full, language, source.as_bytes());
    let provenance = document
        .provenance
        .iter()
        .find(|record| record.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.repository, receipt.repository);
    assert_eq!(provenance.generation, receipt.generation);
    assert_eq!(provenance.language, language);

    if language == "cobol" {
        assert!(document.entities.is_empty());
        assert!(document.occurrences.is_empty());
        assert!(document.relations.is_empty());
        assert_eq!(provenance.producer_kind, ProducerKind::Rule);
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "unsupported-language" })
        );
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.reason == SkippedRegionReason::UnsupportedConstruct && gap.source == *full
        }));
        return (BTreeSet::new(), full.clone());
    }

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
        .find(|descriptor| descriptor.language().as_str() == language)
        .unwrap();
    let frontend = format!("tree-sitter-{language}-{}", descriptor.grammar_version());
    assert_eq!(provenance.producer_kind, ProducerKind::Parser);
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some(frontend.as_str())
    );
    assert_eq!(
        provenance.binary_digest,
        grammar_binary_digest(descriptor).unwrap()
    );
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == language)
    );

    for (kind, name) in [
        (EntityKind::Module, "SAMPLE"),
        (EntityKind::Variable, "STORED"),
        (EntityKind::Function, "MAIN"),
    ] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == kind && entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1, "{language}: written declaration {name}");
        let entity = entities[0];
        let start = u64::try_from(source.find(name).unwrap()).unwrap();
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(name.len()).unwrap()
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{language}: exact definition {name}");
        assert_eq!(
            definitions[0].target,
            OccurrenceTarget::Resolved { symbol: entity.id }
        );
        exact_source(service, &definitions[0].source, language, name.as_bytes());
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hits: Vec<_> = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol == Some(entity.id) && hit.file == file.id)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "{language}: retained definition query {name}"
        );
        let hit = hits[0];
        assert_eq!(hit.identifier, name);
        assert_eq!(hit.language, language);
        let reference = hit.source.as_ref().unwrap();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), file.content_hash);
        assert_eq!(reference.span().file(), file.id);
        let span = reference.span();
        exact_source(
            service,
            reference,
            language,
            source
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap(),
        );
    }
    let move_start = source.find("MOVE STORED TO STORED.").unwrap();
    let expected_uses = [
        (
            move_start + "MOVE ".len(),
            "STORED",
            "cobol.storage.reference",
            OccurrenceRole::Reference,
        ),
        (
            move_start + "MOVE STORED TO ".len(),
            "STORED",
            "cobol.storage.reference",
            OccurrenceRole::Reference,
        ),
        (
            source.find("PERFORM MAIN.").unwrap() + "PERFORM ".len(),
            "MAIN",
            "cobol.perform_target.reference",
            OccurrenceRole::CallSite,
        ),
        (
            source.find("CALL \"EXTERNAL\".").unwrap() + "CALL ".len(),
            "\"EXTERNAL\"",
            "cobol.call_target.reference",
            OccurrenceRole::CallSite,
        ),
        (
            source.find("COPY MISSING.").unwrap(),
            "COPY MISSING.",
            "cobol.copy.import",
            OccurrenceRole::ImportUse,
        ),
    ];
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::Reference | OccurrenceRole::CallSite | OccurrenceRole::ImportUse
            )
        })
        .collect();
    assert_eq!(
        uses.len(),
        expected_uses.len(),
        "{language}: retained use sites {:?}",
        uses.iter()
            .map(|site| (site.syntax_kind.as_str(), site.source.span(), site.role))
            .collect::<Vec<_>>()
    );
    for (start, written, syntax_kind, role) in expected_uses {
        let span = SourceSpan::new(
            file.id,
            u64::try_from(start).unwrap(),
            u64::try_from(start + written.len()).unwrap(),
        )
        .unwrap();
        let matches: Vec<_> = uses
            .iter()
            .filter(|site| site.source.span() == span)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "{language}: exact source use {syntax_kind}"
        );
        let site = matches[0];
        assert_eq!(site.syntax_kind, syntax_kind);
        assert_eq!(site.role, role);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert_eq!(site.syntactic_text_hash, content_hash(written.as_bytes()));
        let detail = if role == OccurrenceRole::ImportUse {
            "cobol-copy-target-unavailable"
        } else {
            "cobol-binding-target-unavailable"
        };
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == IrFactDomain::Relations
                && gap.detail == detail
                && gap.source.span() == span
        }));
    }
    // Quoted dispatch retains independent lexical evidence, not another binding use.
    let strings: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::StringEvidence)
        .collect();
    assert_eq!(strings.len(), 1);
    let string = strings[0];
    let quoted_start = source.find("CALL \"EXTERNAL\".").unwrap() + "CALL ".len();
    assert_eq!(string.syntax_kind, "cobol.string.string");
    assert_eq!(
        string.source.span(),
        SourceSpan::new(
            file.id,
            u64::try_from(quoted_start).unwrap(),
            u64::try_from(quoted_start + "\"EXTERNAL\"".len()).unwrap(),
        )
        .unwrap()
    );
    assert_eq!(string.syntactic_text_hash, content_hash(b"\"EXTERNAL\""));
    assert_eq!(document.entities.len(), 3);
    assert_eq!(document.occurrences.len(), 9);
    for site in &document.occurrences {
        assert_eq!(site.repository, receipt.repository);
        assert_eq!(site.generation, receipt.generation);
        assert_eq!(site.source.repository(), receipt.repository);
        assert_eq!(site.source.generation(), receipt.generation);
        assert_eq!(site.source.content_hash(), full.content_hash());
        assert_eq!(site.source.span().file(), file.id);
        assert_eq!(
            site.id,
            rootlight_ir::derive_occurrence_record_id(site).unwrap()
        );
        let span = site.source.span();
        let written = source
            .as_bytes()
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, &site.source, language, written);
        if !matches!(
            site.role,
            OccurrenceRole::Definition | OccurrenceRole::Declaration
        ) {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(
                document
                    .relations
                    .iter()
                    .all(|relation| { relation.subject != RelationEndpoint::Occurrence(site.id) })
            );
        }
    }
    assert!(document.relations.iter().all(|relation| {
        matches!(
            relation.predicate,
            RelationPredicate::Contains
                | RelationPredicate::Declares
                | RelationPredicate::DefinesAt
        )
    }));
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.domain == IrFactDomain::Relations
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
        })
        .collect();
    assert!(!gaps.is_empty());
    for gap in gaps {
        assert_eq!(gap.source.repository(), receipt.repository);
        assert_eq!(gap.source.generation(), receipt.generation);
        assert_eq!(gap.source.content_hash(), full.content_hash());
        assert_eq!(gap.source.span().file(), file.id);
        let span = gap.source.span();
        exact_source(
            service,
            &gap.source,
            language,
            source
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap(),
        );
    }
    (
        document.entities.iter().map(|entity| entity.id).collect(),
        full.clone(),
    )
}

#[test]
fn configured_cobol_formats_rebuild_unchanged_sources_and_survive_reopen() {
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
        let source = SAMPLE.replace('\n', newline);
        fs::write(repository.path().join("sample.cbl"), &source).unwrap();
        let mut prior: Option<GenerationId> = None;
        let mut retained: Vec<(FirstSliceIndexReceipt, &str, BTreeSet<SymbolId>, SourceRef)> =
            Vec::new();
        let mut first_fixed_symbols: Option<BTreeSet<SymbolId>> = None;
        for language in ["cobol-fixed", "cobol-free", "cobol", "cobol-fixed"] {
            let storage_policy = policy(language);
            let configuration_hash = storage_policy.config.hash();
            let mut service = FirstSliceService::new_durable_with_policy(
                storage_policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            let committed = index(
                &mut service,
                repository.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            );
            let receipt = committed.receipt().clone();
            assert_eq!(receipt.discovered_inputs, 1);
            assert!(receipt.discovery_complete);
            assert_eq!(
                service
                    .generation_metadata(receipt.generation)
                    .unwrap()
                    .configuration_hash(),
                configuration_hash
            );
            if let Some(previous) = prior {
                assert_ne!(receipt.generation, previous);
                assert!(committed.evidence().changed_inputs > 0);
            }
            assert_eq!(
                committed.evidence().changed_files,
                u64::from(prior.is_none())
            );
            assert_eq!(
                committed.evidence().rebuilt_files,
                u64::from(language != "cobol")
            );
            assert_eq!(committed.evidence().reused_files, 0);
            let (symbols, full) = assert_generation(&service, &receipt, language, &source);
            if language == "cobol-fixed" {
                if let Some(initial) = &first_fixed_symbols {
                    assert_eq!(&symbols, initial);
                } else {
                    first_fixed_symbols = Some(symbols.clone());
                }
            }
            for (old_receipt, old_language, old_symbols, old_source) in &retained {
                // Public reads hydrate retained payloads from earlier configurations.
                exact_source(&service, old_source, old_language, source.as_bytes());
                assert_eq!(
                    assert_generation(&service, old_receipt, old_language, &source).0,
                    *old_symbols
                );
                assert!(
                    service
                        .source_read_with_options_and_budget(
                            receipt.generation,
                            vec![old_source.clone()],
                            SourceReadOptions::new(),
                            FirstSliceBudget::default(),
                            &deadline()
                        )
                        .is_err()
                );
            }
            let noop = index(
                &mut service,
                repository.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            );
            assert_eq!(noop.receipt(), &receipt);
            assert_eq!(
                noop.evidence().strategy,
                FirstSliceIndexOperationStrategy::RetainedGeneration
            );
            assert_eq!(noop.evidence().changed_inputs, 0);
            assert_eq!(noop.evidence().changed_files, 0);
            assert_eq!(noop.evidence().rebuilt_files, 0);
            assert_eq!(noop.evidence().rebuilt_facts, 0);
            drop(service);

            let mut reopened = FirstSliceService::new_durable_with_policy(
                storage_policy,
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            exact_source(&reopened, &full, language, source.as_bytes());
            assert_eq!(
                assert_generation(&reopened, &receipt, language, &source).0,
                symbols
            );
            let noop = index(
                &mut reopened,
                repository.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            );
            assert_eq!(noop.receipt(), &receipt);
            assert_eq!(noop.evidence().rebuilt_files, 0);
            assert_eq!(noop.evidence().rebuilt_facts, 0);
            let clean = index(
                &mut reopened,
                repository.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            );
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(clean.receipt().logical_snapshot, receipt.logical_snapshot);
            assert_eq!(
                assert_generation(&reopened, clean.receipt(), language, &source).0,
                symbols
            );
            exact_source(&reopened, &full, language, source.as_bytes());
            assert_eq!(
                fs::read(repository.path().join("sample.cbl")).unwrap(),
                source.as_bytes()
            );
            prior = Some(clean.receipt().generation);
            retained.push((receipt, language, symbols, full));
        }
    }
}
