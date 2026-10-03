//! Authored F# implementation/signature syntax and scanner-state qualification.
//! Native source spans are evidence only, not type checking or cross-file binding proof.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

const IMPLEMENTATION: &str = "module Example.Core\nopen System\nlet seed = 1\nlet advance input = input + seed\ntype Entry = { Count: int; Name: string }\ntype Choice = | Empty | Item of int\ntype Counter(start: int) =\n    member this.Value = start\n    member this.Advance input = input + start\n";

const SIGNATURE: &str = "module Example.Core\nopen System\nval seed : int\nval advance : input:int -> int\ntype Entry = { Count: int; Name: string }\ntype Choice = | Empty | Item of int\n";

fn parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(
            &if signature {
                tree_sitter_fsharp::LANGUAGE_SIGNATURE
            } else {
                tree_sitter_fsharp::LANGUAGE_FSHARP
            }
            .into(),
        )
        .expect("pinned native grammar is incompatible with Tree-sitter");
    parser
}

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = source.get(..offset).expect("node offset exceeds fixture");
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn bounded_parse(parser: &mut Parser, source: &[u8], old: Option<&Tree>) -> Tree {
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
    parser
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            old,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("native fixture exceeded its parse watchdog")
}

#[derive(Debug, PartialEq, Eq)]
struct Witness {
    kind: String,
    span: Range<usize>,
    depth: usize,
    named: bool,
    error: bool,
    missing: bool,
}

fn inventory(tree: &Tree, source: &[u8]) -> Vec<Witness> {
    let mut pending = vec![(tree.root_node(), 0_usize)];
    let mut result = Vec::new();
    while let Some((node, depth)) = pending.pop() {
        assert!(
            result.len() < 100_000,
            "native fixture node count exceeded its bound"
        );
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        result.push(Witness {
            kind: node.kind().to_owned(),
            span: node.byte_range(),
            depth,
            named: node.is_named(),
            error: node.is_error(),
            missing: node.is_missing(),
        });
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("fixture child index exceeds native API");
            pending.push((
                node.child(index).expect("advertised child is absent"),
                depth + 1,
            ));
        }
    }
    result
}

fn clean(tree: &Tree, source: &[u8]) {
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().end_byte(), source.len());
    assert!(
        inventory(tree, source)
            .iter()
            .all(|node| !node.error && !node.missing)
    );
}

fn nodes<'tree>(tree: &'tree Tree, kind: &str) -> Vec<Node<'tree>> {
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

fn exact_node<'tree>(tree: &'tree Tree, kind: &str, source: &str, written: &str) -> Node<'tree> {
    let start = source.find(written).expect("authored spelling is absent");
    let matches: Vec<_> = nodes(tree, kind)
        .into_iter()
        .filter(|node| node.byte_range() == (start..start + written.len()))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "{kind} {written:?}: {}",
        tree.root_node().to_sexp()
    );
    let node = matches[0];
    assert_eq!(
        source.as_bytes().get(node.byte_range()).unwrap(),
        written.as_bytes()
    );
    assert_eq!(node.start_position(), point(source.as_bytes(), start));
    assert_eq!(
        node.end_position(),
        point(source.as_bytes(), start + written.len())
    );
    node
}

fn exact_field<'tree>(owner: Node<'tree>, field: &str, source: &str, written: &str) -> Node<'tree> {
    let node = owner
        .child_by_field_name(field)
        .unwrap_or_else(|| panic!("{field} field is absent from {}", owner.to_sexp()));
    let owner_text = source.get(owner.byte_range()).unwrap();
    let start = owner.start_byte() + owner_text.find(written).unwrap();
    assert_eq!(node.byte_range(), start..start + written.len());
    assert_eq!(
        source.as_bytes().get(node.byte_range()).unwrap(),
        written.as_bytes()
    );
    assert_eq!(node.parent(), Some(owner));
    assert_eq!(node.start_position(), point(source.as_bytes(), start));
    assert_eq!(
        node.end_position(),
        point(source.as_bytes(), start + written.len())
    );
    node
}

fn source_variant(fixture: &str, newline: &str, trailing_newline: bool) -> String {
    let text = if trailing_newline {
        fixture
    } else {
        fixture.strip_suffix('\n').unwrap()
    };
    text.replace('\n', newline)
}

