//! Maps authored Groovy nodes to parser-independent structural roles.
//! Declaration ownership follows native fields, never descendant-name heuristics.

use super::StructuralRole;
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::collections::HashMap;
use tree_sitter::Node;

/// Shares callable classification across declaration, scope and parameter captures.
#[derive(Default)]
pub(super) struct CallableKinds {
    declarations: HashMap<usize, &'static str>,
}

impl CallableKinds {
    /// Retains method metadata only for its source-identified native declaration.
    ///
    /// # Errors
    /// Returns cancellation, allocation, or inconsistent native-span errors.
    pub(super) fn adjust(
        &mut self,
        node: Node<'_>,
        candidate: &mut super::QueryCandidate,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let mut declaration = node;
        if candidate.role == StructuralRole::Expression {
            // Only direct parameter-list, parameter and parameter-annotation
            // captures belong to this header; never walk through a nested body.
            for kind in ["annotation", "formal_parameter", "formal_parameters"] {
                if declaration.kind() == kind {
                    let Some(parent) = declaration.parent() else {
                        return Ok(false);
                    };
                    declaration = parent;
                }
            }
        }
        if !matches!(
            declaration.kind(),
            "method_declaration" | "constructor_declaration"
        ) {
            return Ok(true);
        }
        let kind = if let Some(kind) = self.declarations.get(&declaration.id()) {
            *kind
        } else {
            let kind = callable_kind(declaration, source, cancellation)?;
            self.declarations
                .try_reserve(1)
                .map_err(|_| super::query_failure("query-groovy-callable-allocation"))?;
            self.declarations.insert(declaration.id(), kind);
            kind
        };
        match candidate.role {
            StructuralRole::Expression => {
                if kind != "groovy.function" {
                    return Ok(false);
                }
                if declaration == node {
                    candidate.syntax = callable_header(node, cancellation)?;
                }
            }
            StructuralRole::Declaration | StructuralRole::Scope => candidate.syntax = kind,
            _ => {}
        }
        Ok(true)
    }
}

fn callable_kind(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    cancellation.check()?;
    if node.child_by_field_name("return_type").is_some() {
        return Ok("groovy.function");
    }
    let name = node
        .child_by_field_name("name")
        .ok_or_else(|| super::query_failure("query-groovy-callable-name"))?;
    let owner = node
        .parent()
        .filter(|body| matches!(body.kind(), "class_body" | "enum_body"))
        .and_then(|body| {
            body.parent()
                .filter(|owner| owner.child_by_field_name("body") == Some(body))
        });
    // Interfaces and annotation types do not declare constructors, even when
    // an omitted-return method has exactly the same name as its type owner.
    if owner.is_some_and(|owner| {
        matches!(
            owner.kind(),
            "interface_declaration" | "annotation_type_declaration"
        )
    }) {
        return Ok("groovy.function");
    }
    let mut has_prefix = false;
    let mut invalid_constructor_modifier = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.start_byte() >= name.start_byte() {
            break;
        }
        match child.kind() {
            "annotation" | "public" | "protected" | "private" | "def" | "synchronized"
            | "transient" | "strictfp" => has_prefix = true,
            "static" | "final" | "abstract" | "native" | "volatile" => {
                has_prefix = true;
                invalid_constructor_modifier = true;
            }
            _ => {}
        }
    }
    if let Some(owner) = owner.filter(|owner| {
        matches!(
            owner.kind(),
            "class_declaration" | "trait_declaration" | "record_declaration" | "enum_declaration"
        )
    }) {
        let owner_name = owner
            .child_by_field_name("name")
            .ok_or_else(|| super::query_failure("query-groovy-callable-owner-name"))?;
        let text = |node: Node<'_>| {
            source
                .get(node.byte_range())
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
        };
        let (Some(name_text), Some(owner_text)) = (text(name), text(owner_name)) else {
            return Ok("groovy.invalid_callable");
        };
        let canonical = |text| {
            rootlight_adapter_sdk::structural_captured_name_for_language(
                "groovy",
                text,
                source.len(),
            )
        };
        let (Some(name), Some(owner_name)) = (canonical(name_text), canonical(owner_text)) else {
            // Keep an established callable boundary even if its quoted name
            // is unavailable. Lowering reports the exact name gap and prevents
            // parameters or locals from leaking into a different owner.
            return Ok(if has_prefix {
                "groovy.function"
            } else {
                "groovy.invalid_callable"
            });
        };
        cancellation.check()?;
        if name == owner_name {
            // Dynamic def is not a real return type. Conversely, the early
            // authored return-field branch keeps Object Owner() a method.
            return Ok(
                if invalid_constructor_modifier || node.child_by_field_name("body").is_none() {
                    "groovy.invalid_callable"
                } else {
                    "groovy.constructor"
                },
            );
        }
    }
    Ok(if has_prefix {
        "groovy.function"
    } else {
        "groovy.invalid_callable"
    })
}

