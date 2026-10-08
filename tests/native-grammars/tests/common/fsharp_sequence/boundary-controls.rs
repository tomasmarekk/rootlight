// Native ownership contracts for twelve compiler-qualified specialized boundaries.
// Included after the unchanged 103-control pack; node inventories come from the
// preferred grammar, not from returned query captures or compiler AST topology.

include!("specialized-head-boundary-fixtures.rs");

fn specialized_boundary_sole_field(owner: Node<'_>, child: Node<'_>, name: &str) {
    for index in 0..owner.child_count() {
        let index = u32::try_from(index).unwrap();
        assert_eq!(
            owner.field_name_for_child(index),
            (owner.child(index) == Some(child)).then_some(name),
        );
    }
}

fn specialized_boundary_literal_expressions<'tree>(
    literal: Node<'tree>,
    family: SpecializedArgumentFamily,
    source: &str,
    binder: Node<'tree>,
) -> Vec<Node<'tree>> {
    match family {
        SpecializedArgumentFamily::VerbatimString => {
            let children =
                frontier_children(literal, source, &[("verbatim_string", "@\"neutral\"")]);
            frontier_children(children[0], source, &[]);
            Vec::new()
        }
        SpecializedArgumentFamily::InterpolatedString => {
            let string = frontier_children(literal, source, &[("string", "$\"neutral {value}\"")]);
            let format = frontier_children(
                string[0],
                source,
                &[("format_string", "$\"neutral {value}\"")],
            );
            let evaluation =
                frontier_children(format[0], source, &[("format_string_eval", "{value}")]);
            let expression =
                frontier_children(evaluation[0], source, &[("long_identifier_or_op", "value")]);
            assert_eq!(evaluation[0].child_count(), 3);
            specialized_argument_punctuation(evaluation[0], source, 0, "{");
            specialized_argument_punctuation(evaluation[0], source, 2, "}");
            assert_ne!(expression[0].id(), binder.id());
            assert!(expression[0].start_byte() > binder.end_byte());
            expression
        }
        SpecializedArgumentFamily::Quotation => {
            let quoted = frontier_children(literal, source, &[("long_identifier_or_op", "value")]);
            assert_eq!(literal.child_count(), 3);
            specialized_argument_punctuation(literal, source, 0, "<@");
            specialized_argument_punctuation(literal, source, 2, "@>");
            assert_ne!(quoted[0].id(), binder.id());
            assert!(quoted[0].start_byte() > binder.end_byte());
            quoted
        }
    }
}

