//! Python formal binders use native named-function parameter fields.
//! Annotation/default expressions and lambda binders remain outside this contract.

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

const SOURCE: &str = include_str!("fixtures/python_parameters/floor.py");

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
    fs::write(directory.path().join("source.python"), raw).unwrap();
    let repository = derive_repository(b"python-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.python")).unwrap(),
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
            "python-source-contract",
            "1.0",
            content_hash(b"python-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("python").unwrap(),
        "python-static-contract",
        content_hash(b"python-contract-binary"),
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
        LanguageId::new("python").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"python-contract-build-context")),
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
fn original_floor_parameter_has_exact_name_and_function_ownership() {
    let output = analyze(SOURCE);
    let function_start = SOURCE.find("def floor(").unwrap() + 4;
    let owner = entity_at(&output, "floor", EntityKind::Function, function_start, 5);
    let start = function_start + "floor(".len();
    let parameter = entity_at(&output, "x", EntityKind::Parameter, start, 1);
    contains(&output, owner, parameter, start, start + 1);
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| { entity.kind == EntityKind::Parameter })
            .count(),
        1
    );
}

#[test]
fn all_direct_formal_shapes_exclude_annotation_default_and_lambda_identifiers() {
    let source = concat!(
        "# def phantom(comment): pass\n",
        "\"def phantom(string): pass\"\n",
        "def forms(plain, /, typed: Annotation, defaulted=default_source, ",
        "typed_default: Annotation=default_source, *args: Annotation, ",
        "keyword: Annotation=default_source, **kwargs: Annotation):\n",
        "    return plain\n",
        "def variadic(*rest, **options): pass\n",
        "def keyword_only(*, value): pass\n",
        "def factory(callback=lambda ghost=seed: ghost): pass\n",
        "anonymous = lambda hidden=seed: hidden\n",
    );
    let output = analyze(source);
    let expected = [
        (
            "forms",
            &[
                "plain",
                "typed",
                "defaulted",
                "typed_default",
                "args",
                "keyword",
                "kwargs",
            ][..],
        ),
        ("variadic", &["rest", "options"][..]),
        ("keyword_only", &["value"][..]),
        ("factory", &["callback"][..]),
    ];
    for (owner_name, parameters) in expected {
        let function = source.find(&format!("def {owner_name}(")).unwrap() + 4;
        let owner = entity_at(
            &output,
            owner_name,
            EntityKind::Function,
            function,
            owner_name.len(),
        );
        let header = function + owner_name.len() + 1;
        for name in parameters {
            let start = header + source[header..].find(name).unwrap();
            let parameter = entity_at(&output, name, EntityKind::Parameter, start, name.len());
            contains(&output, owner, parameter, start, start + name.len());
        }
    }
    let mut names: Vec<_> = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    names.sort_unstable();
    let mut expected = vec![
        "plain",
        "typed",
        "defaulted",
        "typed_default",
        "args",
        "keyword",
        "kwargs",
        "rest",
        "options",
        "value",
        "callback",
    ];
    expected.sort_unstable();
    assert_eq!(names, expected);
    for absent in [
        "Annotation",
        "default_source",
        "ghost",
        "seed",
        "hidden",
        "comment",
        "string",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == absent),
            "{absent}"
        );
    }
}

#[test]
fn nested_duplicate_names_and_method_parameters_keep_distinct_written_owners() {
    let source = concat!(
        "def outer(value):\n",
        "    def inner(value: int=1): return value\n",
        "    return value\n",
        "class Written:\n",
        "    def method(self, value): return value\n",
    );
    let output = analyze(source);
    let mut parameter_ids = Vec::new();
    for (name, kind, names) in [
        ("outer", EntityKind::Function, &["value"][..]),
        ("inner", EntityKind::Function, &["value"][..]),
        ("method", EntityKind::Method, &["self", "value"][..]),
    ] {
        let function = source.find(&format!("def {name}(")).unwrap() + 4;
        let owner = entity_at(&output, name, kind, function, name.len());
        let header = function + name.len() + 1;
        for parameter_name in names {
            let start = header + source[header..].find(parameter_name).unwrap();
            let parameter = entity_at(
                &output,
                parameter_name,
                EntityKind::Parameter,
                start,
                parameter_name.len(),
            );
            contains(
                &output,
                owner,
                parameter,
                start,
                start + parameter_name.len(),
            );
            assert!(!parameter_ids.contains(&parameter));
            parameter_ids.push(parameter);
        }
    }
    assert_eq!(parameter_ids.len(), 4);
}

