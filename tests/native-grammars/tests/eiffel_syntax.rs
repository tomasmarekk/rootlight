//! Native Eiffel contracts retain declaration heads, inert text and edit ranges.
//! These finite syntax checks do not establish product reference resolution.

use std::ops::{ControlFlow, Range};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_eiffel::LANGUAGE_EIFFEL.into())
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

fn valid(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some());
    }
    tree
}

fn selected<'a>(tree: &'a Tree, source: &'a str, kind: &str) -> Vec<&'a str> {
    nodes(tree.root_node())
        .into_iter()
        .filter(|node| node.is_named() && node.kind() == kind)
        .map(|node| &source[node.byte_range()])
        .collect()
}

fn signature(tree: &Tree) -> Vec<(String, Range<usize>, bool, bool)> {
    nodes(tree.root_node())
        .into_iter()
        .map(|node| {
            (
                node.kind().to_owned(),
                node.byte_range(),
                node.is_named(),
                node.is_missing(),
            )
        })
        .collect()
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix.rfind('\n').map_or(offset, |last| offset - last - 1),
    )
}

#[test]
fn generic_class_inheritance_adaptation_and_feature_heads_keep_exact_spans() {
    let language: tree_sitter::Language = tree_sitter_eiffel::LANGUAGE_EIFFEL.into();
    assert_eq!(language.abi_version(), 15);
    let source = "note\n description: \"syntax fixture\"\ndeferred class BOX [G -> ANY]\ninherit\n PARENT [G]\n  rename old_item as parent_item\n  export {NONE} parent_item\n  undefine parent_item\n  redefine item\n  select item\n  end\ncreate make\nfeature {NONE}\n count, capacity: INTEGER\n answer: INTEGER = 42\n item: G deferred end\n make (value: G)\n  do\n  end\nend\n";
    let tree = valid(source);
    assert_eq!(selected(&tree, source, "header_mark"), ["deferred"]);
    assert_eq!(selected(&tree, source, "formal_generic"), ["G -> ANY"]);
    assert_eq!(
        selected(&tree, source, "rename_pair"),
        ["old_item as parent_item"]
    );
    assert_eq!(
        selected(&tree, source, "new_exports"),
        ["export {NONE} parent_item"]
    );
    assert_eq!(
        selected(&tree, source, "undefine"),
        ["undefine parent_item"]
    );
    assert_eq!(selected(&tree, source, "redefine"), ["redefine item"]);
    assert_eq!(selected(&tree, source, "select"), ["select item"]);
    assert_eq!(
        selected(&tree, source, "new_feature"),
        ["count", "capacity", "answer", "item", "make"]
    );
    assert_eq!(selected(&tree, source, "formal_arguments"), ["(value: G)"]);
    let all = nodes(tree.root_node());
    let class = all
        .iter()
        .find(|node| node.kind() == "class_declaration")
        .unwrap();
    for feature in all
        .iter()
        .filter(|node| node.kind() == "feature_declaration")
    {
        assert!(class.byte_range().contains(&feature.start_byte()));
        assert_eq!(feature.parent().unwrap().kind(), "feature_clause");
    }
}

#[test]
fn routines_contracts_creation_agents_and_qualified_calls_remain_distinct() {
    let source = "class WORKER\nfeature\n run (input: detachable STRING): INTEGER\n  require\n   input_present: input /= Void\n  local\n   object: ARRAY [INTEGER]\n   callback: PROCEDURE\n  do\n   create object.make_empty\n   object.extend (1)\n   callback := agent object.wipe_out\n   callback := agent (value: INTEGER) do object.extend (value) end\n   Result := object.count\n  ensure\n   nonnegative: Result >= 0\n  rescue\n   retry\n  end\n cached: INTEGER once Result := 42 end\n native: INTEGER external \"C\" alias \"constant_value\" end\ninvariant\n stable: True\nend\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "creation"),
        ["create object.make_empty"]
    );
    assert_eq!(
        selected(&tree, source, "call_agent"),
        ["agent object.wipe_out"]
    );
    assert_eq!(
        selected(&tree, source, "inline_agent"),
        ["agent (value: INTEGER) do object.extend (value) end"]
    );
    assert_eq!(
        selected(&tree, source, "external"),
        ["external \"C\" alias \"constant_value\""]
    );
    assert_eq!(selected(&tree, source, "once"), ["once"]);
    assert_eq!(
        selected(&tree, source, "tag"),
        ["input_present", "nonnegative", "stable"]
    );
    assert!(selected(&tree, source, "call").contains(&"object.extend (value)"));
    assert_eq!(
        selected(&tree, source, "entity_declaration_group"),
        [
            "input: detachable STRING",
            "object: ARRAY [INTEGER]",
            "callback: PROCEDURE",
            "value: INTEGER"
        ]
    );
}

