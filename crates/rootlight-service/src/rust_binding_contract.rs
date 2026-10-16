//! Publication preserves Rust lexical proofs and physical encoded sources.
//! Project-produced evidence is checked separately from structural fallback.

use super::*;
use rootlight_ir::{ContainerRef, OccurrenceTarget};
use std::fs;

use rootlight_adapter_sdk::{
    AnalysisUnitId as SdkAnalysisUnitId, BuildTargetId, ProjectAnalysisLimits,
    ProjectAnalysisRequest as SdkProjectAnalysisRequest, ProjectSourceInput,
    execute_project_analysis,
};
use rootlight_adapters::SemanticProjectAnalyzer;
use rootlight_ir::{RUST_DIRECT_CALL_NAMESPACE, decode_rust_direct_call_envelope};
use rootlight_vfs::SourceText;
use std::sync::atomic::{AtomicUsize, Ordering};

struct SourceBackedRustProjectAnalyzer {
    identity: ContentHash,
    calls: AtomicUsize,
}

impl SourceBackedRustProjectAnalyzer {
    fn new() -> Self {
        Self {
            identity: content_hash(b"source-backed-rust-project-service-fixture"),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl FirstSliceProjectAnalyzer for SourceBackedRustProjectAnalyzer {
    fn provider_identity(&self) -> ContentHash {
        self.identity
    }

    fn analyze(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
        cancellation
            .check()
            .map_err(|error| FirstSliceProjectAnalysisError::Cancelled(error.reason()))?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.language() != "rust" {
            return Err(FirstSliceProjectAnalysisError::Analysis);
        }

        let mut snapshots = Vec::new();
        let mut sources = Vec::new();
        let mut encodings = Vec::new();
        for input in request.inputs() {
            let path = RelativePath::parse(Path::new(input.path()))
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let snapshot = SourceSnapshot::from_persisted(
                request.repository(),
                path,
                input.file(),
                input.content_hash(),
                input.source().to_vec(),
            )
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let text = SourceText::parse(snapshot.content(), cancellation)
                .map_err(|error| FirstSliceProjectAnalysisError::Cancelled(error.reason()))?
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            let encoding = EncodingId::new(text.encoding().as_str())
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let end = u64::try_from(snapshot.content().len())
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            let span = SourceSpan::new(snapshot.file(), 0, end)
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            sources.push(SourceRef::new(
                request.repository(),
                request.generation(),
                span,
                snapshot.content_hash(),
                None,
            ));
            encodings.push(encoding);
            snapshots.push(snapshot);
        }
        let mut sdk_inputs = snapshots
            .iter()
            .zip(&sources)
            .zip(&encodings)
            .zip(request.inputs())
            .map(|(((snapshot, source), encoding), input)| {
                Ok(ProjectSourceInput::new(
                    GenerationBoundSnapshot::new(snapshot, source)
                        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
                    LanguageId::new("rust")
                        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
                    encoding.clone(),
                    input.generated(),
                    input.origins().to_vec(),
                ))
            })
            .collect::<Result<Vec<_>, FirstSliceProjectAnalysisError>>()?;
        // SDK path identities include framing and need not sort like display paths.
        sdk_inputs.sort_by(|left, right| {
            left.source()
                .path()
                .identity_bytes()
                .cmp(right.source().path().identity_bytes())
        });
        let build_context = BuildContextIdentity::new(request.build_context());
        let config_digest = content_hash(request.context_manifest());
        let producer = ProducerIdentity::new("rootlight-rust-project-test", "1.0.0", config_digest)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
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
            ProjectAnalysisLimits::new(4, 256 * 1024, 64 * 1024, 16, 128 * 1024, 128, 128)
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
        );
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            Arc::new(parser),
            producer,
            self.identity,
            build_context,
        )
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let sdk_request = SdkProjectAnalysisRequest::new(
            SdkAnalysisUnitId::new("fixture.rust-project")
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            BuildTargetId::new("//fixture:rust-project")
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            build_context,
            config_digest,
            request.context_manifest(),
            sdk_inputs,
            AnalysisTier::TierB,
            &limits,
        )
        .map_err(|error| {
            eprintln!(
                "source-backed Rust SDK request failed: {error:?}; paths: {:?}",
                request
                    .inputs()
                    .iter()
                    .map(|input| input.path())
                    .collect::<Vec<_>>()
            );
            FirstSliceProjectAnalysisError::Analysis
        })?;
        let output = execute_project_analysis(
            &analyzer,
            &sdk_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )
        .map_err(|error| match error {
            AdapterError::Cancelled { reason } => FirstSliceProjectAnalysisError::Cancelled(reason),
            error => {
                eprintln!("source-backed Rust producer failed: {error:?}");
                FirstSliceProjectAnalysisError::Analysis
            }
        })?;
        for input in request.inputs() {
            let file = output
                .document()
                .files
                .iter()
                .find(|file| file.id == input.file())
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            let text = SourceText::parse(input.source(), cancellation)
                .map_err(|error| FirstSliceProjectAnalysisError::Cancelled(error.reason()))?
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            let physical_bytes = u64::try_from(input.source().len())
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
            if file.path != input.path()
                || file.content_hash != input.content_hash()
                || file.byte_length != physical_bytes
                || file.encoding != text.encoding().as_str()
            {
                return Err(FirstSliceProjectAnalysisError::Analysis);
            }
        }

        // The service's private test seam requires this admission bit. Only the
        // real adapter-host process tests can establish native isolation.
        Ok(FirstSliceProjectAnalysis::new(
            output.document().clone(),
            true,
        ))
    }
}

fn service_with_source_backed_rust_project(
    mode: FirstSliceIndexMode,
) -> (FirstSliceService, Arc<SourceBackedRustProjectAnalyzer>) {
    let bridge = Arc::new(SourceBackedRustProjectAnalyzer::new());
    let service = match mode {
        FirstSliceIndexMode::Structural => FirstSliceService::new(2).unwrap(),
        FirstSliceIndexMode::Deep => FirstSliceService::new_with_storage(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            Some(bridge.clone()),
        )
        .unwrap(),
    };
    assert_eq!(
        service.deep_analysis_available(),
        mode == FirstSliceIndexMode::Deep
    );
    (service, bridge)
}

fn assert_direct_call_source_carrier(
    document: &NormalizedIrDocument,
    call: &OccurrenceRecord,
    definition: &OccurrenceRecord,
    mode: FirstSliceIndexMode,
    bridge: &SourceBackedRustProjectAnalyzer,
) {
    let envelopes = document
        .extensions
        .iter()
        .filter(|envelope| {
            envelope.namespace == RUST_DIRECT_CALL_NAMESPACE
                && envelope.evidence.source.as_ref() == Some(&call.source)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        envelopes.len(),
        1,
        "one exact written-call proof must survive publication"
    );
    let envelope = envelopes[0];
    let claim = decode_rust_direct_call_envelope(envelope).unwrap();
    let OccurrenceTarget::Resolved { symbol } = definition.target else {
        panic!("written function definition must resolve");
    };
    assert_eq!(claim.target, symbol);
    assert_eq!(claim.definition, definition.source.span());
    let carrier = document
        .provenance
        .iter()
        .find(|record| record.id == envelope.provenance)
        .unwrap();
    assert_eq!(definition.provenance, carrier.id);
    assert_eq!(carrier.language, "rust");
    assert_eq!(carrier.tier, AnalysisTier::TierB);
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.provenance, carrier.id);
    assert_eq!(
        claim.canonical_name,
        content_hash(entity.canonical_name.as_bytes())
    );
    let file = document
        .files
        .iter()
        .find(|file| file.id == call.file)
        .unwrap();
    assert_eq!(file.provenance, carrier.id);
    assert_eq!(file.content_hash, call.source.content_hash());
    let adopted = document
        .provenance
        .iter()
        .find(|record| record.id == call.provenance)
        .unwrap();
    assert!(
        adopted
            .derivation_parents
            .contains(&FactRef::Fact(envelope.id))
    );
    assert!(
        adopted
            .derivation_parents
            .contains(&FactRef::Fact(definition.id))
    );
    match mode {
        FirstSliceIndexMode::Structural => assert_eq!(carrier.producer_kind, ProducerKind::Parser),
        FirstSliceIndexMode::Deep => {
            assert_eq!(carrier.producer_kind, ProducerKind::Derivation);
            assert_eq!(carrier.producer.name(), "rootlight-rust-project-test");
            assert_eq!(carrier.binary_digest, bridge.provider_identity());
            assert_eq!(
                carrier.frontend_version.as_deref(),
                Some("rootlight-project-structural-v1")
            );
            assert_eq!(
                carrier.rule.as_deref(),
                Some("rootlight.project-structural.v1")
            );
            assert!(
                !document
                    .diagnostics
                    .iter()
                    .any(|record| is_project_fallback_code(&record.code))
            );
        }
    }
}

fn encoded(source: &str, encoding: u8) -> Vec<u8> {
    match encoding {
        0 => source.as_bytes().to_vec(),
        1 | 2 => {
            let mut bytes = if encoding == 1 {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for value in source.encode_utf16() {
                bytes.extend_from_slice(&if encoding == 1 {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                });
            }
            bytes
        }
        _ => unreachable!("fixture supports only the three source encodings"),
    }
}

fn physical_range(source: &str, name: &str, position: usize, encoding: u8) -> (u64, u64) {
    let start = encoded(&source[..position], encoding).len();
    let end = start
        + if encoding == 0 {
            name.len()
        } else {
            name.encode_utf16().count() * 2
        };
    (u64::try_from(start).unwrap(), u64::try_from(end).unwrap())
}

#[test]
fn encoded_locals_keep_exact_definition_owner_and_reference_proof() {
    // The scalar changes physical offsets across UTF-8 and UTF-16 projections.
    let source = "// λ\nfn owner(parameter: i32) {\n let local = parameter;\n { let local = local; use_value(local); }\n use_value(local);\n}\n";
    for encoding in 0..3 {
        let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let bytes = encoded(source, encoding);
        fs::write(fixture.path().join("lib.rs"), &bytes).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
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
        let file = &document.files[0];
        assert_eq!(document.files.len(), 1);
        assert_eq!(file.language, "rust");
        assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(&bytes));
        let definitions = [
            ("owner", source.find("owner").unwrap()),
            ("parameter", source.find("parameter").unwrap()),
            ("local", source.find("local").unwrap()),
            (
                "local",
                source.find("let local = local").unwrap() + "let ".len(),
            ),
        ];
        let mut symbols = Vec::new();
        for (name, position) in definitions {
            let range = physical_range(source, name, position, encoding);
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && (
                            site.source.span().start_byte(),
                            site.source.span().end_byte(),
                        ) == range
                })
                .collect();
            assert_eq!(
                sites.len(),
                1,
                "encoding {encoding}: definition {name} at {range:?}"
            );
            assert_eq!(sites[0].source.repository(), receipt.repository);
            assert_eq!(sites[0].source.generation(), receipt.generation);
            assert_eq!(sites[0].source.content_hash(), file.content_hash);
            let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
                panic!("written definition is missing its owned symbol");
            };
            symbols.push(symbol);
        }
        assert_ne!(
            symbols[2], symbols[3],
            "shadowed leaves need distinct symbols"
        );
        for symbol in &symbols[1..] {
            let owners: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.object == RelationEndpoint::Entity(*symbol)
                })
                .collect();
            assert_eq!(owners.len(), 1);
            assert_eq!(owners[0].subject, RelationEndpoint::Entity(symbols[0]));
        }
        for (anchor, name, symbol, last) in [
            ("local = parameter", "parameter", symbols[1], false),
            ("let local = local", "local", symbols[2], false),
            ("use_value(local)", "local", symbols[3], false),
            ("use_value(local)", "local", symbols[2], true),
        ] {
            let position = if last {
                source.rfind(anchor).unwrap()
            } else {
                source.find(anchor).unwrap()
            } + anchor.rfind(name).unwrap();
            let range = physical_range(source, name, position, encoding);
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Reference
                        && (
                            site.source.span().start_byte(),
                            site.source.span().end_byte(),
                        ) == range
                })
                .collect();
            assert_eq!(sites.len(), 1, "encoding {encoding}: value use {anchor}");
            assert_eq!(sites[0].target, OccurrenceTarget::Resolved { symbol });
        }
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .collect();
        assert_eq!(calls.len(), 2, "both written calls must remain represented");
        assert!(
            calls
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        for call in calls {
            assert!(
                document.skipped_regions.iter().any(|gap| {
                    gap.detail == "rust-call-dispatch-target-unavailable"
                        && gap.source.span() == call.source.span()
                        && gap.source.content_hash() == file.content_hash
                        && gap.source.repository() == receipt.repository
                        && gap.source.generation() == receipt.generation
                }),
                "each written call needs an exact dispatch gap"
            );
        }
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
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
        assert_eq!(read.data.chunks[0].bytes, bytes);
        let no_op = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        assert_eq!(no_op.generation, receipt.generation);
    }
}

