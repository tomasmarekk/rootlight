//! F# enum separators retain exact ownership at the bounded offside frontier.
//! Negative compiler recoveries do not imply retention or loss of later native
//! declarations. Numeric spellings are checked as source syntax, not evaluated.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};
use tree_sitter::{Node, ParseOptions, Parser, Point, Tree};

const UNPREFIXED_THREE: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
   | Next = 1
let following = 30
"#;
const UNPREFIXED_FOUR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
    | Next = 1
let following = 30
"#;
const UNPREFIXED_FIVE: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
     | Next = 1
let following = 30
"#;
const UNPREFIXED_SIX: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
      | Next = 1
let following = 30
"#;
const BARS_THREE: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      | First = 0
   | Next = 1
let following = 30
"#;
const BARS_FOUR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      | First = 0
    | Next = 1
let following = 30
"#;
const BARS_FIVE: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      | First = 0
     | Next = 1
let following = 30
"#;
const BARS_SIX: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      | First = 0
      | Next = 1
let following = 30
"#;
const SIGNED_THREE: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = -1
   | Next = +2
let following = 30
"#;
const SIGNED_FOUR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = -1
    | Next = +2
let following = 30
"#;
const NESTED_EIGHT: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
module Nested =
    type Sample =
          First = 0
        | Next = 1
    let following = 30
let outerFollowing = 40
"#;
const NESTED_FLOOR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
module Nested =
    type Sample =
          First = 0
    | Next = 1
    let following = 30
let outerFollowing = 40
"#;
const INLINE_FIRST: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample = First = 0
    | Next = 1
let following = 30
"#;
const MODULE_FLOOR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
| Next = 1
let following = 30
"#;
const UNICODE_FOUR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
    |λ = 1
let following = 30
"#;
const COMMENT_FOUR: &str = r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
    |(* gap *) Next = 1
let following = 30
"#;

fn offside_parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_fsharp::LANGUAGE_FSHARP.into())
        .expect("pinned grammar is incompatible with Tree-sitter");
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
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("authored enum fixture exceeded its cooperative parse watchdog")
}

fn offside_nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(
            found.len() < 100_000,
            "authored node inventory exceeds its bound"
        );
        found.push(node);
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("child index exceeds native API");
            pending.push(node.child(index).expect("advertised child is absent"));
        }
    }
    found
}

fn offside_kind<'tree>(tree: &'tree Tree, kind: &str) -> Vec<Node<'tree>> {
    offside_nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind)
        .collect()
}

fn offside_children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| {
            let index = u32::try_from(index).expect("named child index exceeds native API");
            node.named_child(index)
                .expect("advertised named child is absent")
        })
        .filter(|child| !child.is_extra())
        .collect()
}

fn offside_point(source: &str, offset: usize) -> Point {
    let prefix = source
        .as_bytes()
        .get(..offset)
        .expect("offset escapes source");
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn offside_ranges(tree: &Tree, source: &str) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().start_byte(), 0);
    assert_eq!(tree.root_node().end_byte(), source.len());
    for node in offside_nodes(tree) {
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(
            node.start_position(),
            offside_point(source, node.start_byte())
        );
        assert_eq!(node.end_position(), offside_point(source, node.end_byte()));
        if let Some(parent) = node.parent() {
            assert!(parent.start_byte() <= node.start_byte());
            assert!(node.end_byte() <= parent.end_byte());
        }
    }
}

fn offside_written(node: Node<'_>, source: &str, kind: &str, text: &str) {
    assert_eq!(node.kind(), kind);
    assert_eq!(source.get(node.byte_range()), Some(text));
    assert_eq!(
        node.start_position(),
        offside_point(source, node.start_byte())
    );
    assert_eq!(node.end_position(), offside_point(source, node.end_byte()));
}

fn offside_exact<'tree>(tree: &'tree Tree, source: &str, kind: &str, text: &str) -> Node<'tree> {
    let found: Vec<_> = offside_kind(tree, kind)
        .into_iter()
        .filter(|node| source.get(node.byte_range()) == Some(text))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
    offside_written(found[0], source, kind, text);
    found[0]
}

