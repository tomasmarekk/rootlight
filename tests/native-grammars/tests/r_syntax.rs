//! R syntax stays source-exact across raw strings and contextual newlines.
//! Native parser checks do not imply symbol resolution or execute package code.

use tree_sitter::{InputEdit, Node, Parser, Point, Query};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .unwrap();
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
    output
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rfind('\n')
        .map_or(prefix.len(), |last| prefix.len() - last - 1);
    Point::new(row, column)
}

#[test]
fn r_unicode_codepoints_cannot_alias_ascii_raw_string_delimiters() {
    for source in [
        "x <- r\u{122}(value)\"",
        "x <- r\"\u{128}value)\"",
        "x <- r\"\u{15b}value]\"",
        "x <- r\"\u{17b}value}\"",
    ] {
        let tree = parser().parse(source, None).unwrap();
        // The upstream grammar may recover as adjacent expressions. The scanner must
        // never turn the non-ASCII code point into an ASCII raw-string opener.
        for node in nodes(tree.root_node())
            .into_iter()
            .filter(|node| node.kind() == "string_open")
        {
            let opener = node.utf8_text(source.as_bytes()).unwrap();
            assert!(!opener.starts_with(['r', 'R']), "{source}: {opener}");
        }
    }
}

#[test]
fn r_language_constructs_preserve_complete_byte_extents() {
    let language = tree_sitter_r::LANGUAGE.into();
    for query in [
        tree_sitter_r::HIGHLIGHTS_QUERY,
        tree_sitter_r::TAGS_QUERY,
        tree_sitter_r::LOCALS_QUERY,
    ] {
        Query::new(&language, query).unwrap();
    }
    for source in [
        "  \r\nidentity <- function(value, ...) { value }\n",
        "identity = function(value = 1) value\n",
        "function(value) value -> identity\n",
        "identity <<- function(value) value\n",
        "short <- \\(value) value + 1\n",
        "`with spaces` <- function(`first arg`) `first arg`\n",
        "{ if (TRUE) 1\n# context\nelse 2 }\n",
        "values[[list(1,\n2)[[1]]]]\n",
        "data |> transform(value = sqrt(value))\n",
        "stats::median(values, na.rm = TRUE)\n",
        "setClass('Record', slots = c(value = 'numeric'))\n",
        "setMethod('show', 'Record', function(object) print(object@value))\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(tree.root_node().byte_range(), 0..source.len());
        for node in nodes(tree.root_node()) {
            assert!(source.get(node.byte_range()).is_some());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
        }
    }
}

#[test]
fn r_raw_strings_keep_delimiters_unicode_overlaps_and_nul_bytes() {
    for (prefix, quote, open, close) in [
        ('r', '"', '(', ')'),
        ('R', '\'', '[', ']'),
        ('r', '"', '{', '}'),
    ] {
        for count in [0, 1, 255] {
            let dashes = "-".repeat(count);
            for body in ["", "  value\nπ Ж 💡", "one\0two", ")-x)-", "]]]", "}}x"] {
                let literal = format!("{prefix}{quote}{dashes}{open}{body}{close}{dashes}{quote}");
                let source = format!("value <- {literal}\n");
                let tree = parser().parse(&source, None).unwrap();
                assert!(
                    !tree.root_node().has_error(),
                    "{source:?}: {}",
                    tree.root_node().to_sexp()
                );
                let strings: Vec<_> = nodes(tree.root_node())
                    .into_iter()
                    .filter(|node| node.kind() == "string")
                    .collect();
                assert_eq!(strings.len(), 1);
                assert_eq!(strings[0].utf8_text(source.as_bytes()).unwrap(), literal);
            }
        }
    }
}

#[test]
fn r_incremental_edits_match_every_fresh_node_and_source_span() {
    for initial in [
        "value <- r\"--(one)--\"\n",
        "value <- function(x) {\n if (x) one\n else 2\n}\n",
        "value <- list(a = list(one))[[1]][[1]]\n",
        "value <- function(one = 1) one\n",
    ] {
        let mut parser = parser();
        let mut source = initial.to_owned();
        let mut tree = parser.parse(&source, None).unwrap();
        for (old, new) in [
            ("one", "π +\n1"),
            ("π +\n1", "one"),
            ("one", ""),
            ("", "one"),
        ] {
            let start = if old.is_empty() {
                initial.find("one").unwrap()
            } else {
                source.find(old).unwrap()
            };
            let old_end = start + old.len();
            let start_position = point(&source, start);
            let old_end_position = point(&source, old_end);
            source.replace_range(start..old_end, new);
            tree.edit(&InputEdit {
                start_byte: start,
                old_end_byte: old_end,
                new_end_byte: start + new.len(),
                start_position,
                old_end_position,
                new_end_position: point(&source, start + new.len()),
            });
            let incremental = parser.parse(&source, Some(&tree)).unwrap();
            let fresh = parser.parse(&source, None).unwrap();
            let snapshot = |root| {
                nodes(root)
                    .into_iter()
                    .map(|node| {
                        (
                            node.kind().to_owned(),
                            node.byte_range(),
                            node.start_position(),
                            node.end_position(),
                            node.is_missing(),
                            node.is_error(),
                            node.is_named(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                snapshot(incremental.root_node()),
                snapshot(fresh.root_node()),
                "{source}"
            );
            tree = incremental;
        }
    }
}
