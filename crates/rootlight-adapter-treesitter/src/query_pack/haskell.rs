//! Haskell selectors retain written names and body-independent declaration headers.
//! Shared native node kinds need context checks before they can describe bindings.

use std::ops::Range;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn equation(node: Node<'_>) -> bool {
    matches!(node.kind(), "function" | "bind")
        && node.child_by_field_name("match").is_some()
        && node.child_by_field_name("arrow").is_none()
}

fn ordinary_prefix_equation(node: Node<'_>) -> bool {
    node.kind() == "function"
        && equation(node)
        && !node.has_error()
        && !node.is_missing()
        && node
            .child_by_field_name("name")
            .is_some_and(|name| name.kind() == "variable")
        && node.child_by_field_name("patterns").is_some()
        && node.child_by_field_name("binds").is_none()
        && node.child_by_field_name("parens").is_none()
        && node.parent().is_some_and(|declarations| {
            declarations.kind() == "declarations"
                && declarations
                    .parent()
                    .is_some_and(|root| root.kind() == "haskell")
        })
}

fn simple_binary_infix(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let (Some(left), Some(operator), Some(right)) = (
        node.child_by_field_name("left_operand"),
        node.child_by_field_name("operator"),
        node.child_by_field_name("right_operand"),
    ) else {
        return Ok(false);
    };
    if node.kind() != "infix"
        || node.has_error()
        || [left, operator, right]
            .into_iter()
            .any(|child| child.is_missing() || child.start_byte() == child.end_byte())
        || left.end_byte() > operator.start_byte()
        || operator.end_byte() > right.start_byte()
    {
        return Ok(false);
    }
    let written_operator = match operator.kind() {
        // Literal star and hash-head tokens are unnamed operator children;
        // named descendants would introduce a shape outside symbolic dispatch.
        "operator" => operator.named_child_count() == 0,
        "infix_id" if operator.named_child_count() == 1 => operator
            .named_child(0)
            .is_some_and(|name| name.kind() == "variable" && name.child_count() == 0),
        _ => false,
    };
    if !written_operator {
        return Ok(false);
    }
    // The grammar shares infix nodes with patterns and types, including repeated
    // operator fields. The closed RHS walk admits only this exact binary shape.
    let mut cursor = node.walk();
    let mut seen = 0u8;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        let bit = if child == left {
            1
        } else if child == operator {
            2
        } else if child == right {
            4
        } else {
            return Ok(false);
        };
        if seen & bit != 0 {
            return Ok(false);
        }
        seen |= bit;
    }
    Ok(seen == 7)
}

fn simple_function_clause(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if !ordinary_prefix_equation(node) {
        return Ok(false);
    }
    let Some(patterns) = node.child_by_field_name("patterns") else {
        return Ok(false);
    };
    let mut cursor = patterns.walk();
    let mut formals = 0usize;
    for pattern in patterns.named_children(&mut cursor) {
        cancellation.check()?;
        if pattern.is_extra() {
            continue;
        }
        if pattern.kind() != "variable"
            || pattern.is_missing()
            || pattern.start_byte() == pattern.end_byte()
        {
            return Ok(false);
        }
        formals = formals
            .checked_add(1)
            .ok_or_else(|| super::query_failure("haskell-formal-count"))?;
    }
    if formals == 0 {
        return Ok(false);
    }
    let Some(matched) = node.child_by_field_name("match") else {
        return Ok(false);
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == "where" || child.kind() == "match" && child != matched {
            return Ok(false);
        }
        if child.is_named()
            && !child.is_extra()
            && Some(child) != node.child_by_field_name("name")
            && child != patterns
            && child != matched
        {
            return Ok(false);
        }
    }
    if matched.child_by_field_name("guards").is_some() {
        return Ok(false);
    }
    let Some(expression) = matched.child_by_field_name("expression") else {
        return Ok(false);
    };
    // This eligibility marker is projected once per clause. Reads do not repeatedly
    // scan siblings to infer absent binders; every unsupported RHS node closes it.
    let mut cursor = expression.walk();
    loop {
        cancellation.check()?;
        let current = cursor.node();
        let descend = if !current.is_named() || current.is_extra() {
            false
        } else {
            match current.kind() {
                "variable" | "integer" | "float" | "char" | "string" => false,
                "literal" if current.named_child_count() == 1 => true,
                "infix" if simple_binary_infix(current, cancellation)? => true,
                // A backticked formal is dispatch syntax, not an operand read.
                "operator" | "infix_id"
                    if current.parent().is_some_and(|parent| {
                        parent.kind() == "infix"
                            && parent.child_by_field_name("operator") == Some(current)
                    }) =>
                {
                    false
                }
                "apply"
                    if current.child_by_field_name("function").is_some()
                        && current.child_by_field_name("argument").is_some() =>
                {
                    true
                }
                "parens" if current.child_by_field_name("expression").is_some() => true,
                "conditional"
                    if current.child_by_field_name("if").is_some()
                        && current.child_by_field_name("then").is_some()
                        && current.child_by_field_name("else").is_some() =>
                {
                    true
                }
                _ => return Ok(false),
            }
        };
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            cancellation.check()?;
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(true);
            }
        }
    }
}

