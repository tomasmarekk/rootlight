//! Typed declarations must own their names without stealing command-call syntax.
//! Finite authored fixtures preserve exact source spans and are never executed.
use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}
fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut result = vec![];
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        result.push(node);
    }
    result
}
fn valid(source: &str) -> Tree {
    let tree = parse(source);
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some());
    }
    tree
}
fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == kind && n.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn typed_local_without_initializer_owns_only_its_declaration() {
    for declaration in [
        "String label",
        "int count",
        "java.lang.String label",
        "String[] labels",
        "List<String> labels",
        "pkg.type[] values",
        "pkg.type<String> values",
    ] {
        let source = format!("def run() {{ {declaration}\nfinish() }}\n");
        let tree = valid(&source);
        exact(&tree, &source, "local_variable_declaration", declaration);
        exact(&tree, &source, "method_invocation", "finish()");
    }
}

#[test]
fn mixed_initialized_and_uninitialized_declarators_are_preserved() {
    let declaration = "String first, second = 'λ😀', third";
    let source = format!("def run() {{ {declaration}\nfinish() }}\n");
    let tree = valid(&source);
    exact(&tree, &source, "local_variable_declaration", declaration);
    for name in ["first", "third"] {
        exact(&tree, &source, "variable_declarator", name);
    }
}

#[test]
fn initialized_lowercase_types_remain_valid() {
    for declaration in ["widget value = factory()", "pkg.widget value = factory()"] {
        let source = format!("def run() {{ {declaration}\nfinish() }}\n");
        let tree = valid(&source);
        exact(&tree, &source, "local_variable_declaration", declaration);
    }
}

#[test]
fn untyped_command_prefix_is_not_a_type_declaration() {
    for source in [
        "consume value\n",
        "service.consume value\n",
        "String\nlabel\n",
    ] {
        let tree = parse(source);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "local_variable_declaration"),
            "false declaration: {source}\n{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn enhanced_for_preserves_type_variable_collection_and_body() {
    for header in [
        "String item in items",
        "int item : items",
        "def item in items",
        "item in items",
        "final String item in items",
    ] {
        let source = format!("for ({header}) {{ consume(item) }}\n");
        let tree = valid(&source);
        let node = nodes(tree.root_node())
            .into_iter()
            .find(|n| n.kind() == "for_in_statement")
            .unwrap();
        assert_eq!(
            node.child_by_field_name("variable")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "item"
        );
        assert_eq!(
            node.child_by_field_name("value")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "items"
        );
        exact(&tree, &source, "method_invocation", "consume(item)");
    }
}

#[test]
fn classic_for_initializer_accepts_a_typed_declaration() {
    let source = "for (int i = 0; i < 3; i++) { consume(i) }\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "int i = 0");
}

#[test]
fn final_and_annotated_local_declarations_keep_modifiers() {
    for declaration in [
        "final String label",
        "@Deprecated String label",
        "final label = 'x'",
    ] {
        let source = format!("def run() {{ {declaration}\nfinish() }}\n");
        let tree = valid(&source);
        exact(&tree, &source, "local_variable_declaration", declaration);
    }
}

#[test]
fn uninitialized_dynamic_local_does_not_own_the_next_statement() {
    let source = "def run() { def value\nfinish() }\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "def value");
    exact(&tree, source, "method_invocation", "finish()");
}

#[test]
fn capitalized_names_remain_available_outside_type_context() {
    for source in [
        "def Label = 'x'\nconsume(Label)\n",
        "String.valueOf(1)\n",
        "service.Consume()\n",
        "for (String in items) {}\n",
        "String\nlabel\n",
    ] {
        valid(source);
    }
    let source = "String\nlabel\n";
    let tree = valid(source);
    exact(&tree, source, "expression_statement", "String");
    exact(&tree, source, "expression_statement", "label");
}

#[test]
fn declaration_modifiers_do_not_steal_class_and_method_headers() {
    let source = "@Deprecated public class Meter { private int value; public final String read() { 'x' } }\nfinal String render() { 'y' }\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "method_declaration",
        "public final String read() { 'x' }",
    );
    exact(
        &tree,
        source,
        "method_declaration",
        "final String render() { 'y' }",
    );
}

#[test]
fn incomplete_initializers_and_loop_headers_are_rejected() {
    for source in [
        "def run() { String value = }",
        "for (String item in) {}",
        "for (final in items) {}",
    ] {
        assert!(
            parse(source).root_node().has_error(),
            "accepted invalid input: {source}"
        );
    }
}

#[test]
fn declaration_edits_preserve_exact_utf8_points_and_fresh_equivalence() {
    use tree_sitter::{InputEdit, Point};
    let point = |source: &str, offset: usize| {
        let prefix = &source[..offset];
        Point::new(
            prefix.bytes().filter(|b| *b == b'\n').count(),
            prefix
                .rfind('\n')
                .map_or(prefix.len(), |last| prefix.len() - last - 1),
        )
    };
    let source =
        "def run() { String label\nlabel = 'λ😀'\nfor (String item in items) { consume(item) } }\n";
    for (before, after) in [
        ("String label", "String label = 'x'"),
        ("String item", "final String item"),
        ("λ😀", "δ🌍"),
        ("label\n", "label;\r\n"),
    ] {
        let changed = source.replacen(before, after, 1);
        let start = source.find(before).unwrap();
        let mut old = valid(source);
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        let fresh = valid(&changed);
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
        let signature = |tree: &Tree| {
            nodes(tree.root_node())
                .into_iter()
                .map(|n| {
                    (
                        n.kind().to_owned(),
                        n.byte_range(),
                        n.start_position(),
                        n.end_position(),
                        n.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(signature(&incremental), signature(&fresh));
        for node in nodes(incremental.root_node()) {
            assert_eq!(node.start_position(), point(&changed, node.start_byte()));
            assert_eq!(node.end_position(), point(&changed, node.end_byte()));
        }
    }
}
