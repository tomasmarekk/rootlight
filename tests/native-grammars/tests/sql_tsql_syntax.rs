//! Written T-SQL declarations through the pinned native SQL parser.
//! Typed headers and bounded bodies preserve raw source identity without
//! treating batch separators or unsupported control flow as whitespace.

use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    node.utf8_text(source.as_bytes()).unwrap()
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Node<'tree> {
    node.child_by_field_name(name).unwrap_or_else(|| {
        panic!("missing {name} field: {}", node.to_sexp());
    })
}

fn datatype<'tree>(node: Node<'tree>) -> Node<'tree> {
    // The existing hidden SQL type rule exposes custom_type directly instead
    // of the enclosing type field; both must retain the complete written type.
    node.child_by_field_name("type")
        .or_else(|| node.child_by_field_name("custom_type"))
        .unwrap_or_else(|| panic!("missing datatype field: {}", node.to_sexp()))
}

fn owner<'tree>(mut node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == kind {
            return Some(parent);
        }
        node = parent;
    }
    None
}

#[test]
fn procedure_headers_preserve_qualified_names_and_physical_parameters() {
    let source = "CREATE OR ALTER PROC [Store] /* component */ . [Ops] . [Re]]pair]\n\
        @Café AS NVARCHAR(MAX) = N'π' OUTPUT,\n\
        @Rows [Ops].[RowType] READONLY, @Count INT = -2 OUT\n\
        AS BEGIN SET NOCOUNT ON; RETURN (0); END;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedures = nodes(root, "tsql_procedure");
    assert_eq!(procedures.len(), 1);
    let procedure = procedures[0];
    let header = field(procedure, "header");
    assert_eq!(header.kind(), "tsql_procedure_header");
    let object = field(header, "object");
    assert_eq!(object.kind(), "object_reference");
    assert_eq!(text(object, source), "[Store] /* component */ . [Ops] . [Re]]pair]");
    assert_eq!(text(field(object, "database"), source), "[Store]");
    assert_eq!(text(field(object, "schema"), source), "[Ops]");
    assert_eq!(text(field(object, "name"), source), "[Re]]pair]");
    let as_keyword = field(procedure, "as");
    assert_eq!(text(as_keyword, source), "AS");
    assert_eq!(&source[header.end_byte()..as_keyword.start_byte()], "\n");
    let parameters = nodes(header, "tsql_parameter");
    assert_eq!(parameters.len(), 3);
    for (parameter, name, mode) in [
        (parameters[0], "@Café", "OUTPUT"),
        (parameters[1], "@Rows", "READONLY"),
        (parameters[2], "@Count", "OUT"),
    ] {
        assert_eq!(text(field(parameter, "name"), source), name);
        assert_eq!(text(field(parameter, "mode"), source), mode);
        assert_eq!(owner(parameter, "tsql_procedure"), Some(procedure));
        assert!(parameter.end_byte() <= as_keyword.start_byte());
    }
    assert_eq!(text(field(parameters[0], "type"), source), "NVARCHAR(MAX)");
    assert_eq!(text(datatype(parameters[1]), source), "[Ops].[RowType]");
    assert_eq!(text(field(parameters[0], "default"), source), "N'π'");
    assert_eq!(text(field(parameters[2], "default"), source), "-2");
    let body = field(procedure, "body");
    assert_eq!(text(field(body, "begin"), source), "BEGIN");
    assert_eq!(text(field(body, "end"), source), "END");
    assert_eq!(text(procedure, source), source.trim_end_matches(';'));
}

#[test]
fn scalar_and_table_declarations_preserve_direct_written_owners() {
    let source = "ALTER PROCEDURE [app].[Collect] @Input INT AS BEGIN\n\
        DECLARE @Total AS INT = 2, @Label NVARCHAR(MAX) = N'raw';\n\
        DECLARE @Rows TABLE ([Id] [int] IDENTITY(1, 2) PRIMARY KEY,\n\
        [Te]]xt] [nvarchar](MAX) NOT NULL, [Value] [decimal](12, 3));\n\
        BEGIN DECLARE @Nested INT; SELECT @Input; END;\n\
        SET @Total = 3; RETURN @Total; END;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedure = nodes(root, "tsql_procedure")[0];
    let variables = nodes(root, "tsql_variable");
    assert_eq!(variables.len(), 3);
    for (variable, name) in variables.iter().zip(["@Total", "@Label", "@Nested"]) {
        assert_eq!(text(field(*variable, "name"), source), name);
        assert_eq!(owner(*variable, "tsql_procedure"), Some(procedure));
        assert_eq!(owner(*variable, "tsql_declare").unwrap().kind(), "tsql_declare");
    }
    let tables = nodes(root, "tsql_table_variable");
    assert_eq!(tables.len(), 1);
    assert_eq!(text(field(tables[0], "name"), source), "@Rows");
    assert_eq!(text(field(tables[0], "type"), source), "TABLE");
    assert_eq!(owner(tables[0], "tsql_procedure"), Some(procedure));
    let columns = nodes(field(tables[0], "columns"), "column_definition");
    assert_eq!(columns.len(), 3);
    for (column, name, expected_type) in [
        (columns[0], "[Id]", "[int]"),
        (columns[1], "[Te]]xt]", "[nvarchar](MAX)"),
        (columns[2], "[Value]", "[decimal](12, 3)"),
    ] {
        assert_eq!(text(field(column, "name"), source), name);
        assert_eq!(text(datatype(column), source), expected_type);
        assert_eq!(owner(column, "tsql_table_variable"), Some(tables[0]));
    }
    let identity = nodes(root, "tsql_identity");
    assert_eq!(identity.len(), 1);
    assert_eq!(text(field(identity[0], "seed"), source), "1");
    assert_eq!(text(field(identity[0], "increment"), source), "2");
}

