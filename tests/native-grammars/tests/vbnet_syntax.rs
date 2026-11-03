//! Native VB.NET controls preserve an original UTF-8 BOM/CRLF source.
//! The tested declaration structure does not qualify body binding semantics.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/vbnet/TaskbarAnimate.vb");
const MAIN_WINDOW: &[u8] = include_bytes!("../fixtures/vbnet/MainWindow.xaml.vb");

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
fn standalone_end_keeps_compound_terminators_and_written_statement_spans() {
    let source = b"Public Class Owner\nPublic Sub Run(ByVal shouldQuit As Boolean)\nIf shouldQuit Then\nEnd\nEnd If\neNd\nDim text As String = \"End\"\n' End\nEnd Sub\nEnd Class\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "class_block").len(), 1);
    assert_eq!(nodes(root, "method_declaration").len(), 1);
    let statements = nodes(root, "end_statement");
    assert_eq!(statements.len(), 2);
    for (statement, expected) in statements.iter().zip([b"End\n".as_slice(), b"eNd\n"]) {
        assert_eq!(&source[statement.byte_range()], expected);
    }
}

#[test]
fn standalone_end_does_not_consume_empty_clause_or_owner_terminators() {
    for body in [
        "Try\nCatch\nFinally\nEnd Try\n",
        "Try\nEnd\nCatch\nEnd\nFinally\nEnd\nEnd Try\n",
        "If True Then\nElseIf False Then\nElse\nEnd If\n",
        "Select Case 1\nCase 1\nCase Else\nEnd Select\n",
    ] {
        let source = format!("Class Owner\nSub Run()\n{body}End Sub\nEnd Class\n");
        let tree = parse(source.as_bytes());
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(nodes(tree.root_node(), "method_declaration").len(), 1);
    }
    for source in [
        "Class Owner\nSub Run()\nEnd\nEnd Class\n",
        "Class Owner\nSub Run()\nEnd 1\nEnd Sub\nEnd Class\n",
    ] {
        assert!(parse(source.as_bytes()).root_node().has_error(), "{source}");
    }
}

#[test]
fn complete_original_main_window_keeps_nested_structure_and_end_statements() {
    assert_eq!(MAIN_WINDOW.len(), 93_650);
    let tree = parse(MAIN_WINDOW);
    let root = tree.root_node();
    assert!(
        !root.has_error(),
        "native recovery spans: {:?}",
        nodes(root, "ERROR")
            .iter()
            .map(Node::byte_range)
            .collect::<Vec<_>>()
    );
    assert_eq!(nodes(root, "method_declaration").len(), 43);
    assert_eq!(nodes(root, "external_declaration").len(), 5);
    let ends = nodes(root, "end_statement");
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0].start_byte(), 73_925);
    assert_eq!(ends[1].start_byte(), 74_403);
    let policy = nodes(root, "structure_block")
        .into_iter()
        .find(|node| {
            node.child_by_field_name("name")
                .is_some_and(|name| &MAIN_WINDOW[name.byte_range()] == b"AccentPolicy")
        })
        .expect("authored attributed structure");
    assert_eq!(policy.parent().unwrap().kind(), "class_block");
    assert!(policy.child_by_field_name("attributes").is_some());
}

#[test]
fn attributed_structures_require_complete_type_member_placement() {
    let globals = parse(b"<Assembly: Marker>\n<Module: Marker>\nClass Owner\nEnd Class\n");
    assert!(!globals.root_node().has_error());
    let valid = b"Class Owner\n<StructLayout(LayoutKind.Sequential)>\nFriend Structure Policy\nPublic Value As Integer\nEnd Structure\nEnd Class\n";
    let tree = parse(valid);
    assert!(!tree.root_node().has_error());
    let structures = nodes(tree.root_node(), "structure_block");
    assert_eq!(structures.len(), 1);
    assert_eq!(structures[0].parent().unwrap().kind(), "class_block");
    for source in [
        "Class Owner\nSub Run()\n<A>\nStructure Invalid\nEnd Structure\nEnd Sub\nEnd Class\n",
        "Class Owner\n<A>\nEnd Class\n",
        "Class Owner\n<A>\nStructure Incomplete\nEnd Class\n",
        "Class Owner\n<Assembly: Marker>\nStructure Invalid\nEnd Structure\nEnd Class\n",
        "Class Owner\n<Module: Marker>\nStructure Invalid\nEnd Structure\nEnd Class\n",
    ] {
        assert!(parse(source.as_bytes()).root_node().has_error(), "{source}");
    }
}

#[test]
fn vbnet_attributes_keep_nested_class_spans_and_written_owners() {
    let source = b"Public Class Owner\r\n<StructLayout(LayoutKind.Sequential)>\r\nPublic Class Child\r\nPublic Value As Integer\r\nEnd Class\r\nEnd Class\r\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_block");
    assert_eq!(classes.len(), 2);
    let owner = classes[0];
    let child = classes[1];
    assert_eq!(child.parent(), Some(owner));
    let name = child.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 72..77);
    assert_eq!(&source[name.byte_range()], b"Child");
    let attributes = child.child_by_field_name("attributes").unwrap();
    assert_eq!(attributes.start_byte(), 20);
    assert_eq!(attributes.parent(), Some(child));
    assert_eq!(nodes(child, "field_declaration").len(), 1);
    assert!(classes.iter().all(|node| {
        node.child_by_field_name("name")
            .is_some_and(|name| &source[name.byte_range()] != b"StructLayout")
    }));
}

