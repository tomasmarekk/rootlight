//! Written QML owners and id calls require a complete native census.
//! Every unproved receiver remains unresolved; spelling never repairs a barrier.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn text<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn bare<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if !matches!(node.kind(), "identifier" | "property_identifier")
        || node.has_error()
        || node.is_missing()
    {
        return None;
    }
    let name = text(node, source)?;
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    ((first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')))
    .then_some(name)
}

fn id_name<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let name = bare(node, source)?;
    let first = *name.as_bytes().first()?;
    ((first.is_ascii_lowercase() || first == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !matches!(
            name,
            "as" | "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "enum"
                | "export"
                | "extends"
                | "false"
                | "finally"
                | "for"
                | "function"
                | "if"
                | "implements"
                | "import"
                | "in"
                | "instanceof"
                | "interface"
                | "let"
                | "new"
                | "null"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "static"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typeof"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
        ))
    .then_some(name)
}

fn property_name<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let name = id_name(node, source)?;
    name.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_lowercase)
        .then_some(name)
}

fn nearest<'tree>(
    node: Node<'tree>,
    kinds: &[&str],
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        cancel.check()?;
        if kinds.contains(&current.kind()) {
            return Ok(Some(current));
        }
        parent = current.parent();
    }
    Ok(None)
}

fn object<'tree>(
    node: Node<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    nearest(node, &["ui_object_definition"], cancel)
}

fn component(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        cancel.check()?;
        if current.kind() == "ui_inline_component"
            || (current.kind() == "ui_object_definition"
                && current
                    .child_by_field_name("type_name")
                    .and_then(|name| text(name, source))
                    == Some("Component"))
        {
            return Ok(Some(current.start_byte()));
        }
        parent = current.parent();
    }
    Ok(None)
}

fn direct_member(node: Node<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "ui_object_initializer")
}

fn plain_object(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "ui_object_definition"
        && node
            .child_by_field_name("type_name")
            .filter(|name| name.kind() == "identifier")
            .and_then(|name| text(name, source))
            .is_some_and(|name| name.as_bytes().first().is_some_and(u8::is_ascii_uppercase))
}

fn supported_member(node: Node<'_>, source: &[u8]) -> bool {
    direct_member(node)
        && node
            .parent()
            .and_then(|parent| parent.parent())
            .is_some_and(|owner| plain_object(owner, source))
}

#[derive(Default)]
struct Object<'s> {
    component: Option<usize>,
    end: usize,
    id_bindings: usize,
    ids: Vec<(&'s str, usize, usize)>,
    members: HashMap<&'s str, usize>,
}