/// Finds the single leading package that can own the rest of this source unit.
///
/// # Errors
/// Returns cancellation while scanning direct children; malformed, late or
/// repeated package headers are not promoted to compilation-unit containers.
pub(super) fn leading_package<'tree>(
    root: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    let mut package = None;
    let mut seen_statement = false;
    let mut cursor = root.walk();
    for (index, child) in root.named_children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        match child.kind() {
            "line_comment" | "block_comment" | "groovydoc_comment" | "shebang" => {}
            "package_declaration" => {
                if seen_statement || child.has_error() || child.is_missing() {
                    return Ok(None);
                }
                package = Some(child);
                seen_statement = true;
            }
            _ => seen_statement = true,
        }
    }
    Ok(package)
}

/// Per-scan member classification shares a declaration header across all its bindings.
#[derive(Default)]
pub(super) struct MemberKinds {
    declarations: HashMap<usize, &'static str>,
}

impl MemberKinds {
    /// Classifies a native field declarator without repeating annotation/modifier scans.
    ///
    /// # Errors
    /// Returns cancellation, allocation, or invalid native-owner errors.
    pub(super) fn kind(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<&'static str, AdapterError> {
        cancellation.check()?;
        let declaration = node
            .parent()
            .filter(|parent| parent.kind() == "field_declaration")
            .ok_or_else(|| super::query_failure("query-groovy-field-owner"))?;
        if let Some(kind) = self.declarations.get(&declaration.id()) {
            return Ok(*kind);
        }
        let kind = member_kind(declaration, cancellation)?;
        self.declarations
            .try_reserve(1)
            .map_err(|_| super::query_failure("query-groovy-member-allocation"))?;
        self.declarations.insert(declaration.id(), kind);
        Ok(kind)
    }
}

fn member_kind(
    declaration: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if declaration
        .parent()
        .and_then(|body| body.parent())
        .is_some_and(|owner| {
            matches!(
                owner.kind(),
                "interface_declaration" | "annotation_type_declaration"
            )
        })
    {
        return Ok("groovy.field");
    }
    let mut cursor = declaration.walk();
    for (index, child) in declaration.children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        match child.kind() {
            "public" | "protected" | "private" => return Ok("groovy.field"),
            "variable_declarator" => break,
            _ => {}
        }
    }
    // Static/final modifiers do not suppress Groovy's implicit property accessors.
    // Only authored declarations are emitted; generated accessors have no source span.
    Ok("groovy.property")
}

