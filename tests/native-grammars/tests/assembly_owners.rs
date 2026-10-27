//! Native MASM contracts retain physical owners and visible unsupported syntax.
//! Clean written containers do not establish assembled, register or ISA binding.

use tree_sitter::{Node, Parser, Tree};

fn parse(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_asm::LANGUAGE_ASSEMBLY.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        result.push(node);
    }
    result
}

fn name<'a>(node: Node<'_>, source: &'a [u8]) -> &'a [u8] {
    let field = node.child_by_field_name("name").unwrap();
    source.get(field.byte_range()).unwrap()
}

#[test]
fn complete_original_masm_preserves_clean_names_owners_and_physical_eof() {
    let source = include_bytes!(
        "../../../crates/rootlight-adapter-treesitter/tests/fixtures/assembly/reboot.asm"
    );
    assert_eq!(source.len(), 504);
    assert_eq!(source.last(), Some(&0x1a));
    assert_eq!(
        source.windows(2).filter(|bytes| *bytes == b"\r\n").count(),
        42
    );
    let tree = parse(source);
    assert!(tree.root_node().has_error());
    for (spelling, kind, span, parent) in [
        (b"ROMDATA".as_slice(), "segment_definition", 84..91, None),
        (
            b"BootType",
            "data_definition",
            120..128,
            Some(b"ROMDATA".as_slice()),
        ),
        (b"ROMBIOS", "segment_definition", 151..158, None),
        (b"_text", "segment_definition", 217..222, None),
        (
            b"_reboot",
            "procedure_definition",
            348..355,
            Some(b"_text".as_slice()),
        ),
    ] {
        let matches = nodes(&tree)
            .into_iter()
            .filter(|node| node.kind() == kind && name(*node, source) == spelling)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        let node = matches[0];
        assert!(!node.has_error() && !node.is_missing());
        assert_eq!(node.child_by_field_name("name").unwrap().byte_range(), span);
        let owner = node.parent().unwrap();
        if let Some(parent) = parent {
            assert_eq!(owner.kind(), "masm_body");
            assert_eq!(name(owner.parent().unwrap(), source), parent);
        } else {
            assert_eq!(owner.kind(), "program");
        }
    }
    assert!(nodes(&tree).iter().any(|node| node.kind() == "ERROR"));
    assert!(
        !nodes(&tree)
            .iter()
            .any(|node| node.kind() == "data_definition"
                && node.child_by_field_name("name").unwrap().byte_range() == (425..433))
    );
}

#[test]
fn generic_masm_attributes_and_db_dw_initializers_are_clean_without_runtime_interpretation() {
    for newline in ["\n", "\r\n"] {
        let source = [
            "Renamed SEGMENT WORD PUBLIC USE16 'DATA' READONLY",
            "Bytes DB 1, 2",
            "Words DW ?, 7",
            "Routine PROC FAR",
            "ASSUME cs:Renamed, ds:NOTHING",
            "mov ax, Words",
            "Routine ENDP",
            "Renamed ENDS",
            "",
        ]
        .join(newline);
        let tree = parse(source.as_bytes());
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let all = nodes(&tree);
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "data_definition")
                .count(),
            2
        );
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "masm_assume_statement")
                .count(),
            1
        );
        let words = all
            .iter()
            .find(|node| {
                node.kind() == "data_definition" && name(**node, source.as_bytes()) == b"Words"
            })
            .unwrap();
        assert_eq!(
            source
                .as_bytes()
                .get(words.child_by_field_name("value").unwrap().byte_range()),
            Some(b"?, 7".as_slice())
        );
    }
}

#[test]
fn unsupported_masm_headers_and_body_syntax_keep_error_descendants() {
    for source in [
        "Area SEGMENT UNKNOWN\nItem DW ?\nArea ENDS\n",
        "Area SEGMENT PUBLICextra\nItem DW ?\nArea ENDS\n",
        "Area SEGMENT PUBLIC\nRun PROC\nASSUME cs Area\nRun ENDP\nArea ENDS\n",
        "Area SEGMENT PUBLIC\nItem DWextra ?\nArea ENDS\n",
        "Area SEGMENT PUBLIC\nItem DW ?\n",
    ] {
        let tree = parse(source.as_bytes());
        assert!(tree.root_node().has_error(), "{source}");
        assert!(
            !nodes(&tree)
                .iter()
                .any(|node| node.kind() == "segment_definition"
                    && !node.has_error()
                    && !node.is_missing()),
            "{source}"
        );
    }
}

