//! Named Python formal binders must remain independently locatable after indexing.
//! Definition leaves, exact reads and function ownership distinguish parameters
//! from annotation, documentation and body identifiers with the same spelling.

use super::*;

#[test]
fn named_function_parameter_retains_lookup_source_and_owner() {
    let fixture = TempDir::new().unwrap();
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/python_parameters/floor.py"
    );
    fs::write(fixture.path().join("floor.py"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "floor")
        .unwrap();
    let located = service
        .code_locate(
            receipt.generation,
            "x".to_owned(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    let hits: Vec<_> = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol.is_some())
        .collect();
    assert_eq!(hits.len(), 1, "missing Python named-function parameter x");
    assert_eq!(hits[0].path, "floor.py");
    assert_eq!(hits[0].kind, "parameter");
    let parameter = service
        .symbol_explain(receipt.generation, hits[0].symbol.unwrap(), &deadline())
        .unwrap()
        .data
        .entity;
    assert_eq!(parameter.kind, EntityKind::Parameter);
    assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == OccurrenceTarget::Resolved {
                        symbol: parameter.id,
                    }
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let start = source
        .windows(b"def floor(x: float)".len())
        .position(|part| part == b"def floor(x: float)")
        .unwrap()
        + b"def floor(".len();
    let reference = definitions[0].source.clone();
    assert_eq!(reference.span().start_byte(), u64::try_from(start).unwrap());
    assert_eq!(
        reference.span().end_byte(),
        u64::try_from(start + 1).unwrap()
    );
    assert_eq!(reference.repository(), receipt.repository);
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(
        reference.content_hash(),
        rootlight_ids::content_hash(source)
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, b"x");
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == rootlight_ir::RelationEndpoint::Entity(function.id)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(parameter.id)
            })
            .count(),
        1
    );
}

struct SourceBackedPythonProjectAnalyzer {
    calls: AtomicUsize,
}

impl FirstSliceProjectAnalyzer for SourceBackedPythonProjectAnalyzer {
    fn provider_identity(&self) -> ContentHash {
        content_hash(b"source-backed-python-parameter-project-test")
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
        if request.language() != "python" || request.inputs().len() != 1 {
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
            LanguageId::new("python").map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
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
        let producer = ProducerIdentity::new("rootlight-python-project-test", "1.0.0", config)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let analyzer = rootlight_adapters::SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Python,
            Arc::new(parser),
            producer,
            self.provider_identity(),
            build_context,
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let sdk_request = rootlight_adapter_sdk::ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.python-parameters")
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            BuildTargetId::new("//fixture:python-parameters")
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
fn deep_refinement_preserves_named_parameter_containment_and_exact_source() {
    let fixture = TempDir::new().unwrap();
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/python_parameters/floor.py"
    );
    fs::write(fixture.path().join("floor.py"), source).unwrap();
    let analyzer = Arc::new(SourceBackedPythonProjectAnalyzer {
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
        .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Structural, &deadline())
        .unwrap();
    let before = service
        .loaded_generation_snapshot(structural.generation)
        .unwrap();
    let function = before
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "floor")
        .unwrap()
        .id;
    let parameter = before
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "x")
        .unwrap()
        .id;
    assert_eq!(
        before
            .document()
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == rootlight_ir::RelationEndpoint::Entity(function)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(parameter)
            })
            .count(),
        1
    );
    drop(before);

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
    let located = service
        .code_locate(
            refined.generation,
            "x".to_owned(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    let hits: Vec<_> = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol.is_some())
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].symbol, Some(parameter));
    let explained = service
        .symbol_explain(refined.generation, parameter, &deadline())
        .unwrap()
        .data
        .entity;
    assert_eq!(explained.kind, EntityKind::Parameter);
    assert_eq!(explained.container, Some(ContainerRef::Entity(function)));
    assert!(document.entities.iter().any(|entity| {
        entity.id == function
            && entity.kind == EntityKind::Function
            && entity.canonical_name == "floor"
    }));
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: parameter }
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let start = source
        .windows(b"def floor(x: float)".len())
        .position(|part| part == b"def floor(x: float)")
        .unwrap()
        + b"def floor(".len();
    let reference = definitions[0].source.clone();
    assert_eq!(reference.span().start_byte(), u64::try_from(start).unwrap());
    assert_eq!(
        reference.span().end_byte(),
        u64::try_from(start + 1).unwrap()
    );
    assert_eq!(reference.repository(), refined.repository);
    assert_eq!(reference.generation(), refined.generation);
    assert_eq!(reference.content_hash(), content_hash(source));
    let read = service
        .source_read_with_options_and_budget(
            refined.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, b"x");
    let containment: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == rootlight_ir::RelationEndpoint::Entity(function)
                && edge.object == rootlight_ir::RelationEndpoint::Entity(parameter)
        })
        .collect();
    assert_eq!(
        containment.len(),
        1,
        "Deep refinement must preserve formal parameter containment"
    );
    let declarations: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Declares
                && edge.subject == rootlight_ir::RelationEndpoint::Entity(function)
                && edge.object == rootlight_ir::RelationEndpoint::Entity(parameter)
        })
        .collect();
    assert_eq!(
        declarations.len(),
        1,
        "Contains must preserve the independent Declares fact"
    );
    let witness = containment[0].evidence.source.as_ref().unwrap();
    assert_eq!(Some(witness), declarations[0].evidence.source.as_ref());
    assert_eq!(witness.repository(), reference.repository());
    assert_eq!(witness.generation(), reference.generation());
    assert_eq!(witness.span().file(), reference.span().file());
    assert_eq!(witness.content_hash(), reference.content_hash());
    assert!(witness.span().start_byte() <= reference.span().start_byte());
    assert!(witness.span().end_byte() >= reference.span().end_byte());
    assert_eq!(
        containment[0].evidence.derivation,
        declarations[0].evidence.derivation
    );
}
