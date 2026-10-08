// Incremental EOF delimiter controls consume the frozen eight authored sources.
// Append after the frozen EOF, frontier, and quote helpers; tree identities are
// joined within each query, while cross-tree equality uses public snapshots.

fn eof_incremental_public_topology(tree: &Tree, fixture: &EofSource) {
    assert!(!fixture.signature);
    assert!(fixture.source.is_ascii() && fixture.source.len() <= 64);
    clean(tree, fixture.source);
    assert!(nodes(tree).len() <= 64, "{}", fixture.id);
    let application = exact(tree, fixture.source, "application_expression", "ignore 11");
    assert_eq!(
        application.start_byte(),
        fixture.source.find("ignore 11").unwrap()
    );
    assert_eq!(application.end_byte(), fixture.source.find(';').unwrap());
    let [callee, argument] = frontier_application(
        application,
        fixture.source,
        ("long_identifier_or_op", "ignore"),
        ("const", "11"),
    );
    frontier_children(argument, fixture.source, &[("int", "11")]);
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
    span(name, fixture.source, "TerminatorExample");
    assert_eq!(named_operands(module), vec![name, declaration]);
    // Native trailing extras determine containing endpoints; compiler ranges
    // are not substituted for the live public node's own source-bound range.
    span(binding, fixture.source, text(binding, fixture.source));
    span(
        declaration,
        fixture.source,
        text(declaration, fixture.source),
    );
    eof_single_context_queries(
        tree,
        fixture.source,
        &[
            ("module_element", declaration),
            ("binding", binding),
            ("application", application),
        ],
    );
    assert_eq!(
        expression_queries(tree, fixture.source, &[application, callee, argument]),
        3
    );
}

fn eof_incremental_second_semicolon_edit(tree: &mut Tree, before: &str, after: &str) {
    assert!(before.is_ascii() && after.is_ascii());
    assert!(before.len() <= 64 && after.len() <= 64);
    let first_semicolon = before.find(';').unwrap();
    assert_eq!(after.find(';'), Some(first_semicolon));
    let start = first_semicolon + 1;
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
fn eof_second_semicolon_repeated_insert_delete_restores_fresh_public_snapshots_and_raw_captures() {
    assert_eq!(EOF_NAMED_SOURCES.len(), 8);
    for (single_index, double_index) in [(0, 4), (1, 5), (2, 6), (3, 7)] {
        let single = &EOF_NAMED_SOURCES[single_index];
        let double = &EOF_NAMED_SOURCES[double_index];
        let first_semicolon = single.source.find(';').unwrap();
        assert_eq!(double.source.find(';'), Some(first_semicolon));
        let mut inserted = single.source.to_owned();
        inserted.insert(first_semicolon + 1, ';');
        assert_eq!(inserted, double.source);
        let mut deleted = double.source.to_owned();
        assert_eq!(deleted.remove(first_semicolon + 1), ';');
        assert_eq!(deleted, single.source);

        let mut reused = parser(false);
        let original = parse(&mut reused, single.source, None);
        eof_incremental_public_topology(&original, single);
        let original_snapshot = snapshot(&original, single.source);
        let original_captures = quote_padding_ordered_captures(&original, single.source);
        let mut current = original;
        let mut before = single.source;
        for next in [double, single, double, single] {
            eof_incremental_second_semicolon_edit(&mut current, before, next.source);
            current = parse(&mut reused, next.source, Some(&current));
            let fresh = parse(&mut parser(false), next.source, None);
            eof_incremental_public_topology(&current, next);
            eof_incremental_public_topology(&fresh, next);
            assert_eq!(
                snapshot(&current, next.source),
                snapshot(&fresh, next.source),
                "{}",
                next.id
            );
            let current_captures = quote_padding_ordered_captures(&current, next.source);
            let fresh_captures = quote_padding_ordered_captures(&fresh, next.source);
            assert_eq!(current_captures, fresh_captures, "{}", next.id);
            if next.source == single.source {
                assert_eq!(
                    snapshot(&current, single.source),
                    original_snapshot,
                    "{}",
                    next.id
                );
                assert_eq!(current_captures, original_captures, "{}", next.id);
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
