//! Native query roles remain typed source data through the public IR codec.
//! Original DQL fixtures are consumed unchanged; no query engine is executed.

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    CanonicalNormalizedFileChunk, DqlCallKind, DqlLiteralKind, DqlNode, DqlNodeKind, DqlQuery,
    ExtensionCriticality, ExtensionSupport, FactEvidence, IrDocument, IrLimits,
    NormalizedIrDocument, SourceRef, SourceSpan, decode_dql_query_envelope, decode_ir_document,
    new_dql_query_envelope,
};

fn fixture(language: bool) -> (&'static str, SourceRef, Vec<DqlNode>) {
    let text = if language {
        include_str!("../../../native-grammars/dql/tests/fixtures/suite03.dql")
    } else {
        include_str!("../../../native-grammars/dql/tests/fixtures/suite02.dql")
    };
    let file = FileId::from_bytes([3; 20]);
    let span = |start: usize, end: usize| {
        SourceSpan::new(file, start.try_into().unwrap(), end.try_into().unwrap()).unwrap()
    };
    let full = span(0, text.len());
    let source = SourceRef::new(
        RepositoryId::from_bytes([1; 16]),
        GenerationId::from_bytes([2; 20]),
        full,
        content_hash(text.as_bytes()),
        None,
    );
    let part = |owner: SourceSpan, needle: &str| {
        let start = usize::try_from(owner.start_byte()).unwrap();
        let end = usize::try_from(owner.end_byte()).unwrap();
        let offset = start + text[start..end].find(needle).unwrap();
        span(offset, offset + needle.len())
    };
    let root_name = if language { "alex" } else { "has_1_rev_friend" };
    let start = text.find(&format!("{root_name}(")).unwrap();
    let end = start + text[start..].find("\n\t}").unwrap() + 3;
    let root = span(start, end);
    let mut nodes = Vec::new();
    let mut add = |role, parent, range, name| {
        let index = u32::try_from(nodes.len()).unwrap();
        nodes.push(DqlNode {
            span: range,
            parent,
            role,
            name,
        });
        index
    };
    add(DqlNodeKind::Operation, None, full, None);
    let root_id = add(
        DqlNodeKind::Root { aggregation: false },
        Some(0),
        root,
        Some(part(root, root_name)),
    );
    let call_text = if language {
        "anyofterms(name@., \"Alex\")"
    } else {
        "has(friend)"
    };
    let generator = part(root, call_text);
    let function_name = if language { "anyofterms" } else { "has" };
    let call = add(
        DqlNodeKind::Call {
            class: DqlCallKind::Generator,
        },
        Some(root_id),
        generator,
        Some(part(generator, function_name)),
    );
    let predicate = part(generator, if language { "name@." } else { "friend" });
    let predicate_id = add(
        DqlNodeKind::Predicate { reverse: false },
        Some(call),
        predicate,
        Some(part(predicate, if language { "name" } else { "friend" })),
    );
    if language {
        add(
            DqlNodeKind::Language,
            Some(predicate_id),
            part(predicate, "@."),
            None,
        );
        add(
            DqlNodeKind::Literal {
                class: DqlLiteralKind::String,
            },
            Some(call),
            part(generator, "\"Alex\""),
            None,
        );
    } else {
        let filter = part(root, "@filter(eq(count(~friend), 1))");
        let filter_id = add(DqlNodeKind::Filter, Some(root_id), filter, None);
        let eq = part(filter, "eq(count(~friend), 1)");
        let eq_id = add(
            DqlNodeKind::Call {
                class: DqlCallKind::Filter,
            },
            Some(filter_id),
            eq,
            Some(part(eq, "eq")),
        );
        let count = part(eq, "count(~friend)");
        let count_id = add(
            DqlNodeKind::Call {
                class: DqlCallKind::Nested,
            },
            Some(eq_id),
            count,
            Some(part(count, "count")),
        );
        add(
            DqlNodeKind::Predicate { reverse: true },
            Some(count_id),
            part(count, "~friend"),
            Some(part(count, "friend")),
        );
        add(
            DqlNodeKind::Literal {
                class: DqlLiteralKind::Number,
            },
            Some(eq_id),
            part(eq, "1"),
            None,
        );
    }
    let root_start = usize::try_from(root.start_byte()).unwrap();
    let root_end = usize::try_from(root.end_byte()).unwrap();
    let selected_start = root_start + text[root_start..root_end].rfind("\n\t\tname").unwrap() + 3;
    let selected = span(selected_start, selected_start + 4);
    let selection = add(DqlNodeKind::Selection, Some(root_id), selected, None);
    add(
        DqlNodeKind::Predicate { reverse: false },
        Some(selection),
        selected,
        Some(selected),
    );
    (text, source, nodes)
}