#[test]
fn unicode_utf16_parameter_names_preserve_physical_leaf_spans_and_hashes() {
    let source = "# λ🙂\ndef written(δ: float): return δ\n";
    for little_endian in [true, false] {
        let mut raw = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze_raw(
            &raw,
            if little_endian {
                "utf-16le"
            } else {
                "utf-16be"
            },
        );
        let owner_offset = source.find("written").unwrap();
        let name_offset = source.find("δ:").unwrap();
        let start = |offset| 2 + source[..offset].encode_utf16().count() * 2;
        let owner = entity_at(
            &output,
            "written",
            EntityKind::Function,
            start(owner_offset),
            14,
        );
        let parameter = entity_at(&output, "δ", EntityKind::Parameter, start(name_offset), 2);
        contains(
            &output,
            owner,
            parameter,
            start(name_offset),
            start(name_offset) + 2,
        );
        let expected = if little_endian {
            0x03b4_u16.to_le_bytes()
        } else {
            0x03b4_u16.to_be_bytes()
        };
        assert_eq!(&raw[start(name_offset)..start(name_offset) + 2], expected);
    }
}

#[test]
fn body_edits_preserve_parameter_identity_and_required_admission_is_atomic() {
    let source = "def stable(value: int):\n    return value\n";
    let original = analyze(source);
    let changed = analyze(&source.replace("return value", "return 2 * value"));
    let start = source.find("value").unwrap();
    assert_eq!(
        entity_at(&original, "value", EntityKind::Parameter, start, 5),
        entity_at(&changed, "value", EntityKind::Parameter, start, 5)
    );
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, actual) = execute(source.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    entity_at(&bounded, "value", EntityKind::Parameter, start, 5);
    assert_eq!(
        execute(source.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
}

fn resolved_formal_read(
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
fn original_typed_return_references_resolve_to_their_own_formals() {
    let raw = include_bytes!("fixtures/python_parameter_binding/test.py");
    assert_eq!(raw.len(), 4073);
    let output = analyze_raw(raw, "utf-8");
    let owner = entity_at(&output, "add_numbers", EntityKind::Function, 3715, 11);
    for (name, definition, reference) in [("a", 3727, 3899), ("b", 3735, 3903)] {
        let parameter = entity_at(&output, name, EntityKind::Parameter, definition, 1);
        contains(&output, owner, parameter, definition, definition + 1);
        assert_eq!(&raw[definition..definition + 1], name.as_bytes());
        assert_eq!(&raw[reference..reference + 1], name.as_bytes());
        resolved_formal_read(&output, owner, parameter, reference, reference + 1, raw);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == reference as u64)
        );
    }
}

#[test]
fn equal_parameter_names_keep_each_written_function_owner() {
    let text = "def first(a: int):\n    return a\n\ndef second(a: int = 0):\n    return a + 1\n";
    let output = analyze(text);
    for name in ["first", "second"] {
        let begin = text.find(name).unwrap();
        let parameter_start = text[begin..].find("a:").unwrap() + begin;
        let reference_start =
            text[parameter_start..].find("return a").unwrap() + parameter_start + 7;
        let owner = entity_at(&output, name, EntityKind::Function, begin, name.len());
        let parameter = entity_at(&output, "a", EntityKind::Parameter, parameter_start, 1);
        resolved_formal_read(
            &output,
            owner,
            parameter,
            reference_start,
            reference_start + 1,
            text.as_bytes(),
        );
    }
}

#[test]
fn python_compile_time_binders_and_dynamic_scopes_do_not_receive_formal_certificates() {
    let bodies = [
        "    a = 1\n    return a\n",
        "    return a\n    a = 1\n",
        "    del a\n    return a\n",
        "    global a\n    return a\n",
        "    nonlocal a\n    return a\n",
        "    for a in values:\n        pass\n    return a\n",
        "    with manager as a:\n        pass\n    return a\n",
        "    try:\n        pass\n    except Exception as a:\n        pass\n    return a\n",
        "    import package as a\n    return a\n",
        "    return [a for a in values]\n",
        "    return (lambda a: a)(a)\n",
        "    def nested():\n        return a\n    return a\n",
        "    class Nested:\n        a = 0\n    return a\n",
        "    return (a := 1)\n",
        "    return eval('a')\n",
        "    exec('a = 0')\n    return a\n",
        "    return locals()['a']\n",
        "    return a()\n",
        "    return obj.a\n",
        "    return f'{a}'\n",
        "    match value:\n        case a:\n            return a\n",
    ];
    for body in bodies {
        let text = format!("def written(a: int):\n{body}");
        let output = analyze(&text);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "python.parameter_body.reference"
                    || (site.role == OccurrenceRole::Reference
                        && matches!(site.target, OccurrenceTarget::Resolved { .. }))),
            "{text}"
        );
        assert!(output.document().skipped_regions.iter().any(|gap|
            gap.domain == rootlight_ir::FactDomain::Relations
            && gap.detail == "python-function-binding-proof-is-limited-to-the-ordinary-formal-read-subset"), "{text}");
    }
}

