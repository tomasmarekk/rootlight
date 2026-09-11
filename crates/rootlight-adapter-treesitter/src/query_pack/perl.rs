//! Perl declaration positions, sigil-preserving names and callable headers.
//! Binding candidates follow native declaration fields, not arbitrary descendants.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

pub(super) fn native_depth(
    mut node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut depth = 0usize;
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        depth = depth
            .checked_add(1)
            .ok_or_else(|| super::query_failure("query-perl-depth"))?;
        node = parent;
    }
    Ok(depth)
}

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
    if role == StructuralRole::Scope && node.kind() == "substitution_regexp" {
        cancellation.check()?;
        return Ok(node
            .child_by_field_name("modifiers")
            .is_some_and(|modifiers| {
                source
                    .get(modifiers.byte_range())
                    .is_some_and(|flags| flags.contains(&b'e'))
            }));
    }
    if role == StructuralRole::Expression && node.kind() == "glob" {
        return glob_write(node, cancellation);
    }
    if role == StructuralRole::Reference
        && node.kind() == "coderef_call_expression"
        && direct_coderef_operand(node, source, cancellation)?.is_some()
    {
        // The exact callable name carries this invocation's evidence. Retaining
        // a second unresolved whole-expression occurrence would invent a gap.
        return Ok(false);
    }
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
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if role == StructuralRole::Expression {
        return value_expression(node, source, cancellation).map(Some);
    }
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
            "substitution_regexp" => return Ok(Some("perl.substitution")),
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
            "assignment_expression" => {
                return Ok(Some(if linear_assignment(node, cancellation)? {
                    "perl.assignment"
                } else {
                    "perl.non_linear_assignment"
                }));
            }
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
                    parent.kind() == "use_statement"
                        && parent.child_by_field_name("module") == Some(node)
                        && !parent.has_error()
                        && parent
                            .child(0)
                            .is_some_and(|keyword| source.get(keyword.byte_range()) == Some(b"use"))
                }) =>
        {
            "perl.use_module_name"
        }
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
            if directly_invoked_code_reference(node, source, cancellation)? {
                "perl.direct_coderef_function_name"
            } else {
                "perl.code_function_name"
            }
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

fn linear_assignment(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(owner) = parent {
        cancellation.check()?;
        match owner.kind() {
            "source_file"
            | "subroutine_declaration_statement"
            | "method_declaration_statement"
            | "anonymous_subroutine_expression"
            | "anonymous_method_expression"
            | "phaser_statement" => return Ok(true),
            "expression_statement"
            | "block"
            | "block_statement"
            | "parenthesized_expression"
            | "package_statement" => {}
            // Callback bodies and unmodeled control operators are not necessarily
            // evaluated once, even when the assignment has an ordinary block parent.
            _ => return Ok(false),
        }
        parent = owner.parent();
    }
    Ok(false)
}