fn common_headers(tree: &Tree, source: &str) {
    let module_name = exact_node(tree, "long_identifier", source, "Example.Core");
    let module = module_name.parent().expect("module name has no owner");
    assert_eq!(module.kind(), "named_module");
    assert_eq!(module.child_by_field_name("name"), Some(module_name));
    assert_eq!(module.start_byte(), 0);
    let import = exact_node(tree, "import_decl", source, "open System");
    let import_name = exact_node(tree, "long_identifier", source, "System");
    assert_eq!(import_name.parent(), Some(import));
    for (name, owner_kind) in [("Entry", "record_type_defn"), ("Choice", "union_type_defn")] {
        let header = exact_node(tree, "type_name", source, name);
        let identifier = exact_node(tree, "identifier", source, name);
        let written_name = header
            .child_by_field_name("type_name")
            .expect("written type name field is absent");
        assert_eq!(written_name.byte_range(), identifier.byte_range());
        assert_eq!(
            source.as_bytes().get(written_name.byte_range()).unwrap(),
            name.as_bytes()
        );
        let owner = header.parent().expect("type header has no declaration");
        assert_eq!(owner.kind(), owner_kind);
        assert!(owner.start_byte() == header.start_byte() && owner.end_byte() > header.end_byte());
    }
    exact_node(tree, "record_field", source, "Count: int");
    exact_node(tree, "record_field", source, "Name: string");
    exact_node(tree, "union_type_case", source, "Empty");
    exact_node(tree, "union_type_case", source, "Item of int");
}

#[test]
fn implementation_headers_names_and_bodies_keep_original_ranges_at_line_endings_and_eof() {
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            let source = source_variant(IMPLEMENTATION, newline, trailing_newline);
            let tree = bounded_parse(&mut parser(false), source.as_bytes(), None);
            clean(&tree, source.as_bytes());
            common_headers(&tree, &source);
            let function = exact_node(&tree, "function_declaration_left", &source, "advance input");
            let name = exact_node(&tree, "identifier", &source, "advance");
            assert_eq!(name.parent(), Some(function));
            let binding = function.parent().expect("function header has no binding");
            assert_eq!(binding.kind(), "function_or_value_defn");
            let body = binding
                .child_by_field_name("body")
                .expect("binding body is absent");
            assert_eq!(
                source.as_bytes().get(body.byte_range()).unwrap(),
                b"input + seed"
            );
            assert!(function.end_byte() < body.start_byte());
            exact_node(&tree, "value_declaration_left", &source, "seed");
            let class = exact_node(&tree, "type_name", &source, "Counter");
            assert_eq!(class.parent().unwrap().kind(), "anon_type_defn");
            assert_eq!(nodes(&tree, "member_defn").len(), 2);
            for name in ["this.Value", "this.Advance"] {
                let header = exact_node(&tree, "property_or_ident", &source, name);
                let member = header.parent().expect("member header has no owner");
                assert_eq!(member.kind(), "method_or_prop_defn");
                assert_eq!(member.child_by_field_name("name"), Some(header));
                let terminal = header.child_by_field_name("method").unwrap();
                assert_eq!(
                    source.as_bytes().get(terminal.byte_range()).unwrap(),
                    name.split_once('.').unwrap().1.as_bytes()
                );
                assert!(header.end_byte() < member.end_byte());
            }
        }
    }
}

#[test]
fn type_abbreviation_syntax_does_not_invent_an_unmarked_union_case() {
    // Bare RHS names can be reinterpreted semantically by the compiler. These
    // controls pin only its syntax frontier, never type-environment resolution.
    for (written, kind) in [
        ("Alias = string", "type_abbrev_defn"),
        ("Alias = Known", "type_abbrev_defn"),
        ("Alias = ``known type``", "type_abbrev_defn"),
        ("Self = Self", "type_abbrev_defn"),
        ("Alias = System.String", "type_abbrev_defn"),
        ("Alias = list<int>", "type_abbrev_defn"),
        ("Alias = (string)", "type_abbrev_defn"),
        ("Alias = (int * string)", "type_abbrev_defn"),
        ("Alias = int list", "type_abbrev_defn"),
        ("Alias = int -> string", "type_abbrev_defn"),
        ("Wrapper = Wrapped of int", "union_type_defn"),
        ("Marker = | Marker", "union_type_defn"),
        ("Choice = Left | Right", "union_type_defn"),
        ("Choice = Left (* separator *) | Right", "union_type_defn"),
        ("Choice = Left of int | Right", "union_type_defn"),
        ("Lower = | ``lower case``", "union_type_defn"),
        ("Lower = lower of int", "union_type_defn"),
        ("Typed = Case : int -> Typed", "union_type_defn"),
    ] {
        for signature in [false, true] {
            for newline in ["\n", "\r\n"] {
                for trailing_newline in [false, true] {
                    let fixture = format!("module Shapes\ntype {written}\n");
                    let source = source_variant(&fixture, newline, trailing_newline);
                    let tree = bounded_parse(&mut parser(signature), source.as_bytes(), None);
                    clean(&tree, source.as_bytes());
                    exact_node(&tree, kind, &source, written);
                    if kind == "type_abbrev_defn" {
                        assert!(nodes(&tree, "union_type_case").is_empty());
                        assert!(nodes(&tree, "union_type_defn").is_empty());
                    }
                }
            }
        }
    }
}