#[test]
fn decorators_duplicate_and_nfkc_names_keep_explicit_binding_limits() {
    for text in [
        "@decorator\ndef written(a: int):\n    return a\n",
        "def written(a, a):\n    return a\n",
        "def written(K, \u{212a}):\n    return K\n",
        "def written(\u{212a}):\n    return K\n",
        "def written[T](a: T):\n    return a\n",
        "class Box:\n    def written(self, __a, _Box__a):\n        return __a\n",
        "def written(a: ):\n    return a\n",
    ] {
        let output = analyze(text);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "python.parameter_body.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations),
            "{text}"
        );
    }
}

#[test]
fn annotation_default_literals_and_call_heads_are_not_certified_body_reads() {
    let text = "def written(a: a = default):\n    \"\"\"return a; global a; eval('a')\"\"\"\n    # a = ignored\n    return a\n";
    let output = analyze(text);
    let return_start = text.rfind("return a").unwrap() + 7;
    let certified: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "python.parameter_body.reference")
        .collect();
    assert_eq!(certified.len(), 1);
    assert_eq!(certified[0].source.span().start_byte(), return_start as u64);
    let owner = entity_at(&output, "written", EntityKind::Function, 4, 7);
    let parameter = entity_at(
        &output,
        "a",
        EntityKind::Parameter,
        text.find("a:").unwrap(),
        1,
    );
    resolved_formal_read(
        &output,
        owner,
        parameter,
        return_start,
        return_start + 1,
        text.as_bytes(),
    );
}

#[test]
fn ascii_formals_after_unicode_trivia_keep_bom_and_utf16_physical_reference_bytes() {
    let text = "\u{feff}# λ🙂\r\ndef written(a: int):\r\n    return a + 1\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |value: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                value.as_bytes().to_vec()
            } else {
                value
                    .encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(text);
        let output = analyze_raw(&raw, encoding);
        let function = text.find("written").unwrap();
        let declaration = text.find("a:").unwrap();
        let reference = text.rfind("a +").unwrap();
        let offset = |position| encode(&text[..position]).len();
        let width = encode("a").len();
        let owner = entity_at(
            &output,
            "written",
            EntityKind::Function,
            offset(function),
            encode("written").len(),
        );
        let parameter = entity_at(
            &output,
            "a",
            EntityKind::Parameter,
            offset(declaration),
            width,
        );
        let start = offset(reference);
        resolved_formal_read(&output, owner, parameter, start, start + width, &raw);
        assert_eq!(&raw[start..start + width], encode("a"));
    }
}