#[test]
fn strings_and_comments_do_not_create_declarations_or_comment_nodes_inside_verbatim() {
    let source = "class ACTUAL\nfeature\n -- class COMMENT_FAKE feature invented: INTEGER end\n text: STRING = \"class STRING_FAKE feature fake: INTEGER end\"\n block: STRING = \"[\n -- class VERBATIM_FAKE feature phantom: INTEGER end\n 😀 λ\n ]\"\n after_text: INTEGER\nend\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "new_feature"),
        ["text", "block", "after_text"]
    );
    let verbatim = nodes(tree.root_node())
        .into_iter()
        .find(|node| node.kind() == "verbatim_string")
        .unwrap();
    assert!(!nodes(verbatim).iter().any(|node| matches!(
        node.kind(),
        "comment" | "header_comment" | "class_declaration"
    )));
    assert_eq!(selected(&tree, source, "class_declaration").len(), 1);
    for kind in ["class_name", "identifier"] {
        assert!(
            !selected(&tree, source, kind)
                .iter()
                .any(|name| name.contains("FAKE")
                    || matches!(*name, "invented" | "fake" | "phantom"))
        );
    }
}

#[test]
fn unicode_string_and_crlf_ranges_survive_utf16_input() {
    let source =
        "class ENCODED\r\nfeature\r\n text: STRING = \"λ 😀\"\r\n tail: INTEGER\r\nend\r\n";
    let utf8 = valid(source);
    assert_eq!(selected(&utf8, source, "new_feature"), ["text", "tail"]);
    let utf16: Vec<_> = source.encode_utf16().collect();
    let tree = parser().parse_utf16_le(&utf16, None).unwrap();
    assert!(!tree.root_node().has_error());
    let names: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|node| node.kind() == "new_feature")
        .map(|node| String::from_utf16(&utf16[node.start_byte() / 2..node.end_byte() / 2]).unwrap())
        .collect();
    assert_eq!(names, ["text", "tail"]);
    for node in nodes(tree.root_node()) {
        assert_eq!(node.start_byte() % 2, 0);
        assert_eq!(node.end_byte() % 2, 0);
        assert!(node.end_byte() <= utf16.len() * 2);
    }
}

#[test]
fn incremental_edits_and_error_recovery_match_fresh_parsing() {
    let before = "class EDITED\nfeature\n old_name: INTEGER\n tail: INTEGER\nend\n";
    let start = before.find("old_name").unwrap();
    let old_end = start + "old_name".len();
    let after = before.replacen("old_name", "replacement_name", 1);
    let new_end = start + "replacement_name".len();
    let mut previous = valid(before);
    previous.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before, start),
        old_end_position: point(before, old_end),
        new_end_position: point(&after, new_end),
    });
    let incremental = parser().parse(&after, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&valid(&after)));
    assert_eq!(
        selected(&incremental, &after, "new_feature"),
        ["replacement_name", "tail"]
    );
    let unfinished = "class EDITED feature retained: INTEGER unfinished";
    let recovered = parser().parse(unfinished, None).unwrap();
    assert!(recovered.root_node().has_error());
    assert_eq!(
        selected(&recovered, unfinished, "new_feature"),
        ["retained", "unfinished"]
    );
}

