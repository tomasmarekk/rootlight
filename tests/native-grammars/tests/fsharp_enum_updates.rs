//! Safe-API incremental and reuse controls for neutral enum offside witnesses.
//! Complete public tree snapshots are compared with fresh parses, including
//! recovery. Byte damage can split UTF-8; it is not a compiler acceptance claim.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

#[derive(Clone, Copy, Debug)]
enum Profile {
    Implementation,
    Signature,
}

impl Profile {
    fn parser(self) -> Parser {
        let mut parser = Parser::new();
        let language = match self {
            Self::Implementation => tree_sitter_fsharp::LANGUAGE_FSHARP,
            Self::Signature => tree_sitter_fsharp::LANGUAGE_SIGNATURE,
        };
        parser
            .set_language(&language.into())
            .expect("pinned grammar is incompatible with Tree-sitter");
        parser
    }

    fn module_name(self) -> &'static str {
        match self {
            Self::Implementation => "EnumExample",
            Self::Signature => "EnumSignatureExample",
        }
    }
}

#[derive(Clone, Copy)]
enum Layout {
    Unprefixed,
    Prefixed,
    Nested,
}

struct Fixture {
    label: &'static str,
    implementation: &'static str,
    signature: &'static str,
    layout: Layout,
    second_name: &'static str,
}

impl Fixture {
    fn source(&self, profile: Profile) -> &'static str {
        match profile {
            Profile::Implementation => self.implementation,
            Profile::Signature => self.signature,
        }
    }
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "unprefixed-grace-two",
        implementation: r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
    | Next = 1
let following = 30
"#,
        signature: r#"// Parse-only signature enum layout witness; constants are not evaluated or typechecked.
// Following value specifications remain independent of the enum cases.
module EnumSignatureExample
type Sample =
      First = 0
    | Next = 1
val following : int
"#,
        layout: Layout::Unprefixed,
        second_name: "Next",
    },
    Fixture {
        label: "prefixed-grace-two",
        implementation: r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      | First = 0
    | Next = 1
let following = 30
"#,
        signature: r#"// Parse-only signature enum layout witness; constants are not evaluated or typechecked.
// Following value specifications remain independent of the enum cases.
module EnumSignatureExample
type Sample =
      | First = 0
    | Next = 1
val following : int
"#,
        layout: Layout::Prefixed,
        second_name: "Next",
    },
    Fixture {
        label: "glued-unicode",
        implementation: r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
    |λ = 1
let following = 30
"#,
        signature: r#"// Parse-only signature enum layout witness; constants are not evaluated or typechecked.
// Following value specifications remain independent of the enum cases.
module EnumSignatureExample
type Sample =
      First = 0
    |λ = 1
val following : int
"#,
        layout: Layout::Unprefixed,
        second_name: "λ",
    },
    Fixture {
        label: "post-bar-comment",
        implementation: r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
type Sample =
      First = 0
    |(* gap *) Next = 1
let following = 30
"#,
        signature: r#"// Parse-only signature enum layout witness; constants are not evaluated or typechecked.
// Following value specifications remain independent of the enum cases.
module EnumSignatureExample
type Sample =
      First = 0
    |(* gap *) Next = 1
val following : int
"#,
        layout: Layout::Unprefixed,
        second_name: "Next",
    },
    Fixture {
        label: "nested-grace-two",
        implementation: r#"// Parse-only enum indentation witness; constants are not evaluated or typechecked.
// Direct cases and following declarations distinguish source ownership from recovery.
module EnumExample
module Nested =
    type Sample =
          First = 0
        | Next = 1
    let following = 30
let outerFollowing = 40
"#,
        signature: r#"// Parse-only signature enum layout witness; constants are not evaluated or typechecked.
// Following value specifications remain independent of the enum cases.
module EnumSignatureExample
module Nested =
    type Sample =
          First = 0
        | Next = 1
    val following : int
val outerFollowing : int
"#,
        layout: Layout::Nested,
        second_name: "Next",
    },
];
const SIGNATURE_OUTDENT: &str = r#"// Parse-only signature enum layout witness; constants are not evaluated or typechecked.
// Following value specifications remain independent of the enum cases.
module EnumSignatureExample
type Sample =
      First = 0
   | Next = 1
