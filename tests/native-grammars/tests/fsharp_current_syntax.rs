//! Authored current F# syntax witnesses for both native grammar profiles.
//! This standalone integration-test candidate checks syntax and source ownership,
//! not type-checker admissibility or preprocessor condition evaluation.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};
use tree_sitter::{Node, ParseOptions, Parser, Point, Tree};

fn current_parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    let language = if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    };
    parser
        .set_language(&language.into())
        .expect("pinned native grammar is incompatible with Tree-sitter");
    parser
}

fn current_parse(signature: bool, source: &str) -> Tree {
    let started = Instant::now();
    let mut checks = 0_usize;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        checks += 1;
        if checks > 2_000 || started.elapsed() > Duration::from_secs(10) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    current_parser(signature)
        .parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("current syntax fixture exceeded its parse watchdog")
}

fn current_point(source: &str, offset: usize) -> Point {
    let prefix = source
        .as_bytes()
        .get(..offset)
        .expect("node offset exceeds authored fixture");
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn current_nodes<'tree>(tree: &'tree Tree, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![tree.root_node()];
    let mut found = Vec::new();
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        visited += 1;
        assert!(
            visited < 100_000,
            "authored node inventory exceeds its bound"
        );
        if node.kind() == kind {
            found.push(node);
        }
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("fixture child index exceeds native API");
            pending.push(node.child(index).expect("advertised child is absent"));
        }
    }
    found
}

fn current_clean(tree: &Tree, source: &str) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().end_byte(), source.len());
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    let mut pending = vec![tree.root_node()];
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        visited += 1;
        assert!(
            visited < 100_000,
            "authored node inventory exceeds its bound"
        );
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert_eq!(
            node.start_position(),
            current_point(source, node.start_byte())
        );
        assert_eq!(node.end_position(), current_point(source, node.end_byte()));
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("fixture child index exceeds native API");
            pending.push(node.child(index).expect("advertised child is absent"));
        }
    }
}

fn current_exact_nodes<'tree>(
    tree: &'tree Tree,
    source: &str,
    kind: &str,
    written: &str,
) -> Vec<Node<'tree>> {
    let found: Vec<_> = current_nodes(tree, kind)
        .into_iter()
        .filter(|node| source.get(node.byte_range()) == Some(written))
        .collect();
    assert!(
        !found.is_empty(),
        "{kind} {written:?}: candidates {:?}; {}",
        current_nodes(tree, kind)
            .iter()
            .map(|node| (node.byte_range(), source.get(node.byte_range())))
            .collect::<Vec<_>>(),
        tree.root_node().to_sexp()
    );
    found
}

fn current_variants(fixture: &str, mut witness: impl FnMut(&str)) {
    let without_final_newline = fixture
        .strip_suffix('\n')
        .expect("authored fixture must have a final newline");
    for newline in ["\n", "\r\n"] {
        for text in [without_final_newline, fixture] {
            witness(&text.replace('\n', newline));
        }
    }
}

#[test]
fn nullable_types_keep_written_ownership_in_aliases_records_and_generic_arguments() {
    let fixtures: &[(&str, &str, &[&str])] = &[
        (
            "type ReferenceAlias = Reference | null\n",
            "type_abbrev_defn",
            &["Reference | null"],
        ),
        (
            "type Nested = List<List<string | null> | null> | null\n",
            "type_abbrev_defn",
            &[
                "string | null",
                "List<string | null> | null",
                "List<List<string | null> | null> | null",
            ],
        ),
        (
            "type NullableGenericCallback = List<string> | null -> obj | null\n",
            "type_abbrev_defn",
            &["List<string> | null", "obj | null"],
        ),
        (
            "type NullableGenericTuple = List<string> | null * obj\n",
            "type_abbrev_defn",
            &["List<string> | null"],
        ),
        (
            "type Payload = { Text: string | null; Data: Map<string, obj | null> | null }\n",
            "record_type_defn",
            &[
                "string | null",
                "obj | null",
                "Map<string, obj | null> | null",
            ],
        ),
        (
            "type NullableArray = (string | null)[] | null\n",
            "type_abbrev_defn",
            &["string | null", "(string | null)[] | null"],
        ),
        (
            "type NullableList = (string | null) list | null\n",
            "type_abbrev_defn",
            &["string | null", "(string | null) list | null"],
        ),
        (
            "type NullableTupleTail = string * obj | null\n",
            "type_abbrev_defn",
            &["obj | null"],
        ),
        (
            "type NullableTuple = (string * obj) | null\n",
            "type_abbrev_defn",
            &["(string * obj) | null"],
        ),
        (
            "type PlainCallback = string | null -> obj | null\n",
            "type_abbrev_defn",
            &["string | null", "obj | null"],
        ),
        (
            "type NullableCallback = (string | null -> obj | null) | null\n",
            "type_abbrev_defn",
            &[
                "string | null",
                "obj | null",
                "(string | null -> obj | null) | null",
            ],
        ),
        (
            "type NullableWithTrivia = string | (* nullable suffix *) null\n",
            "type_abbrev_defn",
            &["string | (* nullable suffix *) null"],
        ),
    ];
    for signature in [false, true] {
        for (fixture, owner_kind, nullable_spellings) in fixtures {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, owner_kind).len(), 1);
                assert!(current_nodes(&tree, "union_type_defn").is_empty());
                assert!(current_nodes(&tree, "union_type_case").is_empty());
                assert_eq!(
                    current_nodes(&tree, "nullable_type").len(),
                    nullable_spellings.len()
                );
                for spelling in *nullable_spellings {
                    current_exact_nodes(&tree, source, "nullable_type", spelling);
                }
            });
        }
    }
}