#[test]
fn standalone_declarations_and_bracketed_tables_have_exact_spans() {
    let source = "DECLARE @Counter INT = 1, @Payload VARBINARY(MAX);\n\
        DECLARE @Items AS TABLE ([Key] INT IDENTITY, [Name] VARCHAR(MAX));\n\
        CREATE TABLE [catalog].[Odd]]Name] ([Sequence] [bigint] IDENTITY(-3, 1),\n\
        [Display name] [nvarchar](80), [Bytes] VARBINARY(MAX));";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "tsql_declare").len(), 2);
    assert_eq!(nodes(root, "tsql_variable").len(), 2);
    assert_eq!(nodes(root, "tsql_table_variable").len(), 1);
    let tables = nodes(root, "create_table");
    assert_eq!(tables.len(), 1);
    let objects = nodes(tables[0], "object_reference");
    assert_eq!(text(objects[0], source), "[catalog].[Odd]]Name]");
    assert!(owner(tables[0], "tsql_procedure").is_none());
    assert!(nodes(root, "identifier").iter().any(|node| text(*node, source) == "[Display name]"));
}

#[test]
fn closed_and_single_statement_bodies_do_not_absorb_sibling_declarations() {
    let source = "CREATE PROC [app].[First] @Value INT AS BEGIN\n\
        DECLARE @Local INT; BEGIN SELECT @Value; END; RETURN 0; END;\n\
        ALTER PROCEDURE [app].[Second] @Text NVARCHAR(20) AS SELECT @Text;\n\
        CREATE TABLE [app].[After] ([Id] INT);";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedures = nodes(root, "tsql_procedure");
    assert_eq!(procedures.len(), 2);
    let first_body = field(procedures[0], "body");
    assert!(field(first_body, "end").end_byte() < procedures[1].start_byte());
    let second_body = field(procedures[1], "body");
    assert!(second_body.child_by_field_name("begin").is_none());
    assert_eq!(text(field(second_body, "statement"), source), "SELECT @Text");
    assert_eq!(nodes(procedures[0], "tsql_parameter").len(), 1);
    assert_eq!(nodes(procedures[1], "tsql_parameter").len(), 1);
    assert_eq!(nodes(procedures[0], "create_table").len(), 0);
    assert_eq!(nodes(procedures[1], "create_table").len(), 0);
    let table = nodes(root, "create_table")[0];
    assert!(owner(table, "tsql_procedure").is_none());
}

#[test]
fn unsupported_control_flow_retains_a_real_header_and_body_error() {
    let source = "ALTER PROC [app].[Choose] @Value INT AS BEGIN\n\
        IF @Value > 0 PRINT N'positive'; RETURN 0; END;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(root.has_error());
    let procedures = nodes(root, "tsql_procedure");
    assert_eq!(procedures.len(), 1, "{}", root.to_sexp());
    let header = field(procedures[0], "header");
    assert!(!header.has_error(), "{}", root.to_sexp());
    assert_eq!(text(field(header, "object"), source), "[app].[Choose]");
    let body = field(procedures[0], "body");
    assert!(body.has_error());
    assert!(!field(body, "begin").is_missing());
    assert!(!field(body, "end").is_missing());
    assert_eq!(text(field(body, "end"), source), "END");
}

