//! Go named input formals retain exact grouped names and direct native ownership.
//! Receiver, result, closure and function-type names cannot borrow an outer body.

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

const SOURCE: &str = include_str!("fixtures/go_parameters/bytes_reader.go");

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
    execute_observed(raw, encoding, maximum, |_, _| Ok(()))
}

fn execute_observed(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    observe: impl FnOnce(
        &TreeSitterProvider,
        &rootlight_adapter_sdk::ParseRequest<'_>,
    ) -> Result<(), AdapterError>,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    execute_context_observed(raw, encoding, maximum, |_, provider, request| {
        observe(provider, &request.to_parse_request())
    })
}

fn execute_context_observed(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    observe: impl FnOnce(
        &TreeSitterAnalyzer,
        &TreeSitterProvider,
        &AnalysisRequest<'_>,
    ) -> Result<(), AdapterError>,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.go"), raw).unwrap();
    let repository = derive_repository(b"go-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.go")).unwrap(),
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
            "go-source-contract",
            "1.0",
            content_hash(b"go-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("go").unwrap(),
        "go-static-contract",
        content_hash(b"go-contract-binary"),
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
        LanguageId::new("go").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"go-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required =
        provider.required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())?;
    observe(&analyzer, provider.as_ref(), &request)?;
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
fn original_bytes_reader_data_formal_has_exact_written_name_and_owner() {
    assert_eq!(SOURCE.len(), 970);
    let output = analyze(SOURCE);
    let start = SOURCE.find("NewBytesReader(").unwrap();
    let owner = entity_at(&output, "NewBytesReader", EntityKind::Function, start, 14);
    let parameter_start = SOURCE.find("NewBytesReader(data").unwrap() + "NewBytesReader(".len();
    let parameter = entity_at(&output, "data", EntityKind::Parameter, parameter_start, 4);
    contains(
        &output,
        owner,
        parameter,
        parameter_start,
        parameter_start + 4,
    );
}

#[test]
fn grouped_and_ordinary_type_containers_keep_each_written_formal() {
    let source = "package sample\ntype Item struct{}\nfunc forms(left, right int, values []byte, pointer *Item, buffer [4]byte, lookup map[string]int, stream <-chan string, callback func(inner int) int, rest ...string) {}\n";
    let output = analyze(source);
    let start = source.find("forms(").unwrap();
    let owner = entity_at(&output, "forms", EntityKind::Function, start, 5);
    for (name, anchor) in [
        ("left", "left,"),
        ("right", "right int"),
        ("values", "values []byte"),
        ("pointer", "pointer *Item"),
        ("buffer", "buffer [4]byte"),
        ("lookup", "lookup map"),
        ("stream", "stream <-chan"),
        ("callback", "callback func"),
        ("rest", "rest ...string"),
    ] {
        let start = source.find(anchor).unwrap();
        let parameter = entity_at(&output, name, EntityKind::Parameter, start, name.len());
        contains(&output, owner, parameter, start, start + name.len());
    }
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        9
    );
}

#[test]
fn sibling_named_functions_and_methods_keep_their_direct_formals() {
    let source = "package sample\ntype Receiver struct{}\nfunc first(left int) {}\nfunc (receiver *Receiver) second(right int) {}\n";
    let output = analyze(source);
    for (name, formal, kind) in [
        ("first", "left", EntityKind::Function),
        ("second", "right", EntityKind::Method),
    ] {
        let start = source.find(&format!("{name}(")).unwrap();
        let owner = entity_at(&output, name, kind, start, name.len());
        let formal_start = source[start..].find(formal).unwrap() + start;
        let parameter = entity_at(
            &output,
            formal,
            EntityKind::Parameter,
            formal_start,
            formal.len(),
        );
        contains(
            &output,
            owner,
            parameter,
            formal_start,
            formal_start + formal.len(),
        );
    }
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter
                && entity.canonical_name == "receiver")
    );
}

#[test]
fn unnamed_receiver_result_function_type_prototype_and_closure_names_cannot_donate() {
    let source = "package sample\ntype Receiver struct{}\ntype Callback func(typeName int) int\nfunc unnamed(int, *Receiver) {}\nfunc owner(actual int, callback func(inner int)) (result int) { _ = func(closure int) {}; return }\nfunc (receiver Receiver) method(actualMethod int) {}\nfunc external(prototype int)\n";
    let output = analyze(source);
    let mut names = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .map(|entity| entity.canonical_name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["actual", "actualMethod", "callback"]);
    let start = source.find("owner(").unwrap();
    let owner = entity_at(&output, "owner", EntityKind::Function, start, 5);
    for (name, anchor) in [("actual", "actual int"), ("callback", "callback func")] {
        let start = source.find(anchor).unwrap();
        let parameter = entity_at(&output, name, EntityKind::Parameter, start, name.len());
        contains(&output, owner, parameter, start, start + name.len());
    }
}

#[test]
fn malformed_native_header_cannot_publish_recovered_formals() {
    let output = analyze("package sample\nfunc broken(recovered int,, other int) {}\n");
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter)
    );
}

#[test]
fn utf16_grouped_formals_after_unicode_trivia_keep_physical_byte_spans() {
    let source = "// \u{2206}\r\npackage sample\r\nfunc owned(first, second int) {}\r\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let physical = |offset| 2 + source[..offset].encode_utf16().count() * 2;
        let owner = entity_at(
            &output,
            "owned",
            EntityKind::Function,
            physical(source.find("owned(").unwrap()),
            10,
        );
        for name in ["first", "second"] {
            let start = physical(source.find(name).unwrap());
            let length = name.encode_utf16().count() * 2;
            let parameter = entity_at(&output, name, EntityKind::Parameter, start, length);
            contains(&output, owner, parameter, start, start + length);
        }
    }
}

