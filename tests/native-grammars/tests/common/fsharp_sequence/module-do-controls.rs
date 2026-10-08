// Module DO boundary controls consume the frozen eight ParseFile inputs.
// Included after the unchanged private native frontier helpers; public owner
// kinds come from the preferred grammar, not from compiler AST kind equality.

include!("module-do-sources.rs");

#[derive(Debug, PartialEq, Eq)]
struct ModuleDoCapture {
    capture: String,
    walk_index: usize,
    kind: String,
    bytes: Range<usize>,
    parent: Option<(String, Range<usize>)>,
}

fn module_do_context_captures(
    tree: &Tree,
    source: &str,
    expected: &[(&str, Node<'_>)],
) -> Vec<ModuleDoCapture> {
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "(_module_elem) @module_element\n\
         (do) @public_do\n\
         (do_expression) @inner_do\n\
         (application_expression) @application\n",
    )
    .expect("existing module supertype and public DO queries must compile");
    let walked = nodes(tree);
    assert!(walked.len() <= 64);
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(64);
    let mut actual = Vec::new();
    let mut projection = Vec::new();
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            assert!(actual.len() < 16);
            let capture = found.captures[*index];
            let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
            let joins: Vec<_> = walked
                .iter()
                .enumerate()
                .filter(|(_, (node, _))| *node == capture.node)
                .map(|(walk_index, _)| walk_index)
                .collect();
            assert_eq!(joins.len(), 1, "capture must join one live public node");
            span(capture.node, source, text(capture.node, source));
            actual.push((name, capture.node));
            projection.push(ModuleDoCapture {
                capture: name.to_owned(),
                walk_index: joins[0],
                kind: capture.node.kind().to_owned(),
                bytes: capture.node.byte_range(),
                parent: capture
                    .node
                    .parent()
                    .map(|parent| (parent.kind().to_owned(), parent.byte_range())),
            });
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    assert_eq!(actual.len(), expected.len());
    for (name, node) in expected {
        let matches: Vec<_> = actual
            .iter()
            .filter(|(actual_name, actual_node)| actual_name == name && actual_node == node)
            .collect();
        assert_eq!(matches.len(), 1, "raw capture multiplicity for {name}");
        let actual_node = matches[0].1;
        assert_eq!(actual_node.id(), node.id());
        assert_eq!(actual_node.kind(), node.kind());
        assert_eq!(actual_node.byte_range(), node.byte_range());
        assert_eq!(actual_node.parent(), node.parent());
    }
    projection
}

fn module_do_check(tree: &Tree, fixture: &ModuleDoSource) -> Vec<ModuleDoCapture> {
    let source = fixture.source;
    assert!(source.is_ascii() && source.len() <= 128);
    assert!(!source.ends_with('\n'));
    clean(tree, source);
    assert!(nodes(tree).len() <= 64);

    let root_start = source.find("\ndo").unwrap() + 1;
    let double = source.as_bytes().get(fixture.first_semicolon + 1) == Some(&b';');
    let owner_end = fixture.first_semicolon + usize::from(!double);
    let public_do = exact(tree, source, "do", &source[root_start..owner_end]);
    assert_eq!(public_do.start_byte(), root_start);
    assert_eq!(public_do.end_byte(), owner_end);
    // These grammar routes are unfielded; optional fields from other native
    // branches must not be invented as a generic DO body contract.
    assert!(public_do.child_by_field_name("body").is_none());
    assert!(public_do.child_by_field_name("block").is_none());

    let declaration = public_do.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    span(declaration, source, &source[root_start..owner_end]);
    assert_eq!(named_operands(declaration), vec![public_do]);
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let module_name = module.child_by_field_name("name").unwrap();
    span(module_name, source, "DoTerminatorExample");

    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    assert_eq!(following.start_byte(), fixture.following_start);
    assert_eq!(following.end_byte(), source.len());
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_ne!(declaration, following_declaration);
    assert_eq!(
        named_operands(module),
        vec![module_name, declaration, following_declaration],
    );
    assert!(declaration.end_byte() < following_declaration.start_byte());
    let following_body = following.child_by_field_name("body").unwrap();
    assert_eq!(following_body.kind(), "const");
    span(following_body, source, "30");

    let application = exact(tree, source, "application_expression", "ignore 11");
    assert_eq!(application.end_byte(), fixture.first_semicolon);
    let [callee, argument] = frontier_application(
        application,
        source,
        ("long_identifier_or_op", "ignore"),
        ("const", "11"),
    );
    frontier_children(argument, source, &[("int", "11")]);
    assert_eq!(application.child_count(), 2);

    let mut expressions = vec![application, callee, argument, following_body];
    let mut expected = vec![
        ("module_element", declaration),
        ("module_element", following_declaration),
        ("public_do", public_do),
        ("application", application),
    ];
    if fixture.nested {
        let inner_start = source.find("    do").unwrap() + 4;
        let inner = exact(
            tree,
            source,
            "do_expression",
            &source[inner_start..owner_end],
        );
        assert_eq!(inner.parent(), Some(public_do));
        assert_eq!(inner.end_byte(), owner_end);
        assert!(inner.child_by_field_name("body").is_none());
        assert!(inner.child_by_field_name("block").is_none());
        assert_eq!(named_operands(public_do), vec![inner]);
        assert_eq!(named_operands(inner), vec![application]);
        assert_eq!(application.parent(), Some(inner));
        expressions.push(inner);
        expected.push(("inner_do", inner));
    } else {
        assert_eq!(named_operands(public_do), vec![application]);
        assert_eq!(application.parent(), Some(public_do));
    }
    assert_eq!(
        expression_queries(tree, source, &expressions),
        expressions.len()
    );
    frontier_explicit_queries(tree, source);
    module_do_context_captures(tree, source, &expected)
}

#[test]
fn module_do_single_and_double_boundaries_match_exact_compiler_sources() {
    let cases: Vec<_> = MODULE_DO_SOURCES
        .iter()
        .filter(|fixture| !fixture.nested)
        .collect();
    assert_eq!(cases.len(), 4);
    for fixture in cases {
        let tree = parse(&mut parser(false), fixture.source, None);
        module_do_check(&tree, fixture);
    }
}

#[test]
fn nested_module_do_single_and_double_boundaries_keep_direct_public_owners() {
    let cases: Vec<_> = MODULE_DO_SOURCES
        .iter()
        .filter(|fixture| fixture.nested)
        .collect();
    assert_eq!(cases.len(), 4);
    for fixture in cases {
        let tree = parse(&mut parser(false), fixture.source, None);
        module_do_check(&tree, fixture);
    }
}

#[test]
fn module_do_separator_insert_delete_restore_matches_fresh_snapshots_and_queries() {
    for (single_index, double_index) in [(0, 2), (1, 3), (4, 6), (5, 7)] {
        let single = &MODULE_DO_SOURCES[single_index];
        let double = &MODULE_DO_SOURCES[double_index];
        assert_eq!(single.nested, double.nested);
        assert_eq!(single.first_semicolon, double.first_semicolon);
        let mut inserted = single.source.to_owned();
        inserted.insert(single.first_semicolon + 1, ';');
        assert_eq!(inserted, double.source);
        let mut deleted = double.source.to_owned();
        assert_eq!(deleted.remove(single.first_semicolon + 1), ';');
        assert_eq!(deleted, single.source);

        let mut reused = parser(false);
        let original = parse(&mut reused, single.source, None);
        let original_captures = module_do_check(&original, single);
        let original_snapshot = snapshot(&original, single.source);
        let mut before = single.source;
        let mut current = original;
        for next in [double, single, double, single] {
            edit(&mut current, before, next.source);
            current = parse(&mut reused, next.source, Some(&current));
            let fresh = parse(&mut parser(false), next.source, None);
            let actual_captures = module_do_check(&current, next);
            let fresh_captures = module_do_check(&fresh, next);
            assert_eq!(
                snapshot(&current, next.source),
                snapshot(&fresh, next.source),
                "{}",
                next.id
            );
            assert_eq!(actual_captures, fresh_captures, "{}", next.id);
            if next.source == single.source {
                assert_eq!(snapshot(&current, single.source), original_snapshot);
                assert_eq!(actual_captures, original_captures);
            }
            before = next.source;
        }
        assert_eq!(before, single.source);
    }
}
