//! Apex captures retain direct written declarations and conservative source uses.
//! Shared declaration types remain separate source evidence; owner proofs are bounded
//! to one immutable scan and never turn parser recovery into class membership.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[path = "apex_calls.rs"]
mod calls;

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    declarators: usize,
}

struct FormalProof {
    body: Range<usize>,
    bindings: Vec<BindingProof>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingKind {
    Formal,
    Local,
    Barrier,
}

struct BindingProof {
    name: Range<usize>,
    scope: Range<usize>,
    available: usize,
    kind: BindingKind,
}

/// Per-scan native owner evidence shares the existing query capture ceiling.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    formals: HashMap<usize, Option<FormalProof>>,
    calls: HashMap<usize, Option<calls::CallKind>>,
    maximum_entries: usize,
    remaining_body_nodes: usize,
    exhausted: bool,
}

impl CaptureCache {
    /// Creates an empty bounded cache without reserving input-sized storage.
    pub(super) fn new(maximum_entries: usize, maximum_body_nodes: usize) -> Self {
        Self {
            owners: HashMap::new(),
            formals: HashMap::new(),
            calls: HashMap::new(),
            maximum_entries,
            remaining_body_nodes: maximum_body_nodes,
            exhausted: false,
        }
    }

    /// Reports a bound that the caller must propagate as a capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn entries(&self) -> usize {
        self.owners
            .len()
            .saturating_add(self.formals.len())
            .saturating_add(self.calls.len())
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
        if self.entries() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(None);
        }
        let proof = if node.kind() == "local_variable_declaration"
            && !self.local_position(node, cancellation)?
        {
            None
        } else {
            owner_proof(node, source, cancellation)?
        };
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), proof.clone());
        Ok(proof)
    }

    fn visit_body_node(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining_body_nodes.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining_body_nodes = remaining;
        Ok(true)
    }

    fn local_position(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(scope) = node
            .parent()
            .filter(|parent| matches!(parent.kind(), "block" | "constructor_body"))
        else {
            return Ok(false);
        };
        let mut ancestor = Some(scope);
        while let Some(parent) = ancestor {
            if !self.visit_body_node(cancellation)? {
                return Ok(false);
            }
            if matches!(
                parent.kind(),
                "method_declaration" | "constructor_declaration"
            ) {
                return Ok(field(parent, "body").is_some_and(|body| {
                    body.start_byte() <= scope.start_byte() && scope.end_byte() <= body.end_byte()
                }));
            }
            if matches!(
                parent.kind(),
                "catch_clause"
                    | "for_statement"
                    | "enhanced_for_statement"
                    | "class_body"
                    | "interface_body"
                    | "trigger_declaration"
                    | "trigger_body"
            ) || owner_kind(parent)
            {
                return Ok(false);
            }
            ancestor = parent.parent();
        }
        Ok(false)
    }

    #[cfg(test)]
    fn formal_use(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        Ok(self.lexical_use(node, source, cancellation)? == Some(BindingKind::Formal))
    }

    fn lexical_use(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<BindingKind>, AdapterError> {
        cancellation.check()?;
        if !formal_value_position(node) {
            return Ok(None);
        }
        let mut ancestor = node.parent();
        let owner = loop {
            let Some(parent) = ancestor else {
                return Ok(None);
            };
            if !self.visit_body_node(cancellation)? {
                return Ok(None);
            }
            if matches!(
                parent.kind(),
                "method_declaration" | "constructor_declaration"
            ) {
                break parent;
            }
            if (owner_kind(parent) && parent.kind() != "local_variable_declaration")
                || matches!(parent.kind(), "class_body" | "interface_body")
            {
                return Ok(None);
            }
            ancestor = parent.parent();
        };
        if !self.formals.contains_key(&owner.id()) {
            if self.entries() >= self.maximum_entries {
                self.exhausted = true;
                return Ok(None);
            }
            let proof = self.formal_proof(owner, source, cancellation)?;
            if self.exhausted || self.entries() >= self.maximum_entries {
                self.exhausted = true;
                return Ok(None);
            }
            self.formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.formals.insert(owner.id(), proof);
        }
        let Some(Some(proof)) = self.formals.get(&owner.id()) else {
            return Ok(None);
        };
        let range = node.byte_range();
        if range.start < proof.body.start || range.end > proof.body.end {
            return Ok(None);
        }
        let Some(name) = source.get(range) else {
            return Ok(None);
        };
        let start = proof
            .bindings
            .partition_point(|binding| folded_cmp(source, &binding.name, name).is_lt());
        let mut selected = None;
        for binding in proof.bindings.get(start..).unwrap_or_default() {
            cancellation.check()?;
            let Some(remaining) = self.remaining_body_nodes.checked_sub(1) else {
                self.exhausted = true;
                return Ok(None);
            };
            self.remaining_body_nodes = remaining;
            if !folded_cmp(source, &binding.name, name).is_eq() {
                break;
            }
            if binding.scope.start <= node.start_byte() && node.end_byte() <= binding.scope.end {
                if selected.is_some()
                    || binding.kind == BindingKind::Barrier
                    || node.start_byte() < binding.available
                    || source.get(binding.name.clone()) != Some(name)
                {
                    return Ok(None);
                }
                selected = Some(binding.kind);
            }
        }
        Ok(selected)
    }

    fn formal_proof(
        &mut self,
        owner: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<FormalProof>, AdapterError> {
        if self.owner(owner, source, cancellation)?.is_none() {
            return Ok(None);
        }
        let (Some(body), Some(parameters)) = (field(owner, "body"), field(owner, "parameters"))
        else {
            return Ok(None);
        };
        if !matches!(body.kind(), "block" | "constructor_body") {
            return Ok(None);
        }
        let mut bindings = Vec::new();
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !self.visit_body_node(cancellation)? {
                return Ok(None);
            }
            if parameter.is_extra() {
                continue;
            }
            if parameter.kind() != "formal_parameter"
                || self.owner(parameter, source, cancellation)?.is_none()
            {
                return Ok(None);
            }
            let Some(name) = field(parameter, "name") else {
                return Ok(None);
            };
            // Exact ASCII spelling avoids asserting a Unicode case-folding model.
            if source
                .get(name.byte_range())
                .is_none_or(|text| !text.is_ascii())
            {
                return Ok(None);
            }
            bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            bindings.push(BindingProof {
                name: name.byte_range(),
                scope: body.byte_range(),
                available: body.start_byte(),
                kind: BindingKind::Formal,
            });
        }

        let mut cursor = body.walk();
        loop {
            if !self.visit_body_node(cancellation)? {
                return Ok(None);
            }
            let node = cursor.node();
            // Unmodeled declaration scopes cannot borrow the callable's locals.
            if !clean(node, source)
                || matches!(
                    node.kind(),
                    "catch_clause"
                        | "for_statement"
                        | "enhanced_for_statement"
                        | "when_sobject_type"
                        | "inferred_parameters"
                        | "query_expression"
                        | "class_body"
                        | "trigger_declaration"
                        | "trigger_body"
                )
                || (owner_kind(node) && node.kind() != "local_variable_declaration")
            {
                return Ok(None);
            }
            if node.kind() == "local_variable_declaration" {
                let Some(scope) = node
                    .parent()
                    .filter(|parent| matches!(parent.kind(), "block" | "constructor_body"))
                else {
                    return Ok(None);
                };
                let supported = self.owner(node, source, cancellation)?.is_some();
                let mut declarations = node.walk();
                for declarator in node.children_by_field_name("declarator", &mut declarations) {
                    if !self.visit_body_node(cancellation)? {
                        return Ok(None);
                    }
                    let Some(name) =
                        field(declarator, "name").filter(|name| written_name(*name, source))
                    else {
                        return Ok(None);
                    };
                    if source
                        .get(name.byte_range())
                        .is_none_or(|name| !name.is_ascii())
                    {
                        return Ok(None);
                    }
                    bindings
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    bindings.push(BindingProof {
                        name: name.byte_range(),
                        scope: scope.byte_range(),
                        // Earlier declarators can supply later initializers;
                        // a declaration still cannot justify its own value.
                        available: declarator.end_byte(),
                        kind: if supported {
                            BindingKind::Local
                        } else {
                            BindingKind::Barrier
                        },
                    });
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
                    bindings.sort_unstable_by(|left, right| {
                        folded_cmp(
                            source,
                            &left.name,
                            source.get(right.name.clone()).unwrap_or_default(),
                        )
                    });
                    cancellation.check()?;
                    return Ok(Some(FormalProof {
                        body: body.byte_range(),
                        bindings,
                    }));
                }
            }
        }
    }
}

