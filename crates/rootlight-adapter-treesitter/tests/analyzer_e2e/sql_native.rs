//! SQL source declarations through the actual analyzer and validated IR.
//! Source ownership is distinct from a live catalog or resolved database semantics.

use super::*;

const SQL: LanguageCase = LanguageCase {
    name: "sql",
    path: "src/schema.sql",
    frontend: "tree-sitter-sequel-0.3.11",
    source: "CREATE TABLE app.account (id INT, label TEXT);\nCREATE VIEW app.active AS SELECT id FROM app.account;\nCREATE FUNCTION app.identity(value INT) RETURNS INT AS $$ SELECT value; $$ LANGUAGE SQL;",
    generated: false,
    body_before: "SELECT value;",
    body_after: "SELECT value + 1;",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SQL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, SQL),
        &request(&fixture.snapshot, &fixture.source, SQL, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn sql_definitions_are_typed_source_backed_and_owned_without_claiming_catalog_resolution() {
    let result = output(SQL.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    assert_eq!(document.entities.len(), 7, "{:?}", document.entities);
    for (name, kind) in [
        ("app.account", EntityKind::DatabaseObject),
        ("app.active", EntityKind::DatabaseObject),
        ("id", EntityKind::Field),
        ("label", EntityKind::Field),
        ("app.identity", EntityKind::Function),
        ("value", EntityKind::Parameter),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind, "{name}");
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        let text = SQL
            .source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(text, name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(text.as_bytes())
        );
        let owner = document
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id)
            })
            .unwrap();
        let RelationEndpoint::Entity(parent) = owner.subject else {
            panic!("source entity owner")
        };
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.id == parent)
            .unwrap();
        let outer = parent.evidence.source.as_ref().unwrap().span();
        assert!(outer.start_byte() <= span.start_byte() && outer.end_byte() >= span.end_byte());
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "sql-catalog-resolution-unavailable")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "sql-dialect-statement-coverage-incomplete")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "sql-function-body-semantics-unavailable")
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
}

#[test]
fn sql_repeated_declarations_preserve_distinct_source_occurrences() {
    let source = "CREATE TABLE item(id INT); CREATE TABLE item(id TEXT); CREATE VIEW item AS SELECT id FROM item;";
    let result = output(source);
    assert_eq!(result.document().entities.len(), 6);
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "item")
            .count(),
        3
    );
}

#[test]
fn sql_body_and_qualified_name_trivia_edits_keep_source_identities() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SQL);
    let fixture = Fixture::new(SQL, SQL.source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, SQL, &budget),
        &ExtensionSupport::default(),
    );
    let changed_text = SQL
        .source
        .replace("app.account", "app /* qualifier */ . account")
        .replace(SQL.body_before, SQL.body_after);
    let changed = fixture.rewrite(changed_text.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, SQL, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(
        first
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        next.document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    for entity in &next.document().entities {
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().generation(),
            changed.source.generation()
        );
    }
}

#[test]
fn sql_unknown_names_and_parse_errors_remain_visible_gaps() {
    let source =
        "CREATE INDEX ON item(id); CREATE SCHEMA AUTHORIZATION owner; CREATE TABLE broken (";
    let result = output(source);
    assert!(!result.document().skipped_regions.is_empty());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "owner")
    );
}