pub(super) fn syntax(node: Node<'_>, role: StructuralRole) -> Option<&'static str> {
    if role == StructuralRole::Expression {
        if let Some((_, label)) = argument(node) {
            return Some(label);
        }
        match node.kind() {
            "formal_parameters" => return Some("groovy.callable_parameters"),
            "formal_parameter" => {
                let default = node.child_by_field_name("default").is_some();
                let array = node
                    .child_by_field_name("type")
                    .is_some_and(|ty| matches!(ty.kind(), "array_type" | "varargs_type"));
                return Some(match (default, array) {
                    (false, false) => "groovy.callable_parameter_required",
                    (true, false) => "groovy.callable_parameter_default",
                    (false, true) => "groovy.callable_parameter_required_array",
                    (true, true) => "groovy.callable_parameter_default_array",
                });
            }
            "annotation" => return Some("groovy.callable_annotation"),
            _ => {}
        }
    }
    if role == StructuralRole::CallName {
        // A native identifier function field excludes receiver and computed calls.
        // This identifies written lookup syntax, not the selected runtime target.
        return (node.kind() == "identifier"
            && node.parent().is_some_and(|parent| {
                parent.kind() == "method_invocation"
                    && parent.child_by_field_name("function") == Some(node)
            }))
        .then_some("groovy.implicit_call_name");
    }
    if node.kind() == "identifier"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "import_declaration"
                && parent.child_by_field_name("alias") == Some(node)
        })
    {
        return Some("groovy.import_alias");
    }
    if role == StructuralRole::Expression
        && node.parent().is_some_and(|parent| {
            parent.kind() == "for_in_statement" && parent.child_by_field_name("value") == Some(node)
        })
    {
        return Some("groovy.iterable");
    }
    if role == StructuralRole::Signature {
        return Some("groovy.parameters");
    }
    Some(match node.kind() {
        "source_file" => "groovy.file",
        "package_declaration" => "groovy.package",
        "qualified_name" => "groovy.qualified_identifier",
        "qualified_type" => "groovy.qualified_type",
        "class_declaration" => "groovy.class",
        "interface_declaration" | "annotation_type_declaration" => "groovy.interface",
        "trait_declaration" => "groovy.trait",
        "record_declaration" => "groovy.record",
        "enum_declaration" => "groovy.enum",
        "enum_constant" => "groovy.constant",
        // Script methods are file-level functions; lowering refines members using their type owner.
        "method_declaration" => "groovy.function",
        "constructor_declaration" => "groovy.constructor",
        "variable_declarator" => {
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "field_declaration")
            {
                "groovy.field"
            } else {
                "groovy.variable"
            }
        }
        "formal_parameter" | "closure_parameter" | "catch_formal_parameter" => "groovy.parameter",
        "resource" => "groovy.variable",
        "type_parameter" => "groovy.type_parameter",
        "record_component" => "groovy.property",
        "identifier"
            if role == StructuralRole::Declaration
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "for_in_statement"
                        && parent.child_by_field_name("variable") == Some(node)
                }) =>
        {
            "groovy.variable"
        }
        "identifier" => "groovy.identifier",
        "type_identifier" => "groovy.type_identifier",
        "field_declaration" => "groovy.member",
        "static_initializer" => "groovy.static_context",
        "quoted_identifier" => "groovy.quoted_identifier",
        "import_declaration" => "groovy.import",
        "superclass" | "super_interfaces" | "extends_interfaces" => "groovy.inheritance",
        "method_invocation"
            if node
                .child_by_field_name("function")
                .is_some_and(|function| function.kind() == "identifier") =>
        {
            "groovy.implicit_call"
        }
        "method_invocation" => "groovy.call",
        "block" => "groovy.block",
        "class_body" | "enum_body" => "groovy.type_body",
        "for_statement" => "groovy.for",
        "for_in_statement" => "groovy.enhanced_for",
        "local_variable_declaration" => "groovy.local_binding",
        "multi_assignment_declaration" => "groovy.destructuring",
        "catch_clause" => "groovy.catch",
        "try_statement" => "groovy.try",
        "closure" | "gstring_closure" if role == StructuralRole::Declaration => {
            "groovy.anonymous_function"
        }
        "closure" | "gstring_closure" => "groovy.closure",
        "line_comment" => "groovy.line_comment",
        "block_comment" => "groovy.block_comment",
        "string_literal" => "groovy.string",
        _ => return None,
    })
}

