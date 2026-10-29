//! ERB delimiter and combined Ruby syntax experiments on original source.
//! These tests never render templates and do not qualify the ERB product route.

use tree_sitter::{Node, Parser, Range, Tree};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/erb/layout_render_object.erb");
const ESCAPED: &[u8] = include_bytes!("../fixtures/erb/sessions_new.html.erb");
const COMMENT: &[u8] = include_bytes!("../fixtures/erb/comment_peek.html.erb");

fn host(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_embedded_template::LANGUAGE.into())
        .expect("retained ABI15 host grammar loads");
    parser.parse(source, None).expect("host parse completes")
}

fn nodes(tree: &Tree, kind: &str) -> Vec<Range> {
    let mut result = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.kind() == kind {
            result.push(node.range());
        }
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

fn ruby(source: &[u8], ranges: &[Range]) -> Tree {
    assert!(
        !ranges.is_empty(),
        "an empty range list would select the entire host source"
    );
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("retained Ruby grammar loads");
    parser
        .set_included_ranges(ranges)
        .expect("authored code ranges are ordered and disjoint");
    parser
        .parse(source, None)
        .expect("one combined Ruby parse completes")
}

fn at<'a>(node: Node<'a>, kind: &str, start: usize, end: usize) -> Option<Node<'a>> {
    if node.kind() == kind && node.start_byte() == start && node.end_byte() == end {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = at(child, kind, start, end) {
            return Some(found);
        }
    }
    None
}

#[test]
fn original_adjacent_tags_form_one_clean_ruby_block_with_exact_endpoints() {
    assert_eq!(ORIGINAL.len(), 84);
    assert!(
        !ORIGINAL.contains(&b'\n'),
        "the original witness has no physical range-boundary newline"
    );
    let template = host(ORIGINAL);
    assert!(
        !template.root_node().has_error(),
        "{}",
        template.root_node().to_sexp()
    );
    let ranges = nodes(&template, "code");
    assert_eq!(ranges.len(), 3);
    let combined = ruby(ORIGINAL, &ranges);
    assert!(
        !combined.root_node().has_error(),
        "{}",
        combined.root_node().to_sexp()
    );
    println!("{}", combined.root_node().to_sexp());
    let definition =
        at(combined.root_node(), "identifier", 43, 51).expect("authored customer block parameter");
    assert_eq!(definition.parent().unwrap().kind(), "block_parameters");
    let read = at(combined.root_node(), "identifier", 59, 67)
        .expect("authored customer use in the next tag");
    assert_eq!(read.parent().unwrap().kind(), "call");
    let blocks = nodes(&combined, "do_block");
    assert_eq!(blocks.len(), 1, "one cross-tag block owns both endpoints");
    assert!(
        blocks[0].start_byte <= definition.start_byte() && blocks[0].end_byte >= read.end_byte()
    );
    assert!(
        ruby(ORIGINAL, &ranges[..1]).root_node().has_error(),
        "opening island cannot supply its missing end"
    );
    let closing = ruby(ORIGINAL, &ranges[2..]);
    println!("closing-only syntax: {}", closing.root_node().to_sexp());
    assert!(
        nodes(&closing, "do_block").is_empty(),
        "a closing island cannot manufacture the opening block"
    );
    assert!(
        nodes(&closing, "block_parameters").is_empty(),
        "a closing island cannot supply block parameters"
    );
}

#[test]
fn original_escaped_future_template_has_no_ruby_code_ranges() {
    let template = host(ESCAPED);
    assert!(!template.root_node().has_error());
    assert!(
        nodes(&template, "code").is_empty(),
        "future form and block declarations remain literal content"
    );
}

#[test]
fn original_comment_and_literal_fake_declarations_do_not_become_ruby_code() {
    let template = host(COMMENT);
    assert!(!template.root_node().has_error());
    assert_eq!(nodes(&template, "comment").len(), 1);
    let ranges = nodes(&template, "code");
    assert_eq!(ranges.len(), 1);
    assert!(
        !COMMENT[ranges[0].start_byte..ranges[0].end_byte]
            .windows(8)
            .any(|part| part == b"Template")
    );
    assert!(!ruby(COMMENT, &ranges).root_node().has_error());
    let fake = host(b"<%# def ghost; end %><%%= def future; end %>");
    assert!(!fake.root_node().has_error());
    assert!(nodes(&fake, "code").is_empty());
}

#[test]
fn erb_trim_opener_remains_a_statement_across_one_ruby_block() {
    let source = b"<%- items.each do |item| -%><%= item %><%- end -%>";
    let template = host(source);
    assert!(!template.root_node().has_error());
    assert_eq!(
        nodes(&template, "directive").len(),
        2,
        "ERB left trim does not imply output"
    );
    assert_eq!(nodes(&template, "output_directive").len(), 1);
    let combined = ruby(source, &nodes(&template, "code"));
    assert!(
        !combined.root_node().has_error(),
        "{}",
        combined.root_node().to_sexp()
    );
    assert_eq!(nodes(&combined, "do_block").len(), 1);
}

#[test]
fn erb_preserves_ruby_underscore_tilde_and_ordinary_closer_source() {
    for (source, expected) in [
        (b"<%_value = 3%>".as_slice(), b"_value = 3".as_slice()),
        (b"<%~value%>".as_slice(), b"~value".as_slice()),
        (b"<% value_%>".as_slice(), b" value_".as_slice()),
    ] {
        let template = host(source);
        assert!(!template.root_node().has_error());
        assert_eq!(nodes(&template, "directive").len(), 1);
        let ranges = nodes(&template, "code");
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            &source[ranges[0].start_byte..ranges[0].end_byte],
            expected,
            "ERB cannot consume Ruby bytes as a foreign dialect delimiter"
        );
        assert!(!ruby(source, &ranges).root_node().has_error());
    }
}

#[test]
fn foreign_delimiters_do_not_manufacture_clean_ruby_or_output_tags() {
    for (source, expected) in [
        (b"<%|= value %>".as_slice(), b"|= value ".as_slice()),
        (b"<%|== value %>".as_slice(), b"|== value ".as_slice()),
        (b"<%= value =%>".as_slice(), b" value =".as_slice()),
    ] {
        let template = host(source);
        assert!(!template.root_node().has_error());
        let ranges = nodes(&template, "code");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&source[ranges[0].start_byte..ranges[0].end_byte], expected);
        if source.starts_with(b"<%|") {
            assert!(nodes(&template, "output_directive").is_empty());
        }
        assert!(
            ruby(source, &ranges).root_node().has_error(),
            "authored Ruby is incomplete or invalid; the host must not erase the offending bytes"
        );
    }
}

#[test]
fn rails_raw_output_and_ordinary_graphql_named_ruby_keep_their_exact_code() {
    let source = b"<%= nil %><%== nil %><%graphql value %>";
    let template = host(source);
    assert!(!template.root_node().has_error());
    assert_eq!(nodes(&template, "output_directive").len(), 2);
    assert_eq!(nodes(&template, "directive").len(), 1);
    assert!(nodes(&template, "graphql_directive").is_empty());
    let ranges = nodes(&template, "code");
    let code: Vec<_> = ranges
        .iter()
        .map(|range| &source[range.start_byte..range.end_byte])
        .collect();
    assert_eq!(
        code,
        [
            b" nil ".as_slice(),
            b" nil ".as_slice(),
            b"graphql value ".as_slice()
        ]
    );
    assert!(!ruby(source, &ranges).root_node().has_error());
}