#[test]
fn ordinary_postfix_array_and_byref_type_forms_keep_their_original_node_shapes() {
    for signature in [false, true] {
        for (fixture, kind, spelling) in [
            (
                "type PlainList = string list\n",
                "postfix_type",
                "string list",
            ),
            ("type PlainArray = string[]\n", "list_type", "string[]"),
            ("type NestedArray = string[][]\n", "list_type", "string[][]"),
            ("type ByrefAlias = int&\n", "byref_type", "int&"),
            (
                "type CallbackArray = (string -> obj)[]\n",
                "list_type",
                "(string -> obj)[]",
            ),
        ] {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                current_exact_nodes(&tree, source, kind, spelling);
                assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 1);
                assert!(current_nodes(&tree, "nullable_type").is_empty());
                assert!(current_nodes(&tree, "union_type_case").is_empty());
            });
        }
    }
}

#[test]
fn nullable_arguments_and_returns_do_not_absorb_curried_arrows() {
    for signature in [false, true] {
        let fixture = if signature {
            "module NullableFunctions\nval convert : input:string | null -> context:obj | null -> string | null\n"
        } else {
            "module NullableFunctions\nlet convert (input:string | null) (context:obj | null) : string | null = input\n"
        };
        current_variants(fixture, |source| {
            let tree = current_parse(signature, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "nullable_type").len(), 3);
            assert!(current_nodes(&tree, "function_type").is_empty());
            assert!(current_nodes(&tree, "union_type_case").is_empty());
            if signature {
                assert_eq!(current_nodes(&tree, "arguments_spec").len(), 2);
                assert_eq!(current_nodes(&tree, "argument_spec").len(), 2);
                current_exact_nodes(
                    &tree,
                    source,
                    "curried_spec",
                    "input:string | null -> context:obj | null -> string | null",
                );
            } else {
                assert_eq!(
                    current_nodes(&tree, "function_declaration_left").len(),
                    1,
                    "{}",
                    tree.root_node().to_sexp()
                );
            }
        });
    }
    current_variants(
        "val merge : first:string | null * second:obj | null -> obj | null\n",
        |source| {
            let tree = current_parse(true, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "arguments_spec").len(), 1);
            assert_eq!(current_nodes(&tree, "argument_spec").len(), 2);
            assert_eq!(current_nodes(&tree, "nullable_type").len(), 3);
            assert!(current_nodes(&tree, "compound_type").is_empty());
            assert!(current_nodes(&tree, "function_type").is_empty());
        },
    );
}