fn equation_rhs_read(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() != "variable" || node.is_missing() || node.start_byte() == node.end_byte() {
        return Ok(false);
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "parens" if parent.child_by_field_name("expression") == Some(current) => {}
            "infix"
                if parent.child_by_field_name("left_operand") == Some(current)
                    || parent.child_by_field_name("right_operand") == Some(current) => {}
            "conditional"
                if parent.child_by_field_name("if") == Some(current)
                    || parent.child_by_field_name("then") == Some(current)
                    || parent.child_by_field_name("else") == Some(current) => {}
            "apply"
                if parent.child_by_field_name("argument") == Some(current)
                    || current.kind() == "apply"
                        && parent.child_by_field_name("function") == Some(current) => {}
            "match"
                if parent.child_by_field_name("expression") == Some(current)
                    && parent.child_by_field_name("guards").is_none() =>
            {
                return Ok(parent.parent().is_some_and(ordinary_prefix_equation));
            }
            _ => return Ok(false),
        }
        current = parent;
    }
    Ok(false)
}

fn declaration_signature(node: Node<'_>) -> bool {
    node.kind() == "signature"
        && (node.child_by_field_name("name").is_some()
            || node.child_by_field_name("names").is_some())
        && node.child_by_field_name("expression").is_none()
        && node.child_by_field_name("pattern").is_none()
}

fn direct_class_signature(node: Node<'_>) -> bool {
    if !declaration_signature(node) || node.has_error() || node.is_missing() {
        return false;
    }
    let Some(declarations) = node
        .parent()
        .filter(|parent| parent.kind() == "class_declarations")
    else {
        return false;
    };
    declarations.parent().is_some_and(|class| {
        class.kind() == "class"
            && !class.has_error()
            && class.child_by_field_name("declarations") == Some(declarations)
    })
}

fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

fn field_contains(node: Node<'_>, field: &str, inner: Node<'_>) -> bool {
    node.child_by_field_name(field)
        .is_some_and(|child| contains(child, inner))
}

fn unwrapped_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Node<'tree>, AdapterError> {
    while matches!(node.kind(), "prefix_id" | "infix_id" | "field_name") {
        cancellation.check()?;
        let Some(child) = node.named_child(0) else {
            break;
        };
        node = child;
    }
    Ok(node)
}

fn terminal_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    loop {
        cancellation.check()?;
        match node.kind() {
            "prefix_id" | "infix_id" | "field_name" => {
                let Some(child) = node.named_child(0) else {
                    return Ok(None);
                };
                node = child;
            }
            "variable"
            | "operator"
            | "constructor_operator"
            | "constructor"
            | "name"
            | "module" => {
                return Ok(
                    (!node.is_missing() && node.start_byte() < node.end_byte()).then_some(node)
                );
            }
            _ => return Ok(None),
        }
    }
}

fn head_child<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if matches!(child.kind(), "infix" | "parens" | "function_head_parens") {
            return Ok(Some(child));
        }
        if matches!(
            child.kind(),
            "match"
                | "local_binds"
                | "data_constructors"
                | "gadt_constructors"
                | "class_declarations"
                | "instance_declarations"
        ) {
            break;
        }
    }
    Ok(None)
}

fn selected_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    loop {
        cancellation.check()?;
        if node.kind() == "header" {
            return Ok(node.child_by_field_name("module"));
        }
        if let Some(name) = node.child_by_field_name("name") {
            return terminal_name(name, cancellation);
        }
        let next = match node.kind() {
            "function" | "function_head_parens" => node
                .child_by_field_name("parens")
                .or(head_child(node, cancellation)?),
            "bind" => node.child_by_field_name("pattern"),
            "signature" => node.child_by_field_name("pattern"),
            "data_constructor" => node.child_by_field_name("constructor"),
            "infix" => node.child_by_field_name("operator"),
            "data_type" | "newtype" | "type_synomym" | "class" | "instance" => {
                head_child(node, cancellation)?
            }
            "parens" => node.named_child(0),
            _ => return terminal_name(node, cancellation),
        };
        let Some(next) = next else {
            return Ok(None);
        };
        node = next;
    }
}

fn named_declaration(node: Node<'_>) -> bool {
    match node.kind() {
        "function" | "bind" => equation(node),
        "header"
        | "data_type"
        | "newtype"
        | "type_synomym"
        | "class"
        | "data_constructor"
        | "newtype_constructor"
        | "gadt_constructor" => true,
        _ => false,
    }
}

fn data_instance(node: Node<'_>) -> bool {
    matches!(node.kind(), "data_type" | "newtype")
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "data_instance")
}

