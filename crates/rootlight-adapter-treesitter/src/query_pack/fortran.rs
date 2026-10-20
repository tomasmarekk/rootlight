//! Physical Fortran captures preserve direct header and declarator ownership.
//! Name admission rejects recovered template fragments. A separate complete
//! native census may certify same-module calls without claiming compiler legality.

use std::ops::Range;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn direct_child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn header<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let kind = match node.kind() {
        "module" => "module_statement",
        "submodule" => "submodule_statement",
        "program" => "program_statement",
        "function" => "function_statement",
        "subroutine" => "subroutine_statement",
        "module_procedure" => "module_procedure_statement",
        "derived_type_definition" => "derived_type_statement",
        "interface" => "interface_statement",
        _ => return Ok(None),
    };
    direct_child(node, kind, cancellation)
}

fn declarator_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    loop {
        cancellation.check()?;
        if node.is_missing() || node.has_error() {
            return Ok(None);
        }
        node = match node.kind() {
            "identifier" => return Ok(Some(node)),
            "init_declarator" | "pointer_init_declarator" | "data_declarator" => {
                let Some(left) = node.child_by_field_name("left") else {
                    return Ok(None);
                };
                left
            }
            "sized_declarator" | "coarray_declarator" => {
                if let Some(name) = direct_child(node, "identifier", cancellation)? {
                    return Ok(Some(name));
                }
                let Some(sized) = direct_child(node, "sized_declarator", cancellation)? else {
                    return Ok(None);
                };
                sized
            }
            _ => return Ok(None),
        };
    }
}

pub(super) fn definition<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if let Some(statement) = header(node, cancellation)? {
        return Ok(match node.kind() {
            "function" | "subroutine" | "module_procedure" => statement.child_by_field_name("name"),
            "derived_type_definition" => direct_child(statement, "type_name", cancellation)?,
            _ => direct_child(statement, "name", cancellation)?,
        });
    }
    declarator_name(node, cancellation)
}

pub(super) fn ordinary_name(node: Node<'_>, source: &[u8]) -> bool {
    if node.is_missing() || node.has_error() {
        return false;
    }
    let Some(bytes) = source.get(node.byte_range()) else {
        return false;
    };
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && source.get(node.end_byte()).is_none_or(|byte| {
            !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'$' | b'@' | b'#' | b'{')
        })
}