fn offside_field<'tree>(
    owner: Node<'tree>,
    source: &str,
    field: &str,
    kind: &str,
    text: &str,
) -> Node<'tree> {
    let child = owner
        .child_by_field_name(field)
        .expect("written field is absent");
    offside_written(child, source, kind, text);
    assert_eq!(child.parent(), Some(owner));
    child
}

fn offside_case<'tree>(
    tree: &'tree Tree,
    source: &str,
    block: Node<'tree>,
    name: &str,
    rhs: &str,
) -> Node<'tree> {
    let text = format!("{name} = {rhs}");
    let case = offside_exact(tree, source, "enum_type_case", &text);
    assert_eq!(case.parent(), Some(block));
    assert_eq!(
        case.start_byte(),
        source.find(&text).expect("written case is absent")
    );
    let children = offside_children(case);
    assert_eq!(children.len(), 2);
    offside_written(children[0], source, "identifier", name);
    offside_written(children[1], source, "const", rhs);
    assert_eq!(case.start_byte(), children[0].start_byte());
    assert_eq!(case.end_byte(), children[1].end_byte());
    for child in &children {
        assert_eq!(child.parent(), Some(case));
    }
    let value_children = offside_children(children[1]);
    assert_eq!(value_children.len(), 1);
    if let Some(digits) = rhs.strip_suffix('u') {
        offside_written(value_children[0], source, "uint32", rhs);
        let digits_nodes = offside_children(value_children[0]);
        assert_eq!(digits_nodes.len(), 1);
        offside_written(digits_nodes[0], source, "int", digits);
        assert_eq!(digits_nodes[0].parent(), Some(value_children[0]));
    } else {
        offside_written(value_children[0], source, "int", rhs);
    }
    assert_eq!(value_children[0].parent(), Some(children[1]));
    case
}

fn offside_binding<'tree>(
    tree: &'tree Tree,
    source: &str,
    owner: Node<'tree>,
    name: &str,
    rhs: &str,
) -> Node<'tree> {
    let text = format!("let {name} = {rhs}");
    let wrapper = offside_exact(tree, source, "declaration_expression", &text);
    let definition = offside_exact(tree, source, "function_or_value_defn", &text);
    assert_eq!(
        wrapper.start_byte(),
        source.find(&text).expect("binding text is absent")
    );
    assert_eq!(wrapper.parent(), Some(owner));
    assert_eq!(definition.parent(), Some(wrapper));
    assert_eq!(offside_children(wrapper), vec![definition]);
    let left = offside_exact(tree, source, "value_declaration_left", name);
    let pattern = offside_exact(tree, source, "identifier_pattern", name);
    assert_eq!(left.parent(), Some(definition));
    assert_eq!(pattern.parent(), Some(left));
    let body = offside_field(definition, source, "body", "const", rhs);
    let numbers = offside_children(body);
    assert_eq!(numbers.len(), 1);
    offside_written(numbers[0], source, "int", rhs);
    assert_eq!(numbers[0].parent(), Some(body));
    assert_eq!(offside_children(definition), vec![left, body]);
    assert_eq!(definition.end_byte(), body.end_byte());
    wrapper
}

fn offside_variants(fixture: &str, mut check: impl FnMut(&str)) {
    assert!(!fixture.contains('\r'), "canonical source must use LF");
    let eof = fixture
        .strip_suffix('\n')
        .expect("canonical source must end in LF");
    for newline in ["\n", "\r\n"] {
        for source in [eof, fixture] {
            check(&source.replace('\n', newline));
        }
    }
}

