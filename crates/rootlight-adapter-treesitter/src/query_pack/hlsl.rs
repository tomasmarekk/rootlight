//! HLSL captures preserve complete physical owners and unresolved written uses.
//! Native C++ recognition is not shader binding evidence; buffer labels never
//! become entities, name occurrences or named scopes in this dedicated contract.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    name: Option<Range<usize>>,
    syntax: &'static str,
    declarators: usize,
    declarator_extent: Option<Range<usize>>,
}

#[derive(Default)]
struct CacheEntry {
    owner: Option<Option<OwnerProof>>,
    chain: Option<bool>,
    uses: Option<UseContext>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UseContext {
    Expression,
    Declarator,
    Excluded,
}

/// Owner and context proofs share the caller's existing capture/body ceiling.
pub(super) struct CaptureCache {
    entries: HashMap<usize, CacheEntry>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    /// Creates a bounded cache without reserving source-sized storage.
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    /// Reports a proof bound that the caller must propagate as a capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn admit_entry(&mut self, id: usize) -> Result<bool, AdapterError> {
        if self.entries.contains_key(&id) {
            return Ok(true);
        }
        if self.entries.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(false);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.entries.insert(id, CacheEntry::default());
        Ok(true)
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if !self.admit_entry(node.id())? {
            return Ok(None);
        }
        if let Some(proof) = self
            .entries
            .get(&node.id())
            .and_then(|entry| entry.owner.as_ref())
        {
            return Ok(proof.clone());
        }
        let proof = owner_proof(node, source, cancellation, self)?;
        if let Some(entry) = self.entries.get_mut(&node.id()) {
            entry.owner = Some(proof.clone());
        }
        Ok(proof)
    }

    fn chain(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = Some(node);
        let mut pending = Vec::new();
        let mut valid = true;
        while let Some(owner) = current {
            cancellation.check()?;
            if !self.admit_entry(owner.id())? {
                return Ok(false);
            }
            if let Some(cached) = self.entries.get(&owner.id()).and_then(|entry| entry.chain) {
                valid = cached;
                break;
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(owner.id());
            if owner.is_missing()
                || owner.kind() == "ERROR"
                || unsupported_cpp(owner)
                || (owner_kind(owner) && self.owner(owner, source, cancellation)?.is_none())
            {
                valid = false;
                break;
            }
            current = owner.parent();
        }
        for id in pending {
            cancellation.check()?;
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.chain = Some(valid);
            }
        }
        Ok(valid)
    }

    fn uses(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<UseContext, AdapterError> {
        let mut current = Some(node);
        let mut pending = Vec::new();
        let mut context = UseContext::Expression;
        while let Some(owner) = current {
            cancellation.check()?;
            if !self.admit_entry(owner.id())? {
                return Ok(UseContext::Excluded);
            }
            if let Some(cached) = self.entries.get(&owner.id()).and_then(|entry| entry.uses) {
                context = cached;
                break;
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(owner);
            current = owner.parent();
        }
        for child in pending.into_iter().rev() {
            cancellation.check()?;
            if let Some(parent) = child.parent() {
                context = child_use_context(context, parent, child);
            }
            if let Some(entry) = self.entries.get_mut(&child.id()) {
                entry.uses = Some(context);
            }
        }
        Ok(context)
    }
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Option<Node<'tree>> {
    node.child_by_field_name(name)
        .filter(|child| child.parent() == Some(node))
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn text<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn written_name(node: Node<'_>, source: &[u8]) -> bool {
    matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier"
    ) && clean(node, source)
        && text(node, source).is_some_and(|raw| {
            structural_captured_name_for_language("hlsl", raw, raw.len()).as_deref() == Some(raw)
        })
}

fn boundary(
    node: Node<'_>,
    source: &[u8],
    opening: &[u8],
    closing: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !clean(node, source) {
        return Ok(false);
    }
    let mut first = None;
    let mut last = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() {
            first.get_or_insert(child);
            last = Some(child);
        }
    }
    Ok(matches!((first, last), (Some(first), Some(last))
        if clean(first, source) && clean(last, source)
            && first.start_byte() == node.start_byte()
            && last.end_byte() == node.end_byte()
            && source.get(first.byte_range()) == Some(opening)
            && source.get(last.byte_range()) == Some(closing)))
}

fn semicolon<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut last = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() {
            last = Some(child);
        }
    }
    Ok(last.filter(|child| {
        clean(*child, source)
            && child.end_byte() == node.end_byte()
            && text(*child, source) == Some(";")
    }))
}

fn header_before(
    node: Node<'_>,
    delimiter: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    let mut end = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.start_byte() >= delimiter.start_byte() {
            break;
        }
        if !child.is_extra() && child.end_byte() <= delimiter.start_byte() {
            end = Some(child.end_byte());
        }
    }
    Ok(end
        .filter(|end| *end > node.start_byte())
        .map(|end| node.start_byte()..end))
}