#[test]
fn formal_identity_closure_preserves_exact_required_fact_admission() {
    let (_, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, actual) = execute(SOURCE.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    let start = SOURCE.find("NewBytesReader(data").unwrap() + "NewBytesReader(".len();
    entity_at(&bounded, "data", EntityKind::Parameter, start, 4);
    assert_eq!(
        execute(SOURCE.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
}

fn certified_go_formal_read(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    parameter: rootlight_ids::SymbolId,
    start: usize,
    end: usize,
    raw: &[u8],
) {
    let document = output.document();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact authored formal read {start}..{end}");
    let site = sites[0];
    assert_eq!(site.syntax_kind, "go.composite_formal_value.reference");
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: parameter },
        "site={site:?}; diagnostics={:?}; gaps={:?}",
        document.diagnostics,
        document.skipped_regions
    );
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(
        site.syntactic_text_hash,
        content_hash(
            document
                .entities
                .iter()
                .find(|entity| entity.id == parameter)
                .unwrap()
                .canonical_name
                .as_bytes()
        )
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(parameter)
                && edge.evidence.source.as_ref() == Some(&site.source))
            .count(),
        1
    );
}

#[test]
fn original_bytes_reader_return_value_resolves_its_own_data_formal() {
    assert_eq!(SOURCE.len(), 970);
    let output = analyze(SOURCE);
    let function = SOURCE.find("NewBytesReader(").unwrap();
    let owner = entity_at(
        &output,
        "NewBytesReader",
        EntityKind::Function,
        function,
        14,
    );
    let formal_start = function + "NewBytesReader(".len();
    let formal = entity_at(&output, "data", EntityKind::Parameter, formal_start, 4);
    let value = SOURCE.find("data: data").unwrap() + 6;
    certified_go_formal_read(&output, owner, formal, value, value + 4, SOURCE.as_bytes());
    let key = value - 6;
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == key as u64
                && site.target == OccurrenceTarget::Resolved { symbol: formal })
    );
}

#[test]
fn grouped_and_positional_literal_values_bind_only_their_own_function_formals() {
    let source = "package sample\ntype Box struct { first, second int }\nfunc one(left, right int) Box { return Box{first: left, second: right} }\nfunc two(left int) []int { return []int{left} }\n";
    let output = analyze(source);
    let mut symbols = Vec::new();
    for (function, name, anchor) in [
        ("one", "left", "first: left"),
        ("one", "right", "second: right"),
        ("two", "left", "[]int{left}"),
    ] {
        let start = source.find(&format!("{function}(")).unwrap();
        let owner = entity_at(
            &output,
            function,
            EntityKind::Function,
            start,
            function.len(),
        );
        let formal_start = start + source[start..].find(name).unwrap();
        let formal = entity_at(
            &output,
            name,
            EntityKind::Parameter,
            formal_start,
            name.len(),
        );
        let value = source.find(anchor).unwrap() + anchor.rfind(name).unwrap();
        certified_go_formal_read(
            &output,
            owner,
            formal,
            value,
            value + name.len(),
            source.as_bytes(),
        );
        symbols.push(formal);
    }
    assert_ne!(symbols[0], symbols[2]);
}

#[test]
fn keys_other_owners_receivers_results_closures_and_dynamic_values_do_not_receive_certificates() {
    for source in [
        "package sample\nfunc owned(data []byte) *Box { return &Box{data: nil} }\n",
        "package sample\nfunc first(data []byte) {}\nfunc second(other int) *Box { return &Box{data: data} }\n",
        "package sample\nfunc (receiver Box) owned(data []byte) *Box { return &Box{data: data} }\n",
        "package sample\nfunc owned(data []byte) (result *Box) { return &Box{data: data} }\n",
        "package sample\nfunc owned(data []byte) *Box { local := data; return &Box{data: data} }\n",
        "package sample\nfunc owned(data []byte) *Box { return &Box{data: func() []byte { return data }()} }\n",
        "package sample\nfunc owned(data []byte) *Box { return &Box{data: external(data)} }\n",
        "package sample\nfunc owned(callback func(data []byte)) *Box { return &Box{data: data} }\n",
        "package sample\nimport alias \"external\"\nfunc owned(data []byte) *Box { return &Box{data: alias.value} }\n",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "go.composite_formal_value.reference"),
            "{source}"
        );
    }
}

#[test]
fn original_return_value_after_unicode_trivia_keeps_utf16_physical_provenance() {
    let source = format!("// \u{2206}\r\n{SOURCE}");
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let physical = |offset| 2 + source[..offset].encode_utf16().count() * 2;
        let function = source.find("NewBytesReader(").unwrap();
        let owner = entity_at(
            &output,
            "NewBytesReader",
            EntityKind::Function,
            physical(function),
            28,
        );
        let formal_start = physical(function + "NewBytesReader(".len());
        let formal = entity_at(&output, "data", EntityKind::Parameter, formal_start, 8);
        let value = physical(source.find("data: data").unwrap() + 6);
        certified_go_formal_read(&output, owner, formal, value, value + 8, &raw);
    }
}

#[test]
fn body_edits_do_not_enter_formal_identity_or_reuse_an_old_read() {
    let source = "package sample\ntype Box struct { first, second int }\nfunc owned(value int) Box { return Box{first: value} }\n";
    let changed = source.replace("first: value", "second: value");
    let original = analyze(source);
    let next = analyze(&changed);
    let start = source.find("value int").unwrap();
    let formal = entity_at(&original, "value", EntityKind::Parameter, start, 5);
    assert_eq!(
        entity_at(&next, "value", EntityKind::Parameter, start, 5),
        formal
    );
    let owner = entity_at(
        &next,
        "owned",
        EntityKind::Function,
        changed.find("owned(").unwrap(),
        5,
    );
    let value = changed.find("second: value").unwrap() + 8;
    certified_go_formal_read(&next, owner, formal, value, value + 5, changed.as_bytes());
}

