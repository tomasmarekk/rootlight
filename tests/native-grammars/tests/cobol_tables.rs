//! Authored native tests for symbolic OCCURS bounds and existing table operands.
//! Exact nodes, fields and physical coordinates describe syntax candidates only;
//! these tests do not resolve constants, evaluate bounds or prove table semantics.

use std::ops::{ControlFlow, Range};

use tree_sitter::{InputEdit, Language, Node, ParseOptions, Parser, Point, Tree};

#[derive(Clone, Copy, Debug)]
enum Profile {
    Fixed,
    Free,
}

impl Profile {
    fn language(self) -> Language {
        match self {
            Self::Fixed => tree_sitter_cobol::FIXED.into(),
            Self::Free => tree_sitter_cobol::FREE.into(),
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Fixed => "       ",
            Self::Free => "",
        }
    }

    fn parser(self) -> Parser {
        let mut parser = Parser::new();
        parser.set_language(&self.language()).unwrap();
        parser
    }
}

fn source(lines: &[&str], profile: Profile, ending: &str, final_newline: bool) -> String {
    let mut text = String::new();
    for (index, line) in lines.iter().enumerate() {
        text.push_str(profile.prefix());
        text.push_str(line);
        if final_newline || index + 1 < lines.len() {
            text.push_str(ending);
        }
    }
    text
}

fn program(
    declarations: &[&str],
    statements: &[&str],
    profile: Profile,
    ending: &str,
    final_newline: bool,
) -> String {
    let mut lines = vec![
        "identification division.",
        "program-id. Tables.",
        "data division.",
        "working-storage section.",
    ];
    lines.extend_from_slice(declarations);
    lines.push("procedure division.");
    lines.extend_from_slice(statements);
    lines.extend(["stop run.", "end program Tables."]);
    source(&lines, profile, ending, final_newline)
}

fn bounded_parse(parser: &mut Parser, text: &str, old: Option<&Tree>) -> Tree {
    let mut steps = 0_usize;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        steps = steps.saturating_add(1);
        if steps > 2_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| text.as_bytes().get(offset..).unwrap_or_default(),
            old,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("authored table fixture exceeded progress watchdog")
}

fn parse(text: &str, profile: Profile) -> Tree {
    bounded_parse(&mut profile.parser(), text, None)
}

fn point(text: &str, offset: usize) -> Point {
    let prefix = text.as_bytes().get(..offset).unwrap();
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

#[derive(Debug, Eq, PartialEq)]
struct NodeRecord {
    kind: String,
    range: Range<usize>,
    start: Point,
    end: Point,
    field: Option<String>,
    depth: usize,
    named: bool,
    extra: bool,
    error: bool,
    missing: bool,
    has_error: bool,
    written: String,
}

fn inventory(tree: &Tree, text: &str) -> Vec<NodeRecord> {
    let mut records = Vec::new();
    let mut depth = 0_usize;
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= text.len());
        assert_eq!(node.start_position(), point(text, node.start_byte()));
        assert_eq!(node.end_position(), point(text, node.end_byte()));
        let written = text.get(node.byte_range()).unwrap();
        assert_eq!(node.utf8_text(text.as_bytes()).unwrap(), written);
        records.push(NodeRecord {
            kind: node.kind().to_owned(),
            range: node.byte_range(),
            start: node.start_position(),
            end: node.end_position(),
            field: cursor.field_name().map(str::to_owned),
            depth,
            named: node.is_named(),
            extra: node.is_extra(),
            error: node.is_error(),
            missing: node.is_missing(),
            has_error: node.has_error(),
            written: written.to_owned(),
        });
        assert!(
            records.len() <= 2_000,
            "authored node inventory exceeded bound"
        );
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return records;
            }
            depth -= 1;
        }
    }
}