fn clean_header(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.is_missing() || node.has_error() {
        return Ok(false);
    }
    let Some(bytes) = source.get(node.byte_range()) else {
        return Ok(false);
    };
    for chunk in bytes.chunks(1024) {
        cancellation.check()?;
        if chunk.iter().any(|byte| matches!(byte, b'$' | b'@' | b'#')) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if let Some(statement) = header(owner, cancellation)? {
            if !clean_header(statement, source, cancellation)? {
                return Ok(false);
            }
            if let Some(name) = definition(owner, cancellation)? {
                if !ordinary_name(name, source) {
                    return Ok(false);
                }
            } else if owner.kind() != "interface" {
                return Ok(false);
            }
        } else if matches!(
            owner.kind(),
            "module"
                | "submodule"
                | "program"
                | "function"
                | "subroutine"
                | "module_procedure"
                | "derived_type_definition"
                | "interface"
        ) {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

fn valid_definition(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(name) = definition(node, cancellation)? else {
        return Ok(false);
    };
    if !ordinary_name(name, source) || !valid_owner_chain(node, source, cancellation)? {
        return Ok(false);
    }
    if let Some(owner) = node
        .parent()
        .filter(|owner| owner.kind() == "variable_declaration")
    {
        return clean_header(owner, source, cancellation);
    }
    Ok(true)
}

pub(super) fn template_source(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    for (index, pair) in source.windows(2).enumerate() {
        if index.is_multiple_of(1024) {
            cancellation.check()?;
        }
        if pair == b"#:"
            || pair == b"$:"
            || pair == b"@:"
            || pair == b"${"
            || pair == b"@{"
            || pair == b"#{"
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn application_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if let Some(name) = node.child_by_field_name("function") {
        return Ok((name.kind() == "identifier").then_some(name));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        // The published grammar can omit the field for a direct application.
        // Only its first native child is a head, never an identifier in arguments.
        return Ok(
            (child.kind() == "identifier" && child.start_byte() == node.start_byte())
                .then_some(child),
        );
    }
    Ok(None)
}

pub(super) fn terminal_call_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(target) = node.child_by_field_name("subroutine") else {
        return Ok(None);
    };
    match target.kind() {
        "identifier" => Ok(Some(target)),
        "call_expression" => application_name(target, cancellation),
        _ => Ok(None),
    }
}

fn declaration_syntax(node: Node<'_>) -> &'static str {
    match node.kind() {
        "translation_unit" => "fortran.file",
        "module" => "fortran.module",
        "submodule" => "fortran.submodule",
        "program" => "fortran.program",
        "function"
            if node
                .parent()
                .is_some_and(|owner| owner.kind() == "interface") =>
        {
            "fortran.interface_function"
        }
        "subroutine"
            if node
                .parent()
                .is_some_and(|owner| owner.kind() == "interface") =>
        {
            "fortran.interface_subroutine"
        }
        "function" => "fortran.function",
        "subroutine" => "fortran.subroutine",
        "module_procedure" => "fortran.separate_module_procedure",
        "derived_type_definition" => "fortran.derived_type",
        "interface" => "fortran.interface",
        _ if node
            .parent()
            .is_some_and(|owner| owner.kind() == "parameters") =>
        {
            "fortran.parameter"
        }
        _ if node
            .parent()
            .and_then(|owner| owner.parent())
            .is_some_and(|owner| owner.kind() == "derived_type_definition") =>
        {
            "fortran.field"
        }
        _ => "fortran.variable",
    }
}

fn nonlexical_identifier(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        if matches!(
            parent.kind(),
            "use_statement" | "use_alias" | "included_items" | "parameters"
        ) {
            return Ok(true);
        }
        if matches!(parent.kind(), "sized_declarator" | "coarray_declarator")
            && declarator_name(parent, cancellation)? == Some(node)
        {
            return Ok(true);
        }
        if parent.kind() == "variable_declaration" {
            return Ok(declarator_name(current, cancellation)? == Some(node));
        }
        if parent.kind() == "keyword_argument"
            && parent.child_by_field_name("name") == Some(current)
        {
            return Ok(true);
        }
        if matches!(
            parent.kind(),
            "type_member" | "derived_type" | "procedure_statement"
        ) {
            return Ok(true);
        }
        if parent.kind() == "subroutine_call"
            && terminal_call_name(parent, cancellation)? == Some(node)
        {
            return Ok(true);
        }
        if parent.kind() == "call_expression"
            && application_name(parent, cancellation)? == Some(node)
        {
            return Ok(true);
        }
        if matches!(
            parent.kind(),
            "function" | "subroutine" | "module" | "program" | "translation_unit"
        ) {
            break;
        }
        current = parent;
    }
    Ok(false)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "translation_unit" {
        return if role == StructuralRole::Expression {
            template_source(source, cancellation)
        } else {
            Ok(true)
        };
    }
    if role == StructuralRole::Expression {
        return Ok(!valid_definition(node, source, cancellation)?
            || node.kind() == "module_procedure"
            || node.kind() == "interface"
            || node
                .parent()
                .is_some_and(|owner| owner.kind() == "interface"));
    }
    if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Module
    ) {
        return valid_definition(node, source, cancellation);
    }
    if role == StructuralRole::Scope {
        return if node.kind() == "interface" {
            valid_owner_chain(node, source, cancellation)
        } else {
            valid_definition(node, source, cancellation)
        };
    }
    if role == StructuralRole::CallName {
        return Ok(terminal_call_name(node, cancellation)?
            .is_some_and(|name| ordinary_name(name, source))
            && valid_owner_chain(node, source, cancellation)?);
    }
    if matches!(
        role,
        StructuralRole::Reference | StructuralRole::Call | StructuralRole::Import
    ) && !valid_owner_chain(node, source, cancellation)?
    {
        // A recovered template owner cannot attribute descendants to a valid
        // outer entity. Its header and template facts retain all missing domains.
        return Ok(false);
    }
    if role == StructuralRole::Comment {
        return Ok(source
            .get(node.byte_range())
            .is_some_and(|bytes| bytes.first() == Some(&b'!')));
    }
    if role == StructuralRole::Reference && node.kind() == "identifier" {
        return Ok(!nonlexical_identifier(node, cancellation)?);
    }
    if role == StructuralRole::Reference
        && node.kind() == "call_expression"
        && node.parent().is_some_and(|owner| {
            owner.kind() == "subroutine_call"
                && owner.child_by_field_name("subroutine") == Some(node)
        })
    {
        return Ok(false);
    }
    Ok(!node.is_missing())
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    let syntax = match role {
        StructuralRole::Expression if node.kind() == "translation_unit" => {
            "fortran.template_source"
        }
        StructuralRole::Expression if valid_definition(node, source, cancellation)? => {
            "fortran.procedure_linkage"
        }
        StructuralRole::Expression => "fortran.unsupported_header",
        StructuralRole::Definition => "fortran.identifier",
        StructuralRole::Call => "fortran.explicit_subroutine",
        StructuralRole::CallName => "fortran.subroutine_name",
        StructuralRole::Import => "fortran.use",
        StructuralRole::Reference if node.kind() == "call_expression" => "fortran.application",
        StructuralRole::Reference => "fortran.identifier",
        StructuralRole::Comment => "fortran.comment",
        StructuralRole::StringLiteral => "fortran.string",
        _ => declaration_syntax(node),
    };
    let range: Range<usize> = match role {
        StructuralRole::Definition => definition(node, cancellation)?
            .ok_or_else(|| query_failure("query-fortran-definition"))?
            .byte_range(),
        StructuralRole::CallName => terminal_call_name(node, cancellation)?
            .ok_or_else(|| query_failure("query-fortran-call-name"))?
            .byte_range(),
        StructuralRole::Signature => header(node, cancellation)?
            .ok_or_else(|| query_failure("query-fortran-signature"))?
            .byte_range(),
        StructuralRole::Reference if node.kind() == "call_expression" => {
            application_name(node, cancellation)?
                .filter(|name| ordinary_name(*name, source))
                .map_or(node.byte_range(), |name| name.byte_range())
        }
        StructuralRole::Expression if node.kind() != "translation_unit" => {
            header(node, cancellation)?.map_or(node.byte_range(), |header| header.byte_range())
        }
        _ => node.byte_range(),
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "translation_unit" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-fortran-depth"))?;
    }
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    })
}
