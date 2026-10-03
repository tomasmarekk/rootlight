//! Authored native structures retain original declaration and header ranges.
//! Clean-tree assertions qualify syntax only, without COPY expansion or resolution.

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

fn source(lines: &[&str], free: bool, ending: &str) -> String {
    let prefix = if free { "" } else { "       " };
    lines
        .iter()
        .map(|line| format!("{prefix}{line}{ending}"))
        .collect()
}

fn parse(text: &str, free: bool) -> Tree {
    let mut parser = Parser::new();
    let language = if free {
        tree_sitter_cobol::FREE
    } else {
        tree_sitter_cobol::FIXED
    };
    parser.set_language(&language.into()).unwrap();
    parser.parse(text, None).unwrap()
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn clean(tree: &Tree, text: &str) {
    assert!(
        !tree.root_node().has_error(),
        "source {text:?}: {}",
        tree.root_node().to_sexp()
    );
    assert!(nodes(tree.root_node(), "ERROR").is_empty());
    let mut cursor = tree.walk();
    loop {
        assert!(!cursor.node().is_missing(), "missing node in {text:?}");
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn written_range(text: &str, written: &str) -> Range<usize> {
    let start = text.find(written).unwrap();
    start..start.checked_add(written.len()).unwrap()
}

fn exact(node: Node<'_>, text: &str, written: &str) {
    assert_eq!(node.byte_range(), written_range(text, written));
    assert_eq!(text.get(node.byte_range()).unwrap(), written);
}

#[test]
fn separate_program_units_keep_header_and_closing_names_in_their_own_ranges() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            let text = source(
                &[
                    "identification division.",
                    "program-id. Alpha.",
                    "procedure division.",
                    "stop run.",
                    "end program Alpha.",
                    "identification division.",
                    "program-id. Beta.",
                    "procedure division.",
                    "goback.",
                    "end program Beta.",
                ],
                free,
                ending,
            );
            let tree = parse(&text, free);
            clean(&tree, &text);
            let programs = nodes(tree.root_node(), "program_definition");
            assert_eq!(programs.len(), 2);
            assert!(programs[0].end_byte() < programs[1].start_byte());
            assert_eq!(programs[0].parent().unwrap().id(), tree.root_node().id());
            assert_eq!(programs[1].parent().unwrap().id(), tree.root_node().id());
            for (program, name) in programs.into_iter().zip(["Alpha", "Beta"]) {
                let headers = nodes(program, "identification_division");
                let endings = nodes(program, "end_program");
                assert_eq!(headers.len(), 1);
                assert_eq!(endings.len(), 1);
                assert_eq!(nodes(program, "program_definition").len(), 1);
                let header_names = nodes(headers[0], "program_name");
                let closing_names = nodes(endings[0], "program_name");
                assert_eq!(header_names.len(), 1);
                assert_eq!(closing_names.len(), 1);
                let occurrences: Vec<_> = text
                    .match_indices(name)
                    .map(|(start, _)| start..start + name.len())
                    .collect();
                assert_eq!(occurrences.len(), 2);
                assert_eq!(header_names[0].byte_range(), occurrences[0]);
                assert_eq!(closing_names[0].byte_range(), occurrences[1]);
                assert_eq!(text.get(header_names[0].byte_range()).unwrap(), name);
                assert_eq!(text.get(closing_names[0].byte_range()).unwrap(), name);
                assert_eq!(program.start_byte(), headers[0].start_byte());
                assert_eq!(program.end_byte(), endings[0].end_byte());
                assert_eq!(nodes(program, "procedure_division").len(), 1);
            }
        }
    }
}