fn clean(tree: &Tree, text: &str) {
    assert_eq!(tree.root_node().kind(), "start");
    assert!(
        !tree.root_node().has_error(),
        "authored source {text:?}: {}",
        tree.root_node().to_sexp()
    );
    assert!(
        inventory(tree, text)
            .iter()
            .all(|node| { !node.error && !node.missing && !node.has_error })
    );
}

fn recovery(tree: &Tree, text: &str) {
    assert!(
        tree.root_node().has_error(),
        "malformed authored source unexpectedly clean {text:?}: {}",
        tree.root_node().to_sexp()
    );
    let records = inventory(tree, text);
    if !records.iter().any(|node| node.error || node.missing) {
        // Invisible missing leaves can be serialized without appearing through
        // public child traversal, so a visible ERROR-only oracle loses recovery.
        assert!(
            tree.root_node().to_sexp().contains("(MISSING "),
            "error-bearing source has no recovery witness {text:?}: {}",
            tree.root_node().to_sexp()
        );
    }
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut visited = 0_usize;
    let mut cursor = root.walk();
    loop {
        visited += 1;
        assert!(visited <= 2_000, "authored node walk exceeded bound");
        if cursor.node().kind() == kind {
            found.push(cursor.node());
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                found.sort_by_key(Node::start_byte);
                return found;
            }
        }
    }
}

fn unique_range(text: &str, written: &str) -> Range<usize> {
    let matches: Vec<_> = text.match_indices(written).collect();
    assert_eq!(matches.len(), 1, "authored text is not unique: {written:?}");
    let start = matches[0].0;
    start..start + written.len()
}

fn exact(node: Node<'_>, text: &str, expected: Range<usize>) {
    assert_eq!(node.byte_range(), expected);
    assert_eq!(node.start_position(), point(text, expected.start));
    assert_eq!(node.end_position(), point(text, expected.end));
    assert_eq!(
        node.utf8_text(text.as_bytes()).unwrap(),
        text.get(expected).unwrap()
    );
}

fn ordered_exact(nodes: &[Node<'_>], text: &str, start: usize, expected: &[&str]) {
    assert_eq!(nodes.len(), expected.len());
    let mut previous_end = start;
    for (node, written) in nodes.iter().zip(expected) {
        let relative = text.get(previous_end..).unwrap().find(written).unwrap();
        let begin = previous_end + relative;
        let range = begin..begin + written.len();
        exact(*node, text, range.clone());
        previous_end = range.end;
    }
}

fn clause<'tree>(tree: &'tree Tree, text: &str, written: &str) -> Node<'tree> {
    let expected = unique_range(text, written);
    let selected: Vec<_> = nodes(tree.root_node(), "occurs_clause")
        .into_iter()
        .filter(|node| node.byte_range() == expected)
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "missing exact OCCURS clause: {written:?}"
    );
    assert_eq!(selected[0].parent().unwrap().kind(), "data_description");
    exact(selected[0], text, expected);
    selected[0]
}

fn exact_bounds(node: Node<'_>, text: &str, expected: &[(&str, &str)]) {
    let bounds = nodes(node, "occurs_bound");
    assert_eq!(bounds.len(), expected.len());
    let mut previous_end = node.start_byte();
    for (index, (bound, (written, terminal))) in bounds.iter().zip(expected).enumerate() {
        let relative = text
            .get(previous_end..node.end_byte())
            .unwrap()
            .find(written)
            .unwrap();
        let start = previous_end + relative;
        let span = start..start + written.len();
        assert_eq!(bound.parent().unwrap().id(), node.id());
        exact(*bound, text, span.clone());
        let field = if index == 0 { "num" } else { "to" };
        let mut cursor = node.walk();
        let owners: Vec<_> = node
            .children_by_field_name(field, &mut cursor)
            .filter(|child| child.kind() == "occurs_bound")
            .collect();
        assert_eq!(owners.len(), 1, "incorrect bound ownership for {field}");
        assert_eq!(owners[0].id(), bound.id());
        let mut cursor = bound.walk();
        let children: Vec<_> = bound
            .named_children(&mut cursor)
            .filter(|child| !child.is_extra())
            .collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind(), *terminal);
        assert_eq!(children[0].parent().unwrap().id(), bound.id());
        exact(children[0], text, span.clone());
        assert!(nodes(*bound, "subref").is_empty());
        assert!(nodes(*bound, "refmod").is_empty());
        assert!(nodes(*bound, "function_").is_empty());
        previous_end = span.end;
    }
    if expected.len() == 1 {
        assert!(node.child_by_field_name("to").is_none());
    }
}