#[test]
fn original_query_roles_filter_and_language_reach_canonical_ir() {
    for language in [false, true] {
        let (text, source, nodes) = fixture(language);
        let query = DqlQuery::from_evidence(source.clone(), text, nodes).unwrap();
        let envelope = new_dql_query_envelope(FactId::from_bytes([4; 20]), &query).unwrap();
        let decoded = decode_dql_query_envelope(&envelope).unwrap();
        assert_eq!(decoded, query);
        assert!(decoded.references().is_empty());
        assert_eq!(
            decoded
                .nodes()
                .iter()
                .filter(|node| node.role == DqlNodeKind::Filter)
                .count(),
            usize::from(!language)
        );
        assert_eq!(
            decoded
                .nodes()
                .iter()
                .filter(|node| node.role == DqlNodeKind::Language)
                .count(),
            usize::from(language)
        );
        assert_eq!(
            decoded
                .nodes()
                .iter()
                .filter(|node| node.role == DqlNodeKind::Predicate { reverse: true })
                .count(),
            usize::from(!language)
        );
        for index in 0..decoded.nodes().len() {
            let child = decoded.node_source(index).unwrap();
            assert_eq!(child.repository(), source.repository());
            assert_eq!(child.generation(), source.generation());
            assert_eq!(child.content_hash(), content_hash(text.as_bytes()));
            assert_eq!(child.span().file(), source.span().file());
        }
        decoded.verify_source_text(text).unwrap();
    }
}

#[test]
fn codec_and_chunk_rebind_reject_forgery_and_preserve_native_sources() {
    let (text, source, nodes) = fixture(false);
    let query = DqlQuery::from_evidence(source.clone(), text, nodes.clone()).unwrap();
    let provenance = FactId::from_bytes([4; 20]);
    let original = new_dql_query_envelope(provenance, &query).unwrap();
    let mut foreign = nodes.clone();
    let old = foreign[3].span;
    foreign[3].span = SourceSpan::new(
        FileId::from_bytes([99; 20]),
        old.start_byte(),
        old.end_byte(),
    )
    .unwrap();
    assert!(DqlQuery::from_evidence(source.clone(), text, foreign).is_err());
    let mut forged = original.clone();
    forged.criticality = ExtensionCriticality::Critical;
    assert!(decode_dql_query_envelope(&forged).is_err());
    let mut noncanonical = original.clone();
    noncanonical.payload.push(' ');
    assert!(decode_dql_query_envelope(&noncanonical).is_err());
    let generation = GenerationId::from_bytes([87; 20]);
    let limits = IrLimits::default();
    let support = ExtensionSupport::default();
    let IrDocument::NormalizedV1_1(template) = decode_ir_document(
        include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json"),
        &limits,
        &support,
    )
    .unwrap() else {
        panic!("normalized fixture");
    };
    let mut document = NormalizedIrDocument::empty(source.repository(), source.generation());
    let mut producer = template.provenance[0].clone();
    producer.repository = source.repository();
    producer.generation = source.generation();
    producer.language = "dql".to_owned();
    producer.input_sources = vec![source.clone()];
    producer.evidence_sources = vec![source.clone()];
    producer.derivation_parents.clear();
    let mut file = template.files[0].clone();
    file.id = source.span().file();
    file.repository = source.repository();
    file.generation = source.generation();
    file.path = "query.dql".to_owned();
    file.path_locator = None;
    file.content_hash = source.content_hash();
    file.byte_length = source.span().end_byte();
    file.language = "dql".to_owned();
    file.provenance = producer.id;
    file.evidence = FactEvidence {
        source: Some(source.clone()),
        derivation: Vec::new(),
    };
    document
        .extensions
        .push(new_dql_query_envelope(producer.id, &query).unwrap());
    document.provenance.push(producer);
    document.files.push(file);
    let chunk = CanonicalNormalizedFileChunk::new(&document, &limits, &support).unwrap();
    let rebound = chunk.rebind(generation, &limits, &support).unwrap();
    let envelope = &rebound.extensions[0];
    let decoded = decode_dql_query_envelope(envelope).unwrap();
    assert_ne!(envelope.id, original.id);
    assert_eq!(decoded.nodes(), query.nodes());
    assert_eq!(decoded.text(), query.text());
    for index in 0..decoded.nodes().len() {
        assert_eq!(decoded.node_source(index).unwrap().generation(), generation);
    }
}

