//! Native Julia contracts preserve authored syntax, scanner content and edits.
//! These tests never evaluate Julia code or establish runtime binding semantics.

use std::{ops::ControlFlow, ops::Range};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

fn parser() -> Parser {
    let mut parser = Parser::new();
    let language = tree_sitter_julia::LANGUAGE_JULIA.into();
    parser.set_language(&language).unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut output = Vec::new();
    while let Some(node) = pending.pop() {
        output.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    output.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    output
}

fn signature(tree: &Tree) -> Vec<(String, Range<usize>, bool, bool)> {
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

fn valid(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    assert!(
        nodes(tree.root_node())
            .iter()
            .all(|node| source.get(node.byte_range()).is_some())
    );
    tree
}

fn selected<'a>(tree: &'a Tree, source: &'a str, kind: &str) -> Vec<&'a str> {
    nodes(tree.root_node())
        .into_iter()
        .filter(|node| node.kind() == kind)
        .map(|node| &source[node.byte_range()])
        .collect()
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix.rfind('\n').map_or(offset, |last| offset - last - 1),
    )
}

#[test]
fn typed_methods_types_modules_and_quoted_context_keep_physical_heads() {
    let language: tree_sitter::Language = tree_sitter_julia::LANGUAGE_JULIA.into();
    assert_eq!(language.abi_version(), 15);
    let source = "module Outer\nabstract type Parent end\nprimitive type Byte <: Parent 8 end\nmutable struct Box{T} <: Parent\n value::T\n Box(x::T) where T = new{T}(x)\nend\nfunction Base.take!(b::Box{T}) where T\n b.value\nend\nι(x) = x\n⊗(x, y) = x * y\nmacro identity(ex)\n esc(ex)\nend\nbaremodule Inner end\nquote\n function quoted() end\nend\nend\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "abstract_definition"),
        ["abstract type Parent end"]
    );
    assert_eq!(
        selected(&tree, source, "primitive_definition"),
        ["primitive type Byte <: Parent 8 end"]
    );
    assert_eq!(
        selected(&tree, source, "struct_definition"),
        ["mutable struct Box{T} <: Parent\n value::T\n Box(x::T) where T = new{T}(x)\nend"]
    );
    assert_eq!(
        selected(&tree, source, "function_definition"),
        [
            "function Base.take!(b::Box{T}) where T\n b.value\nend",
            "function quoted() end"
        ]
    );
    for head in [
        "Box(x::T) where T = new{T}(x)",
        "ι(x) = x",
        "⊗(x, y) = x * y",
    ] {
        assert!(selected(&tree, source, "assignment").contains(&head));
    }
    assert_eq!(
        selected(&tree, source, "macro_definition"),
        ["macro identity(ex)\n esc(ex)\nend"]
    );
    let all = nodes(tree.root_node());
    let quoted = all
        .iter()
        .find(|node| {
            node.kind() == "function_definition"
                && &source[node.byte_range()] == "function quoted() end"
        })
        .unwrap();
    assert!(
        std::iter::successors(quoted.parent(), |node| node.parent())
            .any(|node| node.kind() == "quote_statement")
    );
    let methods = all
        .iter()
        .find(|node| node.kind() == "struct_definition")
        .unwrap();
    assert!(all.iter().any(|node| node.kind() == "assignment"
        && node.parent().and_then(|parent| parent.parent()) == Some(*methods)));
}

#[test]
fn nested_comments_shell_wrapper_and_literal_decoys_remain_source_content() {
    let source = "#!/bin/bash\n#=\nexec julia \"$0\" \"$@\"\n#= nested =#\n=#\n# function line_decoy() end\ndoc = \"\"\"function string_decoy() end λ😀\"\"\"\nfunction real(x)\n x\nend\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "block_comment"),
        ["#=\nexec julia \"$0\" \"$@\"\n#= nested =#\n=#"]
    );
    assert_eq!(
        selected(&tree, source, "function_definition"),
        ["function real(x)\n x\nend"]
    );
    assert_eq!(
        selected(&tree, source, "string_literal"),
        ["\"\"\"function string_decoy() end λ😀\"\"\""]
    );
}

