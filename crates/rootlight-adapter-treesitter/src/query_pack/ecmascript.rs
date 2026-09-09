//! Native ECMAScript binding and reference classification.
//! Pattern ancestry distinguishes declarations from assignments; native type
//! tokens and export fields keep lexical names separate from public aliases.

use super::{AdapterError, Cancellation, GrammarFamily, StructuralRole};
use tree_sitter::Node;

#[derive(Clone, Copy)]
pub(super) enum BindingKind {
    Parameter,
    Variable,
    Import { type_only: bool },
}

pub(super) fn is_foreign_import_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "import_specifier"
            && parent.child_by_field_name("alias").is_some()
            && parent.child_by_field_name("name") == Some(node)
    })
}

pub(super) fn import_signature_syntax(
    family: GrammarFamily,
    node: Node<'_>,
) -> Option<&'static str> {
    let parent = node.parent()?;
    let role = if node.kind() == "import_specifier" {
        "specifier"
    } else if parent.kind() == "import_statement"
        && parent.child_by_field_name("source") == Some(node)
    {
        "source"
    } else if parent.kind() == "import_clause" && node.kind() == "identifier" {
        "default"
    } else if parent.kind() == "namespace_import" {
        "namespace"
    } else if parent.kind() == "import_specifier"
        && parent.child_by_field_name("name") == Some(node)
    {
        "name"
    } else {
        return None;
    };
    match (family == GrammarFamily::TypeScript, role) {
        (true, "specifier") => Some("typescript.import_specifier"),
        (false, "specifier") => Some("javascript.import_specifier"),
        (true, "source") => Some("typescript.import_source"),
        (false, "source") => Some("javascript.import_source"),
        (true, "default") => Some("typescript.import_default"),
        (false, "default") => Some("javascript.import_default"),
        (true, "namespace") => Some("typescript.import_namespace"),
        (false, "namespace") => Some("javascript.import_namespace"),
        (true, "name") => Some("typescript.import_name"),
        (false, "name") => Some("javascript.import_name"),
        _ => None,
    }
}

pub(super) fn export_signature_syntax(
    family: GrammarFamily,
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let specifier = if node.kind() == "export_specifier" {
        Some(node)
    } else {
        node.parent()
            .filter(|parent| parent.kind() == "export_specifier")
    };
    if let Some(specifier) = specifier {
        let Some(statement) = specifier.parent().and_then(|clause| clause.parent()) else {
            return Ok(None);
        };
        let role = if node != specifier {
            if specifier.child_by_field_name("name") == Some(node) {
                "name"
            } else {
                "alias"
            }
        } else if statement.child_by_field_name("source").is_some()
            || !statement
                .parent()
                .is_some_and(|parent| parent.kind() == "program")
        {
            "remote"
        } else if has_type_modifier(specifier, cancellation)?
            || has_type_modifier(statement, cancellation)?
        {
            "type"
        } else {
            "local"
        };
        return Ok(Some(match (family == GrammarFamily::TypeScript, role) {
            (true, "name") => "typescript.export_binding_name",
            (false, "name") => "javascript.export_binding_name",
            (true, "alias") => "typescript.export_binding_alias",
            (false, "alias") => "javascript.export_binding_alias",
            (true, "type") => "typescript.export_type_specifier",
            (false, "type") => "javascript.export_type_specifier",
            (true, "local") => "typescript.export_local_specifier",
            (false, "local") => "javascript.export_local_specifier",
            (true, _) => "typescript.export_remote_specifier",
            (false, _) => "javascript.export_remote_specifier",
        }));
    }
    let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "export_statement")
    else {
        return Ok(None);
    };
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        cancellation.check()?;
        if child.kind() != "default" || child.is_named() {
            continue;
        }
        let declaration = parent.child_by_field_name("declaration") == Some(node);
        if !declaration && parent.child_by_field_name("value") != Some(node) {
            return Ok(None);
        }
        return Ok(Some(
            match (family == GrammarFamily::TypeScript, declaration) {
                (true, true) => "typescript.default_export_declaration",
                (false, true) => "javascript.default_export_declaration",
                (true, false) => "typescript.default_export_value",
                (false, false) => "javascript.default_export_value",
            },
        ));
    }
    Ok(
        (parent.child_by_field_name("declaration") == Some(node)).then_some(
            if family == GrammarFamily::TypeScript {
                "typescript.export_named_declaration"
            } else {
                "javascript.export_named_declaration"
            },
        ),
    )
}

