//! Native N3 keeps explicit quantified terms inside distinct formula scopes.
//! These written declarations do not imply rule execution or RDF resource identity.

use super::*;

const N3: LanguageCase = LanguageCase {
    name: "n3",
    path: "src/rules.n3",
    frontend: "tree-sitter-n3-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8]) -> AnalysisOutput {
    output_with_profile(source, false)
}

fn output_with_profile(source: &[u8], original: bool) -> AnalysisOutput {
    let provider = Arc::new(if original {
        TreeSitterProvider::new(product_profile::parser_config(MAX_SOURCE_BYTES).unwrap()).unwrap()
    } else {
        provider()
    });
    let fixture = Fixture::new(N3, source);
    let budget = if original {
        product_profile::analysis_limits(MAX_SOURCE_BYTES).unwrap()
    } else {
        limits()
    };
    let result = analyze(
        &analyzer(&provider, N3),
        &request(&fixture.snapshot, &fixture.source, N3, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn n3_written_quantifiers_keep_each_physical_term_and_formula_identity() {
    let source = b"@prefix var: <urn:vars:> .\r\n{ @forAll <#X>, var:y . <#X> var:p var:y . } => { @forSome <#X> . <#X> var:q true . } .\r\n{ @forAll <#X> . <#X> var:p false . } <= { @forSome <#X> . <#X> var:q false . } .\r\n";
    let result = output(source);
    let doc = result.document();
    let variables: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .collect();
    assert_eq!(
        variables.len(),
        5,
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
        5
    );
    assert_eq!(
        variables
            .iter()
            .filter(|entity| entity.canonical_name == "<#X>")
            .count(),
        4
    );
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Namespace && entity.canonical_name == "var")
    );
    for entity in variables {
        let definition = doc.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)).unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name.as_bytes()
        );
        assert_eq!(entity.language, "n3");
    }
    assert!(
        !doc.entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Property))
    );
    assert!(doc.skipped_regions.iter().any(|gap| gap.detail
        == "n3-formula-rule-triple-quick-variable-and-resource-reference-semantics-unavailable"));
}

#[test]
fn n3_quantifiers_do_not_promote_literals_quick_uses_or_malformed_lists() {
    for source in [
        b"@forAll \"literal\" .\n".as_slice(),
        b"@forSome <#x>, .\n",
        b"@forAll <#x>\n",
        b"{ @forAll <#x> . <#x> <urn:p> true .\n",
        b"{ ?x <urn:p> true . } => { ?x <urn:q> true . } .\n",
        b"<urn:subject> <urn:predicate> \"@forAll <#fake>\" .\n",
    ] {
        let result = output(source);
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
fn n3_quantified_names_keep_valid_written_escapes_without_decoding() {
    let source = br"@prefix ex: <urn:example:> .
@forAll <urn:\u0058>, ex:has\#tag .
";
    let result = output(source);
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, BTreeSet::from([r"<urn:\u0058>", r"ex:has\#tag"]));
}

#[test]
#[ignore = "requires explicitly supplied original EYE and RDFLib corpus roots"]
fn n3_original_eye_and_rdflib_keep_source_owned_prefixes_and_quantified_terms() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_DIAGNOSTIC_CORPUS_ROOT").expect("explicit corpus parent"),
    );
    let oracle: serde_json::Value = serde_json::from_slice(&fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../development-docs/language-coverage/future-work/corpus/n3-declarations-originals.json")).unwrap()).unwrap();
    let mut absent_checked = 0;
    for (path, bytes, expected) in [
        (
            "45_Prolog_eye/reasoning/euq/euq.n3",
            173,
            vec![(105, 109, "<#X>", EntityKind::Variable)],
        ),
        (
            "45_Prolog_eye/reasoning/sequents/proof/beetle.n3",
            1751,
            vec![
                (250, 253, "var", EntityKind::Namespace),
                (1163, 1170, "var:x_0", EntityKind::Variable),
                (1319, 1326, "var:x_0", EntityKind::Variable),
                (1328, 1335, "var:x_1", EntityKind::Variable),
            ],
        ),
        (
            "45_Prolog_eye/reasoning/rgb/blueproof003.n3",
            2305,
            vec![
                (1674, 1681, "var:x_0", EntityKind::Variable),
                (1683, 1690, "var:x_1", EntityKind::Variable),
                (2021, 2028, "var:x_0", EntityKind::Variable),
                (2030, 2037, "var:x_1", EntityKind::Variable),
            ],
        ),
        (
            "183_N3_rdflib/test/data/suites/w3c/n3/N3Tests/01etc/owl-Nothing.n3",
            77,
            vec![(8, 11, "owl", EntityKind::Namespace)],
        ),
        (
            "183_N3_rdflib/test/data/suites/w3c/n3/N3Tests/quantification/application_forAll_forSome.n3",
            111,
            vec![
                (43, 45, ":x", EntityKind::Variable),
                (68, 70, ":f", EntityKind::Variable),
            ],
        ),
    ] {
        let source = fs::read(root.join(path)).unwrap();
        assert_eq!(source.len(), bytes, "{path}");
        let result = output_with_profile(&source, true);
        let doc = result.document();
        let (repository, relative) = path.split_once('/').unwrap();
        for absent in oracle[repository]["absent_definitions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["path"].as_str() == Some(relative))
        {
            let start = absent["start_byte"].as_u64().unwrap();
            let end = absent["end_byte"].as_u64().unwrap();
            let written: String = source
                [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(
                written,
                absent["written_hex"].as_str().unwrap(),
                "oracle physical negative {path}"
            );
            assert!(
                !doc.occurrences
                    .iter()
                    .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                        && occurrence.source.span().start_byte() == start
                        && occurrence.source.span().end_byte() == end),
                "unexpected definition {path}:{start}"
            );
            absent_checked += 1;
        }
        for (start, end, name, kind) in expected {
            assert_eq!(
                &source[start..end],
                name.as_bytes(),
                "physical witness {path}"
            );
            let occurrence = doc
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                        && occurrence.source.span().end_byte() == u64::try_from(end).unwrap()
                })
                .unwrap_or_else(|| {
                    panic!(
                        "missing {path}:{start} {name}; diagnostics={:?}",
                        doc.diagnostics
                    )
                });
            let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                panic!("unresolved definition {path}")
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
                (kind, name, "n3"),
                "{path}"
            );
        }
        assert!(
            !doc.entities
                .iter()
                .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Property)),
            "{path}"
        );
    }
    assert_eq!(absent_checked, 16);
}
