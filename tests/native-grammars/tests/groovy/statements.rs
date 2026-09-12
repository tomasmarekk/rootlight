//! Statement boundaries must preserve meaning, not merely remove parser errors.
//! Authored Groovy fixtures are parsed only, with explicit source-span assertions.

use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn valid(source: &str) -> Tree {
    let tree = parse(source);
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
    }
    tree
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, bool)> {
    let mut rows: Vec<_> = nodes(tree.root_node())
        .iter()
        .map(|n| (n.kind().into(), n.byte_range(), n.is_missing()))
        .collect();
    rows.sort_by(|a, b| (&a.0, a.1.start, a.1.end, a.2).cmp(&(&b.0, b.1.start, b.1.end, b.2)));
    rows
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        result.push(node);
    }
    result
}

fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|node| node.kind() == kind && node.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn array_creation_keeps_element_types_and_ordered_dimension_expressions() {
    for (expression, element, sizes) in [
        ("new String[0]", "String", vec![Some("0")]),
        ("new int[size]", "int", vec![Some("size")]),
        (
            "new sample.Widget[rows][columns]",
            "sample.Widget",
            vec![Some("rows"), Some("columns")],
        ),
        ("new String[rows][]", "String", vec![Some("rows"), None]),
    ] {
        let source = format!("def values = {expression}\n");
        let tree = valid(&source);
        let arrays: Vec<_> = nodes(tree.root_node())
            .into_iter()
            .filter(|node| node.kind() == "array_creation_expression")
            .collect();
        assert_eq!(arrays.len(), 1);
        let array = arrays[0];
        assert_eq!(array.utf8_text(source.as_bytes()).unwrap(), expression);
        assert_eq!(
            array
                .child_by_field_name("type")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            element
        );
        assert!(array.child_by_field_name("initializer").is_none());
        let mut cursor = array.walk();
        let dimensions: Vec<_> = array
            .children_by_field_name("dimension", &mut cursor)
            .collect();
        assert_eq!(dimensions.len(), sizes.len());
        for (dimension, expected) in dimensions.into_iter().zip(sizes) {
            assert_eq!(
                dimension
                    .child_by_field_name("size")
                    .map(|node| node.utf8_text(source.as_bytes()).unwrap()),
                expected
            );
        }
        assert!(
            !nodes(array)
                .iter()
                .any(|node| node.kind() == "subscript_expression")
        );
    }
}

#[test]
fn array_creation_initializers_keep_values_and_nested_array_ownership() {
    for (expression, dimensions, initializer, values) in [
        (
            "new String[]{'a', 'b'}",
            1,
            "{'a', 'b'}",
            vec!["'a'", "'b'"],
        ),
        (
            "new int[][]{new int[]{1}, new int[]{2, 3}}",
            2,
            "{new int[]{1}, new int[]{2, 3}}",
            vec!["new int[]{1}", "new int[]{2, 3}"],
        ),
    ] {
        let source = format!("def values = {expression}\n");
        let tree = valid(&source);
        let array = nodes(tree.root_node())
            .into_iter()
            .find(|node| {
                node.kind() == "array_creation_expression"
                    && node.utf8_text(source.as_bytes()).unwrap() == expression
            })
            .unwrap();
        let mut cursor = array.walk();
        let sizes: Vec<_> = array
            .children_by_field_name("dimension", &mut cursor)
            .collect();
        assert_eq!(sizes.len(), dimensions);
        assert!(
            sizes
                .iter()
                .all(|node| node.child_by_field_name("size").is_none())
        );
        let init = array.child_by_field_name("initializer").unwrap();
        assert_eq!(init.utf8_text(source.as_bytes()).unwrap(), initializer);
        let mut cursor = init.walk();
        let elements: Vec<_> = init.children_by_field_name("value", &mut cursor).collect();
        assert_eq!(
            elements
                .iter()
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect::<Vec<_>>(),
            values
        );
        assert!(elements.iter().all(|node| node.parent() == Some(init)));
    }
}

#[test]
fn array_creation_rejects_incomplete_and_reordered_dimensions() {
    for expression in [
        "new int[][]{{1}, {2, 3}}",
        "new int[]",
        "new int[][size]",
        "new int[1]{2}",
        "new int[]{transform value}",
        "new int[]()",
        "new int[1, 2]",
        "new int[1][][index]",
    ] {
        let source = format!("def values = {expression}\n");
        assert!(parse(&source).root_node().has_error(), "{source}");
    }
}