#[test]
fn ordinary_formal_forms_and_written_methods_use_their_own_local_namespace() {
    let source = "from dependency import a\ndef forms(a, /, b: int = 0, *args: tuple, **kwargs: dict):\n    return a, b, args, kwargs\n";
    let output = analyze(source);
    let owner = entity_at(
        &output,
        "forms",
        EntityKind::Function,
        source.find("forms").unwrap(),
        5,
    );
    let body = source.find("return ").unwrap() + 7;
    for name in ["a", "b", "args", "kwargs"] {
        let before_body = &source[..body];
        let marker = match name {
            "a" => "a, /",
            "b" => "b:",
            "args" => "args:",
            _ => "kwargs:",
        };
        let definition = before_body.find(marker).unwrap();
        let reference = body + source[body..].find(name).unwrap();
        let parameter = entity_at(&output, name, EntityKind::Parameter, definition, name.len());
        resolved_formal_read(
            &output,
            owner,
            parameter,
            reference,
            reference + name.len(),
            source.as_bytes(),
        );
    }
    let source = "class Box:\n    def read(self, a: int):\n        return a\n";
    let output = analyze(source);
    let owner = entity_at(
        &output,
        "read",
        EntityKind::Method,
        source.find("read").unwrap(),
        4,
    );
    let parameter = entity_at(
        &output,
        "a",
        EntityKind::Parameter,
        source.find("a:").unwrap(),
        1,
    );
    let reference = source.rfind("return a").unwrap() + 7;
    resolved_formal_read(
        &output,
        owner,
        parameter,
        reference,
        reference + 1,
        source.as_bytes(),
    );
}

#[test]
fn private_spelling_outside_classes_keeps_an_ordinary_formal_binding() {
    let source = "def written(__a: int):\n    return __a\n";
    let output = analyze(source);
    let owner = entity_at(&output, "written", EntityKind::Function, 4, 7);
    let parameter = entity_at(
        &output,
        "__a",
        EntityKind::Parameter,
        source.find("__a:").unwrap(),
        3,
    );
    let reference = source.rfind("__a").unwrap();
    resolved_formal_read(
        &output,
        owner,
        parameter,
        reference,
        reference + 3,
        source.as_bytes(),
    );
}

#[test]
fn original_async_generator_prompt_has_exact_reference_to_its_own_formal() {
    let raw = include_bytes!("fixtures/python_async_formals/original-gradio_app.py");
    assert_eq!(raw.len(), 2917);
    let output = analyze_raw(raw, "utf-8");
    let owner = entity_at(&output, "generate", EntityKind::Function, 90, 8);
    let parameter = entity_at(&output, "prompt", EntityKind::Parameter, 99, 6);
    contains(&output, owner, parameter, 99, 105);
    resolved_formal_read(&output, owner, parameter, 632, 638, raw);
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 632
                && site.source.span().end_byte() == 638)
    );
}

#[test]
fn async_and_ordinary_opaque_call_arguments_keep_unwritten_values_unknown() {
    for text in [
        "async def written(a: int):\n    return a\n",
        "async def written(a: int):\n    other = external(a)\n    if a:\n        other += 1\n    for item in values:\n        yield a\n",
        "def written(a):\n    other = object.method(a, key=1)\n    return a\n",
        "async def written(a):\n    source = iter(lambda: external(), b'')\n    for item in source:\n        yield a\n",
    ] {
        let output = analyze(text);
        let owner_start = text.find("written").unwrap();
        let parameter_start = text.find("(a").unwrap() + 1;
        let read_start = text.rfind(" a").unwrap() + 1;
        let owner = entity_at(&output, "written", EntityKind::Function, owner_start, 7);
        let parameter = entity_at(&output, "a", EntityKind::Parameter, parameter_start, 1);
        resolved_formal_read(
            &output,
            owner,
            parameter,
            read_start,
            read_start + 1,
            text.as_bytes(),
        );
    }
}

