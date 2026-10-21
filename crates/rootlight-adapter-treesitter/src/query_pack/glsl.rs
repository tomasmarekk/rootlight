//! GLSL captures retain complete physical owners and unresolved source uses.
//! The inherited C grammar is not a dialect validator: unsupported owners and
//! shader/preprocessor binding remain scoped evidence, never inferred targets.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

mod bindings;

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    name: Option<Range<usize>>,
    instance: Option<Range<usize>>,
    syntax: &'static str,
    declarators: usize,
    declarator_extent: Option<Range<usize>>,
}

/// Immutable native owner proofs share the existing capture/body scan ceiling.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    maximum_entries: usize,
    exhausted: bool,
    bindings: bindings::CaptureCache,
}

impl CaptureCache {
    /// Creates a bounded cache without reserving source-sized storage.
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            maximum_entries,
            exhausted: false,
            bindings: bindings::CaptureCache::new(maximum_entries, false),
        }
    }

    pub(super) fn new_with_source_frame(maximum_entries: usize, complete: bool) -> Self {
        let mut cache = Self::new(maximum_entries);
        cache.bindings = bindings::CaptureCache::new(maximum_entries, complete);
        cache
    }

    /// Reports a bound that the caller must propagate as a capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted || self.bindings.exhausted()
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self.owners.get(&node.id()) {
            return Ok(proof.clone());
        }
        if self.owners.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(None);
        }
        let proof = owner_proof(node, source, cancellation, self)?;
        // A declarator proof can first insert its physical parent proof.
        if self.owners.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(None);
        }
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), proof.clone());
        Ok(proof)
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
    if !matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier"
    ) || !clean(node, source)
    {
        return false;
    }
    let Some(raw) = text(node, source) else {
        return false;
    };
    let mut bytes = raw.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && raw != "discard"
        && structural_captured_name_for_language("glsl", raw, raw.len()).as_deref() == Some(raw)
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
            && source.get(child.byte_range()) == Some(b";")
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

fn unsupported_c(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "union_specifier"
            | "enum_specifier"
            | "type_definition"
            | "pointer_declarator"
            | "abstract_pointer_declarator"
            | "attributed_declarator"
            | "variadic_parameter"
            | "attribute_specifier"
            | "attribute_declaration"
            | "ms_declspec_modifier"
            | "ms_call_modifier"
            | "gnu_asm_expression"
            | "bitfield_clause"
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
        if unsupported_c(child) || child.kind() == "storage_class_specifier" {
            return Ok(false);
        }
        if child.kind() == "type_qualifier"
            && !matches!(text(child, source), Some("const" | "restrict"))
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
            "init_declarator" | "array_declarator" => {
                if node.kind() == "array_declarator" && !array_shape(node, source, cancellation)? {
                    return Ok(None);
                }
                let Some(child) = field(node, "declarator") else {
                    return Ok(None);
                };
                node = child;
            }
            "function_declarator" if function => {
                let Some(parameters) = field(node, "parameters") else {
                    return Ok(None);
                };
                let Some(name) = field(node, "declarator") else {
                    return Ok(None);
                };
                if name.kind() != "identifier"
                    || !written_name(name, source)
                    || name.start_byte() != node.start_byte()
                    || parameters.end_byte() != node.end_byte()
                    || !boundary(parameters, source, b"(", b")", cancellation)?
                {
                    return Ok(None);
                }
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    cancellation.check()?;
                    if !child.is_extra() && child != name && child != parameters {
                        return Ok(None);
                    }
                }
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    cancellation.check()?;
                    if parameter.is_extra() {
                        continue;
                    }
                    if parameter.kind() != "parameter_declaration"
                        || !parameter_shape(parameter, source, cancellation)?
                    {
                        return Ok(None);
                    }
                }
                return Ok(Some(name));
            }
            _ => return Ok(None),
        }
    }
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
    // C bracket qualifiers and its anonymous `*` size are not GLSL arrays.
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

