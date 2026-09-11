//! Native grammar admission controls independent of the diagnostic corpus.
//! Sources are parsed only; no Groovy code or build from a corpus is executed.

#[cfg(test)]
mod tests {
    use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

    fn parser() -> Parser {
        let mut parser = Parser::new();
        let language = dekobon_tree_sitter_groovy::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        assert_eq!(language.abi_version(), 15);
        parser
    }

    fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
        let mut pending = vec![root];
        let mut result = Vec::new();
        while let Some(node) = pending.pop() {
            let mut cursor = node.walk();
            pending.extend(node.children(&mut cursor));
            result.push(node);
        }
        result.sort_by_key(|n| (n.start_byte(), n.end_byte(), n.kind_id()));
        result
    }

    fn point(source: &str, offset: usize) -> Point {
        let prefix = &source[..offset];
        Point::new(
            prefix.bytes().filter(|b| *b == b'\n').count(),
            prefix
                .rfind('\n')
                .map_or(prefix.len(), |last| prefix.len() - last - 1),
        )
    }

    fn valid(source: &str) -> Tree {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}\n{}",
            tree.root_node().to_sexp()
        );
        for node in nodes(tree.root_node()) {
            assert!(source.get(node.byte_range()).is_some());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
        }
        tree
    }

    fn has_exact(tree: &Tree, source: &str, kind: &str, text: &str) {
        assert!(
            nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == kind
                    && n.utf8_text(source.as_bytes()).unwrap().trim_end() == text),
            "missing {kind} {text:?}: {}",
            tree.root_node().to_sexp()
        );
    }

    fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, bool)> {
        nodes(tree.root_node())
            .iter()
            .map(|n| (n.kind().into(), n.byte_range(), n.is_missing()))
            .collect()
    }

    #[test]
    fn declarations_imports_and_closure_ownership() {
        let source = "package metrics\nimport java.util.List as Items\ntrait Named { String label() { 'x' } }\nclass Meter implements Named {\nint value = 1\ndef adjust(int delta) { def step = { int n -> n + delta }; step(value) }\n}\n";
        let tree = valid(source);
        has_exact(
            &tree,
            source,
            "method_declaration",
            "String label() { 'x' }",
        );
        has_exact(&tree, source, "closure", "{ int n -> n + delta }");
    }

    #[test]
    fn utf8_strings_and_crlf_ranges() {
        for newline in ["\n", "\r\n"] {
            let source =
                "def label = 'λ😀'\ndef render(x) { \"${x} λ😀\" }\n".replace('\n', newline);
            valid(&source);
        }
    }

    #[test]
    fn utf8_identifiers_are_declarations() {
        let source = "def λ = 1\ndef render() { λ }\n";
        valid(source);
    }

    #[test]
    fn typed_uninitialized_local_is_not_a_call_or_two_statements() {
        let source = "def render() { String label\nlabel = 'x'\nlabel }\n";
        let tree = valid(source);
        has_exact(&tree, source, "local_variable_declaration", "String label");
    }

    #[test]
    fn parenthesized_and_command_calls_are_structural() {
        let source = "consume(value)\nconsume value\n";
        let tree = valid(source);
        has_exact(&tree, source, "method_invocation", "consume(value)");
        has_exact(&tree, source, "command_chain", "consume value");
    }

    #[test]
    fn named_command_arguments_are_not_labels() {
        let source = "publish name: 'artifact', enabled: true\n";
        let tree = valid(source);
        has_exact(
            &tree,
            source,
            "command_chain",
            "publish name: 'artifact', enabled: true",
        );
    }

    #[test]
    fn chained_command_call_preserves_whole_expression() {
        let source = "route left then right\n";
        let tree = valid(source);
        has_exact(&tree, source, "command_chain", "route left then right");
    }

    #[test]
    fn trailing_closure_belongs_to_call() {
        let source = "items.collect { n -> n + 1 }\n";
        let tree = valid(source);
        has_exact(
            &tree,
            source,
            "method_invocation",
            "items.collect { n -> n + 1 }",
        );
    }

    #[test]
    fn slashy_strings_allow_leading_space() {
        let source = "def text = / leading space /\n";
        let tree = valid(source);
        has_exact(&tree, source, "string_literal", "/ leading space /");
    }

    #[test]
    fn slashy_strings_allow_newlines() {
        let source = "def text = /first\nsecond/\n";
        let tree = valid(source);
        has_exact(&tree, source, "string_literal", "/first\nsecond/");
    }

    #[test]
    fn interpolated_string_flavors_expose_calls() {
        for value in [
            r#""${render()}""#,
            r#""""${render()}""""#,
            "/prefix${render()}suffix/",
            "$/prefix${render()}suffix/$",
        ] {
            let source = format!("def text = {value}\n");
            let tree = valid(&source);
            has_exact(&tree, &source, "method_invocation", "render()");
        }
    }

    #[test]
    fn division_is_not_a_string() {
        for expression in ["a / b / c", "a/b/c"] {
            let source = format!("def result = {expression}\n");
            let tree = valid(&source);
            has_exact(&tree, &source, "binary_expression", expression);
            assert!(
                !nodes(tree.root_node())
                    .iter()
                    .any(|n| n.kind() == "string_literal")
            );
        }
    }

    #[test]
    fn arithmetic_is_not_a_command_with_unary_argument() {
        for expression in ["n + delta", "n - delta", "n+delta", "n-delta", "n * delta"] {
            let source = format!("def step = {{ n -> {expression} }}\n");
            let tree = valid(&source);
            has_exact(&tree, &source, "binary_expression", expression);
        }
    }

    #[test]
    fn newline_does_not_join_two_independent_identifiers() {
        let source = "first\nsecond\n";
        let tree = valid(source);
        assert_eq!(
            tree.root_node().named_child_count(),
            2,
            "{}",
            tree.root_node().to_sexp()
        );
    }

    #[test]
    fn semicolon_separates_top_level_statements() {
        valid("def first = 1; def second = 2;\n");
    }

    #[test]
    fn typed_for_loop_is_preserved() {
        valid("for (String item in items) { consume(item) }\n");
    }

    #[test]
    fn enum_constants_and_methods_are_preserved() {
        valid("enum Mode { FAST, SLOW; String label() { name() } }\n");
    }

    #[test]
    fn multiple_variable_declarations_are_preserved() {
        valid("def first = 1, second = 2\n");
    }

    #[test]
    fn multi_catch_and_finally_are_preserved() {
        valid(
            "try { load() } catch (IOException | RuntimeException error) { recover(error) } finally { close() }\n",
        );
    }

    #[test]
    fn quoted_method_names_are_preserved() {
        valid("def 'handles value'(x) { x }\nthis.'handles value'(1)\n");
    }

    #[test]
    fn lazy_gstring_closure_is_preserved() {
        let source = "def text = \"${-> render()}\"\n";
        let tree = valid(source);
        has_exact(&tree, source, "method_invocation", "render()");
    }

    #[test]
    fn comments_and_literals_do_not_invent_methods() {
        let source = "// def hidden() {}\n/**/\n/** def phantom() {} */\ndef text = 'def fake() {}'\ndef other = '''def fakeAgain() {}'''\ndef actual() { 1 }\n";
        let tree = valid(source);
        let declarations: Vec<_> = nodes(tree.root_node())
            .iter()
            .filter(|n| matches!(n.kind(), "method_declaration" | "function_definition"))
            .map(|n| n.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(declarations, ["def actual() { 1 }"]);
    }

    #[test]
    fn malformed_inputs_are_incomplete_and_reset_is_clean() {
        let source = "def actual() { 1 }\n";
        let expected = signature(&valid(source));
        let mut parser = parser();
        for damaged in [
            "def broken( {",
            "def text = \"unterminated",
            "/* unterminated",
            "class Broken {",
        ] {
            assert!(
                parser.parse(damaged, None).unwrap().root_node().has_error(),
                "{damaged}"
            );
            parser.reset();
            assert_eq!(signature(&parser.parse(source, None).unwrap()), expected);
        }
    }

    #[test]
    fn incremental_replay_matches_fresh_utf8_trees() {
        let source = "def label = 'λ😀'\ndef render(x) { \"${x} λ😀\" }\n";
        for (before, after) in [("λ😀", "δ🌍!"), ("render", "display"), ("${x}", "${x + 1}")]
        {
            let start = source.find(before).unwrap();
            let mut changed = source.to_owned();
            changed.replace_range(start..start + before.len(), after);
            let mut parser = parser();
            let mut old = parser.parse(source, None).unwrap();
            old.edit(&InputEdit {
                start_byte: start,
                old_end_byte: start + before.len(),
                new_end_byte: start + after.len(),
                start_position: point(source, start),
                old_end_position: point(source, start + before.len()),
                new_end_position: point(&changed, start + after.len()),
            });
            assert_eq!(
                signature(&parser.parse(&changed, Some(&old)).unwrap()),
                signature(&valid(&changed))
            );
        }
    }
}