#[test]
fn new_formal_contexts_do_not_certify_keyword_labels_or_attribute_selectors() {
    let text = "def written(a):\n    external(a=1)\n    other = object.a\n    return a\n";
    let output = analyze(text);
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "python.parameter_body.reference")
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].source.span().start_byte(),
        u64::try_from(text.rfind("return a").unwrap() + 7).unwrap()
    );
    for body in [
        "    other = 1\n    a = 2\n    return a\n",
        "    return a\n    a = 2\n",
        "    for a in values:\n        pass\n    return a\n",
        "    other = lambda x=(a := 1): x\n    return a\n",
        "    exec('a = 1')\n    return a\n",
        "    return external(f'{a}')\n",
    ] {
        let source = format!("async def written(a):\n{body}");
        let output = analyze(&source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "python.parameter_body.reference"),
            "{source}"
        );
    }
}

#[test]
fn original_theme_attribute_receiver_resolves_to_its_written_style_formal() {
    let raw = include_bytes!("fixtures/theme_source55/original-theme.py");
    assert_eq!(raw.len(), 1398);
    assert_eq!(&raw[227..232], b"style");
    assert_eq!(&raw[277..282], b"style");
    let output = analyze_raw(raw, "utf-8");
    let owner = entity_at(&output, "_load_theme", EntityKind::Function, 215, 11);
    let parameter = entity_at(&output, "style", EntityKind::Parameter, 227, 5);
    contains(&output, owner, parameter, 227, 232);
    resolved_formal_read(&output, owner, parameter, 277, 282, raw);
    assert!(!output.document().occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 277
            && site.source.span().end_byte() == 282
    }));
}

#[test]
fn raise_and_plain_attribute_writes_preserve_only_lexical_formal_reads() {
    for body in [
        "    if not ready(style):\n        raise TypeError('unavailable')\n    return style\n",
        "    style.master.loaded = True\n    return style\n",
        "    if not ready(style.master):\n        raise TypeError('unavailable')\n    style.master.loaded = True\n    return style\n",
    ] {
        let source = format!("def written(style):\n{body}");
        let output = analyze(&source);
        let owner = entity_at(&output, "written", EntityKind::Function, 4, 7);
        let parameter = entity_at(&output, "style", EntityKind::Parameter, 12, 5);
        let read = source.rfind("return style").unwrap() + 7;
        resolved_formal_read(&output, owner, parameter, read, read + 5, source.as_bytes());
    }
    let source = "def written(style):\n    style.style = True\n    return style\n";
    let output = analyze(source);
    let selector = source.find("style.style").unwrap() + 6;
    assert!(!output.document().occurrences.iter().any(|site| {
        site.syntax_kind == "python.parameter_body.reference"
            && site.source.span().start_byte() == u64::try_from(selector).unwrap()
    }));
}

#[test]
fn attribute_write_support_does_not_admit_binding_or_computed_target_scopes() {
    for body in [
        "    style = other\n    return style\n",
        "    for style in values:\n        pass\n    return style\n",
        "    style[0].flag = True\n    return style\n",
        "    factory(style).flag = True\n    return style\n",
        "    style.flag += 1\n    return style\n",
        "    style.flag: int = 1\n    return style\n",
    ] {
        let source = format!("def written(style):\n{body}");
        let output = analyze(&source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| { site.syntax_kind == "python.parameter_body.reference" }),
            "{source}"
        );
    }
}
