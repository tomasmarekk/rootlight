//! Human-authored reader-syntax witnesses independent of corpus or parser output.
//! Full physical snapshots are emitted before assertions, including failed cases.

use serde_json::{Value, json};
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Tree};
use tree_sitter_scheme::LANGUAGE_SCHEME;

fn parser() -> Parser {
    let language: Language = LANGUAGE_SCHEME.into();
    assert_eq!(language.abi_version(), 15);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("default ABI-15 table is supported");
    parser
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source
        .get(node.byte_range())
        .expect("physical node has UTF-8 boundaries")
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| {
            node.named_child(u32::try_from(index).expect("small fixture child count"))
                .expect("named child is present")
        })
        .collect()
}

fn kinds(nodes: &[Node<'_>]) -> Vec<&'static str> {
    nodes.iter().map(Node::kind).collect()
}

fn physical_snapshot(tree: &Tree, source: &str) -> Value {
    let mut records = Vec::new();
    let mut leaves = Vec::new();
    let mut pending = vec![(tree.root_node(), None, None::<&str>)];
    while let Some((node, parent, field)) = pending.pop() {
        let ordinal = records.len();
        let start = node.start_position();
        let end = node.end_position();
        records.push(json!({
            "ordinal": ordinal, "parent": parent, "field": field,
            "kind": node.kind(), "named": node.is_named(),
            "missing": node.is_missing(), "error": node.is_error(),
            "has_error": node.has_error(), "start_byte": node.start_byte(),
            "end_byte": node.end_byte(), "start_point": [start.row, start.column],
            "end_point": [end.row, end.column], "source": text(node, source),
        }));
        if node.child_count() == 0 && node.end_byte() > node.start_byte() {
            leaves.push(node.byte_range());
        }
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("small fixture child count");
            pending.push((
                node.child(index).expect("physical child exists"),
                Some(ordinal),
                node.field_name_for_child(index),
            ));
        }
    }
    leaves.sort_by_key(|range| (range.start, range.end));
    let mut pieces = Vec::new();
    let mut rebuilt = String::new();
    let mut cursor = 0;
    for range in leaves {
        assert!(range.start >= cursor, "physical leaf ranges do not overlap");
        if range.start > cursor {
            let gap = source
                .get(cursor..range.start)
                .expect("gap is UTF-8 aligned");
            rebuilt.push_str(gap);
            pieces.push(json!({"kind": "gap", "start": cursor, "end": range.start, "source": gap}));
        }
        let leaf = source.get(range.clone()).expect("leaf is UTF-8 aligned");
        rebuilt.push_str(leaf);
        pieces
            .push(json!({"kind": "leaf", "start": range.start, "end": range.end, "source": leaf}));
        cursor = range.end;
    }
    if cursor < source.len() {
        let gap = source.get(cursor..).expect("trailing gap is UTF-8 aligned");
        rebuilt.push_str(gap);
        pieces.push(json!({"kind": "gap", "start": cursor, "end": source.len(), "source": gap}));
    }
    assert_eq!(
        rebuilt.as_bytes(),
        source.as_bytes(),
        "leaf/gap source conservation"
    );
    json!({"source": source, "source_bytes": source.len(), "nodes": records,
        "partition": pieces, "source_conserved": true, "has_error": tree.root_node().has_error()})
}

fn observe(group: &str, case: &str, source: &str, tree: &Tree) {
    println!(
        "SCHEME_CASE {}",
        json!({"group": group, "case": case,
        "default_root_only": true, "snapshot": physical_snapshot(tree, source)})
    );
}

fn clean(group: &str, case: &str, source: &str) -> Tree {
    let tree = parser()
        .parse(source, None)
        .expect("synthetic source parses to a tree");
    observe(group, case, source, &tree);
    assert_eq!(tree.root_node().kind(), "program");
    assert!(
        !tree.root_node().has_error(),
        "human-valid reader source must be error-free"
    );
    tree
}

fn complete(group: &str) {
    println!(
        "SCHEME_GROUP {}",
        json!({"group": group, "status": "passed"})
    );
}

#[test]
fn proper_square_and_dotted_lists_have_written_ownership() {
    let group = "lists";
    for (case, source, expected) in [
        ("empty", "()", vec![]),
        ("proper", "(alpha beta)", vec!["symbol", "symbol"]),
        ("nested", "(alpha (beta gamma))", vec!["symbol", "list"]),
        ("square", "[left right]", vec!["symbol", "symbol"]),
        ("dotted", "(head . tail)", vec!["symbol", "dot", "symbol"]),
    ] {
        let tree = clean(group, case, source);
        let top = children(tree.root_node());
        assert_eq!(kinds(&top), ["list"]);
        assert_eq!(text(top[0], source), source);
        assert_eq!(kinds(&children(top[0])), expected);
        assert!(top[0].child_by_field_name("name").is_none());
    }
    complete(group);
}

#[test]
fn abbreviation_prefixes_own_their_physical_datum() {
    let group = "abbreviations";
    for (case, source, kind, prefix) in [
        ("quote", "'alpha", "quote", "'"),
        ("quasiquote", "`alpha", "quasiquote", "`"),
        ("unquote", ",alpha", "unquote", ","),
        ("splice", ",@alpha", "unquote_splicing", ",@"),
        ("syntax", "#'alpha", "syntax_quote", "#'"),
        ("quasisyntax", "#`alpha", "quasisyntax", "#`"),
        ("unsyntax", "#,alpha", "unsyntax", "#,"),
        ("unsyntax-splice", "#,@alpha", "unsyntax_splicing", "#,@"),
    ] {
        let tree = clean(group, case, source);
        let top = children(tree.root_node());
        assert_eq!(kinds(&top), [kind]);
        assert_eq!(text(top[0], source), source);
        let datum = children(top[0]);
        assert_eq!(kinds(&datum), ["symbol"]);
        assert_eq!(text(datum[0], source), "alpha");
        assert_eq!(datum[0].start_byte(), prefix.len());
    }
    complete(group);
}

#[test]
fn vectors_and_bytevectors_retain_written_elements() {
    let group = "vectors";
    for (case, source, kind, expected) in [
        (
            "vector",
            "#(alpha (beta) 3)",
            "vector",
            vec!["symbol", "list", "number"],
        ),
        (
            "r7-bytevector",
            "#u8(0 127 255)",
            "byte_vector",
            vec!["number", "number", "number"],
        ),
        (
            "r6-bytevector",
            "#vu8(1 2)",
            "byte_vector",
            vec!["number", "number"],
        ),
    ] {
        let tree = clean(group, case, source);
        let top = children(tree.root_node());
        assert_eq!(kinds(&top), [kind]);
        assert_eq!(text(top[0], source), source);
        assert_eq!(kinds(&children(top[0])), expected);
    }
    complete(group);
}

#[test]
fn comments_keep_nested_and_discarded_data_source_distinct() {
    let group = "comments";
    for (case, source, kind, raw) in [
        ("line", "; alpha\n(keep beta)", "comment", "; alpha"),
        (
            "nested-block",
            "#| outer #| inner |# tail |# (keep beta)",
            "block_comment",
            "#| outer #| inner |# tail |#",
        ),
        (
            "datum",
            "#; (define hidden 1)\n(define visible 2)",
            "sexp_comment",
            "#; (define hidden 1)",
        ),
    ] {
        let tree = clean(group, case, source);
        let top = children(tree.root_node());
        assert_eq!(kinds(&top), [kind, "list"]);
        assert_eq!(text(top[0], source), raw);
        assert_eq!(top[1].end_byte(), source.len());
        if case == "nested-block" {
            let nested = children(top[0]);
            assert_eq!(kinds(&nested), ["block_comment"]);
            assert_eq!(text(nested[0], source), "#| inner |#");
        }
        if case == "datum" {
            assert_eq!(kinds(&children(top[0])), ["list"]);
            assert_eq!(text(top[1], source), "(define visible 2)");
        }
    }
    complete(group);
}

#[test]
fn datum_labels_expose_raw_label_fields_without_graph_resolution() {
    let group = "labels";
    let source = "#12=(alpha . #12#) #12#";
    let tree = clean(group, "label-and-references", source);
    let top = children(tree.root_node());
    assert_eq!(kinds(&top), ["datum_label", "datum_reference"]);
    for node in &top {
        let label = node
            .child_by_field_name("label")
            .expect("written label field");
        assert_eq!(label.kind(), "datum_label_id");
        assert_eq!(text(label, source), "12");
    }
    assert_eq!(kinds(&children(top[0])), ["datum_label_id", "list"]);
    let datum = children(top[0])[1];
    assert_eq!(
        kinds(&children(datum)),
        ["symbol", "dot", "datum_reference"]
    );
    assert_ne!(children(datum)[2].start_byte(), top[1].start_byte());
    complete(group);
}

#[test]
fn unicode_and_escaped_symbols_preserve_raw_spelling_and_directives() {
    let group = "symbols";
    let source = "λ |two words| \\x3bb;alpha ->name";
    let tree = clean(group, "raw-symbols", source);
    let top = children(tree.root_node());
    assert_eq!(kinds(&top), ["symbol", "symbol", "symbol", "symbol"]);
    assert_eq!(
        top.iter()
            .map(|node| text(*node, source))
            .collect::<Vec<_>>(),
        ["λ", "|two words|", "\\x3bb;alpha", "->name"]
    );
    let source = "#!r6rs\n#!fold-case Mixed #!no-fold-case Mixed";
    let tree = clean(group, "directives-are-not-casefold", source);
    let top = children(tree.root_node());
    assert_eq!(
        kinds(&top),
        ["directive", "directive", "symbol", "directive", "symbol"]
    );
    assert_eq!(text(top[2], source), "Mixed");
    assert_eq!(text(top[4], source), "Mixed");
    assert_ne!(top[2].start_byte(), top[4].start_byte());
    complete(group);
}

#[test]
fn union_literal_shapes_preserve_bytes_without_evaluating_values() {
    let group = "literals";
    let source = "#t #false #\\space #\\x3bb #\\u03bb \"a\\nβ\" #:option";
    let tree = clean(group, "literal-kinds", source);
    let top = children(tree.root_node());
    assert_eq!(
        kinds(&top),
        [
            "boolean",
            "boolean",
            "character",
            "character",
            "character",
            "string",
            "keyword"
        ]
    );
    assert_eq!(
        top.iter()
            .map(|node| text(*node, source))
            .collect::<Vec<_>>(),
        [
            "#t",
            "#false",
            "#\\space",
            "#\\x3bb",
            "#\\u03bb",
            "\"a\\nβ\"",
            "#:option"
        ]
    );
    assert_eq!(kinds(&children(top[5])), ["escape_sequence"]);
    let source = "42 -7 1/2 1.25 #x2a #b101 #e1.0 1+2i +inf.0";
    let tree = clean(group, "number-kinds", source);
    let top = children(tree.root_node());
    assert_eq!(kinds(&top), vec!["number"; 9]);
    assert_eq!(
        top.iter()
            .map(|node| text(*node, source))
            .collect::<Vec<_>>(),
        [
            "42", "-7", "1/2", "1.25", "#x2a", "#b101", "#e1.0", "1+2i", "+inf.0"
        ]
    );
    complete(group);
}

#[test]
fn malformed_prefix_does_not_steal_a_complete_clean_sibling() {
    let group = "recovery";
    let source = ") (define clean 1)";
    let tree = parser()
        .parse(source, None)
        .expect("malformed source still has a tree");
    observe(group, "unmatched-closing-delimiter", source, &tree);
    assert!(
        tree.root_node().has_error(),
        "unmatched closing delimiter is malformed"
    );
    let top = children(tree.root_node());
    let list = top
        .iter()
        .find(|node| node.kind() == "list")
        .expect("complete clean sibling survives");
    assert_eq!(text(*list, source), "(define clean 1)");
    assert!(!list.has_error());
    assert_eq!(kinds(&children(*list)), ["symbol", "symbol", "number"]);
    assert_eq!(list.start_byte(), 2);
    assert_eq!(list.end_byte(), source.len());
    complete(group);
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = source.get(..byte).expect("edit is UTF-8 aligned");
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix.rsplit('\n').next().expect("split has a tail").len();
    Point { row, column }
}

#[test]
fn noop_and_incremental_body_edit_match_clean_physical_snapshots() {
    let group = "incremental";
    let source = "(define (identity λ)\n  λ)\n";
    let mut parser = parser();
    let original = parser.parse(source, None).expect("initial tree");
    observe(group, "initial", source, &original);
    assert!(!original.root_node().has_error());
    let noop = parser.parse(source, Some(&original)).expect("no-op tree");
    observe(group, "no-op", source, &noop);
    assert_eq!(
        physical_snapshot(&original, source),
        physical_snapshot(&noop, source)
    );
    let start = source.rfind('λ').expect("body occurrence is present");
    let end = start + 'λ'.len_utf8();
    let replacement = "β";
    let edited_source = format!("{}{}{}", &source[..start], replacement, &source[end..]);
    let mut edited = original.clone();
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: end,
        new_end_byte: start + replacement.len(),
        start_position: point(source, start),
        old_end_position: point(source, end),
        new_end_position: point(&edited_source, start + replacement.len()),
    });
    let incremental = parser
        .parse(&edited_source, Some(&edited))
        .expect("incremental tree");
    observe(group, "incremental", &edited_source, &incremental);
    let rebuilt = self::parser()
        .parse(&edited_source, None)
        .expect("independent clean tree");
    observe(group, "clean-rebuild", &edited_source, &rebuilt);
    assert!(!incremental.root_node().has_error());
    assert_eq!(
        physical_snapshot(&incremental, &edited_source),
        physical_snapshot(&rebuilt, &edited_source)
    );
    let definition = children(incremental.root_node())[0];
    assert_eq!(kinds(&children(definition)), ["symbol", "list", "symbol"]);
    assert_eq!(text(children(definition)[2], &edited_source), "β");
    complete(group);
}