#[test]
fn data_entries_preserve_names_filler_and_flat_written_levels() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            let text = source(
                &[
                    "identification division.",
                    "program-id. Storage.",
                    "data division.",
                    "working-storage section.",
                    "01 Group-Record.",
                    "05 Item pic x(4).",
                    "05 FILLER pic x.",
                    "77 Count pic 9.",
                    "procedure division.",
                    "stop run.",
                ],
                free,
                ending,
            );
            let tree = parse(&text, free);
            clean(&tree, &text);
            let sections = nodes(tree.root_node(), "working_storage_section");
            assert_eq!(sections.len(), 1);
            let entries = nodes(tree.root_node(), "data_description");
            assert_eq!(entries.len(), 4);
            let expected = [
                ("01 Group-Record", "01", "Group-Record"),
                ("05 Item pic x(4)", "05", "Item"),
                ("05 FILLER pic x", "05", "FILLER"),
                ("77 Count pic 9", "77", "Count"),
            ];
            for (entry, (declaration, level, name)) in entries.into_iter().zip(expected) {
                exact(entry, &text, declaration);
                assert_eq!(entry.parent().unwrap().id(), sections[0].id());
                assert_eq!(nodes(entry, "data_description").len(), 1);
                let names = nodes(entry, "entry_name");
                let levels = nodes(entry, "level_number");
                assert_eq!(names.len(), 1);
                assert_eq!(levels.len(), 1);
                exact(names[0], &text, name);
                assert_eq!(text.get(levels[0].byte_range()).unwrap(), level);
                assert_eq!(levels[0].start_byte(), entry.start_byte());
                assert_eq!(levels[0].end_byte(), entry.start_byte() + level.len());
                assert!(names[0].start_byte() > levels[0].end_byte());
            }
            assert_eq!(nodes(tree.root_node(), "constant_entry").len(), 0);
        }
    }
}

#[test]
fn section_and_paragraph_nodes_cover_headers_not_following_statements() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            let text = source(
                &[
                    "identification division.",
                    "program-id. Headers.",
                    "procedure division.",
                    "Work-Section section.",
                    "First-Step.",
                    "display 'first'.",
                    "Next-Step.",
                    "display 'second'.",
                    "100.",
                    "stop run.",
                ],
                free,
                ending,
            );
            let tree = parse(&text, free);
            clean(&tree, &text);
            let procedures = nodes(tree.root_node(), "procedure_division");
            let sections = nodes(tree.root_node(), "section_header");
            let paragraphs = nodes(tree.root_node(), "paragraph_header");
            assert_eq!(procedures.len(), 1);
            assert_eq!(sections.len(), 1);
            assert_eq!(paragraphs.len(), 3);
            let headers = [sections[0], paragraphs[0], paragraphs[1], paragraphs[2]];
            for (header, (written, name)) in headers.into_iter().zip([
                ("Work-Section section.", "Work-Section"),
                ("First-Step.", "First-Step"),
                ("Next-Step.", "Next-Step"),
                ("100.", "100"),
            ]) {
                exact(header, &text, written);
                assert_eq!(header.parent().unwrap().id(), procedures[0].id());
                assert_eq!(header.start_byte(), written_range(&text, name).start);
                let name_range = header.start_byte()..header.start_byte() + name.len();
                assert_eq!(name_range, written_range(&text, name));
                assert_eq!(text.get(name_range.clone()).unwrap(), name);
                // Hidden word tokens need not expose a named field child.
                if let Some(field) = header.child_by_field_name("name") {
                    assert_eq!(field.byte_range(), name_range);
                }
                assert!(nodes(header, "display_statement").is_empty());
                assert!(nodes(header, "stop_statement").is_empty());
            }
            let numeric_name = paragraphs[2].child_by_field_name("name").unwrap();
            assert_eq!(numeric_name.kind(), "integer");
            exact(numeric_name, &text, "100");
            let displays = nodes(tree.root_node(), "display_statement");
            assert_eq!(displays.len(), 2);
            assert_eq!(displays[0].parent().unwrap().id(), procedures[0].id());
            assert_eq!(displays[1].parent().unwrap().id(), procedures[0].id());
            assert!(paragraphs[0].end_byte() < displays[0].start_byte());
            assert!(displays[0].end_byte() < paragraphs[1].start_byte());
        }
    }
}

