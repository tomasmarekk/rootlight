//! C definition formals retain exact typed/pointer names and direct ownership.
//! Nested function-pointer and prototype names cannot borrow a neighboring body.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ResourceKind, SinkError,
    StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
    derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/c_parameters/callbacks.c");

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
    execute_controlled(
        raw,
        encoding,
        maximum,
        IrLimits::default(),
        &Cancellation::new(),
    )
}

fn execute_controlled(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    ir: IrLimits,
    cancel: &Cancellation,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.c"), raw).unwrap();
    let repository = derive_repository(b"c-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.c")).unwrap(),
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
            "c-source-contract",
            "1.0",
            content_hash(b"c-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("c").unwrap(),
        "c-static-contract",
        content_hash(b"c-contract-binary"),
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
        ir,
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("c").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"c-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required = provider.required_syntax_fact_count(&request.to_parse_request(), cancel)?;
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancel,
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
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
fn original_callback_formals_have_exact_names_and_function_ownership() {
    assert_eq!(SOURCE.len(), 146);
    let output = analyze(SOURCE);
    let owner_start = SOURCE.find("c_accept_cb").unwrap();
    let owner = entity_at(
        &output,
        "c_accept_cb",
        EntityKind::Function,
        owner_start,
        11,
    );
    for (name, anchor) in [
        ("loop", "*loop,"),
        ("watcher", "*watcher,"),
        ("revents", "int revents)"),
    ] {
        let start = SOURCE.find(anchor).unwrap() + anchor.find(name).unwrap();
        let parameter = entity_at(&output, name, EntityKind::Parameter, start, name.len());
        contains(&output, owner, parameter, start, start + name.len());
    }
}

#[test]
fn scalar_typedef_qualified_and_nested_pointer_formals_keep_written_leaf_names() {
    let source = "struct Node; typedef unsigned long Size;\nint forms(int scalar, Size size, const struct Node *node, char **buffer, int * const *rest) { return scalar; }\n";
    let output = analyze(source);
    let owner_start = source.find("forms(").unwrap();
    let owner = entity_at(&output, "forms", EntityKind::Function, owner_start, 5);
    for (name, anchor) in [
        ("scalar", "int scalar,"),
        ("size", "Size size,"),
        ("node", "*node,"),
        ("buffer", "**buffer,"),
        ("rest", "*rest)"),
    ] {
        let start = source.find(anchor).unwrap() + anchor.find(name).unwrap();
        let parameter = entity_at(&output, name, EntityKind::Parameter, start, name.len());
        contains(&output, owner, parameter, start, start + name.len());
    }
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Parameter)
            .count(),
        5
    );
}

#[test]
fn sibling_function_headers_and_pointer_return_headers_keep_direct_owners() {
    let source = "int first(int left) { return left; }\nint *second(int right) { return 0; }\n";
    let output = analyze(source);
    for (function, name) in [("first", "left"), ("second", "right")] {
        let start = source.find(&format!("{function}(")).unwrap();
        let owner = entity_at(
            &output,
            function,
            EntityKind::Function,
            start,
            function.len(),
        );
        let parameter_start = source[start..].find(name).unwrap() + start;
        let parameter = entity_at(
            &output,
            name,
            EntityKind::Parameter,
            parameter_start,
            name.len(),
        );
        contains(
            &output,
            owner,
            parameter,
            parameter_start,
            parameter_start + name.len(),
        );
    }
}

#[test]
fn unnamed_and_nested_callback_formals_do_not_donate_names_to_outer_body() {
    let source = "struct Record;\nint owner(int, struct Record *, int (*callback)(int inner), int actual) { return actual; }\nint external(int prototype);\n";
    let output = analyze(source);
    let start = source.find("owner(").unwrap();
    let owner = entity_at(&output, "owner", EntityKind::Function, start, 5);
    let actual_start = source.find("int actual").unwrap() + 4;
    let actual = entity_at(&output, "actual", EntityKind::Parameter, actual_start, 6);
    contains(&output, owner, actual, actual_start, actual_start + 6);
    for nested in output.document().entities.iter().filter(|e| {
        e.kind == EntityKind::Parameter
            && matches!(e.canonical_name.as_str(), "inner" | "prototype")
    }) {
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(owner)
                    && r.object == RelationEndpoint::Entity(nested.id))
        );
    }
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|e| e.kind == EntityKind::Parameter
                && matches!(e.canonical_name.as_str(), "int" | "Record"))
    );
}

#[test]
fn malformed_definition_header_cannot_publish_a_recovered_formal() {
    let source = "int broken(int ghost,, char *other) { return 0; }\n";
    let output = analyze(source);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|e| e.kind == EntityKind::Parameter)
    );
}