#[test]
fn signature_values_records_and_unions_keep_written_declaration_and_header_ranges() {
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            let source = source_variant(SIGNATURE, newline, trailing_newline);
            let tree = bounded_parse(&mut parser(true), source.as_bytes(), None);
            clean(&tree, source.as_bytes());
            common_headers(&tree, &source);
            assert_eq!(nodes(&tree, "value_definition").len(), 2);
            for (header, declaration) in [
                ("seed", "val seed : int"),
                ("advance", "val advance : input:int -> int"),
            ] {
                let owner = exact_node(&tree, "value_definition", &source, declaration);
                let name = exact_node(&tree, "value_declaration_left", &source, header);
                assert_eq!(name.parent(), Some(owner));
                assert!(owner.child_by_field_name("body").is_none());
                let terminal = exact_node(&tree, "identifier", &source, header);
                assert!(
                    name.start_byte() <= terminal.start_byte()
                        && terminal.end_byte() <= name.end_byte()
                );
            }
            exact_node(&tree, "argument_name_spec", &source, "input:");
            assert!(nodes(&tree, "function_declaration_left").is_empty());
        }
    }
}

#[test]
fn signature_class_constructors_and_members_are_declarations_without_implementation_bodies() {
    // Ordinary .fsi class signatures describe declarations, not implementation bodies:
    // https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/signature-files
    let fixture = "module Classes\ntype Counter =\n    new : start:int -> Counter\n    member Value : int\n    member Advance : input:int -> int\n";
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            let source = source_variant(fixture, newline, trailing_newline);
            let tree = bounded_parse(&mut parser(true), source.as_bytes(), None);
            clean(&tree, source.as_bytes());
            exact_node(&tree, "type_name", &source, "Counter");
            let constructor = exact_node(
                &tree,
                "constructor_signature",
                &source,
                "new : start:int -> Counter",
            );
            let constructor_type =
                exact_field(constructor, "signature", &source, "start:int -> Counter");
            assert_eq!(constructor_type.kind(), "curried_spec");
            assert!(constructor.child_by_field_name("name").is_none());
            assert!(constructor.child_by_field_name("body").is_none());
            assert_eq!(constructor.parent().unwrap().kind(), "member_defn");
            for (name, declaration, written_type) in [
                ("Value", "Value : int", "int"),
                ("Advance", "Advance : input:int -> int", "input:int -> int"),
            ] {
                let terminal = exact_node(&tree, "identifier", &source, name);
                assert!(terminal.start_byte() > source.find("type Counter").unwrap());
                let signature = exact_node(&tree, "member_signature", &source, declaration);
                assert_eq!(exact_field(signature, "name", &source, name), terminal);
                let written_type = exact_field(signature, "signature", &source, written_type);
                assert_eq!(written_type.kind(), "curried_spec");
                assert!(signature.child_by_field_name("body").is_none());
                let member = signature.parent().unwrap();
                assert_eq!(member.kind(), "member_defn");
                assert!(member.child_by_field_name("body").is_none());
            }
            exact_node(&tree, "new", &source, "new");
            assert_eq!(nodes(&tree, "constructor_signature").len(), 1);
            assert_eq!(nodes(&tree, "member_signature").len(), 2);
            assert_eq!(nodes(&tree, "member").len(), 2);
            assert_eq!(nodes(&tree, "=").len(), 1);
            assert!(nodes(&tree, "function_or_value_defn").is_empty());
        }
    }
}