#[test]
fn required_formal_scope_cannot_be_silently_lost_at_exact_admission() {
    let (full, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    assert!(full.document().skipped_regions.iter().any(|gap| gap.domain == rootlight_ir::FactDomain::Relations && gap.detail == "go-formal-proof-is-limited-to-ascii-free-functions-and-direct-single-return-literal-values"));
    let (bounded, actual) = execute(SOURCE.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    assert!(bounded.document().skipped_regions.iter().any(|gap| gap.detail == "go-formal-proof-is-limited-to-ascii-free-functions-and-direct-single-return-literal-values"));
    assert_eq!(
        execute(SOURCE.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
}

#[test]
fn original_desktop_split_argument_resolves_to_its_written_value_formal() {
    let raw = include_bytes!("fixtures/desktop_source93/original-parser.go");
    assert_eq!(raw.len(), 1522);
    assert_eq!(&raw[1093..1098], b"value");
    assert_eq!(&raw[1180..1185], b"value");
    let output = analyze_raw(raw, "utf-8");
    let owner = entity_at(&output, "splitList", EntityKind::Function, 1083, 9);
    let parameter = entity_at(&output, "value", EntityKind::Parameter, 1093, 5);
    contains(&output, owner, parameter, 1093, 1098);
    let sites = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1180
                && site.source.span().end_byte() == 1185
        })
        .collect::<Vec<_>>();
    assert_eq!(sites.len(), 1, "exact original direct argument reference");
    let site = sites[0];
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.syntactic_text_hash, content_hash(b"value"));
    assert!(!output.document().occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 1180
            && site.source.span().end_byte() == 1185
    }));
    let other = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "parseBool")
        .unwrap();
    assert_ne!(site.enclosing, Some(other.id));
}

fn direct_argument_site(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    formal: rootlight_ids::SymbolId,
    start: usize,
    end: usize,
    raw: &[u8],
) {
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.syntax_kind, "go.call_formal.reference");
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: formal });
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    assert_eq!(
        site.provenance,
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == formal)
            .unwrap()
            .provenance
    );
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::RefersTo
                    && relation.subject == RelationEndpoint::Occurrence(site.id)
                    && relation.object == RelationEndpoint::Entity(formal)
                    && relation.evidence.source.as_ref() == Some(&site.source)
            })
            .count(),
        1
    );
}

#[test]
fn direct_arguments_use_closed_function_scopes_without_inventing_external_calls() {
    for source in [
        "package sample\nfunc owned(value int) { external(value) }\n",
        "package sample\nfunc owned(value int) { if other := 0; other == 0 { external(value) } }\n",
        "package sample\nfunc owned(value int) { local := 1; for _, item := range items { local = item; external(value) }; _ = local }\n",
    ] {
        let output = analyze(source);
        let owner = entity_at(
            &output,
            "owned",
            EntityKind::Function,
            source.find("owned").unwrap(),
            5,
        );
        let formal = entity_at(
            &output,
            "value",
            EntityKind::Parameter,
            source.find("value int").unwrap(),
            5,
        );
        let start = source.find("external(value)").unwrap() + "external(".len();
        direct_argument_site(&output, owner, formal, start, start + 5, source.as_bytes());
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "external")
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn direct_arguments_refuse_if_short_range_inner_function_and_other_binding_barriers() {
    for source in [
        "package sample\nfunc owned(value int) { value := 0; external(value) }\n",
        "package sample\nfunc owned(value int) { if value := 0; value == 0 { external(value) } }\n",
        "package sample\nfunc owned(value int) { for _, value := range items { external(value) } }\n",
        "package sample\nfunc owned(value int) { nested := func(value int) { external(value) }; _ = nested }\n",
        "package sample\nfunc owned(value int) { var value int; external(value) }\n",
        "package sample\nfunc owned(value int) { type value int; external(value) }\n",
        "package sample\nfunc owned(value int) { value++; external(value) }\n",
        "package sample\nfunc owned(value, value int) { external(value) }\n",
        "package sample\nfunc (receiver Box) owned(value int) { external(value) }\n",
        "package sample\nfunc owned(value int) (result int) { external(value); return }\n",
        "package sample\nfunc owned[T any](value T) { external(value) }\n",
        "package sample\nfunc owned(value int) { external(value) }\nfunc owned(value int) { external(value) }\n",
        "package sample\nfunc owned(value int,,) { external(value) }\n",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "go.call_formal.reference"),
            "{source}"
        );
    }
}

#[test]
fn actual_native_call_protocol_and_required_partition_keep_original_formal_witnesses() {
    let raw = include_bytes!("fixtures/desktop_source93/original-parser.go");
    let inspect = |provider: &TreeSitterProvider,
                   request: &rootlight_adapter_sdk::ParseRequest<'_>| {
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider,
            request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )?;
        let count = |kind, label: &str, start, end| {
            parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == kind
                        && fact.syntax_kind().as_str() == label
                        && fact.span().start_byte() == start
                        && fact.span().end_byte() == end
                })
                .count()
        };
        use rootlight_adapter_sdk::SyntaxFactKind;
        assert_eq!(count(SyntaxFactKind::Root, "go.file.root", 0, 1522), 1);
        assert_eq!(
            count(
                SyntaxFactKind::Declaration,
                "go.parameter.declaration",
                1093,
                1098
            ),
            1
        );
        assert_eq!(
            count(
                SyntaxFactKind::Occurrence,
                "go.identifier.definition",
                1093,
                1098
            ),
            1
        );
        assert_eq!(
            count(
                SyntaxFactKind::Occurrence,
                "go.call_formal.reference",
                1180,
                1185
            ),
            1
        );
        assert!(
            parsed
                .facts()
                .iter()
                .any(|fact| fact.kind() == SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str() == "go.call_formal_context.signature"
                    && fact.span().start_byte() < 1083
                    && fact.span().end_byte() > 1185)
        );
        assert!(
            !parsed
                .facts()
                .iter()
                .any(|fact| fact.kind() == SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str() == "go.call_formal.reference")
        );
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            rootlight_adapter_sdk::execute_parse(
                provider,
                request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        Ok(())
    };
    let (_, required) = execute_observed(raw, "utf-8", 4096, inspect).unwrap();
    let (bounded, counted) = execute_observed(raw, "utf-8", required, inspect).unwrap();
    assert_eq!(counted, required);
    let owner = entity_at(&bounded, "splitList", EntityKind::Function, 1083, 9);
    let formal = entity_at(&bounded, "value", EntityKind::Parameter, 1093, 5);
    direct_argument_site(&bounded, owner, formal, 1180, 1185, raw);
    assert_eq!(
        execute(raw, "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
}

#[test]
fn direct_argument_proof_uses_both_utf16_physical_source_coordinates() {
    let original = include_str!("fixtures/desktop_source93/original-parser.go");
    let source = format!("// λ 😀\r\n{original}");
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let physical = |offset| 2 + source[..offset].encode_utf16().count() * 2;
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let owner = entity_at(
            &output,
            "splitList",
            EntityKind::Function,
            physical(source.find("splitList").unwrap()),
            18,
        );
        let parameter = physical(source.find("splitList(value").unwrap() + "splitList(".len());
        let formal = entity_at(&output, "value", EntityKind::Parameter, parameter, 10);
        let reference = physical(source.find("Split(value").unwrap() + "Split(".len());
        direct_argument_site(&output, owner, formal, reference, reference + 10, &raw);
    }
}