#[test]
fn mismatched_masm_closer_stays_written_and_keyword_prefix_is_not_a_segment() {
    let source = b"Area SEGMENT PUBLIC\nItem DW ?\nOther ENDS\n";
    let tree = parse(source);
    let all = nodes(&tree);
    let segment = all
        .iter()
        .find(|node| node.kind() == "segment_definition")
        .unwrap();
    assert_eq!(name(*segment, source), b"Area");
    let closing = segment.child_by_field_name("end_name").unwrap();
    assert_eq!(source.get(closing.byte_range()), Some(b"Other".as_slice()));
    let tree = parse(b"Area SEGMENTPUBLIC\nmov ax, bx\nArea ENDS\n");
    assert!(
        !nodes(&tree)
            .iter()
            .any(|node| node.kind() == "segment_definition")
    );
}

// The upstream fixtures stay byte-exact; ranges select their original inputs
// and expected trees without replacing them with newly authored examples.
struct UpstreamCase {
    name: &'static str,
    source: std::ops::Range<usize>,
    tree: std::ops::Range<usize>,
}

struct UpstreamCorpus {
    name: &'static str,
    text: &'static str,
    cases: &'static [UpstreamCase],
}

const UPSTREAM_CORPUS: &[UpstreamCorpus] = &[
    UpstreamCorpus {
        name: "comment.test",
        text: include_str!(
            "../../../third_party/tree-sitter-asm-0.24.0/upstream/authored/test/corpus/comment.test"
        ),
        cases: &[
            UpstreamCase {
                name: "Line comment",
                source: 35..56,
                tree: 60..90,
            },
            UpstreamCase {
                name: "Inline comment with instr no args",
                source: 146..182,
                tree: 186..255,
            },
            UpstreamCase {
                name: "Inline comment with instr one reg arg, no space between",
                source: 333..373,
                tree: 377..468,
            },
            UpstreamCase {
                name: "Inline comment with instr one reg arg, space between",
                source: 543..584,
                tree: 588..679,
            },
            UpstreamCase {
                name: "Block comment",
                source: 716..790,
                tree: 794..824,
            },
        ],
    },
    UpstreamCorpus {
        name: "instr_and_reg.test",
        text: include_str!(
            "../../../third_party/tree-sitter-asm-0.24.0/upstream/authored/test/corpus/instr_and_reg.test"
        ),
        cases: &[
            UpstreamCase {
                name: "Instruction -- No args",
                source: 45..52,
                tree: 56..98,
            },
            UpstreamCase {
                name: "Instruction -- One reg arg",
                source: 147..158,
                tree: 162..244,
            },
            UpstreamCase {
                name: "Instruction -- Two reg args",
                source: 294..311,
                tree: 315..423,
            },
            UpstreamCase {
                name: "Instruction -- Pointer offset arg, reg arg",
                source: 488..510,
                tree: 514..636,
            },
            UpstreamCase {
                name: "Instruction -- Integer immediate arg, reg arg (GAS x86)",
                source: 714..729,
                tree: 733..819,
            },
            UpstreamCase {
                name: "Instruction -- Reg arg, Integer immediate arg (ARM A64)",
                source: 897..909,
                tree: 913..1029,
            },
            UpstreamCase {
                name: "Instruction -- Reg arg, reg arg, address pointer with register and negative integer immediate arg, followed by exclmation mark (ARM A64)",
                source: 1188..1212,
                tree: 1216..1382,
            },
            UpstreamCase {
                name: "Instruction -- Reg arg, address pointer with register and integer immediate arg (ARM A64)",
                source: 1494..1511,
                tree: 1515..1641,
            },
            UpstreamCase {
                name: "Instruction -- Reg arg, address pointer with register only (ARM A64)",
                source: 1732..1745,
                tree: 1749..1863,
            },
            UpstreamCase {
                name: "Instruction -- Reg arg, section address arg (ARM A64)",
                source: 1939..1958,
                tree: 1962..2121,
            },
            UpstreamCase {
                name: "Instruction -- Section address arg, reg arg (GAS x86)",
                source: 2197..2212,
                tree: 2216..2357,
            },
            UpstreamCase {
                name: "Instruction -- Dotted label arg",
                source: 2411..2419,
                tree: 2423..2488,
            },
        ],
    },
    UpstreamCorpus {
        name: "labels_directives.test",
        text: include_str!(
            "../../../third_party/tree-sitter-asm-0.24.0/upstream/authored/test/corpus/labels_directives.test"
        ),
        cases: &[
            UpstreamCase {
                name: "Basic label",
                source: 35..41,
                tree: 45..76,
            },
            UpstreamCase {
                name: "Local label",
                source: 111..119,
                tree: 123..159,
            },
            UpstreamCase {
                name: "Directive -- Single string arg",
                source: 212..235,
                tree: 239..301,
            },
            UpstreamCase {
                name: "Directive -- Multiple string args",
                source: 357..388,
                tree: 392..488,
            },
            UpstreamCase {
                name: "Directive -- Single integer arg",
                source: 542..551,
                tree: 555..614,
            },
            UpstreamCase {
                name: "Directive -- Multiple integer args",
                source: 671..685,
                tree: 689..776,
            },
            UpstreamCase {
                name: "Directive -- Single floating point arg",
                source: 837..849,
                tree: 853..914,
            },
            UpstreamCase {
                name: "Directive -- Multiple floating point args",
                source: 978..996,
                tree: 1000..1077,
            },
        ],
    },
    UpstreamCorpus {
        name: "literal.test",
        text: include_str!(
            "../../../third_party/tree-sitter-asm-0.24.0/upstream/authored/test/corpus/literal.test"
        ),
        cases: &[
            UpstreamCase {
                name: "Integer literal base 10",
                source: 46..55,
                tree: 59..116,
            },
            UpstreamCase {
                name: "Integer literal hex",
                source: 158..169,
                tree: 173..230,
            },
            UpstreamCase {
                name: "Float literal",
                source: 266..278,
                tree: 282..341,
            },
            UpstreamCase {
                name: "String literal",
                source: 378..401,
                tree: 405..465,
            },
            UpstreamCase {
                name: "Comma separated list of integer literals base 10",
                source: 536..555,
                tree: 559..644,
            },
            UpstreamCase {
                name: "Comma separated list of integer literals hex",
                source: 711..749,
                tree: 753..838,
            },
            UpstreamCase {
                name: "Comma separated list of string literals",
                source: 900..928,
                tree: 932..1025,
            },
        ],
    },
];