#[test]
fn signature_static_abstract_generic_and_property_members_end_before_following_values() {
    let fixture = "module SignatureMembers\ntype Counter =\n    class\n        new : start:int -> Counter\n        static member Create : unit -> Counter\n        abstract member Advance : input:int -> int\n        member Value : int with get, set\n        member Convert<'T> : input:'T -> 'T\n    end\nval after : int\n[<Literal>]\nval Marker : int = 7\n";
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            let source = source_variant(fixture, newline, trailing_newline);
            let tree = bounded_parse(&mut parser(true), source.as_bytes(), None);
            clean(&tree, source.as_bytes());
            let header = exact_node(&tree, "type_name", &source, "Counter");
            let class = header.parent().unwrap();
            assert_eq!(class.kind(), "anon_type_defn");
            let constructor = exact_node(
                &tree,
                "constructor_signature",
                &source,
                "new : start:int -> Counter",
            );
            assert_eq!(
                exact_field(constructor, "signature", &source, "start:int -> Counter").kind(),
                "curried_spec"
            );
            for (name, declaration, written_type) in [
                ("Create", "Create : unit -> Counter", "unit -> Counter"),
                ("Advance", "Advance : input:int -> int", "input:int -> int"),
                ("Value", "Value : int with get, set", "int"),
                ("Convert", "Convert<'T> : input:'T -> 'T", "input:'T -> 'T"),
            ] {
                let signature = exact_node(&tree, "member_signature", &source, declaration);
                assert_eq!(
                    exact_field(signature, "name", &source, name).kind(),
                    "identifier"
                );
                assert_eq!(
                    exact_field(signature, "signature", &source, written_type).kind(),
                    "curried_spec"
                );
                assert!(signature.child_by_field_name("body").is_none());
                assert_eq!(signature.parent().unwrap().kind(), "member_defn");
                assert!(
                    header.end_byte() < signature.start_byte()
                        && signature.end_byte() <= class.end_byte()
                );
            }
            exact_node(
                &tree,
                "member_defn",
                &source,
                "static member Create : unit -> Counter",
            );
            exact_node(
                &tree,
                "member_defn",
                &source,
                "abstract member Advance : input:int -> int",
            );
            exact_node(&tree, "type_arguments", &source, "<'T>");
            exact_node(&tree, "get", &source, "get");
            exact_node(&tree, "set", &source, "set");
            let end = exact_node(&tree, "end", &source, "end");
            assert!(end.end_byte() <= class.end_byte());
            let after = exact_node(&tree, "value_definition", &source, "val after : int");
            assert!(class.end_byte() <= after.start_byte());
            assert_eq!(after.parent().unwrap().kind(), "named_module");
            assert!(after.child_by_field_name("body").is_none());
            let literal = format!("[<Literal>]{newline}val Marker : int = 7");
            let literal = exact_node(&tree, "value_definition", &source, &literal);
            assert_eq!(literal.parent(), after.parent());
            assert_eq!(
                exact_field(literal, "body", &source, "7")
                    .utf8_text(source.as_bytes())
                    .unwrap(),
                "7"
            );
            assert_eq!(nodes(&tree, "constructor_signature").len(), 1);
            assert_eq!(nodes(&tree, "member_signature").len(), 4);
            assert_eq!(nodes(&tree, "value_definition").len(), 2);
            assert_eq!(nodes(&tree, "=").len(), 2);
            for kind in [
                "function_or_value_defn",
                "method_or_prop_defn",
                "additional_constr_defn",
                "primary_constr_args",
            ] {
                assert!(nodes(&tree, kind).is_empty(), "unexpected {kind}");
            }
        }
    }
}

#[test]
fn signature_class_implementation_bodies_and_malformed_member_types_remain_recovery() {
    let invalid = [
        "module Broken\ntype Counter =\n    new start:int -> Counter\n",
        "module Broken\ntype Counter =\n    new :\n",
        "module Broken\ntype Counter =\n    new : int ->\n",
        "module Broken\ntype Counter =\n    member Value int\n",
        "module Broken\ntype Counter =\n    member Value :\n",
        "module Broken\ntype Counter =\n    static member Create : unit ->\n",
        "module Broken\ntype Counter =\n    abstract member Advance : int ->\n",
        "module Broken\ntype Counter =\n    member this.Value = 1\n",
        "module Broken\ntype Counter =\n    member Value : int = 1\n",
        "module Broken\ntype Counter =\n    new(start:int) = Counter(start)\n",
        "module Broken\ntype Counter(start:int) =\n    member Value : int\n",
        "module Broken\ntype Counter =\n    let value = 1\n",
        "module Broken\ntype Counter =\n    do ()\n",
        "module Broken\ntype Counter =\n    interface System.IDisposable with\n        member this.Dispose() = ()\n",
        "module Broken\ntype Counter =\n    inherit Base(1)\n",
        "module Broken\ntype Counter =\n    class\n        member Value : int\n",
    ];
    let mut reused = parser(true);
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            for fixture in invalid {
                let source = source_variant(fixture, newline, trailing_newline);
                let tree = bounded_parse(&mut reused, source.as_bytes(), None);
                assert!(
                    tree.root_node().has_error(),
                    "{source:?}: {}",
                    tree.root_node().to_sexp()
                );
                let evidence = inventory(&tree, source.as_bytes());
                assert!(
                    evidence.iter().any(|node| node.error || node.missing)
                        || tree.root_node().to_sexp().contains("MISSING"),
                    "recovery has no visible or serialized error witness: {source:?}"
                );
                let next = bounded_parse(&mut reused, SIGNATURE.as_bytes(), None);
                let fresh = bounded_parse(&mut parser(true), SIGNATURE.as_bytes(), None);
                clean(&next, SIGNATURE.as_bytes());
                assert_eq!(
                    inventory(&next, SIGNATURE.as_bytes()),
                    inventory(&fresh, SIGNATURE.as_bytes())
                );
            }
        }
    }
}

