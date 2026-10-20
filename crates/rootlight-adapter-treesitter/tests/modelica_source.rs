//! Modelica input Reads require native function scope and exact physical witnesses.
//! General class/equation/runtime semantics remain explicit gaps.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/modelica/U_sf.mo");

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    execute(raw, encoding, 4096).unwrap().0
}

fn execute(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.mo"), raw).unwrap();
    let repository = derive_repository(b"modelica-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.mo")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "modelica-source-contract",
            "1.0",
            content_hash(b"modelica-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("modelica").unwrap(),
        "modelica-static-contract",
        content_hash(b"modelica-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        maximum,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream,
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("modelica").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"modelica-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required =
        provider.required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())?;
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    Ok((result, required))
}

fn entity_at(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: usize,
    length: usize,
) -> rootlight_ids::SymbolId {
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                && occurrence.source.span().end_byte() == u64::try_from(start + length).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        definitions.len(),
        1,
        "{name}: {:?}",
        output.document().occurrences
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must be resolved")
    };
    let entity = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.display_name, name);
    assert_eq!(
        definitions[0].syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    symbol
}

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    member: rootlight_ids::SymbolId,
    name_start: usize,
    name_end: usize,
) {
    let matches = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1);
    let relation = matches[0];
    let reference = relation.evidence.source.as_ref().unwrap();
    assert!(reference.span().start_byte() <= u64::try_from(name_start).unwrap());
    assert!(reference.span().end_byte() >= u64::try_from(name_end).unwrap());
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn original_function_inputs_reads_and_contains_keep_all_finite_witnesses() {
    assert_eq!(SOURCE.len(), 238);
    let output = analyze(SOURCE);
    let function = entity_at(&output, "U_sf", EntityKind::Function, 40, 4);
    let unom = entity_at(&output, "Unom", EntityKind::Parameter, 115, 4);
    let mdot = entity_at(&output, "Mdot", EntityKind::Parameter, 135, 4);
    contains(&output, function, unom, 115, 119);
    for (start, target) in [(178, unom), (183, mdot)] {
        let reads: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.enclosing == Some(function)
                    && site.target == (OccurrenceTarget::Resolved { symbol: target })
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 4
            })
            .collect();
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0].source.content_hash(),
            content_hash(SOURCE.as_bytes())
        );
    }
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 231
                && site.source.span().end_byte() == 235)
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(unom)
                && edge.object == RelationEndpoint::Entity(function))
    );
}

#[test]
fn type_default_documentary_end_and_call_heads_do_not_donate_formal_or_read_proof() {
    let source = "function f\ninput Real x = fallback;\noutput Real y;\nalgorithm\ny := x(x);\nannotation(Documentation(info=\"function phantom input Real fake; end phantom;\"));\nend f;\n";
    let output = analyze(source);
    for name in ["Real", "fallback", "y", "phantom", "fake"] {
        assert!(!output.document().entities.iter().any(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == name));
    }
    let head = u64::try_from(source.find("x(x)").unwrap()).unwrap();
    assert!(
        !output.document().occurrences.iter().any(
            |site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == head
        )
    );
    let end = u64::try_from(source.rfind("f;").unwrap()).unwrap();
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == end)
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn duplicate_local_output_loop_and_redeclared_names_never_select_an_input_by_spelling() {
    for source in [
        "function f input Real x; input Real x; output Real y; algorithm y := x; end f;",
        "function f input Real x; Real x; output Real y; algorithm y := x; end f;",
        "function f input Real x; output Real x; algorithm x := x; end f;",
        "function f input Real x; output Real y; algorithm for x in 1:2 loop y := x; end for; end f;",
        "function f redeclare input Real x; output Real y; algorithm y := x; end f;",
    ] {
        let output = analyze(source);
        let offset = u64::try_from(source.rfind(":= x").unwrap() + 3).unwrap();
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read && site.source.span().start_byte() == offset
            })
            .collect();
        assert_eq!(sites.len(), 1, "{source}");
        assert!(
            !matches!(sites[0].target, OccurrenceTarget::Resolved { .. }),
            "{source}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "modelica-input-binding-unavailable"
                    && gap.source.span() == sites[0].source.span()),
            "{source}"
        );
    }
}