fn parameter_shape(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !clean(node, source) || !prefix_valid(node, source, cancellation)? {
        return Ok(false);
    }
    let Some(ty) = field(node, "type") else {
        return Ok(false);
    };
    if !shader_type(ty, source, cancellation)? {
        return Ok(false);
    }
    match field(node, "declarator") {
        Some(declarator) if matches!(declarator.kind(), "identifier" | "array_declarator") => {
            Ok(declarator_name(declarator, source, cancellation)?.is_some())
        }
        Some(_) => Ok(false),
        None => Ok(true),
    }
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
            | "struct_specifier"
            | "compound_statement"
            | "field_declaration_list"
    )
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
        instance: None,
        syntax: "glsl.body",
        declarators: 0,
        declarator_extent: None,
    };
    if let Some(parent) = declarator_parent(node) {
        let Some(parent_proof) = cache.owner(parent, source, cancellation)? else {
            return Ok(None);
        };
        if parent_proof.instance.as_ref() == Some(&node.byte_range()) {
            proof.syntax = "glsl.interface_instance";
        } else {
            // The parent already proved every direct declarator. Its physical
            // envelope excludes the type/prefix without rescanning siblings.
            if !parent_proof
                .declarator_extent
                .as_ref()
                .is_some_and(|extent| {
                    extent.start <= node.start_byte() && node.end_byte() <= extent.end
                })
                || !matches!(
                    node.kind(),
                    "identifier"
                        | "field_identifier"
                        | "init_declarator"
                        | "array_declarator"
                        | "function_declarator"
                )
            {
                return Ok(None);
            }
            proof.syntax = match (parent.kind(), node.kind(), parent_proof.declarators) {
                ("declaration", "function_declarator", 1) => "glsl.prototype",
                ("declaration", _, 1) => "glsl.variable",
                ("declaration", _, _) => "glsl.variable_fragment",
                ("field_declaration", _, 1) => "glsl.field",
                ("field_declaration", _, _) => "glsl.field_fragment",
                _ => return Ok(None),
            };
            if parent_proof.declarators == 1 {
                proof.extent = parent_proof.extent;
                proof.header = parent_proof.header;
            }
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
                || !shader_type(ty, source, cancellation)?
                || !boundary(body, source, b"{", b"}", cancellation)?
            {
                return Ok(None);
            }
            proof.name =
                declarator_name(declarator, source, cancellation)?.map(|name| name.byte_range());
            proof.header = node.start_byte()..declarator.end_byte();
            proof.syntax = "glsl.function";
        }
        "declaration" | "field_declaration" => {
            if !physical_position(node, cancellation)? || !prefix_valid(node, source, cancellation)?
            {
                return Ok(None);
            }
            let Some(terminal) = semicolon(node, source, cancellation)? else {
                return Ok(None);
            };
            let Some(header) = header_before(node, terminal, cancellation)? else {
                return Ok(None);
            };
            proof.header = header;
            if let Some(ty) = field(node, "type") {
                if !shader_type(ty, source, cancellation)? {
                    return Ok(None);
                }
                if field(node, "precision").is_some() {
                    proof.syntax = "glsl.precision_context";
                    return Ok(Some(proof));
                }
                let mut function = false;
                let mut cursor = node.walk();
                for declarator in node.children_by_field_name("declarator", &mut cursor) {
                    cancellation.check()?;
                    if declarator_name(declarator, source, cancellation)?.is_none()
                        || (node.kind() == "field_declaration"
                            && declarator.kind() == "function_declarator")
                    {
                        return Ok(None);
                    }
                    function |= declarator.kind() == "function_declarator";
                    proof.declarators = proof
                        .declarators
                        .checked_add(1)
                        .ok_or_else(|| query_failure("query-glsl-declarator-count"))?;
                    match &mut proof.declarator_extent {
                        Some(extent) => extent.end = declarator.end_byte(),
                        None => proof.declarator_extent = Some(declarator.byte_range()),
                    }
                }
                if function && proof.declarators != 1 {
                    return Ok(None);
                }
                if proof.declarators == 0 && ty.kind() != "struct_specifier" {
                    return Ok(None);
                }
                proof.syntax = "glsl.declaration_context";
                return Ok(Some(proof));
            }
            // Interface blocks have unfielded names, unlike C declarations.
            let mut body = None;
            let mut name = None;
            let mut instance = None;
            let mut storage = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                cancellation.check()?;
                if child.is_extra() {
                    continue;
                }
                if !child.is_named() {
                    storage |= matches!(
                        text(child, source),
                        Some("in" | "out" | "uniform" | "buffer")
                    );
                    continue;
                }
                match (child.kind(), body.is_some()) {
                    ("layout_specification" | "extension_storage_class", false) => {}
                    ("identifier", false) if name.is_none() && written_name(child, source) => {
                        name = Some(child)
                    }
                    ("field_declaration_list", false) if name.is_some() => body = Some(child),
                    ("identifier" | "array_declarator", true) if instance.is_none() => {
                        if declarator_name(child, source, cancellation)?.is_none() {
                            return Ok(None);
                        }
                        instance = Some(child);
                    }
                    _ => return Ok(None),
                }
            }
            if name.is_some()
                && body.is_none()
                && instance.is_none()
                && node.child(0).is_some_and(|first| {
                    matches!(text(first, source), Some("invariant" | "precise"))
                })
            {
                proof.syntax = "glsl.qualifier_context";
                return Ok(Some(proof));
            }
            let (Some(name), Some(body)) = (name, body) else {
                return Ok(None);
            };
            if !storage || !boundary(body, source, b"{", b"}", cancellation)? {
                return Ok(None);
            }
            let Some(header) = header_before(node, body, cancellation)? else {
                return Ok(None);
            };
            proof.header = header;
            proof.name = Some(name.byte_range());
            proof.instance = instance.map(|instance| instance.byte_range());
            proof.syntax = "glsl.interface_block";
        }
        "struct_specifier" => {
            let (Some(name), Some(body)) = (field(node, "name"), field(node, "body")) else {
                return Ok(None);
            };
            if !written_name(name, source)
                || !boundary(body, source, b"{", b"}", cancellation)?
                || !prefix_valid(node, source, cancellation)?
                || node
                    .child(0)
                    .is_none_or(|first| text(first, source) != Some("struct"))
            {
                return Ok(None);
            }
            let Some(header) = header_before(node, body, cancellation)? else {
                return Ok(None);
            };
            proof.header = header;
            proof.name = Some(name.byte_range());
            proof.syntax = "glsl.struct";
        }
        "parameter_declaration" => {
            if node
                .parent()
                .is_none_or(|parent| parent.kind() != "parameter_list")
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
            proof.syntax = "glsl.parameter";
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

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.is_missing() || owner.kind() == "ERROR" || unsupported_c(owner) {
            return Ok(false);
        }
        if owner_kind(owner) && cache.owner(owner, source, cancellation)?.is_none() {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

fn lexical_reference(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let target = node.byte_range();
    let mut child = node;
    let mut in_initializer = false;
    while let Some(parent) = child.parent() {
        cancellation.check()?;
        if parent.kind() == "function_definition"
            && field(parent, "body")
                .is_none_or(|body| target.start < body.start_byte() || body.end_byte() < target.end)
        {
            return Ok(false);
        }
        if matches!(
            parent.kind(),
            "layout_specification"
                | "layout_qualifiers"
                | "qualifier"
                | "parameter_declaration"
                | "type_qualifier"
                | "extension_storage_class"
                | "preproc_def"
                | "preproc_function_def"
                | "preproc_call"
                | "preproc_include"
                | "preproc_extension"
                | "string_literal"
        ) || unsupported_c(parent)
            || (conditional(parent)
                && ["condition", "name"].into_iter().any(|name| {
                    field(parent, name).is_some_and(|value| {
                        value.start_byte() <= target.start && target.end <= value.end_byte()
                    })
                }))
            || field(parent, "type")
                .is_some_and(|ty| ty.start_byte() <= target.start && target.end <= ty.end_byte())
        {
            return Ok(false);
        }
        if matches!(
            parent.kind(),
            "function_definition"
                | "declaration"
                | "field_declaration"
                | "struct_specifier"
                | "function_declarator"
                | "array_declarator"
        ) {
            let declarator_region = if matches!(parent.kind(), "declaration" | "field_declaration")
            {
                cache
                    .owner(parent, source, cancellation)?
                    .and_then(|proof| proof.declarator_extent)
            } else {
                field(parent, "declarator").map(|declarator| declarator.byte_range())
            };
            if declarator_region
                .is_some_and(|extent| extent.start <= target.start && target.end <= extent.end)
                && !in_initializer
            {
                return Ok(false);
            }
            if parent.kind() == "declaration" && field(parent, "type").is_none() {
                return Ok(false);
            }
        }
        if parent.kind() == "init_declarator" {
            if field(parent, "declarator") == Some(child) {
                return Ok(false);
            }
            in_initializer |= field(parent, "value").is_some_and(|value| {
                value.start_byte() <= target.start && target.end <= value.end_byte()
            });
        }
        child = parent;
    }
    Ok(true)
}

/// Admits only source-backed identities under proven physical owners.
/// Cancellation/allocation errors propagate and exhausted proof storage must
/// become the caller's existing capture-limit evidence, not a silent omission.
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
    if node.kind() == "identifier" && role == StructuralRole::Signature {
        return Ok(cache
            .bindings
            .candidate(node, role, source, cancellation)?
            .is_some());
    }
    if !valid_owner_chain(node, source, cancellation, cache)? {
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
            && lexical_reference(node, source, cancellation, cache)?
            && node.parent().is_none_or(|parent| {
                parent.kind() != "field_expression"
                    || field(parent, "operator")
                        .is_some_and(|operator| text(operator, source) == Some("."))
            }));
    }
    Ok(role == StructuralRole::Scope && cache.owner(node, source, cancellation)?.is_some())
}