fn unsupported_cpp(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "type_definition"
            | "pointer_declarator"
            | "abstract_pointer_declarator"
            | "reference_declarator"
            | "abstract_reference_declarator"
            | "attributed_declarator"
            | "variadic_declarator"
            | "variadic_parameter_declaration"
            | "attribute_specifier"
            | "attribute_declaration"
            | "ms_declspec_modifier"
            | "ms_call_modifier"
            | "gnu_asm_expression"
            | "namespace_definition"
            | "namespace_alias_definition"
            | "template_declaration"
            | "alias_declaration"
            | "using_declaration"
            | "friend_declaration"
            | "concept_definition"
            | "base_class_clause"
            | "virtual_specifier"
            | "operator_cast"
            | "operator_name"
            | "qualified_identifier"
            | "destructor_name"
            | "new_expression"
            | "delete_expression"
            | "lambda_expression"
            | "pointer_expression"
    )
}

fn prefix_valid(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if unsupported_cpp(child)
            || (child.kind() == "storage_class_specifier"
                && !matches!(text(child, source), Some("static" | "extern" | "inline")))
            || (child.kind() == "type_qualifier"
                && !matches!(text(child, source), Some("const" | "volatile")))
            || (child.kind() == "hlsl_attribute"
                && !boundary(child, source, b"[", b"]", cancellation)?)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn shader_type(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if !clean(node, source) {
        return Ok(false);
    }
    Ok(match node.kind() {
        "primitive_type" => matches!(
            text(node, source),
            Some("void" | "bool" | "int" | "uint" | "float" | "double")
        ),
        "type_identifier" => written_name(node, source),
        "template_type" => {
            field(node, "name").is_some_and(|name| written_name(name, source))
                && match field(node, "arguments") {
                    Some(arguments) => boundary(arguments, source, b"<", b">", cancellation)?,
                    None => false,
                }
        }
        "struct_specifier" => {
            field(node, "name").is_some_and(|name| written_name(name, source))
                && match field(node, "body") {
                    Some(body) => boundary(body, source, b"{", b"}", cancellation)?,
                    None => false,
                }
                && prefix_valid(node, source, cancellation)?
        }
        _ => false,
    })
}

fn semantics_shape(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.kind() != "semantics" || !clean(node, source) {
        return Ok(false);
    }
    let mut stage = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        stage = match stage {
            0 if text(child, source) == Some(":") && child.start_byte() == node.start_byte() => 1,
            1 if child.kind() == "identifier"
                && written_name(child, source)
                && child.end_byte() == node.end_byte() =>
            {
                2
            }
            _ => return Ok(false),
        };
    }
    Ok(stage == 2)
}

fn array_shape(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(declarator) = field(node, "declarator") else {
        return Ok(false);
    };
    let size = field(node, "size");
    if size.is_some_and(|size| !size.is_named() || !clean(size, source)) {
        return Ok(false);
    }
    let mut stage = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        stage = match stage {
            0 if child == declarator && child.start_byte() == node.start_byte() => 1,
            1 if text(child, source) == Some("[") => 2,
            2 if Some(child) == size => 3,
            2 if size.is_none()
                && text(child, source) == Some("]")
                && child.end_byte() == node.end_byte() =>
            {
                4
            }
            3 if text(child, source) == Some("]") && child.end_byte() == node.end_byte() => 4,
            _ => return Ok(false),
        };
    }
    Ok(stage == 4)
}

fn declarator_name<'tree>(
    mut node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let function = node.kind() == "function_declarator";
    loop {
        cancellation.check()?;
        if !clean(node, source) {
            return Ok(None);
        }
        match node.kind() {
            "identifier" | "field_identifier" => {
                return Ok(written_name(node, source).then_some(node));
            }
            "array_declarator" | "init_declarator" => {
                if node.kind() == "array_declarator" && !array_shape(node, source, cancellation)? {
                    return Ok(None);
                }
                let Some(child) = field(node, "declarator") else {
                    return Ok(None);
                };
                if child.start_byte() != node.start_byte() {
                    return Ok(None);
                }
                if node.kind() == "init_declarator" {
                    let Some(value) = field(node, "value") else {
                        return Ok(None);
                    };
                    let mut equals = false;
                    let mut cursor = node.walk();
                    for token in node.children(&mut cursor) {
                        cancellation.check()?;
                        if !token.is_extra() && token != child && token != value {
                            if equals || text(token, source) != Some("=") {
                                return Ok(None);
                            }
                            equals = true;
                        }
                    }
                    if !equals || value.end_byte() != node.end_byte() {
                        return Ok(None);
                    }
                }
                node = child;
            }
            "function_declarator" if function => {
                let (Some(name), Some(parameters)) =
                    (field(node, "declarator"), field(node, "parameters"))
                else {
                    return Ok(None);
                };
                if name.kind() != "identifier"
                    || !written_name(name, source)
                    || name.start_byte() != node.start_byte()
                    || !boundary(parameters, source, b"(", b")", cancellation)?
                {
                    return Ok(None);
                }
                let mut tail = parameters.end_byte();
                let mut semantic = false;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    cancellation.check()?;
                    if child.is_extra() || child == name || child == parameters {
                        continue;
                    }
                    if semantic
                        || child.start_byte() < tail
                        || !semantics_shape(child, source, cancellation)?
                    {
                        return Ok(None);
                    }
                    semantic = true;
                    tail = child.end_byte();
                }
                if tail != node.end_byte() {
                    return Ok(None);
                }
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    cancellation.check()?;
                    if !parameter.is_extra() && !parameter_shape(parameter, source, cancellation)? {
                        return Ok(None);
                    }
                }
                return Ok(Some(name));
            }
            _ => return Ok(None),
        }
    }
}