#[test]
fn direct_argument_body_changes_do_not_enter_formal_identity_or_old_read_provenance() {
    let source = "package sample\nfunc owned(value int) { external(value) }\n";
    let changed = source.replace("external(value)", "other(value)");
    let first = analyze(source);
    let next = analyze(&changed);
    let owner = entity_at(
        &first,
        "owned",
        EntityKind::Function,
        source.find("owned").unwrap(),
        5,
    );
    let next_owner = entity_at(
        &next,
        "owned",
        EntityKind::Function,
        changed.find("owned").unwrap(),
        5,
    );
    assert_eq!(owner, next_owner);
    let formal = entity_at(
        &first,
        "value",
        EntityKind::Parameter,
        source.find("value int").unwrap(),
        5,
    );
    let next_formal = entity_at(
        &next,
        "value",
        EntityKind::Parameter,
        changed.find("value int").unwrap(),
        5,
    );
    assert_eq!(formal, next_formal);
    let start = changed.find("other(value)").unwrap() + "other(".len();
    direct_argument_site(
        &next,
        next_owner,
        next_formal,
        start,
        start + 5,
        changed.as_bytes(),
    );
}

#[test]
fn separate_function_arguments_never_borrow_another_written_formal() {
    let source = "package sample\nfunc first(value int) { external(value) }\nfunc second(value int) { external(value) }\n";
    let output = analyze(source);
    let mut formals = Vec::new();
    for (name, body) in [
        ("first", source.find("func first").unwrap()),
        ("second", source.find("func second").unwrap()),
    ] {
        let start = body + source[body..].find(name).unwrap();
        let owner = entity_at(&output, name, EntityKind::Function, start, name.len());
        let parameter = body + source[body..].find("value int").unwrap();
        let formal = entity_at(&output, "value", EntityKind::Parameter, parameter, 5);
        let reference = body + source[body..].find("external(value)").unwrap() + "external(".len();
        direct_argument_site(
            &output,
            owner,
            formal,
            reference,
            reference + 5,
            source.as_bytes(),
        );
        formals.push(formal);
    }
    assert_ne!(formals[0], formals[1]);
}

#[test]
fn captured_call_argument_artifact_matches_native_required_admission_and_rebinds() {
    let raw = include_bytes!("fixtures/desktop_source93/original-parser.go");
    execute_context_observed(raw, "utf-8", 4096, |analyzer, provider, request| {
        let cancel = Cancellation::new();
        let required = provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
        let artifact = analyzer.prepare_artifact(request, &cancel)?;
        // These independent public API counts must agree for the same native output.
        assert_eq!(artifact.required_syntax_fact_count(&cancel)?, required);
        let limits_for = |capacity| {
            let old = request.limits();
            let syntax = old.syntax_stream();
            let batch = syntax.batch();
            AnalysisLimits::new(
                old.max_source_bytes(), old.max_syntax_nodes(), old.max_syntax_depth(),
                old.max_embedded_ranges(), old.max_reported_memory_bytes(),
                StreamLimits::new(
                    syntax.max_batches(), capacity, syntax.max_output_bytes(), syntax.max_diagnostics(),
                    syntax.max_diagnostic_bytes(), syntax.max_string_bytes(),
                    BatchThresholds::new(capacity.min(batch.max_records()), batch.max_output_bytes(),
                        batch.max_diagnostics(), batch.max_diagnostic_bytes()).unwrap(),
                ).unwrap(), old.ir_stream().clone(), old.ir().clone(),
            ).unwrap()
        };
        let exact_limits = limits_for(required);
        let exact = AnalysisRequest::new_with_parse_context(
            request.source().clone(), request.language().clone(), request.encoding().clone(),
            Vec::new(), AnalysisTier::TierD, request.build_context(), &exact_limits,
        ).unwrap().with_generated_status(false);
        let exact_artifact = analyzer.prepare_artifact(&exact, &cancel)?;
        assert_eq!(exact_artifact.required_syntax_fact_count(&cancel)?, required);
        assert_eq!(exact_artifact.syntax_fact_count(), required);
        let fresh = execute_analysis(analyzer, &exact, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel)?;
        let replayed = analyzer.analyze_from_artifact(&exact, &exact_artifact,
            ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel)?;
        assert_eq!(replayed.document(), fresh.document());
        let owner = entity_at(&fresh, "splitList", EntityKind::Function, 1083, 9);
        let formal = entity_at(&fresh, "value", EntityKind::Parameter, 1093, 5);
        direct_argument_site(&fresh, owner, formal, 1180, 1185, raw);

        let small_limits = limits_for(required - 1);
        let small = AnalysisRequest::new_with_parse_context(
            request.source().clone(), request.language().clone(), request.encoding().clone(),
            Vec::new(), AnalysisTier::TierD, request.build_context(), &small_limits,
        ).unwrap().with_generated_status(false);
        assert_eq!(analyzer.prepare_artifact(&small, &cancel).unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts, observed: required, limit: required - 1,
            }));

        let previous = request.source().source_ref();
        let next_source = SourceRef::new(previous.repository(), GenerationId::from_bytes([10; 20]),
            previous.span(), previous.content_hash(), None);
        let next = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(request.source().snapshot(), &next_source).unwrap(),
            request.language().clone(), request.encoding().clone(), Vec::new(), AnalysisTier::TierD,
            request.build_context(), &exact_limits,
        ).unwrap().with_generated_status(false);
        let next_fresh = execute_analysis(analyzer, &next, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel)?;
        let next_replay = analyzer.analyze_from_artifact(&next, &exact_artifact,
            ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel)?;
        assert_eq!(next_replay.document(), next_fresh.document());
        let next_owner = entity_at(&next_replay, "splitList", EntityKind::Function, 1083, 9);
        let next_formal = entity_at(&next_replay, "value", EntityKind::Parameter, 1093, 5);
        direct_argument_site(&next_replay, next_owner, next_formal, 1180, 1185, raw);
        for site in &next_replay.document().occurrences {
            assert_eq!(site.generation, next_source.generation());
            assert_eq!(site.source.generation(), next_source.generation());
            assert_eq!(site.source.content_hash(), next_source.content_hash());
        }

        let edited = [b"// new source\n".as_slice(), raw.as_slice()].concat();
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("source.go"), &edited).unwrap();
        let root = RepositoryRoot::open(previous.repository(), directory.path()).unwrap();
        let snapshot = root.snapshot(&RelativePath::parse(Path::new("source.go")).unwrap(), 1024 * 1024).unwrap();
        let changed_source = SourceRef::new(previous.repository(), next_source.generation(),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(edited.len()).unwrap()).unwrap(),
            snapshot.content_hash(), None);
        let changed = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &changed_source).unwrap(), request.language().clone(),
            request.encoding().clone(), Vec::new(), AnalysisTier::TierD, request.build_context(), &exact_limits,
        ).unwrap().with_generated_status(false);
        assert!(matches!(analyzer.analyze_from_artifact(&changed, &exact_artifact,
            ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancel),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch"));

        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(exact_artifact.required_syntax_fact_count(&cancelled),
            Err(AdapterError::Cancelled { .. })));
        assert!(matches!(analyzer.analyze_from_artifact(&next, &exact_artifact,
            ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &cancelled),
            Err(AdapterError::Cancelled { .. })));
        Ok(())
    }).unwrap();
}
const WRITTEN_CALLS: &str = include_str!("fixtures/go_written_calls/time.go");