fn expression_syntax(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<&'static str, AdapterError> {
    if node.kind().starts_with("preproc_") {
        return Ok("glsl.preprocessor_context");
    }
    if matches!(
        node.kind(),
        "layout_specification" | "qualifier" | "type_qualifier" | "extension_storage_class"
    ) {
        return Ok("glsl.qualifier_context");
    }
    if node.kind() == "expression_statement"
        && node.named_child_count() == 1
        && node.named_child(0).is_some_and(|child| {
            child.kind() == "identifier" && text(child, source) == Some("discard")
        })
    {
        return Ok("glsl.discard_context");
    }
    if unsupported_c(node) || node.kind() == "ERROR" {
        return Ok("glsl.unsupported_owner");
    }
    if node.kind() == "field_expression"
        && field(node, "operator").is_none_or(|operator| text(operator, source) != Some("."))
    {
        return Ok("glsl.unsupported_owner");
    }
    if owner_kind(node) {
        let Some(proof) = cache.owner(node, source, cancellation)? else {
            return Ok("glsl.unsupported_owner");
        };
        if proof.syntax == "glsl.precision_context" {
            return Ok("glsl.precision_context");
        }
        if proof.syntax == "glsl.qualifier_context" {
            return Ok("glsl.qualifier_context");
        }
        if proof.declarators > 1 {
            return Ok("glsl.shared_declaration_header");
        }
        if node.kind() == "parameter_declaration" && proof.name.is_none() {
            return Ok("glsl.parameter_context");
        }
        if matches!(
            proof.syntax,
            "glsl.function" | "glsl.struct" | "glsl.interface_block"
        ) {
            return Ok("glsl.shader_environment");
        }
    }
    Ok(if valid_owner_chain(node, source, cancellation, cache)? {
        "glsl.body_semantics"
    } else {
        "glsl.unsupported_owner"
    })
}

