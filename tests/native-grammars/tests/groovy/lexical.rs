//! Comment boundaries must not hide adjacent source or reinterpret literal text.
//! Authored line-ending variants exercise scanner behavior without corpus inputs.
use tree_sitter::{Node, Parser};

fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    p
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

#[test]
fn line_comments_stop_before_every_groovy_line_terminator() {
    for ending in ["\n", "\r\n", "\r"] {
        let source = format!("// λ comment{ending}finish(){ending}// final");
        let tree = parser().parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let comments: Vec<_> = nodes(tree.root_node())
            .into_iter()
            .filter(|n| n.kind() == "line_comment")
            .map(|n| n.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(comments, ["// final", "// λ comment"], "{ending:?}");
        assert!(
            nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "method_invocation"
                    && n.utf8_text(source.as_bytes()).unwrap() == "finish()")
        );
    }
}

#[test]
fn comment_lookahead_preserves_cr_only_member_and_statement_boundaries() {
    for ending in ["\n", "\r\n", "\r"] {
        for tail in [".member()", "other()"] {
            let source =
                format!("value{ending}// λ{ending}/* δ */{ending}{tail}{ending}finish(){ending}");
            let tree = parser().parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source:?} {}",
                tree.root_node().to_sexp()
            );
            assert!(
                nodes(tree.root_node())
                    .iter()
                    .any(|n| n.kind() == "method_invocation"
                        && n.utf8_text(source.as_bytes()).unwrap() == "finish()"),
                "{source:?}"
            );
            let comments: Vec<_> = nodes(tree.root_node())
                .into_iter()
                .filter(|n| n.kind() == "line_comment" || n.kind() == "block_comment")
                .map(|n| n.utf8_text(source.as_bytes()).unwrap())
                .collect();
            assert_eq!(comments, ["/* δ */", "// λ"], "{source:?}");
        }
    }
}

#[test]
fn comment_only_files_keep_each_complete_comment() {
    for source in [
        "",
        " \t\r\n",
        "// last",
        "/* a */",
        "/** doc */",
        "/* a */\r// b\r/** c */",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(!tree.root_node().has_error(), "{source:?}");
        for n in nodes(tree.root_node()) {
            assert!(source.get(n.byte_range()).is_some());
        }
    }
    for source in ["/*", "/**", "/* unterminated\rfinish()"] {
        assert!(
            parser()
                .parse(source, None)
                .unwrap()
                .root_node()
                .has_error(),
            "{source:?}"
        );
    }
}

#[test]
fn comment_markers_inside_quotes_are_literal_source() {
    for literal in [
        "'// note'",
        "'/* note */'",
        "\"// note\"",
        "\"/* note */\"",
        "'''first\r// note\r/* note */'''",
        "$/ // note /* note */ /$",
    ] {
        let source = format!("def text = {literal}\nfinish()\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error(), "{source:?}");
        assert!(!nodes(tree.root_node()).iter().any(|n| matches!(
            n.kind(),
            "line_comment" | "block_comment" | "groovydoc_comment"
        )));
    }
}

#[test]
fn comments_in_embedded_expressions_remain_source_nodes() {
    for (open, close) in [("\"", "\""), ("\"\"\"", "\"\"\""), ("/", "/"), ("$/", "/$")] {
        let source =
            format!("def text = {open}first ${{value /* λ */ + 1}} last{close}\nfinish()\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source:?} {}",
            tree.root_node().to_sexp()
        );
        let comments: Vec<_> = nodes(tree.root_node())
            .into_iter()
            .filter(|n| n.kind() == "block_comment")
            .map(|n| n.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(comments, ["/* λ */"]);
    }
}
