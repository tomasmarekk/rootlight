//! Operator continuation must follow Groovy meaning while keeping source trivia visible.
//! Fixtures are authored syntax only, with exact UTF-8 spans and no source execution.

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
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        result.push(node);
    }
    result
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = &source[..byte];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn valid(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
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
fn member_operators_cross_visible_comment_sequences() {
    for (operator, kind) in [
        (".", "field_access"),
        ("?.", "safe_navigation_expression"),
        ("??.", "safe_chain_dot_expression"),
        ("*.", "spread_dot_expression"),
        (".&", "method_pointer_expression"),
        (".@", "direct_field_access_expression"),
        ("::", "method_reference_expression"),
    ] {
        for gap in [
            "\n",
            "\n// λ😀\n",
            "\n/* λ😀 */\n",
            "\n/** λ😀 */\n",
            "\r\n// first\r\n/* second */\r\n",
        ] {
            let expression = format!("value{gap}{operator}member");
            let source = format!("def result = {expression}\n");
            let tree = valid(&source);
            exact(&tree, &source, kind, &expression);
            let comments: Vec<_> = nodes(tree.root_node())
                .into_iter()
                .filter(|n| n.kind().ends_with("comment"))
                .collect();
            assert_eq!(
                comments.len(),
                gap.matches("//").count() + gap.matches("/*").count()
            );
        }
    }
}

#[test]
fn multiplicative_and_binary_operators_continue_after_newlines() {
    for operator in [
        "*", "/", "%", "<<", ">>", ">>>", "<", "<=", ">", ">=", "==", "!=", "&", "^", "|", "&&",
        "||",
    ] {
        for gap in ["\n", "\n/* λ😀 */\n"] {
            let expression = format!("left{gap}{operator} right");
            let source = format!("def result = {expression}\n");
            let tree = valid(&source);
            exact(&tree, &source, "binary_expression", &expression);
        }
    }
}

#[test]
fn ranges_continue_without_becoming_member_access() {
    for operator in ["..", "..<", "<..", "<..<"] {
        let expression = format!("first\n/* note */\n{operator} last");
        let source = format!("def result = {expression}\n");
        let tree = valid(&source);
        exact(&tree, &source, "range_expression", &expression);
    }
}

#[test]
fn leading_plus_minus_start_separate_statements() {
    for operator in ["+", "-"] {
        let source = format!("first\n/* note */\n{operator} second\n");
        let tree = valid(&source);
        exact(&tree, &source, "expression_statement", "first");
        exact(
            &tree,
            &source,
            "unary_expression",
            &format!("{operator} second"),
        );
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "binary_expression")
        );
    }
}

#[test]
fn comment_lines_do_not_join_independent_statements() {
    let source = "first\n// note\n/* λ😀 */\nsecond\n";
    let tree = valid(source);
    exact(&tree, source, "expression_statement", "first");
    exact(&tree, source, "expression_statement", "second");
    assert!(
        !nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == "command_chain")
    );
}

#[test]
fn continued_operators_keep_precedence_and_return_ownership() {
    let source = "def result = left + right\n/* note */\n* last\ndef run() { return\n// note\nvalue\n.member }\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "binary_expression",
        "right\n/* note */\n* last",
    );
    exact(&tree, source, "return_statement", "return");
    exact(&tree, source, "field_access", "value\n.member");
}

#[test]
fn cached_continuation_changes_match_fresh_source_trees() {
    use tree_sitter::InputEdit;
    for (source, before, after) in [
        (
            "def result = value\n// one\n/* λ😀 */\n .member\nfinish()\n",
            ".member",
            "other",
        ),
        (
            "def result = value\n// one\n/* λ😀 */\n other\nfinish()\n",
            "other",
            ".member",
        ),
        (
            "def result = value\n// one\n/* λ😀 */\n .member\nfinish()\n",
            "λ😀",
            "δ🌍 longer",
        ),
        (
            "def result = value\n// one\n/* λ😀 */\n .member\nfinish()\n",
            "\n .member",
            " .member",
        ),
        (
            "def result = value\n// one\n/* λ😀 */\n .member\nfinish()\n",
            "/* λ😀 */",
            "/* λ\n😀 */\n// more",
        ),
    ] {
        let mut parser = parser();
        let mut old = valid(source);
        let start = source.find(before).unwrap();
        let changed = source.replacen(before, after, 1);
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        let fresh = valid(&changed);
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
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
            "edit {before:?} -> {after:?}"
        );
    }
}

#[test]
fn long_comment_sequences_have_bounded_input_work_for_both_decisions() {
    for ending in [".member", "other"] {
        let mut prior = None;
        for count in [128, 256, 512, 1024] {
            let source = format!(
                "def result = value\n{}  {ending}\nfinish()\n",
                "// λ😀\n/* note */\n".repeat(count)
            );
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
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            assert_eq!(
                nodes(tree.root_node())
                    .iter()
                    .filter(|n| n.kind().ends_with("comment"))
                    .count(),
                count * 2
            );
            assert!(
                reads <= source.len() * 20,
                "unbounded reads: {reads}, bytes={}",
                source.len()
            );
            if let Some(previous) = prior {
                assert!(
                    reads <= previous * 3,
                    "superlinear doubling: {previous}->{reads}"
                );
            }
            println!(
                "comment-work ending={ending} count={count} bytes={} reads={reads}",
                source.len()
            );
            prior = Some(reads);
        }
    }
}