#[test]
fn cancellation_preserves_reusable_parser_without_evaluating_source() {
    let mut parser = parser();
    let source = format!(
        "class MANY\nfeature\n{}end\n",
        "value: INTEGER\n".repeat(20_000)
    );
    let mut interrupted = false;
    let mut cancel = |_: &tree_sitter::ParseState| {
        interrupted = true;
        ControlFlow::Break(())
    };
    let result = parser.parse_with_options(
        &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut cancel)),
    );
    assert!(interrupted);
    assert!(result.is_none());
    parser.reset();
    let tree = parser.parse("class NEXT end", None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn tagged_and_braced_verbatim_remain_unresolved_despite_permissive_basic_strings() {
    // The grammar accepts these bytes through its broader basic-string rule.
    // Lack of ERROR nodes therefore cannot establish verbatim-string support.
    for source in [
        "class TAGGED feature text: STRING = \"tag[\ncontent\n]tag\" end",
        "class BRACED feature text: STRING = \"{\ncontent\n}\" end",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        assert!(selected(&tree, source, "verbatim_string").is_empty());
        assert_eq!(selected(&tree, source, "basic_manifest_string").len(), 1);
        for node in nodes(tree.root_node()) {
            assert!(source.get(node.byte_range()).is_some());
        }
    }
}

#[test]
fn integer_bases_preserve_the_complete_literal_and_following_feature() {
    for literal in [
        "0xFF", "0XfF_01", "0c17", "0C1_07", "0b101", "0B10_01", "1_234",
    ] {
        let source = format!("class NUMBERS feature value: INTEGER = {literal} tail: INTEGER end");
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "integer"), [literal]);
        assert_eq!(selected(&tree, &source, "new_feature"), ["value", "tail"]);
    }
}

#[test]
fn character_escapes_space_and_unicode_preserve_exact_source_and_tail() {
    for character in [
        " ",
        "λ",
        "😀",
        "%N",
        "%T",
        "%%",
        "%'",
        "%\"",
        "%(",
        "%)",
        "%<",
        "%>",
        "%/0/",
        "%/123/",
        "%/0xFFFD/",
        "%/0X1_f600/",
        "%/0c17/",
        "%/0b101/",
    ] {
        let source = format!(
            "class CHARACTERS feature value: CHARACTER_32 = '{character}' tail: INTEGER end"
        );
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "character"), [character]);
        assert_eq!(selected(&tree, &source, "new_feature"), ["value", "tail"]);
    }
}

#[test]
fn malformed_character_literals_remain_explicit_parse_errors() {
    for literal in ["''", "'ab'", "'%Z'", "'%/0xG/'", "'%/12 /'", "'a '", "'\n'"] {
        let source =
            format!("class INVALID feature value: CHARACTER = {literal} tail: INTEGER end");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}\n{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn declaration_semicolons_preserve_fields_constants_routines_and_tail() {
    for separator in [";", ";;", "; -- declaration separator\n ;"] {
        let source = format!(
            "class SEPARATED\nfeature\n count, capacity: INTEGER{separator}\n answer: INTEGER = 42{separator}\n text: STRING = \";\"{separator}\n run do end{separator}\n tail: INTEGER{separator}\nend\n"
        );
        let tree = valid(&source);
        assert_eq!(
            selected(&tree, &source, "new_feature"),
            ["count", "capacity", "answer", "text", "run", "tail"]
        );
        assert_eq!(selected(&tree, &source, "basic_manifest_string"), ["\";\""]);
        assert_eq!(selected(&tree, &source, "feature_declaration").len(), 5);
        let before = source.replacen(separator, "", 1);
        let offset = before.find("INTEGER").unwrap() + "INTEGER".len();
        let mut previous = valid(&before);
        previous.edit(&InputEdit {
            start_byte: offset,
            old_end_byte: offset,
            new_end_byte: offset + separator.len(),
            start_position: point(&before, offset),
            old_end_position: point(&before, offset),
            new_end_position: point(&source, offset + separator.len()),
        });
        let incremental = parser().parse(&source, Some(&previous)).unwrap();
        assert_eq!(signature(&incremental), signature(&tree));
    }
    let malformed = "class BROKEN feature value: INTEGER = ; tail: INTEGER end";
    assert!(
        parser()
            .parse(malformed, None)
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn keyword_case_preserves_token_kinds_names_inert_text_and_incremental_trees() {
    let source = "cLaSs MiXeD\ninherit BASE\nfEaTuRe\n Flag: BOOLEAN = tRuE\n Other: BOOLEAN = fAlSe\n classify: STRING = \"class EnD TRUE\"\n Run do precursor {BASE} end\n end_marker: INTEGER\neNd\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "new_feature"),
        ["Flag", "Other", "classify", "Run", "end_marker"]
    );
    assert_eq!(
        selected(&tree, source, "boolean_constant"),
        ["tRuE", "fAlSe"]
    );
    assert_eq!(selected(&tree, source, "precursor"), ["precursor {BASE}"]);
    assert_eq!(
        selected(&tree, source, "basic_manifest_string"),
        ["\"class EnD TRUE\""]
    );
    let all = nodes(tree.root_node());
    for (kind, text) in [("class", "cLaSs"), ("feature", "fEaTuRe"), ("end", "eNd")] {
        assert!(all.iter().any(|node| {
            !node.is_named() && node.kind() == kind && &source[node.byte_range()] == text
        }));
    }
    let before = source.replacen("cLaSs", "class", 1);
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: 0,
        old_end_byte: 5,
        new_end_byte: 5,
        start_position: Point::new(0, 0),
        old_end_position: Point::new(0, 5),
        new_end_position: Point::new(0, 5),
    });
    let incremental = parser().parse(source, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&tree));
    for name in ["ClAsS", "TrUe", "EnD", "pReCuRsOr"] {
        let malformed = format!("class BOX feature {name}: INTEGER end");
        assert!(
            parser()
                .parse(&malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "reserved feature name was accepted: {name}"
        );
    }
}

#[test]
fn combined_header_marks_and_multiple_aliases_keep_written_declarations() {
    let source = "frozen expanded class ORDERED\r\ninherit PARENT rename old_less as parent_less alias \"<=\" alias \"≤\" end\r\nfeature\r\n less alias \"<=\" alias \"≤\" (other: like Current): BOOLEAN do Result := True end\r\n tail: INTEGER\r\nend\r\n";
    let tree = valid(source);
    assert_eq!(selected(&tree, source, "header_mark"), ["frozen expanded"]);
    assert_eq!(
        selected(&tree, source, "rename_pair"),
        ["old_less as parent_less alias \"<=\" alias \"≤\""]
    );
    assert_eq!(
        selected(&tree, source, "new_feature"),
        ["less alias \"<=\" alias \"≤\"", "tail"]
    );
    assert_eq!(
        selected(&tree, source, "formal_arguments"),
        ["(other: like Current)"]
    );
    let added = " alias \"≤\"";
    let offset = source.find(added).unwrap();
    let before = source.replacen(added, "", 1);
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset,
        new_end_byte: offset + added.len(),
        start_position: point(&before, offset),
        old_end_position: point(&before, offset),
        new_end_position: point(source, offset + added.len()),
    });
    let incremental = parser().parse(source, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&tree));
    for mark in ["frozen deferred", "frozen once", "FrOzEn ExPaNdEd"] {
        let source = format!("{mark} class MARKED end");
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "header_mark"), [mark]);
    }
    for malformed in [
        "class BROKEN feature less alias \"<=\" alias end",
        "frozen frozen class BROKEN feature tail: INTEGER end",
        "expanded frozen class BROKEN feature tail: INTEGER end",
    ] {
        assert!(
            parser()
                .parse(malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "malformed declaration was accepted: {malformed}"
        );
    }
}

