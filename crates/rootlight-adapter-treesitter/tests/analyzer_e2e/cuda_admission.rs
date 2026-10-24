//! Complete CUDA corpus sources exercise declared/native routing and exact facets.
//! Ordinary unresolved References remain beside independently certified Reads
//! and launcher Calls; imported header and runtime targets stay unresolved.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

const CUDA: LanguageCase = LanguageCase {
    name: "cuda",
    path: "dev/cuda/crossentropy_forward.cu",
    frontend: "tree-sitter-cuda-0.21.2",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const CUH: LanguageCase = LanguageCase {
    name: "cpp",
    path: "llmc/fused_classifier.cuh",
    frontend: "tree-sitter-cpp-0.23.4",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const ORIGINAL: &[u8] =
    include_bytes!("../fixtures/cuda_original/dev/cuda/crossentropy_forward.cu");
const HEADER: &[u8] = include_bytes!("../fixtures/cuda_original/llmc/fused_classifier.cuh");
const SIMPLE: &str = "__global__ void kernel(int value) {}\nvoid launch(int scalar) { int next = scalar * 2; kernel<<<1, 1>>>(next); }\n";

fn output(case: LanguageCase, bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files[0].content_hash, content_hash(bytes));
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    at: (u64, u64),
    language: &str,
) -> SymbolId {
    let found: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition && span(site) == at)
        .collect();
    assert_eq!(found.len(), 1, "one exact {name} definition at {at:?}");
    let OccurrenceTarget::Resolved { symbol } = found[0].target else {
        panic!("definition resolves")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(
        (
            entity.canonical_name.as_str(),
            entity.kind,
            entity.language.as_str()
        ),
        (name, kind, language)
    );
    symbol
}

fn span(site: &rootlight_ir::OccurrenceRecord) -> (u64, u64) {
    (
        site.source.span().start_byte(),
        site.source.span().end_byte(),
    )
}

fn contains(document: &rootlight_ir::NormalizedIrDocument, owner: SymbolId, member: SymbolId) {
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == member)
            .unwrap()
            .container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(member))
            .count(),
        1
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(member)
                && edge.object == RelationEndpoint::Entity(owner))
    );
}

fn facet(
    document: &rootlight_ir::NormalizedIrDocument,
    bytes: &[u8],
    at: (u64, u64),
    role: OccurrenceRole,
    target: SymbolId,
    owner: SymbolId,
) {
    let refs: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| span(site) == at && site.role == OccurrenceRole::Reference)
        .collect();
    let facets: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| span(site) == at && site.role == role)
        .collect();
    assert_eq!((refs.len(), facets.len()), (1, 1));
    assert!(matches!(
        refs[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let added = facets[0];
    assert_eq!(added.target, OccurrenceTarget::Resolved { symbol: target });
    assert_eq!(added.enclosing, Some(owner));
    assert_eq!(added.source, refs[0].source);
    assert_eq!(added.provenance, refs[0].provenance);
    assert_eq!(added.source.content_hash(), content_hash(bytes));
    assert_ne!(added.id, refs[0].id);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(added.id)
                && edge.object == RelationEndpoint::Entity(target)
                && edge.evidence.source.as_ref() == Some(&added.source))
            .count(),
        1
    );
    if role == OccurrenceRole::CallSite {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(target)
                    && edge.evidence.source.as_ref() == Some(&added.source))
                .count(),
            1
        );
    }
}

#[test]
fn full_original_cuda_has_exact_kernel_formal_launcher_call_and_comment_negative() {
    assert_eq!(ORIGINAL.len(), 5192);
    let result = output(CUDA, ORIGINAL, EncodingId::utf8());
    let doc = result.document();
    let kernel = definition(
        doc,
        "crossentropy_forward_kernel1",
        EntityKind::Function,
        (1259, 1287),
        "cuda",
    );
    let losses = definition(doc, "losses", EntityKind::Parameter, (1295, 1301), "cuda");
    let launcher = definition(
        doc,
        "crossentropy_forward1",
        EntityKind::Function,
        (1815, 1836),
        "cuda",
    );
    contains(doc, kernel, losses);
    facet(
        doc,
        ORIGINAL,
        (2107, 2135),
        OccurrenceRole::CallSite,
        kernel,
        launcher,
    );
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition && span(site) == (243, 263))
    );
}