#[test]
fn unchanged_complete_upstream_corpus_keeps_all_expected_native_trees() {
    let mut observed = 0;
    for corpus in UPSTREAM_CORPUS {
        for case in corpus.cases {
            let source = &corpus.text.as_bytes()[case.source.clone()];
            let expected = &corpus.text[case.tree.clone()];
            let tree = parse(source);
            let (actual, expected) =
                upstream_corpus_comparison(&tree.root_node().to_sexp(), expected);
            assert_eq!(actual, expected, "{}: {}", corpus.name, case.name);
            observed += 1;
        }
    }
    assert_eq!(observed, 32);
}

/// Follow the default field mode of each complete upstream corpus case.
/// An explicit expected field enables comparison of every actual field.
fn upstream_corpus_comparison(actual: &str, expected: &str) -> (String, String) {
    use std::sync::LazyLock;

    static COMMENTS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?m)^\s*;.*$").expect("reviewed corpus comment expression")
    });
    static WHITESPACE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\s+").expect("reviewed corpus whitespace expression"));
    static FIELDS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r" \w+: \(").expect("reviewed corpus field expression"));

    let expected = COMMENTS.replace_all(expected, "");
    let expected = WHITESPACE.replace_all(expected.trim(), " ");
    let expected = expected.replace(" )", ")");
    let actual = if FIELDS.is_match(&expected) {
        actual.to_owned()
    } else {
        FIELDS.replace_all(actual, " (").into_owned()
    };
    (actual, expected)
}

#[test]
fn upstream_corpus_omitted_fields_preserve_the_complete_node_tree() {
    let actual = "(program (instruction kind_2: (word) argument: (reg (word))))";
    let expected =
        "\n; expected fields are optional\n(program\n  (instruction (word) (reg (word)) ) )\n";
    let (actual, expected) = upstream_corpus_comparison(actual, expected);
    assert_eq!(actual, expected);
}

#[test]
fn upstream_corpus_explicit_fields_reject_wrong_missing_and_extra_annotations() {
    let expected = "(program first: (word) (reg))";
    let (actual, normalized) = upstream_corpus_comparison(expected, expected);
    assert_eq!(actual, normalized);
    for actual in [
        "(program wrong: (word) (reg))",
        "(program (word) (reg))",
        "(program first: (word) second: (reg))",
    ] {
        let (actual, normalized) = upstream_corpus_comparison(actual, expected);
        assert_ne!(actual, normalized);
    }
}

#[test]
fn upstream_corpus_omitted_fields_still_reject_node_kind_order_and_arity_changes() {
    let expected = "(program (word) (reg))";
    for actual in [
        "(program first: (word) second: (identifier))",
        "(program first: (reg) second: (word))",
        "(program first: (word))",
        "(program first: (word) second: (reg) extra: (word))",
    ] {
        let (actual, normalized) = upstream_corpus_comparison(actual, expected);
        assert_ne!(actual, normalized);
    }
}
