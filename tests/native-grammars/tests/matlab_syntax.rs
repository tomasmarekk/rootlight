//! Native MATLAB admission pins authored syntax and exact UTF-8 source coordinates.
//! These tests parse text only; command and shell forms are never executed.

use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

const FUNCTIONS: &str = include_str!("../fixtures/matlab/functions.m");
const CLASS: &str = include_str!("../fixtures/matlab/Meter.m");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_matlab::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    result
}

fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, bool, bool)> {
    nodes(tree.root_node())
        .into_iter()
        .map(|node| {
            (
                node.kind().to_owned(),
                node.byte_range(),
                node.is_named(),
                node.is_missing(),
            )
        })
        .collect()
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn valid(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source}: {}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        assert!(!node.is_missing());
    }
    tree
}

fn names<'s>(tree: &Tree, source: &'s str, kind: &str) -> Vec<&'s str> {
    nodes(tree.root_node())
        .into_iter()
        .filter(|node| node.kind() == kind)
        .map(|node| {
            node.child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap()
        })
        .collect()
}

#[test]
fn matlab_definitions_and_parameters_preserve_authored_source() {
    let language: tree_sitter::Language = tree_sitter_matlab::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    for newline in ["\n", "\r\n"] {
        let source = FUNCTIONS.replace('\n', newline);
        let tree = valid(&source);
        assert_eq!(tree.root_node().byte_range(), 0..source.len());
        assert_eq!(
            names(&tree, &source, "function_definition"),
            ["summarize", "adjust", "identity"]
        );
        let all = nodes(tree.root_node());
        let parameters: Vec<_> = all
            .iter()
            .filter(|node| node.kind() == "function_arguments")
            .map(|node| node.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(parameters, ["(values, scale)", "(input)", "(input, ~)"]);
        for kind in ["lambda", "arguments_statement", "matrix", "cell"] {
            assert_eq!(
                all.iter().filter(|node| node.kind() == kind).count(),
                1,
                "{kind}"
            );
        }
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "postfix_operator")
                .count(),
            2
        );
    }
}

#[test]
fn matlab_classes_methods_and_properties_have_exact_names() {
    for newline in ["\n", "\r\n"] {
        let source = CLASS.replace('\n', newline);
        let tree = valid(&source);
        assert_eq!(names(&tree, &source, "class_definition"), ["Meter"]);
        assert_eq!(
            names(&tree, &source, "function_definition"),
            ["Meter", "read", "Value"]
        );
        assert_eq!(names(&tree, &source, "property"), ["Value"]);
    }
}

#[test]
fn matlab_strings_and_comments_do_not_invent_definitions() {
    let source = "% function hidden(x)\n%{\nfunction other(x)\n%}\ntext = \"function fake(x)\";\nfunction result = actual(value)\nresult = value;\nend\n";
    let tree = valid(source);
    assert_eq!(names(&tree, source, "function_definition"), ["actual"]);
}

#[test]
fn matlab_malformed_sources_retain_errors_and_allow_parser_reuse() {
    let mut reused = parser();
    let expected = valid(FUNCTIONS);
    for source in [
        "function =\n",
        "value = ;\n",
        "value = \"unfinished\n",
        "if value\n",
        "a = [1,\n",
    ] {
        let tree = reused.parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "accepted {source}: {}",
            tree.root_node().to_sexp()
        );
        assert!(
            nodes(tree.root_node())
                .iter()
                .all(|node| source.get(node.byte_range()).is_some())
        );
        let next = reused.parse(FUNCTIONS, None).unwrap();
        assert_eq!(signature(&next), signature(&expected), "after {source}");
    }
}

