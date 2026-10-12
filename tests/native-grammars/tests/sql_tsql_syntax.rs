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
    assert_eq!(
        text(object, source),
        "[Store] /* component */ . [Ops] . [Re]]pair]"
    );
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
        assert_eq!(
            owner(*variable, "tsql_declare").unwrap().kind(),
            "tsql_declare"
        );
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
    assert!(
        nodes(root, "identifier")
            .iter()
            .any(|node| text(*node, source) == "[Display name]")
    );
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
    assert_eq!(
        text(field(second_body, "statement"), source),
        "SELECT @Text"
    );
    assert_eq!(nodes(procedures[0], "tsql_parameter").len(), 1);
    assert_eq!(nodes(procedures[1], "tsql_parameter").len(), 1);
    assert_eq!(nodes(procedures[0], "create_table").len(), 0);
    assert_eq!(nodes(procedures[1], "create_table").len(), 0);
    let table = nodes(root, "create_table")[0];
    assert!(owner(table, "tsql_procedure").is_none());
}

#[test]
fn unsupported_control_flow_retains_a_real_header_and_rejected_body() {
    let source = "ALTER PROC [app].[Choose] @Value INT AS BEGIN\n\
        IF @Value > 0 PRINT N'positive'; RETURN 0; END;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedures = nodes(root, "tsql_procedure");
    assert_eq!(procedures.len(), 1, "{}", root.to_sexp());
    let header = field(procedures[0], "header");
    assert!(!header.has_error(), "{}", root.to_sexp());
    assert_eq!(text(field(header, "object"), source), "[app].[Choose]");
    let body = field(procedures[0], "body");
    assert_eq!(body.kind(), "tsql_rejected_body");
    let unsupported = nodes(body, "tsql_unbraced_print");
    assert_eq!(unsupported.len(), 1);
    assert_eq!(
        text(unsupported[0], source),
        "IF @Value > 0 PRINT N'positive'"
    );
    assert!(!field(body, "begin").is_missing());
    assert!(!field(body, "end").is_missing());
    assert_eq!(text(field(body, "end"), source), "END");
}