fn written_go_call(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    target: rootlight_ids::SymbolId,
    start: usize,
    end: usize,
    raw: &[u8],
) {
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "go.direct_file_function.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact written bare callee {start}..{end}");
    let site = sites[0];
    assert_eq!(site.role, OccurrenceRole::CallSite);
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
    assert_eq!(site.source.content_hash(), content_hash(raw));
    let relations: Vec<_> = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(target)
                && relation.evidence.source.as_ref() == Some(&site.source)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    assert_eq!(
        relations[0].id,
        derive_relation_record_id(relations[0]).unwrap()
    );
    for parent in [
        rootlight_ir::FactRef::Fact(site.id),
        rootlight_ir::FactRef::Entity(owner),
        rootlight_ir::FactRef::Entity(target),
    ] {
        assert!(relations[0].evidence.derivation.contains(&parent));
    }
}

#[test]
fn original_time_file_keeps_all_three_written_function_dependencies() {
    assert_eq!(WRITTEN_CALLS.len(), 1478);
    let output = analyze(WRITTEN_CALLS);
    let human_time = entity_at(&output, "HumanTime", EntityKind::Function, 993, 9);
    let lower = entity_at(&output, "HumanTimeLower", EntityKind::Function, 1083, 14);
    let internal = entity_at(&output, "humanTime", EntityKind::Function, 1195, 9);
    let duration = entity_at(&output, "humanDuration", EntityKind::Function, 185, 13);
    for (owner, target, start, end) in [
        (human_time, internal, 1051, 1060),
        (lower, internal, 1162, 1171),
        (internal, duration, 1398, 1411),
    ] {
        written_go_call(&output, owner, target, start, end, WRITTEN_CALLS.as_bytes());
    }
    let later = WRITTEN_CALLS.rfind("humanDuration(delta)").unwrap();
    written_go_call(
        &output,
        internal,
        duration,
        later,
        later + 13,
        WRITTEN_CALLS.as_bytes(),
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "go-call-complete-semantic-validation-unavailable")
    );
}

#[test]
fn direct_written_calls_allow_forward_declarations_nested_arguments_and_recursion() {
    for source in [
        "package sample\nfunc caller() int { return outer(target()) }; func target() int { return 1 }\n",
        "package sample\nimport \"unrelated/path/target\"\nfunc caller() int { return target() }; func target() int { return 1 }\n",
        "package sample\nfunc target() int { return target() }\n",
    ] {
        let output = analyze(source);
        let owner_name = if source.contains("caller") {
            "caller"
        } else {
            "target"
        };
        let owner_start = source.find(&format!("func {owner_name}")).unwrap() + 5;
        let target_start = source.rfind("func target").unwrap() + 5;
        let site =
            source.find("return ").unwrap() + 7 + if source.contains("outer(") { 6 } else { 0 };
        let owner = entity_at(
            &output,
            owner_name,
            EntityKind::Function,
            owner_start,
            owner_name.len(),
        );
        let target = entity_at(&output, "target", EntityKind::Function, target_start, 6);
        written_go_call(&output, owner, target, site, site + 6, source.as_bytes());
    }
}

