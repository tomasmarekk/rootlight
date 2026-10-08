// Inline value names retain the complete aliased binder and exact expression body.
// Append after the unchanged frontier, quote, and shared-consumer helpers. These
// prospective controls do not assert compiler typing or semantic acceptance.

include!("inline-value-sources.rs");

fn inline_value_name_check(
    tree: &Tree,
    source: &str,
    written_name: &str,
) -> (Vec<QuotePaddingCapture>, Vec<QuotePaddingCapture>) {
    assert!(source.len() <= 256);
    clean(tree, source);
    assert!(nodes(tree).len() <= 128);
    assert_eq!(source.matches(written_name).count(), 1);
    let written = format!("let inline {written_name} = add 3");
    let binding = exact(tree, source, "function_or_value_defn", &written);
    let head = exact(
        tree,
        source,
        "value_declaration_left",
        &format!("inline {written_name}"),
    );
    assert_eq!(head.parent(), Some(binding));
    assert_eq!(head.child_count(), 2);
    specialized_argument_punctuation(head, source, 0, "inline");
    assert_eq!(head.field_name_for_child(0), None);
    assert_eq!(head.field_name_for_child(1), None);
    let pattern = frontier_children(head, source, &[("identifier_pattern", written_name)])[0];
    assert_eq!(pattern.child_count(), 1);
    assert_eq!(pattern.field_name_for_child(0), None);
    let name = frontier_children(pattern, source, &[("long_identifier_or_op", written_name)])[0];
    assert_eq!(name.child_count(), 1);
    assert_eq!(name.field_name_for_child(0), None);
    let identifier = frontier_children(name, source, &[("identifier", written_name)])[0];
    assert_eq!(identifier.child_count(), 0);
    let body = binding.child_by_field_name("body").unwrap();
    assert_eq!(body.kind(), "application_expression");
    span(body, source, "add 3");
    assert_eq!(body.parent(), Some(binding));
    assert_eq!(named_operands(binding), vec![head, body]);
    let operands = frontier_children(
        body,
        source,
        &[("long_identifier_or_op", "add"), ("const", "3")],
    );
    let callee = operands[0];
    let argument = operands[1];
    assert_eq!(callee.child_count(), 1);
    let callee_identifier = frontier_children(callee, source, &[("identifier", "add")])[0];
    assert_eq!(callee_identifier.child_count(), 0);
    let integer = frontier_children(argument, source, &[("int", "3")])[0];
    assert_eq!(integer.child_count(), 0);
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    assert_eq!(named_operands(declaration), vec![binding]);
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let module_name = module.child_by_field_name("name").unwrap();
    span(module_name, source, "InlineValueExample");
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_eq!(named_operands(following_declaration), vec![following]);
    assert_eq!(
        named_operands(module),
        vec![module_name, declaration, following_declaration]
    );
    let following_value = following.child_by_field_name("body").unwrap();
    assert_eq!(following_value.kind(), "const");
    span(following_value, source, "30");
    assert_eq!(following_value.parent(), Some(following));
    assert!(body.end_byte() < following.start_byte());
    assert!(declaration.end_byte() < following_declaration.start_byte());
    assert_eq!(
        expression_queries(tree, source, &[body, callee, argument, following_value]),
        4
    );
    let contextual = quote_padding_ordered_captures(tree, source);
    for excluded in [head, pattern, name, identifier] {
        assert!(
            contextual
                .iter()
                .filter(|event| event.capture == "generic")
                .all(|event| event.bytes.start < excluded.start_byte()
                    || event.bytes.end > excluded.end_byte())
        );
    }
    let owners = shared_consumer_owner_captures(
        tree,
        source,
        "(function_or_value_defn \
           (value_declaration_left \
             (identifier_pattern \
               (long_identifier_or_op . (identifier) @name .) @name_reference) @pattern) @head \
           body: (application_expression \
             (long_identifier_or_op . (identifier) @callee_name .) @callee \
             (const (int) @integer) @argument) @body) @binding",
        &[
            ("name", identifier),
            ("name_reference", name),
            ("pattern", pattern),
            ("head", head),
            ("callee_name", callee_identifier),
            ("callee", callee),
            ("integer", integer),
            ("argument", argument),
            ("body", body),
            ("binding", binding),
        ],
    );
    (contextual, owners)
}

#[test]
fn parameterless_inline_value_names_preserve_alias_ownership_and_edit_identity() {
    assert_eq!(INLINE_VALUE_NAME_SOURCES.len(), 2);
    for fixture in INLINE_VALUE_NAME_SOURCES {
        assert_eq!(fixture.source.matches("add3").count(), 1, "{}", fixture.id);
        let changed = fixture.source.replacen("add3", "add4", 1);
        assert_eq!(
            changed.replace("add4", "add3"),
            fixture.source,
            "{}",
            fixture.id
        );
        let mut reused = parser(false);
        let original = parse(&mut reused, fixture.source, None);
        let original_snapshot = snapshot(&original, fixture.source);
        let original_captures = inline_value_name_check(&original, fixture.source, "add3");
        let mut current = original;
        let mut before = fixture.source;
        for (after, written_name) in [(changed.as_str(), "add4"), (fixture.source, "add3")] {
            edit(&mut current, before, after);
            current = parse(&mut reused, after, Some(&current));
            let fresh = parse(&mut parser(false), after, None);
            let actual = inline_value_name_check(&current, after, written_name);
            let expected = inline_value_name_check(&fresh, after, written_name);
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
                assert_eq!(actual, original_captures, "{}", fixture.id);
            }
            before = after;
        }
        assert_eq!(before, fixture.source);
    }
}
