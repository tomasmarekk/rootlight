//! VBA authored binding positions and body-independent declaration headers.
//! Dynamic dispatch and call/index syntax need evidence beyond declared field targets.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

pub(super) fn class_header<'tree>(
    root: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = root.walk();
    let mut header = None;
    for child in root.named_children(&mut cursor) {
        cancellation.check()?;
        match child.kind() {
            "newline" | "comment" => {}
            "frm_version_statement" if header.is_none() => {
                let Some(text) = source.get(child.byte_range()) else {
                    return Ok(None);
                };
                let suffix = source
                    .get(child.end_byte()..)
                    .and_then(|tail| tail.split(|byte| matches!(byte, b'\r' | b'\n')).next())
                    .unwrap_or_default()
                    .trim_ascii();
                // The grammar permits any version and optional identifier here;
                // only the exported class preamble proves a class, not VB_Name.
                let mut words = text
                    .split(u8::is_ascii_whitespace)
                    .filter(|word| !word.is_empty());
                if child.has_error()
                    || !(suffix.is_empty() || suffix.starts_with(b"'"))
                    || !words
                        .next()
                        .is_some_and(|word| word.eq_ignore_ascii_case(b"VERSION"))
                    || words.next() != Some(b"1.0".as_slice())
                    || !words
                        .next()
                        .is_some_and(|word| word.eq_ignore_ascii_case(b"CLASS"))
                    || words.next().is_some()
                {
                    return Ok(None);
                }
                header = Some(child);
            }
            "frm_version_statement" => return Ok(None),
            "attribute_statement"
            | "frm_begin_block"
            | "frm_begin_property_block"
            | "frm_property_statement"
                if header.is_some() => {}
            _ => break,
        }
    }
    Ok(header)
}

pub(super) fn module_name_node<'tree>(
    root: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = root.walk();
    let mut name = None;
    for child in root.named_children(&mut cursor) {
        cancellation.check()?;
        match child.kind() {
            "attribute_statement" => {
                if child.child_by_field_name("name").is_some_and(|attribute| {
                    source
                        .get(attribute.byte_range())
                        .is_some_and(|text| text.eq_ignore_ascii_case(b"VB_Name"))
                }) {
                    // MS-VBAL 4.2 gives the last header attribute precedence.
                    // Even an invalid last value must hide an earlier valid name.
                    name = child.child_by_field_name("value");
                }
            }
            "newline"
            | "comment"
            | "frm_version_statement"
            | "frm_begin_block"
            | "frm_begin_property_block"
            | "frm_property_statement" => {}
            _ => break,
        }
    }
    Ok(name)
}

