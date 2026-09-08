//! Source-backed R capture ownership before parser-independent IR lowering.
//! Native selectors are shared with the adapter; no source code is executed.

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

#[path = "../../../crates/rootlight-adapter-treesitter/src/query_pack/r.rs"]
mod r;

const QUERY: &str = include_str!("../../../crates/rootlight-adapter-treesitter/queries/r.scm");

#[derive(Debug, PartialEq, Eq)]
struct Capture {
    role: String,
    syntax: String,
    text: String,
    start: usize,
    end: usize,
}

fn captures(source: &str, role: &str) -> Vec<Capture> {
    let language = tree_sitter_r::LANGUAGE.into();
    let query = Query::new(&language, QUERY).unwrap();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source}: {}",
        tree.root_node().to_sexp()
    );
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut output = Vec::new();
    while let Some(found) = matches.next() {
        for capture in found.captures {
            let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
            if name != role {
                continue;
            }
            let node = capture.node;
            let mut syntax = node.kind();
            let range = match role {
                "declaration" | "definition" => {
                    let Some(binding) = r::declaration(node, source.as_bytes()) else {
                        continue;
                    };
                    syntax = binding.syntax;
                    if role == "definition" {
                        binding.name.byte_range()
                    } else {
                        node.byte_range()
                    }
                }
                "signature" => {
                    let Some(range) = r::signature_range(node, source.as_bytes()) else {
                        continue;
                    };
                    range
                }
                "reference" if r::is_nonlexical_name(node, source.as_bytes()) => continue,
                _ => node.byte_range(),
            };
            output.push(Capture {
                role: role.to_owned(),
                syntax: syntax.to_owned(),
                text: source.get(range.clone()).unwrap().to_owned(),
                start: range.start,
                end: range.end,
            });
        }
    }
    output.sort_by_key(|capture| (capture.start, capture.end));
    output
}

fn texts(source: &str, role: &str) -> Vec<String> {
    captures(source, role)
        .into_iter()
        .map(|capture| capture.text)
        .collect()
}

#[test]
fn r_assignments_capture_both_directions_and_mark_nonlocal_writes() {
    for (source, expected) in [
        ("left <- function(x) x", "r.function"),
        ("left = function(x) x", "r.function"),
        ("(function(x) x) -> left", "r.function"),
        ("left <<- function(x) x", "r.nonlocal_function"),
        ("(function(x) x) ->> left", "r.nonlocal_function"),
        ("left <- ((function(x) x))", "r.function"),
        ("left <- \\(x) x", "r.function"),
        ("left <- value", "r.variable"),
        ("value -> left", "r.variable"),
        ("function(x) x -> left", "r.variable"),
    ] {
        let definitions = captures(source, "definition");
        let binding = definitions
            .iter()
            .find(|capture| capture.text == "left")
            .unwrap();
        assert_eq!(binding.syntax, expected, "{source}");
        if expected.contains("function") {
            assert_eq!(
                definitions
                    .iter()
                    .filter(|capture| capture.text == "x")
                    .count(),
                1
            );
            assert_eq!(
                texts(source, "signature"),
                [if source.contains('\\') {
                    "\\(x)"
                } else {
                    "function(x)"
                }]
            );
        } else {
            assert!(texts(source, "signature").is_empty());
        }
    }
}

#[test]
fn r_parameter_defaults_named_arguments_and_member_names_are_not_bindings() {
    let source = "identity <- function(value = fallback, ..., `with spaces` = 2) { for (index in values) print(index); stats::median(value, na.rm = TRUE); object$member; object@slot; ..1 }";
    assert_eq!(
        texts(source, "definition"),
        ["identity", "value", "...", "`with spaces`", "index"]
    );
    assert_eq!(
        texts(source, "reference"),
        [
            "fallback",
            "values",
            "print",
            "index",
            "stats::median",
            "value",
            "object",
            "object$member",
            "object",
            "object@slot",
            "..1"
        ]
    );
    assert_eq!(texts(source, "call_name"), ["print", "median"]);
}

#[test]
fn r_replacement_targets_and_runtime_definition_calls_do_not_invent_declarations() {
    for source in [
        "object$member <- function(x) x",
        "object[[key]] <- function(x) x",
        "names(object) <- labels",
        "assign('created', function(x) x)",
        "setClass('Record', slots = c(value = 'numeric'))",
        "object[, member := value]",
    ] {
        let definitions = texts(source, "definition");
        assert!(
            definitions.iter().all(|name| name == "x"),
            "{source}: {definitions:?}"
        );
    }
}

#[test]
fn r_quoted_names_comments_and_literals_preserve_exact_source_identity() {
    let source = "#' Function docs\n`with spaces` <- function(`π`) `π`\n\"quoted name\" <- function() 'not <- function(fake) fake'\n# false <- function(fake) fake\n";
    assert_eq!(
        texts(source, "definition"),
        ["`with spaces`", "`π`", "\"quoted name\""]
    );
    assert_eq!(texts(source, "documentation"), ["#' Function docs"]);
    assert_eq!(texts(source, "signature"), ["function(`π`)", "function()"]);
    for role in [
        "definition",
        "signature",
        "reference",
        "call",
        "comment",
        "string",
    ] {
        for capture in captures(source, role) {
            assert_eq!(
                source.get(capture.start..capture.end),
                Some(capture.text.as_str())
            );
        }
    }
}
