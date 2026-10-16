//! Native Rust pattern leaves and identifier namespaces.
//! The query emits one declaration per proven leaf; complete let and callable
//! extents remain separate metadata for the shared lexical binding consumer.

use std::collections::{BTreeSet, HashMap};

use tree_sitter::Node;

use super::{AdapterError, Cancellation, QueryCandidate, StructuralRole};

#[derive(Clone, Copy)]
enum Binding {
    Variable,
    Parameter,
    Unavailable,
}

#[derive(Default)]
pub(super) struct CaptureCache {
    bindings: HashMap<usize, Option<Binding>>,
    namespaces: HashMap<usize, (BTreeSet<String>, bool)>,
}

impl CaptureCache {
    fn binding(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Binding>, AdapterError> {
        if let Some(binding) = self.bindings.get(&node.id()) {
            return Ok(*binding);
        }
        let mut current = node;
        let mut unavailable = false;
        let mut result = None;
        while let Some(parent) = current.parent() {
            cancellation.check()?;
            match parent.kind() {
                "let_declaration" if parent.child_by_field_name("pattern") == Some(current) => {
                    result = Some(Binding::Variable);
                    break;
                }
                "parameter" if parent.child_by_field_name("pattern") == Some(current) => {
                    result = Some(Binding::Parameter);
                    break;
                }
                "closure_parameters" => {
                    result = Some(Binding::Parameter);
                    break;
                }
                "tuple_pattern" | "slice_pattern" | "mut_pattern" | "ref_pattern"
                | "reference_pattern" | "captured_pattern" => {}
                "field_pattern"
                    if parent.child_by_field_name("pattern") == Some(current)
                        || current.kind() == "shorthand_field_identifier" => {}
                "struct_pattern" | "tuple_struct_pattern"
                    if parent.child_by_field_name("type") != Some(current) => {}
                "or_pattern" => unavailable = true,
                // Ordinary let extents do not prove these binding environments.
                // Their source regions retain explicit visibility gaps.
                "let_condition" | "for_expression" | "match_pattern" => {
                    unavailable = true;
                    result = Some(Binding::Unavailable);
                    break;
                }
                _ => break,
            }
            current = parent;
        }
        if result.is_some()
            && (unavailable || self.namespace_unavailable(node, source, cancellation)?)
        {
            result = Some(Binding::Unavailable);
        }
        self.bindings.insert(node.id(), result);
        Ok(result)
    }

    fn namespace_unavailable(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(name) = source
            .get(node.byte_range())
            .and_then(|raw| std::str::from_utf8(raw).ok())
        else {
            return Ok(true);
        };
        let Some(name) = rootlight_adapter_sdk::canonical_rust_identifier(name, usize::MAX) else {
            return Ok(true);
        };
        let mut parent = node.parent();
        while let Some(scope) = parent {
            cancellation.check()?;
            if matches!(
                scope.kind(),
                "source_file" | "block" | "declaration_list" | "function_item"
            ) {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.namespaces.entry(scope.id())
                {
                    let mut names = BTreeSet::new();
                    let mut glob = false;
                    let mut cursor = scope.walk();
                    for child in scope.named_children(&mut cursor) {
                        cancellation.check()?;
                        let value_item = matches!(child.kind(), "const_item" | "static_item")
                            || (child.kind() == "struct_item"
                                && child.child_by_field_name("body").is_none());
                        if value_item
                            && let Some(item_name) = child.child_by_field_name("name")
                            && let Ok(name) = item_name.utf8_text(source)
                            && let Some(name) =
                                rootlight_adapter_sdk::canonical_rust_identifier(name, usize::MAX)
                        {
                            names.insert(name.into_owned());
                        }
                        if child.kind() == "use_declaration" {
                            let mut pending = vec![child];
                            while let Some(part) = pending.pop() {
                                cancellation.check()?;
                                glob |= part.kind() == "use_wildcard";
                                if part.kind() == "identifier"
                                    && let Ok(name) = part.utf8_text(source)
                                    && let Some(name) =
                                        rootlight_adapter_sdk::canonical_rust_identifier(
                                            name,
                                            usize::MAX,
                                        )
                                {
                                    names.insert(name.into_owned());
                                }
                                let mut cursor = part.walk();
                                pending.extend(part.named_children(&mut cursor));
                            }
                        }
                        if child.kind() == "type_parameters" {
                            let mut cursor = child.walk();
                            for parameter in child.named_children(&mut cursor) {
                                cancellation.check()?;
                                if parameter.kind() == "const_parameter"
                                    && let Some(item_name) = parameter.child_by_field_name("name")
                                    && let Ok(name) = item_name.utf8_text(source)
                                    && let Some(name) =
                                        rootlight_adapter_sdk::canonical_rust_identifier(
                                            name,
                                            usize::MAX,
                                        )
                                {
                                    names.insert(name.into_owned());
                                }
                            }
                        }
                    }
                    entry.insert((names, glob));
                }
                if self
                    .namespaces
                    .get(&scope.id())
                    .is_some_and(|(names, glob)| *glob || names.contains(name.as_ref()))
                {
                    return Ok(true);
                }
            }
            parent = scope.parent();
        }
        Ok(false)
    }

    pub(super) fn retain(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if role == StructuralRole::Signature && node.kind() == "function_item" {
            if module_const_parameter_context(node, cancellation)?.is_some() {
                return Ok(true);
            }
            let mut parent = node.parent();
            while let Some(scope) = parent {
                cancellation.check()?;
                if matches!(
                    scope.kind(),
                    "block" | "impl_item" | "trait_item" | "function_item"
                ) {
                    return Ok(true);
                }
                if matches!(scope.kind(), "source_file" | "mod_item") {
                    return Ok(false);
                }
                parent = scope.parent();
            }
            return Ok(true);
        }
        if role == StructuralRole::Signature && node.kind() == "mod_item" {
            return Ok(node.child_by_field_name("body").is_none());
        }
        if matches!(role, StructuralRole::Signature | StructuralRole::Scope)
            && matches!(node.kind(), "if_expression" | "while_expression")
        {
            return has_let_condition(node, cancellation);
        }
        if role == StructuralRole::Signature
            && let Some(parent) = node.parent()
            && matches!(parent.kind(), "if_expression" | "while_expression")
            && parent.child_by_field_name("condition") == Some(node)
        {
            return Ok(first_let_end(node, cancellation)?.is_some_and(|end| end < node.end_byte()));
        }
        if matches!(role, StructuralRole::Signature | StructuralRole::Scope)
            && matches!(node.kind(), "match_arm" | "for_expression")
        {
            let Some(pattern) = node.child_by_field_name("pattern") else {
                return Ok(false);
            };
            let pattern = if pattern.kind() == "match_pattern" {
                pattern.named_child(0).unwrap_or(pattern)
            } else {
                pattern
            };
            let mut pending = vec![pattern];
            while let Some(node) = pending.pop() {
                cancellation.check()?;
                if node.kind() == "macro_invocation" {
                    return Ok(true);
                }
                if matches!(node.kind(), "identifier" | "shorthand_field_identifier")
                    && self.binding(node, source, cancellation)?.is_some()
                {
                    return Ok(true);
                }
                let mut cursor = node.walk();
                pending.extend(node.named_children(&mut cursor));
            }
            return Ok(false);
        }
        if !matches!(node.kind(), "identifier" | "shorthand_field_identifier") {
            return Ok(true);
        }
        let binding = self.binding(node, source, cancellation)?;
        match role {
            StructuralRole::Declaration => Ok(matches!(
                binding,
                Some(Binding::Variable | Binding::Parameter)
            )),
            StructuralRole::Definition => Ok(matches!(
                binding,
                Some(Binding::Variable | Binding::Parameter)
            ) || node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    "function_item" | "const_item" | "static_item" | "mod_item"
                ) && parent.child_by_field_name("name") == Some(node)
            })),
            StructuralRole::Reference => {
                let mut current = node;
                while let Some(parent) = current.parent() {
                    cancellation.check()?;
                    if matches!(
                        parent.kind(),
                        "token_tree"
                            | "attribute"
                            | "attribute_item"
                            | "inner_attribute_item"
                            | "lifetime"
                            | "label"
                    ) {
                        return Ok(false);
                    }
                    if matches!(
                        parent.kind(),
                        "block" | "function_item" | "closure_expression" | "source_file"
                    ) {
                        break;
                    }
                    current = parent;
                }
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    pub(super) fn syntax(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<&'static str>, AdapterError> {
        if role == StructuralRole::Call && node.kind() == "identifier" {
            return Ok(Some("rust.direct_call"));
        }
        if role == StructuralRole::Call && node.kind() == "generic_function" {
            return Ok(Some("rust.generic_call"));
        }
        if role == StructuralRole::Signature {
            match node.kind() {
                "source_file" => return Ok(Some("rust.proof_context")),
                "attribute_item" | "inner_attribute_item" => return Ok(Some("rust.attribute")),
                "use_declaration"
                | "extern_crate_declaration"
                | "macro_definition"
                | "function_item"
                | "mod_item"
                | "impl_item"
                | "trait_item"
                | "const_item"
                | "static_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "type_item"
                | "foreign_mod_item" => return Ok(Some("rust.namespace_barrier")),
                _ => {}
            }
        }
        if role == StructuralRole::Scope
            && matches!(
                node.kind(),
                "for_expression" | "match_arm" | "if_expression" | "while_expression"
            )
        {
            return Ok(Some("rust.pattern_environment"));
        }
        if role == StructuralRole::Signature
            && node.kind() == "macro_invocation"
            && macro_pattern(node, cancellation)?
        {
            return Ok(Some("rust.pattern_macro_bindings_unavailable"));
        }
        if role == StructuralRole::Signature
            && node.parent().is_some_and(|parent| {
                matches!(parent.kind(), "if_expression" | "while_expression")
                    && parent.child_by_field_name("condition") == Some(node)
            })
        {
            return Ok(Some("rust.unavailable_pattern_environment"));
        }
        if role == StructuralRole::Signature
            && node.parent().is_some_and(|parent| {
                matches!(parent.kind(), "function_item" | "closure_expression")
                    && parent.child_by_field_name("body") == Some(node)
            })
        {
            return Ok(Some("rust.body"));
        }
        if matches!(node.kind(), "identifier" | "shorthand_field_identifier") {
            let binding = self.binding(node, source, cancellation)?;
            if matches!(
                role,
                StructuralRole::Declaration | StructuralRole::Definition
            ) {
                return Ok(match binding {
                    Some(Binding::Variable) => Some("rust.local_variable"),
                    Some(Binding::Parameter) => Some("rust.parameter"),
                    _ => None,
                });
            }
            if role == StructuralRole::Reference {
                return Ok(Some(if matches!(binding, Some(Binding::Unavailable)) {
                    "rust.pattern_binding_unavailable"
                } else if node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "scoped_identifier"
                            | "scoped_type_identifier"
                            | "generic_pattern"
                            | "macro_invocation"
                    ) || matches!(parent.kind(), "tuple_struct_pattern" | "struct_pattern")
                        && parent.child_by_field_name("type") == Some(node)
                }) {
                    "rust.path_name"
                } else {
                    "rust.value"
                }));
            }
        }
        Ok(match (node.kind(), role) {
            ("closure_expression", StructuralRole::Declaration) => Some("rust.anonymous_function"),
            ("closure_parameters", StructuralRole::Signature) => Some("rust.parameters"),
            ("let_declaration", StructuralRole::Signature) => Some("rust.let"),
            ("macro_invocation", StructuralRole::Signature) => {
                Some("rust.macro_bindings_unavailable")
            }
            ("or_pattern", StructuralRole::Signature) => Some("rust.pattern_bindings_unavailable"),
            (
                "for_expression" | "match_arm" | "if_expression" | "while_expression",
                StructuralRole::Signature,
            ) => Some("rust.unavailable_pattern_environment"),
            _ => None,
        })
    }

    pub(super) fn adjust(
        &self,
        node: Node<'_>,
        candidate: &mut QueryCandidate,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        if candidate.syntax == "rust.namespace_barrier"
            && node.kind() == "function_item"
            && let Some(parameters) = module_const_parameter_context(node, cancellation)?
        {
            // Const parameters introduce value names within this callable. A
            // whole-item barrier would also suppress unrelated sibling proofs.
            candidate.start = parameters.start_byte();
            candidate.end = parameters.end_byte();
            return Ok(());
        }
        if candidate.syntax != "rust.unavailable_pattern_environment" {
            return Ok(());
        }
        let range = match node.kind() {
            "for_expression" | "while_expression" => node
                .child_by_field_name("body")
                .map(|body| body.byte_range()),
            "if_expression" => node
                .child_by_field_name("consequence")
                .map(|body| body.byte_range()),
            "match_arm" => node.child_by_field_name("value").map(|value| {
                let start = node
                    .child_by_field_name("pattern")
                    .and_then(|pattern| pattern.child_by_field_name("condition"))
                    .map_or(value.start_byte(), |guard| guard.start_byte());
                start..value.end_byte()
            }),
            _ => first_let_end(node, cancellation)?.map(|end| end..node.end_byte()),
        };
        if let Some(range) = range {
            candidate.start = range.start;
            candidate.end = range.end;
        }
        Ok(())
    }
}