fn parameter_shape(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.kind() != "parameter_declaration"
        || !clean(node, source)
        || !prefix_valid(node, source, cancellation)?
    {
        return Ok(false);
    }
    let Some(ty) = field(node, "type") else {
        return Ok(false);
    };
    if !shader_type(ty, source, cancellation)? {
        return Ok(false);
    }
    let declarator = field(node, "declarator");
    if let Some(declarator) = declarator
        && (!matches!(declarator.kind(), "identifier" | "array_declarator")
            || declarator_name(declarator, source, cancellation)?.is_none())
    {
        return Ok(false);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == "semantics" && !semantics_shape(child, source, cancellation)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn conditional(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef" | "preproc_else"
    )
}

fn physical_position(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut child = node;
    while let Some(parent) = child.parent() {
        cancellation.check()?;
        if conditional(parent) {
            if field(parent, "condition") == Some(child) || field(parent, "name") == Some(child) {
                return Ok(false);
            }
            child = parent;
            continue;
        }
        return Ok(matches!(
            parent.kind(),
            "translation_unit" | "compound_statement" | "field_declaration_list"
        ) || (parent.kind() == "for_statement"
            && field(parent, "initializer") == Some(child)));
    }
    Ok(false)
}

fn declarator_parent(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(
        node.kind(),
        "identifier"
            | "field_identifier"
            | "init_declarator"
            | "array_declarator"
            | "function_declarator"
    ) {
        return None;
    }
    let parent = node.parent()?;
    matches!(parent.kind(), "declaration" | "field_declaration").then_some(parent)
}

fn owner_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_definition"
            | "declaration"
            | "field_declaration"
            | "parameter_declaration"
            | "optional_parameter_declaration"
            | "struct_specifier"
            | "cbuffer_specifier"
            | "compound_statement"
            | "field_declaration_list"
    )
}

