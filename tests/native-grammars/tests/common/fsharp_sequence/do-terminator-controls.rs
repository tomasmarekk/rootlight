// Source-bound delimiter controls retain compiler and native ranges separately.
// Consumed single separators belong to native owners; double closure is zero-width.
// Raw query multiplicity and incremental restoration remain exact contracts.

include!("do-terminator-fixtures.rs");

fn do_terminator_no_fields(owner: Node<'_>) {
    for index in 0..owner.child_count() {
        assert_eq!(
            owner.field_name_for_child(u32::try_from(index).unwrap()),
            None
        );
    }
}

fn do_terminator_call<'tree>(owner: Node<'tree>, source: &str, value: &str) -> Vec<Node<'tree>> {
    let written = format!("ignore {value}");
    span(owner, source, &written);
    let [callee, argument] = frontier_application(
        owner,
        source,
        ("long_identifier_or_op", "ignore"),
        ("const", value),
    );
    do_terminator_no_fields(owner);
    frontier_signed_constant(argument, source, value);
    vec![owner, callee, argument]
}

fn do_terminator_first_owner<'tree>(
    owner: Node<'tree>,
    source: &str,
    fixture: &DoTerminatorSource,
) -> Vec<Node<'tree>> {
    let start = fixture.body.0;
    let owner_end = fixture.separator_start + usize::from(!fixture.double);
    let written = &source[start..owner_end];
    span(owner, source, written);
    assert_eq!(owner.byte_range(), start..owner_end);
    do_terminator_no_fields(owner);
    match fixture.family {
        DoTerminatorFamily::NumericFor => {
            assert_eq!(owner.kind(), "for_expression");
            let children = frontier_children(
                owner,
                source,
                &[
                    ("identifier", "index"),
                    ("const", "1"),
                    ("const", "2"),
                    ("application_expression", "ignore 11"),
                ],
            );
            frontier_signed_constant(children[1], source, "1");
            frontier_signed_constant(children[2], source, "2");
            let mut inventory = vec![owner, children[1], children[2]];
            inventory.extend(do_terminator_call(children[3], source, "11"));
            inventory
        }
        DoTerminatorFamily::ForIn => {
            assert_eq!(owner.kind(), "for_expression");
            let children = frontier_children(
                owner,
                source,
                &[
                    ("identifier_pattern", "index"),
                    ("list_expression", "[1; 2]"),
                    ("application_expression", "ignore 11"),
                ],
            );
            let pattern_identifier =
                frontier_children(children[0], source, &[("long_identifier_or_op", "index")])[0];
            frontier_children(pattern_identifier, source, &[("identifier", "index")]);
            do_terminator_no_fields(children[0]);
            do_terminator_no_fields(pattern_identifier);
            let values = frontier_children(children[1], source, &[("const", "1"), ("const", "2")]);
            do_terminator_no_fields(children[1]);
            frontier_signed_constant(values[0], source, "1");
            frontier_signed_constant(values[1], source, "2");
            let mut inventory = vec![owner, children[1], values[0], values[1]];
            inventory.extend(do_terminator_call(children[2], source, "11"));
            inventory
        }
        DoTerminatorFamily::WhileBang => {
            // ParseFile accepts the authored unwrapped while! source. This
            // assertion proves syntax ownership, never computation typing.
            assert_eq!(owner.kind(), "while_expression");
            let children = frontier_children(
                owner,
                source,
                &[("const", "false"), ("application_expression", "ignore 11")],
            );
            let boolean = frontier_children(children[0], source, &[("bool", "false")]);
            assert_eq!(boolean[0].byte_range(), children[0].byte_range());
            let mut inventory = vec![owner, children[0]];
            inventory.extend(do_terminator_call(children[1], source, "11"));
            inventory
        }
        DoTerminatorFamily::OrdinaryDo => {
            assert_eq!(owner.kind(), "do_expression");
            let children =
                frontier_children(owner, source, &[("application_expression", "ignore 11")]);
            let mut inventory = vec![owner];
            inventory.extend(do_terminator_call(children[0], source, "11"));
            inventory
        }
        DoTerminatorFamily::NestedDo => {
            assert_eq!(owner.kind(), "do_expression");
            let inner_start = source.find("        do").unwrap() + 8;
            let inner_text = &source[inner_start..owner_end];
            let inner = frontier_children(owner, source, &[("do_expression", inner_text)])[0];
            do_terminator_no_fields(inner);
            let children =
                frontier_children(inner, source, &[("application_expression", "ignore 11")]);
            let mut inventory = vec![owner, inner];
            inventory.extend(do_terminator_call(children[0], source, "11"));
            inventory
        }
    }
}

