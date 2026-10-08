// Exact compiler-qualified frontiers use native public ownership, not the
// compiler's desugared App counts or its braces-only ComputationExpr range.

include!("frontier-sources.rs");

fn frontier_children<'tree>(
    owner: Node<'tree>,
    source: &str,
    expected: &[(&str, &str)],
) -> Vec<Node<'tree>> {
    let children = named_operands(owner);
    assert_eq!(children.len(), expected.len(), "{}", owner.to_sexp());
    for (child, (kind, written)) in children.iter().zip(expected) {
        assert_eq!(child.kind(), *kind, "{}", owner.to_sexp());
        span(*child, source, written);
        assert_eq!(child.parent(), Some(owner));
        assert!(owner.start_byte() <= child.start_byte() && child.end_byte() <= owner.end_byte());
    }
    for pair in children.windows(2) {
        assert!(pair[0].end_byte() <= pair[1].start_byte());
    }
    children
}

fn frontier_application<'tree>(
    application: Node<'tree>,
    source: &str,
    callee: (&str, &str),
    argument: (&str, &str),
) -> [Node<'tree>; 2] {
    assert_eq!(application.kind(), "application_expression");
    let children = frontier_children(application, source, &[callee, argument]);
    assert_eq!(application.start_byte(), children[0].start_byte());
    assert_eq!(application.end_byte(), children[1].end_byte());
    [children[0], children[1]]
}

fn frontier_infix<'tree>(
    expression: Node<'tree>,
    source: &str,
    left: (&str, &str),
    operator: &str,
    right: (&str, &str),
) -> [Node<'tree>; 3] {
    assert_eq!(expression.kind(), "infix_expression");
    let children = frontier_children(expression, source, &[left, ("infix_op", operator), right]);
    assert_eq!(expression.start_byte(), children[0].start_byte());
    assert_eq!(expression.end_byte(), children[2].end_byte());
    assert!(
        children
            .iter()
            .all(|child| child.kind() != "application_expression")
    );
    [children[0], children[1], children[2]]
}

fn frontier_prefix<'tree>(
    expression: Node<'tree>,
    source: &str,
    operator: &str,
    operand: &str,
) -> Node<'tree> {
    assert_eq!(expression.kind(), "prefixed_expression");
    let children = frontier_children(
        expression,
        source,
        &[("prefix_op", operator), ("long_identifier_or_op", operand)],
    );
    assert_eq!(expression.start_byte(), children[0].start_byte());
    assert_eq!(expression.end_byte(), children[1].end_byte());
    children[1]
}

fn frontier_signed_constant(value: Node<'_>, source: &str, written: &str) {
    assert_eq!(value.kind(), "const");
    span(value, source, written);
    let children = frontier_children(value, source, &[("int", written)]);
    assert_eq!(children[0].byte_range(), value.byte_range());
    assert!(!nodes_from_owner(value).iter().any(|node| {
        matches!(
            node.kind(),
            "prefixed_expression" | "prefix_op" | "infix_op"
        )
    }));
}

fn nodes_from_owner(owner: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![owner];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(found.len() < 20_000);
        found.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    found
}

fn frontier_explicit_queries(tree: &Tree, source: &str) {
    let selected = [
        "application_expression",
        "infix_expression",
        "prefixed_expression",
        "ce_expression",
        "paren_expression",
        "const",
        "long_identifier_or_op",
        "prefix_op",
        "infix_op",
    ];
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "[(application_expression) (infix_expression) (prefixed_expression) \
         (ce_expression) (paren_expression) (const) (long_identifier_or_op) \
         (prefix_op) (infix_op)] @syntax",
    )
    .expect("explicit frontier public-node query must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let mut events = Vec::new();
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            assert!(events.len() < 20_000);
            let capture = found.captures[*index];
            assert_eq!(
                query.capture_names()[usize::try_from(capture.index).unwrap()],
                "syntax"
            );
            events.push(capture.node);
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    let walked: Vec<_> = nodes(tree)
        .into_iter()
        .map(|(node, _)| node)
        .filter(|node| selected.contains(&node.kind()))
        .collect();
    assert_eq!(events.len(), walked.len());
    // Raw events stay intact. Duplicate or absent captures fail rather than
    // being normalized into an apparently complete selector result.
    for expected in walked {
        assert_eq!(events.iter().filter(|node| **node == expected).count(), 1);
        span(expected, source, text(expected, source));
        for actual in events.iter().filter(|node| **node == expected) {
            assert_eq!(actual.parent(), expected.parent());
            assert_eq!(actual.byte_range(), expected.byte_range());
        }
    }
}