#[test]
fn rejected_body_closes_before_the_next_physical_procedure() {
    let source = "CREATE PROC [app].[Rejected] @Input INT AS BEGIN\n\
        DECLARE @Hidden INT; IF @Input > 0 PRINT 'unavailable'; END;\n\
        CREATE PROC [app].[Valid] @Kept INT AS BEGIN SELECT @Kept; END;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedures = nodes(root, "tsql_procedure");
    assert_eq!(procedures.len(), 2);
    let rejected = field(procedures[0], "body");
    assert_eq!(rejected.kind(), "tsql_rejected_body");
    assert_eq!(text(field(rejected, "end"), source), "END");
    assert_eq!(rejected.end_byte(), source.find("END;").unwrap() + 3);
    assert!(rejected.end_byte() < procedures[1].start_byte());
    assert_eq!(field(procedures[1], "body").kind(), "tsql_body");
    assert_eq!(
        text(field(field(procedures[1], "header"), "object"), source),
        "[app].[Valid]"
    );
    assert_eq!(nodes(procedures[0], "tsql_parameter").len(), 1);
    assert_eq!(nodes(procedures[1], "tsql_parameter").len(), 1);
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
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
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
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
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
    assert!(
        delimiters
            .iter()
            .all(|node| text(*node, source) == "$body$")
    );
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

#[test]
fn script_batches_preserve_guarded_tables_and_key_option_ownership() {
    let source = "SET ANSI_NULLS ON\r\nGO\r\nSET QUOTED_IDENTIFIER OFF\r\nGO\r\n\
        IF NOT EXISTS (SELECT 1 FROM sys.objects WHERE name = N'Entries')\r\n\
        BEGIN\r\nCREATE TABLE [store].[Entries] (\r\n\
        [Id] [int] IDENTITY(1,1) NOT NULL, [Name] [nvarchar](MAX) NULL,\r\n\
        CONSTRAINT [PK_Entries] PRIMARY KEY CLUSTERED ([Id] ASC)\r\n\
        WITH (PAD_INDEX = OFF, STATISTICS_NORECOMPUTE = OFF, IGNORE_DUP_KEY = OFF,\r\n\
        ALLOW_ROW_LOCKS = ON, ALLOW_PAGE_LOCKS = ON, FILLFACTOR = 80))\r\n\
        END\r\nGO\r\n\
        IF NOT EXISTS (SELECT 1 FROM sys.objects WHERE name = N'Names')\r\n\
        BEGIN\r\nCREATE TABLE [store].[Names] ([Id] [bigint], [Name] [nvarchar](40),\r\n\
        CONSTRAINT [UQ_Names] UNIQUE NONCLUSTERED ([Name] DESC, [Id] ASC)\r\n\
        WITH (IGNORE_DUP_KEY = ON))\r\nEND\r\nGO\r\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "tsql_script").len(), 1);
    assert_eq!(nodes(root, "tsql_batch").len(), 4);
    assert_eq!(nodes(root, "tsql_batch_separator").len(), 4);
    let settings = nodes(root, "tsql_set_option");
    assert_eq!(settings.len(), 2);
    for (setting, name, value) in [
        (settings[0], "ANSI_NULLS", "ON"),
        (settings[1], "QUOTED_IDENTIFIER", "OFF"),
    ] {
        assert_eq!(text(field(setting, "option"), source), name);
        assert_eq!(text(field(setting, "value"), source), value);
    }
    let conditions = nodes(root, "tsql_if");
    let tables = nodes(root, "create_table");
    assert_eq!(conditions.len(), 2);
    assert_eq!(tables.len(), 2);
    for (conditional, table, expected) in [
        (conditions[0], tables[0], "[store].[Entries]"),
        (conditions[1], tables[1], "[store].[Names]"),
    ] {
        let branch = field(conditional, "consequence");
        assert_eq!(branch.kind(), "tsql_conditional_block");
        assert_eq!(text(field(branch, "begin"), source), "BEGIN");
        assert_eq!(text(field(branch, "end"), source), "END");
        assert_eq!(owner(table, "tsql_if"), Some(conditional));
        assert_eq!(text(nodes(table, "object_reference")[0], source), expected);
        assert_eq!(nodes(table, "column_definition").len(), 2);
        assert_eq!(nodes(table, "constraint").len(), 1);
    }
    let storage = nodes(root, "tsql_key_storage");
    assert_eq!(storage.len(), 2);
    assert_eq!(text(storage[0], source), "CLUSTERED");
    assert_eq!(text(storage[1], source), "NONCLUSTERED");
    let options = nodes(root, "tsql_key_options");
    assert_eq!(options.len(), 2);
    for (option, table) in options.iter().zip(&tables) {
        assert_eq!(owner(*option, "create_table"), Some(*table));
        assert_eq!(
            owner(*option, "constraint"),
            Some(nodes(*table, "constraint")[0])
        );
    }
    let settings = nodes(root, "tsql_index_option");
    assert_eq!(settings.len(), 7);
    assert!(
        settings
            .iter()
            .all(|node| owner(*node, "tsql_key_options").is_some())
    );
}

#[test]
fn closed_conditional_branches_keep_nested_and_sibling_table_extents() {
    let source = "IF EXISTS (SELECT 1 FROM sys.objects) BEGIN\n\
        IF 1 = 1 BEGIN CREATE TABLE [app].[Inside] ([Id] INT); END\n\
        ELSE BEGIN CREATE TABLE [app].[Other] ([Id] INT); END\n\
        END\nGO\nCREATE TABLE [app].[After] ([Id] INT)\nGO\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let conditions = nodes(root, "tsql_if");
    let tables = nodes(root, "create_table");
    assert_eq!(conditions.len(), 2);
    assert_eq!(tables.len(), 3);
    assert_eq!(owner(conditions[1], "tsql_if"), Some(conditions[0]));
    assert_eq!(owner(tables[0], "tsql_if"), Some(conditions[1]));
    assert_eq!(owner(tables[1], "tsql_if"), Some(conditions[1]));
    assert!(owner(tables[2], "tsql_if").is_none());
    let alternative = field(conditions[1], "alternative");
    assert!(tables[1].start_byte() > field(alternative, "begin").end_byte());
    assert!(tables[1].end_byte() < field(alternative, "end").start_byte());
    assert!(conditions[0].end_byte() < tables[2].start_byte());
}

#[test]
fn physical_go_lines_separate_routines_without_expanding_their_bodies() {
    let source = "CREATE PROC [app].[First] @Value INT AS BEGIN RETURN @Value END\n\
        gO 2 -- a physical utility boundary\n\
        ALTER PROC [app].[Second] AS SELECT 2\n\
          GO\nCREATE TABLE [app].[After] ([Id] INT)\nGO";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedures = nodes(root, "tsql_procedure");
    let separators = nodes(root, "tsql_batch_separator");
    assert_eq!(procedures.len(), 2);
    assert_eq!(separators.len(), 3);
    assert!(text(separators[0], source).starts_with("gO 2"));
    assert_eq!(text(separators[2], source), "GO");
    assert!(procedures[0].end_byte() <= separators[0].start_byte());
    assert!(procedures[1].start_byte() >= separators[0].end_byte());
    assert!(procedures[1].end_byte() <= separators[1].start_byte());
    assert_eq!(text(field(procedures[1], "body"), source), "SELECT 2");
    let table = nodes(root, "create_table")[0];
    assert!(owner(table, "tsql_procedure").is_none());
    assert!(table.start_byte() >= separators[1].end_byte());
}

#[test]
fn inline_keys_and_legacy_fillfactor_keep_exact_written_options() {
    let source = "CREATE TABLE [app].[Entries] (\n\
        [Id] INT PRIMARY KEY NONCLUSTERED WITH (ALLOW_ROW_LOCKS = OFF),\n\
        [Code] INT UNIQUE CLUSTERED WITH FILLFACTOR = 70,\n\
        CONSTRAINT [PK_Extra] PRIMARY KEY ([Id] DESC) WITH FILLFACTOR = 90);";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let columns = nodes(root, "column_definition");
    assert_eq!(columns.len(), 2);
    let options = nodes(root, "tsql_key_options");
    assert_eq!(options.len(), 3);
    assert_eq!(owner(options[0], "column_definition"), Some(columns[0]));
    assert_eq!(owner(options[1], "column_definition"), Some(columns[1]));
    assert!(owner(options[2], "column_definition").is_none());
    assert_eq!(
        owner(options[2], "constraint"),
        Some(nodes(root, "constraint")[0])
    );
    for (fillfactor, expected) in nodes(root, "tsql_fillfactor").iter().zip(["70", "90"]) {
        assert_eq!(text(field(*fillfactor, "value"), source), expected);
    }
}

#[test]
fn batch_boundaries_and_key_options_reject_untyped_or_malformed_forms() {
    for source in [
        "SELECT 1 GO\nSELECT 2;",
        "SELECT 1;\nGO;\nSELECT 2;",
        "SELECT 1\nGO 0\nSELECT 2;",
        "SELECT 1\nGO -2\nSELECT 2;",
        "SELECT 1\nGO 2 SELECT 3\n",
        "CREATE PROC [app].[Read] AS SELECT 1 SELECT 2\nGO\n",
        "IF EXISTS (SELECT 1) CREATE TABLE [app].[Unbraced] ([Id] INT)\nGO\n",
        "CREATE TABLE [app].[Bad] ([Id] INT, PRIMARY KEY CLUSTERED ([Id]) WITH (PAD_INDEX = 'ON'));",
        "CREATE TABLE [app].[Bad] ([Id] INT, PRIMARY KEY ([Id]) WITH (UNKNOWN_SETTING = OFF));",
        "CREATE TABLE [app].[Bad] ([Id] INT, PRIMARY KEY ([Id]) WITH (FILLFACTOR =));",
        "SELECT 1 SELECT 2;",
    ] {
        let tree = parse(source);
        let root = tree.root_node();
        assert!(root.has_error(), "{source}: {}", root.to_sexp());
        assert_eq!(root.end_byte(), source.len());
    }
}

#[test]
fn go_aliases_preserve_ordinary_sql_without_a_utility_boundary() {
    for source in ["SELECT 1\nGO;\nSELECT 2;", "SELECT 1 AS GO;\nSELECT 2;"] {
        let tree = parse(source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{source}: {}", root.to_sexp());
        assert_eq!(root.end_byte(), source.len());
        assert!(nodes(root, "tsql_batch_separator").is_empty());
        assert!(nodes(root, "tsql_script").is_empty());
        let statements = nodes(root, "statement");
        assert_eq!(statements.len(), 2);
        let terms = nodes(root, "term");
        assert_eq!(terms.len(), 2);
        let alias = field(terms[0], "alias");
        assert_eq!(alias.kind(), "identifier");
        assert_eq!(text(alias, source), "GO");
        assert!(terms[1].child_by_field_name("alias").is_none());
    }
}

#[test]
fn incomplete_conditional_or_routine_blocks_cannot_swallow_the_next_batch() {
    for source in [
        "IF EXISTS (SELECT 1) BEGIN CREATE TABLE [app].[First] ([Id] INT)\nGO\nCREATE TABLE [app].[After] ([Id] INT)\nGO\n",
        "CREATE PROC [app].[First] AS BEGIN SELECT 1\nGO\nALTER PROC [app].[After] AS BEGIN RETURN 0 END\nGO\n",
    ] {
        let tree = parse(source);
        let root = tree.root_node();
        assert!(root.has_error(), "{source}: {}", root.to_sexp());
        let boundary = source.find("\nGO\n").unwrap();
        for kind in ["tsql_if", "tsql_procedure"] {
            for node in nodes(root, kind) {
                if !node.has_error() {
                    assert!(
                        node.end_byte() <= boundary || node.start_byte() > boundary,
                        "{}",
                        node.to_sexp()
                    );
                }
            }
        }
    }
}

#[test]
fn go_inside_written_strings_names_and_comments_is_not_a_batch_separator() {
    let source = "CREATE TABLE [app].[GO] ([GO] INT);\n\
        SELECT 'GO', $$\nGO\n$$, [GO] FROM [app].[GO];\n\
        -- GO\n/*\nGO\n*/";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(nodes(root, "tsql_batch_separator").is_empty());
    assert!(nodes(root, "tsql_script").is_empty());
    assert_eq!(nodes(root, "create_table").len(), 1);
}