/// Shares closure-postfix ownership across all captures in one native scan.
#[derive(Default)]
pub(super) struct Calls<'tree> {
    owners: HashMap<usize, Node<'tree>>,
}

impl<'tree> Calls<'tree> {
    /// Keeps one invocation for closure suffixes but preserves explicit result calls.
    ///
    /// # Errors
    /// Returns cancellation or allocation failure during native ownership traversal.
    pub(super) fn retain(
        &mut self,
        node: Node<'tree>,
        role: StructuralRole,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if role == StructuralRole::Expression
            && matches!(
                node.kind(),
                "line_comment" | "block_comment" | "groovydoc_comment"
            )
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "argument_list")
        {
            return Ok(false);
        }
        if node.kind() == "method_invocation"
            && matches!(role, StructuralRole::Call | StructuralRole::Scope)
        {
            return Ok(self.owner(node, cancellation)? == node);
        }
        Ok(true)
    }

    /// Retains argument prefixes and terminal dispatch syntax for the complete call.
    ///
    /// # Errors
    /// Returns cancellation or inconsistent native call/allocation errors.
    pub(super) fn adjust(
        &mut self,
        node: Node<'tree>,
        candidate: &mut super::QueryCandidate,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        if candidate.role == StructuralRole::Expression
            && let Some((call, _)) = argument(node)
        {
            // A prefix is ownership evidence, not the argument's exact source range.
            // Its start prevents an argument-sized nested call/closure from stealing
            // the parent in the parser-independent hierarchy.
            candidate.start = self.owner(call, cancellation)?.start_byte();
        }
        if node.kind() == "method_invocation"
            && matches!(candidate.role, StructuralRole::Call | StructuralRole::Scope)
        {
            let mut call = node;
            let function = loop {
                cancellation.check()?;
                let function = call
                    .child_by_field_name("function")
                    .ok_or_else(|| super::query_failure("query-groovy-call-function"))?;
                if call.child_by_field_name("arguments").is_some() {
                    break function;
                }
                let mut inner = function;
                while inner.kind() == "parenthesized_expression" {
                    cancellation.check()?;
                    let mut expression = None;
                    let mut cursor = inner.walk();
                    for child in inner.named_children(&mut cursor) {
                        cancellation.check()?;
                        if !child.is_extra() && expression.replace(child).is_some() {
                            return Err(super::query_failure("query-groovy-call-function"));
                        }
                    }
                    inner = expression
                        .ok_or_else(|| super::query_failure("query-groovy-call-function"))?;
                }
                if inner.kind() != "method_invocation" {
                    break function;
                }
                call = inner;
            };
            candidate.syntax = if function.kind() == "identifier" {
                "groovy.implicit_call"
            } else {
                "groovy.call"
            };
        }
        Ok(())
    }

    fn owner(
        &mut self,
        mut call: Node<'tree>,
        cancellation: &Cancellation,
    ) -> Result<Node<'tree>, AdapterError> {
        let mut path = Vec::new();
        let owner = loop {
            cancellation.check()?;
            if let Some(owner) = self.owners.get(&call.id()) {
                break *owner;
            }
            path.try_reserve(1)
                .map_err(|_| super::query_failure("query-groovy-call-allocation"))?;
            path.push(call);
            let mut receiver = call;
            while let Some(parent) = receiver.parent()
                && parent.kind() == "parenthesized_expression"
            {
                cancellation.check()?;
                receiver = parent;
            }
            let Some(outer) = receiver.parent().filter(|outer| {
                outer.kind() == "method_invocation"
                    && outer.child_by_field_name("function") == Some(receiver)
                    && outer.child_by_field_name("arguments").is_none()
            }) else {
                break call;
            };
            // Groovy appends trailing closures to the preceding call, including
            // grouped calls. Parenthesized argument lists instead call its result.
            call = outer;
        };
        self.owners
            .try_reserve(path.len())
            .map_err(|_| super::query_failure("query-groovy-call-allocation"))?;
        for call in path {
            cancellation.check()?;
            self.owners.insert(call.id(), owner);
        }
        Ok(owner)
    }
}

