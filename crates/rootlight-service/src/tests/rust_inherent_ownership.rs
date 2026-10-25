//! Written inherent owners survive native/project authority and durable restore.
//! The admitted project test seam executes the real provider; process isolation
//! remains a separate host qualification obligation.

use super::*;
use rootlight_ir::{RelationEndpoint, SymbolIdentityClaim, decode_symbol_identity_claim_envelope};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../rootlight-adapter-treesitter/tests/fixtures/rust_named_fields/event_loop.rs"
);

fn unique(document: &NormalizedIrDocument, name: &str, kind: EntityKind) -> EntityRecord {
    let rows: Vec<_> = document
        .entities
        .iter()
        .filter(|row| row.kind == kind && row.canonical_name == name)
        .collect();
    assert_eq!(rows.len(), 1, "{name}");
    rows[0].clone()
}

fn identity(document: &NormalizedIrDocument, symbol: SymbolId) -> SymbolIdentityClaim {
    let rows: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE)
        .map(|extension| decode_symbol_identity_claim_envelope(extension).unwrap())
        .filter(|claim| claim.symbol == symbol)
        .collect();
    assert_eq!(rows.len(), 1);
    rows[0].clone()
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    project: bool,
) -> Vec<SymbolIdentityClaim> {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let owner = unique(document, "EventLoopPhases", EntityKind::Struct);
    let owner_definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 1104
                && site.source.span().end_byte() == 1119
        })
        .collect();
    assert_eq!(owner_definitions.len(), 1);
    assert_eq!(
        owner_definitions[0].target,
        OccurrenceTarget::Resolved { symbol: owner.id }
    );
    let mut recipes = Vec::new();
    for (name, kind, start, end) in [
        ("close_callbacks", EntityKind::Field, 1160, 1175),
        ("run_close_callbacks", EntityKind::Method, 1392, 1411),
        ("drain_v8_close_callbacks", EntityKind::Method, 1608, 1632),
    ] {
        let row = unique(document, name, kind);
        assert_eq!(row.container, Some(ContainerRef::Entity(owner.id)));
        let claim = identity(document, row.id);
        assert_eq!(claim.container, row.container);
        assert_eq!(claim.derived_symbol(), row.id);
        recipes.push(claim);
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved { symbol: row.id }
        );
        assert_eq!(sites[0].enclosing, Some(owner.id));
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.object == RelationEndpoint::Entity(row.id)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].subject, RelationEndpoint::Entity(owner.id));
        assert_eq!(
            edges[0].evidence.source.as_ref().unwrap().content_hash(),
            sites[0].source.content_hash()
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![sites[0].source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].bytes, name.as_bytes());
        if project {
            assert_eq!(
                document
                    .provenance
                    .iter()
                    .find(|provenance| provenance.id == row.provenance)
                    .unwrap()
                    .producer
                    .name(),
                "rootlight-rust-project-test"
            );
            assert_eq!(
                document
                    .provenance
                    .iter()
                    .find(|provenance| provenance.id == edges[0].provenance)
                    .unwrap()
                    .producer
                    .name(),
                "rootlight-rust-project-test"
            );
        }
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 1455
                && site.source.span().end_byte() == 1470)
    );
    recipes
}

#[test]
fn original_deno_owners_survive_structural_and_real_project_refinement() {
    assert_eq!(ORIGINAL.len(), 1724);
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("event_loop.rs"), ORIGINAL).unwrap();
    let project = Arc::new(SourceBackedRustProjectAnalyzer {
        calls: AtomicUsize::new(0),
    });
    let mut service = FirstSliceService::new_with_storage(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        Some(project.clone()),
    )
    .unwrap();
    let structural = service
        .index_repository_with_mode(root.path(), FirstSliceIndexMode::Structural, &deadline())
        .unwrap();
    let before = assert_generation(&service, &structural, false);
    assert_eq!(project.calls.load(Ordering::Relaxed), 0);
    let deep = service
        .index_repository_with_mode(root.path(), FirstSliceIndexMode::Deep, &deadline())
        .unwrap();
    assert_eq!(project.calls.load(Ordering::Relaxed), 1);
    assert!(
        deep.diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.code.ends_with("-fallback"))
    );
    let after = assert_generation(&service, &deep, true);
    assert_eq!(
        before, after,
        "provider refinement must conserve the existing method identity recipe"
    );
}

#[test]
fn original_deno_ownership_survives_noop_edit_rebuild_and_durable_reopen_in_each_route() {
    for mode in [FirstSliceIndexMode::Structural, FirstSliceIndexMode::Deep] {
        let root = durable_test_tempdir();
        let storage = durable_test_tempdir();
        fs::write(root.path().join("event_loop.rs"), ORIGINAL).unwrap();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let project = Arc::new(SourceBackedRustProjectAnalyzer {
            calls: AtomicUsize::new(0),
        });
        let mut service = FirstSliceService::new_durable_with_project_analyzer(
            2,
            paths.state_dir(),
            project.clone(),
            &deadline(),
        )
        .unwrap();
        let first = service
            .index_repository_with_mode(root.path(), mode, &deadline())
            .unwrap();
        let recipes = assert_generation(&service, &first, mode == FirstSliceIndexMode::Deep);
        let noop = service
            .index_repository_with_mode(root.path(), mode, &deadline())
            .unwrap();
        assert_eq!(
            assert_generation(&service, &noop, mode == FirstSliceIndexMode::Deep),
            recipes
        );
        let mut changed = ORIGINAL.to_vec();
        changed.extend_from_slice(b"\n// unrelated trailing trivia\n");
        fs::write(root.path().join("event_loop.rs"), changed).unwrap();
        let edited = service
            .index_repository_with_mode(root.path(), mode, &deadline())
            .unwrap();
        assert_eq!(
            assert_generation(&service, &edited, mode == FirstSliceIndexMode::Deep),
            recipes
        );
        let rebuilt = service
            .index_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(mode),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            assert_generation(&service, &rebuilt, mode == FirstSliceIndexMode::Deep),
            recipes
        );
        if mode == FirstSliceIndexMode::Deep {
            assert!(project.calls.load(Ordering::Relaxed) >= 2);
        }
        drop(service);
        fs::write(root.path().join("event_loop.rs"), b"changed live source").unwrap();
        let reopened = FirstSliceService::new_durable_with_project_analyzer(
            2,
            paths.state_dir(),
            project,
            &deadline(),
        )
        .unwrap();
        assert_eq!(reopened.active_generation(), Some(rebuilt.generation));
        assert_eq!(
            assert_generation(&reopened, &rebuilt, mode == FirstSliceIndexMode::Deep),
            recipes
        );
    }
}

struct SourceBackedRustProjectAnalyzer {
    calls: AtomicUsize,
}

impl FirstSliceProjectAnalyzer for SourceBackedRustProjectAnalyzer {
    fn provider_identity(&self) -> ContentHash {
        content_hash(b"source-backed-rust-inherent-owner-project-test")
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
        if request.language() != "rust" || request.inputs().len() != 1 {
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
            LanguageId::new("rust").map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
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
        let producer = ProducerIdentity::new("rootlight-rust-project-test", "1.0.0", config)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let analyzer = rootlight_adapters::SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            Arc::new(parser),
            producer,
            self.provider_identity(),
            build_context,
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let sdk_request = rootlight_adapter_sdk::ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.rust-owner")
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            BuildTargetId::new("//fixture:rust-owner")
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
