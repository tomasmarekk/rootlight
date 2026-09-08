//! R source structure through native parsing and validated parser-independent IR.
//! Runtime evaluation and package dispatch remain explicit coverage gaps.

use super::*;

const R: LanguageCase = LanguageCase {
    name: "r",
    path: "src/example.R",
    frontend: "tree-sitter-r-1.3.0",
    source: "identity <- function(value = 1, ...) { value }\n",
    generated: false,
    body_before: "{ value }",
    body_after: "{ value + 1 }",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(R, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, R),
        &request(&fixture.snapshot, &fixture.source, R, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn r_equivalent_written_function_names_preserve_identity_and_original_source() {
    let mut identity = None;
    for name in [
        "identity",
        "`identity`",
        "\"identity\"",
        "'identity'",
        r"`\x69dentity`",
        r#""\u0069dentity""#,
        r#"r"(identity)""#,
    ] {
        let source = format!("{name} <- function(value) {{ value }}\n");
        let result = output(&source);
        let function = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function)
            .unwrap();
        assert_eq!(function.canonical_name, "identity", "{source}");
        if let Some(expected) = identity {
            assert_eq!(function.id, expected, "{source}");
        }
        identity = Some(function.id);
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: function.id,
                        }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
}

#[test]
fn r_undecodable_names_are_scoped_gaps_and_keep_source_evidence() {
    for name in [r"`\u0061`", r#""\xff""#, r#""\uD800""#] {
        let source = format!("{name} <- 1\nsafe <- 2\n");
        let result = output(&source);
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "safe")
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "declaration-name-unavailable"),
            "{source}: {:?}",
            result.document().skipped_regions
        );
        assert_eq!(result.document().files.len(), 1);
    }
}

#[test]
fn r_function_and_parameter_definitions_have_exact_source_and_containment() {
    let result = output(R.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    assert_eq!(document.entities.len(), 4, "{:?}", document.entities);
    for (name, kind) in [
        ("identity", EntityKind::Function),
        ("value", EntityKind::Parameter),
        ("...", EntityKind::Parameter),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind);
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            R.source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id))
        );
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "r-environment-dispatch-resolution-unavailable")
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn r_callable_headers_are_retained_from_source_in_both_assignment_directions() {
    for source in [
        "identity <- function(value = list(1, 2), ...) { value }",
        "(function(value = list(1, 2), ...) { value }) -> identity",
        "identity <<- function(value = list(1, 2), ...) { value }",
        "`with spaces` <- function(value = list(1, 2), ...) { value }",
    ] {
        let result = output(source);
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function)
            .unwrap();
        let signatures: Vec<_> = result
            .document()
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(entity.id))
                .then_some((extension, lexical))
            })
            .collect();
        assert_eq!(
            signatures.len(),
            1,
            "{source}: {:?}",
            result.document().skipped_regions
        );
        let (extension, signature) = &signatures[0];
        assert_eq!(signature.text(), "function(value = list(1, 2), ...)");
        let span = extension.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(signature.text())
        );
    }
}

#[test]
fn r_repeated_assignments_and_anonymous_parameters_keep_distinct_source_occurrences() {
    for (source, names) in [
        ("value <- 1\nvalue <- value + 1\n", vec!["value", "value"]),
        (
            "identity <- function(value) value\nidentity <- function(value) value + 1\n",
            vec!["identity", "value", "identity", "value"],
        ),
        (
            "lapply(values, function(value) value)\nlapply(values, function(value) value + 1)\n",
            vec!["value", "value"],
        ),
    ] {
        let result = output(source);
        let definitions: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
            .filter(|occurrence| {
                occurrence.source.span().start_byte() != 0
                    || occurrence.source.span().end_byte() != u64::try_from(source.len()).unwrap()
            })
            .collect();
        let mut expected = names;
        let symbols = definitions
            .iter()
            .map(|definition| match definition.target {
                OccurrenceTarget::Resolved { symbol } => symbol,
                _ => panic!("written R definition must resolve to its source entity"),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            symbols.len(),
            expected.len(),
            "source owners must remain distinct"
        );
        expected.sort_unstable();
        let mut actual: Vec<_> = definitions
            .iter()
            .map(|occurrence| {
                let span = occurrence.source.span();
                source
                    .get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    )
                    .unwrap()
            })
            .collect();
        actual.sort_unstable();
        assert_eq!(
            actual,
            expected,
            "{source}: {:?}",
            result.document().skipped_regions
        );
    }
}

#[test]
fn r_source_identity_is_stable_across_body_trivia_and_unrelated_name_edits() {
    let source = "identity <- function(value) { value }\nidentity <- function(value) { value + 1 }\nlapply(values, function(value) { value })\nlapply(values, function(value) { value + 1 })\n";
    let original = output(source);
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name != "unrelated")
            .map(|entity| {
                (
                    entity.id,
                    (entity.kind, entity.canonical_name.clone(), entity.container),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&original).len(), 7);
    for changed in [
        source.to_owned(),
        format!("\n# moved source\n{source}"),
        source.replace("{ value }", "{ value * 2 }"),
        format!("unrelated <- 10\n{source}"),
    ] {
        assert_eq!(
            identities(&output(&changed)),
            identities(&original),
            "{changed}"
        );
    }
}
