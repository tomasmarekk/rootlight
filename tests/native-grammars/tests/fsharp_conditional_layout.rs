//! Neutral conditional-layout regressions, not whole-language acceptance.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};
use tree_sitter::{Node, ParseOptions, Parser, Tree};

fn parse(signature: bool, source: &str) -> Tree {
    let mut parser = Parser::new();
    let language = if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    };
    parser.set_language(&language.into()).unwrap();
    let start = Instant::now();
    let mut watchdog = |_: &tree_sitter::ParseState| {
        if start.elapsed() > Duration::from_secs(10) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("authored fixture exceeded its diagnostic watchdog")
}

fn nodes<'a>(tree: &'a Tree, kind: &str) -> Vec<Node<'a>> {
    let mut pending = vec![tree.root_node()];
    let mut selected = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            selected.push(node);
        }
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    selected
}

fn clean(tree: &Tree, source: &str) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().end_byte(), source.len());
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
}

fn variants(fixture: &str, mut check: impl FnMut(&str)) {
    for crlf in [false, true] {
        for final_newline in [false, true] {
            let source = if final_newline {
                fixture.to_owned()
            } else {
                fixture.trim_end_matches('\n').to_owned()
            };
            let source = if crlf {
                source.replace('\n', "\r\n")
            } else {
                source
            };
            check(&source);
        }
    }
}

#[test]
fn structured_branches_keep_same_column_declaration_siblings() {
    for signature in [false, true] {
        let first = if signature {
            "val seed: int"
        } else {
            "let seed = 1"
        };
        let last = if signature {
            "val tail: int"
        } else {
            "let tail = 2"
        };
        for indent in ["", "    "] {
            let header = if indent.is_empty() {
                "module Example\n"
            } else {
                "module Example =\n"
            };
            let fixture = format!(
                "{header}#if ACTIVE\n// Leading branch comment.\n{indent}{first}\n{indent}type Record = {{ Value: int }}\n{indent}{last}\n#endif\n{indent}type Following = int\n"
            );
            variants(&fixture, |source| {
                let tree = parse(signature, source);
                clean(&tree, source);
                assert_eq!(nodes(&tree, "preproc_if").len(), 1);
                assert_eq!(nodes(&tree, "record_type_defn").len(), 1);
                assert_eq!(nodes(&tree, "type_abbrev_defn").len(), 1);
                let kind = if signature {
                    "value_definition"
                } else {
                    "function_or_value_defn"
                };
                let declarations = nodes(&tree, kind);
                assert_eq!(declarations.len(), 2);
                for written in [first, last] {
                    assert!(
                        declarations
                            .iter()
                            .any(|node| source.get(node.byte_range()) == Some(written))
                    );
                }
            });
        }
    }
}

#[test]
fn conditional_members_keep_attributed_same_column_siblings() {
    for signature in [false, true] {
        let first = if signature {
            "member First: int -> int"
        } else {
            "member this.First value = value"
        };
        let last = if signature {
            "member Last: int -> int"
        } else {
            "member this.Last value = value"
        };
        let name = if signature {
            "type Container ="
        } else {
            "type Container() ="
        };
        let fixture = format!(
            "module Example\n{name}\n#if ACTIVE\n    {first}\n    [<System.Obsolete(\"Example\")>]\n    {last}\n#endif\ntype Following = int\n"
        );
        variants(&fixture, |source| {
            let tree = parse(signature, source);
            clean(&tree, source);
            assert_eq!(nodes(&tree, "preproc_if").len(), 1);
            assert_eq!(nodes(&tree, "attribute").len(), 1);
            assert_eq!(nodes(&tree, "member_defn").len(), 2);
            assert_eq!(nodes(&tree, "type_abbrev_defn").len(), 1);
        });
    }
}

#[test]
fn nested_conditionals_do_not_close_the_outer_branch_or_steal_following_source() {
    for signature in [false, true] {
        let values = if signature {
            [
                "val first: int",
                "val nested: int",
                "val alternative: int",
                "val last: int",
            ]
        } else {
            [
                "let first = 1",
                "let nested = 2",
                "let alternative = 3",
                "let last = 4",
            ]
        };
        let fixture = format!(
            "module Example\n#if OUTER\n{}\n#if INNER\n{}\n#endif\n{}\n#else\n{}\n#endif\ntype Following = int\n",
            values[0], values[1], values[3], values[2]
        );
        variants(&fixture, |source| {
            let tree = parse(signature, source);
            clean(&tree, source);
            assert_eq!(nodes(&tree, "type_abbrev_defn").len(), 1);
            let kind = if signature {
                "value_definition"
            } else {
                "function_or_value_defn"
            };
            let declarations = nodes(&tree, kind);
            assert_eq!(declarations.len(), 4);
            for written in values {
                let nested_with_close;
                let expected = if !signature && written == values[1] {
                    // The native extra attaches to this declaration, but never
                    // becomes part of its initializer or the next declaration.
                    let newline = if source.contains("\r\n") {
                        "\r\n"
                    } else {
                        "\n"
                    };
                    nested_with_close = format!("{written}{newline}#endif");
                    nested_with_close.as_str()
                } else {
                    written
                };
                assert!(
                    declarations
                        .iter()
                        .any(|node| source.get(node.byte_range()) == Some(expected)),
                    "signature={signature}, written={written:?}: {}",
                    tree.root_node().to_sexp()
                );
            }
            assert_eq!(nodes(&tree, "preproc_if").len(), 1);
            assert_eq!(nodes(&tree, "preproc_else").len(), 1);
            let trivia = nodes(&tree, "preproc_inactive");
            assert_eq!(trivia.len(), 2);
            let inner_header = if source.contains("\r\n") {
                "#if INNER\r"
            } else {
                "#if INNER"
            };
            assert_eq!(source.get(trivia[0].byte_range()), Some(inner_header));
            assert_eq!(source.get(trivia[1].byte_range()), Some("#endif"));
            if !signature {
                let initializer = nodes(&tree, "const")
                    .into_iter()
                    .find(|node| source.get(node.byte_range()) == Some("2"))
                    .unwrap();
                assert_eq!(initializer.parent(), Some(trivia[1].parent().unwrap()));
            }
        });
    }
}
