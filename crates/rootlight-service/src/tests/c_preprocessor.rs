//! C preprocessor gaps preserve publication, stable identity and exact sources.
//! Structural and project providers retain the same written function identity;
//! unsupported projections remain readable, including durable source recovery.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const NO_ROOM: &[u8] = b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n#define ESCAPED\\\n /* comment */ \"tail\"\nint after(void) { return 0; }\n";

fn assert_generation(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt) {
    let cancel = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    let file = document
        .files
        .iter()
        .find(|file| file.path == "limited.c")
        .unwrap();
    assert_eq!(file.language, "c");
    assert_eq!(file.byte_length, u64::try_from(NO_ROOM.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(NO_ROOM));
    let reference = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    assert!(
        document
            .entities
            .iter()
            .filter(|entity| entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id))
            .all(|entity| entity.kind == EntityKind::Module)
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id)
    );
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.detail == "c-preprocessor-replacement-not-analyzed"
                    && gap.source == reference
            })
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
        assert!(
            document
                .coverage_records
                .iter()
                .any(|coverage| coverage.scope == CoverageScope::File(file.id)
                    && coverage.domain == domain
                    && coverage.status != CoverageStatus::Complete)
        );
    }
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "kept_neighbor"
                && entity.kind == EntityKind::Function)
    );
    let located = service
        .code_locate(
            receipt.generation,
            "limited.c".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancel,
        )
        .unwrap();
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.path == "limited.c" && hit.symbol.is_none())
    );
    let read = |reference: SourceRef, expected: &[u8]| {
        let result = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancel,
            )
            .unwrap();
        assert_eq!(result.data.chunks.len(), 1);
        assert_eq!(result.data.chunks[0].reference, reference);
        assert_eq!(result.data.chunks[0].bytes, expected);
    };
    read(reference, NO_ROOM);
    let query = "tail";
    let found = service
        .find_literal(
            receipt.generation,
            file.id,
            query,
            None,
            SourceFindBudget::new(),
            &cancel,
        )
        .unwrap();
    assert_eq!(found.stop, SourceFindStop::EndOfSource);
    assert!(found.continuation.is_none());
    assert_eq!(found.matches.len(), 1);
    let start = u64::try_from(std::str::from_utf8(NO_ROOM).unwrap().find(query).unwrap()).unwrap();
    let expected = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, start, start + u64::try_from(query.len()).unwrap()).unwrap(),
        file.content_hash,
        None,
    );
    assert_eq!(found.matches[0], expected);
    read(expected, query.as_bytes());
}

#[test]
fn no_room_c_macro_keeps_file_gaps_exact_sources_and_analyzed_neighbor_after_reopen() {
    let fixture = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(fixture.path().join("limited.c"), NO_ROOM).unwrap();
    fs::write(
        fixture.path().join("neighbor.c"),
        b"int kept_neighbor(void) { return 1; }\n",
    )
    .unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        policy.clone(),
        paths.state_dir(),
        &Cancellation::new(),
    )
    .unwrap();
    let prepared = service
        .prepare_repository_with_options(
            fixture.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            &Cancellation::new(),
        )
        .unwrap();
    let commit = service
        .publish_prepared_with_metrics(prepared, &Cancellation::new())
        .unwrap();
    let receipt = commit.receipt().clone();
    assert_eq!(receipt.indexed_files, 2);
    assert_generation(&service, &receipt);
    fs::write(
        fixture.path().join("limited.c"),
        b"unrelated live replacement\n",
    )
    .unwrap();
    assert_generation(&service, &receipt);
    drop(service);
    let reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &Cancellation::new())
            .unwrap();
    assert_generation(&reopened, &receipt);
}

struct SourceBackedCProjectAnalyzer {
    calls: AtomicUsize,
}

impl FirstSliceProjectAnalyzer for SourceBackedCProjectAnalyzer {
    fn provider_identity(&self) -> ContentHash {
        content_hash(b"source-backed-c-preprocessor-project-test")
    }

