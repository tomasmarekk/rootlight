//! Pins the audited Swift native parser's string and declaration contracts.
//! Structural extraction tests separately prove the public Rootlight fact boundary.

use tree_sitter::Parser;

#[test]
fn swift_raw_interpolation_preserves_unsigned_scanner_bytes() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("audited ABI loads");
    for hashes in [1, 2, 127, 128, 129, 255, 256] {
        let delimiter = "#".repeat(hashes);
        let source = format!(
            "let name = 1\nlet value = {delimiter}\"hello \\{delimiter}(name)\"{delimiter}\n"
        );
        let tree = parser.parse(&source, None).expect("bounded input parses");
        assert!(
            !tree.root_node().has_error(),
            "raw delimiter count {hashes}"
        );
    }
}

#[test]
fn swift_declaration_discriminators_are_source_backed() {
    let source = r#"import Foundation
protocol Store { func load(_ key: String) -> String }
struct Entry { var value: String; func render() -> String { return value } }
class Cache { let size = 1 }
actor Worker { func run() {} }
enum Result { case ready, missing }
extension Entry { func copy() -> Entry { return self } }
typealias Label = String
func greet(_ name: String) -> String { return name }
let title = "hello"
"#;
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("audited ABI loads");
    let tree = parser.parse(source, None).expect("bounded input parses");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut cursor = tree.root_node().walk();
    let kinds: Vec<_> = tree
        .root_node()
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "class_declaration")
        .map(|node| {
            node.child_by_field_name("declaration_kind")
                .expect("kind field")
                .utf8_text(source.as_bytes())
                .expect("source slice")
        })
        .collect();
    assert_eq!(kinds, ["struct", "class", "actor", "enum", "extension"]);
}
