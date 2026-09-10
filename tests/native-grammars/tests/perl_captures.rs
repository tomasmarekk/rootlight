//! Source-exact query contracts for the Perl adapter's declaration capture pack.
//! Native captures alone do not establish package ownership or resolved references.

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

const QUERY: &str = include_str!("../../../crates/rootlight-adapter-treesitter/queries/perl.scm");

fn definitions(source: &str) -> Vec<String> {
    let language = ts_parser_perl::LANGUAGE.into();
    let query = Query::new(&language, QUERY).unwrap();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut captures = Vec::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            let index = usize::try_from(capture.index).unwrap();
            if query.capture_names()[index] == "definition" {
                let node = capture.node;
                let text = source.get(node.byte_range()).unwrap();
                assert_eq!(node.utf8_text(source.as_bytes()).unwrap(), text);
                captures.push((node.start_byte(), node.end_byte(), text.to_owned()));
            }
        }
    }
    captures.sort();
    assert!(captures.windows(2).all(|pair| pair[0] != pair[1]));
    captures.into_iter().map(|(_, _, name)| name).collect()
}

#[test]
fn named_parameters_do_not_capture_default_expression_variables() {
    let source = "sub adjust ($value, $step = $fallback, @rest) { return $value + $step; }\n";
    assert_eq!(definitions(source), ["adjust", "$value", "$step", "@rest"]);
}

#[test]
fn anonymous_signature_slots_have_no_symbol_identity() {
    assert_eq!(definitions("sub consume ($, @) { 1 }\n"), ["consume"]);
    assert_eq!(definitions("sub consume ($ = 1, %) { 1 }\n"), ["consume"]);
}

#[test]
fn namespace_and_nested_callable_names_keep_written_spelling() {
    let source = "package Outer; sub first { 1 } package Inner { sub second { my sub helper { 2 } helper() } } sub last { 3 }\n";
    assert_eq!(
        definitions(source),
        ["Outer", "first", "Inner", "second", "helper", "last"]
    );
}

#[test]
fn classes_roles_methods_and_forward_declarations_are_distinct_names() {
    let source = "class Counter { method value ($offset = 1) { $offset } } role Named { method name; } sub forward;\n";
    assert_eq!(
        definitions(source),
        ["Counter", "value", "$offset", "Named", "name", "forward"]
    );
}

#[test]
fn opaque_text_cannot_create_declaration_captures() {
    for newline in ["\n", "\r\n"] {
        let source = "# λ😀 sub hidden {}\nmy $text = q{sub fake ($arg) {}};\n=pod\npackage Phantom;\n=cut\nmy $body = <<'END';\nsub phantom ($arg) {}\nEND\nsub actual ($value) { $value }\n".replace('\n', newline);
        assert_eq!(definitions(&source), ["actual", "$value"]);
    }
}
