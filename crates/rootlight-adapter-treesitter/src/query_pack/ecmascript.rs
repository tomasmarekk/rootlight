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

pub(super) fn binding_signature_syntax(
    family: GrammarFamily,
    node: Node<'_>,
) -> Option<&'static str> {
    if family == GrammarFamily::TypeScript
        && let Some(parent) = node.parent()
        && parent.kind() == "conditional_type"
    {
        if parent.child_by_field_name("right") == Some(node) {
            return Some("typescript.conditional_right");
        }
        if parent.child_by_field_name("consequence") == Some(node) {
            return Some("typescript.conditional_consequence");
        }
    }
    let hoisted = (node.kind() == "variable_declarator"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "variable_declaration"))
        || (node.kind() == "for_in_statement"
            && node
                .child_by_field_name("kind")
                .is_some_and(|kind| kind.kind() == "var"));
    hoisted.then_some(if family == GrammarFamily::TypeScript {
        "typescript.hoisted_binding"
    } else {
        "javascript.hoisted_binding"
    })
}

pub(super) fn is_foreign_import_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "import_specifier"
            && parent.child_by_field_name("alias").is_some()
            && parent.child_by_field_name("name") == Some(node)
    })
}

pub(super) fn qualified_reference_syntax(
    family: GrammarFamily,
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let typescript = family == GrammarFamily::TypeScript;
    if matches!(
        node.kind(),
        "member_expression" | "nested_identifier" | "nested_type_identifier"
    ) {
        return Ok(Some(if typescript {
            "typescript.member_path"
        } else {
            "javascript.member_path"
        }));
    }
    let member = node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "member_expression" | "nested_identifier")
            && parent.child_by_field_name("property") == Some(node)
            || parent.kind() == "nested_type_identifier"
                && parent.child_by_field_name("name") == Some(node)
    });
    let mut current = node;
    let mut type_namespace = false;
    let mut type_name = false;
    let mut type_query = false;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "member_expression" | "nested_identifier" => {
                if parent.child_by_field_name("object") != Some(current)
                    && parent.child_by_field_name("property") != Some(current)
                {
                    break;
                }
            }
            "nested_type_identifier" => {
                type_namespace = parent.child_by_field_name("module") == Some(current);
                type_name = parent.child_by_field_name("name") == Some(current);
                break;
            }
            "type_query" => {
                type_query = true;
                break;
            }
            _ => break,
        }
        current = parent;
    }
    Ok(
        match (typescript, member, type_name, type_namespace, type_query) {
            (true, true, true, _, _) => Some("typescript.type_member_name"),
            (true, true, _, true, _) => Some("typescript.type_namespace_member"),
            (true, true, _, _, true) => Some("typescript.type_query_member_name"),
            (true, false, _, true, _) => Some("typescript.type_namespace_root"),
            (true, false, _, _, true) => Some("typescript.type_query_value"),
            (true, true, _, _, _) => Some("typescript.member_name"),
            (false, true, _, _, _) => Some("javascript.member_name"),
            _ => None,
        },
    )
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
    if node
        .parent()
        .is_some_and(|parent| parent.kind() == "namespace_export")
    {
        return Ok(Some(if family == GrammarFamily::TypeScript {
            "typescript.export_namespace_name"
        } else {
            "javascript.export_namespace_name"
        }));
    }
    if node.kind() == "export_statement" {
        let mut clause = false;
        let mut namespace = false;
        let mut malformed = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            cancellation.check()?;
            clause |= child.kind() == "export_clause";
            namespace |= child.kind() == "namespace_export";
            malformed |= child.has_error() || child.is_missing();
        }
        let unsupported = malformed
            || node
                .parent()
                .is_none_or(|parent| parent.kind() != "program");
        let type_only = has_type_modifier(node, cancellation)?;
        if namespace && !unsupported {
            return Ok(Some(
                match (family == GrammarFamily::TypeScript, type_only) {
                    (true, true) => "typescript.export_type_namespace_statement",
                    (true, false) => "typescript.export_namespace_statement",
                    (false, true) => "javascript.export_type_namespace_statement",
                    (false, false) => "javascript.export_namespace_statement",
                },
            ));
        }
        return Ok(Some(
            match (
                family == GrammarFamily::TypeScript,
                unsupported,
                clause,
                type_only,
            ) {
                (true, true, _, _) => "typescript.export_unsupported_statement",
                (false, true, _, _) => "javascript.export_unsupported_statement",
                (true, false, true, _) => "typescript.export_reexport_statement",
                (false, false, true, _) => "javascript.export_reexport_statement",
                (true, false, false, true) => "typescript.export_type_star_statement",
                (false, false, false, true) => "javascript.export_type_star_statement",
                (true, false, false, false) => "typescript.export_star_statement",
                (false, false, false, false) => "javascript.export_star_statement",
            },
        ));
    }
    if node.parent().is_some_and(|parent| {
        parent.kind() == "export_statement" && parent.child_by_field_name("source") == Some(node)
    }) {
        return Ok(Some(if family == GrammarFamily::TypeScript {
            "typescript.export_source"
        } else {
            "javascript.export_source"
        }));
    }
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
        } else if statement
            .parent()
            .is_none_or(|parent| parent.kind() != "program")
        {
            "unsupported"
        } else if statement.child_by_field_name("source").is_some() {
            if has_type_modifier(specifier, cancellation)?
                || has_type_modifier(statement, cancellation)?
            {
                "remote_type"
            } else {
                "remote"
            }
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
            (true, "remote") => "typescript.export_remote_specifier",
            (false, "remote") => "javascript.export_remote_specifier",
            (true, "remote_type") => "typescript.export_remote_type_specifier",
            (false, "remote_type") => "javascript.export_remote_type_specifier",
            (true, _) => "typescript.export_unsupported_specifier",
            (false, _) => "javascript.export_unsupported_specifier",
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
    if node
        .parent()
        .is_some_and(|parent| parent.kind() == "namespace_export")
    {
        return Ok(Some(if family == GrammarFamily::TypeScript {
            "typescript.export_name"
        } else {
            "javascript.export_name"
        }));
    }
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
    if role == StructuralRole::Definition
        && node.kind() == "type_identifier"
        && let Some(parent) = node.parent()
        && parent.kind() == "infer_type"
    {
        // The grammar has no name field here. The first non-comment named child
        // is the binder; a later type identifier may instead be its constraint.
        let mut cursor = parent.walk();
        for child in parent.named_children(&mut cursor) {
            cancellation.check()?;
            if child.kind() != "comment" {
                return Ok(child == node);
            }
        }
        return Ok(false);
    }
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

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn type_star_export_keeps_its_type_only_modifier() {
        for language in [
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            tree_sitter_typescript::LANGUAGE_TSX,
        ] {
            for source in [
                "export type * from './provider';",
                "export /* before */ type /* after */ * from './provider';",
                "export type *\nfrom './provider';",
                "export\ntype\n* from './provider';",
            ] {
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&language.into()).unwrap();
                let tree = parser.parse(source, None).unwrap();
                assert!(
                    !tree.root_node().has_error(),
                    "{source}: {}",
                    tree.root_node().to_sexp()
                );
                let statement = tree.root_node().named_child(0).unwrap();
                assert_eq!(
                    export_signature_syntax(
                        GrammarFamily::TypeScript,
                        statement,
                        &Cancellation::new()
                    )
                    .unwrap(),
                    Some("typescript.export_type_star_statement"),
                    "{source}: {}",
                    tree.root_node().to_sexp()
                );
            }
        }
    }

    #[test]
    fn star_export_classification_does_not_admit_errors_or_type_spellings() {
        for (source, expected) in [
            (
                "export * from './type';",
                "typescript.export_star_statement",
            ),
            (
                "export /* type */ * from './provider';",
                "typescript.export_star_statement",
            ),
            (
                "export type extra * from './provider';",
                "typescript.export_unsupported_statement",
            ),
            (
                "export extra * from './provider';",
                "typescript.export_unsupported_statement",
            ),
            (
                "export type * from './provider' extra;",
                "typescript.export_unsupported_statement",
            ),
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            let statement = tree.root_node().named_child(0).unwrap();
            assert_eq!(
                export_signature_syntax(GrammarFamily::TypeScript, statement, &Cancellation::new())
                    .unwrap(),
                Some(expected),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
}