#[test]
fn original_callback_utf16_coordinates_retain_physical_formal_spans() {
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in SOURCE.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let start = SOURCE.find("c_accept_cb").unwrap();
        let owner = entity_at(
            &output,
            "c_accept_cb",
            EntityKind::Function,
            2 + 2 * start,
            22,
        );
        let parameter_start = SOURCE.find("*loop,").unwrap() + 1;
        let parameter = entity_at(
            &output,
            "loop",
            EntityKind::Parameter,
            2 + 2 * parameter_start,
            8,
        );
        contains(
            &output,
            owner,
            parameter,
            2 + 2 * parameter_start,
            2 + 2 * parameter_start + 8,
        );
    }
}

#[test]
fn ordinary_formal_identity_closure_preserves_existing_record_admission() {
    let (_, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    let (bounded, actual) = execute(SOURCE.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    let start = SOURCE.find("*loop,").unwrap() + 1;
    entity_at(&bounded, "loop", EntityKind::Parameter, start, 4);
    let refusal = execute(SOURCE.as_bytes(), "utf-8", required - 1)
        .expect_err("required formal captures cannot be silently dropped");
    assert_eq!(
        refusal,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
}

#[test]
fn original_callback_argument_reference_resolves_to_its_written_formal() {
    assert_eq!(SOURCE.len(), 146);
    assert_eq!(&SOURCE.as_bytes()[31..42], b"c_accept_cb");
    assert_eq!(&SOURCE.as_bytes()[59..63], b"loop");
    assert_eq!(&SOURCE.as_bytes()[115..119], b"loop");
    let output = analyze(SOURCE);
    let document = output.document();
    let owner = entity_at(&output, "c_accept_cb", EntityKind::Function, 31, 11);
    let formal = entity_at(&output, "loop", EntityKind::Parameter, 59, 4);
    contains(&output, owner, formal, 59, 63);
    let formal_record = document
        .entities
        .iter()
        .find(|entity| entity.id == formal)
        .unwrap();
    let owner_record = document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap();
    assert_eq!(
        formal_record.container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
    );
    assert_eq!(formal_record.language, "c");
    assert_eq!(formal_record.provenance, owner_record.provenance);
    let definition = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: formal })
                && site.source.span().start_byte() == 59
                && site.source.span().end_byte() == 63
        })
        .unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == 115 && site.source.span().end_byte() == 119
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact written argument occurrence");
    let site = sites[0];
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.repository, document.repository);
    assert_eq!(site.generation, document.generation);
    assert_eq!(site.file, document.files[0].id);
    assert_eq!(site.source.repository(), document.repository);
    assert_eq!(site.source.generation(), document.generation);
    assert_eq!(site.source.span().file(), document.files[0].id);
    assert_eq!(site.source.content_hash(), content_hash(SOURCE.as_bytes()));
    assert_eq!(site.source.content_hash(), definition.source.content_hash());
    assert_eq!(site.provenance, definition.provenance);
    assert_eq!(site.syntactic_text_hash, content_hash(b"loop"));
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|other| other.source.span() == site.source.span()
                && matches!(
                    other.role,
                    OccurrenceRole::Definition | OccurrenceRole::CallSite
                ))
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function
                && entity.canonical_name == "accept_cb")
    );
    assert_ne!(site.target, OccurrenceTarget::Resolved { symbol: owner });
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: formal },
        "the original argument must resolve to its current source-owned formal"
    );
}

fn resolved_argument(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    start: usize,
    end: usize,
    owner: rootlight_ids::SymbolId,
    formal: rootlight_ids::SymbolId,
    raw: &[u8],
    name: &str,
) {
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == end as u64
                && site.role == OccurrenceRole::Reference
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: formal });
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    assert_eq!(
        site.provenance,
        output
            .document()
            .entities
            .iter()
            .find(|e| e.id == formal)
            .unwrap()
            .provenance
    );
    let relations: Vec<_> = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(site.id)
                && relation.object == RelationEndpoint::Entity(formal)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].evidence.source.as_ref(), Some(&site.source));
    assert_eq!(relations[0].provenance, site.provenance);
}

#[test]
fn all_original_direct_arguments_keep_exact_native_formal_ownership() {
    let output = analyze(SOURCE);
    let owner = entity_at(&output, "c_accept_cb", EntityKind::Function, 31, 11);
    for (name, declaration, used) in [
        ("loop", 59, 115),
        ("watcher", 79, 121),
        ("revents", 92, 130),
    ] {
        assert_eq!(&SOURCE[declaration..declaration + name.len()], name);
        assert_eq!(&SOURCE[used..used + name.len()], name);
        let formal = entity_at(
            &output,
            name,
            EntityKind::Parameter,
            declaration,
            name.len(),
        );
        resolved_argument(
            &output,
            used,
            used + name.len(),
            owner,
            formal,
            SOURCE.as_bytes(),
            name,
        );
    }
}