val following : int
"#;

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = source.get(..offset).expect("offset escapes authored bytes");
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
        .expect("authored enum parse exceeded its cooperative watchdog")
}

#[derive(Debug, PartialEq, Eq)]
struct Witness {
    kind: String,
    span: Range<usize>,
    start: Point,
    end: Point,
    depth: usize,
    parent_kind: Option<String>,
    field: Option<String>,
    named: bool,
    extra: bool,
    error: bool,
    missing: bool,
    contains_error: bool,
    root_expression: Option<String>,
}

fn snapshot(tree: &Tree, source: &[u8]) -> Vec<Witness> {
    let root = tree.root_node();
    assert_eq!(root.kind(), "file");
    assert_eq!(root.byte_range(), 0..source.len());
    let mut pending = vec![(root, 0_usize, None)];
    let mut found = Vec::new();
    while let Some((node, depth, field)) = pending.pop() {
        assert!(
            found.len() < 100_000,
            "authored inventory exceeds its bound"
        );
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        if let Some(parent) = node.parent() {
            assert!(parent.start_byte() <= node.start_byte());
            assert!(node.end_byte() <= parent.end_byte());
        }
        found.push(Witness {
            kind: node.kind().to_owned(),
            span: node.byte_range(),
            start: node.start_position(),
            end: node.end_position(),
            depth,
            parent_kind: node.parent().map(|parent| parent.kind().to_owned()),
            field: field.map(str::to_owned),
            named: node.is_named(),
            extra: node.is_extra(),
            error: node.is_error(),
            missing: node.is_missing(),
            contains_error: node.has_error(),
            // Public child traversal omits hidden missing tokens that remain
            // present in the diagnostic expression and subtree error cost.
            root_expression: (depth == 0).then(|| node.to_sexp()),
        });
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("child index exceeds native API");
            pending.push((
                node.child(index).expect("advertised child is absent"),
                depth + 1,
                node.field_name_for_child(index),
            ));
        }
    }
    found
}

fn nodes<'tree>(tree: &'tree Tree, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        visited += 1;
        assert!(visited <= 100_000, "authored node walk exceeds its bound");
        if node.kind() == kind {
            result.push(node);
        }
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("child index exceeds native API");
            pending.push(node.child(index).expect("advertised child is absent"));
        }
    }
    result
}

fn semantic_children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| {
            node.named_child(u32::try_from(index).expect("child index exceeds native API"))
                .expect("advertised named child is absent")
        })
        .filter(|child| !child.is_extra())
        .collect()
}

fn written(node: Node<'_>, source: &str, kind: &str, text: &str) {
    assert_eq!(node.kind(), kind);
    assert_eq!(source.get(node.byte_range()), Some(text));
    assert_eq!(
        node.start_position(),
        point(source.as_bytes(), node.start_byte())
    );
    assert_eq!(
        node.end_position(),
        point(source.as_bytes(), node.end_byte())
    );
}

fn exact<'tree>(tree: &'tree Tree, source: &str, kind: &str, text: &str) -> Node<'tree> {
    let found: Vec<_> = nodes(tree, kind)
        .into_iter()
        .filter(|node| source.get(node.byte_range()) == Some(text))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
    written(found[0], source, kind, text);
    found[0]
}

