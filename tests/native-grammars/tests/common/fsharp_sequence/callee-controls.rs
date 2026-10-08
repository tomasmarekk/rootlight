// Canonical callee controls retain every public node and raw ordered query event.
// Append after the unchanged frontier/quote helpers and frozen EOF controls.
// Cross-tree addresses are not compared; no wrapper is hidden from snapshots.

include!("callee-sources.rs");

// Qualified references retain the existing prefix owner, not a tail-only alias.
fn callee_identity_qualified_prefix(node: Node<'_>, source: &str, written: &str) {
    span(node, source, written);
    match node.kind() {
        "long_identifier" => {
            let names = named_operands(node);
            assert!(!names.is_empty());
            assert_eq!(node.child_count(), names.len() * 2 - 1);
            let mut offset = node.start_byte();
            for index in 0..node.child_count() {
                let child = node.child(u32::try_from(index).unwrap()).unwrap();
                assert_eq!(child.parent(), Some(node));
                assert_eq!(
                    node.field_name_for_child(u32::try_from(index).unwrap()),
                    None
                );
                assert_eq!(child.start_byte(), offset);
                if index % 2 == 0 {
                    assert_eq!(child.kind(), "identifier");
                    assert!(child.is_named());
                    assert_eq!(child.child_count(), 0);
                    span(child, source, text(child, source));
                } else {
                    specialized_argument_punctuation(
                        node,
                        source,
                        u32::try_from(index).unwrap(),
                        ".",
                    );
                }
                offset = child.end_byte();
            }
            assert_eq!(offset, node.end_byte());
        }
        "long_identifier_or_op" => {
            let children = named_operands(node);
            assert_eq!(children.len(), 1);
            assert_eq!(node.child_count(), 1);
            assert_eq!(node.field_name_for_child(0), None);
            assert_eq!(children[0].parent(), Some(node));
            assert_eq!(children[0].byte_range(), node.byte_range());
            if children[0].kind() == "identifier" {
                assert_eq!(children[0].child_count(), 0);
                span(children[0], source, written);
            } else {
                assert_eq!(children[0].kind(), "long_identifier");
                assert!(written.contains('.'));
                callee_identity_qualified_prefix(children[0], source, written);
            }
        }
        "dot_expression" => {
            let children = named_operands(node);
            assert_eq!(children.len(), 2);
            assert_eq!(node.child_count(), 3);
            let base = children[0];
            let field = children[1];
            assert_eq!(node.child_by_field_name("base"), Some(base));
            assert_eq!(node.child_by_field_name("field"), Some(field));
            assert_eq!(node.field_name_for_child(0), Some("base"));
            assert_eq!(node.field_name_for_child(1), None);
            assert_eq!(node.field_name_for_child(2), Some("field"));
            assert_eq!(base.parent(), Some(node));
            assert_eq!(field.parent(), Some(node));
            assert!(matches!(
                base.kind(),
                "long_identifier_or_op" | "dot_expression"
            ));
            assert_eq!(field.kind(), "long_identifier_or_op");
            assert_eq!(base.start_byte(), node.start_byte());
            assert_eq!(field.end_byte(), node.end_byte());
            let dot = node.child(1).unwrap();
            specialized_argument_punctuation(node, source, 1, ".");
            assert_eq!(dot.start_byte(), base.end_byte());
            assert_eq!(field.start_byte(), dot.end_byte());
            callee_identity_qualified_prefix(base, source, text(base, source));
            callee_identity_qualified_prefix(field, source, text(field, source));
        }
        other => panic!("qualified prefix lost its declared owner: {other}"),
    }
}