fn declaration_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
    mut proof: OwnerProof,
) -> Result<Option<OwnerProof>, AdapterError> {
    if !physical_position(node, cancellation)? || !prefix_valid(node, source, cancellation)? {
        return Ok(None);
    }
    let (Some(ty), Some(terminal)) = (field(node, "type"), semicolon(node, source, cancellation)?)
    else {
        return Ok(None);
    };
    if !shader_type(ty, source, cancellation)? {
        return Ok(None);
    }
    let Some(header) = header_before(node, terminal, cancellation)? else {
        return Ok(None);
    };
    proof.header = header;
    proof.syntax = "hlsl.declaration_context";
    if node.kind() == "field_declaration" {
        let Some(container) = node.parent().and_then(|body| {
            (body.kind() == "field_declaration_list")
                .then(|| body.parent())
                .flatten()
        }) else {
            return Ok(None);
        };
        if !matches!(container.kind(), "struct_specifier" | "cbuffer_specifier")
            || cache.owner(container, source, cancellation)?.is_none()
        {
            return Ok(None);
        }
        proof.syntax = if container.kind() == "cbuffer_specifier" {
            "hlsl.buffer_declaration_context"
        } else {
            "hlsl.field_declaration_context"
        };
    }
    let mut function = false;
    let mut annotation_stage = 0;
    let mut cursor = node.walk();
    for declarator in node.children_by_field_name("declarator", &mut cursor) {
        cancellation.check()?;
        // This grammar aliases both ':' and its expression as declarator-field
        // semantics nodes. Neither alias is a declared name or an ordinary use.
        if declarator.kind() == "semantics" {
            if !clean(declarator, source) || proof.declarators == 0 {
                return Ok(None);
            }
            annotation_stage = match annotation_stage {
                0 if text(declarator, source) == Some(":") => 1,
                0 if declarator
                    .child(0)
                    .is_some_and(|child| text(child, source) == Some(":")) =>
                {
                    2
                }
                1 if declarator.is_named() && text(declarator, source).is_some() => 2,
                _ => return Ok(None),
            };
            continue;
        }
        if annotation_stage == 1
            || declarator_name(declarator, source, cancellation)?.is_none()
            || (node.kind() == "field_declaration" && declarator.kind() == "function_declarator")
        {
            return Ok(None);
        }
        annotation_stage = 0;
        function |= declarator.kind() == "function_declarator";
        proof.declarators = proof
            .declarators
            .checked_add(1)
            .ok_or_else(|| query_failure("query-hlsl-declarator-count"))?;
        match &mut proof.declarator_extent {
            Some(extent) => extent.end = declarator.end_byte(),
            None => proof.declarator_extent = Some(declarator.byte_range()),
        }
    }
    if annotation_stage == 1
        || (function && proof.declarators != 1)
        || (proof.declarators == 0 && ty.kind() != "struct_specifier")
    {
        return Ok(None);
    }
    Ok(Some(proof))
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if !clean(node, source) {
        return Ok(None);
    }
    let mut proof = OwnerProof {
        extent: node.byte_range(),
        header: node.byte_range(),
        name: None,
        syntax: "hlsl.body",
        declarators: 0,
        declarator_extent: None,
    };
    if let Some(parent) = declarator_parent(node) {
        let Some(parent_proof) = cache.owner(parent, source, cancellation)? else {
            return Ok(None);
        };
        if !parent_proof
            .declarator_extent
            .as_ref()
            .is_some_and(|extent| {
                extent.start <= node.start_byte() && node.end_byte() <= extent.end
            })
        {
            return Ok(None);
        }
        proof.syntax = match (parent_proof.syntax, node.kind(), parent_proof.declarators) {
            ("hlsl.declaration_context", "function_declarator", 1) => "hlsl.prototype",
            ("hlsl.declaration_context", _, 1) => "hlsl.variable",
            ("hlsl.declaration_context", _, _) => "hlsl.variable_fragment",
            ("hlsl.field_declaration_context", _, 1) => "hlsl.field",
            ("hlsl.field_declaration_context", _, _) => "hlsl.field_fragment",
            ("hlsl.buffer_declaration_context", _, 1) => "hlsl.buffer_member",
            ("hlsl.buffer_declaration_context", _, _) => "hlsl.buffer_member_fragment",
            _ => return Ok(None),
        };
        if parent_proof.declarators == 1 {
            proof.extent = parent_proof.extent;
            proof.header = parent_proof.header;
        }
        proof.name = declarator_name(node, source, cancellation)?.map(|name| name.byte_range());
        proof.declarators = parent_proof.declarators;
        return Ok(proof.name.is_some().then_some(proof));
    }
    match node.kind() {
        "function_definition" => {
            if !physical_position(node, cancellation)? || !prefix_valid(node, source, cancellation)?
            {
                return Ok(None);
            }
            let (Some(ty), Some(declarator), Some(body)) = (
                field(node, "type"),
                field(node, "declarator"),
                field(node, "body"),
            ) else {
                return Ok(None);
            };
            if declarator.kind() != "function_declarator"
                || body.kind() != "compound_statement"
                || !shader_type(ty, source, cancellation)?
                || !boundary(body, source, b"{", b"}", cancellation)?
            {
                return Ok(None);
            }
            proof.name =
                declarator_name(declarator, source, cancellation)?.map(|name| name.byte_range());
            proof.header = node.start_byte()..declarator.end_byte();
            proof.syntax = "hlsl.function";
        }
        "declaration" | "field_declaration" => {
            return declaration_proof(node, source, cancellation, cache, proof);
        }
        "struct_specifier" | "cbuffer_specifier" => {
            let (Some(name), Some(body)) = (field(node, "name"), field(node, "body")) else {
                return Ok(None);
            };
            let introducer = if node.kind() == "struct_specifier" {
                "struct"
            } else {
                "cbuffer"
            };
            if !written_name(name, source)
                || !boundary(body, source, b"{", b"}", cancellation)?
                || !prefix_valid(node, source, cancellation)?
                || node
                    .child(0)
                    .is_none_or(|first| text(first, source) != Some(introducer))
            {
                return Ok(None);
            }
            let Some(header) = header_before(node, body, cancellation)? else {
                return Ok(None);
            };
            proof.header = header;
            if node.kind() == "cbuffer_specifier" {
                if !physical_position(node, cancellation)?
                    || field(node, "binding").is_some_and(|binding| {
                        binding.kind() != "resource_binding" || !clean(binding, source)
                    })
                {
                    return Ok(None);
                }
                // The written buffer label stays in raw source context, never
                // in the name slot consumed by declaration/definition lowering.
                proof.syntax = "hlsl.buffer_context";
                return Ok(Some(proof));
            }
            proof.name = Some(name.byte_range());
            proof.syntax = "hlsl.struct";
        }
        "parameter_declaration" => {
            if !node
                .parent()
                .is_some_and(|parent| parent.kind() == "parameter_list")
                || !parameter_shape(node, source, cancellation)?
            {
                return Ok(None);
            }
            proof.name = match field(node, "declarator") {
                Some(declarator) => {
                    declarator_name(declarator, source, cancellation)?.map(|name| name.byte_range())
                }
                None => None,
            };
            proof.syntax = "hlsl.parameter";
            return Ok(Some(proof));
        }
        "compound_statement" | "field_declaration_list" => {
            if !boundary(node, source, b"{", b"}", cancellation)? {
                return Ok(None);
            }
            return Ok(Some(proof));
        }
        _ => return Ok(None),
    }
    Ok(proof.name.is_some().then_some(proof))
}

