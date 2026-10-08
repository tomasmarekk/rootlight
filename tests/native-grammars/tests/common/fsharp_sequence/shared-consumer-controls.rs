// Shared-name controls distinguish pattern, assignee and expression-member owners.
// Append after the unchanged frontier and quote helpers; all native and compiler
// outcomes remain prospective until the closed source table is qualified.

include!("shared-consumer-sources.rs");

fn shared_consumer_binding<'tree>(
    tree: &'tree Tree,
    fixture: &SharedConsumerSource,
    source: &str,
    written_body: &str,
) -> (Node<'tree>, Node<'tree>) {
    assert!(source.len() <= 256, "{}", fixture.id);
    clean(tree, source);
    assert!(nodes(tree).len() <= 128, "{}", fixture.id);
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let written_binding = format!("let {} ={newline}    {written_body}", fixture.head);
    let binding = exact(tree, source, "function_or_value_defn", &written_binding);
    let head = binding.named_child(0).unwrap();
    assert_eq!(head.kind(), "function_declaration_left");
    span(head, source, fixture.head);
    assert_eq!(head.parent(), Some(binding));
    let expression = binding.child_by_field_name("body").unwrap();
    span(expression, source, written_body);
    assert_eq!(expression.parent(), Some(binding));
    assert_eq!(named_operands(binding), vec![head, expression]);
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    assert_eq!(named_operands(declaration), vec![binding]);
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let name = module.child_by_field_name("name").unwrap();
    span(name, source, "SharedConsumerExample");
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_eq!(named_operands(following_declaration), vec![following]);
    assert_eq!(
        named_operands(module),
        vec![name, declaration, following_declaration]
    );
    let following_value = following.child_by_field_name("body").unwrap();
    assert_eq!(following_value.kind(), "const");
    span(following_value, source, "30");
    assert_eq!(following_value.parent(), Some(following));
    assert!(expression.end_byte() < following.start_byte());
    assert!(declaration.end_byte() < following_declaration.start_byte());
    (expression, following_value)
}

