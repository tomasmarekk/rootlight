//! Solidity syntax qualification for source ownership and incremental reuse.
//! Parsing fixtures never invokes a compiler, deploys contracts or proves dispatch.

use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, StreamingIterator};

const QUERY: &str =
    include_str!("../../../crates/rootlight-adapter-treesitter/queries/solidity.scm");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solidity::LANGUAGE.into())
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
    output.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    output
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

#[test]
fn solidity_declaration_forms_have_source_backed_names() {
    let source = r#"pragma solidity ^0.8.24;
import {Base as Parent} from "./Base.sol";
type Amount is uint256;
uint256 constant SCALE = 100;
error InvalidAmount(uint256 amount);
interface Reader { function read() external view returns (uint256); }
library Arithmetic { function twice(uint256 x) internal pure returns (uint256) { return x * 2; } }
abstract contract Base { function read() public view virtual returns (uint256); }
contract Ledger is Base {
    struct Entry { uint256 amount; address owner; }
    enum Phase { Open, Closed }
    mapping(address => uint256) public balances;
    event Changed(address indexed owner, uint256 amount);
    modifier nonzero(uint256 amount) { require(amount != 0); _; }
    constructor() {}
    receive() external payable {}
    fallback() external payable {}
    function read() public view override returns (uint256) { return balances[msg.sender]; }
    function put(uint256 amount) public nonzero(amount) { balances[msg.sender] = amount; emit Changed(msg.sender, amount); }
}
"#;
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    let all = nodes(tree.root_node());
    for (kind, name) in [
        ("interface_declaration", "Reader"),
        ("library_declaration", "Arithmetic"),
        ("contract_declaration", "Ledger"),
        ("struct_declaration", "Entry"),
        ("enum_declaration", "Phase"),
        ("state_variable_declaration", "balances"),
        ("event_definition", "Changed"),
        ("error_declaration", "InvalidAmount"),
        ("modifier_definition", "nonzero"),
        ("function_definition", "put"),
    ] {
        assert!(
            all.iter().any(|node| {
                node.kind() == kind
                    && node.child_by_field_name("name").is_some_and(|captured| {
                        captured.utf8_text(source.as_bytes()).unwrap() == name
                    })
            }),
            "{kind}: {name}"
        );
    }
    assert_eq!(
        all.iter()
            .filter(|node| node.kind() == "constructor_definition")
            .count(),
        1
    );
    assert_eq!(
        all.iter()
            .filter(|node| node.kind() == "fallback_receive_definition")
            .count(),
        2
    );
    for node in all {
        assert!(source.get(node.byte_range()).is_some());
    }
}

