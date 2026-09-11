//! Call controls retain source ownership across command arguments and closures.
//! Generic authored snippets are parsed only; no source or corpus code is executed.
use tree_sitter::{Node, Parser, Tree};

#[test]
fn growing_command_chains_keep_all_links_with_bounded_input_work() {
    let mut previous = None;
    for count in [16, 32, 64, 128] {
        let source = format!("route value{}\n", " then value".repeat(count));
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let mut reads = 0_usize;
        let tree = parser
            .parse_with_options(
                &mut |offset, _| {
                    reads += 1;
                    source
                        .as_bytes()
                        .get(offset..offset.saturating_add(1))
                        .unwrap_or_default()
                },
                None,
                None,
            )
            .unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(
            nodes(tree.root_node())
                .iter()
                .filter(|n| n.kind() == "command_link")
                .count(),
            count
        );
        assert!(
            reads <= source.len() * 64,
            "reads={reads}, bytes={}",
            source.len()
        );
        if let Some(prior) = previous {
            assert!(reads <= prior * 3, "{prior}->{reads}");
        }
        println!(
            "call-work links={count} bytes={} reads={reads}",
            source.len()
        );
        previous = Some(reads);
    }
}

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}
fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = vec![];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        result.push(node);
    }
    result
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
    }
    tree
}
fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == kind && n.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn command_arguments_preserve_complete_expressions() {
    for source in [
        "consume value",
        "service.consume value",
        "consume first + second * third",
        "consume build(value), [1, 2]",
        "consume (first + second)",
    ] {
        let tree = valid(source);
        let kind = if source == "consume (first + second)" {
            "method_invocation"
        } else {
            "command_chain"
        };
        exact(&tree, source, kind, source);
    }
}
#[test]
fn named_command_arguments_are_source_bound_and_not_labels() {
    let source = "publish name: 'λ😀', enabled: true, value";
    let tree = valid(source);
    exact(&tree, source, "command_chain", source);
    exact(&tree, source, "named_argument", "name: 'λ😀'");
    exact(&tree, source, "named_argument", "enabled: true");
    assert!(
        !nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == "labeled_statement")
    );
}
#[test]
fn command_chains_preserve_each_link() {
    for source in [
        "route left then right",
        "route left then",
        "route left then right afterward last",
    ] {
        let tree = valid(source);
        exact(&tree, source, "command_chain", source);
        exact(
            &tree,
            source,
            "command_link",
            if source == "route left then" {
                "then"
            } else {
                "then right"
            },
        );
    }
}
#[test]
fn trailing_closures_belong_to_the_invoked_expression() {
    for source in [
        "items.collect { n -> n + 1 }",
        "consume(value) { it }",
        "consume { it }",
        "handlers[0] { it }",
        "items?.collect { it }",
        "consume { it } { it + 1 }",
    ] {
        let tree = valid(source);
        exact(&tree, source, "method_invocation", source);
    }
}
#[test]
fn closures_in_command_arguments_keep_their_callee() {
    let source = "consume items.collect { n -> n + 1 }";
    let tree = valid(source);
    exact(&tree, source, "command_chain", source);
    exact(
        &tree,
        source,
        "method_invocation",
        "items.collect { n -> n + 1 }",
    );
}
#[test]
fn command_expressions_are_allowed_in_value_positions() {
    for statement in [
        "def result = consume value",
        "result = consume value",
        "return consume value",
        "consume(consume value)",
    ] {
        let source = format!("def run() {{ {statement}\nfinish() }}\n");
        let tree = valid(&source);
        exact(&tree, &source, "command_chain", "consume value");
        exact(&tree, &source, "method_invocation", "finish()");
    }
}
#[test]
fn calls_do_not_steal_real_statement_boundaries() {
    for separator in ["\n", ";", "\r\n", " // note\n"] {
        let source = format!("consume value{separator}finish()\n");
        let tree = valid(&source);
        exact(&tree, &source, "command_chain", "consume value");
        exact(&tree, &source, "method_invocation", "finish()");
    }
}
#[test]
fn binary_operators_are_not_unary_command_arguments() {
    for source in [
        "left + right",
        "left - right",
        "left * right",
        "left / right",
        "left << right",
        "left && right",
    ] {
        let tree = valid(source);
        exact(&tree, source, "binary_expression", source);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "command_chain")
        );
    }
}