#[test]
fn lexical_shadow_dot_import_collision_method_and_function_value_calls_stay_unproved() {
    for source in [
        "package sample\nfunc target(){}; func caller(target func()){target()}\n",
        "package sample\nfunc target(){}; func caller(){target := external; target()}\n",
        "package sample\nfunc target(){}; func caller(){var target func(); target()}\n",
        "package sample\nfunc target(){}; func caller(){for target := range values {target()}}\n",
        "package sample\nfunc target(){}; func caller(){switch target := value.(type) {default: target()}}\n",
        "package sample\nfunc target(){}; func caller(){select {case target := <-channel: target()}}\n",
        "package sample\nfunc target(){}; func caller()(target func()){target(); return}\n",
        "package sample\nimport . \"external\"\nfunc target(){}; func caller(){target()}\n",
        "package sample\nimport target \"external\"\nfunc target(){}; func caller(){target()}\n",
        "package sample\nfunc target(){}; var target func(); func caller(){target()}\n",
        "package sample\nfunc target(){}; type target int; func caller(){target()}\n",
        "package sample\nfunc target(){}; func target(){}; func caller(){target()}\n",
        "package sample\nfunc target(){}; func caller(){func(){target()}()}\n",
        "package sample\nfunc target[T any](){}; func caller(){target()}\n",
        "package sample\nfunc target(){}; func (r Item) caller(){target()}\n",
        "package sample\nfunc caller(){variable()}; var variable func()\n",
        "package sample\nfunc init(){}; func caller(){init()}\n",
        "package sample\nfunc target(){}; func caller(){receiver.target()}\n",
        "package sample\nfunc target(){}; func caller(){ target( }\n",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(
                    |site| site.syntax_kind == "go.direct_file_function.reference"
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                ),
            "{source}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
}

#[test]
fn original_written_calls_keep_physical_bom_and_utf16_coordinates() {
    for (encoding, little_endian) in [("utf-16le", true), ("utf-16be", false)] {
        let raw: Vec<u8> = std::iter::once(0xfeff)
            .chain(WRITTEN_CALLS.encode_utf16())
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        let output = analyze_raw(&raw, encoding);
        let owner = entity_at(
            &output,
            "HumanTimeLower",
            EntityKind::Function,
            2 + 1083 * 2,
            28,
        );
        let target = entity_at(&output, "humanTime", EntityKind::Function, 2 + 1195 * 2, 18);
        written_go_call(&output, owner, target, 2 + 1162 * 2, 2 + 1171 * 2, &raw);
    }
    let raw = [b"\xef\xbb\xbf".as_slice(), WRITTEN_CALLS.as_bytes()].concat();
    let output = analyze_raw(&raw, "utf-8");
    let owner = entity_at(&output, "HumanTime", EntityKind::Function, 3 + 993, 9);
    let target = entity_at(&output, "humanTime", EntityKind::Function, 3 + 1195, 9);
    written_go_call(&output, owner, target, 3 + 1051, 3 + 1060, &raw);
}

#[test]
fn written_function_calls_agree_at_exact_provider_artifact_admission_and_one_less() {
    execute_context_observed(
        WRITTEN_CALLS.as_bytes(),
        "utf-8",
        4096,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let required =
                provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
            let artifact = analyzer.prepare_artifact(request, &cancel)?;
            assert_eq!(artifact.required_syntax_fact_count(&cancel)?, required);
            let syntax = request.limits().syntax_stream();
            let for_capacity = |capacity| {
                AnalysisLimits::new(
                    request.limits().max_source_bytes(),
                    request.limits().max_syntax_nodes(),
                    request.limits().max_syntax_depth(),
                    request.limits().max_embedded_ranges(),
                    request.limits().max_reported_memory_bytes(),
                    StreamLimits::new(
                        syntax.max_batches(),
                        capacity,
                        syntax.max_output_bytes(),
                        syntax.max_diagnostics(),
                        syntax.max_diagnostic_bytes(),
                        syntax.max_string_bytes(),
                        BatchThresholds::new(
                            capacity.min(syntax.batch().max_records()),
                            syntax.batch().max_output_bytes(),
                            syntax.batch().max_diagnostics(),
                            syntax.batch().max_diagnostic_bytes(),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    request.limits().ir_stream().clone(),
                    request.limits().ir().clone(),
                )
                .unwrap()
            };
            let exact_limits = for_capacity(required);
            let exact = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &exact_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let exact_artifact = analyzer.prepare_artifact(&exact, &cancel)?;
            assert_eq!(
                exact_artifact.required_syntax_fact_count(&cancel)?,
                required
            );
            assert_eq!(exact_artifact.syntax_fact_count(), required);
            let output = execute_analysis(
                analyzer,
                &exact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let replay = analyzer.analyze_from_artifact(
                &exact,
                &exact_artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(output.document(), replay.document());
            let owner = entity_at(&output, "HumanTime", EntityKind::Function, 993, 9);
            let target = entity_at(&output, "humanTime", EntityKind::Function, 1195, 9);
            written_go_call(&output, owner, target, 1051, 1060, WRITTEN_CALLS.as_bytes());
            let small_limits = for_capacity(required - 1);
            let small = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &small_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let expected = AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            });
            assert_eq!(
                analyzer.prepare_artifact(&small, &cancel).unwrap_err(),
                expected
            );
            // Preflight measures demand independently of output admission.
            assert_eq!(
                provider.required_syntax_fact_count(&small.to_parse_request(), &cancel)?,
                required
            );
            assert_eq!(
                rootlight_adapter_sdk::execute_parse(
                    provider,
                    &small.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
                .unwrap_err(),
                expected
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn duplicate_missing_foreign_and_wrong_owner_native_witnesses_cannot_donate_calls() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, SyntaxFactKind, SyntaxKindLabel, execute_parse,
        testkit::MockParseProvider,
    };
    execute_context_observed(
        WRITTEN_CALLS.as_bytes(),
        "utf-8",
        4096,
        |_, provider, request| {
            let cancel = Cancellation::new();
            let parsed = execute_parse(
                provider,
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let replay = |facts| {
                let mock = MockParseProvider::new(
                    provider.capabilities().clone(),
                    facts,
                    parsed.diagnostics().to_vec(),
                    parsed.report().coverage().clone(),
                );
                let analyzer = TreeSitterAnalyzer::new(
                    Arc::new(mock),
                    ProducerIdentity::new(
                        "go-call-witness",
                        "1.0",
                        content_hash(b"go-call-witness"),
                    )
                    .unwrap(),
                    LanguageId::new("go").unwrap(),
                    "go-written-call-replay",
                    content_hash(b"go-call-replay"),
                )
                .unwrap();
                execute_analysis(
                    &analyzer,
                    request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
            };
            let baseline = replay(parsed.facts().to_vec())?;
            let owner = entity_at(&baseline, "HumanTime", EntityKind::Function, 993, 9);
            let target = entity_at(&baseline, "humanTime", EntityKind::Function, 1195, 9);
            written_go_call(
                &baseline,
                owner,
                target,
                1051,
                1060,
                WRITTEN_CALLS.as_bytes(),
            );
            let declaration = parsed
                .facts()
                .iter()
                .find(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "go.function.declaration"
                        && fact.span().start_byte() < 1195
                        && fact.span().end_byte() > 1411
                })
                .unwrap();
            let definition = parsed
                .facts()
                .iter()
                .find(|fact| {
                    fact.parent() == Some(declaration.local_id())
                        && fact.syntax_kind().as_str() == "go.identifier.definition"
                })
                .unwrap();
            let context = parsed
                .facts()
                .iter()
                .find(|fact| {
                    fact.kind() == SyntaxFactKind::Signature
                        && fact.span() == declaration.span()
                        && matches!(
                            fact.syntax_kind().as_str(),
                            "go.call_formal_context.signature"
                                | "go.composite_formal_context.signature"
                        )
                })
                .unwrap();
            let root = parsed
                .facts()
                .iter()
                .find(|fact| fact.kind() == SyntaxFactKind::Root)
                .unwrap();
            for witness in [declaration, definition, context] {
                for mode in 0..3 {
                    let mut facts = parsed.facts().to_vec();
                    if mode == 0 {
                        facts.retain(|fact| fact.local_id() != witness.local_id());
                        for child in &mut facts {
                            if child.parent() == Some(witness.local_id()) {
                                *child = SyntaxFact::new(
                                    child.local_id(),
                                    witness.parent(),
                                    child.kind(),
                                    child.span(),
                                    child.depth(),
                                    child.syntax_kind().clone(),
                                );
                            }
                        }
                    } else if mode == 1 {
                        let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                        facts.push(SyntaxFact::new(
                            next,
                            witness.parent(),
                            witness.kind(),
                            witness.span(),
                            witness.depth(),
                            witness.syntax_kind().clone(),
                        ));
                    } else {
                        let changed = facts
                            .iter_mut()
                            .find(|fact| fact.local_id() == witness.local_id())
                            .unwrap();
                        *changed = SyntaxFact::new(
                            changed.local_id(),
                            changed.parent(),
                            changed.kind(),
                            changed.span(),
                            changed.depth(),
                            SyntaxKindLabel::new("go.foreign.signature").unwrap(),
                        );
                    }
                    let output = replay(facts)?;
                    assert!(
                        !output
                            .document()
                            .occurrences
                            .iter()
                            .any(
                                |site| site.syntax_kind == "go.direct_file_function.reference"
                                    && site.source.span().start_byte() == 1051
                                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
                            ),
                        "mode {mode}"
                    );
                }
            }
            let mut facts = parsed.facts().to_vec();
            let site = facts
                .iter_mut()
                .find(|fact| {
                    fact.syntax_kind().as_str() == "go.direct_file_function.reference"
                        && fact.span().start_byte() == 1051
                })
                .unwrap();
            *site = SyntaxFact::new(
                site.local_id(),
                Some(root.local_id()),
                site.kind(),
                site.span(),
                site.depth(),
                site.syntax_kind().clone(),
            );
            let output = replay(facts)?;
            assert!(
                !output
                    .document()
                    .occurrences
                    .iter()
                    .any(
                        |site| site.syntax_kind == "go.direct_file_function.reference"
                            && site.source.span().start_byte() == 1051
                            && matches!(site.target, OccurrenceTarget::Resolved { .. })
                    )
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn written_function_value_arguments_are_references_without_invented_calls() {
    for (target, owner) in [
        ("GenerateHandler", "RegisterRoutes"),
        ("endpoint", "register"),
    ] {
        let source = format!(
            "package sample\nfunc {target}(){{}};func {owner}(){{ receiver.Accept({target}) }}\n"
        );
        let output = analyze(&source);
        let target_id = entity_at(
            &output,
            target,
            EntityKind::Function,
            source.find(target).unwrap(),
            target.len(),
        );
        let owner_id = entity_at(
            &output,
            owner,
            EntityKind::Function,
            source.find(owner).unwrap(),
            owner.len(),
        );
        let start = u64::try_from(source.rfind(target).unwrap()).unwrap();
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start
                    && site.syntax_kind == "go.function_value.reference"
            })
            .collect();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert_eq!(site.enclosing, Some(owner_id));
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target_id }
        );
        assert!(
            output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(target_id))
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Entity(owner_id)
                    && edge.object == RelationEndpoint::Entity(target_id))
        );
        assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
    }
}

#[test]
fn function_value_shadowing_does_not_borrow_the_file_function_identity() {
    for source in [
        "package sample;func target(){};func use(target func()){external(target)}",
        "package sample;func target(){};func use(){target:=external;external(target)}",
        "package sample;import . \"external\";func target(){};func use(){external(target)}",
        "package sample;func target(){};func use(){func(){external(target)}()}",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "go.function_value.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

#[test]
fn function_value_materialization_requires_the_independent_native_site_witness() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, SyntaxFactKind, SyntaxKindLabel, execute_parse,
        testkit::MockParseProvider,
    };
    for shadowed in [false, true] {
        let source = if shadowed {
            "package sample;func target(){};func use(target func()){external(target)}"
        } else {
            "package sample;func target(){};func use(){external(target)}"
        };
        execute_context_observed(source.as_bytes(), "utf-8", 4096, |_, provider, request| {
            let cancel = Cancellation::new();
            let parsed = execute_parse(
                provider,
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let replay = |facts| {
                let mock = MockParseProvider::new(
                    provider.capabilities().clone(),
                    facts,
                    parsed.diagnostics().to_vec(),
                    parsed.report().coverage().clone(),
                );
                let analyzer = TreeSitterAnalyzer::new(
                    Arc::new(mock),
                    ProducerIdentity::new(
                        "go-value-witness",
                        "1.0",
                        content_hash(b"go-value-witness"),
                    )
                    .unwrap(),
                    LanguageId::new("go").unwrap(),
                    "go-value-replay",
                    content_hash(b"go-value-replay"),
                )
                .unwrap();
                execute_analysis(
                    &analyzer,
                    request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
            };
            let start = u64::try_from(source.rfind("target").unwrap()).unwrap();
            let resolved = |output: &rootlight_adapter_sdk::AnalysisOutput| {
                output.document().occurrences.iter().any(|site| {
                    site.source.span().start_byte() == start
                        && site.role == OccurrenceRole::Reference
                        && site.syntax_kind == "go.function_value.reference"
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                })
            };
            let baseline = replay(parsed.facts().to_vec())?;
            assert_eq!(resolved(&baseline), !shadowed);
            if shadowed {
                let mut facts = parsed.facts().to_vec();
                let fact = facts
                    .iter_mut()
                    .find(|fact| {
                        fact.kind() == SyntaxFactKind::Occurrence
                            && fact.span().start_byte() == start
                            && fact.syntax_kind().as_str().ends_with(".reference")
                    })
                    .unwrap();
                *fact = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    SyntaxKindLabel::new("go.function_value.reference").unwrap(),
                );
                assert!(!resolved(&replay(facts)?));
            } else {
                let context = parsed
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.span().start_byte() == start
                            && fact.syntax_kind().as_str() == "go.function_value_context.signature"
                    })
                    .unwrap();
                for mode in 0..3 {
                    let mut facts = parsed.facts().to_vec();
                    if mode == 0 {
                        facts.retain(|fact| fact.local_id() != context.local_id());
                    } else if mode == 1 {
                        let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                        facts.push(SyntaxFact::new(
                            next,
                            context.parent(),
                            context.kind(),
                            context.span(),
                            context.depth(),
                            context.syntax_kind().clone(),
                        ));
                    } else {
                        let fact = facts
                            .iter_mut()
                            .find(|fact| fact.local_id() == context.local_id())
                            .unwrap();
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            fact.parent(),
                            fact.kind(),
                            fact.span(),
                            fact.depth(),
                            SyntaxKindLabel::new("go.foreign.signature").unwrap(),
                        );
                    }
                    assert!(!resolved(&replay(facts)?), "mode {mode}");
                }
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn function_value_required_artifacts_keep_both_site_roles_at_the_exact_boundary() {
    execute_context_observed(
        b"package sample;func target(){};func use(){external(target)}",
        "utf-8",
        4096,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let required =
                provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
            let artifact = analyzer.prepare_artifact(request, &cancel)?;
            assert_eq!(artifact.required_syntax_fact_count(&cancel)?, required);
            let syntax = request.limits().syntax_stream();
            let for_capacity = |capacity| {
                AnalysisLimits::new(
                    request.limits().max_source_bytes(),
                    request.limits().max_syntax_nodes(),
                    request.limits().max_syntax_depth(),
                    request.limits().max_embedded_ranges(),
                    request.limits().max_reported_memory_bytes(),
                    StreamLimits::new(
                        syntax.max_batches(),
                        capacity,
                        syntax.max_output_bytes(),
                        syntax.max_diagnostics(),
                        syntax.max_diagnostic_bytes(),
                        syntax.max_string_bytes(),
                        BatchThresholds::new(
                            capacity.min(syntax.batch().max_records()),
                            syntax.batch().max_output_bytes(),
                            syntax.batch().max_diagnostics(),
                            syntax.batch().max_diagnostic_bytes(),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    request.limits().ir_stream().clone(),
                    request.limits().ir().clone(),
                )
                .unwrap()
            };
            let exact_limits = for_capacity(required);
            let exact = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &exact_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let exact_artifact = analyzer.prepare_artifact(&exact, &cancel)?;
            assert_eq!(
                exact_artifact.required_syntax_fact_count(&cancel)?,
                required
            );
            assert_eq!(exact_artifact.syntax_fact_count(), required);
            let output = execute_analysis(
                analyzer,
                &exact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let replay = analyzer.analyze_from_artifact(
                &exact,
                &exact_artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(output.document(), replay.document());
            assert!(
                output
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.syntax_kind == "go.function_value.reference"
                        && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            );
            let small_limits = for_capacity(required - 1);
            let small = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &small_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let expected = AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            });
            assert_eq!(
                analyzer.prepare_artifact(&small, &cancel).unwrap_err(),
                expected
            );
            // Preflight measures demand independently of output admission.
            assert_eq!(
                provider.required_syntax_fact_count(&small.to_parse_request(), &cancel)?,
                required
            );
            assert_eq!(
                rootlight_adapter_sdk::execute_parse(
                    provider,
                    &small.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
                .unwrap_err(),
                expected
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn native_package_call_candidates_remain_unresolved_without_project_target_authority() {
    use rootlight_adapter_sdk::execute_parse;
    let source = "package sample;func use(){external()}";
    execute_context_observed(
        source.as_bytes(),
        "utf-8",
        4096,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let parsed = execute_parse(
                provider,
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let site = parsed
                .facts()
                .iter()
                .find(|fact| {
                    fact.syntax_kind().as_str() == "go.package_function_candidate.reference"
                })
                .unwrap();
            let marker = parsed
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "go.package_function_context.signature")
                .unwrap();
            assert_eq!(site.span(), marker.span());
            assert_eq!(site.parent(), marker.parent());
            assert_eq!(site.depth(), marker.depth());
            let required =
                provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
            assert_eq!(
                analyzer
                    .prepare_artifact(request, &cancel)?
                    .required_syntax_fact_count(&cancel)?,
                required
            );
            let output = execute_analysis(
                analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert!(
                output
                    .document()
                    .occurrences
                    .iter()
                    .any(
                        |site| site.syntax_kind == "go.package_function_candidate.reference"
                            && site.role == OccurrenceRole::Reference
                            && matches!(site.target, OccurrenceTarget::Unresolved { .. })
                    )
            );
            assert!(
                !output
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls)
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "go-package-function-target-unavailable")
            );
            Ok(())
        },
    )
    .unwrap();
}