fn value_expression(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    cancellation.check()?;
    if node.kind() == "glob" && glob_write(node, cancellation)? {
        return Ok(
            if !node.has_error()
                && node
                    .named_child(0)
                    .is_some_and(|name| name.kind() == "varname" && name.named_child_count() == 0)
            {
                "perl.static_glob_write"
            } else {
                "perl.dynamic_glob_write"
            },
        );
    }
    if node.kind() == "eval_expression"
        && !node.has_error()
        && single_operand(node, cancellation)?.is_some_and(|operand| operand.kind() == "block")
    {
        // Block eval is already parsed: nested writes retain their own evidence.
        // Exception control flow still prevents definite local CODE-value propagation.
        return Ok("perl.block_eval_barrier");
    }
    if matches!(node.kind(), "eval_expression" | "goto_expression")
        || (node.kind() == "substitution_regexp"
            && node
                .child_by_field_name("modifiers")
                .is_some_and(|modifiers| {
                    source
                        .get(modifiers.byte_range())
                        .is_some_and(|flags| flags.contains(&b'e'))
                }))
    {
        return Ok("perl.code_flow_barrier");
    }
    if coderef_operand(node, source, cancellation)?.is_some_and(scalar_name) {
        return Ok("perl.scalar_coderef");
    }
    if let Some(parent) = node.parent()
        && parent.kind() == "coderef_call_expression"
        && parent.named_child(0) == Some(node)
        && coderef_operand(parent, source, cancellation)?.is_some_and(scalar_name)
    {
        return Ok("perl.scalar_coderef_receiver");
    }
    if let Some(parent) = node.parent()
        && parent.kind() == "assignment_expression"
    {
        if parent.child_by_field_name("left") == Some(node) {
            return Ok(if scalar_name(node) {
                "perl.code_target"
            } else if node.kind() == "variable_declaration"
                && declaration_keyword(node, cancellation)? == Some("my")
                && node
                    .child_by_field_name("variable")
                    .is_some_and(scalar_name)
                && node.child_by_field_name("attributes").is_none()
            {
                "perl.lexical_code_target"
            } else {
                "perl.unproven_code_target"
            });
        }
        if parent.child_by_field_name("right") == Some(node) {
            let mut plain = false;
            for child in parent.children(&mut parent.walk()) {
                cancellation.check()?;
                plain |= child.kind() == "=" && source.get(child.byte_range()) == Some(b"=");
            }
            if !plain || parent.has_error() {
                return Ok("perl.unknown_code_value");
            }
            let mut value = node;
            while value.kind() == "parenthesized_expression" {
                if !explicit_group_end(value, source, cancellation)? {
                    return Ok("perl.unknown_code_value");
                }
                let Some(inner) = single_operand(value, cancellation)? else {
                    return Ok("perl.unknown_code_value");
                };
                value = inner;
            }
            if scalar_name(value) {
                return Ok("perl.copied_code_value");
            }
            if value.kind() == "refgen_expression"
                && let Some(function) = single_operand(value, cancellation)?
                && function.kind() == "function"
                && function
                    .named_child(0)
                    .is_some_and(|name| name.kind() == "varname" && name.named_child_count() == 0)
            {
                return Ok("perl.literal_code_value");
            }
            return Ok("perl.unknown_code_value");
        }
    }
    Ok(match node.kind() {
        "function"
            if node
                .named_child(0)
                .and_then(|name| name.named_child(0))
                .is_some_and(scalar_name) =>
        {
            "perl.scalar_amper"
        }
        _ => "perl.unknown_code_expression",
    })
}

fn glob_write(mut node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        match parent.kind() {
            "assignment_expression" => {
                return Ok(parent.child_by_field_name("left") == Some(node));
            }
            // Localization replaces storage for its dynamic lifetime, including
            // a declaration without an assignment. List/group wrappers preserve lvalues.
            "localization_expression" => return Ok(true),
            "parenthesized_expression" | "list_expression" => node = parent,
            _ => return Ok(false),
        }
    }
    Ok(false)
}

fn scalar_name(node: Node<'_>) -> bool {
    node.kind() == "scalar"
        && node
            .named_child(0)
            .is_some_and(|name| name.kind() == "varname" && name.named_child_count() == 0)
}

fn coderef_operand<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if node.kind() != "coderef_call_expression"
        || node.has_error()
        || !explicit_group_end(node, source, cancellation)?
    {
        return Ok(None);
    }
    let Some(mut operand) = node.named_child(0) else {
        return Ok(None);
    };
    while operand.kind() == "parenthesized_expression" {
        if !explicit_group_end(operand, source, cancellation)? {
            return Ok(None);
        }
        let Some(inner) = single_operand(operand, cancellation)? else {
            return Ok(None);
        };
        operand = inner;
    }
    Ok(Some(operand))
}

fn direct_coderef_operand<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(operand) = coderef_operand(node, source, cancellation)? else {
        return Ok(None);
    };
    if operand.kind() != "refgen_expression" {
        return Ok(None);
    }
    let Some(function) = single_operand(operand, cancellation)? else {
        return Ok(None);
    };
    let static_name = function.kind() == "function"
        && function
            .named_child(0)
            .is_some_and(|name| name.kind() == "varname" && name.named_child_count() == 0);
    Ok(static_name.then_some(function))
}

fn explicit_group_end(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    // The native scanner can synthesize an empty ')' at EOF without setting
    // has_error or is_missing. Only written delimiters establish this call shape.
    for child in node.children(&mut node.walk()) {
        cancellation.check()?;
        if child.kind() == ")" && source.get(child.byte_range()) == Some(b")") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn single_operand<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    for child in node.named_children(&mut node.walk()) {
        cancellation.check()?;
        if child.kind() == "comment" {
            continue;
        }
        if result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

fn directly_invoked_code_reference(
    function: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(reference) = function.parent() else {
        return Ok(false);
    };
    let mut parent = reference.parent();
    while let Some(node) = parent {
        cancellation.check()?;
        if node.kind() != "parenthesized_expression" {
            return Ok(direct_coderef_operand(node, source, cancellation)? == Some(function));
        }
        parent = node.parent();
    }
    Ok(false)
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