#[test]
fn separate_functions_case_and_body_edits_keep_formal_scope_and_stable_identity() {
    let source = "function f input Real x; output Real y; algorithm y := x; end f;\nfunction g input Real x; output Real y; algorithm y := x + X; end g;";
    let first = analyze(source);
    let second = analyze(&source.replace("x + X", "x * 2 + X"));
    for name in ["f", "g"] {
        let find = |output: &rootlight_adapter_sdk::AnalysisOutput| {
            output
                .document()
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
                .unwrap()
                .id
        };
        assert_eq!(find(&first), find(&second));
    }
    let formals = |output: &rootlight_adapter_sdk::AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .map(|entity| entity.id)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(formals(&first), formals(&second));
    let inputs: Vec<_> = first
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "x")
        .collect();
    assert_eq!(inputs.len(), 2);
    let reads: Vec<_> = first
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. })
        })
        .collect();
    assert_eq!(reads.len(), 2);
    assert_ne!(reads[0].target, reads[1].target);
    let upper = u64::try_from(source.rfind('X').unwrap()).unwrap();
    assert!(
        !first
            .document()
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == upper
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn model_class_package_member_and_malformed_files_have_explicit_noncomplete_diagnostics() {
    for source in [
        "model M Real member; equation member=1; end M;",
        "class C Real member; end C;",
        "package P model M Real member; end M; end P;",
        "function f input Real x; algorithm y := x; end other;",
        "function f input Real x; algorithm y := x; annotation(Documentation(info=\"\\</a>\")); end f;",
    ] {
        let output = analyze(source);
        assert!(output.document().skipped_regions.iter().any(|gap| gap.detail == "modelica-model-class-package-member-output-and-general-declarations-unavailable"));
        assert!(output.document().skipped_regions.iter().any(|gap| gap.detail == "modelica-import-inheritance-equation-unit-event-type-and-runtime-binding-unavailable"));
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.document().files[0].byte_length,
            u64::try_from(source.len()).unwrap()
        );
        assert_eq!(
            output.document().files[0].content_hash,
            content_hash(source.as_bytes())
        );
    }
}

#[test]
fn genuine_bom_and_both_utf16_orders_keep_raw_leaf_spans_and_file_hashes() {
    let text = format!("// prefix λ😀\r\n{SOURCE}");
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw: Vec<u8> = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()
        } else {
            let mut raw = if encoding == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                raw.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            raw
        };
        let output = analyze_raw(&raw, encoding);
        let prefix = text.find(SOURCE).unwrap();
        for (offset, name, role) in [
            (40, "U_sf", OccurrenceRole::Definition),
            (115, "Unom", OccurrenceRole::Definition),
            (135, "Mdot", OccurrenceRole::Definition),
            (178, "Unom", OccurrenceRole::Read),
            (183, "Mdot", OccurrenceRole::Read),
        ] {
            let written = prefix + offset;
            let start = if encoding == "utf-8" {
                3 + written
            } else {
                2 + 2 * text[..written].encode_utf16().count()
            };
            let width = if encoding == "utf-8" { 4 } else { 8 };
            let sites: Vec<_> = output
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == role
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte() == u64::try_from(start + width).unwrap()
                })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}: {name}");
            assert!(matches!(sites[0].target, OccurrenceTarget::Resolved { .. }));
            assert_eq!(sites[0].source.content_hash(), content_hash(&raw));
        }
        let written = prefix + 178;
        let start = if encoding == "utf-8" {
            3 + written
        } else {
            2 + 2 * text[..written].encode_utf16().count()
        };
        let width = if encoding == "utf-8" { 4 } else { 8 };
        let reads: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + width).unwrap()
            })
            .collect();
        assert_eq!(reads.len(), 1, "{encoding}");
        assert!(matches!(reads[0].target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(reads[0].source.content_hash(), content_hash(&raw));
        let expected: Vec<u8> = if encoding == "utf-8" {
            b"Unom".to_vec()
        } else {
            "Unom"
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        };
        assert_eq!(&raw[start..start + width], expected);
    }
}