#[test]
fn literal_and_symbolic_bounds_keep_exact_native_fields_and_old_static_forms() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let text = program(
                    &[
                        "78 Lower-Limit value 1.",
                        "78 Upper-Limit value 7.",
                        "01 Active-Count pic 9.",
                        "01 Table-Group.",
                        "03 Static-Entry pic x(20) occurs 7 times.",
                        "03 Short-Entry pic x occurs 6.",
                        "03 Named-Entry pic x(20) occurs Upper-Limit times.",
                        "03 Mixed-Entry occurs 1 to Upper-Limit times",
                        "depending on Active-Count.",
                        "03 Bounded-Entry occurs Lower-Limit to Upper-Limit",
                        "depending on Active-Count.",
                    ],
                    &[],
                    profile,
                    ending,
                    final_newline,
                );
                let tree = parse(&text, profile);
                clean(&tree, &text);
                assert_eq!(nodes(tree.root_node(), "program_definition").len(), 1);
                assert_eq!(nodes(tree.root_node(), "data_description").len(), 9);
                assert_eq!(nodes(tree.root_node(), "occurs_clause").len(), 5);
                assert_eq!(nodes(tree.root_node(), "occurs_bound").len(), 7);
                for (written, bound, terminal) in [
                    ("occurs 7 times", "7", "integer"),
                    ("occurs 6.", "6", "integer"),
                    ("occurs Upper-Limit times", "Upper-Limit", "WORD"),
                ] {
                    let written = written.strip_suffix('.').unwrap_or(written);
                    exact_bounds(clause(&tree, &text, written), &text, &[(bound, terminal)]);
                }
                let mixed = format!(
                    "occurs 1 to Upper-Limit times{ending}{}depending on Active-Count",
                    profile.prefix()
                );
                exact_bounds(
                    clause(&tree, &text, &mixed),
                    &text,
                    &[("1", "integer"), ("Upper-Limit", "WORD")],
                );
                let bounded = format!(
                    "occurs Lower-Limit to Upper-Limit{ending}{}depending on Active-Count",
                    profile.prefix()
                );
                exact_bounds(
                    clause(&tree, &text, &bounded),
                    &text,
                    &[("Lower-Limit", "WORD"), ("Upper-Limit", "WORD")],
                );
            }
        }
    }
}

#[test]
fn symbolic_bound_case_and_physical_line_splits_preserve_original_bytes() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let written = format!(
                    "oCcUrS{ending}{}LoWeR-LiMiT{ending}{}tO UpPeR-LiMiT{ending}{}tImEs dEpEnDiNg On Active-Count",
                    profile.prefix(),
                    profile.prefix(),
                    profile.prefix(),
                );
                let entry = format!("03 Cell pic x(20) {written}.");
                let text = program(
                    &[
                        "78 Lower-Limit value 1.",
                        "78 Upper-Limit value 7.",
                        "01 Active-Count pic 9.",
                        "01 Table-Group.",
                        &entry,
                    ],
                    &[],
                    profile,
                    ending,
                    final_newline,
                );
                let tree = parse(&text, profile);
                clean(&tree, &text);
                assert_eq!(nodes(tree.root_node(), "occurs_clause").len(), 1);
                assert_eq!(nodes(tree.root_node(), "occurs_bound").len(), 2);
                exact_bounds(
                    clause(&tree, &text, &written),
                    &text,
                    &[("LoWeR-LiMiT", "WORD"), ("UpPeR-LiMiT", "WORD")],
                );
            }
        }
    }
}

