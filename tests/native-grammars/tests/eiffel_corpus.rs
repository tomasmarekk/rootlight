//! Executes every retained upstream Eiffel case as inert data. One reserved-word
//! identifier has an explicit corrected recovery oracle and a valid counterpart.

use tree_sitter::Parser;

const CORPORA: [(&str, &str, usize); 11] = [
    (
        "body",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/body.txt"
        ),
        25,
    ),
    (
        "class",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/class.txt"
        ),
        14,
    ),
    (
        "comment",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/comment.txt"
        ),
        2,
    ),
    (
        "comment_spaces",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/comment_spaces.txt"
        ),
        1,
    ),
    (
        "errors",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/errors.txt"
        ),
        2,
    ),
    (
        "expression",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/expression.txt"
        ),
        22,
    ),
    (
        "feature",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/feature.txt"
        ),
        22,
    ),
    (
        "instruction",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/instruction.txt"
        ),
        1,
    ),
    (
        "number",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/number.txt"
        ),
        4,
    ),
    (
        "string",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/string.txt"
        ),
        18,
    ),
    (
        "types",
        include_str!(
            "../../../third_party/tree-sitter-eiffel-1.0.0/upstream/authored-files/test/corpus/types.txt"
        ),
        2,
    ),
];

fn delimiter(line: &str, byte: u8) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    line.len() >= 3 && line.bytes().all(|value| value == byte)
}

fn named_shape(sexp: &str) -> String {
    // The upstream corpus permits omitted field labels. Every node, missing
    // token and parenthesis remains in the compared structural expectation.
    sexp.lines()
        .filter(|line| !line.trim_start().starts_with(';'))
        .collect::<Vec<_>>()
        .join(" ")
        .replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .filter(|token| !token.ends_with(':'))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn every_retained_upstream_case_matches_its_reviewed_complete_named_tree() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_eiffel::LANGUAGE_EIFFEL.into())
        .unwrap();
    let mut total = 0;
    let mut reserved_identifier_corrections = 0;
    for (label, corpus, expected_count) in CORPORA {
        // Keep interior CR/LF bytes, including authored CR-in-verbatim cases.
        let lines: Vec<_> = corpus.split_inclusive('\n').collect();
        let mut index = 0;
        let mut count = 0;
        while index < lines.len() {
            if lines[index].trim().is_empty() {
                index += 1;
                continue;
            }
            assert!(
                delimiter(lines[index], b'='),
                "{label}:{index}: unaccounted fixture bytes"
            );
            let title = lines[index + 1].trim();
            assert!(
                delimiter(lines[index + 2], b'='),
                "{label}: {title}: header"
            );
            index += 3;
            let source_start = index;
            while index < lines.len() && !delimiter(lines[index], b'-') {
                index += 1;
            }
            assert!(
                index < lines.len(),
                "{label}: {title}: missing source separator"
            );
            let source = lines[source_start..index].concat();
            index += 1;
            let expected_start = index;
            while index < lines.len() && !delimiter(lines[index], b'=') {
                index += 1;
            }
            let expected = lines[expected_start..index].concat();
            assert!(
                !expected.trim().is_empty(),
                "{label}: {title}: missing expected tree"
            );
            let tree = parser.parse(&source, None).unwrap();
            let preferred_expected = if label == "body" && title == "Body. Cursor feature access" {
                // ECMA-367 8.32.12/13/17 reserves True regardless of case; the
                // original fixture uses it as an ordinary feature identifier.
                assert_eq!(
                    source,
                    "class A\ninvariant\n  ∀ x: help_messages ¦ @ x.true\nend\n"
                );
                reserved_identifier_corrections += 1;
                let counterpart = source.replace("x.true", "x.item");
                let valid = parser.parse(&counterpart, None).unwrap();
                assert!(!valid.root_node().has_error());
                assert_eq!(
                    named_shape(&valid.root_node().to_sexp()),
                    named_shape(&expected)
                );
                assert!(tree.root_node().has_error());
                "(source_file (class_declaration (class_name) (invariant (assertion_clause (expression (quantifier_loop (identifier) (expression (call (unqualified_call (identifier)))) (expression (call (at_cursor (identifier)) (unqualified_call (identifier (MISSING _id)))))))) (assertion_clause (expression (boolean_constant))))))"
            } else {
                &expected
            };
            assert_eq!(
                named_shape(&tree.root_node().to_sexp()),
                named_shape(preferred_expected),
                "{label}: {title}\n{source}"
            );
            count += 1;
        }
        assert_eq!(count, expected_count, "{label}: complete case denominator");
        total += count;
    }
    assert_eq!(total, 113);
    assert_eq!(reserved_identifier_corrections, 1);
}