fn argument(node: Node<'_>) -> Option<(Node<'_>, &'static str)> {
    let parent = node.parent()?;
    if parent.kind() == "method_invocation" && node.kind() == "closure" {
        return Some((parent, "groovy.argument_closure_prefix"));
    }
    if parent.kind() != "argument_list" {
        return None;
    }
    let call = parent.parent().filter(|call| {
        call.kind() == "method_invocation" && call.child_by_field_name("arguments") == Some(parent)
    })?;
    let label = match node.kind() {
        "named_argument" => "groovy.argument_named_prefix",
        "spread_arguments" => "groovy.argument_spread_prefix",
        _ => "groovy.argument_positional_prefix",
    };
    Some((call, label))
}

/// Distinguishes variable expressions from literal labels and member spelling.
pub(super) fn nonlexical_identifier(node: Node<'_>) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let field = match parent.kind() {
        "field_access" | "direct_field_access_expression" => "field",
        "safe_navigation_expression" | "safe_chain_dot_expression" | "spread_dot_expression" => {
            "property"
        }
        "method_pointer_expression" => "method",
        "method_reference_expression" => "name",
        "method_invocation" => "function",
        "command_chain" => "receiver",
        "command_link" => "name",
        "command_suffix" => "member",
        "map_entry" | "named_argument" => "key",
        "labeled_statement" | "break_statement" | "continue_statement" => "label",
        "qualified_name" => return true,
        "import_declaration" => "alias",
        _ => return false,
    };
    parent.child_by_field_name(field) == Some(node)
}

/// Distinguishes variable reads from writes, excluding native declaration names.
///
/// # Errors
/// Returns cancellation while traversing parenthesized or destructuring targets.
pub(super) fn value_context(
    mut node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if node.parent().is_some_and(|parent| {
        parent.child_by_field_name("name") == Some(node)
            || (parent.kind() == "for_in_statement"
                && parent.child_by_field_name("variable") == Some(node))
    }) {
        return Ok(None);
    }
    let mut structured_target = false;
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        match parent.kind() {
            "parenthesized_expression" | "list_literal" => {
                structured_target = true;
                node = parent;
            }
            "assignment_expression" if parent.child_by_field_name("left") == Some(node) => {
                return Ok(Some(if structured_target {
                    "groovy.value_destructuring"
                } else if parent
                    .child_by_field_name("operator")
                    .is_some_and(|op| op.kind() == "=")
                {
                    "groovy.value_assignment"
                } else {
                    "groovy.value_compound_assignment"
                }));
            }
            "update_expression" | "unary_update_expression"
                if parent.child_by_field_name("operand") == Some(node) =>
            {
                return Ok(Some("groovy.value_update"));
            }
            _ => break,
        }
    }
    Ok(Some("groovy.value_read"))
}