#[test]
fn direct_function_calls_adopt_exact_native_and_project_proofs() {
    let source =
        "// λ\nfn target() -> i32 { 1 }\nfn caller() { target(); let target = || 2; target(); }\n";
    for mode in [FirstSliceIndexMode::Structural, FirstSliceIndexMode::Deep] {
        for encoding in 0..3 {
            let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
            let bytes = encoded(source, encoding);
            fs::write(fixture.path().join("lib.rs"), &bytes).unwrap();
            let (mut service, bridge) = service_with_source_backed_rust_project(mode);
            let cancellation = Cancellation::new();
            let receipt = service
                .index_repository_with_mode(fixture.path(), mode, &cancellation)
                .unwrap();
            assert_eq!(
                bridge.calls(),
                usize::from(mode == FirstSliceIndexMode::Deep)
            );
            let document = service
                .inspect_generation(
                    receipt.repository,
                    Some(receipt.generation),
                    &cancellation,
                    |snapshot| snapshot.document().clone(),
                )
                .unwrap();
            let file = &document.files[0];
            assert_eq!(
                file.encoding,
                ["utf-8", "utf-16le", "utf-16be"][usize::from(encoding)]
            );
            assert_eq!(file.content_hash, content_hash(&bytes));
            assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
            let definition_range =
                physical_range(source, "target", source.find("target").unwrap(), encoding);
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && (
                            site.source.span().start_byte(),
                            site.source.span().end_byte(),
                        ) == definition_range
                })
                .unwrap();
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("written function definition must resolve");
            };
            for (position, resolved) in [
                (source.find("target();").unwrap(), true),
                (source.rfind("target();").unwrap(), false),
            ] {
                let range = physical_range(source, "target", position, encoding);
                let calls = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::CallSite
                            && (
                                site.source.span().start_byte(),
                                site.source.span().end_byte(),
                            ) == range
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    calls.len(),
                    1,
                    "mode {mode:?}, encoding {encoding}, source {range:?}"
                );
                let call = calls[0];
                assert_eq!(call.source.content_hash(), content_hash(&bytes));
                assert_eq!(call.source.repository(), receipt.repository);
                assert_eq!(call.source.generation(), receipt.generation);
                assert_eq!(call.syntactic_text_hash, content_hash(b"target"));
                let detail = if resolved {
                    assert_eq!(call.target, OccurrenceTarget::Resolved { symbol });
                    let producer = document
                        .provenance
                        .iter()
                        .find(|record| record.id == call.provenance)
                        .unwrap();
                    assert_eq!(producer.producer_kind, ProducerKind::Rule);
                    assert_direct_call_source_carrier(&document, call, definition, mode, &bridge);
                    assert_eq!(
                        producer.rule.as_deref(),
                        Some("rust-direct-function-v1.written_module")
                    );
                    assert!(producer.input_sources.contains(&definition.source));
                    assert_eq!(
                        document
                            .relations
                            .iter()
                            .filter(|edge| edge.predicate == RelationPredicate::Calls
                                && edge.object == RelationEndpoint::Entity(symbol)
                                && edge.evidence.source.as_ref() == Some(&call.source))
                            .count(),
                        1
                    );
                    "rust-call-typecheck-and-instantiation-unverified"
                } else {
                    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
                    assert!(
                        !document
                            .relations
                            .iter()
                            .any(|edge| edge.predicate == RelationPredicate::Calls
                                && edge.evidence.source.as_ref() == Some(&call.source))
                    );
                    "rust-call-dispatch-target-unavailable"
                };
                assert!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.detail == detail && gap.source == call.source)
                );
            }
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                file.content_hash,
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
            assert_eq!(read.data.chunks[0].bytes, bytes);
            let no_op = service
                .index_repository_with_mode(fixture.path(), mode, &cancellation)
                .unwrap();
            assert_eq!(no_op.generation, receipt.generation);
            assert_eq!(
                bridge.calls(),
                usize::from(mode == FirstSliceIndexMode::Deep)
            );
        }
    }
}