#[test]
fn command_links_retain_parentheses_indexes_and_closures() {
    for (source, link) in [
        ("route left then(right) afterward last", "then(right)"),
        ("route left then[0] afterward last", "then[0]"),
        ("route left then { it } afterward last", "then { it }"),
        ("route left then()() afterward last", "then()()"),
        (
            "route left then.value(right) afterward last",
            "then.value(right)",
        ),
    ] {
        let tree = valid(source);
        exact(&tree, source, "command_chain", source);
        exact(&tree, source, "command_link", link);
    }
}

#[test]
fn already_called_receivers_start_a_member_link_not_new_arguments() {
    for source in [
        "route(left) then right",
        "route { it } then right",
        "route(left) then(right)",
    ] {
        let tree = valid(source);
        exact(&tree, source, "command_chain", source);
        exact(
            &tree,
            source,
            "command_link",
            if source.ends_with("then(right)") {
                "then(right)"
            } else {
                "then right"
            },
        );
    }
}

#[test]
fn reserved_statement_keywords_are_not_bare_command_members() {
    for keyword in ["finally", "class", "return", "while"] {
        let source = format!("route left {keyword} right");
        assert!(parse(&source).root_node().has_error(), "{source}");
        valid(&format!("service.{keyword}(right)"));
    }
}

#[test]
fn division_dispatch_keeps_comments_and_compound_assignment() {
    for expression in [
        "left/right/end",
        "left / right / end",
        "left/* note *//right",
        "left/=right",
        "left / (right + end)",
    ] {
        let source = format!("def value = {expression}\n");
        let tree = valid(&source);
        let kind = if expression.contains("/=") {
            "assignment_expression"
        } else {
            "binary_expression"
        };
        exact(&tree, &source, kind, expression);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "string_literal")
        );
    }
    assert!(parse("consume /text/").root_node().has_error());
}

#[test]
fn ordinary_labels_and_declarations_are_not_commands() {
    let source = "def run() { String value\nwidget other = factory()\nretry: while (ready) { consume value }\n}\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "String value");
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "widget other = factory()",
    );
    exact(
        &tree,
        source,
        "labeled_statement",
        "retry: while (ready) { consume value }",
    );
}

#[test]
fn incomplete_calls_are_errors_and_parser_reset_is_clean() {
    let baseline = "consume value\nitems.collect { it }\n";
    let expected = valid(baseline).root_node().to_sexp();
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    for broken in [
        "consume value,",
        "publish name:",
        "consume(value",
        "items.collect { it",
        "route left then(",
    ] {
        assert!(
            parser.parse(broken, None).unwrap().root_node().has_error(),
            "{broken}"
        );
        parser.reset();
        assert_eq!(
            parser.parse(baseline, None).unwrap().root_node().to_sexp(),
            expected
        );
    }
}

#[test]
fn call_edits_keep_exact_utf8_points_and_fresh_equivalence() {
    use tree_sitter::{InputEdit, Point};
    let point = |source: &str, offset: usize| {
        let prefix = &source[..offset];
        Point::new(
            prefix.bytes().filter(|b| *b == b'\n').count(),
            prefix
                .rfind('\n')
                .map_or(prefix.len(), |n| prefix.len() - n - 1),
        )
    };
    let source =
        "def text = 'λ😀'\nconsume value\nitems.collect { n -> n + 1 }\nroute left then right\n";
    for (before, after) in [
        ("λ😀", "δ🌍!"),
        ("consume value", "consume(value)"),
        ("n + 1", "n / 2"),
        ("then right", "then(right)"),
        ("\nitems", "\r\nitems"),
        ("consume value", "consume name: text, value"),
    ] {
        let changed = source.replacen(before, after, 1);
        let start = source.find(before).unwrap();
        let mut old = valid(source);
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
        let fresh = valid(&changed);
        let signature = |tree: &Tree| {
            nodes(tree.root_node())
                .into_iter()
                .map(|n| {
                    (
                        n.kind().to_owned(),
                        n.byte_range(),
                        n.start_position(),
                        n.end_position(),
                        n.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            signature(&incremental),
            signature(&fresh),
            "{before} -> {after}"
        );
        for n in nodes(incremental.root_node()) {
            assert_eq!(n.start_position(), point(&changed, n.start_byte()));
            assert_eq!(n.end_position(), point(&changed, n.end_byte()));
        }
    }
}