#[derive(Default)]
struct Function<'s> {
    shadows: HashSet<&'s str>,
    barrier: bool,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    maximum: usize,
    initialized: bool,
    complete: bool,
    exhausted: bool,
    objects: HashMap<usize, Object<'s>>,
    ids: HashMap<(Option<usize>, &'s str), Option<usize>>,
    methods: HashMap<&'s str, Option<usize>>,
    functions: HashMap<usize, Function<'s>>,
    written_names: HashSet<&'s str>,
    global_barrier: bool,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize) -> Self {
        Self {
            source,
            maximum,
            initialized: false,
            complete: false,
            exhausted: false,
            objects: HashMap::new(),
            ids: HashMap::new(),
            methods: HashMap::new(),
            functions: HashMap::new(),
            written_names: HashSet::new(),
            global_barrier: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn initialize(
        &mut self,
        mut root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        while let Some(parent) = root.parent() {
            cancel.check()?;
            root = parent;
        }
        if root.kind() != "program" {
            return Ok(());
        }
        let mut cursor = root.walk();
        let mut visited = 0;
        loop {
            cancel.check()?;
            if visited >= self.maximum {
                self.exhausted = true;
                return Ok(());
            }
            visited += 1;
            let node = cursor.node();
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "program")
                && !matches!(
                    node.kind(),
                    "ui_pragma" | "ui_import" | "ui_object_definition" | "comment"
                )
            {
                self.global_barrier = true;
            }
            if node.kind() == "ui_annotated_object_member" {
                self.global_barrier = true;
            }
            if node.kind() == "ui_object_definition" {
                self.objects
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let entry = self.objects.entry(node.start_byte()).or_default();
                entry.component = component(node, self.source, cancel)?;
                entry.end = node.end_byte();
            }
            if direct_member(node)
                && let Some(owner) = object(node, cancel)?
            {
                self.objects
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let entry = self.objects.entry(owner.start_byte()).or_default();
                if node.kind() == "ui_binding"
                    && node
                        .child_by_field_name("name")
                        .and_then(|name| bare(name, self.source))
                        == Some("id")
                {
                    entry.id_bindings = entry
                        .id_bindings
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                    let value = node
                        .child_by_field_name("value")
                        .filter(|value| {
                            value.kind() == "expression_statement" && value.named_child_count() == 1
                        })
                        .and_then(|value| value.named_child(0));
                    if let Some(value) = value
                        && let Some(name) = id_name(value, self.source)
                    {
                        entry
                            .ids
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        entry.ids.push((name, value.start_byte(), value.end_byte()));
                        self.ids
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.ids
                            .entry((entry.component, name))
                            .and_modify(|id| *id = None)
                            .or_insert(Some(owner.start_byte()));
                    } else {
                        self.global_barrier = true;
                    }
                } else if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| bare(name, self.source))
                {
                    entry
                        .members
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    let count = entry.members.entry(name).or_default();
                    *count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    if node.kind() == "function_declaration" {
                        self.methods
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.methods
                            .entry(name)
                            .and_modify(|method| *method = None)
                            .or_insert(Some(owner.start_byte()));
                    }
                } else if node.child_by_field_name("name").is_some() {
                    self.global_barrier = true;
                }
            }
            let function = nearest(
                node,
                &[
                    "function_declaration",
                    "function_expression",
                    "arrow_function",
                    "generator_function",
                    "generator_function_declaration",
                    "method_definition",
                ],
                cancel,
            )?;
            if let Some(function) = function {
                self.functions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let scope = self.functions.entry(function.start_byte()).or_default();
                let parameter = nearest(node, &["formal_parameters"], cancel)?
                    .is_some_and(|parameters| parameters.start_byte() >= function.start_byte());
                if parameter
                    && matches!(
                        node.kind(),
                        "object_pattern"
                            | "array_pattern"
                            | "rest_pattern"
                            | "assignment_pattern"
                            | "shorthand_property_identifier_pattern"
                    )
                {
                    scope.barrier = true;
                }
                if node.kind() == "identifier" && parameter {
                    if let Some(name) = bare(node, self.source) {
                        scope
                            .shadows
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        scope.shadows.insert(name);
                    } else {
                        scope.barrier = true;
                    }
                }
                if matches!(
                    node.kind(),
                    "variable_declarator"
                        | "function_declaration"
                        | "generator_function_declaration"
                        | "enum_declaration"
                        | "class_declaration"
                        | "catch_clause"
                ) {
                    let name = node
                        .child_by_field_name("name")
                        .or_else(|| node.child_by_field_name("parameter"));
                    if let Some(name) = name.and_then(|name| bare(name, self.source)) {
                        scope
                            .shadows
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        scope.shadows.insert(name);
                    } else {
                        scope.barrier = true;
                    }
                }
            }
            if node.kind() == "with_statement" {
                self.global_barrier = true;
            }
            if node.kind() == "ui_import"
                && let Some(alias) = node
                    .child_by_field_name("alias")
                    .and_then(|alias| bare(alias, self.source))
            {
                self.written_names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.written_names.insert(alias);
            }
            if node.kind() == "ui_inline_component"
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| bare(name, self.source))
            {
                self.written_names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.written_names.insert(name);
            }
            if matches!(
                node.kind(),
                "assignment_expression"
                    | "augmented_assignment_expression"
                    | "update_expression"
                    | "for_in_statement"
            ) {
                let left = node
                    .child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("argument"));
                let name = left
                    .and_then(|left| {
                        if left.kind() == "member_expression" {
                            left.child_by_field_name("property")
                        } else {
                            Some(left)
                        }
                    })
                    .and_then(|name| bare(name, self.source));
                if let Some(name) = name {
                    self.written_names
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.written_names.insert(name);
                } else {
                    self.global_barrier = true;
                }
            }
            if node.kind() == "call_expression"
                && node
                    .child_by_field_name("function")
                    .and_then(|head| bare(head, self.source))
                    .is_some_and(|name| matches!(name, "eval" | "Function"))
            {
                self.global_barrier = true;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.complete = !root.has_error();
                    return Ok(());
                }
            }
        }
    }

    fn id(&self, node: Node<'_>) -> Option<(usize, usize)> {
        let owner = self.objects.get(&node.start_byte())?;
        let [(_, start, end)] = owner.ids.as_slice() else {
            return None;
        };
        (!node.has_error() && plain_object(node, self.source) && owner.id_bindings == 1)
            .then_some((*start, *end))
    }

    fn call<'tree>(
        &self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.complete || self.global_barrier || node.has_error() {
            return Ok(None);
        }
        let Some(head) = node
            .child_by_field_name("function")
            .filter(|head| head.kind() == "member_expression")
        else {
            return Ok(None);
        };
        let Some(receiver) = head.child_by_field_name("object") else {
            return Ok(None);
        };
        let Some(property) = head.child_by_field_name("property") else {
            return Ok(None);
        };
        let (Some(id), Some(method)) = (bare(receiver, self.source), bare(property, self.source))
        else {
            return Ok(None);
        };
        if std::str::from_utf8(&self.source[receiver.end_byte()..property.start_byte()])
            .ok()
            .map(str::trim)
            != Some(".")
        {
            return Ok(None);
        }
        let Some(function) = nearest(
            node,
            &[
                "function_declaration",
                "function_expression",
                "arrow_function",
                "generator_function",
                "generator_function_declaration",
                "method_definition",
            ],
            cancel,
        )?
        else {
            return Ok(None);
        };
        if function.kind() != "function_declaration" || !supported_member(function, self.source) {
            return Ok(None);
        }
        if nearest(
            node,
            &["class_body", "class_static_block", "internal_module"],
            cancel,
        )?
        .is_some()
        {
            return Ok(None);
        }
        if self
            .functions
            .get(&function.start_byte())
            .is_some_and(|scope| scope.barrier || scope.shadows.contains(id))
            || self.written_names.contains(id)
            || self.written_names.contains(method)
        {
            return Ok(None);
        }
        let component = component(node, self.source, cancel)?;
        let Some(target) = self.ids.get(&(component, id)).copied().flatten() else {
            return Ok(None);
        };
        if self.methods.get(method).copied().flatten() != Some(target) {
            return Ok(None);
        }
        let Some(owner) = self.objects.get(&target) else {
            return Ok(None);
        };
        if owner.ids.len() != 1 || owner.members.get(method) != Some(&1) {
            return Ok(None);
        }
        for object in self.objects.values() {
            cancel.check()?;
            if object.component == component && object.members.contains_key(id) {
                return Ok(None);
            }
        }
        if !self.eligible_ancestry(function, cancel)? {
            return Ok(None);
        }
        let mut root = node;
        while let Some(parent) = root.parent() {
            cancel.check()?;
            root = parent;
        }
        let Some(target_node) = root
            .named_descendant_for_byte_range(target, owner.end)
            .filter(|node| {
                node.kind() == "ui_object_definition"
                    && node.start_byte() == target
                    && node.end_byte() == owner.end
            })
        else {
            return Ok(None);
        };
        if !self.eligible_ancestry(target_node, cancel)? {
            return Ok(None);
        }
        Ok(Some(property))
    }

    fn eligible_ancestry(
        &self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut parent = Some(node);
        while let Some(current) = parent {
            cancel.check()?;
            if current.kind() == "ui_inline_component" {
                break;
            }
            if matches!(
                current.kind(),
                "ui_binding"
                    | "ui_property"
                    | "ui_object_array"
                    | "class_body"
                    | "class_static_block"
                    | "internal_module"
            ) {
                return Ok(false);
            }
            if current.kind() == "ui_object_definition" {
                let name = current
                    .child_by_field_name("type_name")
                    .and_then(|name| text(name, self.source));
                let root = current.parent().is_some_and(|parent| {
                    matches!(parent.kind(), "program" | "ui_inline_component")
                });
                if name.is_some_and(|name| self.written_names.contains(name))
                    || (!root
                        && !matches!(
                            name,
                            Some("Connections" | "Item" | "QtObject" | "Rectangle")
                        ))
                {
                    return Ok(false);
                }
            }
            parent = current.parent();
        }
        Ok(true)
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if node.kind() != "program" {
        cache.initialize(node, cancel)?;
    }
    let mut range = node.byte_range();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module => {
            range = 0..source.len();
            "qml.file"
        }
        StructuralRole::Scope => match node.kind() {
            "program" => {
                range = 0..source.len();
                "qml.file"
            }
            "ui_object_definition" => "qml.object",
            "ui_inline_component" => "qml.component",
            "function_declaration" if supported_member(node, source) => "qml.function",
            "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "generator_function"
            | "generator_function_declaration"
            | "method_definition" => "qml.detached_function",
            _ => return Err(query_failure("query-qml-scope")),
        },
        StructuralRole::Declaration if node.kind() == "ui_object_definition" => {
            if cache.id(node).is_some() {
                "qml.id_variable"
            } else if cache
                .objects
                .get(&node.start_byte())
                .is_some_and(|owner| owner.id_bindings > 0)
            {
                "qml.unavailable_id"
            } else {
                return Ok(None);
            }
        }
        StructuralRole::Declaration
            if node.kind() == "ui_property"
                && supported_member(node, source)
                && !node.has_error()
                && node
                    .child_by_field_name("name")
                    .and_then(|name| property_name(name, source))
                    .is_some() =>
        {
            "qml.property"
        }
        StructuralRole::Declaration
            if node.kind() == "function_declaration"
                && supported_member(node, source)
                && !node.has_error() =>
        {
            "qml.method"
        }
        StructuralRole::Declaration => "qml.unsupported",
        StructuralRole::Definition => {
            if node.kind() == "ui_object_definition" {
                let Some((start, end)) = cache.id(node) else {
                    return Ok(None);
                };
                range = start..end;
            } else if matches!(node.kind(), "ui_property" | "function_declaration")
                && supported_member(node, source)
                && !node.has_error()
            {
                let Some(name) = node
                    .child_by_field_name("name")
                    .filter(|name| bare(*name, source).is_some())
                else {
                    return Ok(None);
                };
                if node.kind() == "ui_property" && property_name(name, source).is_none() {
                    return Ok(None);
                }
                range = name.byte_range();
            } else {
                return Ok(None);
            }
            "qml.name"
        }
        StructuralRole::Signature => {
            if !supported_member(node, source) || node.has_error() {
                return Ok(None);
            }
            let Some(body) = node.child_by_field_name("body") else {
                return Ok(None);
            };
            range = node.start_byte()..body.start_byte();
            "qml.header"
        }
        StructuralRole::Call => {
            if let Some(property) = cache.call(node, cancel)? {
                range = property.byte_range();
                "qml.unique_id_method"
            } else {
                if let Some(head) = node.child_by_field_name("function") {
                    range = head.byte_range();
                }
                "qml.unresolved"
            }
        }
        StructuralRole::Expression => "qml.unsupported",
        StructuralRole::Comment => "qml.comment",
        StructuralRole::StringLiteral => "qml.string",
        _ => return Err(query_failure("query-qml-role")),
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancel)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    fn certified(source: &[u8], maximum: usize) -> (Vec<Range<usize>>, bool) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_qmljs::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "synthetic QML must parse cleanly: {}",
            String::from_utf8_lossy(source)
        );
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, maximum);
        cache.initialize(tree.root_node(), &cancel).unwrap();
        let mut cursor = tree.root_node().walk();
        let mut proven = Vec::new();
        loop {
            let node = cursor.node();
            if node.kind() == "call_expression"
                && let Some(name) = cache.call(node, &cancel).unwrap()
            {
                proven.push(name.byte_range());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return (proven, cache.exhausted());
                }
            }
        }
    }

    #[test]
    fn exact_original_call_certification_requires_complete_native_census() {
        let source = include_bytes!("../../tests/fixtures/qml/SettingsUiState.qml");
        let (calls, exhausted) = certified(source, 4096);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], 456..476);
        assert!(!exhausted);
        assert_eq!(certified(source, 1), (Vec::new(), true));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_qmljs::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, 4096);
        assert!(matches!(
            candidate(
                tree.root_node(),
                StructuralRole::Root,
                source,
                &cancel,
                &mut cache
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn native_receivers_do_not_cross_local_duplicate_or_explicit_component_barriers() {
        for nested in [
            "Connections { function handler(root) { root.target(); } }",
            "Connections { function handler({root}) { root.target(); } }",
            "Connections { function handler({nested: {root}}) { root.target(); } }",
            "Connections { function handler(root = other) { root.target(); } }",
            "Connections { function handler(...root) { root.target(); } }",
            "Connections { function handler() { let root = value; root.target(); } }",
            "Connections { function handler() { root.target(); } } Item { id: root }",
            "Connections { function handler() { root.target(); } } Item { function target() {} }",
            "component Other: Item { function handler() { root.target(); } }",
            "Component { Item { function handler() { root.target(); } } }",
            "delegate: Item { function handler() { root.target(); } }",
        ] {
            let source =
                format!("import QtQuick\nItem {{ id: root; function target() {{}} {nested} }}");
            assert!(certified(source.as_bytes(), 4096).0.is_empty(), "{source}");
        }
        let source = b"import QtQuick\nItem { id: root; function target() {} Item { function handler(value) { let other = value; root.target(); } } }";
        assert_eq!(certified(source, 4096).0.len(), 1);
        let factory = b"Component { id: root; function target() {} Item { function handler() { root.target(); } } }";
        assert!(certified(factory, 4096).0.is_empty());
        for target in [
            "delegate: Item { id: targetRoot; function target() {} }",
            "UnprovedType { id: targetRoot; function target() {} }",
            "property Component factory: Item { id: targetRoot; function target() {} }",
            "Component { Item { id: targetRoot; function target() {} } }",
            "component Other: Item { id: targetRoot; function target() {} }",
        ] {
            let source = format!(
                "import QtQuick\nListView {{ {target}\n function handler() {{ targetRoot.target(); }} }}"
            );
            assert!(certified(source.as_bytes(), 4096).0.is_empty(), "{source}");
        }
    }
}
