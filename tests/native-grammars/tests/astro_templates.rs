//! Pins source boundaries for nested templates without recursive native calls.
//! Deep cases run on a deliberately small thread stack to expose call-stack growth.

use tree_sitter::Parser;

fn assert_expression(expression: &str) {
    let source = format!("<Card value={{{expression}}} />");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_astro_next::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut pending = vec![tree.root_node()];
    let mut values = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == "attribute_js_expr" {
            values.push(node.utf8_text(source.as_bytes()).unwrap());
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    assert_eq!(values, [expression]);
}

#[test]
fn templates_keep_escaped_dollars_braces_comments_and_division() {
    for expression in [
        r"`literal \${notInterpolation} tail`",
        r"`literal \` backtick`",
        "`outer ${{value: `inner ${value}`}.value} tail`",
        "`outer ${total / count} tail`",
        "`outer ${/* } ` */ value} tail`",
        "`outer ${// } `\n value} tail`",
        "`outer ${'}' + \"}\"} tail`",
    ] {
        assert_expression(expression);
    }
}

#[test]
fn unterminated_templates_do_not_claim_complete_expression_tokens() {
    for expression in ["`unfinished", "`outer ${value", "`outer ${'unfinished"] {
        let source = format!("<Card value={{{expression}}} />");
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_astro_next::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(tree.root_node().has_error());
    }
}

#[test]
fn deeply_nested_templates_fit_a_small_native_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let expression = format!("{}value{}", "`${".repeat(1500), "}`".repeat(1500));
            assert_expression(&expression);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn template_depth_boundary_rejects_without_poisoning_reuse() {
    let at_limit = format!("{}value{}", "`${".repeat(2048), "}`".repeat(2048));
    assert_expression(&at_limit);
    let too_deep = format!(
        "<Card value={{{}value{}}} />",
        "`${".repeat(2049),
        "}`".repeat(2049)
    );
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_astro_next::LANGUAGE.into())
        .unwrap();
    let rejected = parser.parse(too_deep, None).unwrap();
    assert!(rejected.root_node().has_error());
    let recovered = parser
        .parse("<Card value={`valid ${value}`} />", None)
        .unwrap();
    assert!(!recovered.root_node().has_error());
}