#[test]
fn full_original_cuh_keeps_cpp_fields_scalar_formal_read_and_pending_device_call() {
    assert_eq!(HEADER.len(), 6949);
    let result = output(CUH, HEADER, EncodingId::utf8());
    let doc = result.document();
    let structure = definition(doc, "SoftmaxParams", EntityKind::Struct, (408, 421), "cpp");
    let scale = definition(doc, "Scale", EntityKind::Field, (435, 440), "cpp");
    let offset = definition(doc, "Offset", EntityKind::Field, (453, 459), "cpp");
    contains(doc, structure, scale);
    contains(doc, structure, offset);
    let owner = definition(
        doc,
        "prepare_softmax_blockwide3",
        EntityKind::Function,
        (493, 519),
        "cpp",
    );
    let idx = definition(doc, "idx", EntityKind::Parameter, (528, 531), "cpp");
    contains(doc, owner, idx);
    facet(doc, HEADER, (682, 685), OccurrenceRole::Read, idx, owner);
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition && span(site) == (2431, 2444))
    );
    assert!(
        !doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().start_byte() <= 3479
                        && source.span().end_byte() >= 3505))
    );
}

#[test]
fn cuda_backed_cuh_preserves_existing_const_receiver_proof_and_plain_cpp_route() {
    let text = "struct Value { int field; };\n__device__ void Owned(const Value &value) { External(value[0]); }\n__global__ void kernel() {}\nvoid launch() { kernel<<<1, 1>>>(); }\n";
    let result = output(CUH, text.as_bytes(), EncodingId::utf8());
    let doc = result.document();
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.canonical_name == "field"
                && entity.kind == EntityKind::Field
                && entity.language == "cpp")
    );
    assert_eq!(
        doc.occurrences
            .iter()
            .filter(
                |site| site.syntax_kind == "cpp.const_formal_receiver.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            )
            .count(),
        1
    );
    let plain = LanguageCase {
        path: "plain.cpp",
        frontend: "tree-sitter-cpp-0.23.4",
        ..CUH
    };
    let ordinary = output(
        plain,
        b"struct S { int field; }; void run(int value) {}\n",
        EncodingId::utf8(),
    );
    assert!(
        ordinary
            .document()
            .entities
            .iter()
            .all(|entity| entity.language == "cpp")
    );
}

#[test]
fn cuda_certified_native_references_and_endpoint_markers_are_unique_and_required() {
    for (case, bytes, at, label) in [
        (
            CUDA,
            ORIGINAL,
            (2107, 2135),
            "cuda.kernel_launch_target.signature",
        ),
        (CUH, HEADER, (682, 685), "cuda.scalar_read_target.signature"),
    ] {
        let native = Arc::new(provider());
        let fixture = Fixture::new(case, bytes);
        let budget = limits();
        let input = request(&fixture.snapshot, &fixture.source, case, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            native.as_ref(),
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert!(
            !parsed
                .facts()
                .iter()
                .any(|fact| fact.kind() == SyntaxFactKind::ErrorRecovery)
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::Occurrence
                    && fact
                        .syntax_kind()
                        .as_str()
                        .ends_with(".identifier.reference")
                    && (fact.span().start_byte(), fact.span().end_byte()) == at)
                .count(),
            1
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(
                    |fact| fact.syntax_kind().as_str() == label && fact.span().end_byte() == at.1
                )
                .count(),
            1
        );
        let frontend = analyzer(&native, case);
        let (_, artifact) = frontend
            .analyze_and_capture(
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let required = native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap();
        assert_eq!(
            required,
            artifact.required_syntax_fact_count(&deadline()).unwrap()
        );
        let exact = limits_with_syntax_records(required);
        frontend
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &exact),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let smaller = limits_with_syntax_records(required - 1);
        assert!(
            matches!(frontend.analyze_and_capture(&request(&fixture.snapshot, &fixture.source, case, &smaller), ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })) if observed == required && limit == required - 1)
        );
    }
}

