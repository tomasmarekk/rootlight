// Nested qualified references retain each written prefix component and restored CST.
// Append after the unchanged strict callee controls in a future fresh test closure.
// Every query event and anonymous node remains visible to existing snapshots.

include!("nested-qualified-sources.rs");

fn nested_qualified_prefix_identity(tree: &Tree, fixture: &CalleeIdentitySource) {
    let (prefix_text, _) = fixture.callee.rsplit_once(".(").unwrap();
    let components: Vec<_> = prefix_text.split('.').collect();
    assert!((2..=3).contains(&components.len()), "{}", fixture.id);
    assert!(components.iter().all(|component| !component.is_empty()));
    let application = exact(
        tree,
        fixture.source,
        "application_expression",
        &format!("{} 11", fixture.callee),
    );
    let callee = named_operands(application)[0];
    let prefix = named_operands(callee)[0];
    span(prefix, fixture.source, prefix_text);
    let leaves: Vec<_> = nodes(tree)
        .into_iter()
        .map(|(node, _)| node)
        .filter(|node| {
            matches!(node.kind(), "identifier" | ".")
                && std::iter::successors(Some(*node), |owner| owner.parent())
                    .any(|owner| owner == prefix)
        })
        .collect();
    assert_eq!(leaves.len(), components.len() * 2 - 1, "{}", fixture.id);
    let mut offset = prefix.start_byte();
    for (index, leaf) in leaves.iter().enumerate() {
        assert_eq!(leaf.child_count(), 0);
        assert!(!leaf.is_extra());
        assert_eq!(leaf.start_byte(), offset);
        if index % 2 == 0 {
            assert_eq!(leaf.kind(), "identifier");
            assert!(leaf.is_named());
            span(*leaf, fixture.source, components[index / 2]);
        } else {
            assert_eq!(leaf.kind(), ".");
            assert!(!leaf.is_named());
            span(*leaf, fixture.source, ".");
        }
        offset = leaf.end_byte();
    }
    assert_eq!(offset, prefix.end_byte());
}

#[test]
fn nested_qualified_callees_preserve_prefix_topology_and_semicolon_edit_identity() {
    assert_eq!(NESTED_QUALIFIED_CALLEE_SOURCES.len(), 16);
    for pair in NESTED_QUALIFIED_CALLEE_SOURCES.chunks_exact(2) {
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
        nested_qualified_prefix_identity(&original, single);
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
            nested_qualified_prefix_identity(&current, next);
            nested_qualified_prefix_identity(&fresh, next);
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