/// Preserves import namespaces, interpolation names and native binding boundaries.
///
/// # Errors
/// Returns cancellation or an incomplete native control/header error.
pub(super) fn adjust_candidate(
    node: Node<'_>,
    candidate: &mut super::QueryCandidate,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    if candidate.role == StructuralRole::Expression && node.kind() == "field_declaration" {
        candidate.syntax = member_header(node, cancellation)?;
    }
    if candidate.role == StructuralRole::Expression
        && matches!(
            node.kind(),
            "class_declaration"
                | "interface_declaration"
                | "annotation_type_declaration"
                | "trait_declaration"
                | "record_declaration"
                | "enum_declaration"
        )
    {
        let name = node
            .child_by_field_name("name")
            .ok_or_else(|| super::query_failure("query-groovy-type-name"))?;
        candidate.end = name.end_byte();
        candidate.syntax = type_visibility(node, name.start_byte(), cancellation)?;
    }
    if candidate.role == StructuralRole::Reference
        && matches!(node.kind(), "type_identifier" | "qualified_type")
        && let Some(kind) = inheritance_type(node)
    {
        candidate.syntax = kind;
    }
    if candidate.role == StructuralRole::Reference
        && node.kind() == "qualified_name"
        && let Some(import) = node.parent().filter(|parent| {
            parent.kind() == "import_declaration"
                && parent.child_by_field_name("name") == Some(node)
        })
    {
        // Only direct anonymous tokens distinguish the import namespace. Text
        // inside an annotation or comment must not become a static/star marker.
        let mut is_static = false;
        let mut is_star = false;
        let mut cursor = import.walk();
        for (index, child) in import.children(&mut cursor).enumerate() {
            if index % 64 == 0 {
                cancellation.check()?;
            }
            match child.kind() {
                "static" => is_static = true,
                ".*" => is_star = true,
                _ => {}
            }
        }
        candidate.syntax = match (is_static, is_star) {
            (false, false) => "groovy.type_import",
            (false, true) => "groovy.package_star_import",
            (true, false) => "groovy.static_import",
            (true, true) => "groovy.static_star_import",
        };
    }
    if candidate.role == StructuralRole::Reference
        && node.kind() == "type_identifier"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "qualified_type")
    {
        candidate.syntax = "groovy.qualified_type_identifier";
    }
    if candidate.role == StructuralRole::Reference
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "gstring_dollar_interpolation")
    {
        let text = source
            .get(candidate.start..candidate.end)
            .and_then(|text| text.strip_prefix(b"$"))
            .ok_or_else(|| super::query_failure("query-groovy-interpolation-name"))?;
        let length = text
            .iter()
            .position(|byte| *byte == b'.')
            .unwrap_or(text.len());
        if length == 0 {
            return Err(super::query_failure("query-groovy-interpolation-name"));
        }
        // The native token includes '$' and optional property suffixes; only its
        // first identifier is a lexical read, with exactly those authored bytes.
        candidate.start = candidate
            .start
            .checked_add(1)
            .ok_or_else(|| super::query_failure("query-groovy-interpolation-name"))?;
        candidate.end = candidate
            .start
            .checked_add(length)
            .ok_or_else(|| super::query_failure("query-groovy-interpolation-name"))?;
    }
    if candidate.role != StructuralRole::Scope {
        return Ok(());
    }
    if candidate.syntax == "groovy.function" && static_context(node, cancellation)? {
        candidate.syntax = "groovy.static_function";
    }
    if matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "annotation_type_declaration"
            | "trait_declaration"
            | "record_declaration"
            | "enum_declaration"
            | "field_declaration"
    ) && static_context(node, cancellation)?
    {
        candidate.syntax = "groovy.static_context";
    }
    if node.kind() == "try_statement" {
        // Resource bindings end with the try body, not with its catch/finally clauses.
        candidate.end = node
            .child_by_field_name("body")
            .ok_or_else(|| super::query_failure("query-groovy-try-body"))?
            .end_byte();
    }
    if matches!(node.kind(), "closure" | "gstring_closure") {
        let end = closure_header_end(node, cancellation)?;
        if node
            .child(0)
            .is_some_and(|opener| opener.kind() == "{" && end == opener.end_byte())
        {
            candidate.syntax = "groovy.implicit_closure";
        }
    }
    cancellation.check()?;
    Ok(())
}