fn following<'tree>(
    tree: &'tree Tree,
    source: &str,
    profile: Profile,
    owner: Node<'tree>,
    name: &str,
    rhs: &str,
) -> Node<'tree> {
    match profile {
        Profile::Implementation => {
            let text = format!("let {name} = {rhs}");
            let wrapper = exact(tree, source, "declaration_expression", &text);
            let definition = exact(tree, source, "function_or_value_defn", &text);
            assert_eq!(wrapper.parent(), Some(owner));
            assert_eq!(definition.parent(), Some(wrapper));
            assert_eq!(semantic_children(wrapper), vec![definition]);
            let left = exact(tree, source, "value_declaration_left", name);
            assert_eq!(left.parent(), Some(definition));
            let body = definition
                .child_by_field_name("body")
                .expect("body is absent");
            written(body, source, "const", rhs);
            assert_eq!(body.parent(), Some(definition));
            assert_eq!(semantic_children(definition), vec![left, body]);
            wrapper
        }
        Profile::Signature => {
            let text = format!("val {name} : int");
            let definition = exact(tree, source, "value_definition", &text);
            assert_eq!(definition.parent(), Some(owner));
            assert!(definition.child_by_field_name("body").is_none());
            let left = exact(tree, source, "value_declaration_left", name);
            assert_eq!(left.parent(), Some(definition));
            let children = semantic_children(definition);
            assert_eq!(children.len(), 2);
            assert_eq!(children[0], left);
            written(children[1], source, "curried_spec", "int");
            assert_eq!(children[1].parent(), Some(definition));
            definition
        }
    }
}

