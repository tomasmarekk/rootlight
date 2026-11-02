//! Exercise the retained native VB.NET grammar on physical original source.
//! The optional manual census reports syntax gaps without claiming language coverage.

use std::{
    fs,
    path::{Path, PathBuf},
};
use tree_sitter::{Node, Parser, Tree};

const ORIGINAL: &[u8] = include_bytes!("fixtures/taskbarx/TaskbarAnimate.vb");

fn parse(source: &[u8]) -> Tree {
    let language: tree_sitter::Language = tree_sitter_vbnet::LANGUAGE_VBNET.into();
    assert_eq!(language.abi_version(), 14);
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    parser.parse(source, None).unwrap()
}

fn named<'tree>(node: Node<'tree>, name: &str) -> Node<'tree> {
    node.child_by_field_name(name).unwrap()
}

#[test]
fn original_physical_eof_preserves_class_method_parameter_ownership() {
    assert_eq!(ORIGINAL.len(), 4124);
    assert!(ORIGINAL.starts_with(b"\xef\xbb\xbf"));
    assert_eq!(
        ORIGINAL.windows(2).filter(|pair| *pair == b"\r\n").count(),
        106
    );
    assert!(!ORIGINAL.ends_with(b"\n"));
    assert!(ORIGINAL.ends_with(b"End Class"));
    let tree = parse(ORIGINAL);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(root.end_byte(), ORIGINAL.len());
    let mut cursor = root.walk();
    let classes: Vec<_> = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "class_declaration")
        .collect();
    assert_eq!(classes.len(), 1);
    let class = classes[0];
    assert_eq!(class.byte_range(), 23..4124);
    assert_eq!(named(class, "name").byte_range(), 36..50);
    assert_eq!(
        &ORIGINAL[named(class, "name").byte_range()],
        b"TaskbarAnimate"
    );
    let mut cursor = class.walk();
    let body: Vec<_> = class.children_by_field_name("body", &mut cursor).collect();
    assert_eq!(
        body.iter().map(|node| node.kind()).collect::<Vec<_>>(),
        ["field_declaration", "method_declaration"]
    );
    let method = body[1];
    assert_eq!(method.parent(), Some(class));
    assert_eq!(method.byte_range(), 104..4115);
    assert_eq!(named(method, "name").byte_range(), 122..129);
    assert_eq!(&ORIGINAL[named(method, "name").byte_range()], b"Animate");
    let parameters = named(method, "parameters");
    assert_eq!(parameters.byte_range(), 129..342);
    let parameter = parameters.named_child(0).unwrap();
    assert_eq!(parameter.kind(), "parameter");
    assert_eq!(parameter.parent(), Some(parameters));
    assert_eq!(parameter.byte_range(), 130..150);
    assert_eq!(named(parameter, "name").byte_range(), 136..140);
    assert_eq!(&ORIGINAL[named(parameter, "name").byte_range()], b"hwnd");
    let mut cursor = parameter.walk();
    let declared = parameter
        .named_children(&mut cursor)
        .find(|node| node.kind() == "as_clause")
        .unwrap();
    assert_eq!(named(declared, "declared_type").byte_range(), 144..150);
    assert_eq!(
        &ORIGINAL[named(declared, "declared_type").byte_range()],
        b"IntPtr"
    );
}

fn vb_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink(), "census does not follow symlinks");
        if kind.is_dir() && entry.file_name() != ".git" {
            vb_files(&entry.path(), files);
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vb"))
        {
            files.push(entry.path());
        }
    }
}

fn report_syntax_gaps(path: &Path, tree: &Tree, source: &[u8]) {
    let mut cursor = tree.walk();
    let mut first = None;
    let mut deepest = Vec::new();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            first.get_or_insert(node);
            let mut child_cursor = node.walk();
            if !node
                .children(&mut child_cursor)
                .any(|child| child.has_error())
            {
                deepest.push(node);
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                let mut reported = Vec::new();
                for node in first.into_iter().chain(deepest).take(4) {
                    let key = (
                        node.kind(),
                        node.start_byte(),
                        node.end_byte(),
                        node.is_missing(),
                    );
                    if reported.contains(&key) {
                        continue;
                    }
                    reported.push(key);
                    let end = node.start_byte().saturating_add(120).min(source.len());
                    let snippet = String::from_utf8_lossy(&source[node.start_byte()..end]);
                    println!(
                        "VB_CENSUS_ERROR path={} kind={:?} start={} end={} missing={} snippet={snippet:?}",
                        path.display(),
                        node.kind(),
                        node.start_byte(),
                        node.end_byte(),
                        node.is_missing()
                    );
                }
                return;
            }
        }
    }
}

#[test]
#[ignore = "manual parser census requires ROOTLIGHT_VBNET_CORPUS"]
fn configured_original_repository_parser_census() {
    let root = PathBuf::from(
        std::env::var_os("ROOTLIGHT_VBNET_CORPUS").expect("original corpus root is configured"),
    );
    let mut files = Vec::new();
    vb_files(&root, &mut files);
    files.sort();
    assert_eq!(files.len(), 19);
    let mut total = 0;
    let mut clean = 0;
    for file in files {
        let bytes = fs::read(&file).unwrap();
        total += bytes.len();
        let tree = parse(&bytes);
        let node = tree.root_node();
        let complete = !node.has_error() && node.end_byte() == bytes.len();
        clean += usize::from(complete);
        println!(
            "VB_CENSUS path={} bytes={} clean={} parsed_end={}",
            file.strip_prefix(&root).unwrap().display(),
            bytes.len(),
            complete,
            node.end_byte()
        );
        if !complete {
            report_syntax_gaps(file.strip_prefix(&root).unwrap(), &tree, &bytes);
        }
    }
    assert_eq!(total, 277939);
    println!(
        "VB_CENSUS_SUMMARY files=19 bytes={total} clean={clean} syntax_gaps={}",
        19 - clean
    );
}