#[test]
fn vbnet_attributes_keep_function_return_type_and_parameter_spans() {
    let source = b"Class Owner\nPublic Shared Function Check(ByVal handle As Integer) As <MarshalAs(UnmanagedType.Bool)> Boolean\nReturn True\nEnd Function\nEnd Class\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let methods = nodes(root, "method_declaration");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    assert_eq!(method.parent().unwrap().kind(), "class_block");
    let name = method.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 35..40);
    assert_eq!(&source[name.byte_range()], b"Check");
    let result = method.child_by_field_name("return_type").unwrap();
    assert_eq!(result.byte_range(), 101..108);
    assert_eq!(&source[result.byte_range()], b"Boolean");
    let attributes = method.child_by_field_name("return_attributes").unwrap();
    assert_eq!(attributes.start_byte(), 69);
    assert_eq!(attributes.parent(), Some(method));
    let parameters = nodes(method, "parameter");
    assert_eq!(parameters.len(), 1);
    let parameter = parameters[0].child_by_field_name("name").unwrap();
    assert_eq!(parameter.byte_range(), 47..53);
    assert_eq!(&source[parameter.byte_range()], b"handle");
}

#[test]
fn vbnet_attributes_reject_nested_class_global_targets_and_illegal_placement() {
    for source in [
        "Class Owner\nSub Run()\n<A>\nClass Invalid\nEnd Class\nEnd Sub\nEnd Class\n",
        "Class Owner\n<A>\nClass Incomplete\nEnd Class\n",
        "Class Owner\n<Assembly: Marker>\nClass Invalid\nEnd Class\nEnd Class\n",
        "Class Owner\n<Module: Marker>\nClass Invalid\nEnd Class\nEnd Class\n",
        "Class Owner\n<Type: Marker>\nClass Invalid\nEnd Class\nEnd Class\n",
        "Class Owner\nPublic <Marker> Class Invalid\nEnd Class\nEnd Class\n",
    ] {
        assert!(parse(source.as_bytes()).root_node().has_error(), "{source}");
    }
    let globals = b"<Assembly: Marker>\n<Module: Marker>\nClass Owner\nEnd Class\n";
    assert!(!parse(globals).root_node().has_error());
}

#[test]
fn vbnet_attributes_reject_return_targets_and_nonreturn_type_positions() {
    for source in [
        "Class Owner\nFunction Read() As <Assembly: Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As <Module: Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As <Return: Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As <Type: Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As <Method: Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() <Marker> As Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As Boolean <Marker>\nEnd Function\nEnd Class\n",
        "Class Owner\nSub Run() As <Marker> Boolean\nEnd Sub\nEnd Class\n",
        "Class Owner\nDelegate Sub Run() As <Marker> Boolean\nEnd Class\n",
        "Class Owner\nPublic Value As <Marker> Boolean\nEnd Class\n",
        "Class Owner\nFunction Read(value As <Marker> Integer) As Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As <Marker>\nEnd Function\nEnd Class\n",
        "Class Owner\nFunction Read() As\n<Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nProperty Value As\n<Marker> Integer\nEnd Class\n",
    ] {
        assert!(parse(source.as_bytes()).root_node().has_error(), "{source}");
    }
}

#[test]
fn vbnet_attributes_preserve_supported_signature_sites_and_attribute_newlines() {
    let source = b"Class Owner\nDelegate Function Callback() As <Marker> Boolean\nDeclare Function Native Lib \"user32.dll\" () As <Marker> Boolean\nPublic Property Value As <Marker> Integer\nPublic Function Continued() As _\n<Marker> Boolean\nEnd Function\n<\nMarker\n>\nPublic Class Child\nEnd Class\nEnd Class\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    for kind in [
        "delegate_declaration",
        "external_declaration",
        "method_declaration",
    ] {
        let declarations = nodes(root, kind);
        assert_eq!(declarations.len(), 1, "{kind}");
        assert!(
            declarations[0]
                .child_by_field_name("return_attributes")
                .is_some(),
            "{kind}"
        );
        let result = declarations[0].child_by_field_name("return_type").unwrap();
        assert_eq!(&source[result.byte_range()], b"Boolean");
    }
    let properties = nodes(root, "property_declaration");
    assert_eq!(properties.len(), 1);
    let clauses = nodes(properties[0], "as_clause");
    assert_eq!(clauses.len(), 1);
    assert!(
        clauses[0]
            .child_by_field_name("return_attributes")
            .is_some()
    );
    let result = clauses[0].child_by_field_name("type").unwrap();
    assert_eq!(&source[result.byte_range()], b"Integer");
    let classes = nodes(root, "class_block");
    assert_eq!(classes.len(), 2);
    assert_eq!(classes[1].parent(), Some(classes[0]));
    assert!(classes[1].child_by_field_name("attributes").is_some());
}