#[test]
fn unicode_identifiers_quoted_names_strings_and_nested_comments_preserve_bytes_and_points() {
    for signature in [false, true] {
        let fixture = if signature {
            "module UnicodeSample\n(* outer (* λ😀 *) end *)\nval λvalue : int\nval ``display λ`` : string\n"
        } else {
            "module UnicodeSample\n(* outer (* λ😀 *) end *)\nlet λvalue = 1\nlet ``display λ`` = \"λ😀\"\n"
        };
        for newline in ["\n", "\r\n"] {
            let source = fixture.replace('\n', newline);
            let tree = bounded_parse(&mut parser(signature), source.as_bytes(), None);
            clean(&tree, source.as_bytes());
            exact_node(&tree, "identifier", &source, "λvalue");
            exact_node(&tree, "identifier", &source, "``display λ``");
            exact_node(&tree, "block_comment", &source, "(* outer (* λ😀 *) end *)");
            if !signature {
                exact_node(&tree, "string", &source, "\"λ😀\"");
            }
            let quoted = exact_node(&tree, "value_declaration_left", &source, "``display λ``");
            assert_eq!(quoted.start_position().row, 3);
            assert_eq!(quoted.start_position().column, 4);
        }
    }
}

#[test]
fn malformed_and_truncated_declarations_remain_recovery_with_parser_reuse() {
    for signature in [false, true] {
        let fixture = if signature { SIGNATURE } else { IMPLEMENTATION };
        let invalid: &[&str] = if signature {
            &[
                "module Broken\nval value :",
                "module Broken\ntype Item = { Count: }",
                "module Broken\nval value : (int ->",
            ]
        } else {
            &[
                "module Broken\nlet value =",
                "module Broken\nlet value = (1 +",
                "module Broken\nlet value = \"unterminated",
                "module Broken\ntype Item = { Count: }",
            ]
        };
        let mut reused = parser(signature);
        for newline in ["\n", "\r\n"] {
            for trailing_newline in [false, true] {
                for source in invalid {
                    let mut source = source.replace('\n', newline);
                    if trailing_newline {
                        source.push_str(newline);
                    }
                    let tree = bounded_parse(&mut reused, source.as_bytes(), None);
                    assert!(
                        tree.root_node().has_error(),
                        "signature {signature}, {source:?}: {}",
                        tree.root_node().to_sexp()
                    );
                    let evidence = inventory(&tree, source.as_bytes());
                    assert!(
                        evidence.iter().any(|node| node.error || node.missing)
                            || tree.root_node().to_sexp().contains("MISSING"),
                        "recovery has no visible or serialized error witness"
                    );
                    let next = bounded_parse(&mut reused, fixture.as_bytes(), None);
                    clean(&next, fixture.as_bytes());
                    let fresh = bounded_parse(&mut parser(signature), fixture.as_bytes(), None);
                    assert_eq!(
                        inventory(&next, fixture.as_bytes()),
                        inventory(&fresh, fixture.as_bytes())
                    );
                }
            }
        }
    }
}

