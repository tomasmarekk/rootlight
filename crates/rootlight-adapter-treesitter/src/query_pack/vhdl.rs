//! VHDL capture proofs follow declaration fields and direct identifier lists.
//! End labels, architecture targets and subtype names never declare new entities.

use std::ops::Range;

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

pub(super) struct CaptureCache {
    remaining: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) const fn new(remaining: usize) -> Self {
        Self {
            remaining,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }
}

struct Declaration {
    range: Range<usize>,
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn identifier(node: Node<'_>, source: &[u8]) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "library_constant"
            | "library_constant_boolean"
            | "library_constant_character"
            | "library_constant_debug"
            | "library_constant_env"
            | "library_constant_standard"
            | "library_constant_std_logic"
            | "library_function"
            | "library_type"
    ) && clean(node, source)
        && source
            .get(node.byte_range())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| structural_captured_name_for_language("vhdl", text, source.len()))
            .is_some()
}

fn direct<'tree>(
    node: Node<'tree>,
    kinds: &[&str],
    cancellation: &Cancellation,
    budget: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !budget.visit(cancellation)? {
            return Ok(None);
        }
        if kinds.contains(&child.kind()) {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn owner(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "entity_declaration" => ("entity", "vhdl.entity"),
        "architecture_definition" => ("architecture", "vhdl.architecture"),
        "package_declaration" => ("package", "vhdl.package"),
        "component_declaration" => ("component", "vhdl.component"),
        "type_declaration" => ("type", "vhdl.type"),
        "subtype_declaration" => ("type", "vhdl.subtype"),
        _ => return None,
    })
}

fn unsupported_owner(kind: &str) -> bool {
    matches!(
        kind,
        "package_definition"
            | "configuration_declaration"
            | "context_declaration"
            | "mode_view_declaration"
            | "process_statement"
            | "block_statement"
            | "for_generate_statement"
            | "if_generate_statement"
            | "case_generate_statement"
    )
}

fn declaration(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    budget: &mut CaptureCache,
) -> Result<Option<Declaration>, AdapterError> {
    if !clean(node, source) {
        return Ok(None);
    }
    if let Some((field, syntax)) = owner(node.kind()) {
        let Some(name) = node
            .child_by_field_name(field)
            .filter(|name| identifier(*name, source))
        else {
            return Ok(None);
        };
        return Ok(Some(Declaration {
            range: node.byte_range(),
            name: name.byte_range(),
            header: node.start_byte()..name.end_byte(),
            syntax,
        }));
    }
    if matches!(
        node.kind(),
        "subprogram_declaration" | "subprogram_definition"
    ) {
        let Some(head) = direct(
            node,
            &["function_specification", "procedure_specification"],
            cancellation,
            budget,
        )?
        else {
            return Ok(None);
        };
        let (field, syntax) = if head.kind() == "function_specification" {
            ("function", "vhdl.function")
        } else {
            ("procedure", "vhdl.procedure")
        };
        let Some(name) = head
            .child_by_field_name(field)
            .filter(|name| identifier(*name, source))
        else {
            return Ok(None);
        };
        return Ok(Some(Declaration {
            range: node.byte_range(),
            name: name.byte_range(),
            header: head.byte_range(),
            syntax,
        }));
    }
    if !identifier(node, source) {
        return Ok(None);
    }
    let Some(list) = node
        .parent()
        .filter(|parent| parent.kind() == "identifier_list")
    else {
        return Ok(None);
    };
    let Some(decl) = list.parent().filter(|parent| clean(*parent, source)) else {
        return Ok(None);
    };
    let syntax = match decl.kind() {
        "signal_declaration" => "vhdl.signal",
        "constant_declaration" => "vhdl.constant",
        "variable_declaration" => "vhdl.variable",
        "file_declaration" => "vhdl.file_object",
        "element_declaration" => "vhdl.record_field",
        "interface_declaration"
        | "interface_signal_declaration"
        | "interface_constant_declaration"
        | "interface_variable_declaration"
        | "interface_file_declaration" => {
            let mut ancestor = decl.parent();
            let mut context = None;
            while let Some(outer) = ancestor {
                if !budget.visit(cancellation)? {
                    return Ok(None);
                }
                match outer.kind() {
                    "port_clause" => {
                        context = Some("vhdl.port");
                        break;
                    }
                    "generic_clause" => {
                        context = Some("vhdl.generic");
                        break;
                    }
                    "parameter_list_specification" => {
                        context = Some("vhdl.argument");
                        break;
                    }
                    "entity_declaration"
                    | "architecture_definition"
                    | "component_declaration"
                    | "subprogram_declaration"
                    | "subprogram_definition" => break,
                    _ => ancestor = outer.parent(),
                }
            }
            let Some(context) = context else {
                return Ok(None);
            };
            context
        }
        _ => return Ok(None),
    };
    // Group members use disjoint name extents, not overlapping shared declarations.
    Ok(Some(Declaration {
        range: node.byte_range(),
        name: node.byte_range(),
        header: node.byte_range(),
        syntax,
    }))
}

fn visible(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    budget: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if !budget.visit(cancellation)? {
            return Ok(false);
        }
        if parent.is_missing()
            || parent.kind() == "ERROR"
            || unsupported_owner(parent.kind())
            || matches!(
                parent.kind(),
                "line_comment" | "block_comment" | "string_literal" | "string_literal_std_logic"
            )
        {
            return Ok(false);
        }
        if (owner(parent.kind()).is_some()
            || matches!(
                parent.kind(),
                "subprogram_declaration" | "subprogram_definition"
            ))
            && declaration(parent, source, cancellation, budget)?.is_none()
        {
            return Ok(false);
        }
        ancestor = parent.parent();
    }
    Ok(true)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    budget: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !budget.visit(cancellation)? || !visible(node, source, cancellation, budget)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    if source.get(range.clone()).is_none() {
        return Err(query_failure("query-vhdl-source-range"));
    }
    let syntax = if node.kind() == "design_file"
        && matches!(role, StructuralRole::Root | StructuralRole::Module)
    {
        range = 0..source.len();
        "vhdl.file"
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        if let Some(proof) = declaration(node, source, cancellation, budget)? {
            match role {
                StructuralRole::Declaration => {
                    range = proof.range;
                    proof.syntax
                }
                StructuralRole::Definition => {
                    range = proof.name;
                    "vhdl.name"
                }
                StructuralRole::Signature => {
                    range = proof.header;
                    "vhdl.written_header"
                }
                _ => return Ok(None),
            }
        } else if role == StructuralRole::Signature
            && (owner(node.kind()).is_some()
                || unsupported_owner(node.kind())
                || node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "identifier_list")
                || matches!(
                    node.kind(),
                    "subprogram_declaration" | "subprogram_definition"
                ))
        {
            "vhdl.unsupported"
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Comment
        && clean(node, source)
        && matches!(node.kind(), "line_comment" | "block_comment")
    {
        "vhdl.comment"
    } else if role == StructuralRole::StringLiteral
        && clean(node, source)
        && matches!(node.kind(), "string_literal" | "string_literal_std_logic")
    {
        "vhdl.string"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
        ),
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_walk_budget_and_cancellation_stay_strict() {
        let cancellation = Cancellation::new();
        let mut budget = CaptureCache::new(1);
        assert!(budget.visit(&cancellation).unwrap());
        assert!(!budget.visit(&cancellation).unwrap());
        assert!(budget.exhausted());
        let cancelled = Cancellation::new();
        assert!(cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(CaptureCache::new(1).visit(&cancelled).is_err());
    }
}