fn binding(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let name = unwrapped_name(node, cancellation)?;
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        if named_declaration(parent) && selected_name(parent, cancellation)? == Some(node) {
            // Whole declarations own their head capture; leaf candidates must not duplicate it.
            return Ok(None);
        }
        if declaration_signature(parent) {
            return Ok((field_contains(parent, "name", node)
                || field_contains(parent, "names", node))
            .then_some(if direct_class_signature(parent) {
                "haskell.signature_member_name"
            } else {
                "haskell.signature_name"
            }));
        }
        if parent.kind() == "signature" && !field_contains(parent, "pattern", node) {
            return Ok(None);
        }
        if parent.kind() == "gadt_constructor" && field_contains(parent, "names", node) {
            return Ok(Some("haskell.constructor"));
        }
        if parent.kind() == "field_pattern"
            && parent.child_by_field_name("pattern").is_some()
            && field_contains(parent, "field", node)
        {
            // An explicit record selector names the field, not the pattern's binder.
            return Ok(None);
        }
        if parent.kind() == "view_pattern" && !field_contains(parent, "pattern", node) {
            return Ok(None);
        }
        if parent.kind() == "qualified" {
            return Ok(None);
        }
        if name.kind() == "variable" {
            match parent.kind() {
                "field"
                    if current.kind() == "field_name" || field_contains(parent, "name", node) =>
                {
                    return Ok(Some("haskell.field"));
                }
                "patterns" => {
                    return Ok(parent.parent().and_then(|owner| match owner.kind() {
                        "function" if equation(owner) => Some("haskell.parameter"),
                        "function_head_parens" | "lambda" => Some("haskell.parameter"),
                        "alternative" => Some("haskell.variable"),
                        _ => None,
                    }));
                }
                "function" if equation(parent) => {
                    let head = head_child(parent, cancellation)?;
                    return Ok(head
                        .filter(|head| contains(*head, node))
                        .map(|_| "haskell.parameter"));
                }
                "bind" => {
                    return Ok(
                        field_contains(parent, "pattern", node).then_some("haskell.variable")
                    );
                }
                "generator" | "alternative" => {
                    return Ok((field_contains(parent, "pattern", node)
                        || field_contains(parent, "patterns", node))
                    .then_some("haskell.variable"));
                }
                _ => {}
            }
        }
        if !matches!(
            parent.kind(),
            "prefix_id"
                | "infix_id"
                | "field_name"
                | "binding_list"
                | "function_head_parens"
                | "infix"
                | "apply"
                | "parens"
                | "tuple"
                | "unboxed_tuple"
                | "unboxed_sum"
                | "list"
                | "as"
                | "record"
                | "field_pattern"
                | "view_pattern"
                | "strict"
                | "irrefutable"
                | "signature"
        ) {
            return Ok(None);
        }
        current = parent;
    }
    Ok(None)
}

fn definition_head(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let Some(name) = terminal_name(node, cancellation)? else {
        return Ok(false);
    };
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        if named_declaration(parent) && selected_name(parent, cancellation)? == Some(name) {
            return Ok(!data_instance(parent));
        }
        if !matches!(
            parent.kind(),
            "prefix_id"
                | "infix_id"
                | "function_head_parens"
                | "infix"
                | "prefix"
                | "record"
                | "parens"
        ) {
            return Ok(false);
        }
        current = parent;
    }
    Ok(false)
}

fn outer_application(node: Node<'_>) -> bool {
    !node.parent().is_some_and(|parent| {
        parent.kind() == "apply" && parent.child_by_field_name("function") == Some(node)
    })
}

fn callee<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    loop {
        cancellation.check()?;
        let next = match node.kind() {
            "apply" => node.child_by_field_name("function"),
            "infix" => node.child_by_field_name("operator"),
            "parens" => node.child_by_field_name("expression"),
            "qualified" => node.child_by_field_name("id"),
            _ => return terminal_name(node, cancellation),
        };
        let Some(next) = next else {
            return Ok(None);
        };
        node = next;
    }
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if matches!(role, StructuralRole::Call | StructuralRole::CallName) {
        return Ok(outer_application(node)
            && (role != StructuralRole::CallName || callee(node, cancellation)?.is_some()));
    }
    if matches!(node.kind(), "function" | "bind") {
        return Ok(match role {
            StructuralRole::Scope => {
                equation(node)
                    || node.kind() == "bind" && node.child_by_field_name("arrow").is_some()
            }
            StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature => {
                equation(node) && selected_name(node, cancellation)?.is_some()
            }
            _ => true,
        });
    }
    if role == StructuralRole::Signature {
        return Ok(match node.kind() {
            "signature" => declaration_signature(node),
            "match" => match node.parent() {
                Some(clause) => simple_function_clause(clause, cancellation)?,
                None => false,
            },
            "field" => node.child_by_field_name("name").is_some(),
            _ => true,
        });
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) {
        return if named_declaration(node) {
            Ok(!data_instance(node) && selected_name(node, cancellation)?.is_some())
        } else {
            Ok(binding(node, cancellation)?.is_some())
        };
    }
    if role == StructuralRole::Reference {
        if node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "qualified" | "prefix_id" | "infix_id" | "field_name"
            )
        }) {
            return Ok(false);
        }
        if node.kind() == "field_name"
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "field_pattern")
        {
            return Ok(true);
        }
        return Ok(binding(node, cancellation)?.is_none() && !definition_head(node, cancellation)?);
    }
    Ok(true)
}

