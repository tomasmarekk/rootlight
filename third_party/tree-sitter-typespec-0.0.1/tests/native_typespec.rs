//! Exercise native TypeSpec grammar loading and written declaration structure.
//! These tests do not qualify production routing, IR or imported type resolution.

use tree_sitter::{Node, Parser, Tree};

fn parse(source: &[u8]) -> Tree {
    let language = tree_sitter_typespec::LANGUAGE.into();
    assert_eq!(tree_sitter::Language::abi_version(&language), 15);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained ABI15 loads");
    parser.parse(source, None).expect("native parse completes")
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut matching = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            matching.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    matching
}

fn clean(source: &[u8], tree: &Tree) {
    let root = tree.root_node();
    assert_eq!(root.kind(), "source_file");
    assert_eq!(root.byte_range(), 0..source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
}

fn name<'source>(node: Node<'_>, source: &'source [u8]) -> &'source str {
    node.child_by_field_name("name")
        .expect("written declaration has a name")
        .utf8_text(source)
        .expect("authored source is UTF-8")
}

#[test]
fn loads_actual_decorated_dotted_namespace_and_written_members() {
    let source = include_bytes!("fixtures/namespace.tsp");
    let tree = parse(source);
    clean(source, &tree);
    let root = tree.root_node();
    let mut namespaces: Vec<_> = nodes(root, "namespace_statement")
        .into_iter()
        .map(|node| name(node, source))
        .collect();
    namespaces.sort_unstable();
    assert_eq!(
        namespaces,
        ["TspTest.NamespaceClient", "TspTest.NamespaceModel"]
    );
    assert_eq!(nodes(root, "namespace_body").len(), 2);
    assert_eq!(nodes(root, "import_statement").len(), 3);
    assert_eq!(nodes(root, "using_statement").len(), 4);
    assert_eq!(nodes(root, "object_value").len(), 1);
    let operation = nodes(root, "operation_statement")[0];
    assert_eq!(name(operation, source), "get");
    assert_eq!(nodes(operation, "operation_signature_declaration").len(), 1);
    let model = nodes(root, "model_statement")[0];
    assert_eq!(name(model, source), "Model");
    let property = nodes(model, "model_property")[0];
    assert_eq!(name(property, source), "name");
    assert_eq!(
        property
            .child_by_field_name("type")
            .unwrap()
            .utf8_text(source)
            .unwrap(),
        "string"
    );
}

#[test]
fn distinguishes_file_namespace_properties_and_operation_signatures() {
    let source = br#"namespace Sample.Models;
@doc("pet") model Pet { @doc("name") name?: string; }
interface Base { get(id: string): Pet; }
interface Pets extends Base { op find(id: string): Pet; op copy is Base.get; }
op read(id: string): Pet;
op copied is read;
"#;
    let tree = parse(source);
    clean(source, &tree);
    let root = tree.root_node();
    let namespace = nodes(root, "namespace_statement")[0];
    assert_eq!(name(namespace, source), "Sample.Models");
    assert!(nodes(namespace, "namespace_body").is_empty());
    assert_eq!(nodes(root, "interface_statement").len(), 2);
    assert_eq!(nodes(root, "interface_heritage").len(), 1);
    assert_eq!(nodes(root, "operation_signature_declaration").len(), 3);
    assert_eq!(nodes(root, "operation_signature_reference").len(), 2);
    let model = nodes(root, "model_statement")[0];
    let property = nodes(model, "model_property")[0];
    assert_eq!(name(property, source), "name");
    let mut cursor = property.walk();
    assert!(
        property
            .children(&mut cursor)
            .any(|node| node.kind() == "?")
    );
    for operation in nodes(root, "operation_statement") {
        let expected = match name(operation, source) {
            "read" => "operation_signature_declaration",
            "copied" => "operation_signature_reference",
            other => panic!("unexpected operation: {other}"),
        };
        assert_eq!(nodes(operation, expected).len(), 1);
    }
}

#[test]
fn malformed_written_declarations_retain_native_error_state() {
    for source in [
        b"model Missing { name: string;".as_slice(),
        b"op missing(): ;".as_slice(),
        b"namespace { model Pet {} }".as_slice(),
    ] {
        let tree = parse(source);
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
#[ignore = "requires a pinned source list; reports native syntax, not semantic qualification"]
fn census_pinned_original_sources() {
    let list = std::env::var_os("ROOTLIGHT_TYPESPEC_SOURCE_LIST")
        .expect("pinned source-list path is required");
    let paths = std::fs::read_to_string(list).expect("source list is UTF-8");
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let mut files = 0;
    let mut incomplete = 0;
    let mut erroneous = 0;
    for path in paths.lines() {
        assert!(!path.is_empty());
        let source = std::fs::read(path).expect("pinned source is readable");
        let tree = parse(&source);
        let root = tree.root_node();
        let complete = root.kind() == "source_file" && root.byte_range() == (0..source.len());
        incomplete += usize::from(!complete);
        erroneous += usize::from(root.has_error());
        let mut errors = 0;
        let mut missing = 0;
        let mut nodes = vec![root];
        while let Some(node) = nodes.pop() {
            errors += usize::from(node.is_error());
            missing += usize::from(node.is_missing());
            *kinds.entry(node.kind().to_owned()).or_default() += 1;
            let mut cursor = node.walk();
            nodes.extend(node.children(&mut cursor));
        }
        let path_hex: String = path
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        println!(
            "typespec-syntax\t{}\t{}\t{}\t{}\t{}\t{}",
            source.len(),
            errors,
            missing,
            root.has_error(),
            complete,
            path_hex
        );
        files += 1;
    }
    assert!(files > 0);
    for (kind, count) in kinds {
        println!("typespec-kind\t{kind}\t{count}");
    }
    println!("typespec-files\t{files}");
    assert_eq!(
        (incomplete, erroneous),
        (0, 0),
        "native syntax gaps remain in the pinned corpus"
    );
}
