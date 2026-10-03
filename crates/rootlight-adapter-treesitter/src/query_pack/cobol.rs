//! Source-bound COBOL program, storage and procedural-header captures.
//! Hidden word tokens are selected through native ownership; data levels and
//! use-site spellings do not establish storage hierarchy or resolved targets.

use std::ops::Range;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for candidate in node.named_children(&mut cursor) {
        cancellation.check()?;
        if candidate.kind() == kind {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn first_non_extra_named_child<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for candidate in node.named_children(&mut cursor) {
        cancellation.check()?;
        if !candidate.is_extra() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn file_descriptor_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if node.kind() != "file_description_entry" || node.is_missing() || node.has_error() {
        return Ok(None);
    }
    let Some(owner) = node
        .parent()
        .filter(|owner| matches!(owner.kind(), "file_description" | "file_section"))
    else {
        return Ok(None);
    };
    let mut preceding = node.prev_named_sibling();
    while let Some(extra) = preceding.filter(|candidate| candidate.is_extra()) {
        cancellation.check()?;
        preceding = extra.prev_named_sibling();
    }
    let Some(file_type) = preceding.filter(|candidate| {
        candidate.kind() == "file_type"
            && !candidate.is_missing()
            && !candidate.has_error()
            && candidate.parent() == Some(owner)
    }) else {
        return Ok(None);
    };
    let Some(keyword) = first_non_extra_named_child(file_type, cancellation)? else {
        return Ok(None);
    };
    if !matches!(keyword.kind(), "FD" | "SD") || keyword.is_missing() || keyword.has_error() {
        return Ok(None);
    }
    // Both native layouts put the descriptor WORD first in this header. A WORD
    // inside a clause or record is not the descriptor's authored name.
    Ok(
        first_non_extra_named_child(node, cancellation)?.filter(|name| {
            name.kind() == "WORD"
                && !name.is_missing()
                && !name.has_error()
                && node.start_byte() <= name.start_byte()
                && name.start_byte() < name.end_byte()
                && name.end_byte() <= node.end_byte()
        }),
    )
}

fn definition_node<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    match node.kind() {
        "program_definition" => {
            let Some(header) = child(node, "identification_division", cancellation)? else {
                return Ok(None);
            };
            child(header, "program_name", cancellation)
        }
        "data_description" => {
            if let Some(name) = child(node, "entry_name", cancellation)? {
                return Ok(Some(name));
            }
            Ok(child(node, "constant_entry", cancellation)?
                .and_then(|entry| entry.child_by_field_name("name")))
        }
        "file_description_entry" => file_descriptor_name(node, cancellation),
        _ => Ok(None),
    }
}

fn header_name_range(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    cancellation.check()?;
    if !matches!(node.kind(), "section_header" | "paragraph_header")
        || node.is_missing()
        || node.has_error()
    {
        return Ok(None);
    }
    let Some(text) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    if let Some(name) = node.child_by_field_name("name") {
        return Ok((!name.is_missing()
            && name.start_byte() == node.start_byte()
            && name.start_byte() < name.end_byte()
            && name.end_byte() <= node.end_byte())
        .then_some(name.byte_range()));
    }
    // The hidden _WORD field is absent from some native trees. Both header
    // productions begin with that authored token, never a following statement.
    if !text.first().is_some_and(u8::is_ascii_alphanumeric) {
        return Ok(None);
    }
    let mut length = 0_usize;
    let mut letter = false;
    let mut hyphen = false;
    for byte in text {
        cancellation.check()?;
        if !byte.is_ascii_alphanumeric() && *byte != b'-' {
            break;
        }
        letter |= byte.is_ascii_alphabetic();
        hyphen |= *byte == b'-';
        length += 1;
    }
    if (!letter && hyphen)
        || !(text.get(length).is_some_and(|byte| {
            byte.is_ascii_whitespace() || matches!(*byte, b'.' | b',' | b';' | b'\x0b')
        }) || text
            .get(length..)
            .is_some_and(|tail| tail.starts_with(b"*>")))
    {
        return Ok(None);
    }
    Ok(node
        .start_byte()
        .checked_add(length)
        .map(|end| node.start_byte()..end))
}

fn definition_range(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    if matches!(node.kind(), "section_header" | "paragraph_header") {
        return header_name_range(node, source, cancellation);
    }
    if node.kind() == "file_description_entry" && source.get(node.byte_range()).is_none() {
        return Ok(None);
    }
    Ok(definition_node(node, cancellation)?
        .filter(|name| !name.is_missing() && name.start_byte() < name.end_byte())
        .map(|name| name.byte_range()))
}

fn using_range(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    let mut cursor = node.walk();
    let mut name = None;
    for candidate in node.children(&mut cursor) {
        cancellation.check()?;
        if matches!(candidate.kind(), "_WORD" | "WORD") {
            name = Some(candidate.byte_range());
        }
    }
    if name.is_some() {
        return Ok(name);
    }
    // The grammar ends procedure_param with a hidden ASCII word token. Some
    // trees omit that child; only its contiguous authored suffix is projected.
    let end = node.end_byte();
    if source.get(node.byte_range()).is_none() {
        return Ok(None);
    }
    let mut start = end;
    let mut letter = false;
    while start > node.start_byte() {
        cancellation.check()?;
        let Some(previous) = start.checked_sub(1) else {
            return Ok(None);
        };
        let Some(byte) = source.get(previous).copied() else {
            return Ok(None);
        };
        if !byte.is_ascii_alphanumeric() && byte != b'-' {
            break;
        }
        letter |= byte.is_ascii_alphabetic();
        start = previous;
    }
    Ok((letter && start < end).then_some(start..end))
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "file_description_entry" {
        return Ok(definition_range(node, source, cancellation)?.is_some());
    }
    if role == StructuralRole::Reference && node.kind() == "WORD" {
        return Ok(!node.is_missing()
            && !node.has_error()
            && node.start_byte() < node.end_byte()
            && source.get(node.byte_range()).is_some()
            && node.parent().is_some_and(|bound| {
                bound.kind() == "occurs_bound"
                    && !bound.is_missing()
                    && !bound.has_error()
                    && bound.start_byte() <= node.start_byte()
                    && node.end_byte() <= bound.end_byte()
            }));
    }
    if role == StructuralRole::Reference
        && node.kind() == "qualified_word"
        && node.parent().is_some_and(|label| {
            label.kind() == "label"
                && label
                    .parent()
                    .is_some_and(|owner| owner.kind() == "perform_procedure")
        })
    {
        // PERFORM owns a procedural label, not a storage read. CALL identifier
        // operands still retain both the storage read and dispatch-site roles.
        return Ok(false);
    }
    if node.kind() == "data_description"
        && let Some(name) = definition_node(node, cancellation)?
        && source
            .get(name.byte_range())
            .is_some_and(|text| text.eq_ignore_ascii_case(b"FILLER"))
    {
        return Ok(false);
    }
    if role == StructuralRole::Definition {
        return Ok(definition_range(node, source, cancellation)?.is_some());
    }
    if role == StructuralRole::Reference && node.kind() == "procedure_param" {
        return Ok(using_range(node, source, cancellation)?.is_some());
    }
    if role == StructuralRole::Reference && node.kind() == "call_statement" {
        return Ok(node.child_by_field_name("x").is_some_and(|target| {
            !target.is_missing() && target.start_byte() < target.end_byte()
        }));
    }
    Ok(true)
}

pub(super) fn syntax(node: Node<'_>, role: StructuralRole) -> Option<&'static str> {
    if role == StructuralRole::Definition {
        return Some(match node.kind() {
            "program_definition" => "cobol.program_name",
            "data_description" => "cobol.data_name",
            "file_description_entry" => "cobol.file_descriptor_name",
            "section_header" => "cobol.section_name",
            "paragraph_header" => "cobol.paragraph_name",
            _ => return None,
        });
    }
    Some(match node.kind() {
        "start" => "cobol.file",
        "program_definition" => "cobol.program",
        "data_description" => "cobol.data_item",
        "file_description_entry" => "cobol.file_descriptor",
        "section_header" => "cobol.section",
        "paragraph_header" => "cobol.paragraph",
        "copy_statement" => "cobol.copy",
        "qualified_word" => "cobol.storage",
        "WORD"
            if role == StructuralRole::Reference
                && node
                    .parent()
                    .is_some_and(|bound| bound.kind() == "occurs_bound") =>
        {
            "cobol.occurs_bound"
        }
        "procedure_param" => "cobol.using_name",
        "call_statement" => "cobol.call_target",
        "label" => "cobol.perform_target",
        "program_name" => "cobol.end_program_name",
        "comment" => "cobol.comment",
        "string" | "x_string" | "n_string" | "h_string" => "cobol.string",
        _ => return None,
    })
}

pub(super) fn capture_range(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    cancellation.check()?;
    if role == StructuralRole::Reference && node.kind() == "procedure_param" {
        return using_range(node, source, cancellation);
    }
    if role == StructuralRole::Definition {
        return definition_range(node, source, cancellation);
    }
    let selected = match role {
        StructuralRole::Signature if node.kind() == "program_definition" => {
            child(node, "identification_division", cancellation)?
        }
        StructuralRole::Reference if node.kind() == "call_statement" => {
            node.child_by_field_name("x")
        }
        _ => Some(node),
    };
    Ok(selected.map(|selected| selected.byte_range()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &str, free: bool) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = if free {
            tree_sitter_cobol::FREE
        } else {
            tree_sitter_cobol::FIXED
        };
        parser.set_language(&language.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        tree
    }

    fn headers(root: Node<'_>) -> Vec<Node<'_>> {
        let mut output = Vec::new();
        let mut cursor = root.walk();
        loop {
            if matches!(cursor.node().kind(), "section_header" | "paragraph_header") {
                output.push(cursor.node());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return output;
                }
            }
        }
    }

    fn source(free: bool, ending: &str) -> String {
        [
            "identification division.",
            "program-id. Headers.",
            "procedure division.",
            "Work-Section section.",
            "First-Step.",
            "display 'first'.",
            "100.",
            "stop run.",
        ]
        .iter()
        .map(|line| format!("{}{line}{ending}", if free { "" } else { "       " }))
        .collect()
    }

    fn descriptor_entries(root: Node<'_>) -> Vec<Node<'_>> {
        let cancellation = Cancellation::new();
        let program = child(root, "program_definition", &cancellation)
            .unwrap()
            .unwrap();
        let data = child(program, "data_division", &cancellation)
            .unwrap()
            .unwrap();
        let section = child(data, "file_section", &cancellation).unwrap().unwrap();
        let mut cursor = section.walk();
        section
            .named_children(&mut cursor)
            .filter_map(|node| match node.kind() {
                "file_description_entry" => Some(node),
                "file_description" => child(node, "file_description_entry", &cancellation).unwrap(),
                _ => None,
            })
            .collect()
    }

    fn descriptor_source(free: bool, ending: &str, section_header: bool) -> String {
        let prefix = if free { "" } else { "       " };
        [
            "identification division.",
            "program-id. Descriptors.",
            "data division.",
            if section_header { "file section." } else { "" },
            "fd *> descriptor note",
            "Input-File recording mode F.",
            "01 Input-Record pic x.",
            "sd Sorted-File.",
            "01 Sort-Record pic x.",
            "procedure division.",
            "goback.",
        ]
        .iter()
        .filter(|line| !line.is_empty())
        .map(|line| format!("{prefix}{line}{ending}"))
        .collect()
    }

    #[test]
    fn occurs_bound_words_require_native_ownership_source_and_cancellation() {
        let source = concat!(
            "identification division.\n",
            "program-id. Bounds.\n",
            "data division.\n",
            "working-storage section.\n",
            "01 Items occurs Lower-Limit to 9 times pic x.\n",
            "procedure division.\n",
            "move Lower-Limit to Items(1).\n",
        );
        let tree = parse(source, true);
        let cancellation = Cancellation::new();
        let program = child(tree.root_node(), "program_definition", &cancellation)
            .unwrap()
            .unwrap();
        let data = child(program, "data_division", &cancellation)
            .unwrap()
            .unwrap();
        let section = child(data, "working_storage_section", &cancellation)
            .unwrap()
            .unwrap();
        let entry = child(section, "data_description", &cancellation)
            .unwrap()
            .unwrap();
        let clause = child(entry, "occurs_clause", &cancellation)
            .unwrap()
            .unwrap();
        let bound = child(clause, "occurs_bound", &cancellation)
            .unwrap()
            .unwrap();
        let word = child(bound, "WORD", &cancellation).unwrap().unwrap();
        let start = source.find("Lower-Limit").unwrap();
        assert_eq!(word.byte_range(), start..start + "Lower-Limit".len());
        assert_eq!(
            syntax(word, StructuralRole::Reference),
            Some("cobol.occurs_bound")
        );
        assert_eq!(syntax(word, StructuralRole::Declaration), None);
        assert_eq!(syntax(word, StructuralRole::Definition), None);
        assert!(
            retain_capture(
                word,
                StructuralRole::Reference,
                source.as_bytes(),
                &cancellation
            )
            .unwrap()
        );
        assert_eq!(
            capture_range(
                word,
                StructuralRole::Reference,
                source.as_bytes(),
                &cancellation
            )
            .unwrap(),
            Some(word.byte_range())
        );
        assert!(!retain_capture(word, StructuralRole::Reference, &[], &cancellation).unwrap());
        let procedure = child(program, "procedure_division", &cancellation)
            .unwrap()
            .unwrap();
        let statement = child(procedure, "move_statement", &cancellation)
            .unwrap()
            .unwrap();
        let storage = child(statement, "qualified_word", &cancellation)
            .unwrap()
            .unwrap();
        let other_word = child(storage, "WORD", &cancellation).unwrap().unwrap();
        assert_eq!(syntax(other_word, StructuralRole::Reference), None);
        assert!(
            !retain_capture(
                other_word,
                StructuralRole::Reference,
                source.as_bytes(),
                &cancellation
            )
            .unwrap()
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            retain_capture(
                word,
                StructuralRole::Reference,
                source.as_bytes(),
                &cancellation
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            capture_range(
                word,
                StructuralRole::Reference,
                source.as_bytes(),
                &cancellation
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn file_descriptors_project_native_header_names_in_both_layouts() {
        for free in [false, true] {
            for ending in ["\n", "\r\n"] {
                for section_header in [false, true] {
                    let source = descriptor_source(free, ending, section_header);
                    let tree = parse(&source, free);
                    let entries = descriptor_entries(tree.root_node());
                    assert_eq!(entries.len(), 2);
                    for (index, (entry, (name, header, keyword))) in entries
                        .into_iter()
                        .zip([
                            ("Input-File", "Input-File recording mode F.", "fd"),
                            ("Sorted-File", "Sorted-File.", "sd"),
                        ])
                        .enumerate()
                    {
                        assert_eq!(
                            entry.parent().unwrap().kind(),
                            if index == 0 && !section_header {
                                "file_section"
                            } else {
                                "file_description"
                            }
                        );
                        let owner = entry.parent().unwrap();
                        let file_type = child(owner, "file_type", &Cancellation::new())
                            .unwrap()
                            .unwrap();
                        assert_eq!(source.get(file_type.byte_range()), Some(keyword));
                        let start = source.find(header).unwrap();
                        assert_eq!(entry.byte_range(), start..start + header.len());
                        let cancellation = Cancellation::new();
                        let native_name =
                            file_descriptor_name(entry, &cancellation).unwrap().unwrap();
                        assert_eq!(native_name.kind(), "WORD");
                        assert_eq!(native_name.parent(), Some(entry));
                        assert_eq!(native_name.byte_range(), start..start + name.len());
                        for role in [StructuralRole::Declaration, StructuralRole::Definition] {
                            assert!(
                                retain_capture(entry, role, source.as_bytes(), &cancellation)
                                    .unwrap()
                            );
                            assert_eq!(
                                capture_range(entry, role, source.as_bytes(), &cancellation)
                                    .unwrap(),
                                Some(if role == StructuralRole::Definition {
                                    native_name.byte_range()
                                } else {
                                    entry.byte_range()
                                })
                            );
                        }
                        assert_eq!(
                            syntax(entry, StructuralRole::Declaration),
                            Some("cobol.file_descriptor")
                        );
                        assert_eq!(
                            syntax(entry, StructuralRole::Definition),
                            Some("cobol.file_descriptor_name")
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn file_descriptor_projection_requires_source_ownership_and_honors_cancellation() {
        let source = descriptor_source(true, "\n", true);
        let tree = parse(&source, true);
        let entry = descriptor_entries(tree.root_node())[0];
        for role in [StructuralRole::Declaration, StructuralRole::Definition] {
            assert!(!retain_capture(entry, role, &[], &Cancellation::new()).unwrap());
            assert!(
                !retain_capture(
                    entry,
                    role,
                    &source.as_bytes()[..entry.end_byte() - 1],
                    &Cancellation::new()
                )
                .unwrap()
            );
        }
        assert_eq!(
            file_descriptor_name(tree.root_node(), &Cancellation::new()).unwrap(),
            None
        );
        let clause = child(entry, "file_description_clause", &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            file_descriptor_name(clause, &Cancellation::new()).unwrap(),
            None
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        for role in [StructuralRole::Declaration, StructuralRole::Definition] {
            assert!(matches!(
                retain_capture(entry, role, source.as_bytes(), &cancellation),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert!(matches!(
                capture_range(entry, role, source.as_bytes(), &cancellation),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
        }
    }

    #[test]
    fn hidden_header_words_project_exact_original_name_ranges() {
        for free in [false, true] {
            for ending in ["\n", "\r\n"] {
                let source = source(free, ending);
                let tree = parse(&source, free);
                let headers = headers(tree.root_node());
                assert_eq!(headers.len(), 3);
                for (header, name) in headers
                    .into_iter()
                    .zip(["Work-Section", "First-Step", "100"])
                {
                    assert_eq!(header.parent().unwrap().kind(), "procedure_division");
                    if name == "100" {
                        assert_eq!(
                            header.child_by_field_name("name").unwrap().kind(),
                            "integer"
                        );
                    } else {
                        assert!(header.child_by_field_name("name").is_none());
                    }
                    let start = source.find(name).unwrap();
                    let expected = start..start + name.len();
                    assert_eq!(header.start_byte(), expected.start);
                    assert!(
                        retain_capture(
                            header,
                            StructuralRole::Definition,
                            source.as_bytes(),
                            &Cancellation::new()
                        )
                        .unwrap()
                    );
                    assert_eq!(
                        capture_range(
                            header,
                            StructuralRole::Definition,
                            source.as_bytes(),
                            &Cancellation::new()
                        )
                        .unwrap(),
                        Some(expected.clone())
                    );
                    assert_eq!(source.get(expected).unwrap(), name);
                    assert_eq!(
                        capture_range(
                            header,
                            StructuralRole::Signature,
                            source.as_bytes(),
                            &Cancellation::new()
                        )
                        .unwrap(),
                        Some(header.byte_range())
                    );
                }
            }
        }
    }

    #[test]
    fn hidden_header_words_accept_native_separator_and_comment_extras() {
        for free in [false, true] {
            for ending in ["\n", "\r\n"] {
                let prefix = if free { "" } else { "       " };
                for (section, paragraph) in [
                    (
                        "Work-Section, section.".to_owned(),
                        "First-Step; .".to_owned(),
                    ),
                    (
                        "Work-Section\x0bsection.".to_owned(),
                        "First-Step\x0b.".to_owned(),
                    ),
                    (
                        format!("Work-Section*> header note{ending}{prefix}section."),
                        format!("First-Step*> header note{ending}{prefix}."),
                    ),
                ] {
                    let source = source(free, ending)
                        .replace("Work-Section section.", &section)
                        .replace("First-Step.", &paragraph);
                    let tree = parse(&source, free);
                    let headers = headers(tree.root_node());
                    assert_eq!(headers.len(), 3);
                    for (header, (name, written)) in headers.into_iter().take(2).zip([
                        ("Work-Section", section.as_str()),
                        ("First-Step", paragraph.as_str()),
                    ]) {
                        assert!(header.child_by_field_name("name").is_none());
                        let start = source.find(name).unwrap();
                        assert_eq!(header.byte_range(), start..start + written.len());
                        assert!(
                            retain_capture(
                                header,
                                StructuralRole::Definition,
                                source.as_bytes(),
                                &Cancellation::new()
                            )
                            .unwrap()
                        );
                        assert_eq!(
                            capture_range(
                                header,
                                StructuralRole::Definition,
                                source.as_bytes(),
                                &Cancellation::new()
                            )
                            .unwrap(),
                            Some(start..start + name.len())
                        );
                        assert_eq!(
                            capture_range(
                                header,
                                StructuralRole::Signature,
                                source.as_bytes(),
                                &Cancellation::new()
                            )
                            .unwrap(),
                            Some(header.byte_range())
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn header_projection_rejects_missing_source_bad_boundaries_and_cancellation() {
        let source = source(true, "\n");
        let tree = parse(&source, true);
        let header = headers(tree.root_node())[0];
        assert_eq!(
            header_name_range(header, &[], &Cancellation::new()).unwrap(),
            None
        );
        let mut mismatched = source.into_bytes();
        mismatched[header.start_byte() + "Work-Section".len()] = b'_';
        assert_eq!(
            header_name_range(header, &mismatched, &Cancellation::new()).unwrap(),
            None
        );
        mismatched[header.start_byte() + "Work-Section".len()] = b'*';
        assert_eq!(
            header_name_range(header, &mismatched, &Cancellation::new()).unwrap(),
            None
        );
        mismatched[header.start_byte()] = b'\'';
        assert_eq!(
            header_name_range(header, &mismatched, &Cancellation::new()).unwrap(),
            None
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            header_name_range(header, &mismatched, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(
            header_name_range(tree.root_node(), &mismatched, &Cancellation::new()).unwrap(),
            None
        );
    }
}