#[test]
fn solidity_comments_strings_and_assembly_keep_distinct_syntax_domains() {
    let source = r#"// contract Fake { function hidden() {} }
contract Real {
    string constant TEXT = "function phantom() {}";
    string constant LABEL = unicode"snow: 雪";
    bytes constant DATA = hex"ff00";
    function compute(uint256 input) public pure returns (uint256 output) {
        assembly ("memory-safe") { function helper(x) -> y { y := add(x, 1) } output := helper(input) }
    }
}
"#;
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let all = nodes(tree.root_node());
    let names = all
        .iter()
        .filter(|node| matches!(node.kind(), "contract_declaration" | "function_definition"))
        .map(|node| {
            node.child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["Real", "compute"]);
    assert!(
        all.iter()
            .any(|node| node.kind() == "yul_function_definition")
    );
    assert!(all.iter().any(|node| node.kind() == "comment"));
}

#[test]
fn solidity_incremental_edits_equal_fresh_structure_and_byte_ranges() {
    let mut source = "// source: 雪\ncontract Sample { function value() public pure returns (uint256) { return 10; } }\n".to_owned();
    let mut parser = parser();
    let mut tree = parser.parse(&source, None).unwrap();
    for (old, new) in [
        ("10", "2000"),
        ("Sample", "Renamed"),
        ("2000", "(4 + 5)"),
        ("return (4 + 5);", "assembly { mstore(0, 42) } return 42;"),
        ("雪", "δ"),
        ("public pure", "external pure"),
        ("value", "readValue"),
    ] {
        let start = source.find(old).unwrap();
        let end = start + old.len();
        let changed = source.replacen(old, new, 1);
        tree.edit(&InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: start + new.len(),
            start_position: point(&source, start),
            old_end_position: point(&source, end),
            new_end_position: point(&changed, start + new.len()),
        });
        let incremental = parser.parse(&changed, Some(&tree)).unwrap();
        let fresh = self::parser().parse(&changed, None).unwrap();
        assert!(
            !fresh.root_node().has_error(),
            "{}",
            fresh.root_node().to_sexp()
        );
        assert_eq!(
            incremental.root_node().to_sexp(),
            fresh.root_node().to_sexp()
        );
        let shape = |root| {
            nodes(root)
                .into_iter()
                .map(|node| {
                    (
                        node.kind_id(),
                        node.byte_range(),
                        node.start_position(),
                        node.end_position(),
                        node.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(incremental.root_node()), shape(fresh.root_node()));
        source = changed;
        tree = incremental;
    }
}

#[test]
fn solidity_recovery_keeps_all_reported_spans_inside_input() {
    for source in [
        "contract C { function broken(",
        "contract {",
        "/* unfinished",
        "contract C { string x = \"unfinished",
        "contract C { function f() public { assembly { let x := } } }",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(tree.root_node().has_error(), "{source}");
        for node in nodes(tree.root_node()) {
            assert!(
                source.get(node.byte_range()).is_some(),
                "{source}: {node:?}"
            );
        }
    }
}

#[test]
fn solidity_call_captures_preserve_complete_member_targets() {
    let language: tree_sitter::Language = tree_sitter_solidity::LANGUAGE.into();
    let query = Query::new(&language, QUERY).unwrap();
    let source = "contract Caller { function run(Peer peer) public { peer.apply(1); apply(2); factory()(3); this.apply(4); } }";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut targets = Vec::new();
    let mut calls = Vec::new();
    while let Some(found) = matches.next() {
        for capture in found.captures {
            let role = query.capture_names()[usize::try_from(capture.index).unwrap()];
            let text = capture.node.utf8_text(source.as_bytes()).unwrap();
            match role {
                "call_name" => targets.push(text),
                "call" => calls.push(text),
                _ => {}
            }
        }
    }
    targets.sort_unstable();
    calls.sort_unstable();
    assert_eq!(targets, ["apply", "factory", "peer.apply", "this.apply"]);
    assert_eq!(
        calls,
        [
            "apply(2)",
            "factory()",
            "factory()(3)",
            "peer.apply(1)",
            "this.apply(4)"
        ]
    );
}

#[test]
fn solidity_source_queries_capture_definitions_without_editor_highlight_assumptions() {
    let language: tree_sitter::Language = tree_sitter_solidity::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    let query = Query::new(&language, QUERY).unwrap();
    let source = "contract Ledger { uint256 public total; constructor() {} receive() external payable {} fallback() external payable {} function put(uint256 value) public { total = value; } }";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut definitions = Vec::new();
    while let Some(found) = matches.next() {
        for capture in found.captures {
            if query.capture_names()[usize::try_from(capture.index).unwrap()] == "definition" {
                definitions.push(capture.node.utf8_text(source.as_bytes()).unwrap());
            }
        }
    }
    definitions.sort_unstable();
    assert_eq!(
        definitions,
        [
            "Ledger",
            "constructor",
            "fallback",
            "put",
            "receive",
            "total",
            "value"
        ]
    );
    // The pinned upstream editor query contains invalid grouping at its struct capture.
    // Rootlight's extraction contract must compile independently of that unused asset.
    let error = Query::new(&language, tree_sitter_solidity::HIGHLIGHT_QUERY).unwrap_err();
    assert_eq!(error.kind, tree_sitter::QueryErrorKind::Syntax);
    assert_eq!(error.row, 89);
    Query::new(&language, tree_sitter_solidity::LOCALS_QUERY).unwrap();
    Query::new(&language, tree_sitter_solidity::TAGS_QUERY).unwrap();
}