fn reference_syntax(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if equation_rhs_read(node, cancellation)? {
        return Ok("haskell.equation_rhs_read");
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "import" | "import_name" | "import_list" => return Ok("haskell.import_name"),
            "exports" | "export" | "module_export" => return Ok("haskell.export_name"),
            "instance"
                if field_contains(parent, "name", node)
                    || selected_name(parent, cancellation)?
                        .is_some_and(|name| contains(node, name)) =>
            {
                return Ok("haskell.instance_class_name");
            }
            "type_params" | "type_patterns" | "type_application" | "explicit_type" | "context"
            | "constraint" | "forall" | "forall_required" => {
                return Ok("haskell.type_name");
            }
            _ => {}
        }
        if !matches!(
            parent.kind(),
            "prefix_id" | "infix_id" | "parens" | "binding_list" | "infix"
        ) {
            break;
        }
        current = parent;
    }
    if node.kind() == "field_name" {
        return Ok("haskell.field_name");
    }
    Ok(match unwrapped_name(node, cancellation)?.kind() {
        "qualified" => "haskell.qualified_name",
        "module" => "haskell.module_name",
        "field_name" => "haskell.field_name",
        "constructor" | "constructor_operator" => "haskell.constructor_name",
        "name" => "haskell.type_name",
        "operator" => "haskell.operator_name",
        _ => "haskell.identifier",
    })
}

pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    _source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    if role == StructuralRole::Definition {
        return Ok(Some("haskell.binding_name"));
    }
    if role == StructuralRole::Declaration && !named_declaration(node) {
        return binding(node, cancellation);
    }
    if role == StructuralRole::Reference {
        return reference_syntax(node, cancellation).map(Some);
    }
    if role == StructuralRole::CallName {
        return Ok(Some("haskell.application_name"));
    }
    if role == StructuralRole::Signature {
        return Ok(Some(match node.kind() {
            "function" => "haskell.function_header",
            "match" => "haskell.simple_equation_match",
            "bind" => "haskell.binding_header",
            "signature" => "haskell.written_signature",
            "field" => "haskell.field_signature",
            "instance" => "haskell.instance_header",
            "lambda" => "haskell.lambda_header",
            _ => "haskell.declaration_header",
        }));
    }
    Ok(Some(match node.kind() {
        "haskell" => "haskell.file",
        "header" => "haskell.module",
        "import" => "haskell.import",
        "function" if role == StructuralRole::Scope => "haskell.function_clause",
        "function" => "haskell.function",
        "bind" if node.child_by_field_name("arrow").is_some() => "haskell.do_bind",
        "bind" if role == StructuralRole::Scope && selected_name(node, cancellation)?.is_none() => {
            "haskell.pattern_bind"
        }
        "bind" if role == StructuralRole::Scope => "haskell.variable_binding",
        "bind" => "haskell.variable",
        "data_type" | "newtype" if data_instance(node) => "haskell.data_instance",
        "data_type" => "haskell.data_type",
        "newtype" => "haskell.newtype",
        "type_synomym" => "haskell.type_synonym",
        "class" => "haskell.class",
        "instance" => "haskell.instance",
        "data_constructor" | "newtype_constructor" | "gadt_constructor" => "haskell.constructor",
        "local_binds"
            if node.parent().is_some_and(|parent| {
                matches!(parent.kind(), "function" | "bind" | "alternative")
            }) =>
        {
            "haskell.where_binds"
        }
        "local_binds" => "haskell.let_binds",
        "let" | "let_in" => "haskell.let",
        "lambda" | "lambda_case" | "lambda_cases" => "haskell.lambda",
        "alternative" => "haskell.alternative",
        "do" | "rec" => "haskell.do",
        "list_comprehension" => "haskell.list_comprehension",
        "generator" => "haskell.generator",
        "apply" => "haskell.application",
        "infix" => "haskell.infix_application",
        "comment" => "haskell.comment",
        "haddock" => "haskell.documentation",
        "string" => "haskell.string",
        _ => return Ok(None),
    }))
}