#[test]
fn continuation_lookahead_does_not_turn_string_contents_into_comments() {
    for literal in [
        "'''value\n// note\n.member'''",
        "\"\"\"value\n/* note */\n.member\"\"\"",
        "$/value\n// note\n.member/$",
    ] {
        let source = format!("def text = {literal}\n");
        let tree = valid(&source);
        exact(&tree, &source, "string_literal", literal);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind().ends_with("comment")),
            "literal trivia became comment nodes: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn keyword_continuations_require_complete_keyword_boundaries() {
    for (operator, kind, right) in [
        ("in", "membership_expression", "items"),
        ("!in", "membership_expression", "items"),
        ("as", "cast_expression", "String"),
        ("instanceof", "instanceof_expression", "String"),
        ("!instanceof", "instanceof_expression", "String"),
    ] {
        let expression = format!("value\n/* note */\n{operator} {right}");
        let source = format!("def result = {expression}\n");
        let tree = valid(&source);
        exact(&tree, &source, kind, &expression);
    }
    for word in ["inside", "instanceofValue", "assertion", "asValue"] {
        let source = format!("value\n// note\n{word}\n");
        let tree = valid(&source);
        exact(&tree, &source, "expression_statement", "value");
        exact(&tree, &source, "expression_statement", word);
    }
}

#[test]
fn trailing_indentation_cannot_leak_a_cached_decision() {
    for ending in [".member", "other"] {
        for gap in ["\n// note\n  ", "\n/* note */  "] {
            let source = format!("def result = value{gap}{ending}\nfirst\nsecond\n");
            let tree = valid(&source);
            exact(&tree, &source, "expression_statement", "first");
            exact(&tree, &source, "expression_statement", "second");
        }
    }
}

#[test]
fn malformed_continuation_reset_is_clean() {
    let mut parser = parser();
    let source = "def result = value\n// note\n.member\nfirst\nsecond\n";
    for malformed in [
        "def result = value\n// note\n.",
        "def result = value\n/* unfinished",
        "def result = value\n// note\n*",
        "def result = value\n** 2",
    ] {
        assert!(
            parser
                .parse(malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "{malformed}"
        );
        parser.reset();
        assert_eq!(
            parser.parse(source, None).unwrap().root_node().to_sexp(),
            valid(source).root_node().to_sexp()
        );
    }
}

#[test]
fn edits_at_every_trivia_boundary_match_fresh_recovery() {
    use tree_sitter::InputEdit;
    let source = "def x = value\n// one\n/* λ😀 */\n .member\nfirst\nsecond\n";
    let original = valid(source);
    for (start, _) in source.char_indices() {
        for inserted in [";", "\n", "/*x*/", " "] {
            let mut changed = source.to_owned();
            changed.insert_str(start, inserted);
            let mut old = original.clone();
            old.edit(&InputEdit {
                start_byte: start,
                old_end_byte: start,
                new_end_byte: start + inserted.len(),
                start_position: point(source, start),
                old_end_position: point(source, start),
                new_end_position: point(&changed, start + inserted.len()),
            });
            let mut parser = parser();
            let incremental = parser.parse(&changed, Some(&old)).unwrap();
            let fresh = parser.parse(&changed, None).unwrap();
            let signature = |tree: &Tree| {
                nodes(tree.root_node())
                    .into_iter()
                    .map(|n| (n.kind().to_owned(), n.byte_range(), n.is_missing()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                signature(&incremental),
                signature(&fresh),
                "offset={start}, inserted={inserted:?}"
            );
        }
    }
}

#[test]
fn included_continuation_range_preserves_host_coordinates() {
    let prefix = "# λ😀\r\n```groovy\r\n";
    let body = "def result = value\r\n// note\r\n/* λ😀 */\r\n.member\r\n";
    let source = format!("{prefix}{body}```\r\n");
    let mut parser = parser();
    let end = prefix.len() + body.len();
    parser
        .set_included_ranges(&[tree_sitter::Range {
            start_byte: prefix.len(),
            end_byte: end,
            start_point: point(&source, prefix.len()),
            end_point: point(&source, end),
        }])
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert!(!tree.root_node().has_error());
    for node in nodes(tree.root_node()) {
        assert!(node.start_byte() >= prefix.len() && node.end_byte() <= end);
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(&source, node.start_byte()));
        assert_eq!(node.end_position(), point(&source, node.end_byte()));
    }
    exact(
        &tree,
        &source,
        "field_access",
        "value\r\n// note\r\n/* λ😀 */\r\n.member",
    );
}