fn callee_identity_qualified_topology(callee: Node<'_>, source: &str, written: &str) {
    let Some((prefix_text, tail_rest)) = written.rsplit_once(".(") else {
        return;
    };
    assert!(!prefix_text.is_empty());
    let tail_text = format!("({tail_rest}");
    assert!(tail_text.ends_with(')'));
    let tail_kind = if tail_text.starts_with("(|") {
        "active_pattern"
    } else {
        "op_identifier"
    };
    let children = named_operands(callee);
    assert_eq!(children.len(), 2);
    assert_eq!(callee.child_count(), 3);
    let prefix = children[0];
    let terminal;
    match callee.kind() {
        "long_identifier_or_op" => {
            assert_eq!(prefix.kind(), "long_identifier");
            terminal = children[1];
            for index in 0..3 {
                assert_eq!(callee.field_name_for_child(index), None);
            }
        }
        "dot_expression" => {
            assert!(matches!(
                prefix.kind(),
                "long_identifier_or_op" | "dot_expression"
            ));
            let field = children[1];
            assert_eq!(field.kind(), "long_identifier_or_op");
            assert_eq!(callee.child_by_field_name("base"), Some(prefix));
            assert_eq!(callee.child_by_field_name("field"), Some(field));
            assert_eq!(callee.field_name_for_child(0), Some("base"));
            assert_eq!(callee.field_name_for_child(1), None);
            assert_eq!(callee.field_name_for_child(2), Some("field"));
            assert_eq!(field.parent(), Some(callee));
            assert_eq!(field.child_count(), 1);
            assert_eq!(field.field_name_for_child(0), None);
            let fields = named_operands(field);
            assert_eq!(fields.len(), 1);
            terminal = fields[0];
            assert_eq!(terminal.parent(), Some(field));
            assert_eq!(terminal.byte_range(), field.byte_range());
            span(field, source, &tail_text);
        }
        other => panic!("qualified callee lost its declared owner: {other}"),
    }
    assert_eq!(prefix.parent(), Some(callee));
    assert_eq!(children[1].parent(), Some(callee));
    assert_eq!(prefix.start_byte(), callee.start_byte());
    assert_eq!(children[1].end_byte(), callee.end_byte());
    span(prefix, source, prefix_text);
    callee_identity_qualified_prefix(prefix, source, prefix_text);
    let dot = callee.child(1).unwrap();
    specialized_argument_punctuation(callee, source, 1, ".");
    assert_eq!(dot.start_byte(), prefix.end_byte());
    assert_eq!(children[1].start_byte(), dot.end_byte());
    assert_eq!(terminal.kind(), tail_kind);
    assert!(terminal.is_named());
    span(terminal, source, &tail_text);
    if tail_kind == "op_identifier" {
        assert_eq!(terminal.child_count(), 0);
    } else {
        let names = named_operands(terminal);
        assert!(!names.is_empty());
        assert_eq!(terminal.child_count(), names.len() * 2 + 1);
        let mut offset = terminal.start_byte();
        for index in 0..terminal.child_count() {
            let child = terminal.child(u32::try_from(index).unwrap()).unwrap();
            assert_eq!(child.parent(), Some(terminal));
            assert_eq!(
                terminal.field_name_for_child(u32::try_from(index).unwrap()),
                None
            );
            assert_eq!(child.start_byte(), offset);
            if index % 2 == 1 {
                assert!(child.is_named());
                assert_eq!(child.child_count(), 0);
                if child.kind() == "wildcard_active_pattern_op" {
                    assert_eq!(index, terminal.child_count() - 2);
                    span(child, source, "_");
                } else {
                    assert_eq!(child.kind(), "active_pattern_op_name");
                    span(child, source, text(child, source));
                }
            } else {
                let punctuation = if index == 0 {
                    "(|"
                } else if index + 1 == terminal.child_count() {
                    "|)"
                } else {
                    "|"
                };
                specialized_argument_punctuation(
                    terminal,
                    source,
                    u32::try_from(index).unwrap(),
                    punctuation,
                );
            }
            offset = child.end_byte();
        }
        assert_eq!(offset, terminal.end_byte());
    }
}

fn callee_identity_topology(tree: &Tree, fixture: &CalleeIdentitySource) {
    assert!(fixture.source.len() <= 256, "{}", fixture.id);
    clean(tree, fixture.source);
    let walked = nodes(tree);
    assert!(walked.len() <= 128, "{}", fixture.id);
    let written = format!("{} 11", fixture.callee);
    let application = exact(tree, fixture.source, "application_expression", &written);
    assert_eq!(application.end_byte(), fixture.source.find(';').unwrap());
    let operands = named_operands(application);
    assert_eq!(operands.len(), 2, "{}", fixture.id);
    let callee = operands[0];
    let argument = operands[1];
    assert_eq!(callee.parent(), Some(application));
    assert_eq!(argument.parent(), Some(application));
    span(callee, fixture.source, fixture.callee);
    callee_identity_qualified_topology(callee, fixture.source, fixture.callee);
    assert_eq!(argument.kind(), "const");
    span(argument, fixture.source, "11");
    frontier_children(argument, fixture.source, &[("int", "11")]);
    if let Some(kind) = fixture.direct_kind {
        assert_eq!(callee.kind(), "long_identifier_or_op", "{}", fixture.id);
        let children = named_operands(callee);
        assert_eq!(children.len(), 1, "{}", fixture.id);
        assert_eq!(children[0].kind(), kind, "{}", fixture.id);
        assert_eq!(children[0].parent(), Some(callee));
        span(children[0], fixture.source, fixture.callee);
    }
    // Dotted expression access may also own a dot_expression under the existing
    // precedence rules. The native snapshot stays exact; no alternate shape is
    // normalized into a single expected compiler representation here.
    for (node, _) in &walked {
        if node.kind() == "long_identifier_or_op" {
            let children = named_operands(*node);
            if children.len() == 1 && children[0].kind() == "long_identifier" {
                assert!(
                    text(children[0], fixture.source).contains('.'),
                    "singleton wrapper: {}",
                    fixture.id
                );
            }
        }
    }
    let binding = application.parent().unwrap();
    assert_eq!(binding.kind(), "function_or_value_defn");
    assert_eq!(binding.child_by_field_name("body"), Some(application));
    assert_eq!(
        binding.start_byte(),
        fixture.source.find("let run").unwrap()
    );
    let head = binding.named_child(0).unwrap();
    assert_eq!(head.kind(), "function_declaration_left");
    assert_eq!(head.parent(), Some(binding));
    span(head, fixture.source, "run ()");
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    assert_eq!(named_operands(declaration), vec![binding]);
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let name = module.child_by_field_name("name").unwrap();
    span(name, fixture.source, "CalleeIdentityExample");
    assert_eq!(named_operands(module), vec![name, declaration]);
    expression_queries(tree, fixture.source, &[application, callee, argument]);
}