#[test]
fn typed_values_and_parenthesized_or_patterns_keep_declaration_shapes() {
    for (fixture, disjuncts, nullable) in [
        ("let (text : string | null) = null\n", 0, 1),
        ("let ((0 : int) | (1 : int)) = 0\n", 1, 0),
        ("let ((\"ready\" : string) | null) = null\n", 1, 0),
    ] {
        current_variants(fixture, |source| {
            let tree = current_parse(false, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "value_declaration_left").len(), 1);
            assert!(current_nodes(&tree, "function_declaration_left").is_empty());
            assert_eq!(current_nodes(&tree, "disjunct_pattern").len(), disjuncts);
            assert_eq!(current_nodes(&tree, "nullable_type").len(), nullable);
        });
    }
    current_variants(
        "let classify ((\"ready\" : string) | null) = 0\n",
        |source| {
            let tree = current_parse(false, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "function_declaration_left").len(), 1);
            assert!(current_nodes(&tree, "value_declaration_left").is_empty());
            assert_eq!(current_nodes(&tree, "disjunct_pattern").len(), 1);
            assert!(current_nodes(&tree, "nullable_type").is_empty());
        },
    );
}

#[test]
fn nullable_constructor_arguments_and_member_returns_keep_object_signature_shapes() {
    for signature in [false, true] {
        let fixture = if signature {
            "type NullableObject =\n    new : input:string | null -> NullableObject\n    member Read : unit -> string | null\n"
        } else {
            "type NullableObject(input:string | null) =\n    member this.Read() : string | null = input\n"
        };
        current_variants(fixture, |source| {
            let tree = current_parse(signature, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "anon_type_defn").len(), 1);
            assert_eq!(current_nodes(&tree, "nullable_type").len(), 2);
            assert!(current_nodes(&tree, "union_type_case").is_empty());
            if signature {
                assert_eq!(current_nodes(&tree, "constructor_signature").len(), 1);
                assert_eq!(current_nodes(&tree, "member_signature").len(), 1);
            }
        });
    }
}

#[test]
fn nullable_payloads_preserve_original_discriminated_union_controls() {
    let fixtures: &[(&str, &[&str])] = &[
        (
            "type OriginalUnion = First | Second\n",
            &["First", "Second"],
        ),
        (
            "type MarkedUnion = | Absent | Present of int\n",
            &["Absent", "Present of int"],
        ),
        (
            "type NullablePayload = | Absent | Present of (string | null)\n",
            &["Absent", "Present of (string | null)"],
        ),
        (
            "type NamedPayload = | Present of text:(string | null)\n",
            &["Present of text:(string | null)"],
        ),
        (
            "type NullableFlexiblePayload = | Present of (#System.Object | null)\n",
            &["Present of (#System.Object | null)"],
        ),
        (
            "type FlexibleNullableOperand = | Present of #(System.Object | null)\n",
            &["Present of #(System.Object | null)"],
        ),
        (
            "type FlexibleGenericPayload = | Present of #List<System.Object | null>\n",
            &["Present of #List<System.Object | null>"],
        ),
        (
            "type ConstrainedNullableTarget<'T> = | Present of 'T :> System.Object | null\n",
            &["Present of 'T :> System.Object | null"],
        ),
        (
            "type EscapedKeyword = | ``null`` | Other\n",
            &["``null``", "Other"],
        ),
    ];
    for signature in [false, true] {
        for (fixture, cases) in fixtures {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "union_type_defn").len(), 1);
                assert_eq!(current_nodes(&tree, "union_type_case").len(), cases.len());
                assert!(current_nodes(&tree, "type_abbrev_defn").is_empty());
                for case in *cases {
                    current_exact_nodes(&tree, source, "union_type_case", case);
                }
            });
        }
        current_variants("type BareAlias = Reference\n", |source| {
            let tree = current_parse(signature, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 1);
            assert!(current_nodes(&tree, "union_type_case").is_empty());
        });
        for fixture in [
            "type InvalidPayload = | Present of string | null\n",
            "type InvalidFlexiblePayload = | Present of #System.Object | null\n",
            "type InvalidKeywordCase = | null\n",
        ] {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                assert!(
                    tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
            });
        }
    }
}