#[test]
fn string_and_command_scanner_ranges_keep_interpolation_and_raw_text() {
    let source = "plain = \"λ😀 $(name)\"\nraw_text = raw\"$not_interpolated \\\" retained\"\ntriple = \"\"\"first\r\n$(name) last\"\"\"\ncommand = `echo $(name) λ😀`\nraw_command = custom`$literal`\n";
    let tree = valid(source);
    for literal in [
        "\"λ😀 $(name)\"",
        "\"\"\"first\r\n$(name) last\"\"\"",
        "`echo $(name) λ😀`",
    ] {
        assert!(
            nodes(tree.root_node())
                .iter()
                .any(|node| source.get(node.byte_range()) == Some(literal))
        );
    }
    assert_eq!(
        selected(&tree, source, "string_interpolation"),
        ["$(name)", "$(name)", "$(name)"]
    );
    assert!(
        !nodes(tree.root_node())
            .iter()
            .any(|node| node.kind() == "identifier"
                && matches!(&source[node.byte_range()], "not_interpolated" | "literal"))
    );
}

#[test]
fn bom_and_utf16_endian_inputs_keep_original_byte_coordinates() {
    let text = "# λ😀\nfunction ι(x)\n x\nend\n";
    let written = "function ι(x)\n x\nend";
    for bom in [false, true] {
        let source = if bom {
            format!("\u{feff}{text}")
        } else {
            text.to_owned()
        };
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "function_definition"), [written]);
        for big_endian in [false, true] {
            let units: Vec<u16> = source
                .encode_utf16()
                .map(|unit| {
                    if big_endian {
                        unit.to_be()
                    } else {
                        unit.to_le()
                    }
                })
                .collect();
            let raw: Vec<u8> = units.iter().flat_map(|unit| unit.to_ne_bytes()).collect();
            let tree = if big_endian {
                parser().parse_utf16_be(&units, None)
            } else {
                parser().parse_utf16_le(&units, None)
            }
            .unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            let all = nodes(tree.root_node());
            let method = all
                .iter()
                .find(|node| node.kind() == "function_definition")
                .unwrap();
            let start = source[..source.find(written).unwrap()]
                .encode_utf16()
                .count()
                * 2;
            assert_eq!(
                method.byte_range(),
                start..start + written.encode_utf16().count() * 2
            );
            let expected: Vec<u8> = written
                .encode_utf16()
                .flat_map(|unit| {
                    if big_endian {
                        unit.to_be_bytes()
                    } else {
                        unit.to_le_bytes()
                    }
                })
                .collect();
            assert_eq!(&raw[method.byte_range()], expected);
            assert!(all.iter().all(|node| node.start_byte() % 2 == 0
                && node.end_byte() % 2 == 0
                && node.end_byte() <= raw.len()));
        }
    }
}

#[test]
fn incremental_delimiter_and_definition_edits_match_cold_ranges() {
    for (before, removed, replacement) in [
        (
            "#= nested #= retained =# =#\nfunction λ(x)\n x\nend\n",
            "=# =#",
            "=#",
        ),
        (
            "value = \"\"\"λ😀\"\"\"\nfunction next() end\n",
            "\"\"\"\nfunction",
            "\"\"\nfunction",
        ),
        ("function original(x)\n x\nend\n", "original", "ι"),
        ("function recovered(x)\n x\n", "x\n", "x\nend\n"),
    ] {
        let start = before.rfind(removed).unwrap();
        let after = format!(
            "{}{}{}",
            &before[..start],
            replacement,
            &before[start + removed.len()..]
        );
        let mut reused = parser();
        let mut previous = reused.parse(before, None).unwrap();
        previous.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + removed.len(),
            new_end_byte: start + replacement.len(),
            start_position: point(before, start),
            old_end_position: point(before, start + removed.len()),
            new_end_position: point(&after, start + replacement.len()),
        });
        let incremental = reused.parse(&after, Some(&previous)).unwrap();
        let cold = parser().parse(&after, None).unwrap();
        assert_eq!(signature(&incremental), signature(&cold), "{after}");
        assert_eq!(
            incremental.root_node().to_sexp(),
            cold.root_node().to_sexp()
        );
    }
}

#[test]
fn malformed_input_retains_errors_and_cancellation_can_reset() {
    for source in [
        "#= unclosed λ😀",
        "text = \"\"\"unclosed λ😀",
        "function missing(x)\n x\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}\n{}",
            tree.root_node().to_sexp()
        );
        assert!(
            nodes(tree.root_node())
                .iter()
                .all(|node| source.get(node.byte_range()).is_some())
        );
    }
    let source = "value = \"λ😀\"\n".repeat(10_000);
    let mut reused = parser();
    let mut called = false;
    let mut cancel = |_state: &tree_sitter::ParseState| {
        called = true;
        ControlFlow::Break(())
    };
    let result = reused.parse_with_options(
        &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut cancel)),
    );
    assert!(result.is_none());
    assert!(called);
    reused.reset();
    assert_eq!(
        signature(&reused.parse("function recovered() end", None).unwrap()),
        signature(&valid("function recovered() end"))
    );
}
