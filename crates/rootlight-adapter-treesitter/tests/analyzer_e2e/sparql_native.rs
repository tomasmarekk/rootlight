//! SPARQL written AS aliases remain distinct from ordinary variable uses.
//! Query scopes preserve identity without asserting evaluation or resource binding.

use super::*;

const SPARQL: LanguageCase = LanguageCase {
    name: "sparql",
    path: "src/query.rq",
    frontend: "tree-sitter-sparql-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8], original: bool) -> AnalysisOutput {
    let provider = Arc::new(if original {
        TreeSitterProvider::new(product_profile::parser_config(MAX_SOURCE_BYTES).unwrap()).unwrap()
    } else {
        provider()
    });
    let fixture = Fixture::new(SPARQL, source);
    let budget = if original {
        product_profile::analysis_limits(MAX_SOURCE_BYTES).unwrap()
    } else {
        limits()
    };
    let result = analyze(
        &analyzer(&provider, SPARQL),
        &request(&fixture.snapshot, &fixture.source, SPARQL, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn sparql_explicit_aliases_keep_bare_names_full_tokens_and_query_scope_identity() {
    let source = b"PREFIX ex: <urn:example:>\r\nSELECT ?ordinary (1 AS # comment\r\n ?same) WHERE { BIND(\"AS ?fake\" AS ?inner) { SELECT (3 AS ?same) WHERE {} } ?s ex:p ?ordinary . }\r\nSELECT (4 AS $same) WHERE {}\r\nSELECT (5 AS ?0) WHERE { BIND(6 AS $1) }\r\n# SELECT (0 AS ?commented) WHERE {}\r\n";
    let result = output(source, false);
    let doc = result.document();
    let variables: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .collect();
    assert_eq!(
        variables.len(),
        6,
        "entities={:#?}; diagnostics={:?}",
        doc.entities,
        doc.diagnostics
    );
    assert_eq!(
        variables
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        variables
            .iter()
            .filter(|entity| entity.canonical_name == "same")
            .count(),
        3
    );
    assert!(variables.iter().any(|entity| entity.canonical_name == "0"));
    assert!(variables.iter().any(|entity| entity.canonical_name == "1"));
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Namespace && entity.canonical_name == "ex")
    );
    for entity in variables {
        let definition = doc.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)).unwrap();
        let span = definition.source.span();
        let raw = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert!(matches!(raw.first(), Some(b'?' | b'$')));
        assert_eq!(&raw[1..], entity.canonical_name.as_bytes());
        assert_eq!(entity.language, "sparql");
    }
    assert!(
        !doc.entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Property))
    );
    assert!(!doc.entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "ordinary" | "fake" | "commented" | "s"
    )));
}

#[test]
fn sparql_variable_uses_and_malformed_aliases_are_not_definitions() {
    for source in [
        b"SELECT ?ordinary WHERE { ?ordinary <urn:p> ?object . FILTER(?ordinary = ?object) }\n"
            .as_slice(),
        b"SELECT (1 AS ?broken WHERE {}\n",
        b"SELECT (1 AS \"literal\") WHERE {}\n",
        b"SELECT (1 AS ?) WHERE {}\n",
        b"SELECT (1 AS ?clean) WHERE {\n",
        b"SELECT ?ordinary WHERE { ?s <urn:p> \"AS ?literal\" . }\n",
    ] {
        let result = output(source, false);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable),
            "{source:?}"
        );
    }
}

#[test]
#[ignore = "requires explicitly supplied original EYE, RDFLib and Jena corpus roots"]
fn sparql_original_prefixes_and_aliases_match_independent_physical_oracle() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_DIAGNOSTIC_CORPUS_ROOT").expect("explicit corpus parent"),
    );
    let oracle: serde_json::Value = serde_json::from_slice(&fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../development-docs/language-coverage/future-work/corpus/sparql-alias-declarations-originals.json")).unwrap()).unwrap();
    let mut positives = 0;
    let mut negatives = 0;
    for (repository, obligations) in oracle.as_object().unwrap() {
        for file in obligations["source_files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            let source = fs::read(root.join(repository).join(path)).unwrap();
            assert_eq!(
                u64::try_from(source.len()).unwrap(),
                file["bytes"].as_u64().unwrap()
            );
            let result = output(&source, true);
            let doc = result.document();
            for absent in obligations["absent_definitions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["path"].as_str() == Some(path))
            {
                let start = absent["start_byte"].as_u64().unwrap();
                let end = absent["end_byte"].as_u64().unwrap();
                let written: String = source
                    [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                assert_eq!(written, absent["written_hex"].as_str().unwrap(), "{path}");
                assert!(
                    !doc.occurrences
                        .iter()
                        .any(|site| site.role == OccurrenceRole::Definition
                            && site.source.span().start_byte() == start
                            && site.source.span().end_byte() == end),
                    "unexpected definition {repository}/{path}:{start}"
                );
                negatives += 1;
            }
            for expected in obligations["definitions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["path"].as_str() == Some(path))
            {
                let start = expected["start_byte"].as_u64().unwrap();
                let end = expected["end_byte"].as_u64().unwrap();
                let written: String = source
                    [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                assert_eq!(written, expected["written_hex"].as_str().unwrap(), "{path}");
                let name = expected["name"].as_str().unwrap();
                let kind = match expected["kind"].as_str().unwrap() {
                    "namespace" => EntityKind::Namespace,
                    "variable" => EntityKind::Variable,
                    other => panic!("unexpected oracle kind {other}"),
                };
                let occurrence = doc
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.source.span().start_byte() == start
                            && site.source.span().end_byte() == end
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "missing {repository}/{path}:{start}; diagnostics={:?}",
                            doc.diagnostics
                        )
                    });
                let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                    panic!("unresolved written definition")
                };
                let entity = doc
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap();
                assert_eq!(
                    (
                        entity.kind,
                        entity.canonical_name.as_str(),
                        entity.language.as_str()
                    ),
                    (kind, name, "sparql")
                );
                positives += 1;
            }
            assert!(
                !doc.entities.iter().any(|entity| matches!(
                    entity.kind,
                    EntityKind::Function | EntityKind::Property
                ))
            );
        }
    }
    assert_eq!((positives, negatives), (10, 23));
}