#[test]
fn note_separators_keep_class_and_routine_ownership_and_inert_values() {
    for separator in [";", ";;", "; -- note separator\n ;"] {
        let source = format!(
            "note names: alpha, beta{separator} description: \"class FAKE feature invented: INTEGER end\"{separator}\nclass BOX\nfeature\n run note description: \"routine metadata\"{separator} do end\n tail: INTEGER\nnote names: closing{separator} description: \"class metadata\"{separator}\nend\n"
        );
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "new_feature"), ["run", "tail"]);
        assert_eq!(selected(&tree, &source, "notes").len(), 3);
        let class = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "class_declaration")
            .unwrap();
        let mut cursor = class.walk();
        assert_eq!(
            class
                .named_children(&mut cursor)
                .filter(|node| node.kind() == "notes")
                .count(),
            2
        );
        let routine = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "attribute_or_routine")
            .unwrap();
        let mut cursor = routine.walk();
        assert_eq!(
            routine
                .named_children(&mut cursor)
                .filter(|node| node.kind() == "notes")
                .count(),
            1
        );
        let offset = source.find(separator).unwrap();
        let before = source.replacen(separator, "", 1);
        let mut previous = valid(&before);
        previous.edit(&InputEdit {
            start_byte: offset,
            old_end_byte: offset,
            new_end_byte: offset + separator.len(),
            start_position: point(&before, offset),
            old_end_position: point(&before, offset),
            new_end_position: point(&source, offset + separator.len()),
        });
        let incremental = parser().parse(&source, Some(&previous)).unwrap();
        assert_eq!(signature(&incremental), signature(&tree));
    }
    let malformed = "note names: ; class BOX feature tail: INTEGER end";
    assert!(
        parser()
            .parse(malformed, None)
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn type_marks_unique_attributes_and_addresses_keep_exact_nodes() {
    let source = "class MARKED [G -> detachable separate ANY]\r\nfeature\r\n first, second: INTEGER = unique;\r\n item: detachable separate G\r\n pair: attached separate TUPLE [label: STRING]\r\n anchor: detachable separate like Current\r\n value: expanded VALUE\r\n address: POINTER do consume ($Result) consume ($Current) end\r\n tail: INTEGER\r\nend\r\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "new_feature"),
        [
            "first", "second", "item", "pair", "anchor", "value", "address", "tail"
        ]
    );
    assert_eq!(selected(&tree, source, "explicit_value"), ["= unique"]);
    assert_eq!(selected(&tree, source, "address"), ["$Result", "$Current"]);
    assert_eq!(
        selected(&tree, source, "anchored"),
        ["detachable separate like Current"]
    );
    assert!(selected(&tree, source, "class_type").contains(&"expanded VALUE"));
    let added = "separate ";
    let offset = source.find(added).unwrap();
    let before = source.replacen(added, "", 1);
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset,
        new_end_byte: offset + added.len(),
        start_position: point(&before, offset),
        old_end_position: point(&before, offset),
        new_end_position: point(source, offset + added.len()),
    });
    let incremental = parser().parse(source, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&tree));
    for malformed in [
        "class BOX feature item: separate detachable STRING tail: INTEGER end",
        "class BOX feature item: = unique tail: INTEGER end",
        "class BOX feature run do consume ($class) end tail: INTEGER end",
        "class BOX feature unique: INTEGER end",
    ] {
        assert!(
            parser()
                .parse(malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "malformed atom was accepted: {malformed}"
        );
    }
}