#[test]
fn cuda_bom_and_utf16_preserve_physical_reference_and_facet_frames() {
    let text = "// λ😀\r\n__device__ int owned(int scalar) { return scalar * 2; }\r\n";
    let definition_at = text.find("scalar").unwrap();
    let use_at = text.rfind("scalar").unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut bytes = if encoding == "utf-8" {
            vec![0xef, 0xbb, 0xbf]
        } else if encoding == "utf-16le" {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        if encoding == "utf-8" {
            bytes.extend_from_slice(text.as_bytes());
        } else {
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let physical = |offset: usize| {
            if encoding == "utf-8" {
                u64::try_from(3 + offset).unwrap()
            } else {
                u64::try_from(2 + 2 * text[..offset].encode_utf16().count()).unwrap()
            }
        };
        let result = output(CUH, &bytes, EncodingId::new(encoding).unwrap());
        let doc = result.document();
        let owner = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "owned")
            .unwrap();
        let scalar = definition(
            doc,
            "scalar",
            EntityKind::Parameter,
            (physical(definition_at), physical(definition_at + 6)),
            "cpp",
        );
        facet(
            doc,
            &bytes,
            (physical(use_at), physical(use_at + 6)),
            OccurrenceRole::Read,
            scalar,
            owner.id,
        );
    }
}

#[test]
fn cuda_ambiguous_shadowed_mutated_macro_template_and_ordinary_call_sites_decline_facets() {
    for text in [
        "int owned(int value, int value) { return value * 2; }",
        "int owned(int value) { int value = 1; return value * 2; }",
        "int owned(int value) { ++value; return value * 2; }",
        "int owned(int value) { auto f = [&] { return value * 2; }; return value * 2; }",
        "#define value other\nint owned(int value) { return value * 2; }",
        "template<class T> int owned(int value) { return value * 2; }",
        "__global__ void kernel() {} void kernel(int value) {} void launch() { kernel<<<1,1>>>(); }",
        "void kernel() {} void launch() { kernel<<<1,1>>>(); }",
        "__global__ void kernel(int value) {} void launch() { kernel<<<1,1>>>(); }",
        "__global__ void kernel() {} void launch() { kernel(); }",
        "__global__ void kernel() {} void launch() { auto kernel = other; kernel<<<1,1>>>(); }",
    ] {
        let result = output(CUDA, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result.document().occurrences.iter().any(|site| matches!(
                site.syntax_kind.as_str(),
                "cuda.scalar_formal.read" | "cuda.kernel_launch.call"
            )),
            "{text}"
        );
    }
}