fn frontier_run_body<'tree>(tree: &'tree Tree, source: &str) -> Node<'tree> {
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let start = source.find("let run ").unwrap();
    let end = source.rfind(&format!("{newline}{FOLLOWING}")).unwrap();
    let binding = exact(tree, source, "function_or_value_defn", &source[start..end]);
    let expression = binding.child_by_field_name("body").unwrap();
    let body_start =
        source[start..end].find(&format!("{newline}    ")).unwrap() + start + newline.len() + 4;
    span(expression, source, &source[body_start..end]);
    assert_eq!(expression.parent(), Some(binding));
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let module_name = source
        .lines()
        .next()
        .unwrap()
        .strip_prefix("module ")
        .unwrap();
    span(
        module.child_by_field_name("name").unwrap(),
        source,
        module_name,
    );
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_ne!(declaration, following_declaration);
    assert_eq!(
        following.child_by_field_name("body").unwrap().kind(),
        "const"
    );
    span(following.child_by_field_name("body").unwrap(), source, "30");
    assert!(declaration.end_byte() <= following_declaration.start_byte());
    assert!(expression.end_byte() < following.start_byte());
    expression
}

fn frontier_yield<'tree>(yield_expression: Node<'tree>, source: &str, operand: &str) {
    assert_eq!(yield_expression.kind(), "prefixed_expression");
    span(yield_expression, source, &format!("yield {operand}"));
    let kind = if operand == "1" {
        "const"
    } else {
        "long_identifier_or_op"
    };
    frontier_children(yield_expression, source, &[(kind, operand)]);
    let keyword = yield_expression.child(0).unwrap();
    assert_eq!(keyword.kind(), "yield");
    span(keyword, source, "yield");
    assert_eq!(keyword.parent(), Some(yield_expression));
}

fn frontier_simple_ce<'tree>(
    computation: Node<'tree>,
    source: &str,
    builder: (&str, &str),
) -> [Node<'tree>; 2] {
    assert_eq!(computation.kind(), "ce_expression");
    let children = frontier_children(
        computation,
        source,
        &[builder, ("prefixed_expression", "yield 1")],
    );
    assert_eq!(computation.start_byte(), children[0].start_byte());
    assert_eq!(
        source.get(children[0].end_byte()..children[1].start_byte()),
        Some(" { ")
    );
    assert_eq!(
        source.get(children[1].end_byte()..computation.end_byte()),
        Some(" }")
    );
    frontier_yield(children[1], source, "1");
    [children[0], children[1]]
}