#[test]
fn list_separators_preserve_declaration_groups_and_syntax_boundaries() {
    let source = "note ;; title: \"Lists\";;;\r\nclass BOX\r\ninherit ;; PARENT export ;; {ANY} item;;; {NONE} hidden;; end;;; OTHER;;\r\nfeature ;;\r\n run (;; first: INTEGER;;; second: INTEGER;;)\r\n require ;; ready: True;;;\r\n local ;; temporary: INTEGER;;; extra: INTEGER;;\r\n do temporary := first check ;; valid: True;;; end\r\n ensure ;; done: True;;; end;;;\r\n pair: TUPLE [;; left: INTEGER;;; right: INTEGER;;]\r\n tail: INTEGER\r\ninvariant ;; valid: True;;;\r\nend\r\n";
    let tree = valid(source);
    assert_eq!(
        selected(&tree, source, "new_feature"),
        ["run", "pair", "tail"]
    );
    assert_eq!(selected(&tree, source, "parent").len(), 2);
    assert_eq!(selected(&tree, source, "new_export_item").len(), 2);
    assert_eq!(
        selected(&tree, source, "entity_declaration_group"),
        [
            "first: INTEGER",
            "second: INTEGER",
            "temporary: INTEGER",
            "extra: INTEGER",
            "left: INTEGER",
            "right: INTEGER",
        ]
    );
    assert_eq!(selected(&tree, source, "assertion_clause").len(), 4);
    let offset = source.find("first: INTEGER;;;").unwrap() + "first: INTEGER".len();
    let mut before = source.to_owned();
    before.remove(offset);
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset,
        new_end_byte: offset + 1,
        start_position: point(&before, offset),
        old_end_position: point(&before, offset),
        new_end_position: point(source, offset + 1),
    });
    let incremental = parser().parse(source, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&tree));
    valid("class EMPTY feature run (;;) do end first: TUPLE [] second: TUPLE [;;] end");
    for malformed in [
        "class BOX feature run (item; : INTEGER) do end tail: INTEGER end",
        "class BOX feature item: ARRAY [INTEGER; STRING] tail: INTEGER end",
        "class BOX feature run do consume (first; second) end tail: INTEGER end",
        "class BOX feature run (first: ; second: INTEGER) do end tail: INTEGER end",
    ] {
        assert!(
            parser()
                .parse(malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "separator escaped its list position: {malformed}"
        );
    }
}