fn child_use_context(context: UseContext, parent: Node<'_>, child: Node<'_>) -> UseContext {
    if context == UseContext::Excluded {
        return context;
    }
    if matches!(
        parent.kind(),
        "semantics"
            | "bitfield_clause"
            | "resource_binding"
            | "hlsl_attribute"
            | "qualifiers"
            | "type_qualifier"
            | "storage_class_specifier"
            | "parameter_declaration"
            | "optional_parameter_declaration"
            | "parameter_list"
            | "function_declarator"
            | "array_declarator"
            | "preproc_def"
            | "preproc_function_def"
            | "preproc_call"
            | "preproc_include"
            | "string_literal"
            | "raw_string_literal"
            | "char_literal"
    ) || unsupported_cpp(parent)
        || field(parent, "type") == Some(child)
        || (conditional(parent)
            && ["condition", "name"]
                .into_iter()
                .any(|name| field(parent, name) == Some(child)))
        || (matches!(parent.kind(), "function_definition" | "cbuffer_specifier")
            && field(parent, "body") != Some(child))
    {
        return UseContext::Excluded;
    }
    if parent.kind() == "init_declarator" {
        return if field(parent, "value") == Some(child) {
            UseContext::Expression
        } else {
            UseContext::Excluded
        };
    }
    if declarator_parent(child) == Some(parent) {
        return if child.kind() == "init_declarator" {
            UseContext::Declarator
        } else {
            UseContext::Excluded
        };
    }
    context
}

/// Admits complete identities under proven owners and propagates proof bounds.
///
/// Exhaustion becomes the caller's existing capture-limit evidence. Buffer-body
/// containment is raw physical context, not a scope or a buffer-label identity.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "translation_unit" {
        return Ok(true);
    }
    if role == StructuralRole::Expression {
        return Ok(text(node, source).is_some() && !node.byte_range().is_empty());
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) {
        return Ok(clean(node, source) && text(node, source).is_some());
    }
    if !cache.chain(node, source, cancellation)? {
        return Ok(false);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        return Ok(cache
            .owner(node, source, cancellation)?
            .is_some_and(|proof| {
                proof.name.is_some()
                    && (role != StructuralRole::Signature || proof.declarators <= 1)
            }));
    }
    if role == StructuralRole::Reference {
        return Ok(written_name(node, source)
            && cache.uses(node, cancellation)? == UseContext::Expression
            && node.parent().is_none_or(|parent| {
                parent.kind() != "field_expression"
                    || field(parent, "operator")
                        .is_some_and(|operator| text(operator, source) == Some("."))
            }));
    }
    Ok(role == StructuralRole::Scope
        && matches!(node.kind(), "compound_statement" | "field_declaration_list")
        && !(node.kind() == "field_declaration_list"
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "cbuffer_specifier"))
        && cache.owner(node, source, cancellation)?.is_some())
}

fn expression_syntax(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<&'static str, AdapterError> {
    if node.kind().starts_with("preproc_") {
        return Ok("hlsl.preprocessor_context");
    }
    if matches!(
        node.kind(),
        "semantics"
            | "bitfield_clause"
            | "resource_binding"
            | "hlsl_attribute"
            | "qualifiers"
            | "type_qualifier"
            | "storage_class_specifier"
    ) {
        return Ok("hlsl.annotation_context");
    }
    if unsupported_cpp(node) || node.kind() == "ERROR" {
        return Ok("hlsl.unsupported_owner");
    }
    if owner_kind(node) {
        let Some(proof) = cache.owner(node, source, cancellation)? else {
            return Ok(
                if matches!(
                    node.kind(),
                    "parameter_declaration" | "optional_parameter_declaration"
                ) {
                    "hlsl.parameter_context"
                } else {
                    "hlsl.unsupported_owner"
                },
            );
        };
        if proof.syntax == "hlsl.buffer_context" {
            return Ok("hlsl.buffer_context");
        }
        if proof.declarators > 1 {
            return Ok("hlsl.shared_header");
        }
        if node.kind() == "parameter_declaration" && proof.name.is_none() {
            return Ok("hlsl.annotation_context");
        }
    }
    Ok(if cache.chain(node, source, cancellation)? {
        "hlsl.body_semantics"
    } else {
        "hlsl.unsupported_owner"
    })
}