fn do_terminator_contexts(
    tree: &Tree,
    source: &str,
    elements: &[Node<'_>],
    standalone: Option<Node<'_>>,
) {
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "(named_module (_module_elem) @element)\n\
         (named_module (_module_expression) @standalone)",
    )
    .expect("the retained module-element and standalone ancestry must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let mut module_elements = Vec::new();
    let mut standalone_expressions = Vec::new();
    let mut events = 0usize;
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            events += 1;
            assert!(events < 20_000);
            let capture = found.captures[*index];
            span(capture.node, source, text(capture.node, source));
            assert_eq!(
                nodes(tree)
                    .iter()
                    .filter(|(node, _)| *node == capture.node)
                    .count(),
                1,
            );
            match query.capture_names()[usize::try_from(capture.index).unwrap()] {
                "element" => module_elements.push(capture.node),
                "standalone" => standalone_expressions.push(capture.node),
                other => panic!("unexpected DO context {other}"),
            }
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    // Compare raw events; duplicated alternatives must not be normalized away.
    assert_eq!(module_elements, elements);
    assert_eq!(
        standalone_expressions,
        standalone.into_iter().collect::<Vec<_>>()
    );
}

fn do_terminator_topology(tree: &Tree, source: &str, fixture: &DoTerminatorSource) {
    assert_eq!(source, fixture.source, "{}", fixture.id);
    assert_eq!(source.contains("\r\n"), fixture.crlf);
    assert_eq!(source.ends_with('\n'), fixture.final_newline);
    assert_eq!(
        &source[fixture.separator_start..fixture.separator_start + 1],
        ";"
    );
    assert_eq!(
        source.as_bytes().get(fixture.separator_start + 1) == Some(&b';'),
        fixture.double,
    );
    clean(tree, source);
    let body_text = &source[fixture.body.0..fixture.body.1];
    let binding_text = &source[fixture.declaration.0..fixture.declaration.1];
    let binding = exact(tree, source, "function_or_value_defn", binding_text);
    assert_eq!(
        binding.byte_range(),
        fixture.declaration.0..fixture.declaration.1
    );
    let body = binding.child_by_field_name("body").unwrap();
    span(body, source, body_text);
    assert_eq!(body.byte_range(), fixture.body.0..fixture.body.1);
    specialized_boundary_sole_field(binding, body, "body");
    let body_kind = if fixture.double {
        match fixture.family {
            DoTerminatorFamily::NumericFor | DoTerminatorFamily::ForIn => "for_expression",
            DoTerminatorFamily::WhileBang => "while_expression",
            DoTerminatorFamily::OrdinaryDo | DoTerminatorFamily::NestedDo => "do_expression",
        }
    } else {
        "sequential_expression"
    };
    assert_eq!(body.kind(), body_kind);
    let binding_children = frontier_children(
        binding,
        source,
        &[
            ("function_declaration_left", "run ()"),
            (body_kind, body_text),
        ],
    );
    assert_eq!(binding_children[1], body);
    let head_children = frontier_children(
        binding_children[0],
        source,
        &[("identifier", "run"), ("argument_patterns", "()")],
    );
    let binder = frontier_children(head_children[1], source, &[("const", "()")])[0];
    frontier_children(binder, source, &[("unit", "()")]);
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    span(declaration, source, binding_text);
    assert_eq!(
        frontier_children(
            declaration,
            source,
            &[("function_or_value_defn", binding_text)]
        ),
        vec![binding],
    );

    let following_text = &source[fixture.following.0..fixture.following.1];
    assert_eq!(following_text, FOLLOWING);
    let following = exact(tree, source, "function_or_value_defn", following_text);
    assert_eq!(
        following.byte_range(),
        fixture.following.0..fixture.following.1
    );
    let following_body = following.child_by_field_name("body").unwrap();
    specialized_boundary_sole_field(following, following_body, "body");
    frontier_signed_constant(following_body, source, "30");
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    span(following_declaration, source, FOLLOWING);
    assert_eq!(
        frontier_children(
            following_declaration,
            source,
            &[("function_or_value_defn", FOLLOWING)]
        ),
        vec![following],
    );

    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    assert_eq!(module.parent(), Some(tree.root_node()));
    assert_eq!(named_operands(tree.root_node()), vec![module]);
    let mut expected_module = vec![
        ("long_identifier", "DoTerminatorExample"),
        ("declaration_expression", binding_text),
    ];
    let module_text = fixture
        .module_expression
        .map(|(start, end)| &source[start..end]);
    if let Some(written) = module_text {
        expected_module.push(("application_expression", written));
    }
    expected_module.push(("declaration_expression", FOLLOWING));
    let module_children = frontier_children(module, source, &expected_module);
    assert_eq!(module_children[1], declaration);
    assert_eq!(module_children.last().copied(), Some(following_declaration));
    assert_eq!(module.child_by_field_name("name"), Some(module_children[0]));
    specialized_boundary_sole_field(module, module_children[0], "name");
    assert!(body.end_byte() < following.start_byte());

    let mut inventory;
    let mut elements = vec![declaration];
    let standalone;
    if fixture.double {
        assert_eq!(body.end_byte(), fixture.separator_start);
        assert_eq!(declaration.end_byte(), fixture.separator_start);
        inventory = do_terminator_first_owner(body, source, fixture);
        let application = module_children[2];
        let written = module_text.unwrap();
        let [inner_call, unit] = frontier_application(
            application,
            source,
            ("application_expression", "ignore 13"),
            ("const", "()"),
        );
        assert_eq!(application.parent(), Some(module));
        assert_eq!(application.byte_range(), {
            let (start, end) = fixture.module_expression.unwrap();
            start..end
        });
        span(application, source, written);
        do_terminator_no_fields(application);
        inventory.push(application);
        inventory.extend(do_terminator_call(inner_call, source, "13"));
        frontier_children(unit, source, &[("unit", "()")]);
        inventory.push(unit);
        elements.push(application);
        standalone = Some(application);
    } else {
        assert!(fixture.module_expression.is_none());
        let first_kind = match fixture.family {
            DoTerminatorFamily::NumericFor | DoTerminatorFamily::ForIn => "for_expression",
            DoTerminatorFamily::WhileBang => "while_expression",
            DoTerminatorFamily::OrdinaryDo | DoTerminatorFamily::NestedDo => "do_expression",
        };
        let children = frontier_children(
            body,
            source,
            &[
                (
                    first_kind,
                    &source[fixture.body.0..fixture.separator_start + 1],
                ),
                ("application_expression", "ignore 13"),
                ("const", "()"),
            ],
        );
        do_terminator_no_fields(body);
        inventory = vec![body];
        inventory.extend(do_terminator_first_owner(children[0], source, fixture));
        inventory.extend(do_terminator_call(children[1], source, "13"));
        frontier_children(children[2], source, &[("unit", "()")]);
        inventory.push(children[2]);
        standalone = None;
    }
    elements.push(following_declaration);
    inventory.push(following_body);
    let expected_count = match fixture.family {
        DoTerminatorFamily::NumericFor => 12,
        DoTerminatorFamily::OrdinaryDo => 10,
        DoTerminatorFamily::NestedDo | DoTerminatorFamily::WhileBang => 11,
        DoTerminatorFamily::ForIn => 13,
    };
    assert_eq!(inventory.len(), expected_count);
    for (position, node) in inventory.iter().enumerate() {
        assert!(
            inventory[..position]
                .iter()
                .all(|other| other.id() != node.id())
        );
        assert_ne!(node.id(), binder.id());
    }
    assert_eq!(expression_queries(tree, source, &inventory), expected_count);
    frontier_explicit_queries(tree, source);
    do_terminator_contexts(tree, source, &elements, standalone);
}

fn do_terminator_family(family: DoTerminatorFamily, count: usize) {
    let selected: Vec<_> = DO_TERMINATOR_SOURCES
        .iter()
        .filter(|fixture| fixture.family == family)
        .collect();
    assert_eq!(selected.len(), count);
    assert_eq!(
        selected.iter().filter(|fixture| fixture.double).count(),
        count / 2
    );
    for fixture in selected {
        let tree = parse(&mut parser(false), fixture.source, None);
        do_terminator_topology(&tree, fixture.source, fixture);
    }
}

#[test]
fn do_terminator_numeric_for_preserves_all_single_and_double_owners() {
    do_terminator_family(DoTerminatorFamily::NumericFor, 8);
}

#[test]
fn do_terminator_inner_do_preserves_all_single_and_double_owners() {
    do_terminator_family(DoTerminatorFamily::OrdinaryDo, 8);
}

#[test]
fn do_terminator_nested_do_preserves_all_single_and_double_owners() {
    do_terminator_family(DoTerminatorFamily::NestedDo, 8);
}

#[test]
fn do_terminator_for_in_preserves_all_single_and_double_owners() {
    do_terminator_family(DoTerminatorFamily::ForIn, 4);
}

#[test]
fn do_terminator_while_bang_preserves_parse_only_delimiter_owners() {
    do_terminator_family(DoTerminatorFamily::WhileBang, 4);
}

#[test]
fn do_terminator_second_semicolon_insert_delete_restore_matches_fresh_snapshots() {
    let singles: Vec<_> = DO_TERMINATOR_SOURCES
        .iter()
        .filter(|fixture| !fixture.double)
        .collect();
    assert_eq!(singles.len(), 16);
    for single in singles {
        let doubles: Vec<_> = DO_TERMINATOR_SOURCES
            .iter()
            .filter(|fixture| {
                fixture.double
                    && fixture.family == single.family
                    && fixture.crlf == single.crlf
                    && fixture.final_newline == single.final_newline
            })
            .collect();
        assert_eq!(doubles.len(), 1);
        let double = doubles[0];
        let mut inserted = single.source.to_owned();
        inserted.insert(single.separator_start + 1, ';');
        assert_eq!(inserted, double.source);
        let mut deleted = double.source.to_owned();
        assert_eq!(deleted.remove(double.separator_start + 1), ';');
        assert_eq!(deleted, single.source);
        assert_eq!(single.separator_start, double.separator_start);

        let mut reused = parser(false);
        let original = parse(&mut reused, single.source, None);
        do_terminator_topology(&original, single.source, single);
        let original_projection = (
            snapshot(&original, single.source),
            quote_padding_ordered_captures(&original, single.source),
        );
        let mut current = original;
        let mut before = single.source.to_owned();
        for fixture in [double, single, double, single] {
            edit(&mut current, &before, fixture.source);
            current = parse(&mut reused, fixture.source, Some(&current));
            let fresh = parse(&mut parser(false), fixture.source, None);
            do_terminator_topology(&current, fixture.source, fixture);
            do_terminator_topology(&fresh, fixture.source, fixture);
            assert_eq!(
                (
                    snapshot(&current, fixture.source),
                    quote_padding_ordered_captures(&current, fixture.source)
                ),
                (
                    snapshot(&fresh, fixture.source),
                    quote_padding_ordered_captures(&fresh, fixture.source)
                ),
            );
            before = fixture.source.to_owned();
        }
        assert_eq!(before, single.source);
        assert_eq!(
            (
                snapshot(&current, single.source),
                quote_padding_ordered_captures(&current, single.source)
            ),
            original_projection,
        );
    }
}
