// Prospective native EOF controls reuse eight actual FCS176 sources and accepted15.
// Append after the frozen native frontier helpers; no compiler/native kind equivalence.
// Only previously observed body offsets/parents are exact native expectations.

include!("eof-sources.rs");

fn eof_single_context_queries(tree: &Tree, source: &str, expected: &[(&str, Node<'_>)]) {
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "(_module_elem) @module_element\n\
         (function_or_value_defn) @binding\n\
         (application_expression) @application\n",
    )
    .expect("retained root module/body queries must compile");
    let walked = nodes(tree);
    assert!(walked.len() <= 64);
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(64);
    let mut actual = Vec::new();
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            assert!(actual.len() < 16);
            let capture = found.captures[*index];
            let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
            let joins: Vec<_> = walked
                .iter()
                .filter(|(node, _)| *node == capture.node)
                .collect();
            assert_eq!(joins.len(), 1);
            assert_eq!(joins[0].0.id(), capture.node.id());
            assert_eq!(joins[0].0.kind(), capture.node.kind());
            assert_eq!(joins[0].0.byte_range(), capture.node.byte_range());
            assert_eq!(joins[0].0.parent(), capture.node.parent());
            span(capture.node, source, text(capture.node, source));
            actual.push((name, capture.node));
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    assert_eq!(actual.len(), expected.len());
    for (name, node) in expected {
        assert_eq!(
            actual
                .iter()
                .filter(|(actual_name, actual_node)| actual_name == name && actual_node == node)
                .count(),
            1,
            "raw capture multiplicity for {name}",
        );
    }
}

#[test]
fn eof_single_named_matrix_keeps_exact_body_and_root_context_without_recovery() {
    assert_eq!(EOF_NAMED_SOURCES.len(), 8);
    for fixture in EOF_NAMED_SOURCES {
        assert!(!fixture.signature);
        assert!(fixture.source.is_ascii() && fixture.source.len() <= 64);
        let mut parser = parser(false);
        let tree = parse(&mut parser, fixture.source, None);
        clean(&tree, fixture.source);
        let application = exact(&tree, fixture.source, "application_expression", "ignore 11");
        let semicolon = fixture.source.find(';').unwrap();
        assert_eq!(
            application.start_byte(),
            fixture.source.find("ignore 11").unwrap(),
            "{}",
            fixture.id
        );
        assert_eq!(application.end_byte(), semicolon, "{}", fixture.id);
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
        // Hidden trailing extras may affect a containing native range. Compiler
        // binding endpoints are not used as an invented native parent contract.
        span(binding, fixture.source, text(binding, fixture.source));
        span(
            declaration,
            fixture.source,
            text(declaration, fixture.source),
        );
        eof_single_context_queries(
            &tree,
            fixture.source,
            &[
                ("module_element", declaration),
                ("binding", binding),
                ("application", application),
            ],
        );
        expression_queries(&tree, fixture.source, &[application, callee, argument]);
    }
}

#[test]
fn eof_single_all_original_accepted_sources_remain_clean_with_declared_profile_queries() {
    assert_eq!(EOF_SELECTION_SOURCES.len(), 15);
    for fixture in EOF_SELECTION_SOURCES {
        assert!(fixture.source.is_ascii() && fixture.source.len() <= 256);
        let mut parser = parser(fixture.signature);
        let tree = parse(&mut parser, fixture.source, None);
        clean(&tree, fixture.source);
        let walked = nodes(&tree);
        assert!(walked.len() <= 256, "{}", fixture.id);
        for (node, _) in &walked {
            span(*node, fixture.source, text(*node, fixture.source));
        }
        let language = if fixture.signature {
            tree_sitter_fsharp::LANGUAGE_SIGNATURE
        } else {
            tree_sitter_fsharp::LANGUAGE_FSHARP
        };
        let policy = if fixture.signature {
            "(_module_signature_elements) @module_signature_element\n\
             (named_module) @module\n\
             (sequential_expression) @sequence\n\
             (typecast_expression) @typecast\n\
             (const) @const\n\
             (long_identifier_or_op) @identifier_expression\n"
        } else {
            "[(_expression) (_atomic_expression) (_arg_expression) (_module_expression) \
             (_application_operand/ce_expression) (application_expression) \
             (sequential_expression) (typecast_expression)] @generic\n\
             (_expression) @legacy_expression\n\
             (_module_elem) @module_element\n\
             (_module_expression) @module_expression\n\
             (named_module) @module\n\
             (declaration_expression) @declaration\n\
             (function_or_value_defn) @binding\n\
             (do) @public_do\n\
             (do_expression) @do_expression\n\
             (for_expression) @for\n\
             (while_expression) @while\n\
             (application_expression) @application\n\
             (sequential_expression) @sequence\n\
             (const) @const\n\
             (long_identifier_or_op) @identifier_expression\n"
        };
        let query =
            Query::new(&language.into(), policy).expect("frozen profile query must compile");
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(256);
        let mut count = 0;
        {
            let mut captures = cursor.captures(&query, tree.root_node(), fixture.source.as_bytes());
            while let Some((found, index)) = captures.next() {
                count += 1;
                assert!(count <= 512, "{}", fixture.id);
                let capture = found.captures[*index];
                let joins: Vec<_> = walked
                    .iter()
                    .filter(|(node, _)| *node == capture.node)
                    .collect();
                assert_eq!(joins.len(), 1);
                assert_eq!(joins[0].0.id(), capture.node.id());
                assert_eq!(joins[0].0.kind(), capture.node.kind());
                assert_eq!(joins[0].0.byte_range(), capture.node.byte_range());
                assert_eq!(joins[0].0.parent(), capture.node.parent());
                span(
                    capture.node,
                    fixture.source,
                    text(capture.node, fixture.source),
                );
            }
        }
        assert!(count > 0, "{}", fixture.id);
        assert!(!cursor.did_exceed_match_limit());
    }
}