pub(super) fn capture_range(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    cancellation.check()?;
    if role == StructuralRole::Definition {
        return Ok(selected_name(node, cancellation)?.map(|name| name.byte_range()));
    }
    if role == StructuralRole::CallName {
        return Ok(callee(node, cancellation)?.map(|name| name.byte_range()));
    }
    if role == StructuralRole::Reference {
        return Ok(Some(unwrapped_name(node, cancellation)?.byte_range()));
    }
    if role != StructuralRole::Signature || node.kind() == "match" {
        return Ok(Some(node.byte_range()));
    }
    let mut end = node.end_byte();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if matches!(
            child.kind(),
            "match"
                | "local_binds"
                | "data_constructors"
                | "gadt_constructors"
                | "class_declarations"
                | "instance_declarations"
                | "newtype_constructor"
                | "where"
                | "="
        ) || node.kind() == "lambda"
            && (matches!(child.kind(), "->" | "→")
                || node.child_by_field_name("expression") == Some(child))
        {
            end = child.start_byte();
            break;
        }
    }
    Ok((node.start_byte() < end).then_some(node.start_byte()..end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_haskell::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        tree
    }

    fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
        let mut output = Vec::new();
        let mut cursor = root.walk();
        loop {
            if cursor.node().kind() == kind {
                output.push(cursor.node());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return output;
                }
            }
        }
    }

    #[test]
    fn native_query_preserves_all_required_capture_roles() {
        let query = tree_sitter::Query::new(
            &tree_sitter_haskell::LANGUAGE.into(),
            include_str!("../../queries/haskell.scm"),
        )
        .unwrap();
        let mut actual = query
            .capture_names()
            .iter()
            .copied()
            .filter(|name| *name != "call_name")
            .collect::<Vec<_>>();
        actual.sort_unstable();
        let mut expected = super::super::EXPECTED_CAPTURES.to_vec();
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn shared_native_kinds_distinguish_equations_annotations_and_do_binds() {
        let source = "module Example where\ntyped :: Int -> Int\ntyped value = value\nannotated = (typed 1 :: Int)\naction = do\n  result <- typed 1\n  pure result\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let functions = nodes(tree.root_node(), "function");
        assert!(functions.iter().any(|node| equation(*node)));
        assert!(
            functions
                .iter()
                .any(|node| node.child_by_field_name("arrow").is_some())
        );
        for node in functions {
            for role in [StructuralRole::Declaration, StructuralRole::Scope] {
                assert_eq!(
                    retain_capture(node, role, &cancellation).unwrap(),
                    equation(node)
                );
            }
        }
        let signatures = nodes(tree.root_node(), "signature");
        assert!(signatures.iter().any(|node| declaration_signature(*node)));
        assert!(
            signatures
                .iter()
                .any(|node| node.child_by_field_name("expression").is_some())
        );
        for node in signatures {
            assert_eq!(
                retain_capture(node, StructuralRole::Signature, &cancellation).unwrap(),
                declaration_signature(node)
            );
        }
        let binds = nodes(tree.root_node(), "bind");
        let do_bind = binds
            .iter()
            .find(|node| node.child_by_field_name("arrow").is_some())
            .unwrap();
        assert!(!retain_capture(*do_bind, StructuralRole::Declaration, &cancellation).unwrap());
        assert!(retain_capture(*do_bind, StructuralRole::Scope, &cancellation).unwrap());
    }

    #[test]
    fn terminal_operator_definitions_exclude_wrappers_and_body_headers() {
        let source = "module Example where\n(/) left right = left\nleft <+> right = right\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let functions = nodes(tree.root_node(), "function");
        assert_eq!(functions.len(), 2);
        for (node, name, header) in functions
            .into_iter()
            .zip(["/", "<+>"])
            .zip(["(/) left right", "left <+> right"])
            .map(|((node, name), header)| (node, name, header))
        {
            let definition = capture_range(node, StructuralRole::Definition, &cancellation)
                .unwrap()
                .unwrap();
            assert_eq!(source.get(definition).unwrap(), name);
            let signature = capture_range(node, StructuralRole::Signature, &cancellation)
                .unwrap()
                .unwrap();
            assert_eq!(source.get(signature).unwrap().trim_end(), header);
        }
    }

    #[test]
    fn repeated_record_field_names_each_retain_definition_evidence() {
        let source = "data Record = Record { first, second :: Int }\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let fields = nodes(tree.root_node(), "variable")
            .into_iter()
            .filter(|node| binding(*node, &cancellation).unwrap() == Some("haskell.field"))
            .map(|node| node.utf8_text(source.as_bytes()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fields, ["first", "second"]);
        for node in nodes(tree.root_node(), "field_name") {
            assert!(!retain_capture(node, StructuralRole::Reference, &cancellation).unwrap());
        }
    }

    #[test]
    fn explicit_record_patterns_and_puns_keep_distinct_binding_evidence() {
        let source = "module Example where\nexplicit Record { field = other } = other\npun Record { field } = field\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let functions = nodes(tree.root_node(), "function");
        assert_eq!(functions.len(), 2);
        for (function, (expected_binding, expected_parameters)) in functions.into_iter().zip([
            (None, vec!["other"]),
            (Some("haskell.parameter"), vec!["field"]),
        ]) {
            let fields = nodes(function, "field_name");
            assert_eq!(fields.len(), 1);
            let field = fields[0];
            let name = field.named_child(0).unwrap();
            assert_eq!(name.utf8_text(source.as_bytes()).unwrap(), "field");
            assert_eq!(binding(name, &cancellation).unwrap(), expected_binding);
            for role in [StructuralRole::Declaration, StructuralRole::Definition] {
                assert_eq!(
                    retain_capture(name, role, &cancellation).unwrap(),
                    expected_binding.is_some()
                );
            }
            assert!(retain_capture(field, StructuralRole::Reference, &cancellation).unwrap());
            assert_eq!(
                syntax(
                    field,
                    StructuralRole::Reference,
                    source.as_bytes(),
                    &cancellation
                )
                .unwrap(),
                Some("haskell.field_name")
            );
            let range = capture_range(field, StructuralRole::Reference, &cancellation)
                .unwrap()
                .unwrap();
            assert_eq!(source.get(range).unwrap(), "field");
            let parameters = nodes(function, "variable")
                .into_iter()
                .filter(|node| binding(*node, &cancellation).unwrap() == Some("haskell.parameter"))
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(parameters, expected_parameters);
        }
    }

    #[test]
    fn view_patterns_bind_only_the_written_pattern_side() {
        let source = "module Example where\nshared (bound -> bound) = bound\ndistinct (extract -> bound) plain = bound\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let functions = nodes(tree.root_node(), "function");
        assert_eq!(functions.len(), 2);
        for (function, (expression_name, expected_parameters)) in functions.into_iter().zip([
            ("bound", vec!["bound"]),
            ("extract", vec!["bound", "plain"]),
        ]) {
            let views = nodes(function, "view_pattern");
            assert_eq!(views.len(), 1);
            let view = views[0];
            let expression = view.child_by_field_name("expression").unwrap();
            let pattern = view.child_by_field_name("pattern").unwrap();
            assert_eq!(
                expression.utf8_text(source.as_bytes()).unwrap(),
                expression_name
            );
            assert_eq!(pattern.utf8_text(source.as_bytes()).unwrap(), "bound");
            assert_eq!(binding(expression, &cancellation).unwrap(), None);
            assert_eq!(
                binding(pattern, &cancellation).unwrap(),
                Some("haskell.parameter")
            );
            for role in [StructuralRole::Declaration, StructuralRole::Definition] {
                assert!(!retain_capture(expression, role, &cancellation).unwrap());
                assert!(retain_capture(pattern, role, &cancellation).unwrap());
            }
            assert!(retain_capture(expression, StructuralRole::Reference, &cancellation).unwrap());
            assert!(!retain_capture(pattern, StructuralRole::Reference, &cancellation).unwrap());
            let parameters = nodes(function, "variable")
                .into_iter()
                .filter(|node| binding(*node, &cancellation).unwrap() == Some("haskell.parameter"))
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(parameters, expected_parameters);
        }
    }

    #[test]
    fn repeated_generator_binders_have_distinct_written_scopes() {
        use super::super::{GrammarFamily, QueryPack};

        let source = "values = [x | x <- xs, x <- ys]\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let query = QueryPack::compile(
            GrammarFamily::Haskell,
            include_str!("../../queries/haskell.scm"),
        )
        .unwrap();
        let extraction = query
            .extract(
                GrammarFamily::Haskell,
                &tree,
                source.as_bytes(),
                tree.root_node().descendant_count(),
                128,
                &cancellation,
            )
            .unwrap();
        assert_eq!(extraction.limit, None);
        let mut scopes = extraction
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.role == StructuralRole::Scope && candidate.syntax == "haskell.generator"
            })
            .collect::<Vec<_>>();
        scopes.sort_by_key(|candidate| candidate.start);
        assert_eq!(scopes.len(), 2);
        assert!(scopes[0].end < scopes[1].start);
        let mut bindings = extraction
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.role == StructuralRole::Declaration
                    && candidate.syntax == "haskell.variable"
            })
            .collect::<Vec<_>>();
        bindings.sort_by_key(|candidate| candidate.start);
        assert_eq!(bindings.len(), 3);
        let generator_bindings = bindings
            .into_iter()
            .filter(|candidate| source.get(candidate.start..candidate.end).unwrap() == "x")
            .collect::<Vec<_>>();
        assert_eq!(generator_bindings.len(), 2);
        for (index, (scope, expected)) in scopes.iter().zip(["x <- xs", "x <- ys"]).enumerate() {
            assert_eq!(source.get(scope.start..scope.end).unwrap(), expected);
            let binding = generator_bindings[index];
            assert_eq!(binding.start, scope.start);
            assert_eq!(binding.end, scope.start + 1);
            assert_eq!(
                scopes
                    .iter()
                    .filter(|owner| { owner.start <= binding.start && binding.end <= owner.end })
                    .count(),
                1
            );
            assert!(extraction.candidates.iter().any(|candidate| {
                candidate.role == StructuralRole::Definition
                    && candidate.start == binding.start
                    && candidate.end == binding.end
            }));
        }
        let mut references = extraction
            .candidates
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Reference)
            .collect::<Vec<_>>();
        references.sort_by_key(|candidate| candidate.start);
        assert_eq!(
            references
                .into_iter()
                .map(|candidate| { source.get(candidate.start..candidate.end).unwrap() })
                .collect::<Vec<_>>(),
            ["x", "xs", "ys"]
        );
    }

    #[test]
    fn native_declaration_nodes_retain_their_lexical_scope_candidates() {
        use super::super::{GrammarFamily, QueryPack};

        let source = "module Example where\ndata Box = Box Int\nclass Action a where\n  act :: a -> a\n  act value = value\nchoose 0 = 0\nchoose value = value\n";
        let tree = parse(source);
        let cancellation = Cancellation::new();
        let query = QueryPack::compile(
            GrammarFamily::Haskell,
            include_str!("../../queries/haskell.scm"),
        )
        .unwrap();
        let candidates = query
            .extract_identity(
                GrammarFamily::Haskell,
                &tree,
                source.as_bytes(),
                tree.root_node().descendant_count(),
                &cancellation,
            )
            .unwrap();
        let equations = nodes(tree.root_node(), "function")
            .into_iter()
            .filter(|node| equation(*node))
            .collect::<Vec<_>>();
        assert_eq!(equations.len(), 3);
        for (kind, expected_syntax, expected_count) in [
            ("function", "haskell.function_clause", 3),
            ("class", "haskell.class", 1),
            ("data_type", "haskell.data_type", 1),
            ("data_constructor", "haskell.constructor", 1),
        ] {
            let declarations = nodes(tree.root_node(), kind)
                .into_iter()
                .filter(|node| node.is_named() && (kind != "function" || equation(*node)))
                .collect::<Vec<_>>();
            assert_eq!(declarations.len(), expected_count, "{kind}");
            for node in declarations {
                let scopes = candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.role == StructuralRole::Scope
                            && candidate.syntax == expected_syntax
                            && candidate.start == node.start_byte()
                            && candidate.end == node.end_byte()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(scopes.len(), 1, "{kind}: {candidates:?}");
                let declarations = candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.role == StructuralRole::Declaration
                            && candidate.start == node.start_byte()
                            && candidate.end == node.end_byte()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(declarations.len(), 1);
                assert_eq!(scopes[0].native_depth, declarations[0].native_depth + 1);
            }
        }
    }

    #[test]
    fn exact_equation_frontier_excludes_binder_changing_and_unknown_rhs_syntax() {
        let cancellation = Cancellation::new();
        for (source, eligible) in [
            ("identity x = x\n", true),
            ("choose x y = unknown x y\n", true),
            ("group x = (x)\n", true),
            ("value x = 1\n", true),
            ("value x = 1.5\n", true),
            ("value x = 'a'\n", true),
            ("value x = \"text\"\n", true),
            ("value x = unknown 1 1.5 'a' \"text\" x\n", true),
            ("blocked x = x where\n  x = 1\n", false),
            ("blocked x | True = x\n", false),
            ("blocked (Just x) = x\n", false),
            ("blocked (extract -> x) = x\n", false),
            ("blocked x = (\\x -> x)\n", false),
            ("blocked x = let x = 1 in x\n", false),
            ("blocked x = case x of { Just y -> y }\n", false),
            ("blocked x = do { y <- pure x; pure y }\n", false),
            ("blocked xs = [x | x <- xs]\n", false),
            ("blocked x = Module.x\n", false),
            ("blocked x = Record { field = x }\n", false),
            ("blocked x = (x +)\n", false),
            ("blocked x = (x :: Int)\n", false),
            ("blocked x = [| x |]\n", false),
            ("blocked x = Just x\n", false),
            ("blocked x = unknown (\\y -> y) x\n", false),
        ] {
            let tree = parse(source);
            let clause = nodes(tree.root_node(), "function")
                .into_iter()
                .find(|node| equation(*node))
                .unwrap();
            assert_eq!(
                simple_function_clause(clause, &cancellation).unwrap(),
                eligible,
                "{source}"
            );
        }
        let tree = parse("forward x = unknown x\n");
        let variables = nodes(tree.root_node(), "variable");
        let marked = variables
            .into_iter()
            .filter(|node| equation_rhs_read(*node, &cancellation).unwrap())
            .map(|node| node.utf8_text(b"forward x = unknown x\n").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(marked, ["x"]);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            simple_function_clause(tree.root_node(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            equation_rhs_read(tree.root_node(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn conditional_fields_extend_only_the_closed_equation_read_frontier() {
        let cancellation = Cancellation::new();
        for (source, eligible) in [
            (
                "choose flag left right = if flag then left else right\n",
                true,
            ),
            (
                "nested flag other left right = if flag then (if other then left else right) else right\n",
                true,
            ),
            ("group flag value = (if (flag) then (value) else 0)\n", true),
            (
                "forward flag value = if flag then unknown value else value\n",
                true,
            ),
            ("free flag value = if missing then value else 0\n", true),
            (
                "dispatch flag first second value = (if flag then first else second) value\n",
                true,
            ),
            (
                "blocked flag x = if flag then let x = 1 in x else x\n",
                false,
            ),
            ("blocked flag x = if flag then x else (\\x -> x)\n", false),
            ("blocked flag x = if flag then x else (x :: Int)\n", false),
            ("blocked flag x = if Module.flag then x else x\n", false),
            ("blocked flag x = if flag then (x +) else x\n", false),
            ("blocked flag x = if flag then Just x else x\n", false),
            (
                "blocked flag x = if flag then x else x where\n  x = 1\n",
                false,
            ),
            ("blocked flag x | flag = if flag then x else x\n", false),
            ("blocked flag x = if | flag -> x\n", false),
        ] {
            let tree = parse(source);
            assert!(!tree.root_node().has_error(), "{source}");
            let clause = nodes(tree.root_node(), "function")
                .into_iter()
                .find(|node| equation(*node))
                .unwrap();
            assert_eq!(
                simple_function_clause(clause, &cancellation).unwrap(),
                eligible,
                "{source}"
            );
        }
        for (source, expected) in [
            (
                "choose flag left right = if flag then left else right\n",
                vec!["flag", "left", "right"],
            ),
            (
                "nested flag other left right = if flag then (if other then left else right) else right\n",
                vec!["flag", "other", "left", "right", "right"],
            ),
            (
                "forward flag value = if flag then unknown value else value\n",
                vec!["flag", "value", "value"],
            ),
            (
                "group flag value = (if (flag) then (value) else 0)\n",
                vec!["flag", "value"],
            ),
            (
                "dispatch flag first second value = (if flag then first else second) value\n",
                vec!["value"],
            ),
        ] {
            let tree = parse(source);
            let actual = nodes(tree.root_node(), "variable")
                .into_iter()
                .filter(|node| equation_rhs_read(*node, &cancellation).unwrap())
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{source}");
        }
    }

    #[test]
    fn binary_infix_operands_exclude_operator_dispatch_and_unknown_shapes() {
        let cancellation = Cancellation::new();
        for (source, eligible) in [
            ("add x = x + 1\n", true),
            ("multiply x = x * 1\n", true),
            ("hash x = x # 1\n", true),
            ("hashes x = x ## 1\n", true),
            ("hash_pipe x = x #| 1\n", true),
            ("mixed left right x = (left + right) * x\n", true),
            ("forward x = unknown $ other x\n", true),
            ("tick left right = left `unknown` right\n", true),
            (
                "dispatch operation left right = left `operation` right\n",
                true,
            ),
            ("conditional flag x = if flag then x + 1 else x\n", true),
            ("blocked x = (x +)\n", false),
            ("blocked x = (+ x)\n", false),
            ("blocked x = x Module.+ 1\n", false),
            ("blocked x = x `Module.combine` x\n", false),
            ("blocked x = x : x\n", false),
            ("blocked x = x `Just` x\n", false),
            ("blocked x = x + (let x = 1 in x)\n", false),
            ("blocked x = x + (\\x -> x)\n", false),
            ("blocked x = x + (x :: Int)\n", false),
            ("blocked x = x + 1 where\n  x = 1\n", false),
            ("blocked x | x = x + 1\n", false),
        ] {
            let tree = parse(source);
            let clause = nodes(tree.root_node(), "function")
                .into_iter()
                .find(|node| equation(*node))
                .unwrap();
            assert_eq!(
                simple_function_clause(clause, &cancellation).unwrap(),
                eligible,
                "{source}: {}",
                clause.to_sexp()
            );
        }
        for (source, expected) in [
            ("add x = x + 1\n", vec!["x"]),
            (
                "mixed left right x = (left + right) * x\n",
                vec!["left", "right", "x"],
            ),
            ("forward x = unknown $ other x\n", vec!["unknown", "x"]),
            (
                "tick left right = left `unknown` right\n",
                vec!["left", "right"],
            ),
            (
                "dispatch operation left right = left `operation` right\n",
                vec!["left", "right"],
            ),
            ("callee operation x = operation x + 1\n", vec!["x"]),
            (
                "conditional flag x = if flag then x + 1 else x\n",
                vec!["flag", "x", "x"],
            ),
        ] {
            let tree = parse(source);
            let actual = nodes(tree.root_node(), "variable")
                .into_iter()
                .filter(|node| equation_rhs_read(*node, &cancellation).unwrap())
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{source}");
        }
        let source = format!("chain x = {}\n", vec!["x"; 24].join(" + "));
        let tree = parse(&source);
        let clause = nodes(tree.root_node(), "function")
            .into_iter()
            .find(|node| equation(*node))
            .unwrap();
        assert!(simple_function_clause(clause, &cancellation).unwrap());
        let variables = nodes(tree.root_node(), "variable")
            .into_iter()
            .filter(|node| equation_rhs_read(*node, &cancellation).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(variables.len(), 24);
        let infix = nodes(tree.root_node(), "infix").into_iter().next().unwrap();
        assert!(simple_binary_infix(infix, &cancellation).unwrap());
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            simple_binary_infix(infix, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            equation_rhs_read(variables[0], &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn selectors_observe_cancellation_before_visiting_nodes() {
        let tree = parse("value = 1\n");
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let node = tree.root_node();
        assert!(matches!(
            retain_capture(node, StructuralRole::Root, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            syntax(node, StructuralRole::Root, b"value = 1\n", &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            capture_range(node, StructuralRole::Root, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