#[test]
fn array_creation_trivia_and_initializer_calls_preserve_boundaries() {
    for expression in [
        "new int[]{}",
        "new String[]{\n'λ',\n// boundary\n'x',\n}",
        "new T[rows + 1][columns][]",
        "new List<String>[size]",
        "new int[]{transform(value), fallback()}",
        "new int[rows] [columns]",
        "new int[rows /* λ */][]",
        "new int[\nrows\n]",
        "new int[]{\r\nvalue\r\n}",
    ] {
        let source = format!("def values = {expression}\nfinish()\n");
        let tree = valid(&source);
        exact(&tree, &source, "array_creation_expression", expression);
        exact(&tree, &source, "method_invocation", "finish()");
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|node| node.kind() == "closure")
        );
    }
}

#[test]
fn array_creation_is_distinct_from_post_creation_access_and_subscripts() {
    for (expression, kind) in [
        ("new int[rows].length", "field_access"),
        ("(new int[rows])[index]", "subscript_expression"),
    ] {
        let source = format!("def values = {expression}\n");
        let tree = valid(&source);
        exact(&tree, &source, kind, expression);
        exact(&tree, &source, "array_creation_expression", "new int[rows]");
    }
}

#[test]
fn array_creation_postfix_calls_distinguish_initializers_and_closures() {
    for (expression, array, kind, closures) in [
        ("(new int[1]){2}", "new int[1]", "method_invocation", 1),
        ("new int[]{1}{2}", "new int[]{1}", "method_invocation", 1),
        ("new int[]{1}(2)", "new int[]{1}", "method_invocation", 0),
        ("new int[1](2)", "new int[1]", "method_invocation", 0),
        ("(new int[1])(2)", "new int[1]", "method_invocation", 0),
        ("new int[1] value", "new int[1]", "command_chain", 0),
        (
            "new int[]{1}[index]",
            "new int[]{1}",
            "subscript_expression",
            0,
        ),
    ] {
        let source = format!("def values = {expression}\nfinish()\n");
        let tree = valid(&source);
        exact(&tree, &source, "array_creation_expression", array);
        exact(&tree, &source, kind, expression);
        exact(&tree, &source, "method_invocation", "finish()");
        assert_eq!(
            nodes(tree.root_node())
                .iter()
                .filter(|node| node.kind() == "closure")
                .count(),
            closures
        );
    }
}

#[test]
fn semicolons_separate_script_and_closure_statements() {
    let source = "def first = 1; def second = 2;\ndef run() { def a = 1; def b = 2; a + b; }\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "def first = 1");
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn closure_body_declarations_are_not_parameters_without_arrow() {
    for body in [
        "var value = 1; sink(value)",
        "final def value = 1; sink(value)",
        "String value = 'seed'; sink(value)",
        "value = 'seed'; sink(value)",
    ] {
        let source = format!("def action = {{ {body} }}\n");
        let tree = valid(&source);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|node| node.kind() == "closure_parameters")
        );
        exact(&tree, &source, "method_invocation", "sink(value)");
    }
    for source in [
        "def action = { String... -> sink() }",
        "def action = { value = -> sink() }",
        "def items = [[1],, [2]]",
        "class Reader { @Deprecated; void read() {} }",
    ] {
        assert!(
            parse(source).root_node().has_error(),
            "accepted malformed source: {source}"
        );
    }
}

fn closure_parameter_fields(header: &str, name: &str, ty: Option<&str>, default: Option<&str>) {
    let source = format!("def action = {{ {header} -> sink({name}) }}\n");
    let tree = valid(&source);
    let parameters: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|node| node.kind() == "closure_parameter")
        .collect();
    assert_eq!(parameters.len(), 1, "{source}");
    let parameter = parameters[0];
    assert_eq!(parameter.utf8_text(source.as_bytes()).unwrap(), header);
    for (field, expected) in [("name", Some(name)), ("type", ty), ("default", default)] {
        assert_eq!(
            parameter
                .child_by_field_name(field)
                .map(|node| node.utf8_text(source.as_bytes()).unwrap()),
            expected,
            "{field}: {source}"
        );
    }
    exact(
        &tree,
        &source,
        "method_invocation",
        &format!("sink({name})"),
    );
}

#[test]
fn closure_contextual_var_preserves_parameter_and_read() {
    closure_parameter_fields("var", "var", None, None);
    closure_parameter_fields("String var", "var", Some("String"), None);
}

