//! Runs the complete retained upstream syntax corpus without upstream tools.
//! Named-tree expectations describe syntax and make no evaluation claims.

use tree_sitter::Parser;

const CORPORA: [(&str, &str); 6] = [
    (
        "collections",
        include_str!(
            "../../../third_party/tree-sitter-julia-0.25.0/upstream/authored-files/test/corpus/collections.txt"
        ),
    ),
    (
        "definitions",
        include_str!(
            "../../../third_party/tree-sitter-julia-0.25.0/upstream/authored-files/test/corpus/definitions.txt"
        ),
    ),
    (
        "expressions",
        include_str!(
            "../../../third_party/tree-sitter-julia-0.25.0/upstream/authored-files/test/corpus/expressions.txt"
        ),
    ),
    (
        "literals",
        include_str!(
            "../../../third_party/tree-sitter-julia-0.25.0/upstream/authored-files/test/corpus/literals.txt"
        ),
    ),
    (
        "operators",
        include_str!(
            "../../../third_party/tree-sitter-julia-0.25.0/upstream/authored-files/test/corpus/operators.txt"
        ),
    ),
    (
        "statements",
        include_str!(
            "../../../third_party/tree-sitter-julia-0.25.0/upstream/authored-files/test/corpus/statements.txt"
        ),
    ),
];

fn delimiter(line: &str, byte: u8) -> bool {
    line.len() >= 3 && line.bytes().all(|value| value == byte)
}

fn named_shape(sexp: &str) -> String {
    // Upstream named-tree expectations permit omitted field labels and comments.
    // Both sides retain every node, quoted missing token and parenthesis.
    sexp.lines()
        .filter(|line| !line.trim_start().starts_with(';'))
        .map(|line| line.split(";;").next().unwrap())
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
fn every_retained_upstream_case_matches_its_authored_named_tree() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_julia::LANGUAGE_JULIA.into())
        .unwrap();
    let mut counts = Vec::new();
    for (label, corpus) in CORPORA {
        let lines: Vec<_> = corpus.lines().collect();
        let mut index = 0;
        let mut cases = 0;
        while index < lines.len() {
            if lines[index].trim().is_empty() {
                index += 1;
                continue;
            }
            assert!(
                delimiter(lines[index], b'='),
                "{label}:{index}: unaccounted fixture bytes"
            );
            let title = lines[index + 1];
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
            let source = lines[source_start..index].join("\n");
            index += 1;
            let expected_start = index;
            while index < lines.len() && !delimiter(lines[index], b'=') {
                index += 1;
            }
            let expected = lines[expected_start..index].join("\n");
            assert!(
                !expected.trim().is_empty(),
                "{label}: {title}: missing expected tree"
            );
            let tree = parser.parse(&source, None).unwrap();
            assert_eq!(
                named_shape(&tree.root_node().to_sexp()),
                named_shape(&expected),
                "{label}: {title}\n{source}"
            );
            cases += 1;
        }
        counts.push((label, cases));
    }
    assert_eq!(
        counts,
        [
            ("collections", 6),
            ("definitions", 13),
            ("expressions", 14),
            ("literals", 8),
            ("operators", 8),
            ("statements", 14)
        ]
    );
}