#[test]
fn inline_separate_preserves_alias_heads_and_nested_blocks() {
    let source = "class BOX\r\nfeature\r\n run do\r\n separate target as held, other.item as second do\r\n  separate held.item as nested do nested.use (second) end\r\n end\r\n end\r\n tail: INTEGER\r\nend\r\n";
    let tree = valid(source);
    let aliases: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|node| {
            node.kind() == "identifier"
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "separate")
        })
        .map(|node| &source[node.byte_range()])
        .collect();
    assert_eq!(aliases, ["held", "second", "nested"]);
    assert_eq!(selected(&tree, source, "separate").len(), 2);
    assert_eq!(selected(&tree, source, "new_feature"), ["run", "tail"]);
    let offset = source.find("as second do").unwrap() + "as second ".len();
    let mut before = source.to_owned();
    before.replace_range(offset..offset + 2, "then");
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset + 4,
        new_end_byte: offset + 2,
        start_position: point(&before, offset),
        old_end_position: point(&before, offset + 4),
        new_end_position: point(source, offset + 2),
    });
    let incremental = parser().parse(source, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&tree));
    valid("class BOX feature run do separate target as held do end end end");
    for malformed in [
        "class BOX feature run do separate target held do held.use (1) end end end",
        "class BOX feature run do separate target as class do end end end",
        "class BOX feature run do separate target as held do held.use (1) end",
    ] {
        assert!(
            parser()
                .parse(malformed, None)
                .unwrap()
                .root_node()
                .has_error(),
            "malformed separate instruction was accepted: {malformed}"
        );
    }
}

#[test]
fn real_exponents_preserve_contiguous_decimal_components() {
    for literal in [
        "4.2E+7",
        "1.0E-10",
        "1.0e6",
        ".3e-10",
        "1.E3",
        "1_2.3_4E+5_6",
        "12.0",
        "12.",
        ".5",
    ] {
        let source = format!("class BOX feature value: REAL = {literal} tail: INTEGER end");
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "real"), [literal]);
        assert_eq!(selected(&tree, &source, "new_feature"), ["value", "tail"]);
        if let Some(offset) = literal.find(['e', 'E']) {
            assert_eq!(
                selected(&tree, &source, "real_exponent"),
                [&literal[offset..]]
            );
        }
    }
    let source = "class BOX feature value: REAL = .3E-10 tail: INTEGER end\r\n";
    let tree = valid(source);
    let before = source.replace(".3E-10", ".3e-10");
    let offset = source.find("E-10").unwrap();
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset + 1,
        new_end_byte: offset + 1,
        start_position: point(&before, offset),
        old_end_position: point(&before, offset + 1),
        new_end_position: point(source, offset + 1),
    });
    let incremental = parser().parse(source, Some(&previous)).unwrap();
    assert_eq!(signature(&incremental), signature(&tree));
    for literal in [
        "1.0E+", "1.0e", "1.0e+ 5", "1.0e 5", "1. 0", ". 5", ".E3", "1e3", "1.0e+_5", "1.0e5_",
    ] {
        let source = format!("class BOX feature value: REAL = {literal} tail: INTEGER end");
        assert!(
            parser()
                .parse(&source, None)
                .unwrap()
                .root_node()
                .has_error(),
            "malformed real literal was accepted: {literal}"
        );
    }
}

#[test]
fn formal_representation_marks_preserve_parameter_names_and_contextual_identifiers() {
    let source = "class BOX [expanded G -> ANY, ReFeReNcE H, frozen K]\r\nfeature item: G other: H reference: INTEGER end\r\n";
    let tree = valid(source);
    let names: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|node| {
            node.kind() == "class_name"
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "formal_generic")
        })
        .map(|node| &source[node.byte_range()])
        .collect();
    assert_eq!(names, ["G", "H", "K"]);
    assert_eq!(
        selected(&tree, source, "new_feature"),
        ["item", "other", "reference"]
    );
    let before = source.replacen("expanded ", "", 1);
    let offset = source.find("expanded ").unwrap();
    let mut previous = valid(&before);
    previous.edit(&InputEdit {
        start_byte: offset,
        old_end_byte: offset,
        new_end_byte: offset + "expanded ".len(),
        start_position: point(&before, offset),
        old_end_position: point(&before, offset),
        new_end_position: point(source, offset + "expanded ".len()),
    });
    assert_eq!(
        signature(&parser().parse(source, Some(&previous)).unwrap()),
        signature(&tree)
    );
    let identifier = "class BOX [reference] feature item: reference end";
    let tree = valid(identifier);
    assert_eq!(selected(&tree, identifier, "formal_generic"), ["reference"]);
    for parameters in [
        "expanded",
        "expanded reference G",
        "expanded expanded G",
        "frozen expanded G",
    ] {
        let source = format!("class BOX [{parameters}] feature tail: INTEGER end");
        assert!(
            parser()
                .parse(&source, None)
                .unwrap()
                .root_node()
                .has_error(),
            "malformed generic marks accepted: {parameters}"
        );
    }
}