#[test]
fn depending_key_and_index_names_do_not_become_symbolic_bounds() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let written = format!(
                    "occurs Upper-Limit times depending on Active-Count{ending}{}ascending key is Sort-Key Secondary-Key{ending}{}descending key is Reverse-Key{ending}{}indexed by Entry-Cursor Other-Cursor",
                    profile.prefix(),
                    profile.prefix(),
                    profile.prefix(),
                );
                let entry = format!("03 Entry-Group {written}.");
                let text = program(
                    &[
                        "78 Upper-Limit value 7.",
                        "01 Active-Count pic 9.",
                        "01 Table-Group.",
                        &entry,
                        "05 Sort-Key pic 9.",
                        "05 Secondary-Key pic 9.",
                        "05 Reverse-Key pic 9.",
                    ],
                    &[],
                    profile,
                    ending,
                    final_newline,
                );
                let tree = parse(&text, profile);
                clean(&tree, &text);
                let occurs = clause(&tree, &text, &written);
                exact_bounds(occurs, &text, &[("Upper-Limit", "WORD")]);
                assert_eq!(nodes(tree.root_node(), "occurs_bound").len(), 1);
                ordered_exact(
                    &nodes(occurs, "qualified_word"),
                    &text,
                    occurs.start_byte(),
                    &["Active-Count", "Sort-Key", "Secondary-Key", "Reverse-Key"],
                );
                let keys = nodes(occurs, "occurs_key");
                assert_eq!(keys.len(), 2);
                exact(
                    keys[0],
                    &text,
                    unique_range(&text, "ascending key is Sort-Key Secondary-Key"),
                );
                exact(
                    keys[1],
                    &text,
                    unique_range(&text, "descending key is Reverse-Key"),
                );
                let indexes = nodes(occurs, "occurs_indexed");
                assert_eq!(indexes.len(), 1);
                exact(
                    indexes[0],
                    &text,
                    unique_range(&text, "indexed by Entry-Cursor Other-Cursor"),
                );
                ordered_exact(
                    &nodes(indexes[0], "WORD"),
                    &text,
                    indexes[0].start_byte(),
                    &["Entry-Cursor", "Other-Cursor"],
                );
                assert_eq!(
                    occurs.child_by_field_name("key_spec").unwrap().kind(),
                    "occurs_key_spec"
                );
                let mut cursor = occurs.walk();
                let depending: Vec<_> = occurs
                    .children_by_field_name("depending", &mut cursor)
                    .filter(|child| child.kind() == "qualified_word")
                    .collect();
                assert_eq!(depending.len(), 1);
                let phrase = unique_range(&text, "depending on Active-Count");
                exact(
                    depending[0],
                    &text,
                    phrase.end - "Active-Count".len()..phrase.end,
                );
            }
        }
    }
}

