//! SQL source declarations through the actual analyzer and validated IR.
//! Source ownership is distinct from a live catalog or resolved database semantics.

use super::*;

pub(super) const SQL: LanguageCase = LanguageCase {
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

#[test]
fn sql_function_signatures_preserve_return_contracts_without_bodies() {
    for (header, body) in [
        (
            "CREATE FUNCTION app.identity(value INT) RETURNS INT",
            "AS $$ SELECT value; $$ LANGUAGE SQL;",
        ),
        (
            "CREATE FUNCTION app.rows(value INT) RETURNS TABLE (id INT, label TEXT)",
            "AS $$ SELECT value, 'label'; $$ LANGUAGE SQL;",
        ),
        (
            "CREATE FUNCTION app.items() RETURNS SETOF app.item",
            "AS 'SELECT * FROM app.item;' LANGUAGE SQL;",
        ),
        (
            "CREATE OR REPLACE FUNCTION app.identity(value INT) RETURNS BIGINT LANGUAGE SQL IMMUTABLE",
            "RETURN value;",
        ),
        (
            "CREATE FUNCTION app.identity(value INT) RETURNS INT LANGUAGE SQL",
            "BEGIN ATOMIC SELECT value; END;",
        ),
        (
            "CREATE FUNCTION \"AS body\"(value INT) RETURNS INT /* AS is not the body */",
            "AS $$ SELECT value; $$ LANGUAGE SQL;",
        ),
    ] {
        let source = format!("{header}\n{body}");
        let result = output(&source);
        assert!(
            result.document().diagnostics.is_empty(),
            "{source}: {:?}",
            result.document().diagnostics
        );
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function)
            .unwrap();
        let signatures = result
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
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1, "{source}");
        let (extension, signature) = &signatures[0];
        assert_eq!(signature.text(), header, "{source}");
        assert!(!signature.is_truncated());
        let reference = extension.evidence.source.as_ref().unwrap();
        let start = usize::try_from(reference.span().start_byte()).unwrap();
        let end = usize::try_from(reference.span().end_byte()).unwrap();
        assert_eq!(source.get(start..end), Some(header));
    }
}

#[test]
fn sql_return_type_changes_invalidate_callable_identity_but_not_unrelated_objects() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SQL);
    let fixture = Fixture::new(SQL, SQL.source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, SQL, &budget),
        &ExtensionSupport::default(),
    );
    let changed = fixture.rewrite(
        SQL.source
            .replace("RETURNS INT", "RETURNS BIGINT")
            .as_bytes(),
    );
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, SQL, &budget),
        &ExtensionSupport::default(),
    );
    let callable = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function)
            .unwrap()
            .id
    };
    assert_ne!(callable(&first), callable(&next));
    let objects = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::DatabaseObject)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(objects(&first), objects(&next));
}

#[test]
fn sql_tsql_routines_keep_raw_names_and_direct_parameter_and_local_owners() {
    let source = "CREATE OR ALTER PROCEDURE [work].[Step]]Name]\n\
                  @Input int, @Output nvarchar(max) = 'AS body' OUTPUT,\n\
                  @Rows work.RowType READONLY\n\
                  AS BEGIN\n\
                  DECLARE @Local int = 1, @Other nvarchar(max) = 'item';\n\
                  DECLARE @Items TABLE ([ItemId] int, [Label] nvarchar(max));\n\
                  SELECT @Local;\n\
                  END;";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let routine = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "[work].[Step]]Name]")
        .unwrap();
    assert_eq!(routine.kind, EntityKind::Function);
    for (name, kind) in [
        ("@Input", EntityKind::Parameter),
        ("@Output", EntityKind::Parameter),
        ("@Rows", EntityKind::Parameter),
        ("@Local", EntityKind::Variable),
        ("@Other", EntityKind::Variable),
        ("@Items", EntityKind::Variable),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind, "{name}");
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(routine.id))
        );
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
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
    let table_variable = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "@Items")
        .unwrap();
    for name in ["[ItemId]", "[Label]"] {
        let column = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(column.kind, EntityKind::Field);
        assert_eq!(
            column.container,
            Some(rootlight_ir::ContainerRef::Entity(table_variable.id))
        );
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn sql_tsql_table_names_and_columns_preserve_bracket_escapes_without_catalog_binding() {
    let source = "CREATE TABLE [work].[Items] ([Id] [int] IDENTITY(1, 1) PRIMARY KEY,\n\
                  [Display]]Name] [nvarchar](max) NOT NULL);";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let table = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "[work].[Items]")
        .unwrap();
    assert_eq!(table.kind, EntityKind::DatabaseObject);
    for name in ["[Id]", "[Display]]Name]"] {
        let column = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(column.kind, EntityKind::Field);
        assert_eq!(
            column.container,
            Some(rootlight_ir::ContainerRef::Entity(table.id))
        );
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
}

#[test]
fn sql_tsql_routine_signatures_use_native_as_and_keep_sibling_formals_separate() {
    let source = "CREATE PROC [work].[First] @Value int = 1 AS BEGIN SELECT @Value; END;\n\
                  ALTER PROCEDURE [work].[Second] @Value nvarchar(max) = 'BEGIN END AS' OUTPUT\n\
                  AS BEGIN SELECT @Value; END;";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let routines = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect::<Vec<_>>();
    assert_eq!(routines.len(), 2);
    let parameters = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    for routine in routines {
        assert_eq!(
            parameters
                .iter()
                .filter(|parameter| parameter.container
                    == Some(rootlight_ir::ContainerRef::Entity(routine.id)))
                .count(),
            1
        );
        let signatures = document
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(routine.id))
                .then_some(lexical)
            })
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1);
        let header = signatures[0].text();
        assert!(header.ends_with("AS"), "{header}");
        assert!(!header.contains("SELECT"), "{header}");
        assert!(header.contains(&routine.canonical_name));
    }
}