    fn analyze(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
        use rootlight_adapter_sdk::{
            AnalysisUnitId, BuildTargetId, EncodingId, GenerationBoundSnapshot, LanguageId,
            MemoryAdmissionPolicy, ParseProvider, ProjectAnalysisLimits, ProjectSourceInput,
            execute_project_analysis,
        };

        cancellation
            .check()
            .map_err(|error| FirstSliceProjectAnalysisError::Cancelled(error.reason()))?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        if request.language() != "c" || request.inputs().len() != 1 {
            return Err(FirstSliceProjectAnalysisError::Analysis);
        }
        let input = &request.inputs()[0];
        let snapshot = SourceSnapshot::from_persisted(
            request.repository(),
            RelativePath::parse(Path::new(input.path()))
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            input.file(),
            input.content_hash(),
            input.source().to_vec(),
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let source = SourceRef::new(
            request.repository(),
            request.generation(),
            SourceSpan::new(
                input.file(),
                0,
                u64::try_from(input.source().len()).unwrap(),
            )
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            input.content_hash(),
            None,
        );
        let sdk_input = ProjectSourceInput::new(
            GenerationBoundSnapshot::new(&snapshot, &source)
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            LanguageId::new("c").map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            EncodingId::utf8(),
            input.generated(),
            input.origins().to_vec(),
        );
        let parser = TreeSitterProvider::new(
            parser_config(64 * 1024).map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let limits = provider_analysis_limits(
            &analysis_limits(64 * 1024).map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            parser.capabilities(),
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?
        .with_project_limits(
            ProjectAnalysisLimits::new(1, 256 * 1024, 64 * 1024, 16, 128 * 1024, 128, 128)
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
        );
        let build_context = BuildContextIdentity::new(request.build_context());
        let config = content_hash(request.context_manifest());
        let producer = ProducerIdentity::new("rootlight-c-project-test", "1.0.0", config)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let analyzer = rootlight_adapters::SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::C,
            Arc::new(parser),
            producer,
            self.provider_identity(),
            build_context,
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let sdk_request = rootlight_adapter_sdk::ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.c-preprocessor")
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            BuildTargetId::new("//fixture:c-preprocessor")
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            build_context,
            config,
            request.context_manifest(),
            vec![sdk_input],
            AnalysisTier::TierB,
            &limits,
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let output = execute_project_analysis(
            &analyzer,
            &sdk_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )
        .map_err(|error| match error {
            AdapterError::Cancelled { reason } => FirstSliceProjectAnalysisError::Cancelled(reason),
            _ => FirstSliceProjectAnalysisError::Analysis,
        })?;
        // This private service seam requires admission; process isolation is
        // qualified separately by the adapter-host tests, not this fixture.
        Ok(FirstSliceProjectAnalysis::new(
            output.document().clone(),
            true,
        ))
    }
}

#[test]
fn body_preprocessor_context_keeps_one_c_symbol_after_semantic_refinement() {
    for condition in ["#if defined(_WIN32)", "#ifdef _WIN32", "#ifndef _WIN32"] {
        let fixture = TempDir::new().unwrap();
        let source = format!(
            "struct heap {{ int value; }};\nstatic struct heap *timer_heap(const void* loop) {{\n{}{condition}\n  return (struct heap*) loop;\n#else\n  return (struct heap*) loop;\n#endif\n}}\nint invoke_heap(void) {{ return timer_heap(0) != 0; }}\n",
            "  (void) loop;\n".repeat(300),
        );
        fs::write(fixture.path().join("timer.c"), &source).unwrap();
        let analyzer = Arc::new(SourceBackedCProjectAnalyzer {
            calls: AtomicUsize::new(0),
        });
        let mut service = FirstSliceService::new_with_storage(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            Some(analyzer.clone()),
        )
        .unwrap();
        let structural = service
            .index_repository_with_mode(
                fixture.path(),
                FirstSliceIndexMode::Structural,
                &deadline(),
            )
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(structural.generation)
            .unwrap();
        let before = snapshot
            .document()
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "timer_heap"
            })
            .collect::<Vec<_>>();
        assert_eq!(before.len(), 1);
        let expected_symbol = before[0].id;
        let expected_discriminator =
            rootlight_ir::canonical_symbol_signature("(const void* loop)", 1024)
                .unwrap()
                .into_bytes();
        drop(snapshot);

        let refined = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .unwrap();
        assert_eq!(analyzer.calls.load(Ordering::Relaxed), 1);
        assert!(
            refined
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.ends_with("-fallback"))
        );
        let snapshot = service
            .loaded_generation_snapshot(refined.generation)
            .unwrap();
        let document = snapshot.document();
        assert!(
            document.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == rootlight_adapters::PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC
            }),
            "the real project producer must exercise bounded optional reference retention"
        );
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "timer_heap"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            entities.len(),
            1,
            "body preprocessor metadata must not split one function identity"
        );
        let entity = entities[0];
        assert_eq!(entity.id, expected_symbol);
        let claim = document
            .extensions
            .iter()
            .filter_map(|extension| {
                rootlight_ir::decode_symbol_identity_claim_envelope(extension).ok()
            })
            .find(|claim| claim.symbol == entity.id)
            .unwrap();
        assert_eq!(claim.signature_discriminator, expected_discriminator);
        for domain in [
            IrFactDomain::Entities,
            IrFactDomain::Occurrences,
            IrFactDomain::Relations,
        ] {
            assert!(
                document.skipped_regions.iter().any(|gap| {
                    gap.domain == domain
                        && gap.detail == "c-preprocessor-conditional-evaluation-unavailable"
                }),
                "unevaluated preprocessor context must remain an explicit {domain:?} gap"
            );
        }
        let reference = entity.evidence.source.as_ref().unwrap().clone();
        assert_eq!(reference.generation(), refined.generation);
        let located = service
            .code_locate(
                refined.generation,
                "timer_heap".to_owned(),
                LocateMode::Exact,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol, Some(expected_symbol));
        assert_eq!(hits[0].path, "timer.c");
        let start = usize::try_from(reference.span().start_byte()).unwrap();
        let end = usize::try_from(reference.span().end_byte()).unwrap();
        let read = service
            .source_read_with_options_and_budget(
                refined.generation,
                vec![reference],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].bytes, source.as_bytes()[start..end]);
        assert!(source[start..end].starts_with("static struct heap *timer_heap("));
        assert!(source[start..end].contains(condition));
    }
}