#[test]
fn project_module_calls_and_generic_sites_survive_encoded_updates() {
    let root = "pub mod calls;\npub mod worker;\n";
    let caller = concat!(
        "// λ😀\nuse crate::worker::ping;\n",
        "pub fn call() { ping(); crate::worker::generic::<u8>(); ",
        "let ping = || (); ping(); }\n",
    );
    let original_worker = "pub fn ping() -> u8 { 1 }\npub fn generic<T>() {}\n";
    let updated_worker = original_worker.replace("{ 1 }", "{ 2 }");
    for encoding in 0..3 {
        let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::create_dir(fixture.path().join("src")).unwrap();
        for (path, text) in [
            ("lib.rs", root),
            ("calls.rs", caller),
            ("worker.rs", original_worker),
        ] {
            fs::write(
                fixture.path().join("src").join(path),
                encoded(text, encoding),
            )
            .unwrap();
        }
        let (mut service, bridge) =
            service_with_source_backed_rust_project(FirstSliceIndexMode::Deep);
        let cancellation = Cancellation::new();
        let first = service
            .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Deep, &cancellation)
            .unwrap();
        let mut original_reads = Vec::new();
        let mut original_symbol = None;
        for phase in 0..2 {
            let worker = if phase == 0 {
                original_worker
            } else {
                &updated_worker
            };
            let receipt = if phase == 0 {
                first.clone()
            } else {
                fs::write(
                    fixture.path().join("src/worker.rs"),
                    encoded(worker, encoding),
                )
                .unwrap();
                service
                    .index_repository_with_mode(
                        fixture.path(),
                        FirstSliceIndexMode::Deep,
                        &cancellation,
                    )
                    .unwrap()
            };
            assert_eq!(bridge.calls(), phase + 1);
            if phase == 1 {
                assert_ne!(receipt.generation, first.generation);
            }
            let document = service
                .inspect_generation(
                    receipt.repository,
                    Some(receipt.generation),
                    &cancellation,
                    |snapshot| snapshot.document().clone(),
                )
                .unwrap();
            assert!(
                !document
                    .diagnostics
                    .iter()
                    .any(|record| is_project_fallback_code(&record.code)),
                "encoding {encoding}, phase {phase}: {:?}",
                document.diagnostics,
            );
            let caller_file = document
                .files
                .iter()
                .find(|file| file.path == "src/calls.rs")
                .unwrap();
            let worker_file = document
                .files
                .iter()
                .find(|file| file.path == "src/worker.rs")
                .unwrap();
            assert_eq!(
                caller_file.content_hash,
                content_hash(&encoded(caller, encoding))
            );
            assert_eq!(
                worker_file.content_hash,
                content_hash(&encoded(worker, encoding))
            );
            for file in [caller_file, worker_file] {
                assert_eq!(
                    file.encoding,
                    ["utf-8", "utf-16le", "utf-16be"][usize::from(encoding)]
                );
            }
            let definition_range =
                physical_range(worker, "ping", worker.find("ping").unwrap(), encoding);
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.file == worker_file.id
                        && site.role == OccurrenceRole::Definition
                        && (
                            site.source.span().start_byte(),
                            site.source.span().end_byte(),
                        ) == definition_range
                })
                .unwrap();
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("worker definition must resolve");
            };
            match original_symbol {
                None => original_symbol = Some(symbol),
                Some(original) => assert_eq!(symbol, original),
            }
            let mut reads = Vec::new();
            for (index, callee, position, detail) in [
                (
                    0,
                    "ping",
                    caller.find("ping();").unwrap(),
                    "rust-call-typecheck-and-instantiation-unverified",
                ),
                (
                    1,
                    "crate::worker::generic::<u8>",
                    caller.find("crate::worker::generic::<u8>").unwrap(),
                    "rust-generic-instantiation-and-typecheck-unverified",
                ),
                (
                    2,
                    "ping",
                    caller.rfind("ping();").unwrap(),
                    "rust-written-module-call-proof-unavailable",
                ),
            ] {
                let range = physical_range(caller, callee, position, encoding);
                let sites = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.file == caller_file.id
                            && site.role == OccurrenceRole::CallSite
                            && (
                                site.source.span().start_byte(),
                                site.source.span().end_byte(),
                            ) == range
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    sites.len(),
                    1,
                    "encoding {encoding}, phase {phase}, callee {callee}"
                );
                let call = sites[0];
                assert_eq!(call.syntactic_text_hash, content_hash(callee.as_bytes()));
                assert_eq!(call.source.repository(), receipt.repository);
                assert_eq!(call.source.generation(), receipt.generation);
                assert!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == call.source && gap.detail == detail)
                );
                let edges = document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.predicate == RelationPredicate::Calls
                            && edge.evidence.source.as_ref() == Some(&call.source)
                    })
                    .collect::<Vec<_>>();
                if index == 0 {
                    assert_eq!(call.target, OccurrenceTarget::Resolved { symbol });
                    assert_eq!(edges.len(), 1);
                    assert_eq!(edges[0].object, RelationEndpoint::Entity(symbol));
                    assert!(call.evidence.derivation.contains(&FactRef::Entity(symbol)));
                    assert!(
                        call.evidence
                            .derivation
                            .contains(&FactRef::File(worker_file.id))
                    );
                    assert!(
                        call.evidence
                            .derivation
                            .contains(&FactRef::Fact(worker_file.provenance))
                    );
                    let producer = document
                        .provenance
                        .iter()
                        .find(|record| record.id == call.provenance)
                        .unwrap();
                    assert_eq!(producer.producer_kind, ProducerKind::Derivation);
                    assert_eq!(producer.binary_digest, bridge.provider_identity());
                } else {
                    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
                    assert!(edges.is_empty());
                }
                assert!(
                    !document
                        .extensions
                        .iter()
                        .any(|envelope| envelope.namespace == RUST_DIRECT_CALL_NAMESPACE
                            && envelope.evidence.source.as_ref() == Some(&call.source))
                );
                reads.push((
                    call.source.clone(),
                    encoded(callee, encoding)
                        .into_iter()
                        .skip(if encoding == 0 { 0 } else { 2 })
                        .collect::<Vec<_>>(),
                ));
            }
            reads.push((
                definition.source.clone(),
                encoded("ping", encoding)
                    .into_iter()
                    .skip(if encoding == 0 { 0 } else { 2 })
                    .collect(),
            ));
            let result = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    reads
                        .iter()
                        .map(|(reference, _)| reference.clone())
                        .collect(),
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &cancellation,
                )
                .unwrap();
            assert_eq!(result.data.chunks.len(), reads.len());
            for (chunk, (reference, bytes)) in result.data.chunks.iter().zip(&reads) {
                assert_eq!(chunk.reference, *reference);
                assert_eq!(chunk.bytes, *bytes);
            }
            if phase == 0 {
                original_reads = reads;
            }
            let no_op = service
                .index_repository_with_mode(
                    fixture.path(),
                    FirstSliceIndexMode::Deep,
                    &cancellation,
                )
                .unwrap();
            assert_eq!(no_op.generation, receipt.generation);
            assert_eq!(bridge.calls(), phase + 1);
        }
        let old = service
            .source_read_with_options_and_budget(
                first.generation,
                original_reads
                    .iter()
                    .map(|(reference, _)| reference.clone())
                    .collect(),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(old.data.chunks.len(), original_reads.len());
        for (chunk, (reference, bytes)) in old.data.chunks.iter().zip(original_reads) {
            assert_eq!(chunk.reference, reference);
            assert_eq!(chunk.bytes, bytes);
        }
    }
}