fn shared_consumer_owner_captures(
    tree: &Tree,
    source: &str,
    pattern: &str,
    expected: &[(&str, Node<'_>)],
) -> Vec<QuotePaddingCapture> {
    let query = Query::new(&tree_sitter_fsharp::LANGUAGE_FSHARP.into(), pattern)
        .expect("authored shared-consumer owner query must compile");
    let walked = nodes(tree);
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(128);
    let started = Instant::now();
    let mut checks = 0usize;
    let mut aborted = false;
    let mut watchdog = |_: &tree_sitter::QueryCursorState| {
        checks += 1;
        if checks > 20_000 || started.elapsed() > Duration::from_secs(10) {
            aborted = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut events = Vec::new();
    {
        let options = tree_sitter::QueryCursorOptions::new().progress_callback(&mut watchdog);
        let mut captures =
            cursor.captures_with_options(&query, tree.root_node(), source.as_bytes(), options);
        while let Some((found, index)) = captures.next() {
            assert!(events.len() < 32);
            let capture = found.captures[*index];
            let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
            assert_eq!(
                expected
                    .iter()
                    .filter(|(role, node)| *role == name && *node == capture.node)
                    .count(),
                1
            );
            let joins: Vec<_> = walked
                .iter()
                .enumerate()
                .filter(|(_, (node, _))| *node == capture.node)
                .map(|(index, _)| index)
                .collect();
            assert_eq!(joins.len(), 1);
            span(capture.node, source, text(capture.node, source));
            events.push(QuotePaddingCapture {
                capture: name.to_owned(),
                walk_index: joins[0],
                kind: capture.node.kind().to_owned(),
                bytes: capture.node.byte_range(),
                start: capture.node.start_position(),
                end: capture.node.end_position(),
            });
        }
    }
    assert!(!aborted);
    assert!(!cursor.did_exceed_match_limit());
    assert_eq!(events.len(), expected.len());
    for (role, node) in expected {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.capture == *role
                    && event.walk_index
                        == walked
                            .iter()
                            .position(|(actual, _)| actual == node)
                            .unwrap())
                .count(),
            1
        );
    }
    events
}

fn shared_consumer_check(
    tree: &Tree,
    fixture: &SharedConsumerSource,
    source: &str,
    written_body: &str,
) -> (Vec<QuotePaddingCapture>, Vec<QuotePaddingCapture>) {
    let (expression, following) = shared_consumer_binding(tree, fixture, source, written_body);
    let mut genuine = vec![expression, following];
    let mut negatives = Vec::new();
    let owner_events = match fixture.family {
        SharedConsumerFamily::QualifiedPattern => {
            assert_eq!(expression.kind(), "match_expression");
            let children = frontier_children(
                expression,
                source,
                &[
                    ("long_identifier_or_op", "value"),
                    ("rules", written_body.split_once("    ").unwrap().1),
                ],
            );
            genuine.push(children[0]);
            let rules = children[1];
            let arms = named_operands(rules);
            assert_eq!(arms.len(), 2);
            for arm in &arms {
                assert_eq!(arm.kind(), "rule");
                assert_eq!(arm.parent(), Some(rules));
            }
            span(arms[0], source, "Choice.Item payload -> payload");
            let constructor = arms[0].child_by_field_name("pattern").unwrap();
            let result = arms[0].child_by_field_name("block").unwrap();
            let pattern_children = frontier_children(
                constructor,
                source,
                &[
                    ("long_identifier_or_op", "Choice.Item"),
                    ("identifier_pattern", "payload"),
                ],
            );
            assert_eq!(constructor.kind(), "identifier_pattern");
            span(constructor, source, "Choice.Item payload");
            assert_eq!(constructor.parent(), Some(arms[0]));
            let name = frontier_children(
                pattern_children[0],
                source,
                &[("long_identifier", "Choice.Item")],
            )[0];
            frontier_children(
                name,
                source,
                &[("identifier", "Choice"), ("identifier", "Item")],
            );
            let binder = frontier_children(
                pattern_children[1],
                source,
                &[("long_identifier_or_op", "payload")],
            )[0];
            frontier_children(binder, source, &[("identifier", "payload")]);
            assert_eq!(result.parent(), Some(arms[0]));
            assert_eq!(result.kind(), "long_identifier_or_op");
            span(result, source, "payload");
            frontier_children(result, source, &[("identifier", "payload")]);
            let second_pattern = arms[1].child_by_field_name("pattern").unwrap();
            let second_result = arms[1].child_by_field_name("block").unwrap();
            assert_eq!(second_pattern.kind(), "wildcard_pattern");
            span(second_pattern, source, "_");
            assert_eq!(second_pattern.parent(), Some(arms[1]));
            assert_eq!(second_result.kind(), "const");
            span(
                second_result,
                source,
                written_body.rsplit_once("-> ").unwrap().1,
            );
            span(
                arms[1],
                source,
                &format!("_ -> {}", text(second_result, source)),
            );
            assert_eq!(second_result.parent(), Some(arms[1]));
            assert_eq!(named_operands(arms[0]), vec![constructor, result]);
            assert_eq!(named_operands(arms[1]), vec![second_pattern, second_result]);
            genuine.extend([result, second_result]);
            negatives.extend([constructor, second_pattern]);
            shared_consumer_owner_captures(
                tree,
                source,
                "(rule pattern: (_) @pattern block: (_) @block) @rule",
                &[
                    ("pattern", constructor),
                    ("block", result),
                    ("rule", arms[0]),
                    ("pattern", second_pattern),
                    ("block", second_result),
                    ("rule", arms[1]),
                ],
            )
        }
        SharedConsumerFamily::Mutation => {
            assert_eq!(expression.kind(), "mutate_expression");
            let assignee = expression.child_by_field_name("assignee").unwrap();
            let value = expression.child_by_field_name("value").unwrap();
            let rhs = written_body.strip_prefix("current <- ").unwrap();
            assert_eq!(named_operands(expression), vec![assignee, value]);
            assert_eq!(assignee.parent(), Some(expression));
            assert_eq!(value.parent(), Some(expression));
            assert_eq!(assignee.kind(), "long_identifier_or_op");
            assert_eq!(value.kind(), "long_identifier_or_op");
            span(assignee, source, "current");
            span(value, source, rhs);
            frontier_children(assignee, source, &[("identifier", "current")]);
            frontier_children(value, source, &[("identifier", rhs)]);
            genuine.push(value);
            negatives.push(assignee);
            shared_consumer_owner_captures(
                tree,
                source,
                "(mutate_expression assignee: (_) @assignee value: (_) @value) @mutation",
                &[
                    ("assignee", assignee),
                    ("value", value),
                    ("mutation", expression),
                ],
            )
        }
        SharedConsumerFamily::DotMember => {
            assert_eq!(expression.kind(), "dot_expression");
            let base = expression.child_by_field_name("base").unwrap();
            let field = expression.child_by_field_name("field").unwrap();
            assert_eq!(named_operands(expression), vec![base, field]);
            assert_eq!(base.kind(), "paren_expression");
            assert_eq!(field.kind(), "long_identifier_or_op");
            assert_eq!(base.parent(), Some(expression));
            assert_eq!(field.parent(), Some(expression));
            span(base, source, "(value)");
            let member = written_body.strip_prefix("(value).").unwrap();
            span(field, source, member);
            let value = frontier_children(base, source, &[("long_identifier_or_op", "value")])[0];
            frontier_children(value, source, &[("identifier", "value")]);
            let property = frontier_children(field, source, &[("identifier", member)])[0];
            genuine.extend([base, value]);
            negatives.push(field);
            shared_consumer_owner_captures(
                tree,
                source,
                "(dot_expression base: (paren_expression) @base field: (long_identifier_or_op . (identifier) @property .) @field) @dot",
                &[
                    ("base", base),
                    ("property", property),
                    ("field", field),
                    ("dot", expression),
                ],
            )
        }
    };
    assert_eq!(expression_queries(tree, source, &genuine), genuine.len());
    let contextual_events = quote_padding_ordered_captures(tree, source);
    for negative in negatives {
        // Expression owners can contain excluded children; child-local events cannot.
        assert!(
            contextual_events
                .iter()
                .filter(|event| event.capture == "generic")
                .all(|event| event.bytes.start < negative.start_byte()
                    || event.bytes.end > negative.end_byte())
        );
    }
    (contextual_events, owner_events)
}

fn shared_consumer_family(family: SharedConsumerFamily) {
    assert_eq!(SHARED_CONSUMER_SOURCES.len(), 6);
    let selected: Vec<_> = SHARED_CONSUMER_SOURCES
        .iter()
        .filter(|fixture| fixture.family == family)
        .collect();
    assert_eq!(selected.len(), 2);
    for fixture in selected {
        assert_eq!(fixture.source.matches(fixture.body).count(), 1);
        let changed = fixture
            .source
            .replacen(fixture.body, fixture.changed_body, 1);
        assert_eq!(changed.matches(fixture.changed_body).count(), 1);
        let mut reused = parser(false);
        let original = parse(&mut reused, fixture.source, None);
        let original_snapshot = snapshot(&original, fixture.source);
        let original_events =
            shared_consumer_check(&original, fixture, fixture.source, fixture.body);
        let mut current = original;
        let mut before = fixture.source;
        for (after, written_body) in [
            (changed.as_str(), fixture.changed_body),
            (fixture.source, fixture.body),
        ] {
            edit(&mut current, before, after);
            current = parse(&mut reused, after, Some(&current));
            let fresh = parse(&mut parser(false), after, None);
            let actual = shared_consumer_check(&current, fixture, after, written_body);
            let expected = shared_consumer_check(&fresh, fixture, after, written_body);
            assert_eq!(
                snapshot(&current, after),
                snapshot(&fresh, after),
                "{}",
                fixture.id
            );
            assert_eq!(actual, expected, "{}", fixture.id);
            if after == fixture.source {
                assert_eq!(
                    snapshot(&current, after),
                    original_snapshot,
                    "{}",
                    fixture.id
                );
                assert_eq!(actual, original_events, "{}", fixture.id);
            }
            before = after;
        }
        assert_eq!(before, fixture.source);
    }
}

#[test]
fn qualified_constructor_patterns_preserve_names_binders_and_expression_exclusion() {
    shared_consumer_family(SharedConsumerFamily::QualifiedPattern);
}

#[test]
fn mutation_assignee_and_value_preserve_distinct_source_fields_and_query_roles() {
    shared_consumer_family(SharedConsumerFamily::Mutation);
}

#[test]
fn compound_expression_member_fields_preserve_direct_names_and_query_ownership() {
    shared_consumer_family(SharedConsumerFamily::DotMember);
}
