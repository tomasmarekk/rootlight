//! Dart native syntax qualification before production analyzer registration.
//! Source spans and incremental trees are checked independently of editor tags.

use tree_sitter::{InputEdit, Node, Parser, Point, Query};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_dart::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    result
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

#[test]
fn dart_named_declarations_and_modern_forms_have_exact_source_nodes() {
    let source = r#"library sample;
import 'dart:async' as async;
typedef Callback = int Function(int value);
abstract interface class Reader { int read(); }
mixin Counting { int count = 0; }
class Store<T> with Counting implements Reader {
  final T value;
  Store(this.value);
  Store.named(this.value);
  int read() => count;
  int get size => count;
  set size(int value) { count = value; }
  Store<T> operator +(Store<T> other) => this;
}
enum Phase { open, closed }
extension TextLength on String { int get doubled => length * 2; }
extension type UserId(int value) {}
(int, {String label}) describe(int value) => (value, label: 'item');
int select(Object input) => switch (input) { [int first, ...] => first, _ => 0 };
void run() { final (number, label: text) = describe(1); print(text); }
"#;
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    let all = nodes(tree.root_node());
    for (kind, name) in [
        ("class_declaration", "Reader"),
        ("class_declaration", "Store"),
        ("enum_declaration", "Phase"),
        ("extension_declaration", "TextLength"),
        ("function_signature", "describe"),
        ("function_signature", "select"),
        ("function_signature", "run"),
        ("getter_signature", "size"),
        ("setter_signature", "size"),
    ] {
        let found: Vec<_> = all
            .iter()
            .filter(|node| {
                node.kind() == kind
                    && node.child_by_field_name("name").is_some_and(|name_node| {
                        name_node.utf8_text(source.as_bytes()).unwrap() == name
                    })
            })
            .collect();
        assert_eq!(found.len(), 1, "{kind}: {name}");
    }
    for kind in [
        "mixin_declaration",
        "extension_type_declaration",
        "type_alias",
        "operator_signature",
    ] {
        assert_eq!(
            all.iter().filter(|node| node.kind() == kind).count(),
            1,
            "{kind}"
        );
    }
    for node in all {
        assert!(source.get(node.byte_range()).is_some(), "{node:?}");
    }
}

#[test]
fn dart_literals_and_nested_comments_never_become_declarations() {
    for literal in [
        r#"'class Hidden {}'"#,
        r#""class Hidden {}""#,
        r#"r'class Hidden {} $value \n'"#,
        r#"r"class Hidden {} $value \n""#,
        "'''class Hidden {}\n雪'''",
        "\"\"\"class Hidden {}\n雪\"\"\"",
        r#"'before ${value + 1} after $value'"#,
    ] {
        let source = format!(
            "/* outer /* class Fake {{}} */ tail */\nclass Real {{ String text(int value) => {literal}; }}"
        );
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let all = nodes(tree.root_node());
        let classes: Vec<_> = all
            .iter()
            .filter(|node| node.kind() == "class_declaration")
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(
            classes[0]
                .child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "Real"
        );
    }
}

#[test]
fn dart_incremental_edits_equal_fresh_structure_and_all_source_ranges() {
    let mut source = "// source: 雪\r\nclass Sample { String text(int value) => 'before ${value + 1} after'; }\r\n".to_owned();
    let mut parser = parser();
    let mut tree = parser.parse(&source, None).unwrap();
    for (old, new) in [
        ("value + 1", "value + 2000"),
        ("Sample", "Renamed"),
        ("'before ${value + 2000} after'", "r'raw $value \\n'"),
        ("r'raw $value \\n'", "'''before\n${value} after'''"),
        ("雪", "δ"),
        ("text", "render"),
        ("// source: δ", "/* source /* nested */ δ */"),
    ] {
        let start = source.find(old).unwrap();
        let end = start + old.len();
        let changed = source.replacen(old, new, 1);
        tree.edit(&InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: start + new.len(),
            start_position: point(&source, start),
            old_end_position: point(&source, end),
            new_end_position: point(&changed, start + new.len()),
        });
        let incremental = parser.parse(&changed, Some(&tree)).unwrap();
        let fresh = self::parser().parse(&changed, None).unwrap();
        assert!(
            !fresh.root_node().has_error(),
            "{changed}: {}",
            fresh.root_node().to_sexp()
        );
        assert_eq!(
            incremental.root_node().to_sexp(),
            fresh.root_node().to_sexp()
        );
        let shape = |root| {
            nodes(root)
                .into_iter()
                .map(|node| {
                    (
                        node.kind_id(),
                        node.byte_range(),
                        node.start_position(),
                        node.end_position(),
                        node.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(incremental.root_node()), shape(fresh.root_node()));
        source = changed;
        tree = incremental;
    }
}

#[test]
fn dart_malformed_input_retains_explicit_errors_and_bounded_ranges() {
    for source in [
        "class Broken {",
        "void run( {",
        "/* unfinished",
        "final text = 'unfinished",
        "final text = '${unfinished';",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(tree.root_node().has_error(), "accepted {source}");
        for node in nodes(tree.root_node()) {
            assert!(
                source.get(node.byte_range()).is_some(),
                "{source}: {node:?}"
            );
        }
    }
}

#[test]
fn dart_pinned_abi_and_editor_queries_are_loadable() {
    let language: tree_sitter::Language = tree_sitter_dart::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    for query in [
        tree_sitter_dart::HIGHLIGHTS_QUERY,
        tree_sitter_dart::TAGS_QUERY,
        tree_sitter_dart::LOCALS_QUERY,
    ] {
        Query::new(&language, query).unwrap();
    }
}

#[test]
fn dart_annotations_preserve_record_return_types_and_constructor_arguments() {
    for source in [
        "@deprecated (int, String) pair() => (1, 'x');",
        "@Mark(1) (int, String) pair() => (1, 'x');",
        "class C { @override (int, String) pair() => (1, 'x'); }",
        "@Mark.named(1) class C {}",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let all = nodes(tree.root_node());
        if source.contains("pair") {
            let signature = all
                .iter()
                .find(|node| node.kind() == "function_signature")
                .unwrap();
            assert_eq!(
                signature
                    .child_by_field_name("name")
                    .unwrap()
                    .utf8_text(source.as_bytes())
                    .unwrap(),
                "pair"
            );
            assert!(
                signature
                    .utf8_text(source.as_bytes())
                    .unwrap()
                    .contains("(int, String)")
            );
        }
        assert!(
            all.iter()
                .all(|node| source.get(node.byte_range()).is_some())
        );
    }
}