#[test]
fn table_subscripts_and_reference_modifiers_keep_all_original_operand_spans() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let text = program(
                    &[
                        "78 Upper-Limit value 7.",
                        "01 Position pic 9.",
                        "01 Input-Text pic x(20).",
                        "01 Output-Text pic x(20).",
                        "01 Table-Group.",
                        "03 Cell pic x(20) occurs Upper-Limit times.",
                    ],
                    &[
                        "move Cell(Position) to Output-Text.",
                        "move Input-Text to Cell(2).",
                        "display Cell(Position + 1).",
                        "display Cell(Position, 2).",
                        "display Cell(Position)(2:3).",
                        "display Cell of Table-Group(Position).",
                    ],
                    profile,
                    ending,
                    final_newline,
                );
                let tree = parse(&text, profile);
                clean(&tree, &text);
                exact_bounds(
                    clause(&tree, &text, "occurs Upper-Limit times"),
                    &text,
                    &[("Upper-Limit", "WORD")],
                );
                let start = text.find("procedure division.").unwrap();
                ordered_exact(
                    &nodes(tree.root_node(), "subref"),
                    &text,
                    start,
                    &[
                        "(Position)",
                        "(2)",
                        "(Position + 1)",
                        "(Position, 2)",
                        "(Position)",
                        "(Position)",
                    ],
                );
                let modifiers = nodes(tree.root_node(), "refmod");
                assert_eq!(modifiers.len(), 1);
                exact(modifiers[0], &text, unique_range(&text, "(2:3)"));
                ordered_exact(
                    &nodes(tree.root_node(), "qualified_word"),
                    &text,
                    start,
                    &[
                        "Cell",
                        "Position",
                        "Output-Text",
                        "Input-Text",
                        "Cell",
                        "Cell",
                        "Position",
                        "Cell",
                        "Position",
                        "Cell",
                        "Position",
                        "Cell of Table-Group",
                        "Position",
                    ],
                );
                assert_eq!(nodes(tree.root_node(), "move_statement").len(), 2);
                assert_eq!(nodes(tree.root_node(), "display_statement").len(), 4);
            }
        }
    }
}

#[test]
fn unknown_and_noninteger_named_bounds_are_only_unevaluated_syntax_candidates() {
    // A name token cannot prove declaration, constant type, positivity or rank.
    // These trees deliberately preserve that distinction from compiler checking.
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let text = program(
                    &[
                        "78 Text-Limit value 'seven'.",
                        "01 Table-Group.",
                        "03 Unknown-Entry pic x occurs Missing-Limit times.",
                        "03 Text-Entry pic x occurs Text-Limit times.",
                    ],
                    &[],
                    profile,
                    ending,
                    final_newline,
                );
                let tree = parse(&text, profile);
                clean(&tree, &text);
                assert_eq!(nodes(tree.root_node(), "occurs_clause").len(), 2);
                assert_eq!(nodes(tree.root_node(), "occurs_bound").len(), 2);
                exact_bounds(
                    clause(&tree, &text, "occurs Missing-Limit times"),
                    &text,
                    &[("Missing-Limit", "WORD")],
                );
                exact_bounds(
                    clause(&tree, &text, "occurs Text-Limit times"),
                    &text,
                    &[("Text-Limit", "WORD")],
                );
                assert!(
                    nodes(tree.root_node(), "entry_name").iter().all(|name| {
                        name.utf8_text(text.as_bytes()).unwrap() != "Missing-Limit"
                    })
                );
                for name in [
                    "T",
                    "Ti",
                    "Tim",
                    "Time",
                    "Times-Limit",
                    "To1",
                    "To-Limit",
                    "Depending-Limit",
                    "Indexed2",
                    "Values-Count",
                    "1Row",
                    "2-Row3",
                ] {
                    let written = format!("occurs {name} times");
                    let entry = format!("03 Cell pic x {written}.");
                    let text = program(
                        &["01 Table-Group.", &entry],
                        &[],
                        profile,
                        ending,
                        final_newline,
                    );
                    let tree = parse(&text, profile);
                    clean(&tree, &text);
                    exact_bounds(clause(&tree, &text, &written), &text, &[(name, "WORD")]);
                }
            }
        }
    }
}

