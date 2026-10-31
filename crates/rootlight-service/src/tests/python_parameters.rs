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

fn assert_python_local_read_targets(
    document: &rootlight_ir::NormalizedIrDocument,
    raw: &[u8],
) -> Vec<SourceRef> {
    let text = std::str::from_utf8(raw).unwrap();
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "owned")
        .unwrap();
    let mut sources = Vec::new();
    for (name, start) in [
        ("value", text.find("= value +").unwrap() + 2),
        ("result", text.find("return result").unwrap() + 7),
    ] {
        let local = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Variable
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        let start = u64::try_from(start).unwrap();
        let end = start + u64::try_from(name.len()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(site.syntax_kind, "python.local_read.reference");
        assert_eq!(site.enclosing, Some(owner.id));
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: local.id });
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
                        && edge.object == rootlight_ir::RelationEndpoint::Entity(local.id)
                        && edge.evidence.source.as_ref() == Some(&site.source)
                })
                .count(),
            1
        );
        sources.push(site.source.clone());
    }
    for start in [
        text.find("print(value)").unwrap() + 6,
        text.rfind("return value").unwrap() + 7,
    ] {
        let start = u64::try_from(start).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
        }));
    }
    sources
}

#[test]
fn deep_python_local_reads_keep_project_bindings_across_updates_and_restore() {
    let fixture = TempDir::new().unwrap();
    let state = durable_test_tempdir();
    let paths =
        RuntimePaths::new(state.path().join("state"), state.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let source = b"def owned():\n    value = 1\n    result = value + 2\n    return result\n\nprint(value)\n\ndef other():\n    return value\n";
    let changed = b"def owned():\n    value = 3\n    result = value + 2\n    return result\n\nprint(value)\n\ndef other():\n    return value\n";
    let analyzer = Arc::new(SourceBackedPythonProjectAnalyzer {
        calls: AtomicUsize::new(0),
    });
    let mut service = FirstSliceService::new_durable_with_project_analyzer(
        2,
        paths.state_dir(),
        analyzer.clone(),
        &deadline(),
    )
    .unwrap();
    let mut generation = None;
    for (index, raw) in [source.as_slice(), changed.as_slice()]
        .into_iter()
        .enumerate()
    {
        fs::write(fixture.path().join("written.py"), raw).unwrap();
        let receipt = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .unwrap();
        assert_eq!(analyzer.calls.load(Ordering::Relaxed), index + 1);
        assert!(
            receipt
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.ends_with("-fallback"))
        );
        let sources = {
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            assert_python_local_read_targets(snapshot.document(), raw)
        };
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                sources,
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 2);
        for chunk in read.data.chunks {
            let span = chunk.reference.span();
            assert_eq!(
                chunk.bytes.as_slice(),
                raw.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                )
                .unwrap()
            );
        }
        let no_op = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
            .unwrap();
        assert_eq!(no_op.generation, receipt.generation);
        assert_eq!(analyzer.calls.load(Ordering::Relaxed), index + 1);
        generation = Some(receipt.generation);
    }
    drop(service);
    let mut restored = FirstSliceService::new_durable_with_project_analyzer(
        2,
        paths.state_dir(),
        analyzer.clone(),
        &deadline(),
    )
    .unwrap();
    let generation = generation.unwrap();
    {
        let snapshot = restored.loaded_generation_snapshot(generation).unwrap();
        assert_python_local_read_targets(snapshot.document(), changed);
    }
    let no_op = restored
        .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &deadline())
        .unwrap();
    assert_eq!(no_op.generation, generation);
    assert_eq!(analyzer.calls.load(Ordering::Relaxed), 2);
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