#[test]
fn local_bindings_exclude_schema_names_and_opaque_source_forgeries() {
    let text = "query q($p: string) {\n v as var(func: eq(name, $p, \"uid(v)\")) { name }\n q(func: uid(v)) { v # uid(v)\n }\n}";
    let file = FileId::from_bytes([3; 20]);
    let span = |start: usize, end: usize| {
        SourceSpan::new(file, start.try_into().unwrap(), end.try_into().unwrap()).unwrap()
    };
    let full = span(0, text.len());
    let source = SourceRef::new(
        RepositoryId::from_bytes([1; 16]),
        GenerationId::from_bytes([2; 20]),
        full,
        content_hash(text.as_bytes()),
        None,
    );
    let part = |owner: SourceSpan, needle: &str| {
        let start = usize::try_from(owner.start_byte()).unwrap();
        let end = usize::try_from(owner.end_byte()).unwrap();
        let offset = start + text[start..end].find(needle).unwrap();
        span(offset, offset + needle.len())
    };
    let mut nodes = Vec::new();
    let mut add = |role, parent, range, name| {
        let id = u32::try_from(nodes.len()).unwrap();
        nodes.push(DqlNode {
            span: range,
            parent,
            role,
            name,
        });
        id
    };
    let operation_name_start = text.find("q(").unwrap();
    add(
        DqlNodeKind::Operation,
        None,
        full,
        Some(span(operation_name_start, operation_name_start + 1)),
    );
    let parameter = part(full, "$p: string");
    let parameter_id = add(
        DqlNodeKind::Parameter {
            value_type: part(parameter, "string"),
            required: false,
        },
        Some(0),
        parameter,
        Some(part(parameter, "$p")),
    );
    let root = part(full, "v as var(func: eq(name, $p, \"uid(v)\")) { name }");
    let root_id = add(
        DqlNodeKind::Root { aggregation: false },
        Some(0),
        root,
        Some(part(root, "var")),
    );
    let binding = part(root, "v as");
    let binding_id = add(
        DqlNodeKind::Binding,
        Some(root_id),
        binding,
        Some(part(binding, "v")),
    );
    let generator = part(root, "eq(name, $p, \"uid(v)\")");
    let call_id = add(
        DqlNodeKind::Call {
            class: DqlCallKind::Generator,
        },
        Some(root_id),
        generator,
        Some(part(generator, "eq")),
    );
    let predicate = part(generator, "name");
    add(
        DqlNodeKind::Predicate { reverse: false },
        Some(call_id),
        predicate,
        Some(predicate),
    );
    add(
        DqlNodeKind::ParameterReference,
        Some(call_id),
        part(generator, "$p"),
        None,
    );
    let literal = part(generator, "\"uid(v)\"");
    add(
        DqlNodeKind::Literal {
            class: DqlLiteralKind::String,
        },
        Some(call_id),
        literal,
        None,
    );
    let selected = part(root, "{ name }");
    let selected = part(selected, "name");
    let selected_id = add(DqlNodeKind::Selection, Some(root_id), selected, None);
    add(
        DqlNodeKind::Predicate { reverse: false },
        Some(selected_id),
        selected,
        Some(selected),
    );
    let root = part(full, "q(func: uid(v)) { v # uid(v)\n }");
    let use_root_id = add(
        DqlNodeKind::Root { aggregation: false },
        Some(0),
        root,
        Some(span(
            usize::try_from(root.start_byte()).unwrap(),
            usize::try_from(root.start_byte()).unwrap() + 1,
        )),
    );
    let generator = part(root, "uid(v)");
    let uid_id = add(
        DqlNodeKind::Call {
            class: DqlCallKind::Generator,
        },
        Some(use_root_id),
        generator,
        Some(part(generator, "uid")),
    );
    add(
        DqlNodeKind::VariableReference,
        Some(uid_id),
        part(generator, "v"),
        None,
    );
    let selected = part(root, "{ v");
    let selected = part(selected, "v");
    let selected_id = add(DqlNodeKind::Selection, Some(use_root_id), selected, None);
    add(
        DqlNodeKind::Predicate { reverse: false },
        Some(selected_id),
        selected,
        Some(selected),
    );
    let query = DqlQuery::from_evidence(source.clone(), text, nodes.clone()).unwrap();
    assert_eq!(query.references().len(), 2);
    assert_eq!(query.references()[0].declaration, Some(parameter_id));
    assert_eq!(query.references()[1].declaration, Some(binding_id));
    assert!(query.references().iter().all(|reference| {
        query.nodes()[usize::try_from(reference.node).unwrap()].role
            != DqlNodeKind::Predicate { reverse: false }
    }));
    let string_start = usize::try_from(literal.start_byte()).unwrap() + 1;
    let comment_start = text.rfind("uid(v)").unwrap();
    for (start, parent) in [(string_start, call_id), (comment_start, use_root_id)] {
        let mut forged = nodes.clone();
        forged.push(DqlNode {
            span: span(start, start + 6),
            parent: Some(parent),
            role: DqlNodeKind::Call {
                class: DqlCallKind::Nested,
            },
            name: Some(span(start, start + 3)),
        });
        assert!(
            DqlQuery::from_evidence(source.clone(), text, forged).is_err(),
            "opaque source at {start}"
        );
    }
}