pub(super) fn export_reference_syntax(
    family: GrammarFamily,
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let Some(specifier) = node
        .parent()
        .filter(|parent| parent.kind() == "export_specifier")
    else {
        return Ok(None);
    };
    let Some(statement) = specifier
        .parent()
        .and_then(|clause| clause.parent())
        .filter(|parent| parent.kind() == "export_statement")
    else {
        return Ok(None);
    };
    let external = specifier.child_by_field_name("alias") == Some(node)
        || statement.child_by_field_name("source").is_some();
    let type_only =
        has_type_modifier(specifier, cancellation)? || has_type_modifier(statement, cancellation)?;
    Ok(Some(
        match (family == GrammarFamily::TypeScript, external, type_only) {
            (true, true, _) => "typescript.export_name",
            (false, true, _) => "javascript.export_name",
            (true, false, true) => "typescript.type_export_local",
            (false, false, true) => "javascript.type_export_local",
            (true, false, false) => "typescript.export_local",
            (false, false, false) => "javascript.export_local",
        },
    ))
}

fn has_type_modifier(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        // A local/exported identifier spelled type is not a type-only token.
        if !child.is_named() && matches!(child.kind(), "type" | "typeof") {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if role == StructuralRole::Declaration && node.kind() == "variable_declarator" {
        // Destructuring has one declaration per binding, not one unnamed
        // declaration for the entire pattern. Simple declarators keep their span.
        return Ok(node
            .child_by_field_name("name")
            .is_some_and(|name| name.kind() == "identifier"));
    }
    if !matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) || !matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) || !node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "required_parameter"
                | "optional_parameter"
                | "arrow_function"
                | "array_pattern"
                | "object_pattern"
                | "pair_pattern"
                | "assignment_pattern"
                | "object_assignment_pattern"
                | "rest_pattern"
                | "variable_declarator"
                | "for_in_statement"
                | "catch_clause"
                | "import_clause"
                | "namespace_import"
                | "import_specifier"
                | "import_require_clause"
                | "import_alias"
        )
    }) {
        return Ok(true);
    }
    Ok(binding_kind(node, cancellation)?.is_some())
}

pub(super) fn binding_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<BindingKind>, AdapterError> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "import_clause" | "namespace_import" | "import_require_clause" => {
                return import_kind(parent, cancellation);
            }
            "import_specifier" => {
                let local = parent
                    .child_by_field_name("alias")
                    .or_else(|| parent.child_by_field_name("name"));
                return if local == Some(current) {
                    import_kind(parent, cancellation)
                } else {
                    Ok(None)
                };
            }
            "import_alias" => {
                return if parent.named_child(0) == Some(current) {
                    import_kind(parent, cancellation)
                } else {
                    Ok(None)
                };
            }
            "required_parameter" | "optional_parameter" => {
                return Ok((parent.child_by_field_name("pattern") == Some(current)
                    || parent.child_by_field_name("name") == Some(current))
                .then_some(BindingKind::Parameter));
            }
            "arrow_function" => {
                return Ok((parent.child_by_field_name("parameter") == Some(current))
                    .then_some(BindingKind::Parameter));
            }
            "variable_declarator" => {
                return Ok((parent.child_by_field_name("name") == Some(current))
                    .then_some(BindingKind::Variable));
            }
            "catch_clause" => {
                return Ok((parent.child_by_field_name("parameter") == Some(current))
                    .then_some(BindingKind::Variable));
            }
            "for_in_statement" => {
                return Ok((parent.child_by_field_name("left") == Some(current)
                    && parent.child_by_field_name("kind").is_some())
                .then_some(BindingKind::Variable));
            }
            "pair_pattern" if parent.child_by_field_name("value") == Some(current) => {}
            "assignment_pattern" | "object_assignment_pattern"
                if parent.child_by_field_name("left") == Some(current) => {}
            "array_pattern" | "object_pattern" | "rest_pattern" => {}
            _ => return Ok(None),
        }
        current = parent;
    }
    Ok(None)
}

fn import_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<BindingKind>, AdapterError> {
    let mut current = Some(node);
    let mut type_only = false;
    while let Some(owner) = current {
        cancellation.check()?;
        if matches!(
            owner.kind(),
            "import_statement" | "import_specifier" | "import_alias"
        ) {
            type_only |= has_type_modifier(owner, cancellation)?;
        }
        if matches!(owner.kind(), "import_statement" | "import_alias") {
            return Ok(Some(BindingKind::Import { type_only }));
        }
        current = owner.parent();
    }
    Ok(None)
}