#[test]
fn copy_directives_remain_unexpanded_and_bare_fragments_remain_recovery() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            let directive = "COPY 'Shared.cpy'.";
            let text = source(
                &[
                    "identification division.",
                    "program-id. Copies.",
                    "data division.",
                    "working-storage section.",
                    directive,
                    "01 Local-Item pic x.",
                    "procedure division.",
                    "stop run.",
                ],
                free,
                ending,
            );
            let tree = parse(&text, free);
            clean(&tree, &text);
            let copies = nodes(tree.root_node(), "copy_statement");
            assert_eq!(copies.len(), 1);
            exact(copies[0], &text, directive);
            let book = copies[0].child_by_field_name("book").unwrap();
            assert_eq!(book.kind(), "string");
            exact(book, &text, "'Shared.cpy'");
            assert!(nodes(copies[0], "replacing_clause").is_empty());
            let entries = nodes(tree.root_node(), "data_description");
            assert_eq!(entries.len(), 1);
            exact(entries[0], &text, "01 Local-Item pic x");
            let names = nodes(tree.root_node(), "entry_name");
            assert_eq!(names.len(), 1);
            exact(names[0], &text, "Local-Item");

            let copy_only = source(&["COPY 'Shared.cpy'."], free, ending);
            let copy_tree = parse(&copy_only, free);
            clean(&copy_tree, &copy_only);
            assert!(nodes(copy_tree.root_node(), "program_definition").is_empty());
            assert!(nodes(copy_tree.root_node(), "data_description").is_empty());
            let directives = nodes(copy_tree.root_node(), "copy_statement");
            assert_eq!(directives.len(), 1);
            exact(directives[0], &copy_only, "COPY 'Shared.cpy'.");

            for lines in [
                &["01 Fragment-Item pic x."][..],
                &["procedure division.", "stop run."][..],
            ] {
                let fragment = source(lines, free, ending);
                let recovered = parse(&fragment, free);
                assert!(
                    recovered.root_node().has_error(),
                    "fragment unexpectedly clean: {}",
                    recovered.root_node().to_sexp()
                );
                assert!(!nodes(recovered.root_node(), "ERROR").is_empty());
            }
            // The pinned COPY production lacks the REPLACING keyword itself.
            let replacing = text.replace(
                directive,
                "COPY 'Shared.cpy' REPLACING old-name BY new-name.",
            );
            let recovered = parse(&replacing, free);
            assert!(
                recovered.root_node().has_error(),
                "COPY REPLACING unexpectedly clean: {}",
                recovered.root_node().to_sexp()
            );
            assert!(!nodes(recovered.root_node(), "ERROR").is_empty());
        }
    }
}

#[test]
fn using_parameters_keep_authored_header_ranges_and_linkage_entries_separate() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            let text = source(
                &[
                    "identification division.",
                    "program-id. Arguments.",
                    "data division.",
                    "linkage section.",
                    "01 First-Arg pic x.",
                    "01 Second-Arg pic 9.",
                    "procedure division using by reference First-Arg",
                    "by value Second-Arg.",
                    "goback.",
                ],
                free,
                ending,
            );
            let tree = parse(&text, free);
            clean(&tree, &text);
            let using = nodes(tree.root_node(), "procedure_using_chaining");
            let parameters = nodes(tree.root_node(), "procedure_param");
            let entries = nodes(tree.root_node(), "data_description");
            assert_eq!(using.len(), 1);
            assert_eq!(parameters.len(), 2);
            assert_eq!(entries.len(), 2);
            for (parameter, written) in parameters
                .into_iter()
                .zip(["by reference First-Arg", "by value Second-Arg"])
            {
                exact(parameter, &text, written);
                assert_eq!(parameter.parent().unwrap().id(), using[0].id());
                assert!(nodes(parameter, "data_description").is_empty());
                assert!(nodes(parameter, "entry_name").is_empty());
            }
            for (entry, written) in entries
                .into_iter()
                .zip(["01 First-Arg pic x", "01 Second-Arg pic 9"])
            {
                exact(entry, &text, written);
                assert_eq!(entry.parent().unwrap().kind(), "record_description_list");
                assert!(entry.end_byte() < using[0].start_byte());
            }
            assert_eq!(nodes(tree.root_node(), "entry_name").len(), 2);
            assert_eq!(nodes(tree.root_node(), "call_statement").len(), 0);
            assert_eq!(nodes(tree.root_node(), "goback_statement").len(), 1);
        }
    }
}