fn callee_identity_edit_second_semicolon(tree: &mut Tree, before: &str, after: &str) {
    assert!(before.len() <= 256 && after.len() <= 256);
    let first = before.find(';').unwrap();
    assert_eq!(after.find(';'), Some(first));
    let start = first + 1;
    let deleting = before.as_bytes().get(start) == Some(&b';');
    let inserting = after.as_bytes().get(start) == Some(&b';');
    assert_ne!(deleting, inserting);
    let old_end = start + usize::from(deleting);
    let new_end = start + usize::from(inserting);
    assert_eq!(&before[..start], &after[..start]);
    assert_eq!(&before[old_end..], &after[new_end..]);
    assert_eq!(&before[start..old_end], if deleting { ";" } else { "" });
    assert_eq!(&after[start..new_end], if inserting { ";" } else { "" });
    tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before, start),
        old_end_position: point(before, old_end),
        new_end_position: point(after, new_end),
    });
    assert!(tree.root_node().has_changes());
}

#[test]
fn plain_operator_and_dotted_callees_keep_source_bound_canonical_public_routes() {
    assert_eq!(CALLEE_IDENTITY_SOURCES.len(), 36);
    for fixture in CALLEE_IDENTITY_SOURCES {
        let tree = parse(&mut parser(false), fixture.source, None);
        callee_identity_topology(&tree, fixture);
        quote_padding_ordered_captures(&tree, fixture.source);
    }
}

#[test]
fn callee_semicolon_edits_restore_fresh_public_snapshots_and_ordered_raw_queries() {
    assert_eq!(CALLEE_IDENTITY_SOURCES.len(), 36);
    for pair in CALLEE_IDENTITY_SOURCES.chunks_exact(2) {
        let single = &pair[0];
        let double = &pair[1];
        assert_eq!(single.callee, double.callee);
        let offset = single.source.find(';').unwrap() + 1;
        let mut inserted = single.source.to_owned();
        inserted.insert(offset, ';');
        assert_eq!(inserted, double.source);
        let mut deleted = double.source.to_owned();
        assert_eq!(deleted.remove(offset), ';');
        assert_eq!(deleted, single.source);
        let mut reused = parser(false);
        let original = parse(&mut reused, single.source, None);
        callee_identity_topology(&original, single);
        let original_snapshot = snapshot(&original, single.source);
        let original_captures = quote_padding_ordered_captures(&original, single.source);
        let mut current = original;
        let mut before = single.source;
        for next in [double, single, double, single] {
            callee_identity_edit_second_semicolon(&mut current, before, next.source);
            current = parse(&mut reused, next.source, Some(&current));
            let fresh = parse(&mut parser(false), next.source, None);
            callee_identity_topology(&current, next);
            callee_identity_topology(&fresh, next);
            assert_eq!(
                snapshot(&current, next.source),
                snapshot(&fresh, next.source),
                "{}",
                next.id
            );
            let actual = quote_padding_ordered_captures(&current, next.source);
            let expected = quote_padding_ordered_captures(&fresh, next.source);
            assert_eq!(actual, expected, "{}", next.id);
            if next.source == single.source {
                assert_eq!(
                    snapshot(&current, single.source),
                    original_snapshot,
                    "{}",
                    next.id
                );
                assert_eq!(actual, original_captures, "{}", next.id);
            }
            before = next.source;
        }
        assert_eq!(before.as_bytes(), single.source.as_bytes());
        assert_eq!(snapshot(&current, single.source), original_snapshot);
        assert_eq!(
            quote_padding_ordered_captures(&current, single.source),
            original_captures
        );
    }
}
