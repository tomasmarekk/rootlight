//! Native VB.NET controls preserve an original UTF-8 BOM/CRLF source.
//! The tested declaration structure does not qualify body binding semantics.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/vbnet/TaskbarAnimate.vb");

fn parse(source: &[u8]) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vbnet::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut result = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            result.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    result.sort_by_key(Node::start_byte);
    result
}

#[test]
fn original_bom_crlf_source_has_exact_native_class_method_and_parameters() {
    assert_eq!(ORIGINAL.len(), 4124);
    assert_eq!(&ORIGINAL[..3], b"\xef\xbb\xbf");
    let tree = parse(ORIGINAL);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_block");
    assert_eq!(classes.len(), 1);
    let class = classes[0];
    let name = class.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 36..50);
    assert_eq!(&ORIGINAL[name.byte_range()], b"TaskbarAnimate");
    let methods = nodes(class, "method_declaration");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    assert_eq!(method.parent(), Some(class));
    let name = method.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 122..129);
    assert_eq!(&ORIGINAL[name.byte_range()], b"Animate");
    let parameters = nodes(method, "parameter");
    assert_eq!(parameters.len(), 8);
    let name = parameters[0].child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 136..140);
    assert_eq!(&ORIGINAL[name.byte_range()], b"hwnd");
}

#[test]
fn quoted_and_commented_names_do_not_create_native_declarations() {
    let source = b"Public Class Owner\nPublic Sub Run(ByVal Value As Integer)\nDim text As String = \"Class Fake\"\n' Public Sub Hidden()\nEnd Sub\nEnd Class\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "class_block").len(), 1);
    assert_eq!(nodes(root, "method_declaration").len(), 1);
    assert_eq!(nodes(root, "parameter").len(), 1);
}

#[test]
fn mixed_case_keywords_parse_and_incomplete_owner_keeps_recovery() {
    let tree =
        parse(b"pUbLiC cLaSs Owner\npUbLiC sUb Run(ByVal Value As Integer)\nEnD sUb\nEnD cLaSs\n");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let broken = parse(b"Public Class Owner\nPublic Sub Run(ByVal Value As Integer)\n");
    assert!(
        broken.root_node().has_error(),
        "{}",
        broken.root_node().to_sexp()
    );
}

#[test]
fn external_declarations_keep_native_names_literals_and_formals() {
    for declaration in [
        "Private Declare Function SetFocus Lib \"user32.dll\" (ByVal hwnd As IntPtr) As IntPtr",
        "Private Declare Auto Function FindWindow Lib \"user32.dll\" (ByVal lpClassName As String, ByVal lpWindowName As String) As IntPtr",
        "Public Declare Unicode Sub Notify Lib \"native.dll\" Alias \"NotifyW\" (ByRef value As Integer)",
        "Declare Ansi Function Tick Lib \"native.dll\" Alias \"#123\" As Integer",
    ] {
        let source = format!("Public Class Api\r\n{declaration}\r\nEnd Class\r\n");
        let tree = parse(source.as_bytes());
        let root = tree.root_node();
        assert!(!root.has_error(), "{declaration}: {}", root.to_sexp());
        let declarations = nodes(root, "external_declaration");
        assert_eq!(declarations.len(), 1);
        let declaration = declarations[0];
        assert_eq!(declaration.parent().unwrap().kind(), "class_block");
        let name = declaration.child_by_field_name("name").unwrap();
        assert_eq!(name.kind(), "identifier");
        let library = declaration.child_by_field_name("library").unwrap();
        assert_eq!(library.kind(), "string_literal");
        for formal in nodes(declaration, "parameter") {
            assert!(formal.child_by_field_name("name").is_some());
        }
    }
}

#[test]
fn complete_original_external_declaration_files_parse_without_recovery() {
    for (source, classes) in [
        (include_bytes!("../fixtures/vbnet/MSAA.vb").as_slice(), 2),
        (
            include_bytes!("../fixtures/vbnet/TaskbarStyle.vb").as_slice(),
            1,
        ),
    ] {
        let tree = parse(source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        assert_eq!(nodes(root, "class_block").len(), classes);
        assert_eq!(nodes(root, "external_declaration").len(), 1);
    }
}

#[test]
fn malformed_or_misplaced_external_declarations_keep_recovery() {
    for source in [
        "Class Api\nDeclare Function Broken() As Integer\nEnd Class\n",
        "Class Api\nDeclare Sub Broken Lib library()\nEnd Class\n",
        "Class Api\nDeclare Sub Broken Lib \"native.dll\" Alias\nEnd Class\n",
        "Class Api\nDeclare Sub Broken Lib \"native.dll\"() As Integer\nEnd Class\n",
        "Interface Api\nDeclare Sub Broken Lib \"native.dll\"()\nEnd Interface\n",
        "Declare Sub Broken Lib \"native.dll\"()\n",
        "Class Api\nPublic Shared Declare Sub Broken Lib \"native.dll\"()\nEnd Class\n",
    ] {
        let tree = parse(source.as_bytes());
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
#[ignore = "manual diagnosis requires ROOTLIGHT_VBNET_ORIGINAL_ROOT; not corpus qualification"]
fn inspect_original_vbnet_parser_recovery() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_VBNET_ORIGINAL_ROOT").expect("original repository root"),
    );
    for path in [
        "TaskbarX/TaskbarX/Main.vb",
        "TaskbarX/TaskbarX/MSAA.vb",
        "TaskbarX/TaskbarX/TaskbarCenter.vb",
        "TaskbarX/TaskbarX/TaskbarStyle.vb",
        "TaskbarX/TaskbarX/TrayIconBuster.vb",
        "TaskbarX/TaskbarX/Win32.vb",
        "TaskbarX Configurator/TaskbarX Configurator/MainWindow.xaml.vb",
    ] {
        let source = std::fs::read(directory.join(path)).expect("original source bytes");
        let tree = parse(&source);
        let root = tree.root_node();
        let errors = nodes(root, "ERROR");
        eprintln!(
            "ORIGINAL {path}: bytes={} complete={} external={} methods={} errors={}",
            source.len(),
            !root.has_error(),
            nodes(root, "external_declaration").len(),
            nodes(root, "method_declaration").len(),
            errors.len()
        );
        for error in errors.iter().take(4) {
            let end = error.end_byte().min(error.start_byte().saturating_add(120));
            eprintln!(
                "RECOVERY {}..{} line={} text={:?}",
                error.start_byte(),
                error.end_byte(),
                error.start_position().row + 1,
                String::from_utf8_lossy(&source[error.start_byte()..end])
            );
        }
    }
}