#[test]
fn cuda_missing_duplicate_and_foreign_native_certificates_cannot_create_facets() {
    let native = provider();
    let fixture = Fixture::new(CUDA, SIMPLE.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, CUDA, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(CUDA),
            CUDA.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = replay(parsed.facts().to_vec()).unwrap();
    assert!(
        baseline
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "cuda.scalar_formal.read")
    );
    assert!(
        baseline
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "cuda.kernel_launch.call")
    );
    for marker in [
        "cuda.clean_file.signature",
        "cuda.owned_function.signature",
        "cuda.scalar_read_target.signature",
        "cuda.kernel_launch_target.signature",
    ] {
        let facts: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() != marker)
            .cloned()
            .collect();
        if let Ok(result) = replay(facts) {
            assert!(
                !result
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.syntax_kind
                        == if marker == "cuda.kernel_launch_target.signature" {
                            "cuda.kernel_launch.call"
                        } else {
                            "cuda.scalar_formal.read"
                        })
            );
        }
        let mut facts = parsed.facts().to_vec();
        let selected: Vec<_> = facts
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == marker)
            .cloned()
            .collect();
        for witness in selected {
            let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                next,
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.depth(),
                witness.syntax_kind().clone(),
            ));
        }
        if let Ok(result) = replay(facts) {
            assert!(
                !result
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.syntax_kind
                        == if marker == "cuda.kernel_launch_target.signature" {
                            "cuda.kernel_launch.call"
                        } else {
                            "cuda.scalar_formal.read"
                        })
            );
        }
    }
    let at = u64::try_from(SIMPLE.rfind("scalar").unwrap()).unwrap();
    for mutation in 0..9 {
        let mut facts = parsed.facts().to_vec();
        let reference = facts
            .iter()
            .position(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "cuda.identifier.reference"
                    && fact.span().start_byte() == at
            })
            .unwrap();
        let old = facts[reference].clone();
        let root = facts
            .iter()
            .find(|fact| fact.kind() == SyntaxFactKind::Root)
            .unwrap()
            .clone();
        if mutation == 0 {
            facts[reference] = SyntaxFact::new(
                old.local_id(),
                Some(root.local_id()),
                old.kind(),
                old.span(),
                root.depth() + 1,
                old.syntax_kind().clone(),
            );
        } else if mutation == 1 {
            let owner = facts
                .iter()
                .find(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "cuda.function.declaration"
                        && fact.span().start_byte() <= at
                        && fact.span().end_byte() >= at + 6
                })
                .unwrap()
                .clone();
            let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                next,
                Some(owner.local_id()),
                SyntaxFactKind::Scope,
                owner.span(),
                owner.depth() + 1,
                rootlight_adapter_sdk::SyntaxKindLabel::new("cuda.block.scope").unwrap(),
            ));
            facts[reference] = SyntaxFact::new(
                old.local_id(),
                Some(next),
                old.kind(),
                old.span(),
                owner.depth() + 2,
                old.syntax_kind().clone(),
            );
        } else if mutation == 2 {
            let marker = facts
                .iter()
                .position(|fact| {
                    fact.syntax_kind().as_str() == "cuda.scalar_read_target.signature"
                        && fact.span().end_byte() == at + 6
                })
                .unwrap();
            let old = facts[marker].clone();
            let wrong = SourceSpan::new(
                old.span().file(),
                old.span().start_byte() + 1,
                old.span().end_byte(),
            )
            .unwrap();
            facts[marker] = SyntaxFact::new(
                old.local_id(),
                old.parent(),
                old.kind(),
                wrong,
                old.depth(),
                old.syntax_kind().clone(),
            );
        } else if mutation == 3 {
            let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                next,
                old.parent(),
                old.kind(),
                old.span(),
                old.depth(),
                old.syntax_kind().clone(),
            ));
        } else if mutation == 4 {
            let foreign = SourceSpan::new(
                rootlight_ids::FileId::from_bytes([99; 20]),
                old.span().start_byte(),
                old.span().end_byte(),
            )
            .unwrap();
            facts[reference] = SyntaxFact::new(
                old.local_id(),
                old.parent(),
                old.kind(),
                foreign,
                old.depth(),
                old.syntax_kind().clone(),
            );
        } else if mutation == 5 {
            facts[reference] = SyntaxFact::new(
                old.local_id(),
                old.parent(),
                old.kind(),
                old.span(),
                old.depth(),
                rootlight_adapter_sdk::SyntaxKindLabel::new("cpp.identifier.reference").unwrap(),
            );
        } else if mutation == 6 {
            facts.retain(|fact| fact.syntax_kind().as_str() != "cuda.parameters.signature");
        } else {
            let start = u64::try_from(SIMPLE.find("scalar").unwrap()).unwrap();
            facts.retain(|fact| {
                if mutation == 7 {
                    !(fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "cuda.parameter.declaration"
                        && fact.span().start_byte() <= start
                        && start + 6 <= fact.span().end_byte())
                } else {
                    !(fact.kind() == SyntaxFactKind::Occurrence
                        && fact.syntax_kind().as_str() == "cuda.identifier.definition"
                        && fact.span().start_byte() == start)
                }
            });
        }
        if let Ok(result) = replay(facts) {
            assert!(
                !result
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.syntax_kind == "cuda.scalar_formal.read")
            );
        }
    }
}

#[test]
fn cuda_ir_extra_facets_reserve_occurrences_relations_records_and_strings_at_exact_fit() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CUDA);
    let fixture = Fixture::new(CUDA, SIMPLE.as_bytes());
    let base = limits();
    let run = |ir| {
        let budget = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            ir,
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, CUDA, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = run(IrLimits::default()).unwrap();
    assert_eq!(
        baseline
            .document()
            .occurrences
            .iter()
            .filter(|site| matches!(
                site.syntax_kind.as_str(),
                "cuda.scalar_formal.read" | "cuda.kernel_launch.call"
            ))
            .count(),
        2
    );
    for dimension in 0..4 {
        let make = |bound| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_occurrences = bound,
                1 => ir.max_relations = bound,
                2 => ir.max_total_records = bound,
                _ => ir.max_total_string_bytes = bound,
            }
            ir
        };
        let (mut low, mut high) = (0, 65_536);
        while low < high {
            let middle = low + (high - low) / 2;
            match run(make(middle)) {
                Ok(result) => {
                    assert_eq!(result.document(), baseline.document());
                    high = middle;
                }
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected quota result: {error:?}"),
            }
        }
        assert!(low > 0 && low < 65_536);
        assert_eq!(run(make(low)).unwrap().document(), baseline.document());
        assert!(
            matches!(run(make(low - 1)), Err(AdapterError::Sink(SinkError::StreamLimit { observed, limit, .. })) if observed > limit && limit == low - 1)
        );
    }
}

