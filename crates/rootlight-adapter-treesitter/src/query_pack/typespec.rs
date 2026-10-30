//! Retain written TypeSpec owners and exact names from the native CST.
//! File-scoped namespaces require a source interval beyond their semicolon node.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

fn bare_path(node: Node<'_>, source: &[u8]) -> bool {
    let Some(text) = source.get(node.byte_range()) else {
        return false;
    };
    !text.is_empty()
        && text.split(|byte| *byte == b'.').all(|part| {
            part.first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                && part
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        })
}

fn supported(node: Node<'_>, source: &[u8]) -> bool {
    if node.has_error()
        || node.is_missing()
        || node.is_error()
        || !node
            .child_by_field_name("name")
            .is_some_and(|name| bare_path(name, source))
    {
        return false;
    }
    match node.kind() {
        "namespace_statement"
        | "model_statement"
        | "interface_statement"
        | "operation_statement" => node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "source_file" | "namespace_body")),
        "interface_member" => node
            .parent()
            .is_some_and(|parent| parent.kind() == "interface_body"),
        "model_property" => node.parent().is_some_and(|parent| {
            parent.kind() == "model_body"
                && parent.parent().is_some_and(|body| {
                    body.kind() == "model_expression"
                        && body
                            .parent()
                            .is_some_and(|owner| owner.kind() == "model_statement")
                })
        }),
        _ => false,
    }
}

fn admitted(
    mut node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    loop {
        cancel.check()?;
        if matches!(
            node.kind(),
            "namespace_statement"
                | "model_statement"
                | "interface_statement"
                | "operation_statement"
                | "interface_member"
                | "model_property"
        ) && !supported(node, source)
        {
            return Ok(false);
        }
        if node.is_error() || node.is_missing() {
            return Ok(false);
        }
        let Some(parent) = node
            .parent()
            .filter(|parent| parent.kind() != "source_file")
        else {
            return Ok(true);
        };
        node = parent;
    }
}

fn has_body(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| child.kind() == "namespace_body")
}

/// Namespace proof belongs to one current native tree and complete source view.
pub(super) struct Context {
    file_namespace: Option<(usize, usize)>,
    admitted: bool,
}

impl Context {
    pub(super) fn new(
        root: Node<'_>,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            file_namespace: None,
            admitted: root.kind() == "source_file",
        };
        let mut preceding_declaration = false;
        let mut remaining = super::HARD_MAX_QUERY_CAPTURES;
        let mut cursor = root.walk();
        loop {
            cancel.check()?;
            let Some(next) = remaining.checked_sub(1) else {
                result.admitted = false;
                return Ok(result);
            };
            remaining = next;
            let node = cursor.node();
            let at_file = node.parent() == Some(root);
            if node.kind() == "namespace_statement" && !has_body(node) {
                // A file has one blockless namespace after its imports/using directives.
                // Native ancestry alone does not own the declarations after its semicolon.
                // https://typespec.io/docs/language-basics/namespaces/#file-level-namespaces
                if !at_file
                    || preceding_declaration
                    || result.file_namespace.is_some()
                    || !supported(node, source)
                    || root.has_error()
                    || root.start_byte() != 0
                    || root.end_byte() != source.len()
                {
                    result.admitted = false;
                }
                result.file_namespace = Some((node.start_byte(), root.end_byte()));
            } else if at_file
                && !matches!(
                    node.kind(),
                    "import_statement"
                        | "using_statement"
                        | "single_line_comment"
                        | "multi_line_comment"
                )
            {
                preceding_declaration = true;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(result);
                }
            }
        }
    }

    pub(super) fn candidate(
        &self,
        node: Node<'_>,
        role: StructuralRole,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<Option<QueryCandidate>, AdapterError> {
        cancel.check()?;
        let owner = if role == StructuralRole::Definition {
            node.parent()
        } else {
            Some(node)
        };
        if matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        ) {
            let Some(owner) = owner.filter(|owner| self.admitted && supported(*owner, source))
            else {
                return Ok(None);
            };
            if !admitted(owner, source, cancel)? {
                return Ok(None);
            }
        }
        if role == StructuralRole::Expression && node.kind() == "source_file" && self.admitted {
            return Ok(None);
        }
        let syntax = match role {
            StructuralRole::Root | StructuralRole::Module => "typespec.file",
            StructuralRole::Declaration => match node.kind() {
                "namespace_statement" => "typespec.namespace",
                "model_statement" => "typespec.model",
                "model_property" => "typespec.property",
                "interface_statement" => "typespec.interface",
                "operation_statement" => "typespec.operation",
                "interface_member" => "typespec.method",
                _ => return Err(query_failure("query-typespec-declaration")),
            },
            StructuralRole::Definition => "typespec.name",
            StructuralRole::Expression if node.kind() == "source_file" => {
                "typespec.namespace_context_unavailable"
            }
            StructuralRole::Expression => "typespec.context_unavailable",
            StructuralRole::Comment => "typespec.comment",
            _ => return Err(query_failure("query-typespec-role")),
        };
        Ok(Some(QueryCandidate {
            start: node.start_byte(),
            end: if role == StructuralRole::Declaration
                && self.file_namespace.is_some_and(|(start, _)| {
                    start == node.start_byte()
                        && node.kind() == "namespace_statement"
                        && !has_body(node)
                }) {
                self.file_namespace.map_or(node.end_byte(), |(_, end)| end)
            } else {
                node.end_byte()
            },
            role,
            syntax,
            required: role.belongs_to_identity_closure(),
            native_depth: native_depth(node, cancel)?,
        }))
    }
}