fn check_frontier(index: usize) {
    let fixture = &FRONTIER_SOURCES[index];
    let source = fixture.source;
    assert!(!source.ends_with('\n'), "{}", fixture.id);
    let tree = parse(&mut parser(false), source, None);
    clean(&tree, source);
    let run_body = frontier_run_body(&tree, source);
    let expression = if source.starts_with("module ComputationFrontierExample") {
        let operands = named_operands(run_body);
        assert_eq!(run_body.kind(), "sequential_expression");
        assert_eq!(operands.len(), 4);
        for operand in &operands {
            assert_eq!(operand.parent(), Some(run_body));
        }
        span(operands[0], source, "ignore 101");
        frontier_application(
            operands[0],
            source,
            ("long_identifier_or_op", "ignore"),
            ("const", "101"),
        );
        span(operands[2], source, "ignore 103");
        frontier_application(
            operands[2],
            source,
            ("long_identifier_or_op", "ignore"),
            ("const", "103"),
        );
        assert_eq!(operands[3].kind(), "const");
        span(operands[3], source, "()");
        operands[1]
    } else {
        run_body
    };
    span(expression, source, fixture.target);
    match fixture.family {
        "adjacent-string-and-name" => {
            let [callee, _] = frontier_application(
                expression,
                source,
                ("application_expression", "printfn\"%s\""),
                ("long_identifier_or_op", "value"),
            );
            let [_, argument] = frontier_application(
                callee,
                source,
                ("long_identifier_or_op", "printfn"),
                ("const", "\"%s\""),
            );
            frontier_children(argument, source, &[("string", "\"%s\"")]);
            assert_eq!(
                callee.end_byte(),
                named_operands(expression)[1].start_byte()
            );
        }
        "adjacent-parenthesized-callee" => {
            let [callee, argument] = frontier_application(
                expression,
                source,
                ("paren_expression", "(combine)"),
                ("const", "1"),
            );
            frontier_children(callee, source, &[("long_identifier_or_op", "combine")]);
            assert_eq!(callee.end_byte(), argument.start_byte());
        }
        "adjacent-array-argument" => {
            let [callee, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                ("array_expression", "[|1|]"),
            );
            frontier_children(argument, source, &[("const", "1")]);
            assert_eq!(callee.end_byte(), argument.start_byte());
        }
        "adjacent-record-argument" => {
            let [callee, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "consume"),
                ("brace_expression", "{ Value = 1 }"),
            );
            assert_eq!(callee.end_byte(), argument.start_byte());
            let value = exact(&tree, source, "const", "1");
            assert!(
                std::iter::successors(value.parent(), |node| node.parent())
                    .any(|node| node == argument)
            );
        }
        "plus-prefix-left-space" => {
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                ("prefixed_expression", "+value"),
            );
            frontier_prefix(argument, source, "+", "value");
        }
        "plus-infix-no-spaces" | "plus-infix-both-spaces" => {
            frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "+",
                ("long_identifier_or_op", "value"),
            );
        }
        "plus-before-parenthesized-right" => {
            let [_, _, right] = frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "+",
                ("paren_expression", "(value)"),
            );
            frontier_children(right, source, &[("long_identifier_or_op", "value")]);
        }
        "curry-same-line" | "curry-multiline" => {
            let children = named_operands(expression);
            assert_eq!(children.len(), 2);
            let callee_text = if fixture.family == "curry-same-line" {
                "combine 201".to_owned()
            } else {
                format!(
                    "combine{}        201",
                    if source.contains("\r\n") {
                        "\r\n"
                    } else {
                        "\n"
                    }
                )
            };
            let [callee, _] = frontier_application(
                expression,
                source,
                ("application_expression", &callee_text),
                ("const", "202"),
            );
            frontier_application(
                callee,
                source,
                ("long_identifier_or_op", "combine"),
                ("const", "201"),
            );
        }
        "infix-same-line" | "infix-continuation" => {
            frontier_infix(expression, source, ("const", "201"), "+", ("const", "202"));
        }
        "minus-adjacent-argument" | "plus-adjacent-argument" => {
            let written = if fixture.family == "minus-adjacent-argument" {
                "-1"
            } else {
                "+1"
            };
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                ("const", written),
            );
            frontier_signed_constant(argument, source, written);
        }
        "minus-spaced-argument" | "minus-no-spaces" => {
            frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "-",
                ("const", "1"),
            );
        }
        "plus-spaced-argument" | "plus-continuation" => {
            frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "+",
                ("const", "1"),
            );
        }
        "bitwise-prefix-argument" | "dereference-prefix-argument" => {
            let (written, operator) = if fixture.family == "bitwise-prefix-argument" {
                ("~~~value", "~~~")
            } else {
                ("!value", "!")
            };
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "ignore"),
                ("prefixed_expression", written),
            );
            frontier_prefix(argument, source, operator, "value");
        }
        "application-valued-builder" => {
            span(expression, source, "make value { yield 1 }");
            let [builder, _] =
                frontier_simple_ce(expression, source, ("application_expression", "make value"));
            frontier_application(
                builder,
                source,
                ("long_identifier_or_op", "make"),
                ("long_identifier_or_op", "value"),
            );
            expression_queries(&tree, source, &[expression, builder]);
        }
        "completed-computation-callee" => {
            let [callee, _] = frontier_application(
                expression,
                source,
                ("ce_expression", "builder { yield 1 }"),
                ("long_identifier_or_op", "value"),
            );
            frontier_simple_ce(callee, source, ("long_identifier_or_op", "builder"));
            expression_queries(&tree, source, &[expression, callee]);
        }
        "parenthesized-computation-argument" => {
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "consume"),
                ("paren_expression", "(builder { yield 1 })"),
            );
            let children = frontier_children(
                argument,
                source,
                &[("ce_expression", "builder { yield 1 }")],
            );
            frontier_simple_ce(children[0], source, ("long_identifier_or_op", "builder"));
            expression_queries(&tree, source, &[expression, argument, children[0]]);
        }
        "nested-computation-argument" => {
            let newline = if source.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let declaration_text =
                format!("let item = consume (inner {{ yield 1 }}){newline}        yield item");
            assert_eq!(expression.kind(), "ce_expression");
            let children = frontier_children(
                expression,
                source,
                &[
                    ("long_identifier_or_op", "outer"),
                    ("declaration_expression", &declaration_text),
                ],
            );
            let declaration = children[1];
            let local = exact(
                &tree,
                source,
                "function_or_value_defn",
                "let item = consume (inner { yield 1 })",
            );
            let continuation = declaration.child_by_field_name("in").unwrap();
            assert_eq!(local.parent(), Some(declaration));
            assert_eq!(continuation.parent(), Some(declaration));
            assert_eq!(named_operands(declaration), vec![local, continuation]);
            frontier_yield(continuation, source, "item");
            let local_body = local.child_by_field_name("body").unwrap();
            assert_eq!(local_body.parent(), Some(local));
            span(local_body, source, "consume (inner { yield 1 })");
            let [_, argument] = frontier_application(
                local_body,
                source,
                ("long_identifier_or_op", "consume"),
                ("paren_expression", "(inner { yield 1 })"),
            );
            let inner =
                frontier_children(argument, source, &[("ce_expression", "inner { yield 1 }")])[0];
            frontier_simple_ce(inner, source, ("long_identifier_or_op", "inner"));
            expression_queries(&tree, source, &[expression, local_body, argument, inner]);
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|(node, _)| node.kind() == "ce_expression")
                    .count(),
                2
            );
        }
        family => panic!("unqualified frontier family {family}"),
    }
    frontier_explicit_queries(&tree, source);
    expression_queries(&tree, source, &[expression]);
}