fn folded_cmp(source: &[u8], range: &Range<usize>, name: &[u8]) -> std::cmp::Ordering {
    source
        .get(range.clone())
        .unwrap_or_default()
        .iter()
        .map(u8::to_ascii_lowercase)
        .cmp(name.iter().map(u8::to_ascii_lowercase))
}

fn formal_value_position(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "return_statement"
        | "throw_statement"
        | "argument_list"
        | "binary_expression"
        | "unary_expression"
        | "update_expression"
        | "parenthesized_expression"
        | "ternary_expression"
        | "array_access"
        | "array_initializer"
        | "map_entry" => true,
        "field_access" | "method_invocation" => field(parent, "object") == Some(node),
        "assignment_expression" => field(parent, "right") == Some(node),
        "variable_declarator" => field(parent, "value") == Some(node),
        "cast_expression" => field(parent, "value") == Some(node),
        "instanceof_expression" => field(parent, "left") == Some(node),
        _ => false,
    }
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Option<Node<'tree>> {
    node.child_by_field_name(name)
        .filter(|child| child.parent() == Some(node))
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn owner_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "constructor_declaration"
            | "method_declaration"
            | "field_declaration"
            | "local_variable_declaration"
            | "formal_parameter"
            | "enum_constant"
    )
}

fn written_name(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() != "identifier" || !clean(node, source) {
        return false;
    }
    let Some(text) = source
        .get(node.byte_range())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return false;
    };
    !text.is_empty()
        && structural_captured_name_for_language("apex", text, text.len()).as_deref() == Some(text)
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