#[test]
fn vbnet_attributes_reject_colon_after_attribute_blocks() {
    for source in [
        "Class Owner\nFunction Read() As <Marker>: Boolean\nEnd Function\nEnd Class\n",
        "Class Owner\nProperty Value As <Marker>: Integer\nEnd Class\n",
        "Class Owner\n<Marker>: Class Child\nEnd Class\nEnd Class\n",
        "Class Owner\n<Marker>: Structure Child\nEnd Structure\nEnd Class\n",
        "<Marker>: Class Owner\nEnd Class\n",
    ] {
        assert!(parse(source.as_bytes()).root_node().has_error(), "{source}");
    }
    let statements = b"Imports System:Class Owner:Sub Run():Return:End Sub:End Class\n";
    assert!(!parse(statements).root_node().has_error());
}

#[test]
fn vbnet_attributes_keep_global_colon_statements_separate_from_classes() {
    for target in ["Assembly", "Module"] {
        let source = format!("<{target}: Marker>: Class Owner\nEnd Class\n");
        let tree = parse(source.as_bytes());
        let root = tree.root_node();
        assert!(!root.has_error(), "{source}: {}", root.to_sexp());
        let classes = nodes(root, "class_block");
        assert_eq!(classes.len(), 1);
        assert!(classes[0].child_by_field_name("attributes").is_none());
        let attributes = nodes(root, "attribute_block");
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].parent(), Some(root));
        let entries = nodes(attributes[0], "attribute");
        assert_eq!(entries.len(), 1);
        let actual = entries[0].child_by_field_name("target").unwrap();
        assert_eq!(&source.as_bytes()[actual.byte_range()], target.as_bytes());
        let eof = format!("<{target}: Marker>");
        let eof_tree = parse(eof.as_bytes());
        assert!(
            !eof_tree.root_node().has_error(),
            "{eof}: {}",
            eof_tree.root_node().to_sexp()
        );
        let joined = format!("<{target}: Marker> Class Owner\nEnd Class\n");
        assert!(parse(joined.as_bytes()).root_node().has_error(), "{joined}");
    }
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
        "TaskbarX/TaskbarX/Easings.vb",
        "TaskbarX/TaskbarX/Main.vb",
        "TaskbarX/TaskbarX/MSAA.vb",
        "TaskbarX/TaskbarX/My Project/Application.Designer.vb",
        "TaskbarX/TaskbarX/My Project/AssemblyInfo.vb",
        "TaskbarX/TaskbarX/My Project/Resources.Designer.vb",
        "TaskbarX/TaskbarX/My Project/Settings.Designer.vb",
        "TaskbarX/TaskbarX/Settings.vb",
        "TaskbarX/TaskbarX/TaskbarAnimate.vb",
        "TaskbarX/TaskbarX/TaskbarCenter.vb",
        "TaskbarX/TaskbarX/TaskbarStyle.vb",
        "TaskbarX/TaskbarX/TrayIconBuster.vb",
        "TaskbarX/TaskbarX/Win32.vb",
        "TaskbarX Configurator/TaskbarX Configurator/Application.xaml.vb",
        "TaskbarX Configurator/TaskbarX Configurator/Easings.vb",
        "TaskbarX Configurator/TaskbarX Configurator/MainWindow.xaml.vb",
        "TaskbarX Configurator/TaskbarX Configurator/My Project/AssemblyInfo.vb",
        "TaskbarX Configurator/TaskbarX Configurator/My Project/Resources.Designer.vb",
        "TaskbarX Configurator/TaskbarX Configurator/My Project/Settings.Designer.vb",
    ] {
        let source = std::fs::read(directory.join(path)).expect("original source bytes");
        let tree = parse(&source);
        let root = tree.root_node();
        let errors = nodes(root, "ERROR");
        let mut missing = Vec::new();
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if node.is_missing() {
                missing.push(node);
            }
            let mut cursor = node.walk();
            pending.extend(node.children(&mut cursor));
        }
        missing.sort_by_key(Node::start_byte);
        eprintln!(
            "ORIGINAL {path}: bytes={} complete={} external={} methods={} errors={} missing={}",
            source.len(),
            !root.has_error(),
            nodes(root, "external_declaration").len(),
            nodes(root, "method_declaration").len(),
            errors.len(),
            missing.len()
        );
        for error in errors.iter().chain(&missing) {
            let end = error.end_byte().min(error.start_byte().saturating_add(120));
            eprintln!(
                "RECOVERY {} {}..{} line={} text={:?}",
                error.kind(),
                error.start_byte(),
                error.end_byte(),
                error.start_position().row + 1,
                String::from_utf8_lossy(&source[error.start_byte()..end])
            );
        }
        assert_eq!(
            std::fs::read(directory.join(path)).unwrap(),
            source,
            "original bytes changed: {path}"
        );
    }
}
