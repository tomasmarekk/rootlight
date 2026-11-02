//! Native Vento host regressions use the original physical pipeline witness.
//! These controls do not render templates or qualify the service language route.

use tree_sitter::{InputEdit, Parser, Point, Tree};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/vento/badly_formatted.vto");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vento::LANGUAGE.into())
        .expect("retained ABI15 Vento grammar loads");
    parser
}

fn nodes(tree: &Tree, kind: &str) -> Vec<std::ops::Range<usize>> {
    let mut result = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            result.push(node.byte_range());
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    result.sort_by_key(|span| span.start);
    result
}

fn clean(source: &[u8]) -> Tree {
    let tree = parser()
        .parse(source, None)
        .expect("native parse completes");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().kind(), "template");
    assert_eq!(tree.root_node().start_byte(), 0);
    assert_eq!(tree.root_node().end_byte(), source.len());
    tree
}

#[test]
fn original_pipeline_keeps_the_native_filter_boundary() {
    assert_eq!(ORIGINAL.len(), 52);
    let tree = clean(ORIGINAL);
    assert_eq!(nodes(&tree, "tag").len(), 1);
    assert_eq!(nodes(&tree, "filter").len(), 1);
    assert!(
        nodes(&tree, "code")
            .iter()
            .any(|span| { ORIGINAL.get(span.clone()) == Some(b"toUpperCase".as_slice()) })
    );
}

#[test]
fn quoted_pipes_and_comments_do_not_create_filter_stages() {
    let source = b"{{# |> hidden #}}<p>{{ \"a |> not_a_filter }}\" |> first |> second(1) }}</p>";
    let tree = clean(source);
    let code = nodes(&tree, "code")
        .into_iter()
        .map(|span| source.get(span).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        code,
        [
            b"\"a |> not_a_filter }}\" ".as_slice(),
            b"first ".as_slice(),
            b"second(1)".as_slice(),
        ]
    );
    assert_eq!(nodes(&tree, "comment_tag").len(), 1);
    assert_eq!(nodes(&tree, "tag").len(), 1);
}

#[test]
fn incomplete_native_tags_keep_error_evidence() {
    for source in [
        b"{{ if ready }}open".as_slice(),
        b"{{ value |> }}".as_slice(),
        b"{{ for item of values }}open".as_slice(),
        b"{{ \"unterminated |> false_filter }}".as_slice(),
        b"{{ 'unterminated\\".as_slice(),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "missing native syntax must not look clean: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn quoted_escape_parity_keeps_only_the_actual_pipeline() {
    for (source, expected) in [
        (
            br#"{{ "a\" |> hidden }}" |> real }}"#.as_slice(),
            br#""a\" |> hidden }}" "#.as_slice(),
        ),
        (
            br#"{{ "a\\" |> real }}"#.as_slice(),
            br#""a\\" "#.as_slice(),
        ),
        (
            br#"{{ 'a\' |> hidden }}' |> real }}"#.as_slice(),
            br#"'a\' |> hidden }}' "#.as_slice(),
        ),
    ] {
        let tree = clean(source);
        let code = nodes(&tree, "code")
            .into_iter()
            .map(|span| source.get(span).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(code, [expected, b"real".as_slice()]);
        assert_eq!(nodes(&tree, "filter").len(), 1);
    }
    let tight = br#"{{ "}}"}}"#;
    let tree = clean(tight);
    assert_eq!(nodes(&tree, "tag").len(), 1);
    let code = nodes(&tree, "code");
    assert_eq!(code.len(), 1);
    assert_eq!(code.first(), Some(&(3..7)));
}

#[test]
fn nested_template_literals_keep_interpolation_delimiters_inside_code() {
    let source = b"{{ `outer ${`inner |> hidden }}`}` |> actual }}";
    let tree = clean(source);
    let code = nodes(&tree, "code")
        .into_iter()
        .map(|span| source.get(span).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        code,
        [
            b"`outer ${`inner |> hidden }}`}` ".as_slice(),
            b"actual".as_slice()
        ]
    );
    assert_eq!(nodes(&tree, "filter").len(), 1);
}

#[test]
fn default_parameter_objects_keep_quoted_commas_and_braces_inside_the_value() {
    let source = br#"{{ function render (value = {"close": "},) |> hidden", "items": ["|>", "}}"]}) }}{{ value }}{{ /function }}"#;
    let tree = clean(source);
    assert_eq!(nodes(&tree, "function_block").len(), 1);
    assert_eq!(nodes(&tree, "parameter").len(), 1);
    assert!(nodes(&tree, "filter").is_empty());
    assert!(nodes(&tree, "code").iter().any(|span| {
        source.get(span.clone())
            == Some(br#"{"close": "},) |> hidden", "items": ["|>", "}}"]}"#.as_slice())
    }));
}

#[test]
fn a_single_nunjucks_pipe_is_not_a_vento_filter() {
    let source = b"<h1>  {{ \"Hello, world!\" | upper }}\n  </h1>\n\n";
    assert_eq!(source.len(), 45);
    let tree = clean(source);
    assert!(nodes(&tree, "filter").is_empty());
    assert!(
        nodes(&tree, "code")
            .iter()
            .any(|span| source.get(span.clone()) == Some(b"\"Hello, world!\" | upper".as_slice()))
    );
}

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    let row = prefix.iter().filter(|byte| **byte == b'\n').count();
    let column = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(prefix.len(), |newline| prefix.len() - newline - 1);
    Point::new(row, column)
}

#[test]
fn changed_filter_incremental_restore_matches_fresh_native_trees() {
    let mut parser = parser();
    let mut before = parser.parse(ORIGINAL, None).unwrap();
    let start = ORIGINAL
        .windows(b"toUpperCase".len())
        .position(|bytes| bytes == b"toUpperCase")
        .unwrap();
    let end = start + b"toUpperCase".len();
    let mut changed = ORIGINAL.to_vec();
    changed[start..end].copy_from_slice(b"toLowerCase");
    let edit = InputEdit {
        start_byte: start,
        old_end_byte: end,
        new_end_byte: end,
        start_position: point(ORIGINAL, start),
        old_end_position: point(ORIGINAL, end),
        new_end_position: point(&changed, end),
    };
    before.edit(&edit);
    let mut incremental = parser.parse(&changed, Some(&before)).unwrap();
    let fresh = clean(&changed);
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    let code = nodes(&incremental, "code");
    assert!(
        code.iter()
            .any(|span| changed.get(span.clone()) == Some(b"toLowerCase".as_slice()))
    );
    assert!(
        !code
            .iter()
            .any(|span| changed.get(span.clone()) == Some(b"toUpperCase".as_slice()))
    );
    incremental.edit(&edit);
    let restored = parser.parse(ORIGINAL, Some(&incremental)).unwrap();
    assert_eq!(
        restored.root_node().to_sexp(),
        clean(ORIGINAL).root_node().to_sexp()
    );
    let malformed = parser.parse(b"{{ value |> }}", None).unwrap();
    assert!(malformed.root_node().has_error());
    parser.reset();
    let reused = parser.parse(ORIGINAL, None).unwrap();
    assert_eq!(
        reused.root_node().to_sexp(),
        clean(ORIGINAL).root_node().to_sexp()
    );
}
