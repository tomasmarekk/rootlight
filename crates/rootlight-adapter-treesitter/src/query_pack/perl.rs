//! Perl declaration positions, sigil-preserving names and callable headers.
//! Binding candidates follow native declaration fields, not arbitrary descendants.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn variable_kind(node: Node<'_>) -> bool {
    matches!(node.kind(), "scalar" | "array" | "hash")
}

fn binding(
    mut node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        match parent.kind() {
            "mandatory_parameter"
            | "optional_parameter"
            | "named_parameter"
            | "slurpy_parameter" => {
                return Ok((parent.named_child(0) == Some(node)).then_some("perl.parameter"));
            }
            "variable_declaration" => {
                return Ok(Some(match declaration_keyword(parent, cancellation)? {
                    Some("my" | "state") => "perl.lexical_variable",
                    Some("field") => "perl.field",
                    _ => "perl.package_variable",
                }));
            }
            "for_statement" => {
                if parent.child_by_field_name("list") == Some(node) {
                    return Ok(None);
                }
                return Ok(declaration_keyword(parent, cancellation)?.map(
                    |keyword| match keyword {
                        "my" | "state" => "perl.lexical_variable",
                        _ => "perl.package_variable",
                    },
                ));
            }
            "variable_group" | "refalias_variable" => node = parent,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if role == StructuralRole::Reference && node.kind() == "bareword" {
        return Ok(node
            .parent()
            .is_some_and(|parent| parent.child_by_field_name("name") != Some(node)));
    }
    if role == StructuralRole::Reference && node.kind() == "string_content" {
        let mut ancestor = node.parent().and_then(|literal| literal.parent());
        while let Some(parent) = ancestor {
            cancellation.check()?;
            match parent.kind() {
                // Grouping preserves literal argument identity; expressions such as
                // concatenation, conditionals and array references do not.
                "parenthesized_expression" | "list_expression" => ancestor = parent.parent(),
                "use_statement" => {
                    return Ok(parent
                        .child(0)
                        .is_some_and(|keyword| keyword.kind() == "use")
                        && parent.child_by_field_name("module").is_some_and(|module| {
                            source.get(module.byte_range()) == Some(b"subs")
                        }));
                }
                _ => return Ok(false),
            }
        }
        return Ok(false);
    }
    if !variable_kind(node) {
        return Ok(true);
    }
    let is_binding = binding(node, cancellation)?.is_some();
    Ok(match role {
        StructuralRole::Declaration | StructuralRole::Definition => is_binding,
        StructuralRole::Reference => !is_binding,
        _ => true,
    })
}

pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if role == StructuralRole::Reference
        && node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "func0op_call_expression" | "func1op_call_expression"
            ) && parent.child_by_field_name("function") == Some(node)
        })
    {
        return Ok(Some("perl.builtin_function_name"));
    }
    if role == StructuralRole::Scope {
        if node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "conditional_statement" | "loop_statement" | "elsif"
            ) && parent.child_by_field_name("condition") == Some(node)
        }) {
            return Ok(Some("perl.statement"));
        }
        match node.kind() {
            "subroutine_declaration_statement" => {
                return Ok(Some(match declaration_keyword(node, cancellation)? {
                    Some("my" | "state") => "perl.lexical_function",
                    Some("our") => "perl.our_function",
                    _ => "perl.function",
                }));
            }
            "package_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    cancellation.check()?;
                    if child.kind() == "block" {
                        return Ok(Some("perl.package_block"));
                    }
                }
                return Ok(Some("perl.package_switch"));
            }
            "expression_statement" => return Ok(Some("perl.statement")),
            "for_statement" => {
                return Ok(Some(match declaration_keyword(node, cancellation)? {
                    Some("my" | "state") => "perl.lexical_for",
                    _ => "perl.package_for",
                }));
            }
            "conditional_statement" | "loop_statement" | "cstyle_for_statement" => {
                return Ok(Some("perl.control"));
            }
            "anonymous_subroutine_expression" => return Ok(Some("perl.lambda")),
            "anonymous_method_expression" => return Ok(Some("perl.method_lambda")),
            "class_phaser_statement" | "class_statement" | "role_statement" | "try_statement" => {
                return Ok(Some("perl.unsupported_context"));
            }
            "phaser_statement" => return Ok(Some("perl.phaser")),
            _ => {}
        }
    }
    if role == StructuralRole::Declaration && variable_kind(node) {
        return binding(node, cancellation);
    }
    Ok(Some(match node.kind() {
        "source_file" => "perl.file",
        "package"
            if role == StructuralRole::Reference
                && node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "package_statement" | "class_statement" | "role_statement"
                    )
                }) =>
        {
            "perl.package_context"
        }
        "package_statement" => "perl.package",
        "class_statement" => "perl.class",
        "role_statement" => "perl.trait",
        "subroutine_declaration_statement" => "perl.function",
        "method_declaration_statement" => "perl.method",
        "mandatory_parameter" | "optional_parameter" | "named_parameter" | "slurpy_parameter" => {
            "perl.parameter"
        }
        "bareword"
            if role == StructuralRole::Reference
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "require_expression") =>
        {
            "perl.module_name"
        }
        "bareword" if role == StructuralRole::Reference => "perl.bare_function_name",
        "bareword" | "package" => "perl.identifier",
        "scalar" | "array" | "hash" => "perl.variable_name",
        "arraylen" => "perl.array_length",
        "container_variable" | "slice_container_variable" | "keyval_container_variable" => {
            if node
                .parent()
                .is_some_and(|parent| parent.child_by_field_name("array") == Some(node))
            {
                "perl.array_container"
            } else if node
                .parent()
                .is_some_and(|parent| parent.child_by_field_name("hash") == Some(node))
            {
                "perl.hash_container"
            } else {
                "perl.dynamic_container"
            }
        }
        // The grammar aliases builtin list operators to `function` too. Only
        // native bareword terminals prove this direct user-function form.
        "function"
            if role == StructuralRole::Reference
                && matches!(node.grammar_name(), "_identifier" | "_bareword_token1")
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "function_call_expression"
                        && parent.child_by_field_name("function") == Some(node)
                        && !parent
                            .named_children(&mut parent.walk())
                            .any(|child| child.kind() == "indirect_object")
                }) =>
        {
            "perl.static_function_name"
        }
        // A native user-function alias has no keyword child. Its bare spelling
        // still needs a declaration/import visible at this compile position.
        "function"
            if role == StructuralRole::Reference
                && node.child_count() == 0
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "ambiguous_function_call_expression"
                        && parent.child_by_field_name("function") == Some(node)
                        && !parent
                            .named_children(&mut parent.walk())
                            .any(|child| child.kind() == "indirect_object")
                }) =>
        {
            "perl.bare_function_name"
        }
        // Builtin list operators retain an anonymous keyword child in this alias.
        // Require a lexical binding or subs import, never a same-named declaration alone.
        "function"
            if role == StructuralRole::Reference
                && node.named_child_count() == 0
                && node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "function_call_expression" | "ambiguous_function_call_expression"
                    ) && parent.child_by_field_name("function") == Some(node)
                        && !parent
                            .named_children(&mut parent.walk())
                            .any(|child| child.kind() == "indirect_object")
                }) =>
        {
            "perl.importable_function_name"
        }
        "function"
            if role == StructuralRole::Reference
                && node
                    .named_child(0)
                    .is_some_and(|name| name.kind() == "varname")
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "function_call_expression"
                        && parent.child_by_field_name("function") == Some(node)
                }) =>
        {
            "perl.amper_function_name"
        }
        "function"
            if role == StructuralRole::Reference
                && node
                    .named_child(0)
                    .is_some_and(|name| name.kind() == "varname")
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "refgen_expression") =>
        {
            "perl.code_function_name"
        }
        "function" => "perl.function_name",
        "string_content"
            if role == StructuralRole::Reference
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "quoted_word_list") =>
        {
            "perl.subs_word_list"
        }
        "string_content" if role == StructuralRole::Reference => "perl.subs_import",
        "method_call_expression" => "perl.method_application",
        "coderef_call_expression" => "perl.coderef_application",
        "block" | "block_statement" => "perl.block",
        "comment" => "perl.comment",
        "pod" => "perl.documentation",
        "string_literal" | "interpolated_string_literal" | "command_string" => "perl.string",
        _ => return Ok(None),
    }))
}

fn declaration_keyword(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        match child.kind() {
            "my" => return Ok(Some("my")),
            "state" => return Ok(Some("state")),
            "our" => return Ok(Some("our")),
            "field" => return Ok(Some("field")),
            _ => {}
        }
    }
    Ok(None)
}

pub(super) fn header_end(node: Node<'_>) -> usize {
    node.child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte())
}
