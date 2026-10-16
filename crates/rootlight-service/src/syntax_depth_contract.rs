//! Default structural admission keeps deep written definitions available.
//! Explicit caller limits and the native hard depth bound still retain exact sources.

use super::*;
use rootlight_ir::OccurrenceTarget;
use std::fs;

fn nested_sources() -> [(&'static str, String); 2] {
    let mut v = String::from("module main\nfn nested() {\n");
    let mut rust = String::from("fn nested() {\n");
    for _ in 0..200 {
        v.push_str("if true {\n");
        rust.push_str("{\n");
    }
    v.push_str("deep_value := 1\nprintln(deep_value)\n");
    rust.push_str("let deep_value = 1;\nprintln!(\"{}\", deep_value);\nfn deep_function() {}\n");
    for _ in 0..200 {
        v.push_str("}\n");
        rust.push_str("}\n");
    }
    v.push_str("}\nfn tail() {}\n");
    rust.push_str("}\nfn tail() {}\n");
    [("deep.v", v), ("deep.rs", rust)]
}

fn set_explicit_depth(service: &mut FirstSliceService, depth: usize) {
    let base = &service.analysis_limits;
    service.analysis_limits = AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        depth,
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap();
}

fn index_sources(
    service: &mut FirstSliceService,
    inputs: &[(&str, String)],
) -> (
    tempfile::TempDir,
    FirstSliceIndexReceipt,
    NormalizedIrDocument,
) {
    let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    for (path, source) in inputs {
        fs::write(fixture.path().join(path), source).unwrap();
    }
    let cancellation = Cancellation::new();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let document = service
        .inspect_generation(
            receipt.repository,
            Some(receipt.generation),
            &cancellation,
            |snapshot| snapshot.document().clone(),
        )
        .unwrap();
    assert_eq!(document.files.len(), inputs.len());
    for (path, source) in inputs {
        let file = document
            .files
            .iter()
            .find(|file| file.path == *path)
            .unwrap();
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, u64::try_from(source.len()).unwrap()).unwrap(),
            content_hash(source.as_bytes()),
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(read.data.chunks[0].bytes, source.as_bytes());
    }
    (fixture, receipt, document)
}

#[test]
fn default_depth_retains_shallow_and_deep_written_definitions() {
    let mut service = FirstSliceService::new(2).unwrap();
    let inputs = nested_sources();
    let (fixture, receipt, document) = index_sources(&mut service, &inputs);
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (path, source) in &inputs {
        let file = document
            .files
            .iter()
            .find(|file| file.path == *path)
            .unwrap();
        let deep_names: &[&str] = if *path == "deep.rs" {
            &["deep_value", "deep_function"]
        } else {
            &["deep_value"]
        };
        let mut symbols = BTreeMap::new();
        for name in std::iter::once("nested")
            .chain(deep_names.iter().copied())
            .chain(std::iter::once("tail"))
        {
            let start = source.find(name).unwrap();
            let end = start + name.len();
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte() == u64::try_from(end).unwrap()
                })
                .collect();
            assert_eq!(definitions.len(), 1, "{path}: missing exact {name}");
            let site = definitions[0];
            assert_eq!(site.source.content_hash(), file.content_hash);
            assert_eq!(site.source.repository(), receipt.repository);
            assert_eq!(site.source.generation(), receipt.generation);
            let OccurrenceTarget::Resolved { symbol } = &site.target else {
                panic!("written definition must resolve to its entity");
            };
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == *symbol)
                .unwrap();
            assert_eq!(entity.canonical_name, name);
            symbols.insert(name, *symbol);
        }
        for name in deep_names {
            let parents: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.object == RelationEndpoint::Entity(symbols[name])
                })
                .collect();
            assert_eq!(parents.len(), 1, "{path}: missing owner of {name}");
            assert_eq!(
                parents[0].subject,
                RelationEndpoint::Entity(symbols["nested"])
            );
        }
    }
    let no_op = service
        .index_repository(fixture.path(), &Cancellation::new())
        .unwrap();
    assert_eq!(no_op.generation, receipt.generation);
}

#[test]
fn explicit_lower_depth_retains_gap_and_exact_source() {
    let mut service = FirstSliceService::new(2).unwrap();
    set_explicit_depth(&mut service, 128);
    let (_, _, document) = index_sources(&mut service, &nested_sources());
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert_eq!(document.diagnostics.len(), 2);
    for file in &document.files {
        assert!(document.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "syntax-depth-limit"
                && diagnostic
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        }));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "syntax-depth-limit" && gap.source.span().file() == file.id
        }));
    }
}

#[test]
fn frontend_admission_preserves_output_quotas_and_lower_caller_depth() {
    let mut service = FirstSliceService::new(2).unwrap();
    for (language, expected_depth) in [("awk", 128), ("rust", 4096)] {
        let limits = provider_analysis_limits(
            &service.analysis_limits,
            service.structural_parsers[language].capabilities(),
        )
        .unwrap();
        assert_eq!(limits.max_syntax_depth(), expected_depth);
        assert_eq!(
            limits.syntax_stream(),
            service.analysis_limits.syntax_stream()
        );
        assert_eq!(limits.ir_stream(), service.analysis_limits.ir_stream());
        assert_eq!(limits.ir(), service.analysis_limits.ir());
        assert_eq!(limits.project(), service.analysis_limits.project());
    }
    set_explicit_depth(&mut service, 64);
    for language in ["awk", "rust"] {
        let limits = provider_analysis_limits(
            &service.analysis_limits,
            service.structural_parsers[language].capabilities(),
        )
        .unwrap();
        assert_eq!(limits.max_syntax_depth(), 64);
    }
}

#[test]
fn shallow_frontend_indexes_under_default_and_lower_depth_ceilings() {
    let inputs = [(
        "written.awk",
        "function written() { return 1; }\n".to_owned(),
    )];
    for explicit_depth in [None, Some(64)] {
        let mut service = FirstSliceService::new(2).unwrap();
        if let Some(depth) = explicit_depth {
            set_explicit_depth(&mut service, depth);
        }
        let (_, _, document) = index_sources(&mut service, &inputs);
        let function = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "written")
            .expect("the provider accepts the repository's default request");
        assert!(document.occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target
                    == OccurrenceTarget::Resolved {
                        symbol: function.id,
                    }
                && occurrence.source.span().start_byte() == 9
                && occurrence.source.span().end_byte() == 16
        }));
    }
}

#[test]
fn native_hard_depth_remains_an_explicit_source_backed_gap() {
    let mut service = FirstSliceService::new(2).unwrap();
    let source = format!(
        "module main\nfn nested() {{\nvalue := {}1{}\nprintln(value)\n}}\nfn tail() {{}}\n",
        "(".repeat(4200),
        ")".repeat(4200),
    );
    let (_, _, document) = index_sources(&mut service, &[("deep.v", source)]);
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert_eq!(document.diagnostics.len(), 1);
    assert_eq!(document.diagnostics[0].code, "syntax-depth-limit");
    assert_eq!(document.skipped_regions.len(), 1);
    assert_eq!(document.skipped_regions[0].detail, "syntax-depth-limit");
}

#[test]
fn cancelled_deep_source_does_not_publish_a_generation() {
    let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    for (path, source) in nested_sources() {
        fs::write(fixture.path().join(path), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let cancellation = Cancellation::new();
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        service.index_repository(fixture.path(), &cancellation),
        Err(FirstSliceError::Cancelled(_))
    ));
    assert!(service.active_by_repository.is_empty());
}