fn clean_ownership(
    tree: &Tree,
    source: &str,
    profile: Profile,
    fixture: &Fixture,
    first_rhs: &str,
) {
    assert!(
        !tree.root_node().has_error(),
        "{}: {}",
        fixture.label,
        tree.root_node().to_sexp()
    );
    assert!(
        snapshot(tree, source.as_bytes())
            .iter()
            .all(|node| !node.error && !node.missing && !node.contains_error)
    );
    let modules = nodes(tree, "named_module");
    assert_eq!(modules.len(), 1);
    let module = modules[0];
    assert_eq!(module.parent(), Some(tree.root_node()));
    let module_name = module
        .child_by_field_name("name")
        .expect("module name is absent");
    written(
        module_name,
        source,
        "long_identifier",
        profile.module_name(),
    );
    assert_eq!(module_name.parent(), Some(module));
    let nested = matches!(fixture.layout, Layout::Nested);
    let inner = nodes(tree, "module_defn");
    assert_eq!(inner.len(), usize::from(nested));
    let owner = if nested {
        assert_eq!(inner[0].parent(), Some(module));
        inner[0]
    } else {
        module
    };
    let definitions = nodes(tree, "enum_type_defn");
    assert_eq!(definitions.len(), 1);
    let definition = definitions[0];
    let wrapper = definition.parent().expect("enum definition has no wrapper");
    assert_eq!(wrapper.kind(), "type_definition");
    assert_eq!(wrapper.parent(), Some(owner));
    assert_eq!(semantic_children(wrapper), vec![definition]);
    assert_eq!(wrapper.start_byte(), source.find("type Sample").unwrap());
    let name = exact(tree, source, "type_name", "Sample");
    assert_eq!(name.parent(), Some(definition));
    let block = definition
        .child_by_field_name("block")
        .expect("enum block is absent");
    assert_eq!(block.kind(), "enum_type_cases");
    assert_eq!(block.parent(), Some(definition));
    assert_eq!(semantic_children(definition), vec![name, block]);
    let mut cases = Vec::new();
    for (name, rhs) in [("First", first_rhs), (fixture.second_name, "1")] {
        let text = format!("{name} = {rhs}");
        let case = exact(tree, source, "enum_type_case", &text);
        assert_eq!(case.parent(), Some(block));
        let children = semantic_children(case);
        assert_eq!(children.len(), 2);
        written(children[0], source, "identifier", name);
        written(children[1], source, "const", rhs);
        for child in &children {
            assert_eq!(child.parent(), Some(case));
        }
        let numeric = semantic_children(children[1]);
        assert_eq!(numeric.len(), 1);
        written(numeric[0], source, "int", rhs);
        assert_eq!(numeric[0].parent(), Some(children[1]));
        assert_eq!(case.start_byte(), children[0].start_byte());
        assert_eq!(case.end_byte(), children[1].end_byte());
        cases.push(case);
    }
    assert_eq!(nodes(tree, "enum_type_case"), cases);
    assert_eq!(semantic_children(block), cases);
    assert_eq!(block.end_byte(), cases[1].end_byte());
    let bars = nodes(tree, "|");
    let offsets: Vec<_> = source
        .match_indices('|')
        .map(|(offset, _)| offset)
        .collect();
    assert_eq!(
        bars.len(),
        if matches!(fixture.layout, Layout::Prefixed) {
            2
        } else {
            1
        }
    );
    assert_eq!(bars.len(), offsets.len());
    for (bar, offset) in bars.iter().zip(offsets) {
        written(*bar, source, "|", "|");
        assert_eq!(bar.parent(), Some(block));
        assert_eq!(bar.byte_range(), offset..offset + 1);
        assert!(!bar.is_named() && !bar.is_extra());
    }
    let next = following(tree, source, profile, owner, "following", "30");
    assert!(block.end_byte() <= wrapper.end_byte() && wrapper.end_byte() <= next.start_byte());
    assert!(
        source
            .get(block.end_byte()..wrapper.end_byte())
            .unwrap()
            .chars()
            .all(char::is_whitespace)
    );
    if nested {
        let nested_name = exact(tree, source, "identifier", "Nested");
        assert_eq!(nested_name.parent(), Some(owner));
        assert_eq!(semantic_children(owner), vec![nested_name, wrapper, next]);
        let outer = following(tree, source, profile, module, "outerFollowing", "40");
        assert!(next.end_byte() <= owner.end_byte() && owner.end_byte() <= outer.start_byte());
        assert_eq!(semantic_children(module), vec![module_name, owner, outer]);
    } else {
        assert_eq!(semantic_children(module), vec![module_name, wrapper, next]);
    }
    if fixture.second_name == "λ" {
        let name = exact(tree, source, "identifier", "λ");
        assert_eq!(name.parent(), Some(cases[1]));
        assert_eq!(name.end_byte() - name.start_byte(), 2);
    }
    let comments = nodes(tree, "block_comment");
    if fixture.label == "post-bar-comment" {
        assert_eq!(comments.len(), 1);
        written(comments[0], source, "block_comment", "(* gap *)");
        assert!(comments[0].is_extra());
        assert!(bars[0].end_byte() <= comments[0].start_byte());
        assert!(comments[0].end_byte() < cases[1].start_byte());
    } else {
        assert!(comments.is_empty());
    }
    assert!(nodes(tree, "union_type_case").is_empty());
    assert!(nodes(tree, "type_abbrev_defn").is_empty());
}

fn variants(fixture: &str, mut check: impl FnMut(&str)) {
    assert!(!fixture.contains('\r'), "canonical fixture must use LF");
    let eof = fixture
        .strip_suffix('\n')
        .expect("canonical fixture must end in LF");
    for newline in ["\n", "\r\n"] {
        for source in [eof, fixture] {
            check(&source.replace('\n', newline));
        }
    }
}

fn edit_tree(
    tree: &mut Tree,
    before: &[u8],
    after: &[u8],
    start: usize,
    old_end: usize,
    new_end: usize,
) {
    assert_eq!(before.get(..start), after.get(..start));
    assert_eq!(before.get(old_end..), after.get(new_end..));
    tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before, start),
        old_end_position: point(before, old_end),
        new_end_position: point(after, new_end),
    });
    assert_eq!(tree.root_node().end_byte(), after.len());
    assert_eq!(tree.root_node().end_position(), point(after, after.len()));
}