#[test]
fn flexible_type_operands_keep_atomic_and_parenthesized_source_ownership() {
    for signature in [false, true] {
        for (fixture, suffix_kind, suffix_text, flexible_text, parenthesized) in [
            (
                "type FlexibleArray = #System.Object[]\n",
                "list_type",
                "#System.Object[]",
                "#System.Object",
                false,
            ),
            (
                "type FlexibleList = #System.Object list\n",
                "postfix_type",
                "#System.Object list",
                "#System.Object",
                false,
            ),
            (
                "type FlexibleLineArray = #line[]\n",
                "list_type",
                "#line[]",
                "#line",
                false,
            ),
            (
                "type FlexibleLineList = #line list\n",
                "postfix_type",
                "#line list",
                "#line",
                false,
            ),
            (
                "type FlexibleIndentArray = #indent[]\n",
                "list_type",
                "#indent[]",
                "#indent",
                false,
            ),
            (
                "type FlexibleIndentList = #indent list\n",
                "postfix_type",
                "#indent list",
                "#indent",
                false,
            ),
            (
                "type FlexibleArrayOperand = #(System.Object[])\n",
                "list_type",
                "System.Object[]",
                "#(System.Object[])",
                true,
            ),
            (
                "type FlexibleListOperand = #(System.Object list)\n",
                "postfix_type",
                "System.Object list",
                "#(System.Object list)",
                true,
            ),
        ] {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 1);
                assert_eq!(current_nodes(&tree, "flexible_type").len(), 1);
                assert_eq!(current_nodes(&tree, suffix_kind).len(), 1);
                let suffix = current_exact_nodes(&tree, source, suffix_kind, suffix_text)[0];
                let flexible =
                    current_exact_nodes(&tree, source, "flexible_type", flexible_text)[0];
                if parenthesized {
                    let paren = suffix.parent().expect("suffix has no containing operand");
                    assert_eq!(paren.kind(), "paren_type");
                    assert_eq!(paren.parent(), Some(flexible));
                } else {
                    assert_eq!(flexible.parent(), Some(suffix));
                }
                assert!(current_nodes(&tree, "nullable_type").is_empty());
                assert!(current_nodes(&tree, "union_type_case").is_empty());
            });
        }
        current_variants("type FlexibleLineCallback = #line -> int\n", |source| {
            let tree = current_parse(signature, source);
            current_clean(&tree, source);
            let function = current_exact_nodes(&tree, source, "function_type", "#line -> int")[0];
            let flexible = current_exact_nodes(&tree, source, "flexible_type", "#line")[0];
            assert_eq!(flexible.parent(), Some(function));
        });
        current_variants("type FlexibleLineParen = ( #line )\n", |source| {
            let tree = current_parse(signature, source);
            current_clean(&tree, source);
            let paren = current_exact_nodes(&tree, source, "paren_type", "( #line )")[0];
            let flexible = current_exact_nodes(&tree, source, "flexible_type", "#line")[0];
            assert_eq!(flexible.parent(), Some(paren));
        });
        for (fixture, parent_kind, parent_text) in [
            (
                "type FlexibleIndentCallback = #indent -> int\n",
                "function_type",
                "#indent -> int",
            ),
            (
                "type FlexibleIndentParen = ( #indent )\n",
                "paren_type",
                "( #indent )",
            ),
        ] {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                let parent = current_exact_nodes(&tree, source, parent_kind, parent_text)[0];
                let flexible = current_exact_nodes(&tree, source, "flexible_type", "#indent")[0];
                assert_eq!(flexible.parent(), Some(parent));
            });
        }
        for (fixture, flexible_text, nullable_text, nullable_is_outer) in [
            (
                "type NullableHashPayload = | Present of (#System.Object | null)\n",
                "#System.Object",
                "#System.Object | null",
                true,
            ),
            (
                "type HashNullableOperandPayload = | Present of #(System.Object | null)\n",
                "#(System.Object | null)",
                "System.Object | null",
                false,
            ),
        ] {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "union_type_case").len(), 1);
                assert_eq!(current_nodes(&tree, "flexible_type").len(), 1);
                assert_eq!(current_nodes(&tree, "nullable_type").len(), 1);
                let flexible =
                    current_exact_nodes(&tree, source, "flexible_type", flexible_text)[0];
                let nullable =
                    current_exact_nodes(&tree, source, "nullable_type", nullable_text)[0];
                if nullable_is_outer {
                    assert_eq!(flexible.parent(), Some(nullable));
                    assert_eq!(nullable.parent().unwrap().kind(), "paren_type");
                } else {
                    let paren = nullable.parent().expect("nullable type has no operand");
                    assert_eq!(paren.kind(), "paren_type");
                    assert_eq!(paren.parent(), Some(flexible));
                }
                assert!(current_nodes(&tree, "type_abbrev_defn").is_empty());
            });
        }
        current_variants(
            "type FlexibleRecordOperand = #({| Value: string |})\n",
            |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "flexible_type").len(), 1);
                assert_eq!(current_nodes(&tree, "anon_record_type").len(), 1);
                let flexible =
                    current_exact_nodes(&tree, source, "flexible_type", "#({| Value: string |})")
                        [0];
                let record =
                    current_exact_nodes(&tree, source, "anon_record_type", "{| Value: string |}")
                        [0];
                let paren = record.parent().expect("record type has no operand");
                assert_eq!(paren.kind(), "paren_type");
                assert_eq!(paren.parent(), Some(flexible));
                assert!(current_nodes(&tree, "nullable_type").is_empty());
            },
        );
        current_variants(
            "type InvalidFlexibleRecord = #{| Value: string |}\n",
            |source| {
                let tree = current_parse(signature, source);
                assert!(
                    tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
            },
        );
    }
}