#[test]
fn contextual_var_declarations_and_loop_names_keep_their_roles() {
    let source = "var var = 1; sink(var)\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "var var = 1");
    exact(&tree, source, "variable_declarator", "var = 1");
    for (header, name) in [("var in values", "var"), ("var value in values", "value")] {
        let source = format!("for ({header}) {{ sink({name}) }}\n");
        let tree = valid(&source);
        let loop_node = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "for_in_statement")
            .unwrap();
        assert_eq!(
            loop_node
                .child_by_field_name("variable")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            name
        );
    }
}

#[test]
fn annotation_trivia_keeps_other_declaration_contexts_and_exact_annotation_span() {
    for (source, kind) in [
        (
            "@Deprecated\n// boundary\nclass Reader {}",
            "class_declaration",
        ),
        (
            "class Reader { @Deprecated\n// boundary\nString value }",
            "field_declaration",
        ),
        (
            "def read(@Deprecated\n// boundary\nString value) { value }",
            "formal_parameter",
        ),
        (
            "try { work() } catch (@Deprecated\n// boundary\nException problem) { sink(problem) }",
            "catch_formal_parameter",
        ),
    ] {
        let tree = valid(source);
        let annotation = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "annotation")
            .unwrap();
        assert_eq!(
            annotation.utf8_text(source.as_bytes()).unwrap(),
            "@Deprecated"
        );
        assert_eq!(annotation.parent().unwrap().kind(), kind);
    }
}

#[test]
fn closure_varargs_preserve_parameter_type_and_body() {
    closure_parameter_fields("String... remaining", "remaining", Some("String..."), None);
    let source = "def action = { String first, String... remaining -> sink(first, remaining) }\n";
    let tree = valid(source);
    exact(&tree, source, "closure_parameter", "String first");
    exact(&tree, source, "closure_parameter", "String... remaining");
    exact(&tree, source, "method_invocation", "sink(first, remaining)");
}

#[test]
fn closure_parameter_modifiers_preserve_authored_fields() {
    for header in [
        "@Deprecated final String value",
        "final @Deprecated String value",
        "final def value",
    ] {
        closure_parameter_fields(
            header,
            "value",
            header.contains("String").then_some("String"),
            None,
        );
    }
}

#[test]
fn closure_parameter_defaults_preserve_expression_span() {
    closure_parameter_fields(
        "String value = 'seed'",
        "value",
        Some("String"),
        Some("'seed'"),
    );
    closure_parameter_fields("value = 'seed'", "value", None, Some("'seed'"));
}

#[test]
fn annotation_comment_trivia_preserves_method_owner() {
    for trivia in ["\n// boundary\n", "\r\n// λ\r\n", "\n/* boundary */\n\n"] {
        let declaration = format!("@Deprecated{trivia}void read() {{}}");
        let source = format!("class Reader {{ {declaration} }}\n");
        let tree = valid(&source);
        let method = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "method_declaration")
            .unwrap();
        assert_eq!(method.utf8_text(source.as_bytes()).unwrap(), declaration);
        assert_eq!(
            method
                .child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "read"
        );
        assert_eq!(
            nodes(method)
                .iter()
                .filter(|node| node.kind() == "annotation")
                .count(),
            1
        );
        exact(&tree, &source, "block", "{}");
    }
}

#[test]
fn list_comma_comment_trivia_preserves_nested_elements() {
    for trivia in ["\n\n// boundary\n", "\r\n// λ\r\n", "\n/* boundary */\n\n"] {
        let literal = format!("[[1],{trivia}[2],{trivia}]");
        let source = format!("def items = {literal}\n");
        let tree = valid(&source);
        exact(&tree, &source, "list_literal", &literal);
        exact(&tree, &source, "list_literal", "[1]");
        exact(&tree, &source, "list_literal", "[2]");
        let outer = nodes(tree.root_node())
            .into_iter()
            .find(|node| {
                node.kind() == "list_literal"
                    && node.utf8_text(source.as_bytes()).unwrap() == literal
            })
            .unwrap();
        let mut cursor = outer.walk();
        let elements: Vec<_> = outer
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "list_literal")
            .map(|node| node.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(elements, ["[1]", "[2]"]);
    }
}

