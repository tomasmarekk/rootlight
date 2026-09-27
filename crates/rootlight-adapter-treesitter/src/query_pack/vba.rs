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
    // argument compatibility and dispatch through returned values are separate.
    if role != StructuralRole::Reference || !matches!(node.kind(), "identifier" | "bang_identifier")
    {
        return true;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
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
            "vba.application"
        }
        "new_expression" if role == StructuralRole::Reference => "vba.construction",
        "comment" => "vba.comment",
        "string_literal" => "vba.string",
        _ => return None,
    })
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
    if let Some(ty) = child
        .parent()
        .filter(|node| node.kind() == "type_expression")
    {
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
            return Ok(
                if child.kind() == "identifier"
                    && parent.child_by_field_name("type") == Some(child)
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
                    && !procedure_pointer_has_qualifier(child, source, cancellation)?
                {
                    "vba.procedure_pointer"
                } else {
                    "vba.unsupported_procedure_pointer"
                },
            );
        }
        if matches!(
            kind,
            "type_expression"
                | "dotted_type_expression"
                | "implements_statement"
                | "preprocessor_const"
                | "goto_statement"
                | "gosub_statement"
                | "on_error_statement"
                | "resume_statement"
        ) || (instance && kind == "const_declarator")
            || (kind == "named_argument" && parent.child_by_field_name("name") == Some(child))
            || (kind == "on_goto_statement"
                && parent.child_by_field_name("selector") != Some(child))
            || (matches!(
                kind,
                "preprocessor_if"
                    | "preprocessor_elseif"
                    | "type_preprocessor_if"
                    | "type_preprocessor_elseif"
            ) && parent.child_by_field_name("condition") == Some(child))
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

fn procedure_pointer_has_qualifier(
    target: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut continued = false;
    for (offset, byte) in source
        .get(target.end_byte()..)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        if offset % 512 == 0 {
            cancellation.check()?;
        }
        match byte {
            b' ' | b'\t' | b'\r' => {}
            b'_' if !continued => continued = true,
            b'\n' if continued => continued = false,
            b'.' | b'!' => return Ok(true),
            _ => return Ok(false),
        }
    }
    Ok(false)
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
        "vba.invoked_identifier" | "vba.invoked_get_identifier"
            if node.kind() == "qualified_member_expression" =>
        {
            "vba.invoked_member"
        }
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

fn property_access(mut expression: Node<'_>) -> Option<PropertyAccess> {
    let mut parent = expression.parent()?;
    if parent.kind() == "call_expression"
        && parent.child_by_field_name("function") == Some(expression)
    {
        expression = parent;
        parent = expression.parent()?;
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
            | "argument_list"
            | "unparenthesized_argument_list"
    ) || parent.child_by_field_name("condition") == Some(expression)
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