#[test]
fn flexible_type_prefixes_open_layout_without_adopting_directive_state() {
    for signature in [false, true] {
        for operand in [
            "System.Object",
            "int",
            "element",
            "endThing",
            "ifThing",
            "endifType",
            "elseValue",
            "indentThing",
            "lineType",
            "lightType",
            "``nowarnType``",
            "``warnonType``",
            "loadType",
            "timeType",
            "helpType",
            "quitType",
            "IType",
            "rType",
            "ifλ",
            "elseλ",
            "endifλ",
            "lineλ",
            "r",
            "I",
            "line",
            "time",
            "load",
            "help",
            "quit",
            "``nowarn``",
            "``warnon``",
            "indent",
            "``light``",
        ] {
            for separator in [" ", "\n    "] {
                let fixture = format!(
                    "type FlexiblePrefix ={separator}#{operand}\ntype FollowingAlias = int\n"
                );
                current_variants(&fixture, |source| {
                    let tree = current_parse(signature, source);
                    current_clean(&tree, source);
                    assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 2);
                    assert_eq!(current_nodes(&tree, "flexible_type").len(), 1);
                    current_exact_nodes(&tree, source, "flexible_type", &format!("#{operand}"));
                    assert!(current_nodes(&tree, "preproc_if").is_empty());
                    assert!(current_nodes(&tree, "preproc_else").is_empty());
                    assert!(current_nodes(&tree, "preproc_inactive").is_empty());
                    assert!(current_nodes(&tree, "compiler_directive_decl").is_empty());
                });
            }
        }
        current_variants(
            "#if TYPES\ntype HashBranch = #ifThing\n#else\ntype HashBranch = #elseValue\n#endif\ntype FollowingBranch = #endifType\n",
            |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "preproc_if").len(), 1);
                assert_eq!(current_nodes(&tree, "preproc_else").len(), 1);
                assert!(current_nodes(&tree, "preproc_inactive").is_empty());
                assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 3);
                assert_eq!(current_nodes(&tree, "flexible_type").len(), 3);
                for spelling in ["#ifThing", "#elseValue", "#endifType"] {
                    current_exact_nodes(&tree, source, "flexible_type", spelling);
                }
            },
        );
        for operand in ["nowarn", "warnon", "light", "nowarnType", "warnonType"] {
            for separator in [" ", "\n    "] {
                let fixture = format!(
                    "type InvalidHashKeyword ={separator}#{operand}\ntype FollowingAlias = int\n"
                );
                current_variants(&fixture, |source| {
                    let tree = current_parse(signature, source);
                    assert!(
                        tree.root_node().has_error(),
                        "{source:?}: {}",
                        tree.root_node().to_sexp()
                    );
                });
            }
        }
        current_variants(
            "#indent \"on\"\n#light \"on\"\n#nowarn \"20\"\n#warnon 20\n#line 40 \"hash-layout.fs\"\n#41 \"hash-layout.fs\"\ntype HashOperand = #System.Object\ntype FollowingAlias = int\n",
            |source| {
                let tree = current_parse(signature, source);
                assert!(
                    !tree.root_node().has_error(),
                    "signature={signature}, {source:?}: {}",
                    tree.root_node().to_sexp()
                );
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 2);
                assert_eq!(current_nodes(&tree, "flexible_type").len(), 1);
                assert_eq!(current_nodes(&tree, "compiler_directive_decl").len(), 3);
                assert_eq!(current_nodes(&tree, "preproc_line").len(), 2);
                for (kind, written) in [
                    ("compiler_directive_decl", "#light \"on\""),
                    ("compiler_directive_decl", "#nowarn \"20\""),
                    ("compiler_directive_decl", "#warnon 20"),
                    ("preproc_line", "#line 40 \"hash-layout.fs\""),
                    ("preproc_line", "#41 \"hash-layout.fs\""),
                ] {
                    let directives = current_exact_nodes(&tree, source, kind, written);
                    assert_eq!(directives.len(), 1);
                    assert!(directives[0].is_extra());
                }
                let header_end = source.find('\n').expect("directive line has no LF");
                let header = &source[..header_end];
                let trivia = current_exact_nodes(&tree, source, "preproc_inactive", header);
                assert_eq!(trivia.len(), 1);
                assert_eq!(trivia[0].start_byte(), 0);
                assert_eq!(trivia[0].end_byte(), header_end);
                assert_eq!(source.get(header_end..header_end + 1), Some("\n"));
                current_exact_nodes(&tree, source, "flexible_type", "#System.Object");
                let following = current_exact_nodes(
                    &tree,
                    source,
                    "type_definition",
                    "type FollowingAlias = int",
                );
                assert_eq!(following.len(), 1);
                assert_eq!(
                    following[0].start_byte(),
                    source.find("type FollowingAlias = int").unwrap()
                );
            },
        );
        let declaration = if signature {
            ["val selected : int", "val alternative : int"]
        } else {
            ["let selected = 1", "let alternative = 2"]
        };
        let fixture = format!(
            "module DirectiveProfile\n#if TYPES\n#line 40 \"hash-layout.fs\"\n#41 \"hash-layout.fs\"\n#light \"on\"\n{}\n#else\n#nowarn \"20\"\n#warnon 20\n{}\n#endif\ntype FollowingAlias = int\n",
            declaration[0], declaration[1]
        );
        current_variants(&fixture, |source| {
            let tree = current_parse(signature, source);
            assert!(
                !tree.root_node().has_error(),
                "signature={signature}, {source:?}: {}",
                tree.root_node().to_sexp()
            );
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "named_module").len(), 1);
            assert_eq!(current_nodes(&tree, "preproc_if").len(), 1);
            assert_eq!(current_nodes(&tree, "preproc_else").len(), 1);
            assert_eq!(current_nodes(&tree, "preproc_line").len(), 2);
            assert_eq!(current_nodes(&tree, "compiler_directive_decl").len(), 3);
            for (kind, written) in [
                ("compiler_directive_decl", "#light \"on\""),
                ("compiler_directive_decl", "#nowarn \"20\""),
                ("compiler_directive_decl", "#warnon 20"),
                ("preproc_line", "#line 40 \"hash-layout.fs\""),
                ("preproc_line", "#41 \"hash-layout.fs\""),
            ] {
                let directives = current_exact_nodes(&tree, source, kind, written);
                assert_eq!(directives.len(), 1);
                assert!(directives[0].is_extra());
            }
            let kind = if signature {
                "value_definition"
            } else {
                "function_or_value_defn"
            };
            assert_eq!(current_nodes(&tree, kind).len(), 2);
            for written in declaration {
                current_exact_nodes(&tree, source, kind, written);
            }
            let following = current_exact_nodes(
                &tree,
                source,
                "type_definition",
                "type FollowingAlias = int",
            );
            assert_eq!(following.len(), 1);
            assert_eq!(
                following[0].start_byte(),
                source.find("type FollowingAlias = int").unwrap()
            );
        });
    }
    current_variants(
        "#r @\"λ.dll\"\n#load \"neutral.fsx\"\n#time \"on\"\n#I \"directory\"\n#help\n#quit\ntype HashOperand = #System.Object\n",
        |source| {
            let tree = current_parse(false, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "fsi_directive_decl").len(), 6);
            assert_eq!(current_nodes(&tree, "flexible_type").len(), 1);
            current_exact_nodes(&tree, source, "fsi_directive_decl", "#r @\"λ.dll\"");
            current_exact_nodes(&tree, source, "flexible_type", "#System.Object");
        },
    );
}