#[test]
fn encoded_markdown_calls_remain_inside_their_written_examples() {
    let source = concat!(
        "# λ\n\n",
        "~~~rust\nfn target() {}\nfn caller() { target(/*first*/); }\n~~~\n\n",
        "~~~rust\nmod inner { fn target() {} fn caller() { target(/*second*/); } }\n~~~\n\n",
        "~~~rust\nfn alone() { target(/*missing*/); }\n~~~\n\n",
        "~~~rust\nfn broken( {\n~~~\n",
    );
    let sole = "~~~rust\n// λ\nfn target() {}\nfn caller() { target(/*first*/); }\n~~~\n";
    for (source, markers, expected_targets) in [
        (source, &["first", "second", "missing"][..], 2),
        (sole, &["first"][..], 1),
    ] {
        for mode in [FirstSliceIndexMode::Structural, FirstSliceIndexMode::Deep] {
            for encoding in 0..3 {
                let fixture = tempfile::tempdir().unwrap();
                let bytes = encoded(source, encoding);
                fs::write(fixture.path().join("guide.md"), &bytes).unwrap();
                let mut service = FirstSliceService::new(2).unwrap();
                let cancellation = Cancellation::new();
                let receipt = service
                    .index_repository_with_mode(fixture.path(), mode, &cancellation)
                    .unwrap();
                let document = service
                    .inspect_generation(
                        receipt.repository,
                        Some(receipt.generation),
                        &cancellation,
                        |snapshot| snapshot.document().clone(),
                    )
                    .unwrap();
                let file = &document.files[0];
                assert_eq!(document.files.len(), 1);
                assert_eq!(file.language, "markdown");
                assert_eq!(file.content_hash, content_hash(&bytes));
                let mut targets = BTreeSet::new();
                let mut examples = BTreeSet::new();
                for marker in markers.iter().copied() {
                    let authored = format!("target(/*{marker}*/)");
                    let range =
                        physical_range(source, "target", source.find(&authored).unwrap(), encoding);
                    let calls = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::CallSite
                                && (
                                    site.source.span().start_byte(),
                                    site.source.span().end_byte(),
                                ) == range
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        calls.len(),
                        1,
                        "mode {mode:?}, encoding {encoding}, call {marker}"
                    );
                    let call = calls[0];
                    assert_eq!(call.source.content_hash(), file.content_hash);
                    assert_eq!(call.source.repository(), receipt.repository);
                    assert_eq!(call.source.generation(), receipt.generation);
                    assert_eq!(call.syntactic_text_hash, content_hash(b"target"));
                    let detail = if marker == "missing" {
                        assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
                        assert!(!document.relations.iter().any(|edge| {
                            edge.predicate == RelationPredicate::Calls
                                && edge.evidence.source.as_ref() == Some(&call.source)
                        }));
                        "rust-call-dispatch-target-unavailable"
                    } else {
                        let OccurrenceTarget::Resolved { symbol } = call.target else {
                            panic!("clean example must retain its exact target: {call:?}");
                        };
                        targets.insert(symbol);
                        let mut owner = symbol;
                        let block = loop {
                            let entity = document
                                .entities
                                .iter()
                                .find(|item| item.id == owner)
                                .unwrap();
                            if entity.language == "markdown" {
                                break entity;
                            }
                            let Some(ContainerRef::Entity(parent)) = entity.container else {
                                panic!("embedded Rust must not fall back to the Markdown file");
                            };
                            owner = parent;
                        };
                        assert_eq!(block.kind, rootlight_ir::EntityKind::Module);
                        assert_eq!(block.canonical_name, "<code-block>");
                        examples.insert(block.id);
                        let witness = document
                            .occurrences
                            .iter()
                            .find(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.syntax_kind == "markdown.code_block.module"
                                    && site.target
                                        == OccurrenceTarget::Resolved { symbol: block.id }
                            })
                            .unwrap();
                        assert_eq!(block.evidence.source.as_ref(), Some(&witness.source));
                        let producer = document
                            .provenance
                            .iter()
                            .find(|item| item.id == call.provenance)
                            .unwrap();
                        assert_eq!(producer.producer_kind, ProducerKind::Rule);
                        assert_eq!(producer.language, "markdown");
                        assert_eq!(producer.tier, rootlight_ir::AnalysisTier::TierD);
                        assert!(
                            producer
                                .derivation_parents
                                .contains(&rootlight_ir::FactRef::Fact(witness.id))
                        );
                        assert!(producer.input_sources.contains(&witness.source));
                        assert_eq!(
                            document
                                .relations
                                .iter()
                                .filter(|edge| {
                                    edge.predicate == RelationPredicate::Calls
                                        && edge.object == RelationEndpoint::Entity(symbol)
                                        && edge.evidence.source.as_ref() == Some(&call.source)
                                })
                                .count(),
                            1
                        );
                        "rust-call-typecheck-and-instantiation-unverified"
                    };
                    assert!(
                        document
                            .skipped_regions
                            .iter()
                            .any(|gap| gap.detail == detail && gap.source == call.source)
                    );
                }
                assert_eq!(targets.len(), expected_targets);
                assert_eq!(examples.len(), expected_targets);
                let reference = SourceRef::new(
                    receipt.repository,
                    receipt.generation,
                    SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                    file.content_hash,
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
                assert_eq!(read.data.chunks[0].bytes, bytes);
                let no_op = service
                    .index_repository_with_mode(fixture.path(), mode, &cancellation)
                    .unwrap();
                assert_eq!(no_op.generation, receipt.generation);
            }
        }
    }
}
