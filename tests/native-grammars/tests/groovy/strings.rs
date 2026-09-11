//! String controls distinguish literal text, interpolation, comments, and division.
//! Authored sources are parsed only and retain exact byte/point ownership.
use tree_sitter::{Node, Parser, Point, Tree};
fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser
}
fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = vec![];
    while let Some(n) = pending.pop() {
        let mut c = n.walk();
        pending.extend(n.children(&mut c));
        result.push(n);
    }
    result
}
fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |p| prefix.len() - p - 1),
    )
}
fn valid(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    for n in nodes(tree.root_node()) {
        assert!(source.get(n.byte_range()).is_some());
        assert_eq!(n.start_position(), point(source, n.start_byte()));
        assert_eq!(n.end_position(), point(source, n.end_byte()));
    }
    tree
}
fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == kind && n.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}
#[test]
fn slashy_literals_preserve_whitespace_newlines_and_equals_prefix() {
    for literal in [
        "/ leading space /",
        "/\t/",
        "/first\nsecond/",
        "/first\r\nλ😀/",
        "/=value/",
        "/\n/",
    ] {
        let source = format!("def text = {literal}\nfinish()\n");
        let tree = valid(&source);
        exact(&tree, &source, "string_literal", literal);
        exact(&tree, &source, "method_invocation", "finish()");
    }
}
#[test]
fn slashy_escape_sequences_do_not_hide_interpolations() {
    for literal in [
        r"/left\/right/",
        r"/\t\n/",
        r"/\${render()}/",
        "/ first\n${render()}\nlast /",
    ] {
        let source = format!("def text = {literal}\n");
        let tree = valid(&source);
        exact(&tree, &source, "string_literal", literal);
        if literal.contains("render()") {
            exact(&tree, &source, "method_invocation", "render()");
        }
    }
}
#[test]
fn lazy_gstrings_preserve_closure_parameters_body_and_calls() {
    for literal in [
        r#""${-> render()}""#,
        r#""""${-> render()}""""#,
        "/${-> render()}/",
        "$/prefix${-> render()}suffix/$",
        r#""${writer -> writer << render()}""#,
        r#""${->}""#,
    ] {
        let source = format!("def text = {literal}\n");
        let tree = valid(&source);
        exact(&tree, &source, "string_literal", literal);
        assert!(
            nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "gstring_closure")
        );
        if literal.contains("render()") {
            exact(&tree, &source, "method_invocation", "render()");
        }
    }
}
#[test]
fn division_and_comments_are_not_slashy_openers() {
    for expression in [
        "left/right/end",
        "left / right / end",
        "left/=right",
        "left/*note*//right",
    ] {
        let source = format!("def value = {expression}\n");
        let tree = valid(&source);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "string_literal")
        );
    }
    let source = "// text\n/* text */\ndef value = / text /\n";
    let tree = valid(source);
    exact(&tree, source, "line_comment", "// text");
    exact(&tree, source, "block_comment", "/* text */");
}
#[test]
fn incomplete_literals_are_errors_and_reset_cannot_retain_string_context() {
    let source = "def text = / text /\nfinish()\n";
    let expected = valid(source).root_node().to_sexp();
    let mut p = parser();
    for broken in [
        "def text = / text",
        "def text = /first\nsecond",
        r#"def text = "${-> render()}"#,
        "def text = /${-> render()/",
    ] {
        assert!(
            p.parse(broken, None).unwrap().root_node().has_error(),
            "{broken}"
        );
        p.reset();
        assert_eq!(
            p.parse(source, None).unwrap().root_node().to_sexp(),
            expected
        );
    }
}

#[test]
fn embedded_division_and_nested_literals_do_not_close_outer_slashy_strings() {
    for literal in [
        "/ first ${left / right} last /",
        "/ first ${render('/')} last /",
        "/ first ${render(/inner/)} last /",
        "/ ${-> def value = render(); value} /",
        "/ ${writer ->\nwriter << render()\n} /",
    ] {
        let source = format!("def text = {literal}\nfinish()\n");
        let tree = valid(&source);
        exact(&tree, &source, "string_literal", literal);
        exact(&tree, &source, "method_invocation", "finish()");
    }
}