#[test]
fn not_null_generic_constraints_coexist_with_legacy_null_and_not_struct_constraints() {
    for signature in [false, true] {
        for (fixture, spelling) in [
            (
                "type NonNull<'T when 'T : not null> = 'T\n",
                "'T : not null",
            ),
            ("type LegacyNullable<'T when 'T : null> = 'T\n", "'T : null"),
            (
                "type ReferenceOnly<'T when 'T : not struct> = 'T\n",
                "'T : not struct",
            ),
            (
                "type ReferenceAlias<'T when 'T : not null and 'T : not struct> = 'T | null\n",
                "'T : not null",
            ),
        ] {
            current_variants(fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                current_exact_nodes(&tree, source, "constraint", spelling);
                assert_eq!(current_nodes(&tree, "type_abbrev_defn").len(), 1);
                assert!(current_nodes(&tree, "union_type_case").is_empty());
            });
        }
    }
    current_variants(
        "let preserve<'T when 'T : not null> (value:'T) : 'T = value\n",
        |source| {
            let tree = current_parse(false, source);
            current_clean(&tree, source);
            current_exact_nodes(&tree, source, "constraint", "'T : not null");
        },
    );
}

#[test]
fn signature_value_constraints_follow_complete_curried_type_specs() {
    for fixture in [
        "val retain : value:'T -> 'T when 'T : not null\n",
        "val select : first:'T -> second:'T -> 'T when 'T : not null and 'T : not struct\n",
        "val createNull : unit -> 'T when 'T : null\n",
        "val referenceValue : 'T when 'T : not struct\n",
        "val nullableValue : unit -> 'T | null when 'T : not struct\n",
        "val inline empty : unit -> ^T when ^T : (static member Zero : ^T)\n",
        "val inline combine : left:^T -> right:^T -> ^T when ^T : (static member (+) : ^T * ^T -> ^T) and ^T : equality\n",
    ] {
        current_variants(fixture, |source| {
            let tree = current_parse(true, source);
            current_clean(&tree, source);
            assert_eq!(current_nodes(&tree, "value_definition").len(), 1);
            let clauses = current_nodes(&tree, "type_argument_constraints");
            assert_eq!(clauses.len(), 1);
            assert_eq!(clauses[0].parent().unwrap().kind(), "value_definition");
            let spec = current_nodes(&tree, "curried_spec");
            assert_eq!(spec.len(), 1);
            assert!(spec[0].end_byte() < clauses[0].start_byte());
        });
    }
    for fixture in [
        "val malformed : 'T when 'T : not unknown\n",
        "val malformed : string |\n",
        "val malformed : string | Missing\n",
        "val malformed : string | null list\n",
        "val malformed : string | null[]\n",
        "val malformed : string | null | null\n",
    ] {
        current_variants(fixture, |source| {
            let tree = current_parse(true, source);
            assert!(
                tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
        });
    }
}

#[test]
fn conditional_module_contents_use_the_selected_implementation_or_signature_profile() {
    for signature in [false, true] {
        let declaration = if signature {
            ["val selected : int", "val alternative : int"]
        } else {
            ["let selected = 1", "let alternative = 2"]
        };
        for fixture in [
            format!(
                "module Conditional\n#if FEATURE\n{}\n#else\n{}\n#endif\n",
                declaration[0], declaration[1]
            ),
            format!(
                "module Conditional =\n    #if FEATURE\n    {}\n    #else\n    {}\n    #endif\n",
                declaration[0], declaration[1]
            ),
            format!(
                "#if FEATURE\nmodule Conditional\n{}\n#else\nmodule Alternate\n{}\n#endif\n",
                declaration[0], declaration[1]
            ),
        ] {
            current_variants(&fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                let owner_kind = if signature {
                    "value_definition"
                } else {
                    "function_or_value_defn"
                };
                assert_eq!(current_nodes(&tree, owner_kind).len(), 2);
                current_exact_nodes(&tree, source, owner_kind, declaration[0]);
                current_exact_nodes(&tree, source, owner_kind, declaration[1]);
                assert_eq!(current_nodes(&tree, "preproc_if").len(), 1);
            });
        }
    }
}