#[test]
fn sql_tsql_malformed_headers_and_quoted_ddl_never_fabricate_routine_definitions() {
    for source in [
        "CREATE PROCEDURE [work].[Incomplete] @Value AS BEGIN SELECT 1; END;",
        "CREATE PROCEDURE [work].[Incomplete] @Value int BEGIN SELECT 1; END;",
        "SELECT 'CREATE PROCEDURE [work].[Quoted] @Value int AS BEGIN SELECT 1; END;';",
    ] {
        let result = output(source);
        assert!(
            !result.document().entities.iter().any(|entity| {
                matches!(entity.kind, EntityKind::Function | EntityKind::Parameter)
            }),
            "{source}: {:?}",
            result.document().entities
        );
        assert_ne!(
            result.report().coverage().status(),
            CoverageStatus::Complete
        );
    }
}

#[test]
fn sql_tsql_rejected_routines_do_not_reparent_locals_or_columns_to_the_file() {
    let source = "CREATE PROC [work].[Broken] @Input int AS BEGIN\n\
                  DECLARE @Leaked int;\n\
                  DECLARE @LeakedTable TABLE ([LeakedColumn] int);\n\
                  IF @Input > 0 PRINT 'unsupported'; END;\n\
                  CREATE PROC [work].[Valid] @Kept int AS BEGIN\n\
                  DECLARE @Owned int; SELECT @Kept; END;";
    let result = output(source);
    let document = result.document();
    for name in [
        "[work].[Broken]",
        "@Input",
        "@Leaked",
        "@LeakedTable",
        "[LeakedColumn]",
    ] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "recovered owner leaked {name}: {:?}",
            document.entities
        );
    }
    let routine = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "[work].[Valid]")
        .unwrap();
    for name in ["@Kept", "@Owned"] {
        let child = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(
            child.container,
            Some(rootlight_ir::ContainerRef::Entity(routine.id))
        );
    }
    let body_start = u64::try_from(source.find("BEGIN").unwrap()).unwrap();
    let body_end = u64::try_from(source.find("END;").unwrap() + "END".len()).unwrap();
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "sql-routine-body-semantics-unavailable")
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 3);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let gap = gaps.iter().find(|gap| gap.domain == domain).unwrap();
        assert_eq!(gap.source.span().start_byte(), body_start);
        assert_eq!(gap.source.span().end_byte(), body_end);
        assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
    }
    assert!(!document.skipped_regions.is_empty());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn sql_tsql_closed_batch_branches_preserve_table_column_owners() {
    let source = "SET ANSI_NULLS ON\r\nGO\r\n\
                  IF NOT EXISTS (SELECT 1 FROM sys.objects) BEGIN\r\n\
                  CREATE TABLE [work].[First] ([Id] INT,\r\n\
                  CONSTRAINT [PK_First] PRIMARY KEY CLUSTERED ([Id])\r\n\
                  WITH (PAD_INDEX = OFF, ALLOW_ROW_LOCKS = ON))\r\n\
                  END\r\nGO\r\n\
                  CREATE TABLE [work].[Second] ([Id] INT)\r\nGO\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let mut ids = Vec::new();
    for name in ["[work].[First]", "[work].[Second]"] {
        let table = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(table.kind, EntityKind::DatabaseObject);
        let columns = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Field
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(table.id))
            })
            .collect::<Vec<_>>();
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].canonical_name, "[Id]");
        ids.push(columns[0].id);
    }
    assert_ne!(ids[0], ids[1]);
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn sql_tsql_unclosed_branch_cannot_publish_its_recovered_child_declarations() {
    let source = "IF EXISTS (SELECT 1) BEGIN\n\
                  CREATE TABLE [work].[Leaked] ([LeakedColumn] INT)\n\
                  GO\nCREATE TABLE [work].[After] ([KeptColumn] INT)\nGO\n";
    let result = output(source);
    let document = result.document();
    for name in ["[work].[Leaked]", "[LeakedColumn]"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "recovered branch leaked {name}: {:?}",
            document.entities
        );
    }
    assert!(!document.skipped_regions.is_empty());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}