fn declaration_syntax(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "sub_declaration" => "vba.sub_function",
        "function_declaration" => "vba.function",
        "conditional_sub_declaration" => "vba.conditional_sub_function",
        "conditional_function_declaration" => "vba.conditional_function",
        "conditional_property_declaration" => "vba.conditional_property",
        "declare_sub_statement" => "vba.external_sub_function",
        "declare_function_statement" => "vba.external_function",
        "property_get_declaration" => "vba.get_property",
        "property_let_declaration" => "vba.let_property",
        "property_set_declaration" => "vba.set_property",
        "event_declaration" => "vba.event",
        "type_declaration" => "vba.struct",
        "enum_declaration" => "vba.enum",
        "type_member" => "vba.field",
        "enum_member" => "vba.enum_constant",
        "parameter" => "vba.parameter",
        "variable_declarator" => "vba.variable",
        "const_declarator" => "vba.constant",
        "preprocessor_const" => "vba.compilation_constant",
        "label_statement" => "vba.statement_label",
        _ => return None,
    })
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole, source: &[u8]) -> bool {
    if role == StructuralRole::Reference && node.kind() == "line_number_literal" {
        return node.parent().is_some_and(|parent| {
            if matches!(parent.kind(), "on_error_statement" | "resume_statement")
                && source
                    .get(node.byte_range())
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(crate::vba_bindings::statement_label_key)
                    .as_deref()
                    == Some("0")
            {
                return false;
            }
            matches!(
                parent.kind(),
                "goto_statement"
                    | "gosub_statement"
                    | "on_goto_statement"
                    | "on_error_statement"
                    | "resume_statement"
            )
        });
    }
    // Keep application evidence even when its named procedure can be bound:
    // its argument mapping and dispatch through returned values need separate proof.
    if role != StructuralRole::Reference || !matches!(node.kind(), "identifier" | "bang_identifier")
    {
        return true;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
    if parent.kind() == "dotted_type_expression" {
        // A qualified type has one lookup context; its individual names cannot
        // independently bind to runtime values or unrelated module-local types.
        return false;
    }
    if parent.kind() == "type_expression"
        && parent
            .parent()
            .is_some_and(|owner| owner.kind() == "implements_statement")
    {
        // The complete type path owns the implementation reference. Retaining
        // its identifier pieces would fabricate independent runtime reads.
        return false;
    }
    if parent.kind() == "bang_identifier" {
        return false;
    }
    if declaration_syntax(parent.kind()).is_some()
        && parent.child_by_field_name("name") == Some(node)
    {
        return false;
    }
    !matches!(parent.kind(), "attribute_statement" | "label_statement")
        && !(matches!(
            parent.kind(),
            "qualified_member_expression" | "implicit_member_expression"
        ) && parent.child_by_field_name("member") == Some(node))
}

pub(super) fn syntax(node: Node<'_>, role: StructuralRole) -> Option<&'static str> {
    if role == StructuralRole::Signature {
        if matches!(node.kind(), "preprocessor_else" | "type_preprocessor_else") {
            return Some(if node.has_error() {
                "vba.compilation_unknown"
            } else {
                "vba.compilation_else"
            });
        }
        if let Some(parent) = node.parent()
            && parent.child_by_field_name("condition") == Some(node)
        {
            if matches!(
                parent.kind(),
                "preprocessor_if"
                    | "type_preprocessor_if"
                    | "preprocessor_elseif"
                    | "type_preprocessor_elseif"
            ) && parent.has_error()
            {
                return Some("vba.compilation_unknown");
            }
            match parent.kind() {
                "preprocessor_if" | "type_preprocessor_if" => return Some("vba.compilation_if"),
                "preprocessor_elseif" | "type_preprocessor_elseif" => {
                    return Some("vba.compilation_elseif");
                }
                _ => {}
            }
        }
    }
    if node.kind() == "line_number_literal"
        && matches!(
            role,
            StructuralRole::Definition | StructuralRole::Declaration
        )
    {
        return Some("vba.statement_label");
    }
    if matches!(role, StructuralRole::Definition | StructuralRole::Reference)
        && node.parent().is_some_and(|parent| {
            parent.kind() == "attribute_statement"
                && parent.child_by_field_name("value") == Some(node)
        })
    {
        return Some("vba.module_name");
    }
    if node.kind() == "parameter"
        && (node.child_by_field_name("optional_modifier").is_some()
            || node.child_by_field_name("paramarray_modifier").is_some())
    {
        return Some("vba.omittable_parameter");
    }
    if let Some(syntax) = declaration_syntax(node.kind()) {
        return Some(syntax);
    }
    Some(match node.kind() {
        "frm_version_statement" if role == StructuralRole::Signature => "vba.class",
        "source_file" => "vba.file",
        "preprocessor_if" | "type_preprocessor_if" => "vba.preprocessor",
        "preprocessor_block" | "type_preprocessor_block" => "vba.preprocessor_branch",
        "identifier" | "bang_identifier" => "vba.identifier",
        "qualified_member_expression" | "implicit_member_expression" => "vba.member",
        "call_expression" | "call_statement" if role == StructuralRole::Reference => {
            if node.has_error() {
                "vba.incomplete_application"
            } else {
                "vba.application"
            }
        }
        "new_expression" if role == StructuralRole::Reference => "vba.construction",
        "comment" => "vba.comment",
        "string_literal" => "vba.string",
        _ => return None,
    })
}

pub(super) fn implemented_type_syntax(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    // A conditional or recovered directive cannot establish an unconditional
    // class-wide interface contract.
    let supported = node.parent().is_some_and(|statement| {
        statement.kind() == "implements_statement"
            && !statement.has_error()
            && statement
                .parent()
                .is_some_and(|owner| owner.kind() == "source_file")
    }) && crate::vba_pointer::parse(
        source.get(node.start_byte()..).unwrap_or_default(),
        cancellation,
    )?
    .is_some_and(|path| path.end() == node.end_byte() - node.start_byte());
    let tail = source.get(node.end_byte()..).unwrap_or_default();
    let whitespace = rootlight_vba::logical_whitespace_len(tail, &mut || {
        cancellation.check().map_err(AdapterError::from)
    })?;
    Ok(
        if supported && matches!(tail.get(whitespace), None | Some(b'\r' | b'\n' | b'\'')) {
            "vba.implemented_type"
        } else {
            "vba.unsupported_implemented_type"
        },
    )
}

pub(super) fn declared_visibility(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let header = if matches!(node.kind(), "variable_declarator" | "const_declarator") {
        node.parent().unwrap_or(node)
    } else {
        node
    };
    // This is the written access modifier, not proof of project membership or
    // external accessibility. Defaults and enclosing-module restrictions need
    // their own source evidence before cross-file resolution may consume them.
    if header.has_error() {
        return Ok("vba.visibility_unknown");
    }
    let mut parent = header.parent();
    while let Some(ancestor) = parent {
        cancellation.check()?;
        if declaration_syntax(ancestor.kind()).is_some() {
            return Ok("vba.visibility_unknown");
        }
        parent = ancestor.parent();
    }
    let Some(visibility) = header.child_by_field_name("visibility") else {
        return Ok("vba.visibility_unknown");
    };
    Ok(match source.get(visibility.byte_range()) {
        Some(text) if text.eq_ignore_ascii_case(b"Public") => "vba.public",
        Some(text) if text.eq_ignore_ascii_case(b"Private") => "vba.private",
        Some(text) if text.eq_ignore_ascii_case(b"Friend") => "vba.project_restricted",
        _ => "vba.visibility_unknown",
    })
}

pub(super) fn self_member_node<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    if node.kind() != "qualified_member_expression" || node.has_error() {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let operator = node.child_by_field_name("operator")?;
    if receiver.kind() != "identifier"
        || !source
            .get(receiver.byte_range())?
            .eq_ignore_ascii_case(b"Me")
        || source.get(operator.byte_range())? != b"."
    {
        return None;
    }
    node.child_by_field_name("member")
}

pub(super) fn reference_syntax(
    mut child: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let member = child
        .parent()
        .filter(|parent| self_member_node(*parent, source) == Some(child));
    let expression = member.unwrap_or(child);
    // Me is an instance expression, not an ordinary name lookup. Quoted or
    // type-suffixed names and terminal members do not establish this keyword.
    let instance = member.is_none()
        && expression.kind() == "identifier"
        && source
            .get(expression.byte_range())
            .is_some_and(|text| text.eq_ignore_ascii_case(b"Me"));
    let invoked = expression.parent().is_some_and(|parent| {
        (parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(expression))
            || (parent.kind() == "call_statement"
                && parent.child_by_field_name("callee") == Some(expression))
    }) || split_output_argument_is_invoked(expression, source);
    if invoked && !instance && single_grouped_statement_call(expression, source, cancellation)? {
        return Ok("vba.invoked_identifier");
    }
    if let Some(ty) = child
        .parent()
        .filter(|node| node.kind() == "type_expression")
    {
        if child.kind() == "dotted_type_expression" {
            let tail = source.get(child.end_byte()..).unwrap_or_default();
            let trivia = rootlight_vba::logical_whitespace_len(tail, &mut || {
                cancellation.check().map_err(AdapterError::from)
            })?;
            // Native recovery can split another qualifier into a new statement.
            // The retained prefix must not prove the type of the whole name.
            if child.has_error() || matches!(tail.get(trivia), Some(b'.' | b'!')) {
                return Ok("vba.unsupported_type_identifier");
            }
        }
        if ty.parent().is_some_and(|parent| {
            parent.kind() == "type_of_expression" && parent.child_by_field_name("type") == Some(ty)
        }) {
            return Ok("vba.tested_type_identifier");
        }
        let syntax = if let Some(clause) =
            ty.parent().filter(|node| node.kind() == "as_type_clause")
            && let Some(declaration) = clause.parent()
        {
            // The grammar's anonymous regex keywords are invisible to child
            // traversal. Check only the native clause prefix, preserving New
            // as a distinct instantiation rule even across continued lines.
            let prefix = source
                .get(clause.start_byte()..ty.start_byte())
                .unwrap_or_default();
            let mut words = prefix
                .split(u8::is_ascii_whitespace)
                .filter(|word| !word.is_empty() && *word != b"_");
            let starts_as = words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(b"As"));
            let modifier = words.next();
            let automatic = starts_as
                && modifier.is_some_and(|word| word.eq_ignore_ascii_case(b"New"))
                && words.next().is_none();
            let mut scalar = declaration.child_by_field_name("bounds").is_none();
            let mut cursor = clause.walk();
            for part in clause.children(&mut cursor) {
                cancellation.check()?;
                if part.kind() == "array_bounds" {
                    scalar = false;
                }
            }
            if automatic {
                if scalar && matches!(declaration.kind(), "variable_declarator" | "type_member") {
                    "vba.automatic_type_identifier"
                } else {
                    "vba.unsupported_automatic_type_identifier"
                }
            } else if starts_as
                && modifier.is_none()
                && scalar
                && matches!(
                    declaration.kind(),
                    "variable_declarator" | "parameter" | "type_member"
                )
            {
                "vba.direct_type_identifier"
            } else {
                "vba.type_identifier"
            }
        } else {
            "vba.type_identifier"
        };
        return Ok(syntax);
    }
    while let Some(parent) = child.parent() {
        cancellation.check()?;
        if child.kind() == "implicit_member_expression"
            && parent.kind() == "call_statement"
            && parent
                .prev_named_sibling()
                .is_some_and(|statement| split_construction_type(statement, source) == Some(child))
        {
            return Ok("vba.unsupported_constructed_type_identifier");
        }
        let kind = parent.kind();
        if kind == "new_expression" {
            let complete = rootlight_vba::constructed_type_range(
                source.get(parent.start_byte()..).unwrap_or_default(),
                &mut || cancellation.check(),
            )?
            .is_some_and(|range| parent.start_byte() + range.end == parent.end_byte());
            return Ok(
                if child.kind() == "identifier"
                    && parent.child_by_field_name("type") == Some(child)
                    && complete
                    && !parent.parent().is_some_and(|statement| {
                        split_construction_type(statement, source).is_some()
                    })
                {
                    "vba.constructed_type_identifier"
                } else {
                    "vba.unsupported_constructed_type_identifier"
                },
            );
        }
        if kind == "raise_event_statement" && parent.child_by_field_name("event") == Some(child) {
            return Ok("vba.event_identifier");
        }
        if (matches!(
            kind,
            "goto_statement" | "gosub_statement" | "on_error_statement" | "resume_statement"
        ) && parent.child_by_field_name("target") == Some(child))
            || (kind == "on_goto_statement"
                && parent.child_by_field_name("selector") != Some(child))
        {
            return Ok(if kind == "on_error_statement" {
                "vba.error_label"
            } else if kind == "resume_statement" {
                "vba.resume_label"
            } else {
                "vba.statement_label"
            });
        }
        if kind == "addressof_expression" {
            // VBA's pinned grammar admits only a simple target here. Do not
            // bind the first component if a qualified name was split off.
            return Ok(
                if child.kind() == "identifier"
                    && parent.child_by_field_name("target") == Some(child)
                    && !parent.has_error()
                    && crate::vba_pointer::parse(
                        source.get(child.start_byte()..).unwrap_or_default(),
                        cancellation,
                    )?
                    .is_some_and(|path| {
                        path.components() == 1
                            && path.end() == child.end_byte() - child.start_byte()
                    })
                {
                    "vba.procedure_pointer"
                } else {
                    "vba.unsupported_procedure_pointer"
                },
            );
        }
        if kind == "preprocessor_const" {
            return Ok("vba.compilation_identifier");
        }
        if matches!(
            kind,
            "preprocessor_if"
                | "preprocessor_elseif"
                | "type_preprocessor_if"
                | "type_preprocessor_elseif"
                | "conditional_sub_declaration"
                | "conditional_function_declaration"
                | "conditional_property_declaration"
        ) {
            // Conditional procedure headers may have several condition fields.
            let mut cursor = parent.walk();
            for condition in parent.children_by_field_name("condition", &mut cursor) {
                cancellation.check()?;
                if condition == child {
                    return Ok("vba.compilation_identifier");
                }
            }
        }
        if matches!(
            kind,
            "type_expression"
                | "dotted_type_expression"
                | "implements_statement"
                | "goto_statement"
                | "gosub_statement"
                | "on_error_statement"
                | "resume_statement"
        ) || (instance && kind == "const_declarator")
            || (kind == "named_argument" && parent.child_by_field_name("name") == Some(child))
            || (kind == "on_goto_statement"
                && parent.child_by_field_name("selector") != Some(child))
        {
            return Ok("vba.nonvalue_identifier");
        }
        if kind == "parameter" {
            return Ok(if member.is_some() {
                "vba.nonvalue_identifier"
            } else {
                "vba.default_constant_identifier"
            });
        }
        if kind.starts_with("conditional_")
            && declaration_syntax(kind).is_some()
            && !matches!(child.kind(), "block" | "conditional_branch_body")
        {
            return Ok("vba.nonvalue_identifier");
        }
        if declaration_syntax(kind).is_some() || kind == "source_file" {
            break;
        }
        child = parent;
    }
    if instance {
        return Ok(if invoked {
            "vba.invoked_instance"
        } else {
            "vba.instance"
        });
    }
    Ok(if let Some(access) = member.and_then(property_access) {
        match (access, invoked) {
            (PropertyAccess::Get, false) => "vba.self_get_member",
            (PropertyAccess::Let, false) => "vba.self_let_member",
            (PropertyAccess::Set, false) => "vba.self_set_member",
            (PropertyAccess::Get, true) => "vba.self_invoked_get_member",
            (PropertyAccess::Let, true) => "vba.self_invoked_let_member",
            (PropertyAccess::Set, true) => "vba.self_invoked_set_member",
        }
    } else if member.is_some() && invoked {
        "vba.self_invoked_member"
    } else if member.is_some() {
        "vba.self_member"
    } else if let Some(access) = property_access(expression) {
        match (access, invoked) {
            (PropertyAccess::Get, false) => "vba.value_identifier",
            (PropertyAccess::Let, false) => "vba.let_identifier",
            (PropertyAccess::Set, false) => "vba.set_identifier",
            (PropertyAccess::Get, true) => "vba.invoked_get_identifier",
            (PropertyAccess::Let, true) => "vba.invoked_let_identifier",
            (PropertyAccess::Set, true) => "vba.invoked_set_identifier",
        }
    } else if invoked {
        "vba.invoked_identifier"
    } else {
        "vba.identifier"
    })
}

pub(super) fn qualified_pointer_range(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<std::ops::Range<usize>>, AdapterError> {
    if node.kind() != "identifier"
        || !node.parent().is_some_and(|parent| {
            parent.kind() == "addressof_expression"
                && parent.child_by_field_name("target") == Some(node)
                && !parent.has_error()
        })
    {
        return Ok(None);
    }
    let start = node.start_byte();
    Ok(
        crate::vba_pointer::parse(source.get(start..).unwrap_or_default(), cancellation)?
            .filter(|path| path.components() > 1)
            .map(|path| start..start + path.end()),
    )
}

pub(super) fn is_pointer_fragment(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !matches!(
        node.kind(),
        "implicit_member_expression" | "qualified_member_expression" | "call_statement"
    ) {
        return Ok(false);
    }
    let mut statement = node;
    while statement.kind() != "call_statement" {
        cancellation.check()?;
        if statement.kind() == "comparison_expression"
            && let Some(operator) = statement.child_by_field_name("operator")
            && let Some(missing) = operator.child(0)
            && is_pointer_comparison_recovery(missing, source, cancellation)?
            && let Some(right) = statement.child_by_field_name("right")
            && node.start_byte() >= right.start_byte()
            && node.end_byte() <= right.end_byte()
        {
            return Ok(true);
        }
        let Some(parent) = statement.parent() else {
            return Ok(false);
        };
        if declaration_syntax(parent.kind()).is_some() {
            return Ok(false);
        }
        statement = parent;
    }
    let Some(mut previous) = statement.prev_named_sibling() else {
        return Ok(false);
    };
    while previous.kind() == "line_continuation" {
        cancellation.check()?;
        let Some(prior) = previous.prev_named_sibling() else {
            return Ok(false);
        };
        previous = prior;
    }
    if previous.kind() != "call_statement" {
        return Ok(false);
    }
    // The native grammar splits the trailing pointer argument into an adjacent
    // implicit-member call. Suppress only its counterfeit callee/application,
    // preserving any actual argument expressions after the complete name.
    while previous.kind() != "addressof_expression" {
        cancellation.check()?;
        let Some(child) = u32::try_from(previous.named_child_count().saturating_sub(1))
            .ok()
            .and_then(|last| previous.named_child(last))
        else {
            return Ok(false);
        };
        previous = child;
    }
    let Some(target) = previous.child_by_field_name("target") else {
        return Ok(false);
    };
    let Some(range) = qualified_pointer_range(target, source, cancellation)? else {
        return Ok(false);
    };
    if let Some(callee) = statement.child_by_field_name("callee")
        && callee.start_byte() >= target.end_byte()
        && callee.end_byte() <= range.end
    {
        return Ok(node == statement
            || (node.start_byte() >= callee.start_byte() && node.end_byte() <= range.end));
    }
    Ok(false)
}

pub(crate) fn is_pointer_comparison_recovery(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !node.is_missing() || node.kind() != "=" {
        return Ok(false);
    }
    let Some(operator) = node
        .parent()
        .filter(|parent| parent.kind() == "comparison_operator" && parent.child_count() == 1)
    else {
        return Ok(false);
    };
    let Some(comparison) = operator
        .parent()
        .filter(|parent| parent.kind() == "comparison_expression")
    else {
        return Ok(false);
    };
    let Some(left) = comparison
        .child_by_field_name("left")
        .filter(|left| left.kind() == "addressof_expression")
    else {
        return Ok(false);
    };
    let Some(target) = left.child_by_field_name("target") else {
        return Ok(false);
    };
    let Some(right) = comparison
        .child_by_field_name("right")
        .filter(|right| !right.has_error())
    else {
        return Ok(false);
    };
    let Some(range) = qualified_pointer_range(target, source, cancellation)? else {
        return Ok(false);
    };
    // Inside parentheses the grammar inserts an '=' between the pointer's two
    // pieces. The first-party name parser proves these exact bytes form one
    // qualified target; no other error or real comparison is repaired here.
    Ok(right.end_byte() == range.end
        && target.end_byte() <= node.start_byte()
        && node.end_byte() <= right.start_byte()
        && comparison.end_byte() == range.end)
}

fn split_output_argument_is_invoked(expression: Node<'_>, source: &[u8]) -> bool {
    let Some(output) = expression.parent() else {
        return false;
    };
    let Some(statement) = output.parent() else {
        return false;
    };
    let Some(next) = statement.next_named_sibling() else {
        return false;
    };
    // The grammar can end an output statement at F in `Print F(x) & tail`
    // and put `(x) & tail` in an adjacent expression node. Binding still
    // distinguishes a declared function from an indexed value.
    output.kind() == "output_list"
        && output.child_by_field_name("value") == Some(expression)
        && statement.kind() == "call_statement"
        && statement.child_by_field_name("arguments") == Some(output)
        && next.kind() == "expression_statement"
        && !next.has_error()
        && next.start_byte() == expression.end_byte()
        && source.get(expression.end_byte()) == Some(&b'(')
}

fn split_construction_type<'tree>(statement: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let construction = statement.child_by_field_name("right")?;
    let next = statement.next_named_sibling()?;
    let member = next.child_by_field_name("callee")?;
    // The grammar can split `New Library.Class` into a simple construction and
    // an adjacent implicit-member statement. Neither half proves a type target.
    (construction.kind() == "new_expression"
        && next.kind() == "call_statement"
        && member.kind() == "implicit_member_expression"
        && source
            .get(construction.end_byte()..member.start_byte())?
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t')))
    .then_some(member)
}

pub(super) fn member_syntax(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if let Some(statement) = node.parent()
        && statement.kind() == "call_statement"
        && statement.child_by_field_name("callee") == Some(node)
        && let Some(arguments) = statement.child_by_field_name("arguments")
        && arguments.kind() == "unparenthesized_argument_list"
        && arguments.named_child_count() == 1
        && let Some(argument) = arguments.named_child(0)
        && argument.kind() == "comparison_expression"
        && argument
            .child_by_field_name("left")
            .is_some_and(|left| left.kind() == "parenthesized_expression")
        && argument
            .child_by_field_name("operator")
            .and_then(|operator| source.get(operator.byte_range()))
            == Some(b"=")
    {
        // The grammar keeps either GLR reading of obj.Item(i) = value. A call
        // with one comparison argument does not rule out indexed assignment.
        return Ok("vba.member");
    }
    // Invocation position alone cannot distinguish a procedure from a field or
    // default-member dispatch. Keep it as a reference until binding supplies a target.
    Ok(match reference_syntax(node, source, cancellation)? {
        "vba.invoked_identifier"
            if matches!(
                node.kind(),
                "qualified_member_expression" | "implicit_member_expression"
            ) =>
        {
            "vba.invoked_member"
        }
        "vba.value_identifier" => "vba.get_member",
        "vba.let_identifier" => "vba.let_member",
        "vba.set_identifier" => "vba.set_member",
        "vba.invoked_get_identifier" => "vba.invoked_get_member",
        "vba.invoked_let_identifier" => "vba.invoked_let_member",
        "vba.invoked_set_identifier" => "vba.invoked_set_member",
        "vba.unsupported_constructed_type_identifier" => {
            "vba.unsupported_constructed_type_identifier"
        }
        _ => "vba.member",
    })
}

enum PropertyAccess {
    Get,
    Let,
    Set,
}

fn single_grouped_statement_call(
    expression: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if expression.kind() != "identifier" {
        return Ok(false);
    }
    let Some(application) = expression.parent().filter(|parent| {
        parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(expression)
    }) else {
        return Ok(false);
    };
    let Some(statement) = application.parent().filter(|parent| {
        parent.kind() == "call_statement"
            && parent.child_by_field_name("callee") == Some(application)
            && parent.child_by_field_name("arguments").is_none()
            && !parent.has_error()
            && parent.start_byte() == expression.start_byte()
            && parent.end_byte() == application.end_byte()
    }) else {
        return Ok(false);
    };
    let Some(arguments) = application.child_by_field_name("arguments") else {
        return Ok(false);
    };
    if arguments.kind() != "argument_list" || arguments.named_child_count() != 1 {
        return Ok(false);
    }
    let mut cursor = arguments.walk();
    for child in arguments.children(&mut cursor) {
        cancellation.check()?;
        if matches!(
            child.kind(),
            "," | ";" | "named_argument" | "byval_argument"
        ) {
            return Ok(false);
        }
    }
    let Some(gap) = source.get(expression.end_byte()..arguments.start_byte()) else {
        return Ok(false);
    };
    let mut check = || cancellation.check().map_err(AdapterError::from);
    if rootlight_vba::logical_whitespace_len(gap, &mut check)? != gap.len() {
        return Ok(false);
    }
    let Some(group) = source.get(arguments.byte_range()) else {
        return Ok(false);
    };
    if rootlight_vba::parenthesized_expression_end(group, &mut check)? != Some(group.len()) {
        return Ok(false);
    }
    let mut before = statement.start_byte();
    while before > 0 {
        check()?;
        match source.get(before - 1) {
            Some(b' ' | b'\t' | b'\x0c') => before -= 1,
            Some(b'\r' | b'\n' | b':') => break,
            _ => return Ok(false),
        }
    }
    let Some(tail) = source.get(statement.end_byte()..) else {
        return Ok(false);
    };
    let whitespace = rootlight_vba::logical_whitespace_len(tail, &mut check)?;
    // MS-VBAL 3.3 and 5.4.2.1 allow a sole grouped statement argument with or
    // without separating trivia. Native GLR can wrap the callee in a value
    // application; that partition must not select Get or hide a callable Sub.
    Ok(matches!(
        tail.get(whitespace),
        None | Some(b'\r' | b'\n' | b':' | b'\'')
    ))
}

fn property_access(mut expression: Node<'_>) -> Option<PropertyAccess> {
    let mut parent = expression.parent()?;
    if parent.kind() == "call_expression"
        && parent.child_by_field_name("function") == Some(expression)
    {
        expression = parent;
        parent = expression.parent()?;
        // A subsequent index consumes this application's result. Its outer
        // assignment/call context must not select a setter on the inner callee.
        if (parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(expression))
            || (parent.kind() == "call_statement"
                && parent.child_by_field_name("callee") == Some(expression))
        {
            return Some(PropertyAccess::Get);
        }
    }
    if matches!(
        parent.kind(),
        "assignment_statement" | "let_statement" | "set_statement"
    ) {
        return if parent.child_by_field_name("left") == Some(expression) {
            Some(if parent.kind() == "set_statement" {
                PropertyAccess::Set
            } else {
                PropertyAccess::Let
            })
        } else if parent.child_by_field_name("right") == Some(expression) {
            Some(PropertyAccess::Get)
        } else {
            None
        };
    }
    // These positions evaluate the member's value. Unknown statement targets
    // must not default to Get (for example a loop control or ReDim operand).
    if matches!(
        parent.kind(),
        "binary_expression"
            | "comparison_expression"
            | "unary_expression"
            | "parenthesized_expression"
            | "logical_value_expression"
            | "condition_binary_expression"
            | "argument_list"
            | "unparenthesized_argument_list"
    ) || parent.child_by_field_name("condition") == Some(expression)
        || (matches!(
            parent.kind(),
            "named_argument" | "with_statement" | "inline_with_statement"
        ) && parent.child_by_field_name("value") == Some(expression))
        || (parent.kind() == "qualified_member_expression"
            && parent.child_by_field_name("receiver") == Some(expression))
    {
        Some(PropertyAccess::Get)
    } else {
        None
    }
}

pub(super) fn header_end(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        // A named newline terminates the header; escaped physical newlines are
        // grammar extras and must not truncate a continued parameter list.
        if child.kind() == "newline" || node.child_by_field_name("body") == Some(child) {
            return Ok(child.start_byte());
        }
    }
    Ok(node.end_byte())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_grouped_calls_do_not_depend_on_native_partition() {
        let family = super::super::GrammarFamily::Vba;
        let pack = super::super::QueryPack::compile(family, include_str!("../../queries/vba.scm"))
            .unwrap();
        for newline in ["\n", "\r\n"] {
            for prefix in ["", "Earlier Nothing\n"] {
                for (body, expected) in [
                    ("Target (Nothing)", Some("vba.invoked_identifier")),
                    ("Target\t((value))", Some("vba.invoked_identifier")),
                    ("Target (value + 1)", Some("vba.invoked_identifier")),
                    (
                        "Target (Inner(value, other))",
                        Some("vba.invoked_identifier"),
                    ),
                    ("Target (\"a,b\")", Some("vba.invoked_identifier")),
                    ("Target _\n (Nothing)", Some("vba.invoked_identifier")),
                    ("Target(Nothing)", Some("vba.invoked_identifier")),
                    ("Target(value)", Some("vba.invoked_identifier")),
                    ("Target((value))", Some("vba.invoked_identifier")),
                    ("Target(value + 1)", Some("vba.invoked_identifier")),
                    (
                        "Target(Inner(value, other))",
                        Some("vba.invoked_identifier"),
                    ),
                    ("Target(\"a,b\")", Some("vba.invoked_identifier")),
                    ("Target()", None),
                    ("Target(Nothing, value)", None),
                    ("Target(value,)", None),
                    ("Target(value:=1)", None),
                    ("Target(ByVal value)", None),
                    ("Target(value) extra", None),
                    ("Target(value).Member", None),
                    ("Target ()", None),
                    ("Target (Nothing, value)", None),
                    ("Target (value,)", None),
                    ("Target (value:=1)", None),
                    ("Target (ByVal value)", None),
                    ("Target (value) extra", None),
                    ("Target (value).Member", None),
                    ("Call Target(Nothing)", Some("vba.invoked_identifier")),
                    (
                        "sink = Target (Nothing)",
                        Some("vba.invoked_get_identifier"),
                    ),
                    ("sink = Target(Nothing)", Some("vba.invoked_get_identifier")),
                    (
                        "Earlier _\n Target (Nothing)",
                        Some("vba.invoked_get_identifier"),
                    ),
                    (
                        "Earlier _\n Target(Nothing)",
                        Some("vba.invoked_get_identifier"),
                    ),
                    (
                        "sink = _\n Target (Nothing)",
                        Some("vba.invoked_get_identifier"),
                    ),
                    (
                        "sink = _\n Target(Nothing)",
                        Some("vba.invoked_get_identifier"),
                    ),
                    (
                        "Call Earlier _\n (Target (Nothing))",
                        Some("vba.invoked_get_identifier"),
                    ),
                    (
                        "Call Earlier _\n (Target(Nothing))",
                        Some("vba.invoked_get_identifier"),
                    ),
                    (
                        "' continuation-like comment _\nTarget (Nothing)",
                        Some("vba.invoked_identifier"),
                    ),
                    (
                        "' continuation-like comment _\nTarget(Nothing)",
                        Some("vba.invoked_identifier"),
                    ),
                ] {
                    let source =
                        format!("Sub Run()\n{prefix}{body}\nEnd Sub\n").replace('\n', newline);
                    let mut parser = tree_sitter::Parser::new();
                    parser
                        .set_language(&tree_sitter_vba::LANGUAGE.into())
                        .unwrap();
                    let tree = parser.parse(&source, None).unwrap();
                    assert!(!tree.root_node().has_error(), "{source}");
                    let extraction = pack
                        .extract(
                            family,
                            &tree,
                            source.as_bytes(),
                            10_000,
                            1024,
                            &Cancellation::new(),
                        )
                        .unwrap();
                    assert!(extraction.limit.is_none());
                    let start = source.find("Target").unwrap();
                    let references: Vec<_> = extraction
                        .candidates
                        .iter()
                        .filter(|candidate| {
                            candidate.role == StructuralRole::Reference
                                && candidate.start == start
                                && candidate.end == start + 6
                        })
                        .collect();
                    assert_eq!(references.len(), 1, "{source}: {references:?}");
                    if let Some(expected) = expected {
                        assert_eq!(
                            references[0].syntax,
                            expected,
                            "{source}: {}",
                            tree.root_node().to_sexp()
                        );
                        if expected == "vba.invoked_get_identifier" {
                            let name = tree
                                .root_node()
                                .named_descendant_for_byte_range(start, start + 6)
                                .unwrap();
                            assert!(
                                !single_grouped_statement_call(
                                    name,
                                    source.as_bytes(),
                                    &Cancellation::new(),
                                )
                                .unwrap(),
                                "{source}: {}",
                                tree.root_node().to_sexp()
                            );
                        }
                    } else {
                        let name = tree
                            .root_node()
                            .named_descendant_for_byte_range(start, start + 6)
                            .unwrap();
                        assert_eq!(name.kind(), "identifier");
                        assert!(
                            !single_grouped_statement_call(
                                name,
                                source.as_bytes(),
                                &Cancellation::new(),
                            )
                            .unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn member_access_modes_follow_value_and_assignment_contexts() {
        for (statement, expected) in [
            ("a = holder.Value", "vba.get_member"),
            ("holder.Value = 1", "vba.let_member"),
            ("Let holder.Value = 1", "vba.let_member"),
            ("Set holder.Value = other", "vba.set_member"),
            ("a = holder.Value(1)", "vba.invoked_get_member"),
            ("a = holder.Value(1)(2)", "vba.invoked_get_member"),
            ("holder.Value(1)(2) = 3", "vba.invoked_get_member"),
            ("Set holder.Value(1)(2) = other", "vba.invoked_get_member"),
            ("Call holder.Value(1)(2)", "vba.invoked_get_member"),
            ("Use value:=holder.Value", "vba.get_member"),
            ("With holder.Value\nEnd With", "vba.get_member"),
            ("holder.Value", "vba.invoked_member"),
            ("ReDim holder.Value(1)", "vba.member"),
        ] {
            let source = format!("Sub Run()\n{statement}\nEnd Sub\n");
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_vba::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(&source, None).unwrap();
            let family = super::super::GrammarFamily::Vba;
            let pack =
                super::super::QueryPack::compile(family, include_str!("../../queries/vba.scm"))
                    .unwrap();
            let extraction = pack
                .extract(
                    family,
                    &tree,
                    source.as_bytes(),
                    1024,
                    1024,
                    &Cancellation::new(),
                )
                .unwrap();
            let labels: Vec<_> = extraction
                .candidates
                .iter()
                .filter(|candidate| {
                    candidate.role == StructuralRole::Reference
                        && candidate.syntax != "vba.application"
                        && source.get(candidate.start..candidate.end) == Some("holder.Value")
                })
                .map(|candidate| candidate.syntax)
                .collect();
            assert_eq!(
                labels,
                [expected],
                "{statement}: {}",
                tree.root_node().to_sexp()
            );
        }
    }

    #[test]
    fn procedure_pointer_capture_retains_the_complete_qualified_target() {
        let source = "Sub Run()\nAccept AddressOf Callbacks _\n.Notify\nEnd Sub\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_vba::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let family = super::super::GrammarFamily::Vba;
        let pack = super::super::QueryPack::compile(family, include_str!("../../queries/vba.scm"))
            .unwrap();
        let extraction = pack
            .extract(
                family,
                &tree,
                source.as_bytes(),
                1024,
                1024,
                &Cancellation::new(),
            )
            .unwrap();
        let expected = "Callbacks _\n.Notify";
        assert!(
            extraction.candidates.iter().any(|candidate| {
                candidate.syntax == "vba.qualified_procedure_pointer"
                    && source.get(candidate.start..candidate.end) == Some(expected)
            }),
            "{} {:#?}",
            tree.root_node().to_sexp(),
            extraction.candidates
        );
    }

    #[test]
    fn automatic_type_capture_requires_a_scalar_variable_or_field() {
        for (source, expected) in [
            (
                "Sub Run()\nDim receiver As New Worker\nEnd Sub\n",
                "vba.automatic_type_identifier",
            ),
            (
                "Sub Run()\nDim receiver As _\r\n New Worker\nEnd Sub\n",
                "vba.automatic_type_identifier",
            ),
            (
                "Private receiver As New Worker\n",
                "vba.automatic_type_identifier",
            ),
            (
                "Type Holder\nreceiver As New Worker\nEnd Type\n",
                "vba.automatic_type_identifier",
            ),
            (
                "Sub Run(ByRef receiver As New Worker)\nEnd Sub\n",
                "vba.unsupported_automatic_type_identifier",
            ),
            (
                "Dim receiver() As New Worker\n",
                "vba.unsupported_automatic_type_identifier",
            ),
            (
                "Dim receiver As New Worker()\n",
                "vba.unsupported_automatic_type_identifier",
            ),
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_vba::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            let family = super::super::GrammarFamily::Vba;
            let pack =
                super::super::QueryPack::compile(family, include_str!("../../queries/vba.scm"))
                    .unwrap();
            let extraction = pack
                .extract(
                    family,
                    &tree,
                    source.as_bytes(),
                    1024,
                    1024,
                    &Cancellation::new(),
                )
                .unwrap();
            let labels: Vec<_> = extraction
                .candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Reference)
                .map(|candidate| candidate.syntax)
                .collect();
            assert_eq!(
                labels,
                [expected],
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
    }

    #[test]
    fn declared_type_capture_distinguishes_arrays_and_automatic_objects() {
        for (declaration, direct) in [
            ("point As Coordinate", true),
            ("point As _\r\n Coordinate", true),
            ("point As New Coordinate", false),
            ("point As _\r\n New Coordinate", false),
            ("point() As Coordinate", false),
            ("point As Coordinate()", false),
        ] {
            let source = format!("Sub Run(ByRef {declaration})\nEnd Sub\n");
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_vba::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(&source, None).unwrap();
            let family = super::super::GrammarFamily::Vba;
            let pack =
                super::super::QueryPack::compile(family, include_str!("../../queries/vba.scm"))
                    .unwrap();
            let extraction = pack
                .extract(
                    family,
                    &tree,
                    source.as_bytes(),
                    1024,
                    1024,
                    &Cancellation::new(),
                )
                .unwrap();
            let labels: Vec<_> = extraction
                .candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Reference)
                .map(|candidate| candidate.syntax)
                .collect();
            let clause = tree
                .root_node()
                .named_child(0)
                .unwrap()
                .child_by_field_name("parameters")
                .unwrap()
                .named_child(0)
                .unwrap()
                .child_by_field_name("type")
                .unwrap();
            let mut cursor = clause.walk();
            let parts: Vec<_> = clause
                .children(&mut cursor)
                .map(|node| (node.kind(), source.get(node.byte_range())))
                .collect();
            assert_eq!(
                labels.contains(&"vba.direct_type_identifier"),
                direct,
                "{source}: {} {labels:?} {parts:?}",
                tree.root_node().to_sexp()
            );
        }
    }

    #[test]
    fn class_identity_requires_an_exact_leading_export_preamble() {
        for (source, expected) in [
            ("VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\n", true),
            (
                "' export\r\n\r\nversion\t1.0\tclass\r\nOption Explicit\r\n",
                true,
            ),
            ("VERSION 1.0 CLASS ' export\nOption Explicit\n", true),
            ("VERSION 1.0\nAttribute VB_Name = \"Counter\"\n", false),
            ("VERSION 1.0 Form\n", false),
            ("VERSION 2.0 CLASS\n", false),
            ("VERSION 1.0 CLASS Extra\n", false),
            ("VERSION 1.0 CLASS\nVERSION 1.0 CLASS\n", false),
            (
                "Attribute VB_Name = \"Counter\"\nVERSION 1.0 CLASS\n",
                false,
            ),
            ("Sub Run()\nVERSION 1.0 CLASS\nEnd Sub\n", false),
            (
                "Private label As String\nlabel = \"VERSION 1.0 CLASS\"\n",
                false,
            ),
            ("' VERSION 1.0 CLASS\nOption Explicit\n", false),
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_vba::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert_eq!(
                class_header(tree.root_node(), source.as_bytes(), &Cancellation::new())
                    .unwrap()
                    .is_some(),
                expected,
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
    }

    #[test]
    fn exported_header_selects_the_last_module_name() {
        let source =
            b"Attribute VB_Name = \"Old\"\nAttribute VB_Name = \"Current\"\nOption Explicit\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_vba::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let name = module_name_node(tree.root_node(), source, &Cancellation::new()).unwrap();
        assert_eq!(
            name.and_then(|node| source.get(node.byte_range())),
            Some(b"\"Current\"".as_slice()),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            rootlight_adapter_sdk::vba_module_name("\"Current\""),
            Some("Current")
        );
        let family = super::super::GrammarFamily::Vba;
        let pack = super::super::QueryPack::compile(family, include_str!("../../queries/vba.scm"))
            .unwrap();
        let extraction = pack
            .extract(family, &tree, source, 1024, 1024, &Cancellation::new())
            .unwrap();
        assert!(
            extraction
                .candidates
                .iter()
                .any(|candidate| candidate.role == StructuralRole::Definition
                    && candidate.syntax == "vba.module_name"),
            "{:?}",
            extraction.candidates
        );
    }
}