#[test]
fn certified_formals_survive_exact_required_partition_and_refuse_one_less() {
    let (_, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    let (output, counted) = execute(SOURCE.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(counted, required);
    let owner = entity_at(&output, "c_accept_cb", EntityKind::Function, 31, 11);
    let formal = entity_at(&output, "loop", EntityKind::Parameter, 59, 4);
    resolved_argument(&output, 115, 119, owner, formal, SOURCE.as_bytes(), "loop");
    assert_eq!(
        execute(SOURCE.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
}

#[test]
fn direct_argument_bindings_use_physical_utf16_bom_and_unicode_offsets() {
    let text = format!("/* λ 😀 */\n{SOURCE}");
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let physical = |offset: usize| 2 + 2 * text[..offset].encode_utf16().count();
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let owner_start = text.find("c_accept_cb").unwrap();
        let owner = entity_at(
            &output,
            "c_accept_cb",
            EntityKind::Function,
            physical(owner_start),
            22,
        );
        let formal_start = text.find("*loop").unwrap() + 1;
        let formal = entity_at(
            &output,
            "loop",
            EntityKind::Parameter,
            physical(formal_start),
            8,
        );
        let used = text.rfind("loop").unwrap();
        resolved_argument(
            &output,
            physical(used),
            physical(used + 4),
            owner,
            formal,
            &raw,
            "loop",
        );
    }
}

#[test]
fn shadowed_directive_recovered_and_unsupported_bodies_keep_explicit_binding_gaps() {
    for source in [
        "void owned(int value) { int value; external(value); }",
        "void owned(int value) { { external(value); } }",
        "void owned(int value) { if (1) external(value); }",
        "void owned(int value, int value) { external(value); }",
        "void owned(int value[2]) { external(value); }",
        "#define value other\nvoid owned(int value) { external(value); }",
        "#if ACTIVE\nvoid owned(int value) { external(value); }\n#endif",
        "void owned(int value) { external(value);",
    ] {
        let output = analyze(source);
        let used = source.rfind("value").unwrap() as u64;
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.source.span().start_byte() == used
                    && site.role == OccurrenceRole::Reference
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
        assert!(output.document().skipped_regions.iter().any(|gap|
            gap.domain == rootlight_ir::FactDomain::Relations
            && (gap.detail == "c-formal-proof-is-limited-to-direct-arguments-in-a-single-clean-call-statement"
                || gap.detail == "c-native-written-formal-binding-unavailable")), "{source}");
    }
}

#[test]
fn full_body_context_never_changes_header_identity_or_resolves_the_external_callee() {
    let text = "void owned(int value) { external(value); }";
    let changed = text.replace("external", "another");
    let before = analyze(text);
    let after = analyze(&changed);
    for (name, kind) in [
        ("owned", EntityKind::Function),
        ("value", EntityKind::Parameter),
    ] {
        let left = before
            .document()
            .entities
            .iter()
            .find(|e| e.canonical_name == name && e.kind == kind)
            .unwrap();
        let right = after
            .document()
            .entities
            .iter()
            .find(|e| e.canonical_name == name && e.kind == kind)
            .unwrap();
        assert_eq!(left.id, right.id);
    }
    assert!(
        before
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let header = analyze(&text.replace("int value", "long value"));
    let owned = |output: &rootlight_adapter_sdk::AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .find(|e| e.kind == EntityKind::Function && e.canonical_name == "owned")
            .unwrap()
            .id
    };
    assert_ne!(owned(&before), owned(&header));
}

#[test]
fn resolved_formal_relations_and_records_obey_exact_transaction_limits_and_cancel() {
    for relations in [true, false] {
        let selected = |maximum| {
            let mut ir = IrLimits::default();
            if relations {
                ir.max_relations = maximum;
            } else {
                ir.max_total_records = maximum;
            }
            ir
        };
        let refusal = execute_controlled(
            SOURCE.as_bytes(),
            "utf-8",
            4096,
            selected(1),
            &Cancellation::new(),
        );
        let Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed,
            limit: 1,
        })) = refusal
        else {
            panic!("the real transaction must expose its record demand: {refusal:?}")
        };
        assert!(observed > 1);
        let output = execute_controlled(
            SOURCE.as_bytes(),
            "utf-8",
            4096,
            selected(observed),
            &Cancellation::new(),
        )
        .unwrap()
        .0;
        let owner = entity_at(&output, "c_accept_cb", EntityKind::Function, 31, 11);
        let formal = entity_at(&output, "loop", EntityKind::Parameter, 59, 4);
        resolved_argument(&output, 115, 119, owner, formal, SOURCE.as_bytes(), "loop");
        assert_eq!(
            execute_controlled(
                SOURCE.as_bytes(),
                "utf-8",
                4096,
                selected(observed - 1),
                &Cancellation::new()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::Records,
                observed,
                limit: observed - 1
            })
        );
    }
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        execute_controlled(
            SOURCE.as_bytes(),
            "utf-8",
            4096,
            IrLimits::default(),
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}
