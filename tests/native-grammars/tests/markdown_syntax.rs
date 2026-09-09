//! Markdown block/inline source boundaries before production registration.
//! These tests keep embedded regions in their original document coordinates.

use std::ops::ControlFlow;

use tree_sitter::{InputEdit, Language, Node, ParseOptions, Parser, Point, Range, Tree};

fn parser(inline: bool) -> Parser {
    let language: Language = if inline {
        tree_sitter_md::INLINE_LANGUAGE.into()
    } else {
        tree_sitter_md::LANGUAGE.into()
    };
    assert!((13..=15).contains(&language.abi_version()));
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
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

fn point(source: &str, offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn shape(tree: &Tree, source: &str) -> Vec<(String, usize, usize, Point, Point)> {
    nodes(tree.root_node())
        .into_iter()
        .map(|node| {
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
            (
                node.kind().to_owned(),
                node.start_byte(),
                node.end_byte(),
                node.start_position(),
                node.end_position(),
            )
        })
        .collect()
}

#[test]
fn markdown_blocks_retain_written_headings_links_and_fence_bodies() {
    let source = "# Guide 雪\r\n\r\nIntroduction [entry][target].\r\n\r\nSection\r\n-------\r\n\r\n```javascript title=example\r\nfunction sample() { return 1; }\r\n```\r\n\r\n[target]: ./module.js \"Module\"\r\n";
    let tree = parser(false).parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let all = nodes(tree.root_node());
    for (kind, text) in [
        ("atx_heading", "# Guide 雪\r\n"),
        ("setext_heading", "Section\r\n-------\r\n"),
        ("language", "javascript"),
        ("code_fence_content", "function sample() { return 1; }\r\n"),
        ("link_destination", "./module.js"),
    ] {
        assert!(
            all.iter()
                .any(|node| node.kind() == kind
                    && node.utf8_text(source.as_bytes()).unwrap() == text),
            "missing {kind}: {text:?}; {}",
            tree.root_node().to_sexp()
        );
    }
    shape(&tree, source);
}

#[test]
fn markdown_inline_ranges_exclude_container_markers_without_rewriting_source() {
    let source = "> Paragraph **bold\n> continuation** and [link](./entry.rs).\n";
    let tree = parser(false).parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let inline = nodes(tree.root_node())
        .into_iter()
        .find(|node| node.kind() == "inline")
        .unwrap();
    let mut ranges = Vec::new();
    let mut start = inline.start_byte();
    let mut cursor = inline.walk();
    for child in inline.named_children(&mut cursor) {
        if start < child.start_byte() {
            ranges.push(Range {
                start_byte: start,
                end_byte: child.start_byte(),
                start_point: point(source, start),
                end_point: child.start_position(),
            });
        }
        start = child.end_byte();
    }
    if start < inline.end_byte() {
        ranges.push(Range {
            start_byte: start,
            end_byte: inline.end_byte(),
            start_point: point(source, start),
            end_point: inline.end_position(),
        });
    }
    assert_eq!(ranges.len(), 2);
    let mut inline_parser = parser(true);
    inline_parser.set_included_ranges(&ranges).unwrap();
    let parsed = inline_parser.parse(source, None).unwrap();
    assert!(!parsed.root_node().has_error());
    let all = nodes(parsed.root_node());
    assert!(all.iter().any(|node| node.kind() == "strong_emphasis"));
    assert!(all.iter().any(|node| node.kind() == "link_destination"
        && node.utf8_text(source.as_bytes()).unwrap() == "./entry.rs"));
    shape(&parsed, source);
}

#[test]
fn markdown_long_fences_do_not_wrap_or_close_at_shorter_runs() {
    for length in [3, 127, 255, 256, 257, 511, 1024] {
        for delimiter in ['`', '~'] {
            let fence = delimiter.to_string().repeat(length);
            let shorter = delimiter.to_string().repeat(length - 1);
            let body = format!("before\n{shorter}\nafter\n");
            let source = format!("{fence}javascript\n{body}{fence}\n\n# outside\n");
            let tree = parser(false).parse(&source, None).unwrap();
            assert!(!tree.root_node().has_error(), "{delimiter}:{length}");
            let all = nodes(tree.root_node());
            let bodies: Vec<_> = all
                .iter()
                .filter(|node| node.kind() == "code_fence_content")
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect();
            assert_eq!(bodies, [body.as_str()], "{delimiter}:{length}");
            assert_eq!(
                all.iter()
                    .filter(|node| node.kind() == "atx_heading")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn markdown_long_code_spans_keep_matching_delimiter_lengths() {
    for length in [1, 127, 255, 256, 257, 511, 1024] {
        let fence = "`".repeat(length);
        let source = format!("before {fence}value{fence} after");
        let tree = parser(true).parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let all = nodes(tree.root_node());
        let spans: Vec<_> = all
            .iter()
            .filter(|node| node.kind() == "code_span")
            .map(|node| node.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(spans, [format!("{fence}value{fence}")], "{length}");
        shape(&tree, &source);
    }
}

#[test]
fn markdown_incremental_fence_edits_match_clean_boundaries() {
    for length in [3, 255, 256, 1024] {
        let fence = "`".repeat(length);
        let source = format!(
            "# Guide 雪\r\n\r\n{fence}rust\r\nfn first() {{}}\r\n{fence}\r\n\r\n[entry](./a.rs)\r\n"
        );
        let changed = source.replace("first", "second_longer");
        let start = source.find("first").unwrap();
        let mut parser = parser(false);
        let mut old = parser.parse(&source, None).unwrap();
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 5,
            new_end_byte: start + 13,
            start_position: point(&source, start),
            old_end_position: point(&source, start + 5),
            new_end_position: point(&changed, start + 13),
        });
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
        let clean = parser.parse(&changed, None).unwrap();
        assert_eq!(
            shape(&incremental, &changed),
            shape(&clean, &changed),
            "{length}"
        );
        assert!(!clean.root_node().has_error());
    }
}

#[test]
fn markdown_large_indentation_does_not_turn_code_into_a_heading() {
    for width in [4, 127, 255, 256, 257, 1024] {
        let source = format!("{}# literal\n\n# outside\n", " ".repeat(width));
        let tree = parser(false).parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error(), "{width}");
        let all = nodes(tree.root_node());
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "indented_code_block")
                .count(),
            1,
            "{width}"
        );
        let headings: Vec<_> = all
            .iter()
            .filter(|node| node.kind() == "atx_heading")
            .map(|node| node.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(headings, ["# outside\n"], "{width}");
    }
}

#[test]
fn markdown_deep_containers_keep_complete_state_or_report_parse_failure() {
    for depth in [1, 64, 128, 255, 256, 512, 1002] {
        let source = format!("{}entry\n", "> ".repeat(depth));
        let mut parser = parser(false);
        let mut polls = 0;
        let mut progress = |_: &tree_sitter::ParseState| {
            polls += 1;
            if polls > 10_000 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut input =
            |offset: usize, _: Point| source.as_bytes().get(offset..).unwrap_or_default();
        let tree = parser
            .parse_with_options(
                &mut input,
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
            .expect("bounded nesting probe must terminate without cancellation");
        if depth <= 512 {
            assert!(!tree.root_node().has_error(), "{depth}");
            assert_eq!(
                nodes(tree.root_node())
                    .iter()
                    .filter(|node| node.kind() == "block_quote")
                    .count(),
                depth
            );
        } else {
            assert!(
                tree.root_node().has_error(),
                "native state exhaustion cannot claim full structure"
            );
        }
        parser.reset();
        let next = parser.parse("# recovered\n", None).unwrap();
        assert!(!next.root_node().has_error());
    }
}

#[test]
fn markdown_included_inline_edits_match_clean_ranges_and_coordinates() {
    for width in [1, 255, 256, 257, 1024] {
        let fence = "`".repeat(width);
        let source = format!(
            "# Host 雪\r\n\r\nleft **bold** and {fence}entry{fence} [link](./a.rs)\r\n\r\n# Tail\r\n"
        );
        let changed = source.replace("entry", "longer_entry");
        let start = source.find("entry").unwrap();
        let host_start = source.find("left").unwrap();
        let host_end = source.find("\r\n\r\n# Tail").unwrap();
        let mut parser = parser(true);
        parser
            .set_included_ranges(&[Range {
                start_byte: host_start,
                end_byte: host_end,
                start_point: point(&source, host_start),
                end_point: point(&source, host_end),
            }])
            .unwrap();
        let mut old = parser.parse(&source, None).unwrap();
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 5,
            new_end_byte: start + 12,
            start_position: point(&source, start),
            old_end_position: point(&source, start + 5),
            new_end_position: point(&changed, start + 12),
        });
        parser
            .set_included_ranges(&[Range {
                start_byte: host_start,
                end_byte: host_end + 7,
                start_point: point(&changed, host_start),
                end_point: point(&changed, host_end + 7),
            }])
            .unwrap();
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
        let clean = parser.parse(&changed, None).unwrap();
        assert!(!clean.root_node().has_error());
        assert_eq!(
            shape(&incremental, &changed),
            shape(&clean, &changed),
            "{width}"
        );
        assert!(
            nodes(clean.root_node())
                .iter()
                .any(|node| node.kind() == "code_span"
                    && node.utf8_text(changed.as_bytes()).unwrap()
                        == format!("{fence}longer_entry{fence}"))
        );
    }
}