#[test]
fn multiline_maps_remain_distinct_from_lists() {
    for trivia in ["\n", "\n// boundary\n", "\r\n/* λ */\r\n"] {
        let literal = format!("[{trivia}first: 1,{trivia}second: [2],{trivia}]");
        let source = format!("def values = {literal}\n");
        let tree = valid(&source);
        exact(&tree, &source, "map_literal", &literal);
        exact(&tree, &source, "map_entry", "first: 1");
        exact(&tree, &source, "map_entry", "second: [2]");
        exact(&tree, &source, "list_literal", "[2]");
    }
}

#[test]
fn assert_message_separators_preserve_closure_statement_ownership() {
    for separator in [",", ":"] {
        let assertion = format!("assert value != null{separator} 'missing'");
        let source = format!("def action = {{ value -> {assertion}; sink(value) }}\n");
        let tree = valid(&source);
        exact(&tree, &source, "assert_statement", &assertion);
        let node = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "assert_statement")
            .unwrap();
        assert_eq!(
            node.child_by_field_name("message")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "'missing'"
        );
        exact(&tree, &source, "method_invocation", "sink(value)");
    }
}

#[test]
fn semicolons_separate_fields_and_abstract_methods() {
    valid(
        "abstract class Meter { int value = 1; abstract int measure(); int read() { value; } }\n",
    );
}

#[test]
fn no_separator_between_declarations_is_rejected() {
    for source in [
        "def first = 1 def second = 2",
        "def run() { def first = 1 def second = 2 }",
    ] {
        assert!(
            parse(source).root_node().has_error(),
            "silently joined {source}"
        );
    }
}

#[test]
fn return_does_not_capture_next_line_call() {
    let source = "def run() { return\nfinish() }\n";
    let tree = valid(source);
    exact(&tree, source, "return_statement", "return");
    exact(&tree, source, "method_invocation", "finish()");
}

#[test]
fn leading_operator_starts_a_new_statement() {
    let source = "def run() { first\n+ second }\n";
    let tree = valid(source);
    exact(&tree, source, "expression_statement", "first");
    exact(&tree, source, "unary_expression", "+ second");
}

#[test]
fn trailing_operator_and_parentheses_allow_continuation() {
    for source in [
        "def value = first +\nsecond\n",
        "consume(\nfirst +\nsecond,\nthird\n)\n",
        "def values = [\nfirst,\nsecond\n]\n",
    ] {
        valid(source);
    }
}

#[test]
fn method_bodies_may_start_on_the_next_line() {
    valid("class Meter {\nint read()\n{\nreturn 1\n}\nint twice()\n{\nread() * 2\n}\n}\n");
}

#[test]
fn conditional_and_loop_bodies_keep_semicolons() {
    valid(
        "if (ready) first(); else second();\nwhile (ready) advance();\ndo { advance() } while (ready);\nfor (i = 0; i < 3; i++) advance();\n",
    );
}

#[test]
fn conditional_separators_do_not_require_an_else_clause() {
    for source in [
        "def run() { if (true) { def local = 1 }; if (false) { def local = 2 } }\n",
        "if (ready) first(); second()\n",
        "if (ready) { first() }; second()\n",
        "if (ready) { first() };\n",
        "if (ready) first(); else second(); third()\n",
        "if (ready) { first() }; else { second() }; third()\n",
        "if (outer) if (inner) first(); else second(); third()\n",
    ] {
        valid(source);
    }
    let source = "if (ready) { first() }; second()\n";
    let tree = valid(source);
    exact(&tree, source, "if_statement", "if (ready) { first() }");
    exact(&tree, source, "expression_statement", "second()");
    let source = "if (outer) if (inner) first(); else second(); third()\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "if_statement",
        "if (inner) first(); else second()",
    );
    exact(&tree, source, "expression_statement", "third()");
}

