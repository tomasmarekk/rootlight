//! Written JSX names belong to native element and attribute containers.
//! Tag names never become lexical references; expressions retain normal JS
//! captures. Paired names and containing JSX headers must agree exactly.

use tree_sitter::Node;

use super::{AdapterError, Cancellation, GrammarFamily, QueryCandidate, StructuralRole};

fn element(node: Node<'_>) -> bool {
    matches!(node.kind(), "jsx_element" | "jsx_self_closing_element")
}

fn name_node(
    mut node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    loop {
        cancel.check()?;
        if node.is_missing() || node.has_error() {
            return Ok(false);
        }
        let Some(raw) = source
            .get(node.byte_range())
            .and_then(|raw| std::str::from_utf8(raw).ok())
        else {
            return Ok(false);
        };
        if raw.is_empty() {
            return Ok(false);
        }
        for (index, character) in raw.chars().enumerate() {
            if index % 256 == 0 {
                cancel.check()?;
            }
            if character == '\\' || character.is_control() || character.is_whitespace() {
                return Ok(false);
            }
        }
        return Ok(match node.kind() {
            "identifier" | "property_identifier" => true,
            "jsx_namespace_name" => {
                node.named_child_count() == 2
                    && node
                        .named_child(0)
                        .is_some_and(|child| child.kind() == "identifier")
                    && node
                        .named_child(1)
                        .is_some_and(|child| child.kind() == "identifier")
            }
            "member_expression" => {
                if node.child_by_field_name("optional_chain").is_some() {
                    false
                } else if let (Some(object), Some(property)) = (
                    node.child_by_field_name("object"),
                    node.child_by_field_name("property"),
                ) {
                    if matches!(object.kind(), "identifier" | "member_expression")
                        && property.kind() == "property_identifier"
                    {
                        node = object;
                        continue;
                    }
                    false
                } else {
                    false
                }
            }
            _ => false,
        });
    }
}

fn header(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "jsx_element" {
        node.child_by_field_name("open_tag")
    } else if node.kind() == "jsx_self_closing_element" {
        Some(node)
    } else {
        None
    }
}

fn consistent(node: Node<'_>, source: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if node.is_missing() || node.has_error() {
        return Ok(false);
    }
    let Some(open) = header(node) else {
        return Ok(false);
    };
    let name = open.child_by_field_name("name");
    if node.kind() == "jsx_self_closing_element" {
        return match name {
            Some(name) => name_node(name, source, cancel),
            None => Ok(false),
        };
    }
    let Some(close) = node.child_by_field_name("close_tag") else {
        return Ok(false);
    };
    match (name, close.child_by_field_name("name")) {
        (None, None) => Ok(true), // A fragment is transparent, never a named entity.
        (Some(left), Some(right)) => Ok(name_node(left, source, cancel)?
            && name_node(right, source, cancel)?
            && source.get(left.byte_range()) == source.get(right.byte_range())
            && source.get(left.byte_range()).is_some()),
        _ => Ok(false),
    }
}

fn ancestors(
    mut node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    loop {
        cancel.check()?;
        // Only actual recovery ancestors fence this node. A root containing an
        // unrelated error must not suppress a clean JSX sibling.
        if node.is_error() || node.is_missing() {
            return Ok(false);
        }
        if element(node) && !consistent(node, source, cancel)? {
            return Ok(false);
        }
        let Some(parent) = node.parent() else {
            return Ok(true);
        };
        node = parent;
    }
}

fn name_position(mut node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        cancel.check()?;
        match parent.kind() {
            "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element" => {
                return Ok(parent.child_by_field_name("name") == Some(node));
            }
            "jsx_attribute" => return Ok(parent.named_child(0) == Some(node)),
            "member_expression" | "jsx_namespace_name" => node = parent,
            _ => return Ok(false),
        }
    }
    Ok(false)
}