fn offside_positive(fixture: &str, cases: &[(&str, &str)], prefixed: bool, nested: bool) {
    offside_variants(fixture, |source| {
        let tree = offside_parse(source);
        offside_ranges(&tree, source);
        assert!(
            !tree.root_node().has_error(),
            "{source:?}: {}",
            tree.root_node().to_sexp()
        );
        for node in offside_nodes(&tree) {
            assert!(!node.is_error() && !node.is_missing());
        }
        let modules = offside_kind(&tree, "named_module");
        assert_eq!(modules.len(), 1);
        let module = modules[0];
        assert_eq!(module.parent(), Some(tree.root_node()));
        let module_name = offside_field(module, source, "name", "long_identifier", "EnumExample");
        let nested_modules = offside_kind(&tree, "module_defn");
        assert_eq!(nested_modules.len(), usize::from(nested));
        let owner = if nested {
            let inner = nested_modules[0];
            assert_eq!(inner.parent(), Some(module));
            assert_eq!(inner.start_byte(), source.find("module Nested =").unwrap());
            inner
        } else {
            module
        };
        let definitions = offside_kind(&tree, "enum_type_defn");
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        let wrapper = definition.parent().expect("enum definition has no wrapper");
        assert_eq!(wrapper.kind(), "type_definition");
        assert_eq!(wrapper.parent(), Some(owner));
        assert_eq!(offside_children(wrapper), vec![definition]);
        assert_eq!(wrapper.start_byte(), source.find("type Sample").unwrap());
        let type_name = offside_exact(&tree, source, "type_name", "Sample");
        assert_eq!(type_name.parent(), Some(definition));
        offside_field(type_name, source, "type_name", "identifier", "Sample");
        let block = definition
            .child_by_field_name("block")
            .expect("scoped enum body is absent");
        assert_eq!(block.kind(), "enum_type_cases");
        assert_eq!(block.parent(), Some(definition));
        assert_eq!(offside_children(definition), vec![type_name, block]);
        let found: Vec<_> = cases
            .iter()
            .map(|(name, rhs)| offside_case(&tree, source, block, name, rhs))
            .collect();
        assert_eq!(offside_children(block), found);
        assert_eq!(offside_kind(&tree, "enum_type_case"), found);
        assert_eq!(found.len(), 2);
        assert_eq!(block.end_byte(), found[1].end_byte());
        assert!(found[0].end_byte() < found[1].start_byte());
        let bars: Vec<_> = (0..block.child_count())
            .map(|index| block.child(u32::try_from(index).unwrap()).unwrap())
            .filter(|node| node.kind() == "|")
            .collect();
        let written_bars: Vec<_> = source
            .match_indices('|')
            .map(|(offset, _)| offset)
            .collect();
        assert_eq!(bars.len(), if prefixed { 2 } else { 1 });
        assert_eq!(bars.len(), written_bars.len());
        assert_eq!(offside_kind(&tree, "|"), bars);
        for (bar, offset) in bars.iter().zip(written_bars) {
            offside_written(*bar, source, "|", "|");
            assert!(!bar.is_named() && !bar.is_extra() && !bar.is_missing());
            assert_eq!(bar.parent(), Some(block));
            assert_eq!(bar.start_byte(), offset);
            assert_eq!(bar.end_byte(), offset + 1);
        }
        assert_eq!(
            block.start_byte(),
            if prefixed {
                bars[0].start_byte()
            } else {
                found[0].start_byte()
            }
        );
        assert!(found[0].end_byte() < bars.last().unwrap().start_byte());
        assert!(bars.last().unwrap().end_byte() <= found[1].start_byte());
        let following = offside_binding(&tree, source, owner, "following", "30");
        assert!(
            block.end_byte() <= wrapper.end_byte() && wrapper.end_byte() <= following.start_byte()
        );
        assert!(
            source
                .get(block.end_byte()..wrapper.end_byte())
                .unwrap()
                .chars()
                .all(char::is_whitespace)
        );
        assert_eq!(offside_kind(&tree, "type_definition").len(), 1);
        assert!(offside_kind(&tree, "type_abbrev_defn").is_empty());
        assert!(offside_kind(&tree, "union_type_case").is_empty());
        if nested {
            let inner_name = offside_exact(&tree, source, "identifier", "Nested");
            assert_eq!(inner_name.parent(), Some(owner));
            assert_eq!(
                offside_children(owner),
                vec![inner_name, wrapper, following]
            );
            assert_eq!(owner.child_by_field_name("block"), Some(wrapper));
            let outer = offside_binding(&tree, source, module, "outerFollowing", "40");
            assert!(
                following.end_byte() <= owner.end_byte() && owner.end_byte() <= outer.start_byte()
            );
            assert!(
                source
                    .get(following.end_byte()..owner.end_byte())
                    .unwrap()
                    .chars()
                    .all(char::is_whitespace)
            );
            assert_eq!(offside_children(module), vec![module_name, owner, outer]);
            assert_eq!(offside_kind(&tree, "declaration_expression").len(), 2);
        } else {
            assert_eq!(
                offside_children(module),
                vec![module_name, wrapper, following]
            );
            assert_eq!(offside_kind(&tree, "declaration_expression").len(), 1);
        }
        let comments = offside_kind(&tree, "block_comment");
        if fixture == COMMENT_FOUR {
            assert_eq!(comments.len(), 1);
            let comment = comments[0];
            offside_written(comment, source, "block_comment", "(* gap *)");
            assert!(comment.is_extra());
            assert_eq!(comment.start_byte(), source.find("(* gap *)").unwrap());
            assert!(bars[0].end_byte() <= comment.start_byte());
            assert!(comment.end_byte() < found[1].start_byte());
            assert!(found[0].end_byte() < comment.start_byte());
            // Extras can attach to a container without becoming a semantic field.
            for node in offside_nodes(&tree) {
                for field in ["name", "type_name", "body", "block", "pattern"] {
                    assert_ne!(node.child_by_field_name(field), Some(comment));
                }
            }
        } else {
            assert!(comments.is_empty());
        }
        if fixture == UNICODE_FOUR {
            let name = offside_exact(&tree, source, "identifier", "λ");
            assert_eq!(name.parent(), Some(found[1]));
            assert_eq!(name.end_byte() - name.start_byte(), 2);
            assert_eq!(name.end_position().column - name.start_position().column, 2);
        }
    });
}