#[test]
fn matlab_incremental_edits_match_fresh_trees() {
    for newline in ["\n", "\r\n"] {
        let source = FUNCTIONS.replace('\n', newline);
        for (before, after) in [
            ("λ😀", "δ🌍"),
            ("+ 1", "+ 23"),
            ("values, scale", "values, factor"),
            ("...", "... continued"),
        ] {
            let mut reused = parser();
            let mut old = reused.parse(&source, None).unwrap();
            let start = source.find(before).unwrap();
            let mut changed = source.clone();
            changed.replace_range(start..start + before.len(), after);
            old.edit(&InputEdit {
                start_byte: start,
                old_end_byte: start + before.len(),
                new_end_byte: start + after.len(),
                start_position: point(&source, start),
                old_end_position: point(&source, start + before.len()),
                new_end_position: point(&changed, start + after.len()),
            });
            let incremental = reused.parse(&changed, Some(&old)).unwrap();
            let fresh = valid(&changed);
            assert_eq!(
                signature(&incremental),
                signature(&fresh),
                "{before} {newline:?}"
            );
        }
    }
}

#[test]
fn matlab_included_ranges_keep_host_byte_coordinates() {
    let prefix = "# Host λ😀\r\n```matlab\r\n";
    let source = format!("{prefix}{FUNCTIONS}\r\n```\r\nHost text");
    let start = prefix.len();
    let end = start + FUNCTIONS.len();
    let mut parser = parser();
    parser
        .set_included_ranges(&[tree_sitter::Range {
            start_byte: start,
            end_byte: end,
            start_point: point(&source, start),
            end_point: point(&source, end),
        }])
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), start..end);
    assert_eq!(
        names(&tree, &source, "function_definition"),
        ["summarize", "adjust", "identity"]
    );
    assert!(
        nodes(tree.root_node())
            .iter()
            .all(|node| node.start_byte() >= start
                && node.end_byte() <= end
                && source.get(node.byte_range()).is_some())
    );
}

#[test]
fn matlab_cancellation_then_reset_preserves_fresh_parse() {
    let source = "value = 1;\n".repeat(10_000);
    let mut reused = parser();
    let mut called = false;
    let mut cancel = |_state: &tree_sitter::ParseState| {
        called = true;
        std::ops::ControlFlow::Break(())
    };
    let options = ParseOptions::new().progress_callback(&mut cancel);
    let result = reused.parse_with_options(
        &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
        None,
        Some(options),
    );
    assert!(result.is_none());
    assert!(called);
    reused.reset();
    assert_eq!(
        signature(&reused.parse(FUNCTIONS, None).unwrap()),
        signature(&valid(FUNCTIONS))
    );
}

#[test]
fn matlab_command_and_matrix_continuations_keep_following_statements() {
    for newline in ["\n", "\r\n"] {
        for source in [
            "disp 'λ😀';\nvalue = 1;\n",
            "!echo fixture\nvalue = 1;\n",
            "values = [1 ...\n2];\nvalue = 1;\n",
            "values = {1 ...\n2};\nvalue = 1;\n",
            "disp first ...\nsecond\nvalue = 1;\n",
        ] {
            let source = source.replace('\n', newline);
            let tree = valid(&source);
            let assignments: Vec<_> = nodes(tree.root_node())
                .into_iter()
                .filter(|node| node.kind() == "assignment")
                .map(|node| {
                    node.child_by_field_name("left")
                        .unwrap()
                        .utf8_text(source.as_bytes())
                        .unwrap()
                })
                .collect();
            assert_eq!(assignments.last(), Some(&"value"), "{source}");
        }
    }
}

#[test]
fn matlab_long_identifier_tokens_are_not_split_or_truncated() {
    for length in [254, 255, 256, 257, 1024] {
        let name = "x".repeat(length);
        let source = format!("function output = {name}(value)\noutput = value;\nend\n");
        let tree = valid(&source);
        assert_eq!(names(&tree, &source, "function_definition"), [name]);
    }
}

#[test]
fn matlab_consecutive_comments_use_bounded_native_stack() {
    const CHILD: &str = "ROOTLIGHT_MATLAB_COMMENT_STACK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let source = format!("{}value = 1;\n", "% comment\n".repeat(8_000));
                let tree = valid(&source);
                assert_eq!(
                    nodes(tree.root_node())
                        .iter()
                        .filter(|node| node.kind() == "assignment")
                        .count(),
                    1
                );
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    // A native stack overflow aborts the process, so its negative control is isolated.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "matlab_consecutive_comments_use_bounded_native_stack",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