fn every_byte_damage_and_restore(fixture: &Fixture) {
    for profile in [Profile::Implementation, Profile::Signature] {
        variants(fixture.source(profile), |source| {
            let mut reused = profile.parser();
            let bytes = source.as_bytes();
            let original = bounded_parse(&mut reused, bytes, None);
            clean_ownership(&original, source, profile, fixture, "0");
            let original_shape = snapshot(&original, bytes);
            for offset in 0..bytes.len() {
                let mut damaged = bytes.to_vec();
                damaged[offset] = if bytes[offset] == b'?' { 0xff } else { b'?' };
                let mut old = original.clone();
                edit_tree(&mut old, bytes, &damaged, offset, offset + 1, offset + 1);
                let mut incremental = bounded_parse(&mut reused, &damaged, Some(&old));
                let fresh = bounded_parse(&mut profile.parser(), &damaged, None);
                assert_eq!(
                    snapshot(&incremental, &damaged),
                    snapshot(&fresh, &damaged),
                    "{profile:?}, {}, damage at {offset}, source {source:?}",
                    fixture.label
                );
                edit_tree(
                    &mut incremental,
                    &damaged,
                    bytes,
                    offset,
                    offset + 1,
                    offset + 1,
                );
                let restored = bounded_parse(&mut reused, bytes, Some(&incremental));
                let fresh_restored = bounded_parse(&mut profile.parser(), bytes, None);
                clean_ownership(&restored, source, profile, fixture, "0");
                let restored_shape = snapshot(&restored, bytes);
                assert_eq!(restored_shape, snapshot(&fresh_restored, bytes));
                assert_eq!(
                    restored_shape, original_shape,
                    "{profile:?}, {}, restore at {offset}",
                    fixture.label
                );
            }
        });
    }
}

#[test]
fn unprefixed_offside_every_byte_damage_and_restore_matches_fresh() {
    every_byte_damage_and_restore(&FIXTURES[0]);
}

#[test]
fn prefixed_offside_every_byte_damage_and_restore_matches_fresh() {
    every_byte_damage_and_restore(&FIXTURES[1]);
}

#[test]
fn unicode_offside_every_byte_damage_and_restore_matches_fresh() {
    every_byte_damage_and_restore(&FIXTURES[2]);
}

#[test]
fn comment_offside_every_byte_damage_and_restore_matches_fresh() {
    every_byte_damage_and_restore(&FIXTURES[3]);
}

#[test]
fn nested_offside_every_byte_damage_and_restore_matches_fresh() {
    every_byte_damage_and_restore(&FIXTURES[4]);
}

#[test]
fn length_changing_enum_value_edit_and_restore_matches_fresh() {
    for fixture in FIXTURES {
        for profile in [Profile::Implementation, Profile::Signature] {
            variants(fixture.source(profile), |source| {
                let mut reused = profile.parser();
                let original = bounded_parse(&mut reused, source.as_bytes(), None);
                clean_ownership(&original, source, profile, fixture, "0");
                let offset = source.find("First = 0").unwrap() + "First = ".len();
                let mut changed = source.as_bytes().to_vec();
                changed.splice(offset..offset + 1, *b"11");
                let mut old = original.clone();
                edit_tree(
                    &mut old,
                    source.as_bytes(),
                    &changed,
                    offset,
                    offset + 1,
                    offset + 2,
                );
                let mut updated = bounded_parse(&mut reused, &changed, Some(&old));
                let fresh = bounded_parse(&mut profile.parser(), &changed, None);
                let changed_text =
                    std::str::from_utf8(&changed).expect("ASCII numeric edit preserves UTF-8");
                clean_ownership(&updated, changed_text, profile, fixture, "11");
                assert_eq!(snapshot(&updated, &changed), snapshot(&fresh, &changed));
                edit_tree(
                    &mut updated,
                    &changed,
                    source.as_bytes(),
                    offset,
                    offset + 2,
                    offset + 1,
                );
                let restored = bounded_parse(&mut reused, source.as_bytes(), Some(&updated));
                let fresh_restored = bounded_parse(&mut profile.parser(), source.as_bytes(), None);
                clean_ownership(&restored, source, profile, fixture, "0");
                assert_eq!(
                    snapshot(&restored, source.as_bytes()),
                    snapshot(&fresh_restored, source.as_bytes())
                );
                assert_eq!(
                    snapshot(&restored, source.as_bytes()),
                    snapshot(&original, source.as_bytes())
                );
            });
        }
    }
}