#[test]
fn symbolic_operators_keep_whole_tokens_and_reserved_operator_kinds() {
    for spelling in [
        "∋", "⊆", "√", "∀∀", "∑⊆", "%", "`", "-*", "/*", "/~∋", "\\", "+++",
    ] {
        let source = format!(
            "class BOX feature run (left, right: INTEGER): INTEGER do Result := left {spelling} right end tail: INTEGER end"
        );
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "free_operator"), [spelling]);
        assert_eq!(selected(&tree, &source, "new_feature"), ["run", "tail"]);
    }
    for (spelling, kind) in [
        ("+", "binary_plus_minus"),
        ("-", "binary_plus_minus"),
        ("*", "binary_mul_div"),
        ("/", "binary_mul_div"),
        ("//", "binary_mul_div"),
        ("\\\\", "binary_mul_div"),
        ("^", "binary_caret"),
        ("<", "binary_comparison"),
        (">", "binary_comparison"),
    ] {
        let source = format!(
            "class BOX feature run (left, right: INTEGER): INTEGER do Result := left {spelling} right end tail: INTEGER end"
        );
        let tree = valid(&source);
        assert!(selected(&tree, &source, "free_operator").is_empty());
        assert_eq!(selected(&tree, &source, kind), [spelling]);
    }
    let source = "class BOX feature run (left, right: INTEGER): INTEGER do Result := left -- ∋ right\n end tail: INTEGER end";
    let tree = valid(source);
    assert!(selected(&tree, source, "free_operator").is_empty());
    assert_eq!(selected(&tree, source, "comment"), ["-- ∋ right\n"]);
    let source = "class BOX feature run (left, right: INTEGER): INTEGER do Result := left ∋\u{a0}∋ right end tail: INTEGER end";
    let tree = valid(source);
    assert_eq!(selected(&tree, source, "free_operator"), ["∋", "∋"]);
}