fn terminal_semicolon<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut last = None;
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

fn member_owner(node: Node<'_>) -> Option<Node<'_>> {
    let body = node.parent()?;
    let owner = body.parent()?;
    let kind_matches = matches!(
        (body.kind(), owner.kind()),
        ("class_body", "class_declaration") | ("interface_body", "interface_declaration")
    );
    (kind_matches && field(owner, "body") == Some(body)).then_some(owner)
}

fn declaration_position(node: Node<'_>) -> bool {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            node.parent()
                .is_some_and(|parent| parent.kind() == "parser_output")
                || member_owner(node).is_some()
        }
        "constructor_declaration" | "field_declaration" => {
            member_owner(node).is_some_and(|owner| owner.kind() == "class_declaration")
        }
        "local_variable_declaration" => node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "block" | "constructor_body")),
        "method_declaration" => member_owner(node).is_some(),
        "enum_constant" => node.parent().is_some_and(|body| {
            body.kind() == "enum_body"
                && body.parent().is_some_and(|owner| {
                    owner.kind() == "enum_declaration" && field(owner, "body") == Some(body)
                })
        }),
        "formal_parameter" => node.parent().is_some_and(|parameters| {
            parameters.kind() == "formal_parameters"
                && parameters.parent().is_some_and(|owner| {
                    matches!(
                        owner.kind(),
                        "constructor_declaration" | "method_declaration"
                    ) && field(owner, "parameters") == Some(parameters)
                })
        }),
        _ => false,
    }
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if !clean(node, source) || !declaration_position(node) {
        return Ok(None);
    }
    let mut declarators = 0usize;
    let header = match node.kind() {
        "local_variable_declaration" => {
            if field(node, "type").is_none_or(|ty| !clean(ty, source))
                || terminal_semicolon(node, source, cancellation)?.is_none()
            {
                return Ok(None);
            }
            let mut cursor = node.walk();
            let mut first = None;
            for declarator in node.children_by_field_name("declarator", &mut cursor) {
                cancellation.check()?;
                if declarator.parent() != Some(node)
                    || !clean(declarator, source)
                    || declarator.kind() != "variable_declarator"
                    || field(declarator, "name").is_none_or(|name| !written_name(name, source))
                {
                    return Ok(None);
                }
                first.get_or_insert(declarator);
                declarators = declarators
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-apex-declarator-accounting"))?;
            }
            let Some(declarator) = first else {
                return Ok(None);
            };
            let Some(name) = field(declarator, "name") else {
                return Ok(None);
            };
            let end = field(declarator, "dimensions")
                .map_or(name.end_byte(), |dimensions| dimensions.end_byte());
            node.start_byte()..end
        }
        "field_declaration" => {
            if field(node, "type").is_none_or(|ty| !clean(ty, source))
                || terminal_semicolon(node, source, cancellation)?.is_none()
            {
                return Ok(None);
            }
            let mut cursor = node.walk();
            for child in node.children_by_field_name("declarator", &mut cursor) {
                cancellation.check()?;
                if child.parent() != Some(node)
                    || child.kind() != "variable_declarator"
                    || !clean(child, source)
                    || field(child, "name").is_none_or(|name| !written_name(name, source))
                {
                    return Ok(None);
                }
                declarators = declarators
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-apex-declarator-accounting"))?;
            }
            if declarators == 0 {
                return Ok(None);
            }
            node.byte_range()
        }
        "formal_parameter" => {
            if field(node, "type").is_none_or(|ty| !clean(ty, source))
                || field(node, "name").is_none_or(|name| !written_name(name, source))
            {
                return Ok(None);
            }
            node.byte_range()
        }
        "enum_constant" => {
            if field(node, "name").is_none_or(|name| !written_name(name, source)) {
                return Ok(None);
            }
            node.byte_range()
        }
        _ => {
            if field(node, "name").is_none_or(|name| !written_name(name, source)) {
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "constructor_declaration" | "method_declaration"
            ) {
                let Some(parameters) = field(node, "parameters") else {
                    return Ok(None);
                };
                if !boundary(parameters, source, b"(", b")", cancellation)?
                    || (node.kind() == "method_declaration"
                        && field(node, "type").is_none_or(|ty| !clean(ty, source)))
                {
                    return Ok(None);
                }
            }
            let delimiter = if let Some(body) = field(node, "body") {
                if !boundary(body, source, b"{", b"}", cancellation)? {
                    return Ok(None);
                }
                body
            } else if node.kind() == "method_declaration" {
                let Some(semicolon) = terminal_semicolon(node, source, cancellation)? else {
                    return Ok(None);
                };
                semicolon
            } else {
                return Ok(None);
            };
            let Some(header) = header_before(node, delimiter, cancellation)? else {
                return Ok(None);
            };
            header
        }
    };
    Ok(Some(OwnerProof {
        extent: node.byte_range(),
        header,
        declarators,
    }))
}

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let target = node.byte_range();
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.is_missing() || owner.kind() == "ERROR" {
            return Ok(false);
        }
        if owner_kind(owner) {
            let Some(proof) = cache.owner(owner, source, cancellation)? else {
                return Ok(false);
            };
            if target.start < proof.extent.start || target.end > proof.extent.end {
                return Ok(false);
            }
        }
        // Root error state may describe an unrelated malformed sibling.
        current = owner.parent();
    }
    Ok(true)
}

