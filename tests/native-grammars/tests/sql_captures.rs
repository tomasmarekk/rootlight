//! SQL declaration/reference boundaries against the shipped native grammar.
//! Queries share the analyzer's native ownership selector and exact source spans.

use std::collections::BTreeSet;

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

#[path = "../../../crates/rootlight-adapter-treesitter/src/query_pack/sql.rs"]
mod sql;

const QUERY: &str = include_str!("../../../crates/rootlight-adapter-treesitter/queries/sql.scm");

fn captures(source: &str, role: &str) -> Vec<(String, String, usize, usize)> {
    let language = tree_sitter_sequel::LANGUAGE.into();
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
            if query.capture_names()[usize::try_from(capture.index).unwrap()] == role {
                if role == "reference" && sql::is_declared_name(capture.node) {
                    continue;
                }
                let node = if role == "definition" {
                    let Some(node) = sql::definition_node(capture.node) else {
                        continue;
                    };
                    node
                } else {
                    capture.node
                };
                let range = if role == "signature" && node.kind() == "create_function" {
                    sql::signature_range(node, source.as_bytes()).unwrap()
                } else {
                    node.byte_range()
                };
                output.push((
                    node.kind().to_owned(),
                    source.get(range.clone()).unwrap().to_owned(),
                    range.start,
                    range.end,
                ));
            }
        }
    }
    output.sort_by_key(|(_, _, start, end)| (*start, *end));
    assert_eq!(
        output.iter().collect::<BTreeSet<_>>().len(),
        output.len(),
        "duplicate captures"
    );
    output
}

#[test]
fn ddl_definitions_do_not_promote_referenced_objects_or_authorization_names() {
    for (source, names) in [
        (
            "CREATE TABLE IF NOT EXISTS app.account (id INT REFERENCES other (id));",
            vec!["app.account", "id"],
        ),
        (
            "CREATE VIEW app.active AS SELECT id FROM app.account;",
            vec!["app.active"],
        ),
        (
            "CREATE MATERIALIZED VIEW IF NOT EXISTS app.cached AS SELECT id FROM app.account;",
            vec!["app.cached"],
        ),
        ("CREATE INDEX by_id ON app.account (id);", vec!["by_id"]),
        ("CREATE INDEX ON app.account (id);", vec![]),
        (
            "CREATE SCHEMA IF NOT EXISTS app AUTHORIZATION owner;",
            vec!["app"],
        ),
        ("CREATE SCHEMA AUTHORIZATION owner;", vec![]),
        ("CREATE DATABASE catalog;", vec!["catalog"]),
        ("CREATE ROLE reader IN ROLE parent;", vec!["reader"]),
        (
            "CREATE SEQUENCE IF NOT EXISTS app.ids OWNED BY app.account.id;",
            vec!["app.ids"],
        ),
        (
            "CREATE EXTENSION IF NOT EXISTS extension_name WITH SCHEMA app;",
            vec!["extension_name"],
        ),
        (
            "CREATE TRIGGER changed AFTER INSERT ON app.account EXECUTE FUNCTION app.notify();",
            vec!["changed"],
        ),
        (
            "CREATE TYPE app.pair AS (left_id INT, right_id INT);",
            vec!["app.pair", "left_id", "right_id"],
        ),
        (
            "CREATE FUNCTION app.identity(value INT) RETURNS INT AS $body$ SELECT value; $body$ LANGUAGE SQL;",
            vec!["app.identity", "value"],
        ),
    ] {
        let actual: Vec<_> = captures(source, "definition")
            .into_iter()
            .map(|(_, name, _, _)| name)
            .collect();
        assert_eq!(actual, names, "{source}");
    }
}

#[test]
fn names_keep_exact_source_bytes_and_do_not_capture_literal_ddl() {
    let source = "-- CREATE TABLE false_comment (id INT);\nCREATE TABLE \"Order Items\" (\"item id\" INT, `label` TEXT);\nSELECT 'CREATE TABLE false_text (id INT);';";
    let found = captures(source, "definition");
    assert_eq!(
        found
            .iter()
            .map(|(_, text, _, _)| text.as_str())
            .collect::<Vec<_>>(),
        ["\"Order Items\"", "\"item id\"", "`label`"]
    );
    for (_, text, start, end) in found {
        assert_eq!(source.get(start..end), Some(text.as_str()));
    }
    let qualified = "CREATE TABLE app /* ownership */ . \"Order Items\" (id INT);";
    assert_eq!(
        captures(qualified, "definition")[0].1,
        "app /* ownership */ . \"Order Items\""
    );
    let commented =
        "CREATE /*a*/ TABLE /*b*/ IF /*c*/ NOT /*d*/ EXISTS /*e*/ app.account (id INT);";
    assert_eq!(
        captures(commented, "definition")
            .iter()
            .map(|(_, name, _, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["app.account", "id"]
    );
}

#[test]
fn reference_captures_retain_written_targets_without_claiming_resolution() {
    let source = "CREATE VIEW app.active AS SELECT id FROM app.account JOIN app.flags USING (id);";
    let references: Vec<_> = captures(source, "reference")
        .into_iter()
        .map(|(_, name, _, _)| name)
        .collect();
    assert!(references.contains(&"app.account".to_owned()));
    assert!(references.contains(&"app.flags".to_owned()));
    assert!(!references.contains(&"app.active".to_owned()));
    assert_eq!(captures(source, "definition").len(), 1);
}

#[test]
fn callable_signature_includes_returned_columns_but_not_the_body() {
    let source = "CREATE FUNCTION app.rows(value INT) RETURNS TABLE (id INT) AS $body$ SELECT value; $body$ LANGUAGE SQL;";
    let signatures = captures(source, "signature");
    assert_eq!(signatures.len(), 2);
    assert_eq!(
        signatures[0].1,
        "CREATE FUNCTION app.rows(value INT) RETURNS TABLE (id INT)"
    );
    assert!(signatures[1].1.contains("SELECT value;"));
    assert_eq!(
        captures(source, "definition")
            .iter()
            .map(|(_, name, _, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["app.rows", "value", "id"]
    );
}