#[test]
fn indentation_columns_above_one_byte_keep_inner_and_following_bindings_separate() {
    for newline in ["\n", "\r\n"] {
        for width in [256_usize, 300, 65_536] {
            let indent = " ".repeat(width);
            let source = format!(
                "module Deep{newline}let outer input ={newline}{indent}let inner = input + 1{newline}{indent}inner{newline}let after = 2{newline}"
            );
            let mut reused = parser(false);
            let tree = bounded_parse(&mut reused, source.as_bytes(), None);
            clean(&tree, source.as_bytes());
            let outer = exact_node(&tree, "function_declaration_left", &source, "outer input");
            let inner = exact_node(&tree, "value_declaration_left", &source, "inner");
            let after = exact_node(&tree, "value_declaration_left", &source, "after");
            assert_eq!(inner.start_position().column, width + 4);
            assert_eq!(after.start_position().column, 4);
            let outer_binding = outer.parent().expect("function header has no binding");
            assert!(
                outer_binding.start_byte() <= inner.start_byte()
                    && inner.end_byte() <= outer_binding.end_byte()
            );
            assert!(outer_binding.end_byte() <= after.start_byte());
            let mut edit_tree = tree.clone();
            let offset = source.find("+ 1").unwrap() + 2;
            let mut changed = source.clone().into_bytes();
            changed[offset] = b'3';
            edit_tree.edit(&InputEdit {
                start_byte: offset,
                old_end_byte: offset + 1,
                new_end_byte: offset + 1,
                start_position: point(source.as_bytes(), offset),
                old_end_position: point(source.as_bytes(), offset + 1),
                new_end_position: point(&changed, offset + 1),
            });
            let incremental = bounded_parse(&mut reused, &changed, Some(&edit_tree));
            let fresh = bounded_parse(&mut parser(false), &changed, None);
            clean(&incremental, &changed);
            assert_eq!(
                inventory(&incremental, &changed),
                inventory(&fresh, &changed),
                "indentation {width}, newline {newline:?}"
            );
        }
    }
}

#[test]
fn every_byte_damage_and_restore_matches_a_fresh_layout_tree() {
    for signature in [false, true] {
        let fixture = if signature {
            "module Edits\nmodule Nested =\n    val value : int -> int\nval after : int\n"
        } else {
            "module Edits\nlet outer input =\n    let inner = input + 1\n    inner\nlet after = 2\n"
        };
        for newline in ["\n", "\r\n"] {
            let source = fixture.replace('\n', newline).into_bytes();
            let mut reused = parser(signature);
            let original = bounded_parse(&mut reused, &source, None);
            clean(&original, &source);
            let expected = inventory(&original, &source);
            for offset in 0..source.len() {
                let mut damaged = source.clone();
                damaged[offset] = if source[offset] == b'?' { 0xff } else { b'?' };
                let mut old = original.clone();
                old.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&source, offset),
                    old_end_position: point(&source, offset + 1),
                    new_end_position: point(&damaged, offset + 1),
                });
                let mut updated = bounded_parse(&mut reused, &damaged, Some(&old));
                let fresh = bounded_parse(&mut parser(signature), &damaged, None);
                assert_eq!(
                    inventory(&updated, &damaged),
                    inventory(&fresh, &damaged),
                    "signature {signature}, newline {newline:?}, damage at {offset}"
                );
                updated.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&damaged, offset),
                    old_end_position: point(&damaged, offset + 1),
                    new_end_position: point(&source, offset + 1),
                });
                let restored = bounded_parse(&mut reused, &source, Some(&updated));
                clean(&restored, &source);
                assert_eq!(
                    inventory(&restored, &source),
                    expected,
                    "signature {signature}, newline {newline:?}, restore at {offset}"
                );
            }
        }
    }
}

#[test]
fn cancellation_then_reset_keeps_the_next_complete_document_exact() {
    for signature in [false, true] {
        let line = if signature {
            "val value : int -> int\n"
        } else {
            "let value input = input + 1\n"
        };
        let source = format!("module Work\n{}", line.repeat(10_000));
        let mut reused = parser(signature);
        let mut called = false;
        let mut cancel = |_: &tree_sitter::ParseState| {
            called = true;
            ControlFlow::Break(())
        };
        let result = reused.parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut cancel)),
        );
        assert!(
            called && result.is_none(),
            "signature {signature}: parse did not cancel"
        );
        reused.reset();
        let source = if signature { SIGNATURE } else { IMPLEMENTATION };
        let next = bounded_parse(&mut reused, source.as_bytes(), None);
        let fresh = bounded_parse(&mut parser(signature), source.as_bytes(), None);
        clean(&next, source.as_bytes());
        assert_eq!(
            inventory(&next, source.as_bytes()),
            inventory(&fresh, source.as_bytes())
        );
    }
}