fn definition_owner(node: Node<'_>) -> Option<Node<'_>> {
    let parent = node.parent()?;
    match parent.kind() {
        "jsx_opening_element" if parent.child_by_field_name("name") == Some(node) => {
            parent.parent()
        }
        "jsx_self_closing_element" if parent.child_by_field_name("name") == Some(node) => {
            Some(parent)
        }
        "jsx_attribute" if parent.named_child(0) == Some(node) => Some(parent),
        _ => None,
    }
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    complete_source: bool,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if role == StructuralRole::Reference && name_position(node, cancel)? {
        return Ok(false);
    }
    // Valid attributes need no extra context record; rejected attributes keep
    // their exact native span so identity domains cannot silently be complete.
    if role == StructuralRole::Expression && node.kind() == "jsx_attribute" {
        return Ok(!identity_available(node, source, complete_source, cancel)?);
    }
    let owner = match role {
        StructuralRole::Declaration if element(node) || node.kind() == "jsx_attribute" => {
            Some(node)
        }
        StructuralRole::Definition => definition_owner(node),
        _ => None,
    };
    let Some(owner) = owner else { return Ok(true) };
    identity_available(owner, source, complete_source, cancel)
}

// Context evidence and declarations use the same native eligibility boundary.
fn identity_available(
    owner: Node<'_>,
    source: &[u8],
    complete_source: bool,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    cancel.check()?;
    if !complete_source {
        return Ok(false);
    }
    if owner.kind() == "jsx_attribute" {
        let Some(tag) = owner.parent() else {
            return Ok(false);
        };
        if !matches!(
            tag.kind(),
            "jsx_opening_element" | "jsx_self_closing_element"
        ) || (tag.kind() == "jsx_opening_element"
            && !tag.parent().is_some_and(|element| {
                element.kind() == "jsx_element"
                    && element.child_by_field_name("open_tag") == Some(tag)
            }))
        {
            return Ok(false);
        }
        let Some(name) = owner.named_child(0) else {
            return Ok(false);
        };
        if !matches!(name.kind(), "property_identifier" | "jsx_namespace_name")
            || !name_node(name, source, cancel)?
        {
            return Ok(false);
        }
    } else if header(owner)
        .and_then(|open| open.child_by_field_name("name"))
        .is_none()
    {
        return Ok(false);
    }
    ancestors(owner, source, cancel)
}

pub(super) fn candidate(
    family: GrammarFamily,
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    complete_source: bool,
    cancel: &Cancellation,
) -> Result<Option<QueryCandidate>, AdapterError> {
    let kind = match role {
        StructuralRole::Declaration if element(node) => "element",
        StructuralRole::Declaration if node.kind() == "jsx_attribute" => "attribute",
        StructuralRole::Definition if definition_owner(node).is_some() => "name",
        StructuralRole::Expression if element(node) || node.kind() == "jsx_attribute" => {
            let fragment = node.kind() == "jsx_element"
                && !node.is_missing()
                && !node.has_error()
                && header(node).is_some_and(|open| open.child_by_field_name("name").is_none())
                && node
                    .child_by_field_name("close_tag")
                    .is_some_and(|close| close.child_by_field_name("name").is_none());
            if fragment || identity_available(node, source, complete_source, cancel)? {
                "context"
            } else {
                "identity_unavailable"
            }
        }
        _ => return Ok(None),
    };
    let syntax = match (family, kind) {
        (GrammarFamily::TypeScript, "element") => "typescript.jsx_element",
        (GrammarFamily::TypeScript, "attribute") => "typescript.jsx_attribute",
        (GrammarFamily::TypeScript, "name") => "typescript.jsx_name",
        (GrammarFamily::TypeScript, "identity_unavailable") => {
            "typescript.jsx_identity_unavailable"
        }
        (GrammarFamily::TypeScript, _) => "typescript.jsx_context",
        (GrammarFamily::JavaScript, "element") => "javascript.jsx_element",
        (GrammarFamily::JavaScript, "attribute") => "javascript.jsx_attribute",
        (GrammarFamily::JavaScript, "name") => "javascript.jsx_name",
        (GrammarFamily::JavaScript, "identity_unavailable") => {
            "javascript.jsx_identity_unavailable"
        }
        _ => "javascript.jsx_context",
    };
    Ok(Some(QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: false,
        native_depth: super::native_depth(node, cancel)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_jsx_composition_retains_context_without_changing_plain_typescript() {
        use super::super::{QueryPack, language_for};
        use tree_sitter::StreamingIterator;

        for (family, native, query, source, expected) in [
            (
                GrammarFamily::JavaScript,
                GrammarFamily::JavaScript,
                concat!(
                    include_str!("../../queries/javascript.scm"),
                    "\n",
                    include_str!("../../queries/jsx.scm")
                ),
                "const value = <Card flag />;",
                2,
            ),
            (
                GrammarFamily::TypeScript,
                GrammarFamily::JavaScript,
                concat!(
                    include_str!("../../queries/typescript.scm"),
                    "\n",
                    include_str!("../../queries/jsx.scm")
                ),
                "const value = <Card flag />;",
                2,
            ),
            (
                GrammarFamily::TypeScript,
                GrammarFamily::TypeScript,
                include_str!("../../queries/typescript.scm"),
                "const value = <Result>input;",
                0,
            ),
        ] {
            let pack = QueryPack::compile_native(family, native, query).unwrap();
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&language_for(native)).unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert!(!tree.root_node().has_error());
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches =
                cursor.matches(&pack.optional_query, tree.root_node(), source.as_bytes());
            let mut contexts = 0;
            while let Some(found) = matches.next() {
                for capture in found.captures {
                    if pack.roles_by_capture[capture.index as usize] == StructuralRole::Expression {
                        assert!(capture.node.kind().starts_with("jsx_"));
                        contexts += 1;
                    }
                }
            }
            assert_eq!(contexts, expected, "{family:?}/{native:?}");
            assert_eq!(
                pack.optional_query.capture_names().contains(&"expression"),
                expected != 0
            );
        }
    }

    #[test]
    fn actual_error_ancestor_withholds_markup_but_not_a_clean_sibling() {
        let source = b"if <Recovered /> {} const safe = <Safe />;";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(tree.root_node().has_error());
        let cancellation = Cancellation::new();
        let mut cursor = tree.walk();
        let mut rejected = false;
        let mut accepted = false;
        loop {
            let node = cursor.node();
            if node.kind() == "jsx_self_closing_element"
                && let Some(name) = node.child_by_field_name("name")
            {
                let raw = &source[name.byte_range()];
                if raw == b"Recovered" {
                    let mut parent = node.parent();
                    let mut actual_error = false;
                    while let Some(ancestor) = parent {
                        actual_error |= ancestor.is_error() || ancestor.is_missing();
                        parent = ancestor.parent();
                    }
                    assert!(
                        actual_error,
                        "the negative must exercise an actual recovery ancestor"
                    );
                    assert!(!identity_available(node, source, true, &cancellation).unwrap());
                    rejected = true;
                } else if raw == b"Safe" {
                    assert!(identity_available(node, source, true, &cancellation).unwrap());
                    accepted = true;
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    assert!(rejected && accepted, "{}", tree.root_node().to_sexp());
                    return;
                }
            }
        }
    }
}