#[test]
fn slashy_backslashes_only_escape_forward_slashes() {
    for literal in [
        r"/\${value}/",
        r"/left\\/right/",
        r"/\n\t\u0041/",
        r"/\$5/",
        r"/\$ /",
        r"/left\\\/right/",
        r"/end\/",
        r"/end\\/",
    ] {
        let source = format!("def text = {literal}\n");
        let tree = valid(&source);
        exact(&tree, &source, "string_literal", literal);
    }
    let source = "def text = /left\0right/";
    assert!(
        parser()
            .parse(source, None)
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn escaped_delimiter_cannot_close_early_to_rescue_invalid_syntax() {
    let source = r"def text = /first\/ + /last/";
    assert!(
        parser()
            .parse(source, None)
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn string_edits_preserve_exact_utf8_points_and_fresh_equivalence() {
    use tree_sitter::InputEdit;
    let source = "def text = / λ😀\n${-> render()}\nlast /\nfinish()\n";
    for (before, after) in [
        ("λ😀", "δ🌍!"),
        ("-> render()", "writer -> writer << render()"),
        ("\nlast", "\r\nlast"),
        ("render()", "left / right"),
        (" λ😀", "\\${value} λ😀"),
    ] {
        let changed = source.replacen(before, after, 1);
        let start = source.find(before).unwrap();
        let mut old = valid(source);
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        let incremental = parser().parse(&changed, Some(&old)).unwrap();
        let fresh = valid(&changed);
        let signature = |tree: &Tree| {
            nodes(tree.root_node())
                .into_iter()
                .map(|n| {
                    (
                        n.kind().to_owned(),
                        n.byte_range(),
                        n.start_position(),
                        n.end_position(),
                        n.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            signature(&incremental),
            signature(&fresh),
            "{before}->{after}"
        );
    }
}

#[test]
fn included_multiline_string_range_keeps_host_coordinates() {
    let prefix = "host λ😀\r\n";
    let body = "def text = / first\r\n${-> render()} last /\n";
    let source = format!("{prefix}{body}suffix");
    let mut p = parser();
    p.set_included_ranges(&[tree_sitter::Range {
        start_byte: prefix.len(),
        end_byte: prefix.len() + body.len(),
        start_point: point(&source, prefix.len()),
        end_point: point(&source, prefix.len() + body.len()),
    }])
    .unwrap();
    let tree = p.parse(&source, None).unwrap();
    assert!(!tree.root_node().has_error());
    exact(
        &tree,
        &source,
        "string_literal",
        "/ first\r\n${-> render()} last /",
    );
    for n in nodes(tree.root_node()) {
        assert_eq!(n.start_position(), point(&source, n.start_byte()));
        assert_eq!(n.end_position(), point(&source, n.end_byte()));
    }
}

#[test]
fn growing_multiline_slashy_literals_have_bounded_input_work() {
    for (kind, fragment) in [("multiline", "λ😀\n"), ("escaped", r"part\/next")] {
        let mut prior = None;
        for count in [128, 256, 512, 1024] {
            let literal = format!("/ {}last /", fragment.repeat(count));
            let source = format!("def text = {literal}\nfinish()\n");
            let mut reads = 0_usize;
            let tree = parser()
                .parse_with_options(
                    &mut |offset, _| {
                        reads += 1;
                        source
                            .as_bytes()
                            .get(offset..offset.saturating_add(1))
                            .unwrap_or_default()
                    },
                    None,
                    None,
                )
                .unwrap();
            assert!(!tree.root_node().has_error());
            exact(&tree, &source, "string_literal", &literal);
            exact(&tree, &source, "method_invocation", "finish()");
            assert!(reads <= source.len() * 20);
            if let Some(value) = prior {
                assert!(reads <= value * 3);
            }
            prior = Some(reads);
            println!(
                "string-work kind={kind} count={count} bytes={} reads={reads}",
                source.len()
            );
        }
    }
}
