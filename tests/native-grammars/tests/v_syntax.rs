//! Native V contracts bind exact static ABI, physical syntax and parser state.
//! Native trees do not establish compiler evaluation or resolved declarations.

use tree_sitter::{InputEdit, Parser, Point, Tree};

#[derive(Debug, PartialEq, Eq)]
struct Shape {
    kind: u16,
    range: std::ops::Range<usize>,
    points: (Point, Point),
    depth: usize,
    field: Option<String>,
    named: bool,
    missing: bool,
    error: bool,
    extra: bool,
}

fn shapes(tree: &Tree) -> Vec<Shape> {
    let mut output = Vec::new();
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        let node = cursor.node();
        output.push(Shape {
            kind: node.kind_id(),
            range: node.byte_range(),
            points: (node.start_position(), node.end_position()),
            depth,
            field: cursor.field_name().map(str::to_owned),
            named: node.is_named(),
            missing: node.is_missing(),
            error: node.is_error(),
            extra: node.is_extra(),
        });
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return output;
            }
            depth -= 1;
        }
    }
}

fn point(bytes: &[u8], offset: usize) -> Point {
    let prefix = &bytes[..offset];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(offset, |last| offset - last - 1),
    )
}

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_v::LANGUAGE_V.into())
        .unwrap();
    parser
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;
    use tree_sitter::ParseOptions;

    #[test]
    fn runtime_accepts_exact_v_language_abi_and_fields() {
        let language: tree_sitter::Language = tree_sitter_v::LANGUAGE_V.into();
        assert_eq!(language.abi_version(), 15);
        assert_eq!(language.field_count(), 47);
        assert_eq!(language.name(), Some("v"));
        parser();
    }

    #[test]
    fn written_v_constructs_have_exact_native_nodes() {
        for (source, required) in [
            (
                "module sample\nimport os\nfn add(a int, b int) int { return a + b }",
                vec![
                    "module_clause",
                    "import_declaration",
                    "function_declaration",
                    "parameter_declaration",
                ],
            ),
            (
                "struct Box { value int }\nfn (box Box) read() int { return box.value }",
                vec![
                    "struct_declaration",
                    "struct_field_declaration",
                    "function_declaration",
                ],
            ),
            (
                "interface Reader { read() int }\nenum Color { red blue }\ntype Count = int",
                vec![
                    "interface_declaration",
                    "enum_declaration",
                    "type_declaration",
                ],
            ),
            (
                "fn identity[T](value T) T { return value }\nfn main() { first, second := 1, 2; _ = first + second }",
                vec!["function_declaration", "var_declaration"],
            ),
            (
                "const limit = 10\n__global ( counter int )\nfn C.puts(&char) int",
                vec![
                    "const_declaration",
                    "global_var_declaration",
                    "function_declaration",
                ],
            ),
        ] {
            let mut parser = parser();
            let tree = parser.parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let language: tree_sitter::Language = tree_sitter_v::LANGUAGE_V.into();
            let nodes = shapes(&tree);
            for kind in required {
                assert!(
                    nodes
                        .iter()
                        .any(|node| language.node_kind_for_id(node.kind) == Some(kind)),
                    "missing {kind}: {}",
                    tree.root_node().to_sexp()
                );
            }
        }
    }

    #[test]
    fn invalid_bytes_and_incomplete_constructs_remain_visible() {
        for bytes in [
            &b"fn incomplete("[..],
            &b"struct Missing {"[..],
            &b"fn main() { value := '\xff' }"[..],
        ] {
            let tree = parser().parse(bytes, None).unwrap();
            if std::str::from_utf8(bytes).is_ok() {
                assert!(
                    tree.root_node().has_error(),
                    "{:?}: {}",
                    bytes,
                    tree.root_node().to_sexp()
                );
            } else {
                // A native string token can contain invalid bytes. Encoding validity
                // is a separate admission obligation, reported by the census.
                assert_eq!(std::str::from_utf8(bytes).unwrap_err().valid_up_to(), 22);
            }
            assert!(
                shapes(&tree)
                    .iter()
                    .all(|node| node.range.end <= bytes.len())
            );
        }
    }

    #[test]
    fn parsing_cancels_and_recovers_without_reusing_partial_state() {
        let source = "fn item() { value := 1 }\n".repeat(100_000);
        let mut parser = parser();
        let mut checks = 0;
        let mut progress = |_: &tree_sitter::ParseState| {
            checks += 1;
            ControlFlow::Break(())
        };
        let mut read = |offset: usize, _| source.as_bytes().get(offset..).unwrap_or_default();
        assert!(
            parser
                .parse_with_options(
                    &mut read,
                    None,
                    Some(ParseOptions::new().progress_callback(&mut progress))
                )
                .is_none()
        );
        assert!(checks > 0);
        parser.reset();
        let tree = parser.parse("fn recovered() {}", None).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn inline_globals_preserve_names_without_requiring_line_terminators() {
        for source in [
            "__global (counter int)",
            "__global (counter int other bool)",
            "__global (counter int; other bool;)",
            "__global (\n counter int\n other bool\n)",
            "__global (counter = 1)",
            "__global (volatile counter int)",
            "__global ()",
        ] {
            let tree = parser().parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let language: tree_sitter::Language = tree_sitter_v::LANGUAGE_V.into();
            let actual: Vec<_> = shapes(&tree)
                .into_iter()
                .filter(|node| {
                    language.node_kind_for_id(node.kind) == Some("global_var_definition")
                })
                .map(|node| source[node.range].trim().to_owned())
                .collect();
            let expected =
                usize::from(source.contains("counter")) + usize::from(source.contains("other"));
            assert_eq!(actual.len(), expected, "{source}");
        }
        for source in [
            "__global (counter)",
            "__global (counter =)",
            "__global (counter int other)",
            "__global (counter int",
            "__global (counter int, other bool)",
        ] {
            let tree = parser().parse(source, None).unwrap();
            assert!(tree.root_node().has_error(), "accepted malformed {source}");
        }
    }

    #[test]
    fn comment_boundaries_preserve_fresh_and_incremental_shapes() {
        let source = "interface MyInterface {\n}\nstruct St {\n}\n// these used to cause memory corruptions, but work with this PR:\nfn gen_interface() MyInterface {\n\tx := St{\n\t} // `x`will be allocated on heap\n\tassert d == 16.0\n}\n";
        let start = source.find("MyInterface").unwrap();
        let mut changed = source.as_bytes().to_vec();
        changed[start] = b'x';
        let original = parser().parse(source, None).unwrap();
        assert!(!original.root_node().has_error());
        let mut prior = original.clone();
        prior.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 1,
            new_end_byte: start + 1,
            start_position: point(source.as_bytes(), start),
            old_end_position: point(source.as_bytes(), start + 1),
            new_end_position: point(&changed, start + 1),
        });
        let incremental = parser().parse(&changed, Some(&prior)).unwrap();
        let fresh = parser().parse(&changed, None).unwrap();
        assert_eq!(shapes(&incremental), shapes(&fresh));
        let mut back = incremental;
        back.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 1,
            new_end_byte: start + 1,
            start_position: point(&changed, start),
            old_end_position: point(&changed, start + 1),
            new_end_position: point(source.as_bytes(), start + 1),
        });
        let restored = parser().parse(source, Some(&back)).unwrap();
        assert_eq!(shapes(&restored), shapes(&original));
    }

    #[test]
    fn script_semicolons_keep_complete_statement_spans() {
        for source in [
            "import os;\nprintln(os.args);\nprintln('hello');\n",
            "import os; import math; println(os.args.len); println(math.sin(math.pi / 2));",
            "fn main() {};",
        ] {
            let tree = parser().parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
    }

    #[test]
    fn defer_modes_preserve_following_function_declarations() {
        let language: tree_sitter::Language = tree_sitter_v::LANGUAGE_V.into();
        for source in [
            "fn first() { defer(fn) { done() } }\nfn tail() {}\n",
            "fn first() { defer { done() } }\nfn tail() {}\n",
            "fn first() { defer( /* mode */ fn ) { done() } }\nfn tail() {}\n",
        ] {
            let tree = parser().parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let nodes = shapes(&tree);
            let declarations: Vec<_> = nodes
                .iter()
                .filter(|node| language.node_kind_for_id(node.kind) == Some("function_declaration"))
                .map(|node| &source[node.range.clone()])
                .collect();
            assert_eq!(declarations.len(), 2, "{source}");
            assert_eq!(declarations[1], "fn tail() {}");
            assert_eq!(
                nodes
                    .iter()
                    .filter(|node| language.node_kind_for_id(node.kind) == Some("defer_statement"))
                    .count(),
                1
            );
        }
        for source in [
            "fn first() { defer(other) { done() } }\nfn tail() {}\n",
            "fn first() { defer() { done() } }\nfn tail() {}\n",
            "fn first() { defer(fn { done() } }\nfn tail() {}\n",
            "fn first() { defer(fn) }\nfn tail() {}\n",
        ] {
            assert!(
                parser()
                    .parse(source, None)
                    .unwrap()
                    .root_node()
                    .has_error(),
                "{source}"
            );
        }
    }
}

