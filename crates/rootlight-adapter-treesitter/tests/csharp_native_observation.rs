//! Observe the exact C# extension method through the existing native grammar.
//! This records native fields and ownership without qualifying IR binding.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("fixtures/csharp_formal_observation/Extensions.cs");

fn describe(node: Node<'_>) -> String {
    let mut field = None;
    if let Some(parent) = node.parent() {
        for index in 0..parent.child_count() {
            let index = u32::try_from(index).unwrap();
            if parent.child(index) == Some(node) {
                field = parent.field_name_for_child(index);
                break;
            }
        }
    }
    format!(
        "kind={} field={:?} bytes={}..{} error={} missing={} has_error={}",
        node.kind(),
        field,
        node.start_byte(),
        node.end_byte(),
        node.is_error(),
        node.is_missing(),
        node.has_error()
    )
}

fn print_method_path<'tree>(root: Node<'tree>, start: usize, end: usize, tag: &str) -> Node<'tree> {
    let leaf = root.descendant_for_byte_range(start, end).unwrap();
    let mut current = Some(leaf);
    for _ in 0..32 {
        let node = current.expect("the written anchor has a native method owner");
        println!("{tag}-path {}", describe(node));
        if node.kind() == "method_declaration" {
            assert!(!node.has_error() && !node.is_missing());
            return node;
        }
        current = node.parent();
    }
    panic!("the anchor exceeded the explicit ancestry observation bound");
}

#[test]
fn original_csharp_extension_formal_native_fields_and_ancestry() {
    assert_eq!(ORIGINAL.len(), 919);
    assert_eq!(&ORIGINAL[458..468], b"Extensions");
    assert_eq!(&ORIGINAL[521..538], b"EnumerateOverAxis");
    assert_eq!(&ORIGINAL[561..566], b"array");
    assert_eq!(&ORIGINAL[712..717], b"array");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    let root = tree.root_node();
    println!(
        "source bytes={} expected_sha256=eb802d53043e35c7ddd9a8f2ae6c0ed2a5d25a9c06d48c0b2f3ce82c157aab37 root={}",
        ORIGINAL.len(),
        describe(root)
    );
    let method = print_method_path(root, 521, 538, "method");
    assert_eq!(print_method_path(root, 561, 566, "formal"), method);
    assert_eq!(print_method_path(root, 712, 717, "reference"), method);
    println!("method-sexp {}", method.to_sexp());
    if let Some(body) = method.child_by_field_name("body") {
        println!("body-sexp {}", body.to_sexp());
    }
    let mut cursor = method.walk();
    for index in 0..200 {
        println!("node {index} {}", describe(cursor.node()));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
    panic!("the selected method exceeded the explicit native observation bound");
}

mod written_fields {
    use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

    const ORIGINAL: &[u8] = include_bytes!("fixtures/csharp_fields/Original.cs");
    const FIELD_QUERY: &str = r#"
(field_declaration
  (variable_declaration
    (variable_declarator name: (identifier) @definition) @declaration))
"#;

    fn describe(node: Node<'_>) -> String {
        let field = node.parent().and_then(|parent| {
            (0..parent.child_count()).find_map(|index| {
                let index = u32::try_from(index).unwrap();
                (parent.child(index) == Some(node))
                    .then(|| parent.field_name_for_child(index))
                    .flatten()
            })
        });
        format!(
            "id={} parent={:?} kind={} field={field:?} bytes={}..{} error={} missing={}",
            node.id(),
            node.parent().map(|parent| parent.id()),
            node.kind(),
            node.start_byte(),
            node.end_byte(),
            node.has_error(),
            node.is_missing()
        )
    }

    fn observe(tag: &str, source: &[u8], expected_fields: &[&[u8]]) {
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        println!("probe={tag} bytes={} sexp={}", source.len(), root.to_sexp());
        assert!(
            !root.has_error(),
            "observation must expose a clean native input: {tag}"
        );
        let mut walker = root.walk();
        let mut visited = 0;
        loop {
            assert!(visited < 400, "bounded observation node count: {tag}");
            println!("probe={tag} node={visited} {}", describe(walker.node()));
            visited += 1;
            if walker.goto_first_child() {
                continue;
            }
            loop {
                if walker.goto_next_sibling() {
                    break;
                }
                if !walker.goto_parent() {
                    let query = Query::new(&language, FIELD_QUERY).unwrap();
                    let mut cursor = QueryCursor::new();
                    let mut matches = cursor.matches(&query, root, source);
                    let mut names = Vec::new();
                    while let Some(found) = matches.next() {
                        for capture in found.captures {
                            let label =
                                query.capture_names()[usize::try_from(capture.index).unwrap()];
                            println!("probe={tag} capture={label} {}", describe(capture.node));
                            if label == "definition" {
                                names.push(&source[capture.node.byte_range()]);
                            }
                        }
                    }
                    assert_eq!(names, expected_fields, "field-only native query: {tag}");
                    return;
                }
            }
        }
    }

    #[test]
    fn original_and_ordinary_field_declarators_have_native_ownership() {
        assert_eq!(ORIGINAL.len(), 80);
        assert_eq!(&ORIGINAL[44..45], b"S");
        assert_eq!(&ORIGINAL[68..69], b"x");
        println!(
            "original_sha256=b197e17bc5da64b65a742c60eae03a6ef2f083a91722c3e439c076a17cefa96b"
        );
        observe("original", ORIGINAL, &[b"x"]);
        observe(
        "multiple-declarators-and-nonfields",
        b"class Catalog { int left = 1, right; int Property { get; set; } void Run(int input) { int local = input; } }\n",
        &[b"left", b"right"],
    );
        observe(
            "nested-owners",
            b"class Outer { int top; struct Inner { public int nested; } }\n",
            &[b"top", b"nested"],
        );
    }
}