fn module_const_parameter_context<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(parameters) = node.child_by_field_name("type_parameters") else {
        return Ok(None);
    };
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        cancellation.check()?;
        if parameter.kind() == "const_parameter" {
            let mut parent = node.parent();
            while let Some(scope) = parent {
                cancellation.check()?;
                match scope.kind() {
                    "source_file" | "mod_item" => return Ok(Some(parameters)),
                    "block" | "impl_item" | "trait_item" | "function_item" => return Ok(None),
                    _ => parent = scope.parent(),
                }
            }
            return Ok(None);
        }
    }
    Ok(None)
}

fn macro_pattern(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        cancellation.check()?;
        match parent.kind() {
            "let_declaration" | "let_condition" | "parameter" | "for_expression" => {
                return Ok(parent.child_by_field_name("pattern") == Some(current));
            }
            "match_pattern" | "closure_parameters" => return Ok(true),
            "tuple_pattern" | "slice_pattern" | "mut_pattern" | "ref_pattern"
            | "reference_pattern" | "captured_pattern" | "or_pattern" => {}
            "field_pattern" if parent.child_by_field_name("pattern") == Some(current) => {}
            "struct_pattern" | "tuple_struct_pattern"
                if parent.child_by_field_name("type") != Some(current) => {}
            _ => return Ok(false),
        }
        current = parent;
    }
    Ok(false)
}

fn has_let_condition(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let Some(condition) = node.child_by_field_name("condition") else {
        return Ok(false);
    };
    Ok(first_let_end(condition, cancellation)?.is_some())
}

fn first_let_end(
    condition: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut pending = vec![condition];
    let mut first = None;
    while let Some(node) = pending.pop() {
        cancellation.check()?;
        if node.kind() == "let_condition" {
            first = Some(first.map_or(node.end_byte(), |end: usize| end.min(node.end_byte())));
            continue;
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(first)
}