#[test]
fn cuda_artifacts_reuse_only_current_source_and_cpp_macro_projection_stays_guarded() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, CUDA);
    let fixture = Fixture::new(CUDA, SIMPLE.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, CUDA, &budget);
    let (_, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let next = fixture.next_generation();
    let input = request(&next.snapshot, &next.source, CUDA, &budget);
    let reused = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&frontend, &input, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    let prepared = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let changed = next.rewrite(SIMPLE.replace("scalar * 2", "scalar * 3").as_bytes());
    assert!(
        frontend
            .analyze_prepared_artifact(
                &request(&changed.snapshot, &changed.source, CUDA, &budget),
                &prepared,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let projected = output(
        CUH,
        b"#define STORAGE static\nSTORAGE __device__ int owned(int value) { return value * 2; }\n",
        EncodingId::utf8(),
    );
    assert!(
        projected
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "owned")
    );
    assert!(
        !projected
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "cuda.scalar_formal.read")
    );
}

#[test]
fn cuda_native_included_ranges_and_pre_cancelled_requests_are_refused() {
    for case in [CUDA, CUH] {
        let native = provider();
        let fixture = Fixture::new(case, SIMPLE.as_bytes());
        let budget = limits();
        let ranged = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::utf8(),
            vec![IncludedRange::new(fixture.source.span(), language(case))],
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap();
        assert!(matches!(
            rootlight_adapter_sdk::execute_parse(
                &native,
                &ranged.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            ),
            Err(AdapterError::RejectedRequest(
                rootlight_adapter_sdk::RequestError::EmbeddedRangesUnsupported
            ))
        ));
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let full = request(&fixture.snapshot, &fixture.source, case, &budget);
        assert!(
            rootlight_adapter_sdk::execute_parse(
                &native,
                &full.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel
            )
            .is_err()
        );
    }
}

#[test]
fn cuda_registry_admission_keeps_unqualified_markdown_ranges_opaque() {
    let markdown = LanguageCase {
        name: "markdown",
        path: "notes.md",
        frontend: "tree-sitter-md-0.5.3",
        ..CUDA
    };
    let text = format!("```cuda\n{SIMPLE}```\n");
    let result = output(markdown, text.as_bytes(), EncodingId::utf8());
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "cuda" || entity.canonical_name == "kernel")
    );
    assert!(!result.document().occurrences.iter().any(|site| matches!(
        site.syntax_kind.as_str(),
        "cuda.kernel_launch.call" | "cuda.scalar_formal.read"
    )));
}