fn inheritance_type(mut node: Node<'_>) -> Option<&'static str> {
    let mut parent = node.parent()?;
    // Only the generic base is a parent type; arguments and wildcard bounds
    // remain ordinary type uses even when they occur inside an extends clause.
    if parent.kind() == "generic_type" && parent.child_by_field_name("base") == Some(node) {
        node = parent;
        parent = node.parent()?;
    }
    match parent.kind() {
        "superclass" | "extends_interfaces" => Some("groovy.extends_type"),
        "super_interfaces" => Some("groovy.implements_type"),
        _ => None,
    }
}

fn type_visibility(
    node: Node<'_>,
    name_start: usize,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let mut visibility = None;
    let mut annotated = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.start_byte() >= name_start {
            break;
        }
        let explicit = match child.kind() {
            "private" => Some("groovy.type_private"),
            "protected" => Some("groovy.type_protected"),
            "public" => Some("groovy.type_public"),
            "annotation" => {
                annotated = true;
                None
            }
            _ => None,
        };
        if let Some(explicit) = explicit
            && visibility.replace(explicit).is_some()
        {
            return Ok("groovy.type_visibility_unknown");
        }
    }
    // AST transforms can change default accessibility; annotation text alone is
    // not evidence that a transform ran or that its result is public.
    Ok(if annotated {
        "groovy.type_visibility_unknown"
    } else {
        visibility.unwrap_or("groovy.type_public")
    })
}

fn member_header(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let interface = node
        .parent()
        .and_then(|body| body.parent())
        .is_some_and(|owner| {
            matches!(
                owner.kind(),
                "interface_declaration" | "annotation_type_declaration"
            )
        });
    let mut visibility = None;
    let mut is_static = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        match child.kind() {
            "variable_declarator" => break,
            "annotation" => return Ok("groovy.member_unknown"),
            "static" => {
                if is_static {
                    return Ok("groovy.member_unknown");
                }
                is_static = true;
            }
            "public" | "protected" | "private" if visibility.replace(child.kind()).is_some() => {
                return Ok("groovy.member_unknown");
            }
            _ => {}
        }
    }
    // Implicit property access is public; the generated private backing field
    // has no separate authored declaration and must not replace that visibility.
    Ok(
        match (is_static || interface, visibility.unwrap_or("public")) {
            (true, "public") => "groovy.member_static_public",
            (true, "protected") => "groovy.member_static_protected",
            (true, "private") => "groovy.member_static_private",
            (false, "public") => "groovy.member_instance_public",
            (false, "protected") => "groovy.member_instance_protected",
            (false, "private") => "groovy.member_instance_private",
            _ => "groovy.member_unknown",
        },
    )
}

/// Classifies the native return field without inferring types from method bodies.
///
/// # Errors
/// Returns cancellation or an out-of-bounds native return span.
pub(super) fn callable_return(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    cancellation.check()?;
    let Some(ty) = node.child_by_field_name("return_type") else {
        return Ok("groovy.callable_return_other");
    };
    let text = source
        .get(ty.byte_range())
        .ok_or_else(|| super::query_failure("query-groovy-callable-return"))?;
    Ok(if ty.has_error() || text.contains(&b'\\') {
        "groovy.callable_return_unknown"
    } else if ty.kind() == "type_identifier" && text == b"boolean" {
        "groovy.callable_return_boolean"
    } else {
        "groovy.callable_return_other"
    })
}

fn callable_header(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let name = node
        .child_by_field_name("name")
        .ok_or_else(|| super::query_failure("query-groovy-callable-name"))?;
    let mut visibility = None;
    let mut is_static = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.start_byte() >= name.start_byte() {
            break;
        }
        match child.kind() {
            "annotation" => return Ok("groovy.callable_unknown"),
            "static" => {
                if is_static {
                    return Ok("groovy.callable_unknown");
                }
                is_static = true;
            }
            "public" | "protected" | "private" if visibility.replace(child.kind()).is_some() => {
                return Ok("groovy.callable_unknown");
            }
            _ => {}
        }
    }
    // The source records eligibility only; no annotation transform is executed.
    Ok(match (is_static, visibility.unwrap_or("public")) {
        (true, "public") => "groovy.callable_static_public",
        (true, "protected") => "groovy.callable_static_protected",
        (true, "private") => "groovy.callable_static_private",
        (false, "public") => "groovy.callable_instance_public",
        (false, "protected") => "groovy.callable_instance_protected",
        (false, "private") => "groovy.callable_instance_private",
        _ => "groovy.callable_unknown",
    })
}

