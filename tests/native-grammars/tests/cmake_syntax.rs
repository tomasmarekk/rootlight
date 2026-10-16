//! Source-backed CMake parsing keeps physical bracket bytes and edit identity.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cmake::LANGUAGE_CMAKE.into())
        .unwrap();
    parser
}

fn ranges(node: Node<'_>, kind: &str, output: &mut Vec<std::ops::Range<usize>>) {
    if node.kind() == kind {
        output.push(node.byte_range());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        ranges(child, kind, output);
    }
}

#[test]
fn bracket_arguments_preserve_whitespace_wrong_delimiters_and_unicode() {
    for content in [
        "",
        "  first\r\nlast  ",
        "\u{a0} leading",
        "a ]=] b",
        "${literal}",
    ] {
        let source = format!("message([==[{content}]==])\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut selected = Vec::new();
        ranges(tree.root_node(), "bracket_argument_content", &mut selected);
        assert_eq!(selected.len(), 1);
        assert_eq!(&source[selected[0].clone()], content);
    }
}

#[test]
fn bracket_comments_and_long_widths_close_only_at_the_matching_delimiter() {
    for width in [0, 1, 128, 1024] {
        let delimiter = "=".repeat(width);
        let content = format!(
            "  retained\r\n${{no_expansion}} ]{}] \u{a0}",
            "=".repeat(width + 1)
        );
        let source = format!("#[{delimiter}[{content}]{delimiter}]\nmessage(done)\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut selected = Vec::new();
        ranges(tree.root_node(), "bracket_comment_content", &mut selected);
        assert_eq!(selected.len(), 1);
        assert_eq!(&source[selected[0].clone()], content);
    }
}

#[test]
fn normal_commands_and_written_block_headers_have_distinct_arguments() {
    let source = "FuNcTiOn(Written first second)\n set(local ${first})\n message(\"${second}\")\nEnDfUnCtIoN()\nmacro(Forward arg)\n Written(${arg} value)\nendmacro()\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut selected = Vec::new();
    ranges(tree.root_node(), "function_command", &mut selected);
    assert_eq!(selected.len(), 1);
    assert_eq!(
        &source[selected[0].clone()],
        "FuNcTiOn(Written first second)"
    );
    let mut arguments = Vec::new();
    ranges(tree.root_node(), "unquoted_argument", &mut arguments);
    for expected in [
        "Written", "first", "second", "local", "${first}", "Forward", "arg",
    ] {
        assert!(
            arguments
                .iter()
                .any(|span| &source[span.clone()] == expected),
            "missing {expected}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn incremental_bracket_width_edit_matches_the_cold_tree() {
    let before = "message([=[  value\n]=])\n";
    let after = "message([==[  value\n]=])\n";
    let mut parser = parser();
    let mut old = parser.parse(before, None).unwrap();
    let offset = before.find("=[").unwrap();
    old.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset,
        new_end_byte: offset + 1,
        start_position: Point::new(0, offset),
        old_end_position: Point::new(0, offset),
        new_end_position: Point::new(0, offset + 1),
    });
    let incremental = parser.parse(after, Some(&old)).unwrap();
    let cold = parser.parse(after, None).unwrap();
    assert_eq!(
        incremental.root_node().to_sexp(),
        cold.root_node().to_sexp()
    );
    assert!(cold.root_node().has_error());
    for kind in [
        "bracket_argument_open",
        "bracket_argument_content",
        "bracket_argument_close",
        "ERROR",
    ] {
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        ranges(cold.root_node(), kind, &mut expected);
        ranges(incremental.root_node(), kind, &mut actual);
        assert_eq!(actual, expected, "{kind}");
    }
}

#[test]
fn unfinished_delimiter_tail_remains_physical_content_with_a_parse_gap() {
    let source = "message([=[content]===\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(tree.root_node().has_error());
    let mut selected = Vec::new();
    ranges(tree.root_node(), "bracket_argument_content", &mut selected);
    assert_eq!(selected.len(), 1, "{}", tree.root_node().to_sexp());
    assert_eq!(&source[selected[0].clone()], "content]===\n");
    assert_eq!(selected[0].end, source.len());
}

#[test]
fn failed_bracket_comment_opener_is_a_complete_line_comment() {
    let source = "#[===not-bracket ${literal}\r\nmessage(done)\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut selected = Vec::new();
    ranges(tree.root_node(), "line_comment", &mut selected);
    assert_eq!(selected.len(), 1);
    assert_eq!(&source[selected[0].clone()], "#[===not-bracket ${literal}");
    assert_eq!(selected[0].start, 0);
}

#[test]
fn empty_if_and_elseif_preserve_following_quoted_arguments() {
    let source = "if()\n set(first \"\")\nelseif()\n set(second \"${ROOT}/## ${package} license\")\nendif()\nset(after \"\")\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut selected = Vec::new();
    ranges(tree.root_node(), "normal_command", &mut selected);
    assert_eq!(
        selected
            .iter()
            .map(|span| &source[span.clone()])
            .collect::<Vec<_>>(),
        [
            "set(first \"\")",
            "set(second \"${ROOT}/## ${package} license\")",
            "set(after \"\")"
        ]
    );
    assert!(
        parser()
            .parse("while()\nendwhile()\n", None)
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn configure_placeholders_have_exact_nodes_without_swallowing_written_commands() {
    let source = "@PACKAGE_INIT@\nfunction(Written out)\n set(local \"${z_@PORT@_root}/lib\")\nendfunction()\nset(after \"\")\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut placeholders = Vec::new();
    ranges(tree.root_node(), "configure_placeholder", &mut placeholders);
    assert_eq!(
        placeholders
            .iter()
            .map(|span| &source[span.clone()])
            .collect::<Vec<_>>(),
        ["@PACKAGE_INIT@", "@PORT@"]
    );
    let mut commands = Vec::new();
    ranges(tree.root_node(), "normal_command", &mut commands);
    assert_eq!(
        commands
            .iter()
            .map(|span| &source[span.clone()])
            .collect::<Vec<_>>(),
        ["set(local \"${z_@PORT@_root}/lib\")", "set(after \"\")"]
    );
    for malformed in [
        "@UNCLOSED\nset(after value)\n",
        "set(value \"${z_@UNCLOSED_root}\")\n",
    ] {
        assert!(
            parser()
                .parse(malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "{malformed}"
        );
    }
}

#[test]
fn placeholder_closing_edit_matches_the_cold_recovery_tree() {
    let before = "set(value \"${z_@PORT@_root}\")\nset(after value)\n";
    let offset = before.find("@_root").unwrap();
    let mut after = before.to_owned();
    after.remove(offset);
    let mut parser = parser();
    let mut old = parser.parse(before, None).unwrap();
    old.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset + 1,
        new_end_byte: offset,
        start_position: Point::new(0, offset),
        old_end_position: Point::new(0, offset + 1),
        new_end_position: Point::new(0, offset),
    });
    let incremental = parser.parse(&after, Some(&old)).unwrap();
    let cold = parser.parse(&after, None).unwrap();
    assert_eq!(
        incremental.root_node().to_sexp(),
        cold.root_node().to_sexp()
    );
    assert!(cold.root_node().has_error());
    for kind in ["normal_command", "configure_placeholder", "ERROR"] {
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        ranges(cold.root_node(), kind, &mut expected);
        ranges(incremental.root_node(), kind, &mut actual);
        assert_eq!(actual, expected, "{kind}");
    }
}