#[test]
fn cuda_artifact_replay_rejects_native_owner_parent_and_depth_substitutions() {
    use std::collections::HashSet;
    for case in [CUDA, CUH] {
        let native = provider();
        let fixture = Fixture::new(case, SIMPLE.as_bytes());
        let budget = limits();
        let input = request(&fixture.snapshot, &fixture.source, case, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            &native,
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let count = |document: &rootlight_ir::NormalizedIrDocument| {
            document
                .occurrences
                .iter()
                .filter(|site| {
                    matches!(
                        site.syntax_kind.as_str(),
                        "cuda.scalar_formal.read" | "cuda.kernel_launch.call"
                    )
                })
                .count()
        };
        let replay = |facts: Vec<SyntaxFact>, expected: Option<usize>| {
            let parser = MockParseProvider::new(
                native.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let frontend = TreeSitterAnalyzer::new(
                Arc::new(parser),
                producer_identity(),
                language(case),
                case.frontend,
                content_hash(BINARY_SEED),
            )
            .unwrap();
            let result = frontend.analyze_and_capture(
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            match result {
                Ok((output, artifact)) => {
                    let actual = count(output.document());
                    if let Some(expected) = expected {
                        assert!(
                            actual < expected,
                            "a substituted proof cannot retain all original facets"
                        );
                    } else {
                        assert_eq!(actual, 2);
                    }
                    let again = frontend
                        .analyze_from_artifact(
                            &input,
                            &artifact,
                            ExtensionSupport::default(),
                            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                            &deadline(),
                        )
                        .unwrap();
                    assert_eq!(
                        count(again.document()),
                        actual,
                        "artifact replay cannot restore rejected evidence"
                    );
                }
                Err(error) => assert!(
                    expected.is_some(),
                    "native positive premise failed: {error:?}"
                ),
            }
        };
        replay(parsed.facts().to_vec(), None);
        let owner = u64::try_from(SIMPLE.find("void launch").unwrap()).unwrap();
        let scalar = u64::try_from(SIMPLE.rfind("scalar").unwrap()).unwrap();
        let formal = u64::try_from(SIMPLE.find("scalar").unwrap()).unwrap();
        let shifted = |facts: &[SyntaxFact], selected: u64| {
            let mut changed = HashSet::from([selected]);
            loop {
                let previous = changed.len();
                for fact in facts {
                    if fact
                        .parent()
                        .is_some_and(|parent| changed.contains(&parent))
                    {
                        changed.insert(fact.local_id());
                    }
                }
                if previous == changed.len() {
                    break;
                }
            }
            facts
                .iter()
                .map(|fact| {
                    SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        fact.span(),
                        fact.depth() + usize::from(changed.contains(&fact.local_id())),
                        fact.syntax_kind().clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for witness in parsed.facts() {
            let label = witness.syntax_kind().as_str();
            let written = &SIMPLE.as_bytes()[usize::try_from(witness.span().start_byte()).unwrap()
                ..usize::try_from(witness.span().end_byte()).unwrap()];
            let relevant = witness.kind() == SyntaxFactKind::Root
                || matches!(
                    label,
                    "cuda.clean_file.signature"
                        | "cuda.scalar_read_target.signature"
                        | "cuda.kernel_launch_target.signature"
                )
                || label.ends_with(".function.declaration")
                || label.ends_with(".identifier.definition")
                    && matches!(written, b"launch" | b"kernel" | b"scalar")
                || label.ends_with(".parameters.signature") && witness.span().start_byte() >= owner
                || label == "cuda.owned_function.signature" && witness.span().start_byte() == owner
                || label.ends_with(".block.scope") && witness.span().start_byte() >= owner
                || witness.kind() == SyntaxFactKind::Declaration
                    && witness.span().start_byte() <= formal
                    && formal < witness.span().end_byte()
                || witness.kind() == SyntaxFactKind::Declaration
                    && witness.span().start_byte() <= scalar
                    && scalar < witness.span().end_byte()
                || label.ends_with(".identifier.reference")
                    && (witness.span().start_byte() == scalar || written == b"kernel");
            if relevant {
                replay(shifted(parsed.facts(), witness.local_id()), Some(2));
            }
        }
        for witness in parsed.facts().iter().filter(|fact| {
            fact.kind() == SyntaxFactKind::Declaration
                && fact
                    .syntax_kind()
                    .as_str()
                    .ends_with(".function.declaration")
        }) {
            let mut facts = shifted(parsed.facts(), witness.local_id());
            let root = parsed
                .facts()
                .iter()
                .find(|fact| fact.kind() == SyntaxFactKind::Root)
                .unwrap();
            let next = parsed
                .facts()
                .iter()
                .map(SyntaxFact::local_id)
                .max()
                .unwrap()
                + 1;
            facts.push(SyntaxFact::new(
                next,
                Some(root.local_id()),
                SyntaxFactKind::Scope,
                root.span(),
                1,
                rootlight_adapter_sdk::SyntaxKindLabel::new(&format!("{}.block.scope", case.name))
                    .unwrap(),
            ));
            let selected = facts
                .iter_mut()
                .find(|fact| fact.local_id() == witness.local_id())
                .unwrap();
            *selected = SyntaxFact::new(
                selected.local_id(),
                Some(next),
                selected.kind(),
                selected.span(),
                selected.depth(),
                selected.syntax_kind().clone(),
            );
            replay(facts, Some(2));
        }
    }
}