fn static_context(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    // Groovy resolves enclosing type parameters in nested interfaces, traits,
    // annotations and records; Java's implicit-static rule cannot classify them.
    if node.kind() == "enum_declaration" {
        return Ok(true);
    }
    let mut cursor = node.walk();
    for (index, child) in node.children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        if child.kind() == "static" {
            return Ok(true);
        }
        if matches!(
            child.kind(),
            "identifier"
                | "type_identifier"
                | "type_parameters"
                | "class_body"
                | "block"
                | "variable_declarator"
        ) {
            break;
        }
    }
    Ok(node
        .parent()
        .and_then(|body| body.parent())
        .is_some_and(|owner| {
            (node.kind() == "class_declaration"
                && matches!(
                    owner.kind(),
                    "interface_declaration" | "annotation_type_declaration" | "trait_declaration"
                ))
                || (node.kind() == "field_declaration"
                    && matches!(
                        owner.kind(),
                        "interface_declaration" | "annotation_type_declaration"
                    ))
        }))
}

/// Selects only the native closure header, including an explicit empty parameter list.
///
/// # Errors
/// Returns cancellation or an incomplete native header error; never scans body text.
pub(super) fn closure_header_end(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let opener = node.child(0).filter(|child| child.kind() == "{");
    let mut cursor = node.walk();
    for (index, child) in node.named_children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        match child.kind() {
            "closure_parameters" => return Ok(child.end_byte()),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    // No arrow means an implicit parameter, not a body-derived signature.
    opener
        .map(|node| node.end_byte())
        .ok_or_else(|| super::query_failure("query-groovy-closure-header"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_context_requires_one_leading_header_and_honors_cancellation() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        for (source, expected) in [
            ("package sample\nclass Meter {}", Some("package sample")),
            (
                "// header\npackage sample\nclass Meter {}",
                Some("package sample"),
            ),
            ("package sample\npackage repeated\nclass Meter {}", None),
            ("class Before {}\npackage late", None),
            ("", None),
        ] {
            let tree = parser.parse(source, None).unwrap();
            let cancellation = Cancellation::new();
            let package = leading_package(tree.root_node(), &cancellation).unwrap();
            assert_eq!(
                package.map(|node| node.utf8_text(source.as_bytes()).unwrap()),
                expected
            );
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(leading_package(tree.root_node(), &cancellation).is_err());
        }
    }

    #[test]
    fn member_headers_are_shared_and_cancellable_without_text_keyword_matching() {
        let source = b"class Holder { /* private */ @Mark(\"public\") String first, second }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let body = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .child_by_field_name("body")
            .unwrap();
        let mut cursor = body.walk();
        let declaration = body
            .named_children(&mut cursor)
            .find(|node| node.kind() == "field_declaration")
            .unwrap();
        let mut cursor = declaration.walk();
        let bindings: Vec<_> = declaration
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "variable_declarator")
            .collect();
        assert_eq!(bindings.len(), 2);
        let cancellation = Cancellation::new();
        let mut cache = MemberKinds::default();
        for _ in 0..128 {
            for binding in &bindings {
                assert_eq!(
                    cache.kind(*binding, &cancellation).unwrap(),
                    "groovy.property"
                );
            }
        }
        assert_eq!(cache.declarations.len(), 1);
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(cache.kind(bindings[0], &cancellation).is_err());
        assert!(
            MemberKinds::default()
                .kind(bindings[0], &cancellation)
                .is_err()
        );
    }
}