fn specialized_boundary_check(index: usize) {
    let fixture = &SPECIALIZED_BOUNDARY_SOURCES[index];
    let source = fixture.source;
    assert!(!source.ends_with('\n'), "{}", fixture.id);
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let body_text = match fixture.decision {
        SpecializedBoundaryDecision::Sibling => String::from("consume"),
        SpecializedBoundaryDecision::Continuation => {
            format!("consume{newline}        {}", fixture.atom)
        }
    };
    let binding_text = format!("let run value ={newline}    {body_text}");
    let tree = parse(&mut parser(false), source, None);
    clean(&tree, source);
    let binding = exact(&tree, source, "function_or_value_defn", &binding_text);
    let body = binding.child_by_field_name("body").unwrap();
    span(body, source, &body_text);
    assert_eq!(body.parent(), Some(binding));
    specialized_boundary_sole_field(binding, body, "body");
    let body_kind = match fixture.decision {
        SpecializedBoundaryDecision::Sibling => "long_identifier_or_op",
        SpecializedBoundaryDecision::Continuation => "application_expression",
    };
    assert_eq!(body.kind(), body_kind);
    let binding_children = frontier_children(
        binding,
        source,
        &[
            ("function_declaration_left", "run value"),
            (body_kind, &body_text),
        ],
    );
    assert_eq!(binding_children[1], body);
    let head = binding_children[0];
    let head_children = frontier_children(
        head,
        source,
        &[("identifier", "run"), ("argument_patterns", "value")],
    );
    let binder = frontier_children(head_children[1], source, &[("long_identifier", "value")])[0];
    assert!(binder.end_byte() < body.start_byte());

    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    span(declaration, source, &binding_text);
    assert_eq!(
        frontier_children(
            declaration,
            source,
            &[("function_or_value_defn", &binding_text)]
        ),
        vec![binding],
    );
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    assert_eq!(module.parent(), Some(tree.root_node()));
    assert_eq!(named_operands(tree.root_node()), vec![module]);
    span(module, source, source);
    let following = exact(&tree, source, "function_or_value_defn", FOLLOWING);
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    span(following_declaration, source, FOLLOWING);
    assert_eq!(
        frontier_children(
            following_declaration,
            source,
            &[("function_or_value_defn", FOLLOWING)]
        ),
        vec![following],
    );
    let following_body = following.child_by_field_name("body").unwrap();
    specialized_boundary_sole_field(following, following_body, "body");
    frontier_signed_constant(following_body, source, "30");
    assert!(body.end_byte() < following.start_byte());
    assert!(declaration.end_byte() < following_declaration.start_byte());

    let literal_kind = match fixture.family {
        SpecializedArgumentFamily::Quotation => "literal_expression",
        SpecializedArgumentFamily::VerbatimString
        | SpecializedArgumentFamily::InterpolatedString => "const",
    };
    let mut module_expected = vec![
        ("long_identifier", "PendingArgumentHeadExample"),
        ("declaration_expression", binding_text.as_str()),
    ];
    if matches!(fixture.decision, SpecializedBoundaryDecision::Sibling) {
        module_expected.push((literal_kind, fixture.atom));
    }
    module_expected.push(("declaration_expression", FOLLOWING));
    let module_children = frontier_children(module, source, &module_expected);
    assert_eq!(module_children[1], declaration);
    assert_eq!(module_children.last().copied(), Some(following_declaration));
    assert_eq!(module.child_by_field_name("name"), Some(module_children[0]));
    specialized_boundary_sole_field(module, module_children[0], "name");

    let (literal, mut inventory) = match fixture.decision {
        SpecializedBoundaryDecision::Sibling => {
            span(body, source, "consume");
            let literal = module_children[2];
            assert_eq!(literal.parent(), Some(module));
            assert!(body.end_byte() < literal.start_byte());
            assert!(literal.end_byte() < following.start_byte());
            assert_eq!(
                &source[declaration.end_byte()..literal.start_byte()],
                newline
            );
            assert_eq!(&source[literal.end_byte()..following.start_byte()], newline);
            (literal, vec![body, literal, following_body])
        }
        SpecializedBoundaryDecision::Continuation => {
            let [callee, literal] = frontier_application(
                body,
                source,
                ("long_identifier_or_op", "consume"),
                (literal_kind, fixture.atom),
            );
            assert_eq!(body.child_count(), 2);
            for child in 0..body.child_count() {
                assert_eq!(
                    body.field_name_for_child(u32::try_from(child).unwrap()),
                    None
                );
            }
            assert_eq!(
                &source[callee.end_byte()..literal.start_byte()],
                format!("{newline}        "),
            );
            (literal, vec![body, callee, literal, following_body])
        }
    };
    assert_eq!(literal.kind(), literal_kind);
    span(literal, source, fixture.atom);
    inventory.extend(specialized_boundary_literal_expressions(
        literal,
        fixture.family,
        source,
        binder,
    ));
    let expected_count = match (fixture.decision, fixture.family) {
        (SpecializedBoundaryDecision::Sibling, SpecializedArgumentFamily::VerbatimString) => 3,
        (
            SpecializedBoundaryDecision::Sibling,
            SpecializedArgumentFamily::InterpolatedString | SpecializedArgumentFamily::Quotation,
        ) => 4,
        (SpecializedBoundaryDecision::Continuation, SpecializedArgumentFamily::VerbatimString) => 4,
        (
            SpecializedBoundaryDecision::Continuation,
            SpecializedArgumentFamily::InterpolatedString | SpecializedArgumentFamily::Quotation,
        ) => 5,
    };
    assert_eq!(inventory.len(), expected_count);
    for (position, expression) in inventory.iter().enumerate() {
        assert!(
            inventory[..position]
                .iter()
                .all(|other| other.id() != expression.id())
        );
    }
    assert_eq!(
        expression_queries(&tree, source, &inventory),
        expected_count
    );
    frontier_explicit_queries(&tree, source);

    // The contextual query must select a direct module expression, never a
    // nested argument or binding wrapper with an equivalent byte spelling.
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "(named_module (_module_expression) @standalone)",
    )
    .expect("declared direct module expression context must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let mut captures = Vec::new();
    {
        let mut events = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = events.next() {
            assert!(captures.len() < 20_000);
            let capture = found.captures[*index];
            assert_eq!(
                query.capture_names()[usize::try_from(capture.index).unwrap()],
                "standalone",
            );
            captures.push(capture.node);
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    match fixture.decision {
        SpecializedBoundaryDecision::Sibling => {
            assert_eq!(captures, vec![literal]);
            assert_eq!(captures[0].parent(), Some(module));
            assert_eq!(captures[0].byte_range(), literal.byte_range());
            span(captures[0], source, fixture.atom);
        }
        SpecializedBoundaryDecision::Continuation => assert!(captures.is_empty()),
    }
}

#[test]
fn specialized_boundary_sibling_verbatim_string_lf_eof() {
    specialized_boundary_check(0);
}

#[test]
fn specialized_boundary_continuation_verbatim_string_lf_eof() {
    specialized_boundary_check(1);
}

#[test]
fn specialized_boundary_sibling_verbatim_string_crlf_eof() {
    specialized_boundary_check(2);
}

#[test]
fn specialized_boundary_continuation_verbatim_string_crlf_eof() {
    specialized_boundary_check(3);
}

#[test]
fn specialized_boundary_sibling_interpolated_string_lf_eof() {
    specialized_boundary_check(4);
}

#[test]
fn specialized_boundary_continuation_interpolated_string_lf_eof() {
    specialized_boundary_check(5);
}

#[test]
fn specialized_boundary_sibling_interpolated_string_crlf_eof() {
    specialized_boundary_check(6);
}

#[test]
fn specialized_boundary_continuation_interpolated_string_crlf_eof() {
    specialized_boundary_check(7);
}

#[test]
fn specialized_boundary_sibling_quotation_lf_eof() {
    specialized_boundary_check(8);
}

#[test]
fn specialized_boundary_continuation_quotation_lf_eof() {
    specialized_boundary_check(9);
}

#[test]
fn specialized_boundary_sibling_quotation_crlf_eof() {
    specialized_boundary_check(10);
}

#[test]
fn specialized_boundary_continuation_quotation_crlf_eof() {
    specialized_boundary_check(11);
}