#[test]
fn parser_reuse_after_incomplete_enum_keeps_next_document_exact() {
    for profile in [Profile::Implementation, Profile::Signature] {
        let mut reused = profile.parser();
        for fixture in FIXTURES {
            variants(fixture.source(profile), |source| {
                let end = source.rfind(" = 1").expect("second enum value is present") + " = ".len();
                let incomplete = source.as_bytes().get(..end).unwrap();
                let recovery = bounded_parse(&mut reused, incomplete, None);
                let fresh_recovery = bounded_parse(&mut profile.parser(), incomplete, None);
                assert!(
                    recovery.root_node().has_error(),
                    "incomplete enum RHS parsed clean"
                );
                let recovered_shape = snapshot(&recovery, incomplete);
                let expression = recovery.root_node().to_sexp();
                assert!(
                    expression.contains("(MISSING ") || expression.contains("(ERROR"),
                    "incomplete enum lacks serialized recovery: {expression}"
                );
                assert!(
                    recovered_shape
                        .iter()
                        .any(|node| node.error || node.missing || node.contains_error),
                    "incomplete recovery: {} {recovered_shape:?}",
                    recovery.root_node().to_sexp()
                );
                assert_eq!(recovered_shape, snapshot(&fresh_recovery, incomplete));
                let complete = bounded_parse(&mut reused, source.as_bytes(), None);
                let fresh = bounded_parse(&mut profile.parser(), source.as_bytes(), None);
                clean_ownership(&complete, source, profile, fixture, "0");
                assert_eq!(
                    snapshot(&complete, source.as_bytes()),
                    snapshot(&fresh, source.as_bytes())
                );
            });
        }
    }
}

#[test]
fn cancellation_then_reset_keeps_enum_document_ownership_exact() {
    let work = format!(
        "module EnumCancellation\n{}",
        "type Sample =\n      First = 0\n    | Next = 1\n".repeat(2_048)
    );
    assert!(work.len() <= 128 * 1_024);
    for profile in [Profile::Implementation, Profile::Signature] {
        let mut reused = profile.parser();
        for fixture in FIXTURES {
            variants(fixture.source(profile), |source| {
                let mut called = false;
                let mut cancel = |_: &tree_sitter::ParseState| {
                    called = true;
                    ControlFlow::Break(())
                };
                let cancelled = reused.parse_with_options(
                    &mut |offset, _| work.as_bytes().get(offset..).unwrap_or_default(),
                    None,
                    Some(ParseOptions::new().progress_callback(&mut cancel)),
                );
                assert!(
                    called && cancelled.is_none(),
                    "{profile:?}: parse did not cancel"
                );
                // A cancelled parser resumes by default; another document needs reset.
                reused.reset();
                let complete = bounded_parse(&mut reused, source.as_bytes(), None);
                let fresh = bounded_parse(&mut profile.parser(), source.as_bytes(), None);
                clean_ownership(&complete, source, profile, fixture, "0");
                assert_eq!(
                    snapshot(&complete, source.as_bytes()),
                    snapshot(&fresh, source.as_bytes())
                );
            });
        }
    }
}

#[test]
fn signature_three_column_outdent_remains_explicit_recovery() {
    variants(SIGNATURE_OUTDENT, |source| {
        let tree = bounded_parse(&mut Profile::Signature.parser(), source.as_bytes(), None);
        snapshot(&tree, source.as_bytes());
        assert!(
            tree.root_node().has_error(),
            "compiler-negative signature outdent parsed clean"
        );
        assert!(
            !nodes(&tree, "ERROR").is_empty(),
            "signature outdent lacks explicit recovery"
        );
    });
}