#[test]
fn edited_identifiers_and_restoration_match_complete_fresh_trees() {
    let source = "// λ\r\nmodule written\r\nfn first() { defer(fn) { done() }; value := 1 }\r\nfn tail() {}\r\n";
    let changed = source.replace("value", "long_value");
    let mut parser = parser();
    let original = parser.parse(source, None).unwrap();
    let mut previous = original.clone();
    previous.edit(&InputEdit {
        start_byte: 0,
        old_end_byte: source.len(),
        new_end_byte: changed.len(),
        start_position: Point::new(0, 0),
        old_end_position: point(source.as_bytes(), source.len()),
        new_end_position: point(changed.as_bytes(), changed.len()),
    });
    let mut edited = parser.parse(&changed, Some(&previous)).unwrap();
    assert_eq!(
        shapes(&edited),
        shapes(&parser.parse(&changed, None).unwrap())
    );
    edited.edit(&InputEdit {
        start_byte: 0,
        old_end_byte: changed.len(),
        new_end_byte: source.len(),
        start_position: Point::new(0, 0),
        old_end_position: point(changed.as_bytes(), changed.len()),
        new_end_position: point(source.as_bytes(), source.len()),
    });
    assert_eq!(
        shapes(&parser.parse(source, Some(&edited)).unwrap()),
        shapes(&original)
    );
}