fn offside_negative(fixture: &str) {
    offside_variants(fixture, |source| {
        let tree = offside_parse(source);
        offside_ranges(&tree, source);
        assert!(
            tree.root_node().has_error(),
            "compiler-negative enum layout parsed clean: {source:?}"
        );
        let errors = offside_kind(&tree, "ERROR");
        assert!(
            !errors.is_empty(),
            "compiler-negative enum layout lacks visible recovery: {}",
            tree.root_node().to_sexp()
        );
        assert!(errors.iter().all(|node| node.is_error()));
        // Compiler recovery drops later declarations in these probes. Native
        // recovery is not required to duplicate that incidental retained AST.
    });
}

#[test]
fn unprefixed_two_column_offside_grace_keeps_direct_cases() {
    offside_positive(
        UNPREFIXED_FOUR,
        &[("First", "0"), ("Next", "1")],
        false,
        false,
    );
}

#[test]
fn unprefixed_one_column_offside_grace_keeps_direct_cases() {
    offside_positive(
        UNPREFIXED_FIVE,
        &[("First", "0"), ("Next", "1")],
        false,
        false,
    );
}

#[test]
fn unprefixed_aligned_bar_keeps_direct_cases() {
    offside_positive(
        UNPREFIXED_SIX,
        &[("First", "0"), ("Next", "1")],
        false,
        false,
    );
}

#[test]
fn prefixed_two_column_offside_grace_keeps_written_bars() {
    offside_positive(BARS_FOUR, &[("First", "0"), ("Next", "1")], true, false);
}

#[test]
fn prefixed_one_column_offside_grace_keeps_written_bars() {
    offside_positive(BARS_FIVE, &[("First", "0"), ("Next", "1")], true, false);
}

#[test]
fn prefixed_aligned_bar_keeps_written_bars() {
    offside_positive(BARS_SIX, &[("First", "0"), ("Next", "1")], true, false);
}

#[test]
fn signed_values_keep_exact_const_spelling_at_offside_boundary() {
    offside_positive(
        SIGNED_FOUR,
        &[("First", "-1"), ("Next", "+2")],
        false,
        false,
    );
}