/// Preserves raw name bytes, single-owner headers and fragment identity.
/// Reference occurrences have no target binding or call-relationship promise.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    if node.kind() == "identifier"
        && matches!(role, StructuralRole::Reference | StructuralRole::Signature)
        && let Some(candidate) = cache.bindings.candidate(node, role, source, cancellation)?
    {
        return Ok(candidate);
    }
    let mut range = node.byte_range();
    let mut depth_node = node;
    let syntax = if node.kind() == "translation_unit" {
        range = 0..source.len();
        "glsl.file"
    } else if role == StructuralRole::Expression {
        expression_syntax(node, source, cancellation, cache)?
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        let proof = cache
            .owner(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-glsl-owner"))?;
        range = match role {
            StructuralRole::Definition => {
                proof.name.ok_or_else(|| query_failure("query-glsl-name"))?
            }
            StructuralRole::Signature => proof.header,
            _ => proof.extent,
        };
        if declarator_parent(node).is_some()
            && proof.declarators == 1
            && proof.syntax != "glsl.interface_instance"
        {
            depth_node = node
                .parent()
                .ok_or_else(|| query_failure("query-glsl-depth-owner"))?;
        }
        match role {
            StructuralRole::Definition => "glsl.name",
            StructuralRole::Signature => "glsl.header",
            _ => proof.syntax,
        }
    } else if role == StructuralRole::Reference {
        match node.parent() {
            Some(parent)
                if parent.kind() == "call_expression"
                    && field(parent, "function") == Some(node) =>
            {
                "glsl.unproven_call"
            }
            Some(parent)
                if parent.kind() == "field_expression" && field(parent, "field") == Some(node) =>
            {
                "glsl.member"
            }
            _ => "glsl.identifier",
        }
    } else {
        match role {
            StructuralRole::Scope => "glsl.body",
            StructuralRole::Comment => "glsl.comment",
            StructuralRole::StringLiteral => "glsl.string",
            _ => return Err(query_failure("query-glsl-unproven-role")),
        }
    };
    let mut depth = native_depth(depth_node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "translation_unit" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-glsl-depth"))?;
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
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Glsl))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn extract(source: &str, maximum_facts: usize) -> (Tree, super::super::QueryExtraction) {
        let tree = tree(source.as_bytes());
        let family = crate::GrammarFamily::Glsl;
        let pack = super::super::QueryPack::compile(family, include_str!("../../queries/glsl.scm"))
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
    fn functions_prototypes_and_parameters_keep_written_headers() {
        let source = "float Scale(in vec3 Position, out float Magnitude) { Magnitude = Position.x; return Magnitude; }\nfloat Sample(in vec2 UV);\n";
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
            ["Scale", "Position", "Magnitude", "Sample", "UV"]
        );
        let headers = names(source, captures, StructuralRole::Signature);
        assert!(headers.contains(&"float Scale(in vec3 Position, out float Magnitude)"));
        assert!(headers.contains(&"float Sample(in vec2 UV)"));
        assert!(has(captures, "glsl.function", StructuralRole::Declaration));
        assert!(has(captures, "glsl.prototype", StructuralRole::Declaration));
        assert!(has(captures, "glsl.parameter", StructuralRole::Declaration));
        for capture in captures {
            assert!(source.get(capture.start..capture.end).is_some());
            if capture.role.belongs_to_identity_closure() && capture.role != StructuralRole::Scope {
                assert!(capture.required);
            }
        }
    }

    #[test]
    fn shared_declarations_keep_fragments_without_duplicate_headers() {
        let source = "float First, Second[2];\nstruct Pair { float Left, Right; };\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["First", "Second", "Pair", "Left", "Right"]
        );
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.syntax == "glsl.variable_fragment")
                .count(),
            2
        );
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.syntax == "glsl.field_fragment")
                .count(),
            2
        );
        assert!(has(
            captures,
            "glsl.shared_declaration_header",
            StructuralRole::Expression
        ));
        assert_eq!(
            names(source, captures, StructuralRole::Signature),
            ["struct Pair"]
        );
    }

    #[test]
    fn interface_blocks_distinguish_container_instance_and_fields() {
        let source = "layout(std140, binding = 0) uniform Camera { mat4 View; vec3 Eye; } camera;\nlayout(location = 1) in vec3 Position;\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["Camera", "View", "Eye", "camera", "Position"]
        );
        assert!(has(
            captures,
            "glsl.interface_block",
            StructuralRole::Declaration
        ));
        assert!(has(
            captures,
            "glsl.interface_instance",
            StructuralRole::Declaration
        ));
        assert!(has(captures, "glsl.field", StructuralRole::Declaration));
        assert!(
            names(source, captures, StructuralRole::Signature)
                .contains(&"layout(std140, binding = 0) uniform Camera")
        );
        assert!(names(source, captures, StructuralRole::Reference).is_empty());
        assert!(has(
            captures,
            "glsl.qualifier_context",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn precision_discard_and_calls_are_honest_source_contexts() {
        let source = "precision mediump float;\ninvariant Position;\nsubroutine(ShadeType) void Shade() { discard; vec4 Color = texture(Image, UV); Result = Color.x; }\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let captures = &extraction.candidates;
        assert_eq!(
            names(source, captures, StructuralRole::Definition),
            ["Shade", "Color"]
        );
        let reads = names(source, captures, StructuralRole::Reference);
        assert_eq!(reads, ["texture", "Image", "UV", "Result", "Color", "x"]);
        assert!(has(
            captures,
            "glsl.unproven_call",
            StructuralRole::Reference
        ));
        assert!(has(captures, "glsl.member", StructuralRole::Reference));
        assert!(has(
            captures,
            "glsl.precision_context",
            StructuralRole::Expression
        ));
        assert!(has(
            captures,
            "glsl.qualifier_context",
            StructuralRole::Expression
        ));
        assert!(has(
            captures,
            "glsl.discard_context",
            StructuralRole::Expression
        ));
        assert!(has(
            captures,
            "glsl.body_semantics",
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
    fn preprocessor_branches_are_written_not_selected_or_bound() {
        let source = "#version 450\n#define WEIGHT 2\n#if USE_COLOR\nvec4 FirstColor;\n#else\nvec4 SecondColor;\n#endif\n";
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
            "glsl.preprocessor_context",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn comments_and_recovery_cannot_supply_missing_owner_prefixes() {
        let source = "/* float Phantom() { return Ghost; } λ */\n@;\nfloat Visible(float Value) { return Value; }\n";
        let (tree, extraction) = extract(source, 1024);
        assert!(tree.root_node().has_error());
        assert_eq!(
            names(source, &extraction.candidates, StructuralRole::Definition),
            ["Visible", "Value"]
        );
        assert!(has(
            &extraction.candidates,
            "glsl.comment",
            StructuralRole::Comment
        ));
        assert!(has(
            &extraction.candidates,
            "glsl.unsupported_owner",
            StructuralRole::Expression
        ));
        for source in [
            "float Broken(float Value) { return Value;",
            "float Broken( { return Value; }",
            "Broken(float Value) { return Value; }",
        ] {
            let (_, extraction) = extract(source, 1024);
            assert!(
                names(source, &extraction.candidates, StructuralRole::Definition).is_empty(),
                "{source}"
            );
            assert!(has(
                &extraction.candidates,
                "glsl.unsupported_owner",
                StructuralRole::Expression
            ));
        }
    }

    #[test]
    fn inherited_c_owners_are_not_shader_declarations() {
        let source = "float *Pointer;\nunion Foreign { float Member; };\nenum Choice { Item };\ntypedef float Alias;\nstatic float Hidden;\n";
        let (_, extraction) = extract(source, 1024);
        assert!(names(source, &extraction.candidates, StructuralRole::Definition).is_empty());
        assert!(names(source, &extraction.candidates, StructuralRole::Reference).is_empty());
        assert!(has(
            &extraction.candidates,
            "glsl.unsupported_owner",
            StructuralRole::Expression
        ));
    }

    #[test]
    fn inherited_c_declarator_suffixes_do_not_validate_shader_owners() {
        for source in [
            "float Values[static 2];",
            "float Values[*];",
            "void Proto(float Value[const 2]);",
            "void Proto(float Value[static 2]);",
            "void Run() Suffix {}",
            "void Run() Suffix() {}",
            "float First(), Second();",
            "__stdcall float Run() { return 0.0; }",
        ] {
            let (tree, extraction) = extract(source, 1024);
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            assert!(
                names(source, &extraction.candidates, StructuralRole::Definition).is_empty(),
                "{source}"
            );
            assert!(
                has(
                    &extraction.candidates,
                    "glsl.unsupported_owner",
                    StructuralRole::Expression
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn cache_limits_cached_cancellation_utf8_and_body_budget_are_explicit() {
        let source = b"float Value;";
        let tree = tree(source);
        let owner = tree.root_node().named_child(0).unwrap();
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
        assert_eq!(cache.owners.len(), 1);
        let mut cache = CaptureCache::new(8);
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
        let entries = cache.owners.len();
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
        assert_eq!(cache.owners.len(), entries);
        let mut invalid_source = *b"float Value;";
        invalid_source[name.start_byte()] = 0xff;
        assert!(!written_name(name, &invalid_source));
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

#[cfg(test)]
mod original_written_binding_observation {
    use super::*;

    #[test]
    fn inspect_original_common_shader_cst_and_capture_frames() {
        let source = include_str!("../../tests/fixtures/glsl_written_binding/common.glsl");
        assert_eq!(source.len(), 5169);
        eprintln!(
            "GLSL_ORIGINAL_SOURCE content_hash={:?} bytes={}",
            rootlight_ids::content_hash(source.as_bytes()),
            source.len()
        );
        let anchors = [
            (2434, 2443, "luminance"),
            (2449, 2454, "color"),
            (2473, 2478, "color"),
            (2722, 2736, "contrast_ratio"),
            (2788, 2797, "luminance"),
        ];
        for (start, end, written) in anchors {
            assert_eq!(&source[start..end], written);
        }
        let family = crate::GrammarFamily::Glsl;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        eprintln!(
            "GLSL_ORIGINAL_ROOT kind={} range={:?} error={} missing={} has_error={}",
            root.kind(),
            root.byte_range(),
            root.is_error(),
            root.is_missing(),
            root.has_error()
        );
        eprintln!("GLSL_ORIGINAL_SEXPR {}", root.to_sexp());
        let mut cursor = root.walk();
        let mut count = 0_usize;
        loop {
            let node = cursor.node();
            count += 1;
            assert!(
                count <= 16_384,
                "observation prerequisite exceeds existing small syntax budget"
            );
            if anchors
                .iter()
                .any(|(start, end, _)| node.start_byte() <= *start && *end <= node.end_byte())
            {
                let field = node.parent().and_then(|parent| {
                    (0..parent.child_count()).find_map(|index| {
                        let index = u32::try_from(index).unwrap();
                        (parent.child(index) == Some(node))
                            .then(|| parent.field_name_for_child(index))
                            .flatten()
                    })
                });
                eprintln!(
                    "GLSL_ORIGINAL_NODE kind={} range={:?} named={} field={field:?} parent={:?} error={} missing={} has_error={}",
                    node.kind(),
                    node.byte_range(),
                    node.is_named(),
                    node.parent()
                        .map(|parent| (parent.kind(), parent.byte_range())),
                    node.is_error(),
                    node.is_missing(),
                    node.has_error()
                );
            }
            if node.kind() == "function_definition" {
                let mut fields = node.walk();
                for (index, child) in node.children(&mut fields).enumerate() {
                    eprintln!(
                        "GLSL_ORIGINAL_FUNCTION_CHILD owner={:?} field={:?} kind={} range={:?} error={} missing={}",
                        node.byte_range(),
                        node.field_name_for_child(u32::try_from(index).unwrap()),
                        child.kind(),
                        child.byte_range(),
                        child.has_error(),
                        child.is_missing()
                    );
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node() == root {
                break;
            }
        }
        let pack = super::super::QueryPack::compile(family, include_str!("../../queries/glsl.scm"))
            .unwrap();
        let extracted = pack
            .extract(
                family,
                &tree,
                source.as_bytes(),
                16_384,
                16_384,
                &Cancellation::new(),
            )
            .unwrap();
        eprintln!(
            "GLSL_ORIGINAL_QUERY candidates={} limit={:?} nodes={count}",
            extracted.candidates.len(),
            extracted.limit
        );
        for capture in &extracted.candidates {
            if anchors
                .iter()
                .any(|(start, end, _)| capture.start <= *start && *end <= capture.end)
            {
                eprintln!(
                    "GLSL_ORIGINAL_CAPTURE role={:?} syntax={} range={:?} required={} native_depth={}",
                    capture.role,
                    capture.syntax,
                    capture.start..capture.end,
                    capture.required,
                    capture.native_depth
                );
            }
        }
    }
}