fn field_owner(node: Node<'_>) -> Option<Node<'_>> {
    node.parent().filter(|parent| {
        parent.kind() == "field_declaration" && node.kind() == "variable_declarator"
    })
}

fn local_owner(node: Node<'_>) -> Option<Node<'_>> {
    node.parent().filter(|parent| {
        parent.kind() == "local_variable_declaration" && node.kind() == "variable_declarator"
    })
}

fn definition_node(node: Node<'_>) -> Option<Node<'_>> {
    field(node, "name")
}

fn nonlexical_identifier(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let target = node.byte_range();
    let mut current = Some(node);
    while let Some(child) = current {
        cancellation.check()?;
        let Some(parent) = child.parent() else {
            break;
        };
        if matches!(
            parent.kind(),
            "annotation"
                | "switch_label"
                | "type_arguments"
                | "type_parameters"
                | "superclass"
                | "interfaces"
                | "extends_interfaces"
                | "scoped_type_identifier"
                | "generic_type"
        ) || (field(parent, "type")
            .is_some_and(|ty| ty.start_byte() <= target.start && target.end <= ty.end_byte()))
        {
            return Ok(true);
        }
        if matches!(
            parent.kind(),
            "break_statement" | "continue_statement" | "when_sobject_type"
        ) || ((owner_kind(parent)
            || matches!(
                parent.kind(),
                "variable_declarator" | "type_parameter" | "enhanced_for_statement"
            ))
            && field(parent, "name") == Some(child))
        {
            return Ok(true);
        }
        current = Some(parent);
    }
    Ok(false)
}

/// Admits source identities only under clean, direct native declaration owners.
/// Cancellation/allocation errors propagate; exhausted cache entries are exposed
/// to the caller so a partial identity scan cannot masquerade as complete.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "parser_output" || role == StructuralRole::Expression {
        return Ok(true);
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::Documentation | StructuralRole::StringLiteral
    ) {
        return Ok(clean(node, source));
    }
    if !valid_owner_chain(node, source, cancellation, cache)? {
        return Ok(false);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        if let Some(owner) = field_owner(node).or_else(|| local_owner(node)) {
            let Some(proof) = cache.owner(owner, source, cancellation)? else {
                return Ok(false);
            };
            return Ok(clean(node, source)
                && definition_node(node).is_some_and(|name| written_name(name, source))
                && (role != StructuralRole::Signature || proof.declarators == 1));
        }
        return Ok(owner_kind(node) && cache.owner(node, source, cancellation)?.is_some());
    }
    if role == StructuralRole::Reference {
        if let Some(invocation) = node.parent().filter(|parent| {
            parent.kind() == "method_invocation" && field(*parent, "name") == Some(node)
        }) && calls::classify(invocation, source, cancellation, cache)?.is_some()
        {
            return Ok(false);
        }
        return Ok(written_name(node, source) && !nonlexical_identifier(node, cancellation)?);
    }
    Ok(clean(node, source))
}

/// Maps admitted captures to original byte ranges without rewriting source.
/// Shared headers intentionally remain non-container expression evidence,
/// and member/call spellings never establish target bindings. A lexical marker
/// certifies only the native lexical proof; lowering still requires its entity.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    if matches!(role, StructuralRole::ScopedCall) {
        return Err(query_failure("query-apex-unproven-call"));
    }
    let mut depth_node = node;
    let mut range = node.byte_range();
    let syntax = if role == StructuralRole::Call {
        match calls::classify(node, source, cancellation, cache)? {
            Some(calls::CallKind::Private) => "apex.private_instance_call",
            Some(calls::CallKind::Virtual) => "apex.virtual_instance_call",
            None => "apex.unproven_call",
        }
    } else if role == StructuralRole::CallName {
        let Some(invocation) = node.parent().filter(|parent| {
            parent.kind() == "method_invocation" && field(*parent, "name") == Some(node)
        }) else {
            return Err(query_failure("query-apex-call-name"));
        };
        if !clean(invocation, source) || !written_name(node, source) {
            return Err(query_failure("query-apex-call-name"));
        }
        "apex.method_name"
    } else if node.kind() == "parser_output" {
        range = 0..source.len();
        "apex.file"
    } else if role == StructuralRole::Expression {
        if node.kind() == "local_variable_declaration"
            && cache.owner(node, source, cancellation)?.is_none()
        {
            "apex.unsupported_local"
        } else if node.kind() == "field_declaration"
            && cache.owner(node, source, cancellation)?.is_none()
        {
            if clean(node, source) && member_owner(node).is_some() {
                "apex.accessor_semantics"
            } else {
                "apex.unsupported_owner"
            }
        } else if !valid_owner_chain(node, source, cancellation, cache)? {
            "apex.unsupported_owner"
        } else {
            match node.kind() {
                "field_declaration"
                    if cache
                        .owner(node, source, cancellation)?
                        .is_some_and(|proof| proof.declarators > 1) =>
                {
                    "apex.shared_field_header"
                }
                "local_variable_declaration"
                    if cache
                        .owner(node, source, cancellation)?
                        .is_some_and(|proof| proof.declarators > 1) =>
                {
                    "apex.shared_local_header"
                }
                "method_invocation" => match calls::classify(node, source, cancellation, cache)? {
                    Some(calls::CallKind::Private) => "apex.private_application",
                    Some(calls::CallKind::Virtual) => "apex.virtual_application",
                    None => "apex.application",
                },
                "field_access" => "apex.member_access",
                "annotation" => "apex.annotation_semantics",
                _ => "apex.body_semantics",
            }
        }
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        let syntax = if let Some(owner) = local_owner(node) {
            let proof = cache
                .owner(owner, source, cancellation)?
                .ok_or_else(|| query_failure("query-apex-local-owner"))?;
            if proof.declarators == 1 {
                depth_node = owner;
                range = if role == StructuralRole::Signature {
                    proof.header
                } else {
                    proof.extent
                };
                "apex.local_variable"
            } else {
                "apex.local_variable_fragment"
            }
        } else if let Some(owner) = field_owner(node) {
            let proof = cache
                .owner(owner, source, cancellation)?
                .ok_or_else(|| query_failure("query-apex-field-owner"))?;
            if proof.declarators == 1 {
                depth_node = owner;
                range = if role == StructuralRole::Signature {
                    proof.header
                } else {
                    proof.extent
                };
                "apex.field"
            } else {
                "apex.field_fragment"
            }
        } else {
            if role == StructuralRole::Signature {
                range = cache
                    .owner(node, source, cancellation)?
                    .ok_or_else(|| query_failure("query-apex-header"))?
                    .header;
            }
            match node.kind() {
                "class_declaration" => "apex.class",
                "interface_declaration" => "apex.interface",
                "enum_declaration" => "apex.enum",
                "constructor_declaration" => "apex.constructor",
                "method_declaration" => "apex.method",
                "formal_parameter" => "apex.parameter",
                "enum_constant" => "apex.constant",
                _ => return Err(query_failure("query-apex-declaration-kind")),
            }
        };
        if role == StructuralRole::Definition {
            range = definition_node(node)
                .ok_or_else(|| query_failure("query-apex-definition-name"))?
                .byte_range();
        }
        syntax
    } else if role == StructuralRole::Reference {
        match node.parent() {
            Some(parent)
                if parent.kind() == "method_invocation" && field(parent, "name") == Some(node) =>
            {
                "apex.unproven_call"
            }
            Some(parent)
                if parent.kind() == "field_access" && field(parent, "field") == Some(node) =>
            {
                "apex.member"
            }
            _ => match cache.lexical_use(node, source, cancellation)? {
                Some(BindingKind::Formal) => "apex.formal_identifier",
                Some(BindingKind::Local) => "apex.local_identifier",
                Some(BindingKind::Barrier) | None => "apex.identifier",
            },
        }
    } else {
        match role {
            StructuralRole::Comment | StructuralRole::Documentation => "apex.comment",
            StructuralRole::StringLiteral => "apex.string",
            StructuralRole::Scope => "apex.body",
            _ => return Err(query_failure("query-apex-unproven-role")),
        }
    };
    let mut depth = native_depth(depth_node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "parser_output" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-apex-depth"))?;
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
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Apex))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn owner_cache_reuses_proofs_and_fails_closed_at_its_bound() {
        let source = b"class First {} class Second {}";
        let tree = tree(source);
        let first = tree.root_node().named_child(0).unwrap();
        let second = tree.root_node().named_child(1).unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1, 64);
        for _ in 0..4 {
            assert!(valid_owner_chain(first, source, &cancellation, &mut cache).unwrap());
        }
        assert_eq!(cache.owners.len(), 1);
        assert!(!valid_owner_chain(second, source, &cancellation, &mut cache).unwrap());
        assert!(cache.exhausted());
        assert_eq!(cache.owners.len(), 1);
    }

    #[test]
    fn cancelled_cached_owner_proof_is_not_reused() {
        let source = b"class Written {}";
        let tree = tree(source);
        let owner = tree.root_node().named_child(0).unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1, 64);
        assert!(valid_owner_chain(owner, source, &cancellation, &mut cache).unwrap());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(valid_owner_chain(owner, source, &cancellation, &mut cache).is_err());
        assert!(
            candidate(
                owner,
                StructuralRole::Signature,
                source,
                &cancellation,
                &mut cache
            )
            .is_err()
        );
    }

    fn return_use<'tree>(tree: &'tree Tree, source: &[u8]) -> Node<'tree> {
        let start = source
            .windows(b"return Value".len())
            .position(|window| window == b"return Value")
            .unwrap()
            + b"return ".len();
        let node = tree
            .root_node()
            .named_descendant_for_byte_range(start, start + b"Value".len())
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        node
    }

    #[test]
    fn formal_proof_scans_each_callable_once_and_obeys_existing_bounds() {
        let source = b"class Written { Integer Read(Integer Value) { return Value; } }";
        let tree = tree(source);
        let node = return_use(&tree, source);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(64, 64);
        assert!(valid_owner_chain(node, source, &cancellation, &mut cache).unwrap());
        assert!(cache.formal_use(node, source, &cancellation).unwrap());
        let mut remaining = cache.remaining_body_nodes;
        for _ in 0..4 {
            assert!(cache.formal_use(node, source, &cancellation).unwrap());
            // Three ancestors and one matching binding are charged per use;
            // parameter/body scans are cached rather than repeated.
            remaining -= 4;
            assert_eq!(cache.remaining_body_nodes, remaining);
        }
        assert_eq!(cache.formals.len(), 1);
        let mut bounded = CaptureCache::new(64, 2);
        assert!(!bounded.formal_use(node, source, &cancellation).unwrap());
        assert!(bounded.exhausted());
        assert_eq!(bounded.remaining_body_nodes, 0);
        assert!(bounded.owners.is_empty());
        assert!(bounded.formals.is_empty());
        assert!(!bounded.formal_use(node, source, &cancellation).unwrap());
    }

    #[test]
    fn cancelled_cached_formal_proof_is_not_reused() {
        let source = b"class Written { Integer Read(Integer Value) { return Value; } }";
        let tree = tree(source);
        let node = return_use(&tree, source);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(64, 64);
        assert!(cache.formal_use(node, source, &cancellation).unwrap());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(cache.formal_use(node, source, &cancellation).is_err());
    }

    #[test]
    fn malformed_callable_cannot_certify_a_formal_use() {
        let source = b"class Written { Integer Read(Integer Value) { return Value;";
        let tree = tree(source);
        let node = return_use(&tree, source);
        let mut cache = CaptureCache::new(64, 64);
        assert!(
            !cache
                .formal_use(node, source, &Cancellation::new())
                .unwrap()
        );
        assert!(cache.formals.values().all(Option::is_none));
    }

    #[test]
    fn differently_cased_formals_are_ambiguous_even_for_an_exact_use() {
        let source =
            b"class Written { Integer Read(Integer Value, Integer value) { return Value; } }";
        let tree = tree(source);
        let node = return_use(&tree, source);
        let mut cache = CaptureCache::new(64, 128);
        assert!(
            !cache
                .formal_use(node, source, &Cancellation::new())
                .unwrap()
        );
        assert!(!cache.exhausted());
    }

    #[test]
    fn local_proof_reuses_callable_work_and_rejects_a_cached_use_at_the_bound() {
        let source = b"class Written { Integer Read(Integer Input) { Integer Value = Input; return Value; } }";
        let tree = tree(source);
        let node = return_use(&tree, source);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(64, 128);
        assert_eq!(
            cache.lexical_use(node, source, &cancellation).unwrap(),
            Some(BindingKind::Local)
        );
        let remaining = cache.remaining_body_nodes;
        assert_eq!(
            cache.lexical_use(node, source, &cancellation).unwrap(),
            Some(BindingKind::Local)
        );
        assert_eq!(cache.remaining_body_nodes, remaining - 4);
        assert_eq!(cache.formals.len(), 1);
        cache.remaining_body_nodes = 3;
        assert_eq!(
            cache.lexical_use(node, source, &cancellation).unwrap(),
            None
        );
        assert!(cache.exhausted());
        assert_eq!(cache.remaining_body_nodes, 0);
    }

    #[test]
    fn cancelled_cached_local_proof_is_not_reused() {
        let source = b"class Written { Integer Read() { Integer Value = 1; return Value; } }";
        let tree = tree(source);
        let node = return_use(&tree, source);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(64, 128);
        assert_eq!(
            cache.lexical_use(node, source, &cancellation).unwrap(),
            Some(BindingKind::Local)
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(cache.lexical_use(node, source, &cancellation).is_err());
    }
}
