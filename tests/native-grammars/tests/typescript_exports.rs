//! Native export grammar boundaries shared by TypeScript and TSX.
//! Written module fields must survive trivia and incremental edits without
//! confusing reserved export keywords with expression identifiers.

use tree_sitter::{InputEdit, Language, Parser, Point};

fn languages() -> [Language; 2] {
    [
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        tree_sitter_typescript::LANGUAGE_TSX.into(),
    ]
}

#[test]
fn typed_exports_preserve_module_fields_across_trivia() {
    for language in languages() {
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        for separator in [
            " ",
            "\n",
            "\r\n",
            "\u{2028}",
            "\u{2029}",
            "\n\u{A0}",
            "\n\u{FEFF}",
            "/* newline\n*/",
        ] {
            for body in ["*", "* as Space", "* as 'public-name'", "{Item}"] {
                let source =
                    format!("export{separator}type{separator}{body}{separator}from './provider';");
                let tree = parser.parse(&source, None).unwrap();
                let root = tree.root_node();
                assert!(!root.has_error(), "{source}: {}", root.to_sexp());
                assert_eq!(root.named_child_count(), 1);
                let export = root.named_child(0).unwrap();
                assert_eq!(export.kind(), "export_statement");
                assert_eq!(export.byte_range(), 0..source.len());
                let module = export.child_by_field_name("source").unwrap();
                assert_eq!(&source[module.byte_range()], "'./provider'");
                let mut cursor = export.walk();
                assert!(
                    export
                        .children(&mut cursor)
                        .any(|child| !child.is_named() && child.kind() == "type")
                );
                if separator.starts_with("/*") {
                    let mut cursor = export.walk();
                    assert_eq!(
                        export
                            .children(&mut cursor)
                            .filter(|child| child.kind() == "comment")
                            .count(),
                        3
                    );
                }
            }
        }
    }
}

#[test]
fn export_keyword_remains_valid_as_a_property_or_public_module_name() {
    for language in languages() {
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        for source in [
            "const object = {export: 1}; object.export;",
            "class Item { export() {} }",
            "export {Item as export} from './provider';",
            "import {export as Item} from './provider';",
            "export\nconst Item = 1;",
            "export\ndefault function Item() {}",
            "export\ntype Item = string;",
            "export {Item}\nfromage();",
            "export {Item}\nfromα();",
            "const value = 1;\nfrom();",
        ] {
            let tree = parser.parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
        for source in ["export;", "export type *;", "export type * as Space;"] {
            let tree = parser.parse(source, None).unwrap();
            assert!(
                tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
}

#[test]
fn incremental_type_only_removal_matches_fresh_source_ranges() {
    for language in languages() {
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let source = "// café\r\nexport type * from './provider';";
        let start = source.find("type ").unwrap();
        let mut original = parser.parse(source, None).unwrap();
        assert!(!original.root_node().has_error());
        original.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 5,
            new_end_byte: start,
            start_position: Point::new(1, 7),
            old_end_position: Point::new(1, 12),
            new_end_position: Point::new(1, 7),
        });
        let changed = source.replacen("type ", "", 1);
        let incremental = parser.parse(&changed, Some(&original)).unwrap();
        let fresh = parser.parse(&changed, None).unwrap();
        assert!(!incremental.root_node().has_error());
        assert_eq!(
            incremental.root_node().to_sexp(),
            fresh.root_node().to_sexp()
        );
        let incremental_export = incremental.root_node().named_child(1).unwrap();
        let fresh_export = fresh.root_node().named_child(1).unwrap();
        assert_eq!(incremental_export.byte_range(), fresh_export.byte_range());
        let module = incremental_export.child_by_field_name("source").unwrap();
        assert_eq!(&changed[module.byte_range()], "'./provider'");
        let mut cursor = incremental_export.walk();
        assert!(
            !incremental_export
                .children(&mut cursor)
                .any(|child| child.kind() == "type")
        );
    }
}