#[test]
fn symbolic_operator_edits_match_fresh_trees_without_accepting_delimiters() {
    let before = "class BOX feature run (left, right: INTEGER): INTEGER do Result := left ∋ right end tail: INTEGER end";
    let after = before.replace('∋', "⊆");
    let mut parser = parser();
    let mut prior = parser.parse(before, None).unwrap();
    let start_byte = before.find('∋').unwrap();
    let end_byte = start_byte + '∋'.len_utf8();
    prior.edit(&InputEdit {
        start_byte,
        old_end_byte: end_byte,
        new_end_byte: end_byte,
        start_position: point(before, start_byte),
        old_end_position: point(before, end_byte),
        new_end_position: point(&after, end_byte),
    });
    let incremental = parser.parse(&after, Some(&prior)).unwrap();
    let fresh = valid(&after);
    assert_eq!(signature(&incremental), signature(&fresh));
    for expression in [
        "* left",
        "/ left",
        "^ left",
        "// left",
        "~ left",
        "/~ left",
        "left λ right",
        "left \u{1} right",
        "left -> right",
        "left << right",
        "left >> right",
        "left ∋",
    ] {
        let source = format!(
            "class BOX feature run (left, right: INTEGER): INTEGER do Result := {expression} end tail: INTEGER end"
        );
        let tree = parser.parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "accepted {expression}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn symbolic_operator_delimiter_boundaries_follow_whole_spellings() {
    let alphabet = ['∋', '-', '<', '>', '/', '~', '*', '+'];
    let reserved = ["+", "-", "*", "/", "<", ">", "~", "//", "/~"];
    let mut parser = parser();
    let mut checked = 0;
    for length in 1_u32..=3 {
        for mut number in 0..alphabet.len().pow(length) {
            let spelling: String = (0..length)
                .map(|_| {
                    let character = alphabet[number % alphabet.len()];
                    number /= alphabet.len();
                    character
                })
                .collect();
            let expected_free = !reserved.contains(&spelling.as_str())
                && !["--", "->", "<<", ">>"]
                    .iter()
                    .any(|delimiter| spelling.contains(delimiter));
            let source = format!(
                "class BOX feature run (left, right: INTEGER): INTEGER do Result := left {spelling} right\n end tail: INTEGER end"
            );
            let tree = parser.parse(&source, None).unwrap();
            let operators = selected(&tree, &source, "free_operator");
            assert_eq!(
                operators.iter().any(|text| *text == spelling),
                expected_free,
                "{spelling}: {}",
                tree.root_node().to_sexp()
            );
            if expected_free {
                assert!(
                    !tree.root_node().has_error(),
                    "{spelling}: {}",
                    tree.root_node().to_sexp()
                );
                assert_eq!(operators, [spelling.as_str()]);
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 584);
}

fn assert_operator_before_comment(spelling: &str, kind: &str) {
    let source = format!(
        "class BOX feature run (left, right: INTEGER): INTEGER do Result := left {spelling}--ignored\n right end tail: INTEGER end"
    );
    let tree = valid(&source);
    assert_eq!(selected(&tree, &source, kind), [spelling]);
    assert_eq!(selected(&tree, &source, "comment"), ["--ignored\n"]);
    assert_eq!(selected(&tree, &source, "new_feature"), ["run", "tail"]);
}

#[test]
fn symbolic_operator_preserves_adjacent_comment_span() {
    assert_operator_before_comment("∋", "free_operator");
}

#[test]
fn builtin_operator_preserves_adjacent_comment_span() {
    assert_operator_before_comment("+", "binary_plus_minus");
}

#[test]
fn legacy_operator_preserves_adjacent_comment_span() {
    assert_operator_before_comment("#abc", "free_operator");
}

#[test]
fn symbolic_operator_preserves_adjacent_delimiter_spans() {
    for delimiter in ["->", "<<", ">>"] {
        let source = format!(
            "class BOX feature run (left, right: INTEGER): INTEGER do Result := left ∋{delimiter} right end tail: INTEGER end"
        );
        let tree = parser().parse(&source, None).unwrap();
        let operators = selected(&tree, &source, "free_operator");
        assert_eq!(
            operators,
            ["∋"],
            "{delimiter}: {}",
            tree.root_node().to_sexp()
        );
        let boundary = source.find(delimiter).unwrap();
        for node in nodes(tree.root_node())
            .into_iter()
            .filter(|node| node.kind() == "free_operator")
        {
            assert_eq!(
                node.end_byte(),
                boundary,
                "{delimiter}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
}

#[test]
fn operator_comment_and_delimiter_edits_match_fresh_trees_at_eof() {
    let prefix = "class BOX feature run (left, right: INTEGER): INTEGER do Result := left ";
    let tail = " right end tail: INTEGER end";
    for (old, new, suffix) in [
        ("∋--ignored\n", "∋->", tail),
        ("∋->", "∋--ignored\n", tail),
        ("∋<<", "∋>>", tail),
        ("∋>>", "∋", tail),
        ("∋--ignored\n", "∋", tail),
        ("∋", "∋--ignored", ""),
        ("∋--ignored", "∋--ignored\n right end tail: INTEGER end", ""),
    ] {
        let before = format!("{prefix}{old}{suffix}");
        let after = format!("{prefix}{new}{suffix}");
        let mut parser = parser();
        let mut prior = parser.parse(&before, None).unwrap();
        let start_byte = prefix.len();
        let old_end_byte = start_byte + old.len();
        let new_end_byte = start_byte + new.len();
        prior.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position: point(&before, start_byte),
            old_end_position: point(&before, old_end_byte),
            new_end_position: point(&after, new_end_byte),
        });
        let incremental = parser.parse(&after, Some(&prior)).unwrap();
        let fresh = parser.parse(&after, None).unwrap();
        assert_eq!(
            signature(&incremental),
            signature(&fresh),
            "{old:?} -> {new:?}"
        );
        assert_eq!(selected(&incremental, &after, "free_operator"), ["∋"]);
        if let Some(comment) = after.find("--") {
            let end = after[comment..]
                .find('\n')
                .map_or(after.len(), |offset| comment + offset + 1);
            assert_eq!(
                selected(&incremental, &after, "comment"),
                [&after[comment..end]]
            );
        }
    }
}

#[test]
fn legacy_shift_and_arrow_operators_keep_complete_tokens_before_comments() {
    for spelling in ["|<<", "|>>", "#abc->", "#abc<<", "&>>"] {
        let source = format!(
            "class BOX feature run (left, right: INTEGER): INTEGER do Result := left {spelling}--ignored\n right end tail: INTEGER end"
        );
        let tree = valid(&source);
        assert_eq!(selected(&tree, &source, "free_operator"), [spelling]);
        assert_eq!(selected(&tree, &source, "comment"), ["--ignored\n"]);
        assert_eq!(selected(&tree, &source, "new_feature"), ["run", "tail"]);
    }
}
