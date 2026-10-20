//! Real runtime witnesses for the retained VHDL grammar and edited scanner state.
//! Callback-only masks have separate native controls; these tests exercise trees
//! produced by the project's actual pinned Tree-sitter runtime.

fn parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&super::LANGUAGE.into()).unwrap();
    parser
}

fn nodes(tree: &tree_sitter::Tree) -> Vec<(String, usize, usize, bool)> {
    let mut cursor = tree.walk();
    let mut result = Vec::new();
    loop {
        let node = cursor.node();
        result.push((node.kind().to_owned(), node.start_byte(), node.end_byte(), node.is_missing()));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return result;
            }
        }
    }
}

fn point(source: &str, offset: usize) -> tree_sitter::Point {
    let prefix = &source[..offset];
    tree_sitter::Point {
        row: prefix.bytes().filter(|byte| *byte == b'\n').count(),
        column: prefix.rsplit_once('\n').map_or(prefix.len(), |(_, suffix)| suffix.len()),
    }
}

#[test]
fn loads_entity_and_grouped_port_syntax() {
    let source = "entity gate is\n  port (clk, reset : in bit; data : out bit);\nend entity;\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error(), "{}", tree.root_node().to_sexp());
    assert_eq!(tree.root_node().utf8_text(source.as_bytes()).unwrap(), source);
    assert_eq!(nodes(&tree).iter().filter(|row| row.0 == "entity_declaration").count(), 1);
}

#[test]
fn edited_numeric_comment_and_directive_states_match_fresh_trees() {
    let cases = [
        ("entity gate is generic (size : integer := 2147483648); end entity;\n", "2147483648", "999999999999999999999999999999999999999999999"),
        ("entity gate is generic (size : real := 1.2147483648e2147483648); end entity;\n", "2147483648e2147483648", "999999999999999999999999999999e99999999999999999"),
        ("-- first comment\nentity gate is end entity;\n", "first comment", "different -- comment"),
        ("/* first comment */\nentity gate is end entity;\n", "first comment", "different /* inner text"),
        ("`warning \"first\"\nentity gate is end entity;\n", "first", "replacement"),
    ];
    for (original, needle, replacement) in cases {
        let mut parser = parser();
        let mut old = parser.parse(original, None).unwrap();
        assert!(!old.root_node().has_error(), "{}", old.root_node().to_sexp());
        let start = original.find(needle).unwrap();
        let end = start + needle.len();
        let changed = format!("{}{}{}", &original[..start], replacement, &original[end..]);
        old.edit(&tree_sitter::InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: start + replacement.len(),
            start_position: point(original, start),
            old_end_position: point(original, end),
            new_end_position: point(&changed, start + replacement.len()),
        });
        let edited = parser.parse(&changed, Some(&old)).unwrap();
        let fresh = self::parser().parse(&changed, None).unwrap();
        assert!(!fresh.root_node().has_error(), "{}", fresh.root_node().to_sexp());
        assert_eq!(edited.root_node().to_sexp(), fresh.root_node().to_sexp());
        assert_eq!(nodes(&edited), nodes(&fresh));
    }
}