#[test]
fn missing_quoted_and_expression_shaped_bounds_never_become_clean_whole_clauses() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for written in [
                    "occurs",
                    // WORD is contextual; reserved separators must not silently
                    // turn a missing-bound fixture into a constant candidate.
                    "occurs times",
                    "occurs to",
                    "occurs tImEs",
                    "occurs TO",
                    "occurs depending",
                    "occurs indexed",
                    "occurs pic",
                    "occurs value",
                    "occurs 1 to",
                    "occurs '7' times",
                    "occurs \"7\" times",
                    "occurs (Upper-Limit) times",
                    "occurs Upper-Limit() times",
                    "occurs Upper-Limit(1) times",
                    "occurs Upper-Limit(1:2) times",
                    "occurs Upper-Limit + 1 times",
                    "occurs 1 to Upper-Limit + 1 times",
                    "occurs function length('x') times",
                    "occurs Upper-Limit depending on",
                    "occurs 1 to '7' times",
                    "occurs Upper-Limit indexed by",
                ] {
                    let entry = format!("03 Cell pic x(20) {written}.");
                    let text = program(
                        &["78 Upper-Limit value 7.", "01 Table-Group.", &entry],
                        &[],
                        profile,
                        ending,
                        final_newline,
                    );
                    let tree = parse(&text, profile);
                    recovery(&tree, &text);
                    let expected = unique_range(&text, written);
                    assert!(
                        nodes(tree.root_node(), "occurs_clause").iter().all(|node| {
                            node.byte_range() != expected || node.has_error() || node.is_missing()
                        }),
                        "malformed whole clause retained as clean: {written:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn incomplete_table_subscripts_and_modifiers_keep_native_recovery() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for operand in [
                    "Cell()",
                    "Cell(Position",
                    "Cell(:3)",
                    "Cell(Position)(:3)",
                    "Cell(Position)(2:3",
                    "Cell(Position +)",
                ] {
                    let statement = format!("display {operand}.");
                    let text = program(
                        &[
                            "78 Upper-Limit value 7.",
                            "01 Position pic 9.",
                            "01 Table-Group.",
                            "03 Cell pic x(20) occurs Upper-Limit times.",
                        ],
                        &[&statement],
                        profile,
                        ending,
                        final_newline,
                    );
                    let tree = parse(&text, profile);
                    recovery(&tree, &text);
                    exact_bounds(
                        clause(&tree, &text, "occurs Upper-Limit times"),
                        &text,
                        &[("Upper-Limit", "WORD")],
                    );
                }
            }
        }
    }
}

#[test]
fn bound_and_subscript_edits_restore_the_exact_fresh_native_inventory() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let original = program(
                    &[
                        "78 Lower-Limit value 1.",
                        "78 Upper-Limit value 7.",
                        "01 Position pic 9.",
                        "01 Table-Group.",
                        "03 Cell pic x(20) occurs Upper-Limit times.",
                    ],
                    &["display Cell(Position)."],
                    profile,
                    ending,
                    final_newline,
                );
                let mut text = original.clone();
                let mut parser = profile.parser();
                let mut tree = bounded_parse(&mut parser, &text, None);
                clean(&tree, &text);
                let original_inventory = inventory(&tree, &text);
                for (before, after) in [
                    ("occurs Upper-Limit", "occurs Lower-Limit"),
                    ("Cell(Position)", "Cell(Position + 1)"),
                    ("Cell(Position + 1)", "Cell(Position)"),
                    ("occurs Lower-Limit", "occurs Upper-Limit"),
                ] {
                    let range = unique_range(&text, before);
                    let mut updated = text.clone();
                    updated.replace_range(range.clone(), after);
                    let new_end = range.start + after.len();
                    tree.edit(&InputEdit {
                        start_byte: range.start,
                        old_end_byte: range.end,
                        new_end_byte: new_end,
                        start_position: point(&text, range.start),
                        old_end_position: point(&text, range.end),
                        new_end_position: point(&updated, new_end),
                    });
                    let actual = bounded_parse(&mut parser, &updated, Some(&tree));
                    let fresh = parse(&updated, profile);
                    clean(&actual, &updated);
                    clean(&fresh, &updated);
                    assert_eq!(inventory(&actual, &updated), inventory(&fresh, &updated));
                    tree = actual;
                    text = updated;
                }
                assert_eq!(text.as_bytes(), original.as_bytes());
                assert_eq!(inventory(&tree, &text), original_inventory);
            }
        }
    }
}