#[test]
fn conditional_type_splices_retain_existing_inactive_else_trivia() {
    for signature in [false, true] {
        let fixture = if signature {
            "val text :\n#if FORMAT\nstring\n#else\nnot valid type syntax @@@\n#endif\n"
        } else {
            "let text :\n#if FORMAT\nstring\n#else\nnot valid type syntax @@@\n#endif\n= \"written\"\n"
        };
        current_variants(fixture, |source| {
            let tree = current_parse(signature, source);
            current_clean(&tree, source);
            assert!(current_nodes(&tree, "preproc_if").is_empty());
            assert!(current_nodes(&tree, "preproc_inactive").iter().any(|node| {
                source
                    .get(node.byte_range())
                    .is_some_and(|text| text.contains("not valid type syntax @@@"))
            }));
            assert_eq!(current_nodes(&tree, "simple_type").len(), 1);
        });
    }
}

#[test]
fn conditional_union_splice_trivia_stops_before_the_first_body_character() {
    for signature in [false, true] {
        for comment in ["// neutral comment", "// λ", "/// λ"] {
            let fixture = format!(
                "type Choice = | First\n#if TYPES\n{comment}\n    | Second of int\n#else\nnot valid type syntax @@@\n#endif\ntype FollowingAlias = int\n"
            );
            current_variants(&fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert!(current_nodes(&tree, "preproc_if").is_empty());
                assert_eq!(current_nodes(&tree, "union_type_case").len(), 2);
                let header_start = source.find("#if TYPES").unwrap();
                let body_start = source.find("| Second of int").unwrap();
                let header = &source[header_start..body_start];
                let trivia = current_exact_nodes(&tree, source, "preproc_inactive", header);
                assert_eq!(trivia.len(), 1);
                assert_eq!(trivia[0].start_byte(), header_start);
                assert_eq!(trivia[0].end_byte(), body_start);
                assert_eq!(source.get(body_start..body_start + 1), Some("|"));
                assert!(header.contains(comment));
                let case = current_exact_nodes(&tree, source, "union_type_case", "Second of int");
                assert_eq!(case.len(), 1);
                assert_eq!(case[0].start_byte(), body_start + 2);
                let following = current_exact_nodes(
                    &tree,
                    source,
                    "type_definition",
                    "type FollowingAlias = int",
                );
                assert_eq!(following.len(), 1);
                assert_eq!(
                    following[0].start_byte(),
                    source.find("type FollowingAlias = int").unwrap()
                );
            });
        }
        for lookalike in ["#ifλ", "#endifλ"] {
            let fixture = format!(
                "type Choice = | First\n#if TYPES\n    | Second\n#else\n{lookalike}\nnot valid type syntax @@@\n#endif\ntype FollowingAlias = int\n"
            );
            current_variants(&fixture, |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert!(current_nodes(&tree, "preproc_if").is_empty());
                assert_eq!(current_nodes(&tree, "union_type_case").len(), 2);
                let inactive_start = source.find("#else").unwrap();
                let closing_start = source.rfind("#endif\n").unwrap_or_else(|| {
                    source
                        .rfind("#endif\r\n")
                        .expect("authored close is absent")
                });
                let inactive_end = closing_start + source[closing_start..].find('\n').unwrap();
                let inactive = &source[inactive_start..inactive_end];
                assert!(inactive.contains(lookalike));
                let trivia = current_exact_nodes(&tree, source, "preproc_inactive", inactive);
                assert_eq!(trivia.len(), 1);
                assert_eq!(trivia[0].start_byte(), inactive_start);
                assert_eq!(trivia[0].end_byte(), inactive_end);
                let following = current_exact_nodes(
                    &tree,
                    source,
                    "type_definition",
                    "type FollowingAlias = int",
                );
                assert_eq!(following.len(), 1);
                assert_eq!(
                    following[0].start_byte(),
                    source.find("type FollowingAlias = int").unwrap()
                );
            });
        }
        current_variants(
            "#if OUTER\ntype Choice = | First\n#if INNER\n// nested comment λ\n    | Second\n#else\nnot valid type syntax @@@\n#endif\n#else\ntype Choice = | Alternative\n#endif\ntype FollowingAlias = int\n",
            |source| {
                let tree = current_parse(signature, source);
                current_clean(&tree, source);
                assert_eq!(current_nodes(&tree, "preproc_if").len(), 1);
                assert_eq!(current_nodes(&tree, "preproc_else").len(), 1);
                assert_eq!(current_nodes(&tree, "union_type_case").len(), 3);
                let header_start = source.find("#if INNER").unwrap();
                let header_end = header_start + source[header_start..].find('\n').unwrap();
                let header = &source[header_start..header_end];
                let trivia = current_exact_nodes(&tree, source, "preproc_inactive", header);
                assert_eq!(trivia.len(), 1);
                assert_eq!(trivia[0].start_byte(), header_start);
                assert_eq!(trivia[0].end_byte(), header_end);
                assert!(!header.contains("nested comment"));
                current_exact_nodes(&tree, source, "union_type_case", "Second");
                current_exact_nodes(
                    &tree,
                    source,
                    "type_definition",
                    "type FollowingAlias = int",
                );
            },
        );
    }
}