/// Preserves exact names, physical fragment extents and proven raw headers.
///
/// Reference occurrences promise no resolved call, constructor or member target.
/// Constant-buffer context spans include the raw header, label and binding bytes.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let mut depth_node = node;
    let syntax = if node.kind() == "translation_unit" {
        range = 0..source.len();
        "hlsl.file"
    } else if role == StructuralRole::Expression {
        let syntax = expression_syntax(node, source, cancellation, cache)?;
        if syntax == "hlsl.shared_header"
            && let Some(proof) = cache.owner(node, source, cancellation)?
        {
            range = proof.header;
        }
        syntax
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        let proof = cache
            .owner(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-hlsl-owner"))?;
        range = match role {
            StructuralRole::Definition => {
                proof.name.ok_or_else(|| query_failure("query-hlsl-name"))?
            }
            StructuralRole::Signature => proof.header,
            _ => proof.extent,
        };
        if declarator_parent(node).is_some() && proof.declarators == 1 {
            depth_node = node
                .parent()
                .ok_or_else(|| query_failure("query-hlsl-depth-owner"))?;
        }
        match role {
            StructuralRole::Definition => "hlsl.name",
            StructuralRole::Signature => "hlsl.header",
            _ => proof.syntax,
        }
    } else if role == StructuralRole::Reference {
        match node.parent() {
            Some(parent)
                if parent.kind() == "call_expression"
                    && field(parent, "function") == Some(node) =>
            {
                "hlsl.unproven_call"
            }
            Some(parent)
                if parent.kind() == "field_expression" && field(parent, "field") == Some(node) =>
            {
                "hlsl.member"
            }
            _ => "hlsl.identifier",
        }
    } else {
        match role {
            StructuralRole::Scope => "hlsl.body",
            StructuralRole::Comment => "hlsl.comment",
            StructuralRole::StringLiteral => "hlsl.string",
            _ => return Err(query_failure("query-hlsl-unproven-role")),
        }
    };
    let mut depth = native_depth(depth_node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "translation_unit" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-hlsl-depth"))?;
    }
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    })
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use tree_sitter::{Parser, Tree};

    use super::*;

    fn tree(source: &[u8]) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Hlsl))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn extract(source: &str, maximum_facts: usize) -> (Tree, super::super::QueryExtraction) {
        let tree = tree(source.as_bytes());
        let family = crate::GrammarFamily::Hlsl;
        let pack = super::super::QueryPack::compile(family, include_str!("../../queries/hlsl.scm"))
            .unwrap();
        let extraction = pack
            .extract(
                family,
                &tree,
                source.as_bytes(),
                10_000,
                maximum_facts,
                &Cancellation::new(),
            )
            .unwrap();
        (tree, extraction)
    }

    fn names<'source>(
        source: &'source str,
        captures: &[QueryCandidate],
        role: StructuralRole,
    ) -> Vec<&'source str> {
        let mut captures = captures
            .iter()
            .filter(|capture| capture.role == role)
            .collect::<Vec<_>>();
        captures.sort_by_key(|capture| (capture.start, capture.end));
        captures
            .into_iter()
            .map(|capture| &source[capture.start..capture.end])
            .collect()
    }

    fn has(captures: &[QueryCandidate], syntax: &str, role: StructuralRole) -> bool {
        captures
            .iter()
            .any(|capture| capture.syntax == syntax && capture.role == role)
    }

    #[test]
    fn functions_prototypes_and_parameters_keep_complete_semantic_headers() {
        let source = "float4 Shade(in float2 UV : TEXCOORD0, out float Amount) : SV_Target0 { Amount = UV.x; return float4(UV, 0.0, 1.0); }\nfloat Sample(float2 Coord : TEXCOORD1);\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert!(extraction.limit.is_none());
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["Shade", "UV", "Amount", "Sample", "Coord"]
        );
        let headers = names(source, captures, StructuralRole::Signature);
        assert!(
            headers
                .contains(&"float4 Shade(in float2 UV : TEXCOORD0, out float Amount) : SV_Target0")
        );
        assert!(headers.contains(&"float Sample(float2 Coord : TEXCOORD1)"));
        assert!(headers.contains(&"in float2 UV : TEXCOORD0"));
        assert!(has(captures, "hlsl.function", StructuralRole::Declaration));
        assert!(has(captures, "hlsl.prototype", StructuralRole::Declaration));
        assert!(has(captures, "hlsl.parameter", StructuralRole::Declaration));
        for capture in captures {
            assert!(source.get(capture.start..capture.end).is_some());
            if capture.role == StructuralRole::Scope {
                // Runtime promotes scopes containing declarations after deduplication.
                assert!(!capture.required);
            } else if capture.role.belongs_to_identity_closure() {
                assert!(capture.required);
            }
        }
        assert_eq!(
            names(source, captures, StructuralRole::Reference),
            ["Amount", "UV", "x", "float4", "UV"]
        );
    }

    #[test]
    fn resource_annotation_aliases_do_not_become_names_or_reads() {
        let source = "Texture2D<float4> ColorTex : register(t0);\nSamplerState ColorSampler : register(s0);\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["ColorTex", "ColorSampler"]
        );
        assert_eq!(
            names(source, captures, StructuralRole::Signature),
            [
                "Texture2D<float4> ColorTex : register(t0)",
                "SamplerState ColorSampler : register(s0)"
            ]
        );
        assert_eq!(
            captures
                .iter()
                .filter(|capture| {
                    capture.syntax == "hlsl.variable" && capture.role == StructuralRole::Declaration
                })
                .count(),
            2
        );
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
        assert!(has(
            captures,
            "hlsl.annotation_context",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn structs_keep_fields_and_raw_written_semantic_suffixes() {
        let source = "struct VertexInput { float3 Position : POSITION; float2 UV : TEXCOORD0; };\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["VertexInput", "Position", "UV"]
        );
        assert_eq!(
            names(source, captures, StructuralRole::Signature),
            [
                "struct VertexInput",
                "float3 Position : POSITION",
                "float2 UV : TEXCOORD0"
            ]
        );
        assert!(has(captures, "hlsl.struct", StructuralRole::Declaration));
        assert!(has(captures, "hlsl.field", StructuralRole::Declaration));
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
        assert!(has(
            captures,
            "hlsl.annotation_context",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn buffer_labels_are_only_distinct_physical_contexts() {
        let source = "cbuffer Constants : register(b0) { float FirstValue; };\ncbuffer Constants : register(B2, space3) { float SecondValue; };\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["FirstValue", "SecondValue"]
        );
        assert!(has(
            captures,
            "hlsl.buffer_member",
            StructuralRole::Declaration
        ));
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
        let contexts = captures
            .iter()
            .filter(|capture| {
                capture.syntax == "hlsl.buffer_context"
                    && capture.role == StructuralRole::Expression
            })
            .collect::<Vec<_>>();
        assert_eq!(contexts.len(), 2);
        assert_ne!(contexts[0].start, contexts[1].start);
        for context in contexts {
            let raw = &source[context.start..context.end];
            assert!(raw.starts_with("cbuffer Constants : register("));
            assert!(raw.ends_with('}'));
        }
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(64);
        let buffer = tree.root_node().named_child(0).unwrap();
        assert_eq!(buffer.kind(), "cbuffer_specifier");
        assert!(
            retain_capture(
                buffer,
                StructuralRole::Expression,
                source.as_bytes(),
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        let proof = cache
            .owner(buffer, source.as_bytes(), &cancellation)
            .unwrap()
            .unwrap();
        assert!(proof.name.is_none());
        assert_eq!(&source[proof.header], "cbuffer Constants : register(b0)");
        for role in [
            StructuralRole::Declaration,
            StructuralRole::Definition,
            StructuralRole::Signature,
            StructuralRole::Reference,
            StructuralRole::Scope,
        ] {
            assert!(
                !retain_capture(buffer, role, source.as_bytes(), &cancellation, &mut cache)
                    .unwrap()
            );
        }
        let body = field(buffer, "body").unwrap();
        assert!(
            !retain_capture(
                body,
                StructuralRole::Scope,
                source.as_bytes(),
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        assert!(
            !captures
                .iter()
                .any(|capture| capture.role == StructuralRole::Scope
                    && capture.start == body.start_byte())
        );
    }

    #[test]
    fn shared_variables_fields_and_buffer_members_keep_separate_fragments() {
        let source = "float First, Second[2];\nstruct Pair { float Left, Right; };\ncbuffer Constants { float Near, Far[2]; };\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["First", "Second", "Pair", "Left", "Right", "Near", "Far"]
        );
        for syntax in [
            "hlsl.variable_fragment",
            "hlsl.field_fragment",
            "hlsl.buffer_member_fragment",
        ] {
            assert_eq!(
                captures
                    .iter()
                    .filter(|capture| capture.syntax == syntax
                        && capture.role == StructuralRole::Declaration)
                    .count(),
                2,
                "{syntax}"
            );
        }
        assert_eq!(
            names(source, captures, StructuralRole::Signature),
            ["struct Pair"]
        );
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.syntax == "hlsl.shared_header"
                    && capture.role == StructuralRole::Expression)
                .count(),
            3
        );
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
    }

    #[test]
    fn initializer_reads_survive_without_annotation_type_or_formal_reads() {
        let source = "[numthreads(8, 8, 1)]\nvoid Dispatch(uint3 ID : SV_DispatchThreadID) { uint Index = ID.x; float4 Color = float4(Input, 0.0, 1.0); Result = Color.y; discard; }\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["Dispatch", "ID", "Index", "Color"]
        );
        assert_eq!(
            names(source, captures, StructuralRole::Reference),
            ["ID", "x", "float4", "Input", "Result", "Color", "y"]
        );
        assert!(has(
            captures,
            "hlsl.unproven_call",
            StructuralRole::Reference
        ));
        assert!(has(captures, "hlsl.member", StructuralRole::Reference));
        assert!(has(
            captures,
            "hlsl.annotation_context",
            StructuralRole::Expression
        ));
        assert!(has(
            captures,
            "hlsl.body_semantics",
            StructuralRole::Expression
        ));
        assert!(!captures.iter().any(|capture| matches!(
            capture.role,
            StructuralRole::Call
                | StructuralRole::CallName
                | StructuralRole::ScopedCall
                | StructuralRole::Import
        )));
    }

    #[test]
    fn preprocessor_branches_preserve_written_not_selected_declarations() {
        let source = "#define WEIGHT 2\n#if USE_COLOR\nfloat4 FirstColor;\n#else\nfloat4 SecondColor;\n#endif\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["FirstColor", "SecondColor"]
        );
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
        assert!(has(
            captures,
            "hlsl.preprocessor_context",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn unsupported_parameter_defaults_keep_a_scoped_gap_not_partial_identities() {
        let source = "float Deferred(float Value = DefaultValue) { return Value; }\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert!(names(source, captures, StructuralRole::Definition).is_empty());
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
        assert!(has(
            captures,
            "hlsl.parameter_context",
            StructuralRole::Expression
        ));
        assert!(has(
            captures,
            "hlsl.unsupported_owner",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn comments_recovery_and_inherited_owners_cannot_launder_names() {
        let source = "/* float Phantom() { return Ghost; } λ */\n@;\nfloat Visible(float Value) { return Value; }\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(tree.root_node().has_error());
        assert_eq!(
            names(source, &extraction.candidates, StructuralRole::Definition),
            ["Visible", "Value"]
        );
        assert!(has(
            &extraction.candidates,
            "hlsl.comment",
            StructuralRole::Comment
        ));
        assert!(has(
            &extraction.candidates,
            "hlsl.unsupported_owner",
            StructuralRole::Expression
        ));
        for source in [
            "float Broken(float Value) { return Value;",
            "float Broken( { return Value; }",
            "Broken(float Value) { return Value; }",
            "float *Pointer;\nclass Foreign { float Member; };\nnamespace Foreign { float Value; }\n",
            "float First(), Second();",
        ] {
            let (_, extraction) = extract(source, 1024);
            assert!(
                names(source, &extraction.candidates, StructuralRole::Definition).is_empty(),
                "{source}"
            );
            assert!(
                has(
                    &extraction.candidates,
                    "hlsl.unsupported_owner",
                    StructuralRole::Expression
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn cache_limits_memoized_context_utf8_cancellation_and_identity_budget_are_explicit() {
        let source = b"float Value;";
        let parsed = tree(source);
        let owner = parsed.root_node().named_child(0).unwrap();
        let name = field(owner, "declarator").unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1);
        assert!(
            !retain_capture(
                name,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        assert!(cache.exhausted());
        assert_eq!(cache.entries.len(), 1);
        let mut cache = CaptureCache::new(16);
        assert!(
            retain_capture(
                name,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        let entries = cache.entries.len();
        assert!(
            retain_capture(
                name,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        assert_eq!(cache.entries.len(), entries);
        let mut invalid_source = *b"float Value;";
        invalid_source[name.start_byte()] = 0xff;
        assert!(!written_name(name, &invalid_source));
        let expression_source = b"float Value = Input;";
        let expression_tree = tree(expression_source);
        let declaration = expression_tree.root_node().named_child(0).unwrap();
        let initializer = field(declaration, "declarator").unwrap();
        let input = field(initializer, "value").unwrap();
        let mut small = CaptureCache::new(1);
        assert!(small.uses(input, &cancellation).unwrap() == UseContext::Excluded);
        assert!(small.exhausted());
        assert_eq!(small.entries.len(), 1);
        let mut context = CaptureCache::new(16);
        assert!(context.uses(input, &cancellation).unwrap() == UseContext::Expression);
        let entries = context.entries.len();
        assert!(context.uses(input, &cancellation).unwrap() == UseContext::Expression);
        assert_eq!(context.entries.len(), entries);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            retain_capture(
                name,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache
            )
            .is_err()
        );
        assert!(
            retain_capture(
                owner,
                StructuralRole::Expression,
                source,
                &cancellation,
                &mut cache
            )
            .is_err()
        );
        assert!(context.uses(input, &cancellation).is_err());
        let source = "float Kept(float Input) { return Input; }";
        let (_, extraction) = extract(source, 0);
        assert_eq!(extraction.limit, Some(super::super::QueryLimit::Fact));
        assert_eq!(
            names(source, &extraction.candidates, StructuralRole::Definition),
            ["Kept", "Input"]
        );
        assert!(
            !extraction
                .candidates
                .iter()
                .any(|capture| capture.role == StructuralRole::Reference)
        );
    }
}
