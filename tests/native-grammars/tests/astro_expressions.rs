//! Compares complete original expression slices in each supported host context.
//! The shared fixture is also parsed independently by JavaScript, without execution.

use tree_sitter::Parser;

#[test]
fn html_interpolation_regex_does_not_end_at_a_pattern_brace() {
    for expression in ["/}/.test(text)", "/[/*}]/.test(text)"] {
        let source = format!("<p>{{{expression}}}</p>");
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_astro_next::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn regex_and_division_keep_source_boundaries_in_each_host_context() {
    for expression in include_str!("../fixtures/astro/expressions.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let cases = [
            (
                format!("<p>{{{expression}}}</p>"),
                "html_interpolation",
                format!("{{{expression}}}"),
            ),
            (
                format!("<Card value={{{expression}}} />"),
                "attribute_js_expr",
                expression.to_owned(),
            ),
            (
                format!("---\nconst result = {expression};\n---\n<Card />"),
                "frontmatter_js_block",
                format!("\nconst result = {expression};"),
            ),
            (
                format!("<Card value={{`result ${{{expression}}}`}} />"),
                "attribute_js_expr",
                format!("`result ${{{expression}}}`"),
            ),
        ];
        for (source, kind, expected) in cases {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter_astro_next::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let mut nodes = vec![tree.root_node()];
            let mut values = Vec::new();
            while let Some(node) = nodes.pop() {
                if node.kind() == kind {
                    values.push(node.utf8_text(source.as_bytes()).unwrap());
                }
                let mut cursor = node.walk();
                nodes.extend(node.children(&mut cursor));
            }
            if kind == "html_interpolation" {
                // Nested JS braces are separate native interpolation nodes.
                assert!(values.contains(&expected.as_str()), "{source}: {values:?}");
            } else {
                assert_eq!(values, [expected.as_str()], "{source}");
            }
        }
    }
}