#[test]
fn newline_comments_do_not_hide_statement_boundaries() {
    let source = "def first = 1 // note\ndef second = 2\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "def first = 1");
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn crlf_and_blank_lines_preserve_exact_statements() {
    let source = "\r\ndef first = 'λ😀'\r\n\r\ndef second = 2;\r\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def first = 'λ😀'",
    );
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn conditional_alternatives_allow_newlines() {
    for source in [
        "if (ready) { first() }\nelse { second() }\n",
        "if (ready) first();\nelse second();\n",
    ] {
        let tree = valid(source);
        exact(
            &tree,
            source,
            "if_statement",
            source.trim_end().trim_end_matches(';'),
        );
    }
}

#[test]
fn do_while_condition_allows_newlines() {
    valid("do { advance() }\nwhile (ready)\n");
}

#[test]
fn catch_and_finally_allow_newlines() {
    valid("try { load() }\ncatch (Exception error) { recover(error) }\nfinally { close() }\n");
}

#[test]
fn method_throws_and_body_allow_newlines() {
    valid("class Meter {\nint read()\nthrows Exception\n{\nreturn 1\n}\n}\n");
}

#[test]
fn closure_parameters_allow_newlines() {
    valid("def run = {\nint value ->\nvalue + 1\n}\n");
}

#[test]
fn closure_arrow_trivia_preserves_parameters_and_body_ownership() {
    for header in ["->", "value ->", "String value ->"] {
        for trivia in [
            "\n",
            " // note\n",
            " // note\r\n\r\n",
            " /* note */\n",
            "\n// first\n/* second */\n",
        ] {
            for body in ["", "sink(value)"] {
                let closure = format!("{{ {header}{trivia}{body} }}");
                for source in [
                    format!("def action = {closure}\nfinish()\n"),
                    format!("items.each {closure}\nfinish()\n"),
                ] {
                    let tree = valid(&source);
                    exact(&tree, &source, "closure", &closure);
                    exact(&tree, &source, "closure_parameters", header);
                    exact(&tree, &source, "method_invocation", "finish()");
                    if !body.is_empty() {
                        let owner = nodes(tree.root_node())
                            .into_iter()
                            .find(|node| node.kind() == "closure")
                            .unwrap();
                        assert!(nodes(owner).iter().any(|node| {
                            node.kind() == "method_invocation"
                                && node.utf8_text(source.as_bytes()).unwrap() == body
                        }));
                    }
                }
            }
        }
    }
}

#[test]
fn leading_member_access_continues_expression() {
    let source = "def result = value\n.toString()\n";
    let tree = valid(source);
    exact(&tree, source, "method_invocation", "value\n.toString()");
}

#[test]
fn declaration_initializer_allows_newline_before_assignment() {
    let source = "def value\n= 1\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def value\n= 1",
    );
}

#[test]
fn do_while_rejects_nonblock_or_separated_bodies() {
    for source in [
        "do advance(); while (ready);",
        "do { advance() }; while (ready);",
    ] {
        assert!(
            parse(source).root_node().has_error(),
            "accepted invalid do-while: {source}"
        );
    }
}

#[test]
fn incremental_statement_edits_match_fresh_trees() {
    let source = "def run() { def first = 'λ😀'; return\nfinish() }\n";
    for (before, after) in [
        (";", "\r\n"),
        ("return\n", "return "),
        ("λ😀", "δ🌍!"),
        ("finish()", "finish(\nfirst\n)"),
    ] {
        let start = source.find(before).unwrap();
        let mut changed = source.to_owned();
        changed.replace_range(start..start + before.len(), after);
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let mut old = parser.parse(source, None).unwrap();
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        assert_eq!(
            signature(&parser.parse(&changed, Some(&old)).unwrap()),
            signature(&valid(&changed))
        );
    }
}

#[test]
fn included_statement_ranges_preserve_host_coordinates() {
    let prefix = "# λ😀\r\n```groovy\r\n";
    let body = "def first = 1;\r\ndef second = 2\r\n";
    let source = format!("{prefix}{body}```\r\n");
    let start = prefix.len();
    let end = start + body.len();
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser
        .set_included_ranges(&[tree_sitter::Range {
            start_byte: start,
            end_byte: end,
            start_point: point(&source, start),
            end_point: point(&source, end),
        }])
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert!(!tree.root_node().has_error());
    for node in nodes(tree.root_node()) {
        assert!(node.start_byte() >= start && node.end_byte() <= end);
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(&source, node.start_byte()));
        assert_eq!(node.end_position(), point(&source, node.end_byte()));
    }
    exact(
        &tree,
        &source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn malformed_statement_reset_cannot_retain_scanner_context() {
    let source = "def run() { return\nfinish() }\n";
    let expected = signature(&valid(source));
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    for damaged in [
        "def run() { return\n",
        "def x = value\n.",
        "def run() { def a = 1 def b = 2 }",
    ] {
        assert!(
            parser.parse(damaged, None).unwrap().root_node().has_error(),
            "{damaged}"
        );
        parser.reset();
        assert_eq!(signature(&parser.parse(source, None).unwrap()), expected);
    }
}