#[test]
fn nested_two_column_grace_preserves_inner_and_outer_declarations() {
    offside_positive(NESTED_EIGHT, &[("First", "0"), ("Next", "1")], false, true);
}

#[test]
fn glued_unicode_case_keeps_utf8_byte_points() {
    offside_positive(UNICODE_FOUR, &[("First", "0"), ("λ", "1")], false, false);
}

#[test]
fn post_bar_comment_is_extra_not_case_name_or_value() {
    offside_positive(COMMENT_FOUR, &[("First", "0"), ("Next", "1")], false, false);
}

#[test]
fn unprefixed_three_column_outdent_remains_visible_recovery() {
    offside_negative(UNPREFIXED_THREE);
}

#[test]
fn prefixed_three_column_outdent_remains_visible_recovery() {
    offside_negative(BARS_THREE);
}

#[test]
fn signed_values_do_not_admit_three_column_outdent() {
    offside_negative(SIGNED_THREE);
}

#[test]
fn nested_module_floor_is_not_an_enum_continuation() {
    offside_negative(NESTED_FLOOR);
}

#[test]
fn inline_first_case_does_not_admit_far_shallower_bar() {
    offside_negative(INLINE_FIRST);
}

#[test]
fn top_level_module_floor_is_not_an_enum_continuation() {
    offside_negative(MODULE_FLOOR);
}

#[test]
fn malformed_enum_initializers_keep_internal_bar_recovery_and_following_owners() {
    for (fixture, following_type) in [
        (
            "module Example\ntype Sample =\n    | Broken = 1u<<<0\n    | Next = 2u\nlet following = 30\n",
            false,
        ),
        (
            "module Example\ntype Sample = Broken = 1u<<<0 | Next = 2u\nlet following = 30\n",
            false,
        ),
        (
            "module Example\ntype Sample = Broken = 1u<<<0 | Next = 2u\ntype Boundary = int\nlet following = 30\n",
            true,
        ),
    ] {
        offside_variants(fixture, |source| {
            let tree = offside_parse(source);
            offside_ranges(&tree, source);
            assert!(tree.root_node().has_error());
            let modules = offside_kind(&tree, "named_module");
            assert_eq!(modules.len(), 1);
            let module = modules[0];
            let definitions = offside_kind(&tree, "enum_type_defn");
            assert_eq!(definitions.len(), 1);
            let wrapper = definitions[0].parent().unwrap();
            assert_eq!(wrapper.kind(), "type_definition");
            assert_eq!(wrapper.parent(), Some(module));
            let block = definitions[0].child_by_field_name("block").unwrap();
            let broken = offside_case(&tree, source, block, "Broken", "1u");
            let next = offside_case(&tree, source, block, "Next", "2u");
            assert_eq!(offside_kind(&tree, "enum_type_case"), vec![broken, next]);
            assert_eq!(block.end_byte(), next.end_byte());
            let following = offside_binding(&tree, source, module, "following", "30");
            assert!(wrapper.end_byte() < following.start_byte());
            let mut protected = vec![next, following];
            if following_type {
                let boundary =
                    offside_exact(&tree, source, "type_definition", "type Boundary = int");
                let alias = offside_exact(&tree, source, "type_abbrev_defn", "Boundary = int");
                assert_eq!(boundary.parent(), Some(module));
                assert_eq!(alias.parent(), Some(boundary));
                assert!(wrapper.end_byte() < boundary.start_byte());
                assert!(boundary.end_byte() < following.start_byte());
                protected.push(boundary);
            }
            let errors = offside_kind(&tree, "ERROR");
            assert!(!errors.is_empty());
            let shift = source.find("<<<0").unwrap();
            assert!(broken.end_byte() <= shift && shift + 4 < next.start_byte());
            assert!(
                errors
                    .iter()
                    .any(|error| error.start_byte() < shift + 4 && shift < error.end_byte())
            );
            for error in errors {
                assert_eq!(error.parent(), Some(block));
                for node in &protected {
                    assert!(
                        error.end_byte() <= node.start_byte()
                            || node.end_byte() <= error.start_byte()
                    );
                }
            }
        });
    }
}