#[test]
fn malformed_headers_and_unterminated_bodies_remain_errors() {
    for source in [
        "CREATE PROC AS BEGIN SELECT 1; END;",
        "CREATE PROC @Invalid INT AS BEGIN SELECT 1; END;",
        "ALTER PROCEDURE [app].[Broken] @ Value INT AS BEGIN RETURN 0; END;",
        "CREATE PROC [app].[Broken] @Value AS BEGIN RETURN 0; END;",
        "CREATE PROC [app].[Broken] @Value INT BEGIN RETURN 0; END;",
        "CREATE PROC [app].[Broken] @Value INT AS BEGIN RETURN 0;",
        "CREATE PROC [app].[Bad]] @Value INT AS BEGIN RETURN 0; END;",
    ] {
        let tree = parse(source);
        assert!(tree.root_node().has_error(), "{source}: {}", tree.root_node().to_sexp());
        assert_eq!(tree.root_node().end_byte(), source.len());
        for procedure in nodes(tree.root_node(), "tsql_procedure") {
            assert!(
                procedure.has_error(),
                "malformed input acquired a complete routine: {source}: {}",
                procedure.to_sexp()
            );
        }
    }
}

#[test]
fn missing_end_or_batch_tokens_cannot_prove_a_swallowed_sibling_owner() {
    for source in [
        "CREATE PROC [app].[First] AS BEGIN DECLARE @Value INT;\n\
         CREATE PROC [app].[Second] AS BEGIN RETURN 0; END;",
        "CREATE PROC [app].[First] AS BEGIN SELECT 1; GO\n\
         ALTER PROC [app].[Second] AS BEGIN RETURN 0; END;",
    ] {
        let tree = parse(source);
        let root = tree.root_node();
        assert!(root.has_error(), "{source}: {}", root.to_sexp());
        let sibling_start = source
            .find("CREATE PROC [app].[Second]")
            .or_else(|| source.find("ALTER PROC [app].[Second]"))
            .unwrap();
        for procedure in nodes(root, "tsql_procedure") {
            let header = field(procedure, "header");
            if text(field(header, "object"), source) == "[app].[First]" {
                let body = field(procedure, "body");
                assert!(body.has_error() || body.end_byte() <= sibling_start);
            }
            if !procedure.has_error() {
                assert!(
                    procedure.end_byte() <= sibling_start
                        || procedure.start_byte() >= sibling_start,
                    "a clean native routine swallowed the next declaration: {}",
                    procedure.to_sexp()
                );
            }
        }
    }
}

#[test]
fn go_and_unbraced_statement_lists_are_not_implicit_body_delimiters() {
    for source in [
        "CREATE PROC [app].[First] AS SELECT 1 GO ALTER PROC [app].[Second] AS SELECT 2;",
        "CREATE PROC [app].[First] AS SELECT 1 SELECT 2;",
        "DECLARE @Items TABLE ([Id] INT) GO SELECT @Items;",
    ] {
        let tree = parse(source);
        assert!(tree.root_node().has_error(), "{source}: {}", tree.root_node().to_sexp());
        assert_eq!(tree.root_node().end_byte(), source.len());
    }
}

#[test]
fn brackets_do_not_replace_postgres_arrays_or_dollar_function_owners() {
    let source = "CREATE TABLE app.arrays (values INT[3], names TEXT ARRAY[2]);\n\
        SELECT values[1], ARRAY[1, 2] FROM app.arrays;\n\
        CREATE FUNCTION app.bump(value INT) RETURNS INT AS $body$\n\
        DECLARE amount INT := 1; BEGIN SELECT value + amount; END;\n\
        $body$ LANGUAGE SQL;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "array_size_definition").len(), 2);
    assert_eq!(nodes(root, "subscript").len(), 1);
    assert_eq!(nodes(root, "array").len(), 1);
    assert_eq!(nodes(root, "tsql_procedure").len(), 0);
    assert_eq!(nodes(root, "tsql_declare").len(), 0);
    let functions = nodes(root, "create_function");
    assert_eq!(functions.len(), 1);
    assert_eq!(nodes(functions[0], "function_declaration").len(), 1);
    let delimiters = nodes(functions[0], "dollar_quote");
    assert_eq!(delimiters.len(), 2);
    assert!(delimiters.iter().all(|node| text(*node, source) == "$body$"));
}

#[test]
fn bracket_names_remain_available_in_unambiguous_name_positions() {
    let source = "CREATE TABLE [app].[1] ([Odd]]name] NVARCHAR(MAX));\n\
        SELECT [Odd]]name] AS [Display name] FROM [app].[1];";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    for expected in ["[app]", "[1]", "[Odd]]name]", "[Display name]"] {
        assert!(
            nodes(root, "identifier")
                .iter()
                .any(|node| text(*node, source) == expected),
            "missing raw name {expected}: {}",
            root.to_sexp()
        );
    }
    assert_eq!(nodes(root, "subscript").len(), 0);
}
