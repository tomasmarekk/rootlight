//! F# selectors retain source-backed declaration and import evidence.
//! Repeated recursive heads and signature contracts need native ownership checks.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn valid(node: Node<'_>) -> bool {
    !node.is_missing() && !node.has_error() && node.start_byte() < node.end_byte()
}

fn direct_kind<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child.kind() == kind && selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(selected.filter(|node| valid(*node)))
}

fn has_direct_kind(
    node: Node<'_>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child.kind() == kind && valid(child) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn signature_member_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let Some(spec) = direct_kind(node, "curried_spec", cancellation)? else {
        return Ok("fsharp.signature_member");
    };
    // Indexed property contracts have argument specs too. Only direct written
    // accessor tokens distinguish them from an ordinary curried method.
    let accessor = has_direct_kind(node, "with", cancellation)?
        && (has_direct_kind(node, "get", cancellation)?
            || has_direct_kind(node, "set", cancellation)?);
    // A parenthesized function-valued property has no direct arguments_spec.
    // Inspecting arrow text would wrongly promote its nested return type.
    Ok(
        if !accessor && has_direct_kind(spec, "arguments_spec", cancellation)? {
            "fsharp.signature_method"
        } else {
            "fsharp.signature_property"
        },
    )
}

fn ambiguous_type_abbreviation(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.children_by_field_name("block", &mut cursor) {
        cancellation.check()?;
        if child.is_named() && !child.is_extra() && selected.replace(child).is_some() {
            return Ok(false);
        }
    }
    let Some(mut ty) = selected.filter(|node| valid(*node)) else {
        return Ok(false);
    };
    while ty.kind() == "paren_type" {
        cancellation.check()?;
        let mut cursor = ty.walk();
        let mut selected = None;
        for child in ty.named_children(&mut cursor) {
            cancellation.check()?;
            if !child.is_extra() && selected.replace(child).is_some() {
                return Ok(false);
            }
        }
        let Some(inner) = selected.filter(|node| valid(*node)) else {
            return Ok(false);
        };
        ty = inner;
    }
    if ty.kind() != "simple_type" {
        return Ok(false);
    }
    let Some(path) = direct_kind(ty, "long_identifier", cancellation)? else {
        return Ok(false);
    };
    let mut cursor = path.walk();
    let mut identifiers = 0usize;
    for child in path.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if child.kind() != "identifier" {
            return Ok(false);
        }
        identifiers = identifiers
            .checked_add(1)
            .ok_or_else(|| super::query_failure("fsharp-type-name-count"))?;
    }
    Ok(identifiers == 1)
}

fn object_type_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if has_direct_kind(node, "struct", cancellation)? {
        return Ok("fsharp.struct");
    }
    if has_direct_kind(node, "class", cancellation)?
        || has_direct_kind(node, "primary_constr_args", cancellation)?
    {
        return Ok("fsharp.class");
    }
    let mut cursor = node.walk();
    for member in node.named_children(&mut cursor) {
        cancellation.check()?;
        if member.kind() == "member_defn"
            && (has_direct_kind(member, "constructor_signature", cancellation)?
                || has_direct_kind(member, "additional_constr_defn", cancellation)?)
        {
            return Ok("fsharp.class");
        }
        if member.kind() == "type_extension_elements" {
            let mut cursor = member.walk();
            for declaration in member.named_children(&mut cursor) {
                cancellation.check()?;
                if declaration.kind() == "member_defn"
                    && (has_direct_kind(declaration, "constructor_signature", cancellation)?
                        || has_direct_kind(declaration, "additional_constr_defn", cancellation)?)
                {
                    return Ok("fsharp.class");
                }
            }
        }
    }
    infer_object_type_kind(node, cancellation)
}

#[derive(Default)]
struct ObjectBodyEvidence {
    has_element: bool,
    concrete: bool,
    dispatch_slot: bool,
}

enum ObjectElementKind {
    Concrete,
    DispatchSlot,
    Contract,
}

impl ObjectBodyEvidence {
    fn record(&mut self, kind: ObjectElementKind) {
        self.has_element = true;
        match kind {
            ObjectElementKind::Concrete => self.concrete = true,
            ObjectElementKind::DispatchSlot => self.dispatch_slot = true,
            ObjectElementKind::Contract => {}
        }
    }
}

fn infer_object_type_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let signature = match node.language().name() {
        Some("fsharp") => false,
        Some("fsharp_signature") => true,
        _ => return Ok("fsharp.ambiguous_type"),
    };
    if !valid(node) || !attribute_free_type_head(node, cancellation)? {
        return Ok("fsharp.ambiguous_type");
    }
    let mut evidence = ObjectBodyEvidence::default();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() || matches!(child.kind(), "type_name" | "line_comment") {
            continue;
        }
        if child.kind() == "type_extension_elements" {
            let mut cursor = child.walk();
            for element in child.named_children(&mut cursor) {
                cancellation.check()?;
                if element.is_extra() || element.kind() == "attributes" {
                    continue;
                }
                let Some(kind) = object_element_kind(element, signature, cancellation)? else {
                    return Ok("fsharp.ambiguous_type");
                };
                evidence.record(kind);
            }
        } else {
            let Some(kind) = object_element_kind(child, signature, cancellation)? else {
                return Ok("fsharp.ambiguous_type");
            };
            evidence.record(kind);
        }
    }
    // F# type-kind inference uses concrete elements and abstract slots.
    // Signature contracts without fields, constructors or slots remain opaque;
    // unbound type attributes cannot select a representation in this slice.
    Ok(if !evidence.has_element {
        "fsharp.ambiguous_type"
    } else if evidence.concrete {
        "fsharp.class"
    } else if !signature || evidence.dispatch_slot {
        "fsharp.interface"
    } else {
        "fsharp.ambiguous_type"
    })
}

fn attribute_free_type_head(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !node
        .parent()
        .is_some_and(|parent| parent.kind() == "type_definition" && valid(parent))
    {
        return Ok(false);
    }
    let Some(prefix) = previous_type_header_node(node, cancellation)? else {
        return Ok(false);
    };
    match prefix.kind() {
        "and" => Ok(true),
        "type" => Ok(previous_type_header_node(prefix, cancellation)?.is_none()),
        _ => Ok(false),
    }
}

fn previous_type_header_node<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        cancellation.check()?;
        if !sibling.is_extra() {
            return Ok(Some(sibling));
        }
        previous = sibling.prev_sibling();
    }
    Ok(None)
}

fn object_element_kind(
    node: Node<'_>,
    signature: bool,
    cancellation: &Cancellation,
) -> Result<Option<ObjectElementKind>, AdapterError> {
    cancellation.check()?;
    if !valid(node) {
        return Ok(None);
    }
    match node.kind() {
        "member_defn" => {
            if has_direct_kind(node, "val", cancellation)?
                || has_direct_kind(node, "constructor_signature", cancellation)?
                || has_direct_kind(node, "additional_constr_defn", cancellation)?
                || (!signature
                    && (has_direct_kind(node, "method_or_prop_defn", cancellation)?
                        || has_direct_kind(node, "value_declaration", cancellation)?))
            {
                return Ok(Some(ObjectElementKind::Concrete));
            }
            if has_direct_kind(node, "member_signature", cancellation)? {
                return Ok(if has_direct_kind(node, "abstract", cancellation)? {
                    Some(ObjectElementKind::DispatchSlot)
                } else if signature {
                    Some(ObjectElementKind::Contract)
                } else {
                    None
                });
            }
            Ok(None)
        }
        "function_or_value_defn" if !signature => Ok(Some(ObjectElementKind::Concrete)),
        "class_inherits_decl" => {
            let mut cursor = node.walk();
            let mut parts = 0usize;
            for part in node.children_by_field_name("block", &mut cursor) {
                cancellation.check()?;
                if part.is_extra() || !part.is_named() {
                    continue;
                }
                if !valid(part) || parts == 2 {
                    return Ok(None);
                }
                parts += 1;
            }
            Ok(match parts {
                1 => Some(ObjectElementKind::Contract),
                2 if !signature => Some(ObjectElementKind::Concrete),
                _ => None,
            })
        }
        "interface_implementation" => {
            let with_body = has_direct_kind(node, "with", cancellation)?;
            let mut cursor = node.walk();
            let mut has_member = false;
            for member in node.children_by_field_name("block", &mut cursor) {
                cancellation.check()?;
                if member.is_extra() || !member.is_named() {
                    continue;
                }
                if !valid(member) || member.kind() != "member_defn" {
                    return Ok(None);
                }
                has_member = true;
            }
            Ok(match (with_body, has_member) {
                (false, false) => Some(ObjectElementKind::Contract),
                (true, true) if !signature => Some(ObjectElementKind::Concrete),
                _ => None,
            })
        }
        _ => Ok(None),
    }
}

fn single_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    loop {
        cancellation.check()?;
        if !valid(node) {
            return Ok(None);
        }
        if matches!(node.kind(), "identifier" | "op_identifier") {
            return Ok(Some(node));
        }
        if !matches!(
            node.kind(),
            "identifier_pattern"
                | "long_identifier_or_op"
                | "long_identifier"
                | "property_or_ident"
        ) {
            return Ok(None);
        }
        let mut cursor = node.walk();
        let mut selected = None;
        for child in node.named_children(&mut cursor) {
            cancellation.check()?;
            if !child.is_extra() && selected.replace(child).is_some() {
                return Ok(None);
            }
        }
        let Some(child) = selected else {
            return Ok(None);
        };
        node = child;
    }
}

fn value_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() || matches!(child.kind(), "access_modifier" | "type_arguments") {
            continue;
        }
        if selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    match selected {
        Some(pattern) if pattern.kind() == "identifier_pattern" => {
            single_name(pattern, cancellation)
        }
        _ => Ok(None),
    }
}

fn selected_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    match node.kind() {
        "namespace" | "named_module" => {
            let mut cursor = node.walk();
            let mut selected = None;
            for name in node.children_by_field_name("name", &mut cursor) {
                cancellation.check()?;
                if name.kind() == "long_identifier" && selected.replace(name).is_some() {
                    return Ok(None);
                }
            }
            Ok(selected.filter(|name| valid(*name)))
        }
        "module_defn" | "record_field" | "union_type_case" | "enum_type_case" => {
            direct_kind(node, "identifier", cancellation)
        }
        "union_type_field" => {
            if direct_kind(node, ":", cancellation)?.is_some() {
                direct_kind(node, "identifier", cancellation)
            } else {
                Ok(None)
            }
        }
        "function_declaration_left" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                cancellation.check()?;
                if matches!(child.kind(), "identifier" | "op_identifier") {
                    return Ok(valid(child).then_some(child));
                }
                if !child.is_extra() && child.kind() != "access_modifier" {
                    return Ok(None);
                }
            }
            Ok(None)
        }
        "value_declaration_left" => value_name(node, cancellation),
        "value_definition" => match direct_kind(node, "value_declaration_left", cancellation)? {
            Some(left) => value_name(left, cancellation),
            None => Ok(None),
        },
        "record_type_defn"
        | "union_type_defn"
        | "type_abbrev_defn"
        | "enum_type_defn"
        | "anon_type_defn"
        | "interface_type_defn" => Ok(direct_kind(node, "type_name", cancellation)?
            .and_then(|head| head.child_by_field_name("type_name"))
            .filter(|name| valid(*name))),
        "method_or_prop_defn" => {
            let Some(name) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            if let Some(method) = name.child_by_field_name("method") {
                return Ok(valid(method).then_some(method));
            }
            single_name(name, cancellation)
        }
        "member_signature" => node.child_by_field_name("name").map_or_else(
            || direct_kind(node, "identifier", cancellation),
            |name| Ok(valid(name).then_some(name)),
        ),
        "member_defn" => {
            if direct_kind(node, "method_or_prop_defn", cancellation)?.is_some()
                || direct_kind(node, "member_signature", cancellation)?.is_some()
                || direct_kind(node, "additional_constr_defn", cancellation)?.is_some()
                || direct_kind(node, "constructor_signature", cancellation)?.is_some()
                || direct_kind(node, "value_declaration", cancellation)?.is_some()
            {
                return Ok(None);
            }
            if let Some(name) = direct_kind(node, "property_or_ident", cancellation)? {
                return single_name(name, cancellation);
            }
            if direct_kind(node, "val", cancellation)?.is_some() {
                return direct_kind(node, "identifier", cancellation);
            }
            Ok(None)
        }
        "additional_constr_defn" | "constructor_signature" => {
            direct_kind(node, "new", cancellation)
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Clone)]
struct BindingBounds {
    declaration: Range<usize>,
    signature: Range<usize>,
    body: Range<usize>,
}

struct BindingSummary {
    bounds: Option<BindingBounds>,
    kind: Option<&'static str>,
    nonrecursive: bool,
    recursive_eligible: bool,
    parameters: Option<usize>,
}

#[derive(Default)]
struct GroupSummary {
    recursive_bounds: Option<Range<usize>>,
}

struct RecursiveTokenSummary {
    group: usize,
    syntax: &'static str,
    bounds: Range<usize>,
}

struct RecursiveConnector<'tree> {
    token: Node<'tree>,
    syntax: &'static str,
    bounds: Range<usize>,
}

struct RecursiveGroupProof<'tree> {
    let_token: Option<Node<'tree>>,
    rec_token: Option<Node<'tree>>,
    separator: Option<Node<'tree>>,
    last_body_end: Option<usize>,
    members: usize,
    complete: bool,
}

impl<'tree> RecursiveGroupProof<'tree> {
    fn new() -> Self {
        Self {
            let_token: None,
            rec_token: None,
            separator: None,
            last_body_end: None,
            members: 0,
            complete: true,
        }
    }

    fn keyword(&mut self, node: Node<'tree>) {
        match node.kind() {
            "let" => {
                self.complete &= self.members == 0 && self.let_token.replace(node).is_none();
            }
            "rec" => {
                self.complete &= self.members == 0 && self.rec_token.replace(node).is_none();
            }
            "let!" => self.complete = false,
            _ => {}
        }
    }

    fn begin_member(
        &mut self,
        head: Node<'tree>,
    ) -> Result<Option<RecursiveConnector<'tree>>, AdapterError> {
        self.complete &= head.kind() == "function_declaration_left";
        let connector = if self.members == 0 {
            self.let_token.zip(self.rec_token).and_then(|(start, rec)| {
                (valid(start)
                    && valid(rec)
                    && start.end_byte() <= rec.start_byte()
                    && rec.end_byte() <= head.start_byte())
                .then_some(RecursiveConnector {
                    token: rec,
                    syntax: "fsharp.recursive_prefix",
                    bounds: start.start_byte()..head.start_byte(),
                })
            })
        } else {
            self.separator
                .take()
                .zip(self.last_body_end)
                .and_then(|(separator, previous_end)| {
                    (valid(separator)
                        && previous_end <= separator.start_byte()
                        && separator.end_byte() <= head.start_byte())
                    .then_some(RecursiveConnector {
                        token: separator,
                        syntax: "fsharp.recursive_separator",
                        bounds: previous_end..head.start_byte(),
                    })
                })
        };
        self.complete &= connector.is_some();
        self.members = self
            .members
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        Ok(connector.filter(|_| self.complete))
    }

    fn finish_member(&mut self, end: Option<usize>) {
        self.complete &= end.is_some();
        self.last_body_end = end;
    }

    fn separator(&mut self, node: Node<'tree>) {
        self.complete &= self.last_body_end.is_some() && self.separator.replace(node).is_none();
    }

    fn bounds(&self, group: Node<'_>) -> Option<Range<usize>> {
        let start = self.let_token?;
        let end = self.last_body_end?;
        (self.complete
            && self.rec_token.is_some()
            && self.members > 0
            && self.separator.is_none()
            && start.start_byte() == group.start_byte()
            && group.start_byte() < end
            && end <= group.end_byte())
        .then_some(group.start_byte()..end)
    }
}

struct FormalSummary {
    head: usize,
    parameters: usize,
    signature: Range<usize>,
}

struct ParameterSummary {
    complete: bool,
    unit_slots: usize,
    tuple_slots: usize,
}

struct TupleSlotSummary {
    parameters: usize,
    bounds: Range<usize>,
}

type NamedFormal<'tree> = (Node<'tree>, Range<usize>);

enum FormalSelection<'tree> {
    Pending,
    Named(Node<'tree>, Range<usize>),
    Tuple(Node<'tree>, Range<usize>),
    Unit,
    Unsupported,
}

/// Selects owned slots without treating parentheses as ignorable punctuation.
/// Named functions may request tuples; lambdas retain their atomic-only contract.
#[derive(Default)]
struct FormalSelector<'tree> {
    allow_tuples: bool,
    parenthesized: bool,
    group_name: Option<Node<'tree>>,
    group_tuple: Option<Node<'tree>>,
    group_start: Option<usize>,
    group_valid: bool,
}

impl<'tree> FormalSelector<'tree> {
    fn select(
        &mut self,
        child: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<FormalSelection<'tree>, AdapterError> {
        cancellation.check()?;
        if !self.parenthesized && native_unit_slot(child, cancellation)?.is_some() {
            return Ok(FormalSelection::Unit);
        }
        if child.kind() == "(" {
            if self.parenthesized {
                self.group_valid = false;
                return Ok(FormalSelection::Unsupported);
            }
            self.parenthesized = true;
            self.group_name = None;
            self.group_tuple = None;
            self.group_start = Some(child.start_byte());
            self.group_valid = valid(child);
            return Ok(FormalSelection::Pending);
        }
        let formal = if child.kind() == ")" {
            if self.parenthesized {
                self.parenthesized = false;
                if self.group_valid && valid(child) {
                    if let Some(tuple) = self.group_tuple.take()
                        && let Some(start) = self.group_start.take()
                    {
                        return Ok(FormalSelection::Tuple(tuple, start..child.end_byte()));
                    }
                    self.group_name
                        .take()
                        .zip(self.group_start.take())
                        .map(|(name, start)| (name, start..child.end_byte()))
                } else {
                    None
                }
            } else {
                None
            }
        } else if self.parenthesized {
            if self.allow_tuples && child.kind() == "repeat_pattern" && valid(child) {
                if self.group_tuple.replace(child).is_some() || self.group_name.is_some() {
                    self.group_valid = false;
                }
            } else if let Some(name) = single_formal_pattern(child, cancellation)? {
                if self.group_name.replace(name).is_some() || self.group_tuple.is_some() {
                    self.group_valid = false;
                }
            } else {
                self.group_valid = false;
            }
            return Ok(FormalSelection::Pending);
        } else if child.kind() == "long_identifier" {
            single_identifier_path(child, cancellation)?.map(|name| (name, child.byte_range()))
        } else {
            None
        };
        Ok(formal
            .filter(|(name, _)| formal_name_is_binding(*name, source))
            .map_or(FormalSelection::Unsupported, |(name, signature)| {
                FormalSelection::Named(name, signature)
            }))
    }
}

struct BodySummary {
    head: usize,
    unsupported: bool,
    scanned: bool,
}

struct LocalSummary {
    group: usize,
    head: usize,
    continuation: usize,
    wrapper_bounds: Range<usize>,
    group_bounds: Range<usize>,
    continuation_bounds: Range<usize>,
    continuation_unsupported: Option<bool>,
}

struct LambdaSummary {
    parameters: usize,
    body: usize,
    header_bounds: Range<usize>,
    body_bounds: Range<usize>,
    scanned: bool,
    unsupported: bool,
}

struct MatchSummary {
    scrutinee: usize,
    rules: usize,
    scanned: bool,
}

struct MatchBranchSummary {
    pattern: usize,
    arrow: usize,
    body: usize,
    pattern_bounds: Range<usize>,
    header_bounds: Range<usize>,
    body_bounds: Range<usize>,
    scanned: bool,
    unsupported: bool,
}

enum LocalAncestor<'tree> {
    Wrapper(Node<'tree>, LocalSummary),
    Function(Node<'tree>, Node<'tree>),
}

#[derive(Clone, Copy)]
enum BodyRegion<'tree> {
    Binding(Node<'tree>),
    Continuation(Node<'tree>, usize),
    Lambda(Node<'tree>, usize),
    MatchScrutinee(Node<'tree>, usize),
    MatchBranch(Node<'tree>, usize),
}

struct ApplicationSummary {
    callee: Option<usize>,
    argument: usize,
    argument_bounds: Range<usize>,
}

/// Scan-local native ownership proofs with separate storage and body-work bounds.
///
/// Stores only numeric identities and owned spans; query integration keeps it
/// within one scan and never retains native tree borrows. Repeated captures share
/// the same group, formal, record and body coverage/read inspection. Retained summaries
/// count against capture admission. Body and formal traversals are independently
/// bounded by the input tree's admitted node allowance; tuple inspection uses an
/// iterative, entry-bounded work list and is cached for later leaf captures.
pub(super) struct CaptureCache {
    groups: HashMap<usize, GroupSummary>,
    bindings: HashMap<usize, BindingSummary>,
    recursive_tokens: HashMap<usize, RecursiveTokenSummary>,
    bodies: HashMap<usize, BodySummary>,
    locals: HashMap<usize, Option<LocalSummary>>,
    lambdas: HashMap<usize, Option<LambdaSummary>>,
    matches: HashMap<usize, Option<MatchSummary>>,
    match_branches: HashMap<usize, Option<MatchBranchSummary>>,
    match_binders: HashMap<usize, usize>,
    parameters: HashMap<usize, ParameterSummary>,
    tuple_slots: HashMap<usize, TupleSlotSummary>,
    formal_heads: HashMap<usize, Option<FormalSummary>>,
    reads: HashSet<usize>,
    qualified_reads: HashSet<usize>,
    qualified_context: Option<bool>,
    qualified_records: HashMap<usize, bool>,
    signature_records: HashMap<usize, bool>,
    applications: HashMap<usize, ApplicationSummary>,
    collect_body_reads: bool,
    processing_regions: bool,
    body_visits: usize,
    formal_visits: usize,
    entries: usize,
    maximum_entries: usize,
    maximum_body_visits: usize,
    exhausted: bool,
    #[cfg(test)]
    direct_child_visits: usize,
}

impl CaptureCache {
    /// Creates a cache with independent retained-entry and native-body limits.
    ///
    /// Mandatory scans leave read collection disabled so RHS size cannot exhaust
    /// the ownership proofs required for declarations and formal identities.
    pub(super) fn new(
        maximum_entries: usize,
        collect_body_reads: bool,
        maximum_body_visits: usize,
    ) -> Self {
        Self {
            groups: HashMap::new(),
            bindings: HashMap::new(),
            recursive_tokens: HashMap::new(),
            bodies: HashMap::new(),
            locals: HashMap::new(),
            lambdas: HashMap::new(),
            matches: HashMap::new(),
            match_branches: HashMap::new(),
            match_binders: HashMap::new(),
            parameters: HashMap::new(),
            tuple_slots: HashMap::new(),
            formal_heads: HashMap::new(),
            reads: HashSet::new(),
            qualified_reads: HashSet::new(),
            qualified_context: None,
            qualified_records: HashMap::new(),
            signature_records: HashMap::new(),
            applications: HashMap::new(),
            collect_body_reads,
            processing_regions: false,
            body_visits: 0,
            formal_visits: 0,
            entries: 0,
            maximum_entries,
            maximum_body_visits,
            exhausted: false,
            #[cfg(test)]
            direct_child_visits: 0,
        }
    }

    /// Reports that storage or body-work exhaustion must terminate the scan.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn admit_entry(&mut self) -> Result<bool, AdapterError> {
        if self.entries >= self.maximum_entries {
            self.exhausted = true;
            return Ok(false);
        }
        self.entries = self
            .entries
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        Ok(true)
    }

    fn match_expression(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if node.kind() != "match_expression" || !valid(node) || self.exhausted {
            return Ok(false);
        }
        if let Some(summary) = self.matches.get(&node.id()) {
            return Ok(summary.is_some());
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.matches
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.matches.insert(node.id(), None);
        if let Some((scrutinee, rules)) = match_parts(node, cancellation)? {
            self.matches.insert(
                node.id(),
                Some(MatchSummary {
                    scrutinee: scrutinee.id(),
                    rules: rules.id(),
                    scanned: false,
                }),
            );
            return Ok(true);
        }
        Ok(false)
    }

    fn match_branch(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if node.kind() != "rule" || !valid(node) || self.exhausted {
            return Ok(false);
        }
        if let Some(summary) = self.match_branches.get(&node.id()) {
            return Ok(summary.is_some());
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.match_branches
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.match_branches.insert(node.id(), None);
        let Some(rules) = node.parent().filter(|parent| parent.kind() == "rules") else {
            return Ok(false);
        };
        let Some(owner) = rules
            .parent()
            .filter(|parent| parent.kind() == "match_expression")
        else {
            return Ok(false);
        };
        if !self.match_expression(owner, cancellation)?
            || !self
                .matches
                .get(&owner.id())
                .and_then(Option::as_ref)
                .is_some_and(|summary| summary.rules == rules.id())
        {
            return Ok(false);
        }
        let Some((pattern, arrow, body)) = match_branch_parts(node, cancellation)? else {
            return Ok(false);
        };
        let Some(binders) = self.match_pattern(pattern, source, cancellation)? else {
            return Ok(false);
        };
        for binder in binders {
            cancellation.check()?;
            if !self.admit_entry()? {
                return Ok(false);
            }
            self.match_binders
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.match_binders.insert(binder.id(), node.id());
        }
        self.match_branches.insert(
            node.id(),
            Some(MatchBranchSummary {
                pattern: pattern.id(),
                arrow: arrow.id(),
                body: body.id(),
                pattern_bounds: pattern.byte_range(),
                header_bounds: pattern.start_byte()..arrow.end_byte(),
                body_bounds: body.byte_range(),
                scanned: false,
                unsupported: false,
            }),
        );
        Ok(true)
    }

    fn match_pattern<'tree>(
        &mut self,
        pattern: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Vec<Node<'tree>>>, AdapterError> {
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(pattern);
        let mut binders = Vec::new();
        while let Some(node) = pending.pop() {
            cancellation.check()?;
            if self.formal_visits >= self.maximum_body_visits {
                self.exhausted = true;
                return Ok(None);
            }
            self.formal_visits = self
                .formal_visits
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            if !valid(node) {
                return Ok(None);
            }
            if node.kind() == "wildcard_pattern" && source.get(node.byte_range()) == Some(b"_") {
                continue;
            }
            if let Some(name) = single_identifier_path(node, cancellation)? {
                // Quoted and capitalized heads can denote semantic pattern lookups.
                // This slice admits only unquoted names that are necessarily binders.
                if !formal_name_is_binding(name, source)
                    || source
                        .get(name.byte_range())
                        .is_none_or(|text| text.starts_with(b"``"))
                    || source
                        .get(name.byte_range())
                        .and_then(|text| std::str::from_utf8(text).ok())
                        .is_none_or(|name| !name.chars().next().is_some_and(char::is_lowercase))
                {
                    return Ok(None);
                }
                if binders.len() >= self.maximum_entries.saturating_sub(self.entries) {
                    self.exhausted = true;
                    return Ok(None);
                }
                binders
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                binders.push(name);
                continue;
            }
            let mut cursor = node.walk();
            if node.kind() == "paren_pattern" {
                let mut open = None;
                let mut inner = None;
                let mut close = None;
                for child in node.children(&mut cursor) {
                    cancellation.check()?;
                    if child.is_extra() {
                        continue;
                    }
                    let previous = match child.kind() {
                        "(" => open.replace(child),
                        ")" => close.replace(child),
                        _ if child.is_named() => inner.replace(child),
                        _ => return Ok(None),
                    };
                    if previous.is_some() || !valid(child) {
                        return Ok(None);
                    }
                }
                let Some(((open, inner), close)) = open.zip(inner).zip(close) else {
                    return Ok(None);
                };
                if open.start_byte() != node.start_byte()
                    || open.end_byte() > inner.start_byte()
                    || inner.end_byte() > close.start_byte()
                    || close.end_byte() != node.end_byte()
                {
                    return Ok(None);
                }
                pending
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                pending.push(inner);
            } else if node.kind() == "repeat_pattern" {
                let mut expect_pattern = true;
                let mut count = 0usize;
                for child in node.children(&mut cursor) {
                    cancellation.check()?;
                    if child.is_extra() {
                        continue;
                    }
                    if !valid(child) {
                        return Ok(None);
                    }
                    if expect_pattern && child.is_named() {
                        if pending.len() >= self.maximum_entries.saturating_sub(self.entries) {
                            self.exhausted = true;
                            return Ok(None);
                        }
                        pending
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        pending.push(child);
                        count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    } else if expect_pattern || child.kind() != "," {
                        return Ok(None);
                    }
                    expect_pattern = !expect_pattern;
                }
                if expect_pattern || count < 2 {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
            if pending.len() > self.maximum_entries.saturating_sub(self.entries) {
                self.exhausted = true;
                return Ok(None);
            }
        }
        Ok(Some(binders))
    }

    fn match_binder(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = node;
        while let Some(parent) = current.parent() {
            cancellation.check()?;
            if parent.kind() == "rule" {
                return Ok(self.match_branch(parent, source, cancellation)?
                    && self.match_binders.get(&node.id()) == Some(&parent.id()));
            }
            if !matches!(
                parent.kind(),
                "identifier_pattern"
                    | "long_identifier"
                    | "long_identifier_or_op"
                    | "paren_pattern"
                    | "repeat_pattern"
            ) {
                return Ok(false);
            }
            current = parent;
        }
        Ok(false)
    }

    fn match_ancestor(
        &mut self,
        current: Node<'_>,
        parent: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        Ok(match parent.kind() {
            "rule" => {
                self.match_branch(parent, source, cancellation)?
                    && self
                        .match_branches
                        .get(&parent.id())
                        .and_then(Option::as_ref)
                        .is_some_and(|summary| summary.body == current.id())
            }
            "rules" => {
                current.kind() == "rule" && self.match_branch(current, source, cancellation)?
            }
            "match_expression" => {
                self.match_expression(parent, cancellation)?
                    && self
                        .matches
                        .get(&parent.id())
                        .and_then(Option::as_ref)
                        .is_some_and(|summary| {
                            current.id() == summary.scrutinee || current.id() == summary.rules
                        })
            }
            _ => false,
        })
    }

    fn match_branch_capture(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<(&'static str, Range<usize>)>, AdapterError> {
        if !self.collect_body_reads || self.exhausted {
            return Ok(None);
        }
        let owner = if node.kind() == "rule" {
            Some(node)
        } else {
            node.parent().filter(|parent| parent.kind() == "rule")
        };
        let Some(owner) = owner else {
            return Ok(None);
        };
        if !self.match_branch(owner, source, cancellation)? {
            return Ok(None);
        }
        self.ensure_region(owner, source, cancellation)?;
        Ok(self
            .match_branches
            .get(&owner.id())
            .and_then(Option::as_ref)
            .filter(|summary| summary.scanned && !self.exhausted)
            .and_then(|summary| {
                if node == owner {
                    Some(("fsharp.match_branch_body", summary.body_bounds.clone()))
                } else if node.id() == summary.arrow {
                    Some(("fsharp.match_branch_header", summary.header_bounds.clone()))
                } else if node.id() == summary.pattern {
                    Some((
                        "fsharp.match_branch_pattern",
                        summary.pattern_bounds.clone(),
                    ))
                } else {
                    None
                }
            }))
    }

    fn lambda(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if node.kind() != "fun_expression" || !valid(node) || self.exhausted {
            return Ok(false);
        }
        if let Some(summary) = self.lambdas.get(&node.id()) {
            return Ok(summary.is_some());
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.lambdas
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.lambdas.insert(node.id(), None);
        let Some((parameters, arrow, body)) = lambda_parts(node, cancellation)? else {
            return Ok(false);
        };
        let mut formals = Vec::new();
        let mut selector = FormalSelector::default();
        let mut unit_slots = 0usize;
        let mut cursor = parameters.walk();
        for child in parameters.children(&mut cursor) {
            cancellation.check()?;
            if child.is_extra() {
                continue;
            }
            let (name, signature) = match selector.select(child, source, cancellation)? {
                FormalSelection::Named(name, signature) => (name, signature),
                FormalSelection::Unit => {
                    unit_slots = unit_slots
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                    continue;
                }
                FormalSelection::Pending => continue,
                FormalSelection::Tuple(_, _) | FormalSelection::Unsupported => return Ok(false),
            };
            if formals.len() >= self.maximum_entries.saturating_sub(self.entries) {
                self.exhausted = true;
                return Ok(false);
            }
            formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            formals.push((name, signature));
        }
        if selector.parenthesized || formals.is_empty() && unit_slots == 0 {
            return Ok(false);
        }
        // Lambdas publish no partial formals: every native slot must be proved
        // before any identifier can shadow or expose an outer environment.
        for (name, signature) in formals {
            cancellation.check()?;
            if !self.admit_entry()? {
                return Ok(false);
            }
            self.formal_heads
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.formal_heads.insert(
                name.id(),
                Some(FormalSummary {
                    head: node.id(),
                    parameters: parameters.id(),
                    signature,
                }),
            );
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.parameters
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.parameters.insert(
            parameters.id(),
            ParameterSummary {
                complete: true,
                unit_slots,
                tuple_slots: 0,
            },
        );
        self.lambdas.insert(
            node.id(),
            Some(LambdaSummary {
                parameters: parameters.id(),
                body: body.id(),
                header_bounds: node.start_byte()..arrow.end_byte(),
                body_bounds: body.byte_range(),
                scanned: false,
                unsupported: true,
            }),
        );
        Ok(true)
    }

    fn lambda_capture(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<(&'static str, Range<usize>)>, AdapterError> {
        if !self.collect_body_reads || self.exhausted {
            return Ok(None);
        }
        let owner = if node.kind() == "fun_expression" {
            Some(node)
        } else if node.kind() == "->" {
            node.parent()
                .filter(|parent| parent.kind() == "fun_expression")
        } else {
            None
        };
        let Some(owner) = owner else {
            return Ok(None);
        };
        if !self.lambda(owner, source, cancellation)? {
            return Ok(None);
        }
        self.ensure_region(owner, source, cancellation)?;
        Ok(self
            .lambdas
            .get(&owner.id())
            .and_then(Option::as_ref)
            .filter(|summary| summary.scanned && !self.exhausted)
            .map(|summary| {
                if node == owner {
                    ("fsharp.lambda_body", summary.body_bounds.clone())
                } else {
                    ("fsharp.lambda_header", summary.header_bounds.clone())
                }
            }))
    }

    fn group(
        &mut self,
        group: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        if self.groups.contains_key(&group.id()) || self.exhausted {
            return Ok(());
        }
        if !self.admit_entry()? {
            return Ok(());
        }
        self.groups
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.groups.insert(group.id(), GroupSummary::default());
        if !valid(group) {
            return Ok(());
        }
        let local = group.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "declaration_expression" | "comp_declaration_expression"
            ) && parent.child_by_field_name("in").is_some()
        });
        let local_permitted = if let Some(wrapper) = group.parent().filter(|_| local) {
            self.local_binding(wrapper, source, cancellation)?
        } else {
            false
        };
        let mut cursor = group.walk();
        if !cursor.goto_first_child() {
            return Ok(());
        }
        let mut head = None;
        let mut equals = None;
        let mut body = None;
        let mut ambiguous = false;
        let mut nonrecursive = true;
        let mut let_count = 0usize;
        let mut sole_head = None;
        let mut multiple_heads = false;
        let mut recursive = RecursiveGroupProof::new();
        loop {
            cancellation.check()?;
            #[cfg(test)]
            {
                self.direct_child_visits = self
                    .direct_child_visits
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
            }
            let child = cursor.node();
            if !child.is_extra() {
                recursive.keyword(child);
                match child.kind() {
                    "let" => {
                        let_count = let_count
                            .checked_add(1)
                            .ok_or(SinkError::AccountingOverflow)?;
                    }
                    "rec" | "and" | "let!" => nonrecursive = false,
                    "function_declaration_left" | "value_declaration_left" => {
                        multiple_heads |= sole_head.replace(child).is_some();
                    }
                    _ => {}
                }
            }
            if !child.is_extra()
                && matches!(
                    child.kind(),
                    "function_declaration_left" | "value_declaration_left" | "and"
                )
            {
                if let Some(previous) = head.take() {
                    self.binding(
                        previous,
                        equals.take(),
                        body.take(),
                        ambiguous,
                        local && !local_permitted,
                        source,
                        cancellation,
                    )?;
                    if self.exhausted {
                        return Ok(());
                    }
                    recursive.finish_member(self.recursive_member_end(previous));
                }
                ambiguous = false;
                if child.kind() == "and" {
                    recursive.separator(child);
                } else {
                    if let Some(connector) = recursive.begin_member(child)?
                        && self.collect_body_reads
                    {
                        self.recursive_token(
                            group.id(),
                            connector.token,
                            connector.syntax,
                            connector.bounds,
                        )?;
                        if self.exhausted {
                            return Ok(());
                        }
                    }
                    head = Some(child);
                }
            } else if head.is_some() && !child.is_extra() {
                if child.kind() == "=" && equals.replace(child).is_some() {
                    ambiguous = true;
                }
                if cursor.field_name() == Some("body") && body.replace(child).is_some() {
                    ambiguous = true;
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        if let Some(head) = head {
            self.binding(
                head,
                equals,
                body,
                ambiguous,
                local && !local_permitted,
                source,
                cancellation,
            )?;
            recursive.finish_member(self.recursive_member_end(head));
        }
        let module_context = !self.exhausted && nonrecursive_module_context(group, cancellation)?;
        if module_context
            && let Some(bounds) = recursive.bounds(group)
            && let Some(summary) = self.groups.get_mut(&group.id())
        {
            summary.recursive_bounds = Some(bounds);
        }
        if !self.exhausted
            && nonrecursive
            && let_count == 1
            && !multiple_heads
            && let Some(head) = sole_head
            && module_context
            && let Some(name) = selected_name(head, cancellation)?
            && (head.kind() == "function_declaration_left" || formal_name_is_binding(name, source))
            && let Some(binding) = self.bindings.get_mut(&head.id())
        {
            binding.nonrecursive = binding.kind.is_some() && binding.bounds.is_some();
        }
        // Register every head before optional traversal. A local descendant may
        // need its enclosing function's complete native ownership and formals.
        if self.collect_body_reads && !self.processing_regions && !self.exhausted {
            let mut cursor = group.walk();
            for body in group.children_by_field_name("body", &mut cursor) {
                cancellation.check()?;
                if self.bodies.contains_key(&body.id()) {
                    self.body_regions(body, source, cancellation)?;
                    if self.exhausted {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn recursive_member_end(&self, head: Node<'_>) -> Option<usize> {
        self.bindings
            .get(&head.id())
            .filter(|binding| binding.recursive_eligible)
            .and_then(|binding| binding.bounds.as_ref())
            .map(|bounds| bounds.declaration.end)
    }

    fn recursive_token(
        &mut self,
        group: usize,
        token: Node<'_>,
        syntax: &'static str,
        bounds: Range<usize>,
    ) -> Result<(), AdapterError> {
        if !self.admit_entry()? {
            return Ok(());
        }
        self.recursive_tokens
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.recursive_tokens.insert(
            token.id(),
            RecursiveTokenSummary {
                group,
                syntax,
                bounds,
            },
        );
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one direct native head/body pairing preserves the query ownership contract"
    )]
    fn binding(
        &mut self,
        head: Node<'_>,
        equals: Option<Node<'_>>,
        body: Option<Node<'_>>,
        ambiguous: bool,
        local: bool,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        let bounds = match (equals, body) {
            (Some(equals), Some(body))
                if !ambiguous
                    && valid(head)
                    && valid(body)
                    && head.end_byte() <= equals.start_byte()
                    && equals.end_byte() <= body.start_byte() =>
            {
                Some(BindingBounds {
                    declaration: head.start_byte()..body.end_byte(),
                    signature: head.start_byte()..equals.start_byte(),
                    body: body.byte_range(),
                })
            }
            _ => None,
        };
        let kind =
            (!local && bounds.is_some()).then_some(if head.kind() == "function_declaration_left" {
                "fsharp.function"
            } else {
                "fsharp.variable"
            });
        if !self.admit_entry()? {
            return Ok(());
        }
        self.bindings
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.bindings.insert(
            head.id(),
            BindingSummary {
                bounds,
                kind,
                nonrecursive: false,
                recursive_eligible: false,
                parameters: None,
            },
        );
        let selected = selected_name(head, cancellation)?;
        if let Some(binding) = self.bindings.get_mut(&head.id()) {
            // Complete group membership proves written function names, not a
            // callable arity or the lexical usability of unsupported patterns.
            binding.recursive_eligible = kind == Some("fsharp.function")
                && selected.is_some_and(|name| name.kind() == "identifier");
        }
        if let Some(name) = selected {
            if !self.admit_entry()? {
                return Ok(());
            }
            self.formal_heads
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.formal_heads.insert(name.id(), None);
        }
        // Optional value reads share RHS ownership checks, but must not add cache
        // pressure to mandatory value identities or project formal headers.
        if selected.is_none()
            || !(kind == Some("fsharp.function")
                || kind == Some("fsharp.variable")
                    && (self.collect_body_reads
                        || head
                            .parent()
                            .and_then(|group| group.parent())
                            .is_some_and(|wrapper| {
                                self.locals.get(&wrapper.id()).is_some_and(Option::is_some)
                            })))
        {
            return Ok(());
        }
        if kind == Some("fsharp.function")
            && let Some(parameters) = direct_kind(head, "argument_patterns", cancellation)?
        {
            let mut all_simple = valid(parameters);
            let mut unit_slots = 0usize;
            let mut tuple_slots = 0usize;
            let mut count = 0usize;
            let mut selector = FormalSelector {
                allow_tuples: true,
                ..FormalSelector::default()
            };
            let mut cursor = parameters.walk();
            for child in parameters.children(&mut cursor) {
                cancellation.check()?;
                #[cfg(test)]
                {
                    self.direct_child_visits = self
                        .direct_child_visits
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                }
                if child.is_extra() {
                    continue;
                }
                count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                match selector.select(child, source, cancellation)? {
                    FormalSelection::Named(name, signature) => {
                        if !self.admit_entry()? {
                            return Ok(());
                        }
                        self.formal_heads
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.formal_heads.insert(
                            name.id(),
                            Some(FormalSummary {
                                head: head.id(),
                                parameters: parameters.id(),
                                signature,
                            }),
                        );
                    }
                    FormalSelection::Unit => {
                        unit_slots = unit_slots
                            .checked_add(1)
                            .ok_or(SinkError::AccountingOverflow)?;
                    }
                    FormalSelection::Tuple(tuple, bounds) => {
                        let Some(formals) = self.tuple_formals(tuple, source, cancellation)? else {
                            all_simple = false;
                            if self.exhausted {
                                return Ok(());
                            }
                            continue;
                        };
                        for (name, signature) in formals {
                            if !self.admit_entry()? {
                                return Ok(());
                            }
                            self.formal_heads
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.formal_heads.insert(
                                name.id(),
                                Some(FormalSummary {
                                    head: head.id(),
                                    parameters: parameters.id(),
                                    signature,
                                }),
                            );
                        }
                        if !self.admit_entry()? {
                            return Ok(());
                        }
                        self.tuple_slots
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.tuple_slots.insert(
                            tuple.id(),
                            TupleSlotSummary {
                                parameters: parameters.id(),
                                bounds,
                            },
                        );
                        tuple_slots = tuple_slots
                            .checked_add(1)
                            .ok_or(SinkError::AccountingOverflow)?;
                    }
                    FormalSelection::Pending => {}
                    FormalSelection::Unsupported => {
                        // Preserve partial written identities for named functions;
                        // only the complete marker can authorize lexical lookup.
                        all_simple = false;
                    }
                }
            }
            if !self.admit_entry()? {
                return Ok(());
            }
            self.parameters
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.parameters.insert(
                parameters.id(),
                ParameterSummary {
                    complete: all_simple && !selector.parenthesized && count > 0,
                    unit_slots,
                    tuple_slots,
                },
            );
            if let Some(binding) = self.bindings.get_mut(&head.id()) {
                binding.parameters = Some(parameters.id());
            }
        }
        if let Some(body) = body {
            if !self.admit_entry()? {
                return Ok(());
            }
            self.bodies
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.bodies.insert(
                body.id(),
                BodySummary {
                    head: head.id(),
                    unsupported: true,
                    scanned: false,
                },
            );
        }
        Ok(())
    }

    fn body_regions(
        &mut self,
        body: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let previous = self.processing_regions;
        self.processing_regions = true;
        let result = self.walk_body_regions(body, source, cancellation);
        self.processing_regions = previous;
        result
    }

    fn walk_body_regions(
        &mut self,
        body: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(BodyRegion::Binding(body));
        while let Some(region) = pending.pop() {
            cancellation.check()?;
            let node = match region {
                BodyRegion::Binding(node) => {
                    let Some(summary) =
                        self.bodies.get_mut(&node.id()).filter(|body| !body.scanned)
                    else {
                        continue;
                    };
                    summary.scanned = true;
                    node
                }
                BodyRegion::Continuation(node, wrapper) => {
                    let Some(summary) = self
                        .locals
                        .get_mut(&wrapper)
                        .and_then(Option::as_mut)
                        .filter(|local| local.continuation_unsupported.is_none())
                    else {
                        continue;
                    };
                    summary.continuation_unsupported = Some(true);
                    node
                }
                BodyRegion::Lambda(node, owner) => {
                    let Some(summary) = self
                        .lambdas
                        .get_mut(&owner)
                        .and_then(Option::as_mut)
                        .filter(|summary| !summary.scanned)
                    else {
                        continue;
                    };
                    summary.scanned = true;
                    node
                }
                BodyRegion::MatchScrutinee(node, owner) => {
                    let Some(summary) = self
                        .matches
                        .get_mut(&owner)
                        .and_then(Option::as_mut)
                        .filter(|summary| !summary.scanned && summary.scrutinee == node.id())
                    else {
                        continue;
                    };
                    summary.scanned = true;
                    node
                }
                BodyRegion::MatchBranch(node, owner) => {
                    let Some(summary) = self
                        .match_branches
                        .get_mut(&owner)
                        .and_then(Option::as_mut)
                        .filter(|summary| !summary.scanned && summary.body == node.id())
                    else {
                        continue;
                    };
                    summary.scanned = true;
                    node
                }
            };
            let unsupported = self.body_reads(node, source, cancellation, &mut pending)?;
            match region {
                BodyRegion::Binding(_) => {
                    if let Some(summary) = self.bodies.get_mut(&node.id()) {
                        summary.unsupported = unsupported;
                    }
                }
                BodyRegion::Continuation(_, wrapper) => {
                    if let Some(summary) = self.locals.get_mut(&wrapper).and_then(Option::as_mut) {
                        summary.continuation_unsupported = Some(unsupported);
                    }
                }
                BodyRegion::Lambda(_, owner) => {
                    if let Some(summary) = self.lambdas.get_mut(&owner).and_then(Option::as_mut) {
                        summary.unsupported = unsupported;
                    }
                }
                BodyRegion::MatchScrutinee(_, _) => {}
                BodyRegion::MatchBranch(_, owner) => {
                    if let Some(summary) =
                        self.match_branches.get_mut(&owner).and_then(Option::as_mut)
                    {
                        summary.unsupported = unsupported;
                    }
                }
            }
            if self.exhausted {
                break;
            }
        }
        Ok(())
    }

    fn body_reads<'tree>(
        &mut self,
        body: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
        pending: &mut Vec<BodyRegion<'tree>>,
    ) -> Result<bool, AdapterError> {
        let mut cursor = body.walk();
        let mut unsupported = false;
        loop {
            cancellation.check()?;
            // This work bound is shared by all exact bodies in the scan. It is
            // independent of retained summaries because literal-only trees can
            // require native traversal without contributing any cache entries.
            if self.body_visits >= self.maximum_body_visits {
                self.exhausted = true;
                return Ok(true);
            }
            self.body_visits = self
                .body_visits
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            let node = cursor.node();
            let descend = if node.is_extra() || !node.is_named() {
                false
            } else if !valid(node) {
                unsupported = true;
                false
            } else if node.kind() == "identifier" {
                if !self.reads.contains(&node.id()) {
                    if !self.admit_entry()? {
                        return Ok(true);
                    }
                    self.reads
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.reads.insert(node.id());
                }
                false
            } else if node.kind() == "match_expression" {
                // Pattern identifiers are not reads. Keep the scrutinee outside
                // arm scopes and visit only bodies with complete binder ownership.
                unsupported = true;
                if self.match_expression(node, cancellation)?
                    && let Some((scrutinee, rules)) = match_parts(node, cancellation)?
                {
                    let mut rules_cursor = rules.walk();
                    for branch in rules.named_children(&mut rules_cursor) {
                        cancellation.check()?;
                        if branch.is_extra() {
                            continue;
                        }
                        if self.match_branch(branch, source, cancellation)?
                            && let Some((_, _, rhs)) = match_branch_parts(branch, cancellation)?
                        {
                            pending
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            pending.push(BodyRegion::MatchBranch(rhs, branch.id()));
                        }
                        if self.exhausted {
                            return Ok(true);
                        }
                    }
                    pending
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    pending.push(BodyRegion::MatchScrutinee(scrutinee, node.id()));
                }
                false
            } else if node.kind() == "fun_expression" {
                if self.lambda(node, source, cancellation)? {
                    if let Some((_, _, rhs)) = lambda_parts(node, cancellation)? {
                        pending
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        pending.push(BodyRegion::Lambda(rhs, node.id()));
                    } else {
                        unsupported = true;
                    }
                } else {
                    unsupported = true;
                }
                false
            } else if node.kind() == "declaration_expression" {
                if self.local_binding(node, source, cancellation)? {
                    let group = direct_kind(node, "function_or_value_defn", cancellation)?;
                    let continuation = unique_continuation(node, cancellation)?;
                    if let Some((group, continuation)) = group.zip(continuation) {
                        self.group(group, source, cancellation)?;
                        pending
                            .try_reserve(2)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        pending.push(BodyRegion::Continuation(continuation, node.id()));
                        let mut cursor = group.walk();
                        for rhs in group.children_by_field_name("body", &mut cursor) {
                            cancellation.check()?;
                            if self.bodies.contains_key(&rhs.id()) {
                                pending
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                pending.push(BodyRegion::Binding(rhs));
                            }
                        }
                    } else {
                        unsupported = true;
                    }
                } else {
                    unsupported = true;
                }
                false
            } else if node.kind() == "sequential_expression" {
                let safe = sequential_region(node, cancellation)?;
                unsupported |= !safe;
                safe
            } else if matches!(node.kind(), "long_identifier" | "long_identifier_or_op") {
                if let Some(path) = qualified_identifier_path(node, cancellation)? {
                    if !self.qualified_reads.contains(&path.id()) {
                        if !self.admit_entry()? {
                            return Ok(true);
                        }
                        self.qualified_reads
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.qualified_reads.insert(path.id());
                    }
                    // A path is a lookup candidate, not proof of module or member dispatch.
                    unsupported = true;
                    false
                } else {
                    let simple = single_identifier_path(node, cancellation)?.is_some();
                    unsupported |= !simple;
                    simple
                }
            } else if literal_body_node(node.kind()) {
                // String/const wrappers may contain interpolation. Inspect every
                // exposed named child rather than treating the wrapper as proof.
                true
            } else if binder_free_expression(node.kind()) {
                // Native ownership alone cannot prove dispatch or evaluation.
                // Application witnesses need independent target/arity checks.
                unsupported |= matches!(
                    node.kind(),
                    "application_expression" | "infix_expression" | "prefixed_expression"
                );
                true
            } else {
                unsupported = true;
                false
            };
            if descend && cursor.goto_first_child() {
                continue;
            }
            loop {
                cancellation.check()?;
                self.summarize_application(cursor.node(), cancellation)?;
                if self.exhausted {
                    return Ok(true);
                }
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(unsupported);
                }
            }
        }
    }

    fn summarize_application(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        if node.kind() != "application_expression" || !valid(node) {
            return Ok(());
        }
        let Some((left, argument)) = application_operands(node, cancellation)? else {
            return Ok(());
        };
        let Some(argument_bounds) =
            application_argument_bounds(node, left, argument, cancellation)?
        else {
            return Ok(());
        };
        let callee = if left.kind() == "application_expression" {
            self.applications
                .get(&left.id())
                .and_then(|summary| summary.callee)
        } else {
            let simple = single_identifier_path(left, cancellation)?
                .filter(|name| self.reads.contains(&name.id()))
                .map(|name| name.id());
            simple.or(qualified_identifier_path(left, cancellation)?
                .filter(|path| self.qualified_reads.contains(&path.id()))
                .map(|path| path.id()))
        };
        if self.applications.contains_key(&node.id()) {
            return Ok(());
        }
        if !self.admit_entry()? {
            return Ok(());
        }
        self.applications
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.applications.insert(
            node.id(),
            ApplicationSummary {
                callee,
                argument: argument.id(),
                argument_bounds,
            },
        );
        Ok(())
    }

    fn application(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.collect_body_reads
            || self.exhausted
            || node.kind() != "application_expression"
            || !valid(node)
        {
            return Ok(false);
        }
        if !self.applications.contains_key(&node.id()) {
            self.ensure_region(node, source, cancellation)?;
        }
        Ok(!self.exhausted
            && self
                .applications
                .get(&node.id())
                .is_some_and(|summary| summary.callee.is_some()))
    }

    fn application_call(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.application(node, source, cancellation)? {
            return Ok(false);
        }
        // A curried application contributes one dispatch site, not one for each
        // binary prefix. Applications occurring as arguments remain separate.
        if let Some(parent) = node.parent()
            && parent.kind() == "application_expression"
            && let Some((left, _)) = application_operands(parent, cancellation)?
            && left.id() == node.id()
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn application_argument(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(parent) = node
            .parent()
            .filter(|parent| parent.kind() == "application_expression")
        else {
            return Ok(false);
        };
        Ok(self.application(parent, source, cancellation)?
            && self
                .applications
                .get(&parent.id())
                .is_some_and(|summary| summary.argument == node.id()))
    }

    fn nonrecursive_binding(
        &mut self,
        head: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.collect_body_reads
            || self.exhausted
            || !matches!(
                head.kind(),
                "function_declaration_left" | "value_declaration_left"
            )
        {
            return Ok(false);
        }
        self.ensure_head(head, source, cancellation)?;
        Ok(!self.exhausted
            && self
                .bindings
                .get(&head.id())
                .is_some_and(|binding| binding.nonrecursive))
    }

    fn recursive_capture(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<(&'static str, Range<usize>)>, AdapterError> {
        cancellation.check()?;
        if !self.collect_body_reads || self.exhausted {
            return Ok(None);
        }
        let group = if node.kind() == "function_or_value_defn" {
            node
        } else if matches!(node.kind(), "function_declaration_left" | "rec" | "and") {
            let Some(group) = node
                .parent()
                .filter(|parent| parent.kind() == "function_or_value_defn")
            else {
                return Ok(None);
            };
            group
        } else {
            return Ok(None);
        };
        self.group(group, source, cancellation)?;
        if self.exhausted {
            return Ok(None);
        }
        let Some(bounds) = self
            .groups
            .get(&group.id())
            .and_then(|summary| summary.recursive_bounds.as_ref())
        else {
            return Ok(None);
        };
        if node.id() == group.id() {
            return Ok(Some(("fsharp.recursive_group", bounds.clone())));
        }
        if node.kind() == "function_declaration_left" {
            return Ok(self
                .bindings
                .get(&node.id())
                .and_then(|binding| binding.bounds.as_ref())
                .map(|binding| ("fsharp.recursive_member", binding.declaration.clone())));
        }
        Ok(self
            .recursive_tokens
            .get(&node.id())
            .filter(|token| token.group == group.id())
            .map(|token| (token.syntax, token.bounds.clone())))
    }

    fn local_binding(
        &mut self,
        wrapper: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if wrapper.kind() != "declaration_expression"
            || !valid(wrapper)
            || self.exhausted
            || unique_continuation(wrapper, cancellation)?.is_none()
        {
            return Ok(false);
        }
        if let Some(summary) = self.locals.get(&wrapper.id()) {
            return Ok(summary.is_some());
        }
        let mut ancestors = Vec::new();
        let mut current = wrapper;
        let mut admitted = false;
        loop {
            cancellation.check()?;
            if current.kind() == "declaration_expression" {
                if let Some(summary) = self.locals.get(&current.id()) {
                    admitted = summary.is_some();
                    break;
                }
                if !self.admit_entry()? {
                    return Ok(false);
                }
                self.locals
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.locals.insert(current.id(), None);
                let Some(summary) = local_shape(current, source, cancellation, self)? else {
                    break;
                };
                ancestors
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                ancestors.push(LocalAncestor::Wrapper(current, summary));
            }
            let Some(parent) = current.parent().filter(|parent| valid(*parent)) else {
                break;
            };
            if parent.kind() == "function_or_value_defn" {
                if !direct_body(parent, current, cancellation)? {
                    break;
                }
                ancestors
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                ancestors.push(LocalAncestor::Function(parent, current));
                let Some(owner) = parent.parent().filter(|owner| valid(*owner)) else {
                    break;
                };
                if owner.kind() != "declaration_expression" {
                    break;
                }
                if unique_continuation(owner, cancellation)?.is_none() {
                    if !nonrecursive_module_context(parent, cancellation)? {
                        break;
                    }
                    admitted = true;
                    break;
                }
                current = owner;
                continue;
            }
            if parent.kind() == "fun_expression" {
                admitted = self.lambda(parent, source, cancellation)?
                    && self
                        .lambdas
                        .get(&parent.id())
                        .and_then(Option::as_ref)
                        .is_some_and(|summary| summary.body == current.id());
                break;
            }
            if parent.kind() == "declaration_expression" {
                if unique_continuation(parent, cancellation)? != Some(current) {
                    break;
                }
            } else if matches!(parent.kind(), "rule" | "rules" | "match_expression") {
                if !self.match_ancestor(current, parent, source, cancellation)? {
                    break;
                }
            } else if !matches!(parent.kind(), "paren_expression" | "begin_end_expression")
                && !(parent.kind() == "sequential_expression"
                    && sequential_region(parent, cancellation)?)
            {
                break;
            }
            current = parent;
        }
        if admitted {
            // Process ancestor ownership from the already reviewed outer scope.
            // This avoids recursive proof construction for deep local chains.
            let previous = self.processing_regions;
            self.processing_regions = true;
            let result = self.admit_local_ancestors(ancestors, source, cancellation);
            self.processing_regions = previous;
            result?;
        }
        Ok(!self.exhausted && self.locals.get(&wrapper.id()).is_some_and(Option::is_some))
    }

    fn admit_local_ancestors(
        &mut self,
        ancestors: Vec<LocalAncestor<'_>>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        for ancestor in ancestors.into_iter().rev() {
            cancellation.check()?;
            match ancestor {
                LocalAncestor::Wrapper(wrapper, summary) => {
                    self.locals.insert(wrapper.id(), Some(summary));
                }
                LocalAncestor::Function(group, body) => {
                    self.group(group, source, cancellation)?;
                    if self.exhausted || !self.local_owner_body(group, body) {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn local_owner_body(&self, group: Node<'_>, body: Node<'_>) -> bool {
        let Some(summary) = self.bodies.get(&body.id()) else {
            return false;
        };
        let Some(binding) = self.bindings.get(&summary.head).filter(|binding| {
            binding
                .bounds
                .as_ref()
                .is_some_and(|bounds| bounds.body == body.byte_range())
        }) else {
            return false;
        };
        let local = group
            .parent()
            .is_some_and(|wrapper| self.locals.get(&wrapper.id()).is_some_and(Option::is_some));
        if binding.kind == Some("fsharp.variable") {
            return local;
        }
        if binding.kind != Some("fsharp.function") {
            return false;
        }
        (local
            || binding.nonrecursive
            || binding.recursive_eligible
                && self
                    .groups
                    .get(&group.id())
                    .is_some_and(|group| group.recursive_bounds.is_some()))
            && binding
                .parameters
                .and_then(|id| self.parameters.get(&id))
                .is_some_and(|parameters| parameters.complete)
    }

    fn local_capture(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<(&'static str, Range<usize>)>, AdapterError> {
        if !self.collect_body_reads || self.exhausted {
            return Ok(None);
        }
        let wrapper = match node.kind() {
            "declaration_expression" => node,
            "function_or_value_defn" => {
                let Some(wrapper) = node.parent() else {
                    return Ok(None);
                };
                wrapper
            }
            "function_declaration_left" | "value_declaration_left" => {
                let Some(wrapper) = node.parent().and_then(|group| group.parent()) else {
                    return Ok(None);
                };
                wrapper
            }
            _ => return Ok(None),
        };
        if !self.local_binding(wrapper, source, cancellation)? {
            return Ok(None);
        }
        let Some(summary) = self.locals.get(&wrapper.id()).and_then(Option::as_ref) else {
            return Ok(None);
        };
        Ok(if node.id() == wrapper.id() {
            Some((
                "fsharp.local_continuation",
                summary.continuation_bounds.clone(),
            ))
        } else if node.id() == summary.group {
            Some(("fsharp.local_binding", summary.wrapper_bounds.clone()))
        } else if node.id() == summary.head {
            Some(("fsharp.local_group", summary.group_bounds.clone()))
        } else {
            None
        })
    }

    fn reviewed_callable_context(&self, head: Node<'_>) -> bool {
        self.bindings.get(&head.id()).is_some_and(|binding| {
            binding.nonrecursive
                || head
                    .parent()
                    .and_then(|group| group.parent())
                    .is_some_and(|wrapper| {
                        self.locals
                            .get(&wrapper.id())
                            .and_then(Option::as_ref)
                            .is_some_and(|local| local.head == head.id())
                    })
                || binding.recursive_eligible
                    && head.parent().is_some_and(|group| {
                        self.groups
                            .get(&group.id())
                            .is_some_and(|summary| summary.recursive_bounds.is_some())
                    })
        })
    }

    fn unsupported_body(
        &mut self,
        body: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.collect_body_reads || self.exhausted || !valid(body) {
            return Ok(false);
        }
        if let Some(owner) = body.parent().filter(|parent| parent.kind() == "rule") {
            self.ensure_region(body, source, cancellation)?;
            return Ok(!self.exhausted
                && self
                    .match_branches
                    .get(&owner.id())
                    .and_then(Option::as_ref)
                    .is_some_and(|summary| {
                        summary.body == body.id() && summary.scanned && summary.unsupported
                    }));
        }
        if let Some(owner) = body
            .parent()
            .filter(|parent| parent.kind() == "fun_expression")
        {
            self.ensure_region(body, source, cancellation)?;
            return Ok(!self.exhausted
                && self
                    .lambdas
                    .get(&owner.id())
                    .and_then(Option::as_ref)
                    .is_some_and(|summary| {
                        summary.body == body.id() && summary.scanned && summary.unsupported
                    }));
        }
        if let Some(wrapper) = body
            .parent()
            .filter(|parent| parent.kind() == "declaration_expression")
            && unique_continuation(wrapper, cancellation)? == Some(body)
            && self.local_binding(wrapper, source, cancellation)?
        {
            self.ensure_region(body, source, cancellation)?;
            return Ok(!self.exhausted
                && self
                    .locals
                    .get(&wrapper.id())
                    .and_then(Option::as_ref)
                    .is_some_and(|local| {
                        local.continuation == body.id()
                            && local.continuation_unsupported == Some(true)
                    }));
        }
        let Some(group) = body
            .parent()
            .filter(|parent| parent.kind() == "function_or_value_defn")
        else {
            return Ok(false);
        };
        self.group(group, source, cancellation)?;
        if self.bodies.contains_key(&body.id()) && !self.processing_regions {
            self.body_regions(body, source, cancellation)?;
        }
        Ok(!self.exhausted
            && self.bodies.get(&body.id()).is_some_and(|summary| {
                summary.unsupported
                    && self.bindings.get(&summary.head).is_some_and(|binding| {
                        matches!(binding.kind, Some("fsharp.function" | "fsharp.variable"))
                            && binding.bounds.as_ref().is_some_and(|bounds| {
                                bounds.body == body.byte_range()
                                    && bounds.body.start < bounds.body.end
                            })
                    })
            }))
    }

    fn ensure_head(
        &mut self,
        head: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        if !self.bindings.contains_key(&head.id())
            && let Some(group) = head
                .parent()
                .filter(|node| node.kind() == "function_or_value_defn")
        {
            self.group(group, source, cancellation)?;
        }
        Ok(())
    }

    fn bounds(
        &mut self,
        head: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<BindingBounds>, AdapterError> {
        self.ensure_head(head, source, cancellation)?;
        Ok(self
            .bindings
            .get(&head.id())
            .and_then(|binding| binding.bounds.clone()))
    }

    fn written_kind(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<&'static str>, AdapterError> {
        if matches!(
            node.kind(),
            "function_declaration_left" | "value_declaration_left"
        ) {
            self.ensure_head(node, source, cancellation)?;
            Ok(self
                .bindings
                .get(&node.id())
                .and_then(|binding| binding.kind))
        } else {
            written_kind(node, cancellation)
        }
    }

    fn tuple_formals<'tree>(
        &mut self,
        tuple: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Vec<NamedFormal<'tree>>>, AdapterError> {
        let remaining = self.maximum_entries.saturating_sub(self.entries);
        let mut pending = Vec::new();
        let mut formals = Vec::new();
        if remaining == 0 {
            self.exhausted = true;
            return Ok(None);
        }
        pending
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(tuple);
        while let Some(pattern) = pending.pop() {
            cancellation.check()?;
            if self.formal_visits >= self.maximum_body_visits {
                self.exhausted = true;
                return Ok(None);
            }
            self.formal_visits = self
                .formal_visits
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            if !valid(pattern) {
                return Ok(None);
            }
            let mut named = single_formal_pattern(pattern, cancellation)?
                .map(|name| (name, pattern.byte_range()));
            if named.is_none() && pattern.kind() == "paren_pattern" {
                let mut components = 0usize;
                let mut inner = None;
                let mut cursor = pattern.walk();
                for child in pattern.children(&mut cursor) {
                    cancellation.check()?;
                    if child.is_extra() {
                        continue;
                    }
                    if !valid(child) {
                        return Ok(None);
                    }
                    match components {
                        0 if child.kind() == "(" => {}
                        1 if child.is_named() => inner = Some(child),
                        2 if child.kind() == ")" => {}
                        _ => return Ok(None),
                    }
                    components = components
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                }
                let Some(inner) = inner.filter(|_| components == 3) else {
                    return Ok(None);
                };
                named = single_formal_pattern(inner, cancellation)?
                    .map(|name| (name, pattern.byte_range()));
                if named.is_none() {
                    if pending.len() >= remaining {
                        self.exhausted = true;
                        return Ok(None);
                    }
                    pending
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    pending.push(inner);
                    continue;
                }
            }
            if let Some((name, signature)) = named {
                if !formal_name_is_binding(name, source) {
                    return Ok(None);
                }
                if formals.len() >= remaining {
                    self.exhausted = true;
                    return Ok(None);
                }
                formals
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                formals.push((name, signature));
                continue;
            }
            if pattern.kind() != "repeat_pattern" {
                return Ok(None);
            }
            let mut expects_pattern = true;
            let mut elements = 0usize;
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                cancellation.check()?;
                if child.is_extra() {
                    continue;
                }
                if !valid(child) {
                    return Ok(None);
                }
                if expects_pattern && child.is_named() {
                    if pending.len() >= remaining {
                        self.exhausted = true;
                        return Ok(None);
                    }
                    pending
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    pending.push(child);
                    elements = elements
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                } else if expects_pattern || child.kind() != "," {
                    return Ok(None);
                }
                expects_pattern = !expects_pattern;
            }
            if expects_pattern || elements < 2 {
                return Ok(None);
            }
        }
        Ok((formals.len() > 1).then_some(formals))
    }

    fn parameter_head(
        &mut self,
        name: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        cancellation.check()?;
        if self.exhausted {
            return Ok(None);
        }
        if let Some(formal) = self.formal_heads.get(&name.id()) {
            return Ok(formal.as_ref().map(|formal| formal.head));
        }
        if self.reads.contains(&name.id()) {
            return Ok(None);
        }
        let mut current = name;
        while let Some(parent) = current.parent() {
            cancellation.check()?;
            if parent.kind() == "argument_patterns" {
                if let Some(owner) = parent
                    .parent()
                    .filter(|node| node.kind() == "fun_expression")
                {
                    self.lambda(owner, source, cancellation)?;
                }
                if let Some(head) = parent
                    .parent()
                    .filter(|node| node.kind() == "function_declaration_left")
                {
                    self.ensure_head(head, source, cancellation)?;
                }
                break;
            }
            if !matches!(
                parent.kind(),
                "long_identifier"
                    | "long_identifier_or_op"
                    | "identifier_pattern"
                    | "typed_pattern"
                    | "repeat_pattern"
                    | "paren_pattern"
            ) {
                break;
            }
            current = parent;
        }
        Ok(self
            .formal_heads
            .get(&name.id())
            .and_then(|formal| formal.as_ref().map(|formal| formal.head)))
    }

    fn parameter_signature(
        &mut self,
        name: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Range<usize>>, AdapterError> {
        cancellation.check()?;
        self.parameter_head(name, source, cancellation)?;
        if self.exhausted {
            return Ok(None);
        }
        let Some(formal) = self.formal_heads.get(&name.id()).and_then(Option::as_ref) else {
            return Ok(None);
        };
        if !self
            .parameters
            .get(&formal.parameters)
            .is_some_and(|parameters| parameters.complete)
        {
            return Ok(None);
        }
        if let Some(lambda) = self.lambdas.get(&formal.head).and_then(Option::as_ref) {
            return Ok((lambda.parameters == formal.parameters
                && lambda.header_bounds.start <= formal.signature.start
                && formal.signature.end <= lambda.header_bounds.end)
                .then(|| formal.signature.clone()));
        }
        let Some(binding) = self.bindings.get(&formal.head) else {
            return Ok(None);
        };
        let Some(bounds) = binding
            .bounds
            .as_ref()
            .filter(|_| binding.kind == Some("fsharp.function"))
        else {
            return Ok(None);
        };
        // This is the exact admitted binder pattern, not an inferred type or
        // a range reconstructed from adjacent source text.
        Ok((bounds.signature.start <= formal.signature.start
            && formal.signature.start < formal.signature.end
            && formal.signature.end <= bounds.signature.end)
            .then(|| formal.signature.clone()))
    }

    fn simple_parameters(
        &mut self,
        parameters: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        self.reviewed_parameters(parameters, source, cancellation)?;
        Ok(self
            .parameters
            .get(&parameters.id())
            .is_some_and(|summary| {
                summary.complete && summary.unit_slots == 0 && summary.tuple_slots == 0
            }))
    }

    fn reviewed_parameters(
        &mut self,
        parameters: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if let Some(owner) = parameters
            .parent()
            .filter(|owner| owner.kind() == "fun_expression")
        {
            self.lambda(owner, source, cancellation)?;
        }
        if !self.parameters.contains_key(&parameters.id())
            && let Some(head) = parameters
                .parent()
                .filter(|node| node.kind() == "function_declaration_left")
        {
            self.ensure_head(head, source, cancellation)?;
        }
        Ok(self
            .parameters
            .get(&parameters.id())
            .is_some_and(|summary| {
                summary.complete
                    && (summary.unit_slots == 0 && summary.tuple_slots == 0
                        || parameters.parent().is_some_and(|head| {
                            self.lambdas.get(&head.id()).is_some_and(Option::is_some)
                                || self.reviewed_callable_context(head)
                        }))
            }))
    }

    fn unit_parameters(
        &mut self,
        parameters: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if parameters.kind() != "argument_patterns" || !self.collect_body_reads {
            return Ok(false);
        }
        if !self.reviewed_parameters(parameters, source, cancellation)? {
            return Ok(false);
        }
        Ok(!self.exhausted
            && self
                .parameters
                .get(&parameters.id())
                .is_some_and(|summary| {
                    summary.complete && summary.unit_slots > 0 && summary.tuple_slots == 0
                }))
    }

    fn compound_parameters(
        &mut self,
        parameters: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if parameters.kind() != "argument_patterns" || !self.collect_body_reads {
            return Ok(false);
        }
        Ok(self.reviewed_parameters(parameters, source, cancellation)?
            && self
                .parameters
                .get(&parameters.id())
                .is_some_and(|summary| summary.tuple_slots > 0))
    }

    fn tuple_parameter(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Range<usize>>, AdapterError> {
        if node.kind() != "repeat_pattern" || !self.collect_body_reads {
            return Ok(None);
        }
        let Some(parameters) = node
            .parent()
            .filter(|parent| parent.kind() == "argument_patterns")
        else {
            return Ok(None);
        };
        if !self.compound_parameters(parameters, source, cancellation)? {
            return Ok(None);
        }
        Ok(self
            .tuple_slots
            .get(&node.id())
            .filter(|slot| slot.parameters == parameters.id())
            .map(|slot| slot.bounds.clone()))
    }

    fn unit_parameter(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "unit" || !valid(node) {
            return Ok(false);
        }
        let Some(pattern) = node.parent().filter(|parent| parent.kind() == "const") else {
            return Ok(false);
        };
        let Some(parameters) = pattern
            .parent()
            .filter(|parent| parent.kind() == "argument_patterns")
        else {
            return Ok(false);
        };
        Ok(native_unit_slot(pattern, cancellation)? == Some(node)
            && self.collect_body_reads
            && self.reviewed_parameters(parameters, source, cancellation)?)
    }

    fn simple_body_read(
        &mut self,
        name: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.collect_body_reads || self.exhausted || name.kind() != "identifier" || !valid(name)
        {
            return Ok(false);
        }
        if self.reads.contains(&name.id()) {
            return Ok(true);
        }
        if self.formal_heads.contains_key(&name.id()) {
            return Ok(false);
        }
        self.ensure_region(name, source, cancellation)?;
        Ok(!self.exhausted && self.reads.contains(&name.id()))
    }

    fn ensure_region(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut current = node;
        while let Some(parent) = current.parent() {
            cancellation.check()?;
            if !valid(parent) {
                return Ok(());
            }
            if parent.kind() == "function_or_value_defn" {
                self.group(parent, source, cancellation)?;
                if self.bodies.contains_key(&current.id()) && !self.processing_regions {
                    self.body_regions(current, source, cancellation)?;
                }
                return Ok(());
            }
            if parent.kind() == "function_declaration_left" {
                self.ensure_head(parent, source, cancellation)?;
                return Ok(());
            }
            if parent.kind() == "argument_patterns" {
                self.simple_parameters(parent, source, cancellation)?;
                return Ok(());
            }
            if parent.kind() == "fun_expression" {
                if !self.lambda(parent, source, cancellation)?
                    || !self
                        .lambdas
                        .get(&parent.id())
                        .and_then(Option::as_ref)
                        .is_some_and(|summary| summary.body == current.id())
                {
                    return Ok(());
                }
            } else if matches!(parent.kind(), "rule" | "rules" | "match_expression") {
                if !self.match_ancestor(current, parent, source, cancellation)? {
                    return Ok(());
                }
            } else if matches!(parent.kind(), "long_identifier" | "long_identifier_or_op") {
                if single_identifier_path(parent, cancellation)?.is_none()
                    && qualified_identifier_path(parent, cancellation)?.is_none()
                {
                    return Ok(());
                }
            } else if parent.kind() == "declaration_expression" {
                if unique_continuation(parent, cancellation)? != Some(current)
                    || !self.local_binding(parent, source, cancellation)?
                {
                    return Ok(());
                }
            } else if parent.kind() == "sequential_expression" {
                if !sequential_region(parent, cancellation)? {
                    return Ok(());
                }
            } else if !binder_free_expression(parent.kind()) {
                return Ok(());
            }
            current = parent;
        }
        Ok(())
    }

    fn qualified_read(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.collect_body_reads || self.exhausted || node.kind() != "long_identifier" {
            return Ok(false);
        }
        if qualified_identifier_path(node, cancellation)?.is_none() {
            return Ok(false);
        }
        self.ensure_region(node, source, cancellation)?;
        Ok(!self.exhausted && self.qualified_reads.contains(&node.id()))
    }

    fn qualified_file_context(
        &mut self,
        file: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if file.language().name() == Some("fsharp_signature") {
            return self.signature_file_context(file, source, cancellation);
        }
        if !self.collect_body_reads || self.exhausted || file.kind() != "file" || !valid(file) {
            return Ok(false);
        }
        if let Some(complete) = self.qualified_context {
            return Ok(complete && !self.exhausted);
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.qualified_context = Some(false);
        let mut cursor = file.walk();
        let mut modules = 0usize;
        loop {
            cancellation.check()?;
            if self.body_visits >= self.maximum_body_visits {
                self.exhausted = true;
                return Ok(false);
            }
            self.body_visits = self
                .body_visits
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            let node = cursor.node();
            let descend = if node.is_extra() || !node.is_named() {
                false
            } else {
                match node.kind() {
                    "file" => true,
                    "namespace" | "named_module" | "module_defn" => {
                        if !qualified_public_container(node, source, cancellation)? {
                            return Ok(false);
                        }
                        modules = modules
                            .checked_add(1)
                            .ok_or(SinkError::AccountingOverflow)?;
                        true
                    }
                    "identifier" | "long_identifier" | "access_modifier" => false,
                    "import_decl" => {
                        if !qualified_plain_open(node, cancellation)? {
                            return Ok(false);
                        }
                        false
                    }
                    "type_definition" => {
                        if !self.qualified_finite_type(node, source, cancellation)? {
                            return Ok(false);
                        }
                        false
                    }
                    "declaration_expression" => {
                        if unique_continuation(node, cancellation)?.is_some() {
                            return Ok(false);
                        }
                        let Some(group) =
                            direct_kind(node, "function_or_value_defn", cancellation)?
                        else {
                            return Ok(false);
                        };
                        self.group(group, source, cancellation)?;
                        let mut child_cursor = group.walk();
                        let recursive = self
                            .groups
                            .get(&group.id())
                            .is_some_and(|summary| summary.recursive_bounds.is_some());
                        let mut heads = 0usize;
                        for child in group.children(&mut child_cursor) {
                            cancellation.check()?;
                            if child.is_extra() {
                                continue;
                            }
                            if child.kind() == "let!" {
                                return Ok(false);
                            }
                            if matches!(
                                child.kind(),
                                "function_declaration_left" | "value_declaration_left"
                            ) {
                                if !self.bindings.get(&child.id()).is_some_and(|binding| {
                                    binding.nonrecursive || recursive && binding.recursive_eligible
                                }) || selected_name(child, cancellation)?.is_none()
                                    || !qualified_public_head(child, source, cancellation)?
                                {
                                    return Ok(false);
                                }
                                heads =
                                    heads.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                            }
                        }
                        if heads == 0 || !recursive && heads != 1 {
                            return Ok(false);
                        }
                        false
                    }
                    _ => return Ok(false),
                }
            };
            if descend && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    let complete = modules > 0 && !self.exhausted;
                    self.qualified_context = Some(complete);
                    return Ok(complete);
                }
            }
        }
    }

    fn signature_file_context(
        &mut self,
        file: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.collect_body_reads || self.exhausted || file.kind() != "file" || !valid(file) {
            return Ok(false);
        }
        if let Some(complete) = self.qualified_context {
            return Ok(complete && !self.exhausted);
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.qualified_context = Some(false);
        let mut cursor = file.walk();
        let mut modules = 0usize;
        loop {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            let node = cursor.node();
            let descend = if node.is_extra() || !node.is_named() {
                false
            } else {
                match node.kind() {
                    "file" => true,
                    "namespace" | "named_module" | "module_defn" => {
                        if !qualified_public_container(node, source, cancellation)? {
                            return Ok(false);
                        }
                        modules = modules
                            .checked_add(1)
                            .ok_or(SinkError::AccountingOverflow)?;
                        true
                    }
                    "identifier" | "long_identifier" | "access_modifier" => false,
                    "import_decl" => {
                        if !qualified_plain_open(node, cancellation)? {
                            return Ok(false);
                        }
                        false
                    }
                    "value_definition" => {
                        if !signature_plain_value(node, source, cancellation, self)? {
                            return Ok(false);
                        }
                        false
                    }
                    "type_definition" => {
                        if !self.qualified_finite_type(node, source, cancellation)? {
                            return Ok(false);
                        }
                        false
                    }
                    _ => return Ok(false),
                }
            };
            if descend && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    let complete = modules > 0 && !self.exhausted;
                    self.qualified_context = Some(complete);
                    return Ok(complete);
                }
            }
        }
    }

    fn qualified_record_visit(
        &mut self,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if self.body_visits >= self.maximum_body_visits {
            self.exhausted = true;
            return Ok(false);
        }
        self.body_visits = self
            .body_visits
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        Ok(true)
    }

    fn qualified_record_type(
        &mut self,
        definition: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.collect_body_reads
            || self.exhausted
            || definition.kind() != "type_definition"
            || !valid(definition)
            || direct_kind(definition, "record_type_defn", cancellation)?.is_none()
        {
            return Ok(false);
        }
        if let Some(&complete) = self.qualified_records.get(&definition.id()) {
            return Ok(complete);
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.qualified_records
            .try_reserve(1)
            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
        self.qualified_records.insert(definition.id(), false);
        let mut body = None;
        let mut keyword = false;
        let mut cursor = definition.walk();
        for child in definition.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.is_extra() {
                continue;
            }
            match child.kind() {
                "type" if !keyword => keyword = true,
                "record_type_defn" if body.is_none() && valid(child) => body = Some(child),
                _ => return Ok(false),
            }
        }
        let Some(body) = body.filter(|_| keyword) else {
            return Ok(false);
        };
        let Some(name) = selected_name(body, cancellation)? else {
            return Ok(false);
        };
        if single_identifier_path(name, cancellation)?.is_none() {
            return Ok(false);
        }
        let mut fields = None;
        let mut type_name = false;
        let mut equal = false;
        let mut open = false;
        let mut close = false;
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.is_extra() {
                continue;
            }
            match child.kind() {
                "type_name" if !type_name && valid(child) => type_name = true,
                "=" if !equal => equal = true,
                "{" if !open => open = true,
                "}" if !close => close = true,
                "access_modifier" if valid(child) => {}
                "record_fields" if fields.is_none() && valid(child) => fields = Some(child),
                _ => return Ok(false),
            }
        }
        let Some(fields) = fields.filter(|_| type_name && equal && open && close) else {
            return Ok(false);
        };
        let mut count = 0usize;
        let mut cursor = fields.walk();
        for field in fields.named_children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if field.is_extra() {
                continue;
            }
            if field.kind() != "record_field" || !valid(field) {
                return Ok(false);
            }
            let mut cursor = field.walk();
            for child in field.named_children(&mut cursor) {
                if !self.qualified_record_visit(cancellation)? {
                    return Ok(false);
                }
                if !child.is_extra() && child.kind() == "attributes" {
                    return Ok(false);
                }
            }
            count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
        if count == 0 {
            return Ok(false);
        }
        self.qualified_records.insert(definition.id(), true);
        Ok(true)
    }

    fn signature_record_type(
        &mut self,
        definition: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if definition.language().name() != Some("fsharp_signature")
            || !self.collect_body_reads
            || self.exhausted
            || definition.kind() != "type_definition"
            || !valid(definition)
        {
            return Ok(false);
        }
        if let Some(&complete) = self.signature_records.get(&definition.id()) {
            return Ok(complete);
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.signature_records
            .try_reserve(1)
            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
        self.signature_records.insert(definition.id(), false);
        // The implementation witness proves the finite field-only shape, not
        // public signature visibility or absence of generic head parameters.
        if !self.qualified_record_type(definition, cancellation)? {
            return Ok(false);
        }
        let Some(body) = direct_kind(definition, "record_type_defn", cancellation)? else {
            return Ok(false);
        };
        let Some(head) = direct_kind(body, "type_name", cancellation)? else {
            return Ok(false);
        };
        let Some(name) = selected_name(body, cancellation)? else {
            return Ok(false);
        };
        let mut cursor = head.walk();
        for child in head.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if !child.is_extra()
                && child.byte_range() != name.byte_range()
                && !(child.kind() == "access_modifier"
                    && source.get(child.byte_range()) == Some(b"public"))
            {
                return Ok(false);
            }
        }
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.kind() == "access_modifier"
                && source.get(child.byte_range()) != Some(b"public")
            {
                return Ok(false);
            }
        }
        let mut cursor = body.walk();
        loop {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            let node = cursor.node();
            if !valid(node)
                || matches!(
                    node.kind(),
                    "attributes" | "attribute" | "type_attributes" | "type_attribute"
                )
            {
                return Ok(false);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.signature_records.insert(definition.id(), true);
                    return Ok(true);
                }
            }
        }
    }

    fn qualified_finite_type(
        &mut self,
        definition: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let signature = definition.language().name() == Some("fsharp_signature");
        if signature {
            // Signature root masks review records and aliases separately; admitting
            // implementation constructors here would lose their signature roots.
            if direct_kind(definition, "record_type_defn", cancellation)?.is_some() {
                return self.signature_record_type(definition, source, cancellation);
            }
            if direct_kind(definition, "type_abbrev_defn", cancellation)?.is_none() {
                return Ok(false);
            }
        }
        if !signature && definition.language().name() != Some("fsharp")
            || !self.collect_body_reads
            || self.exhausted
            || definition.kind() != "type_definition"
            || !valid(definition)
        {
            return Ok(false);
        }
        if direct_kind(definition, "record_type_defn", cancellation)?.is_some() {
            return self.qualified_record_type(definition, cancellation);
        }
        if let Some(&complete) = self.qualified_records.get(&definition.id()) {
            return Ok(complete);
        }
        if !self.admit_entry()? {
            return Ok(false);
        }
        self.qualified_records
            .try_reserve(1)
            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
        self.qualified_records.insert(definition.id(), false);
        let mut keyword = false;
        let mut body = None;
        let mut cursor = definition.walk();
        for child in definition.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.is_extra() {
                continue;
            }
            match child.kind() {
                "type" if !keyword => keyword = true,
                "type_abbrev_defn" | "enum_type_defn" | "union_type_defn" | "anon_type_defn"
                    if body.is_none() && valid(child) =>
                {
                    body = Some(child)
                }
                _ => return Ok(false),
            }
        }
        let Some(body) = body.filter(|_| keyword) else {
            return Ok(false);
        };
        let Some(head) = direct_kind(body, "type_name", cancellation)? else {
            return Ok(false);
        };
        let Some(name) = selected_name(body, cancellation)? else {
            return Ok(false);
        };
        if single_identifier_path(name, cancellation)?.is_none() {
            return Ok(false);
        }
        let mut cursor = head.walk();
        for child in head.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.is_extra() {
                continue;
            }
            if child.byte_range() != name.byte_range()
                && !(child.kind() == "access_modifier"
                    && source.get(child.byte_range()) == Some(b"public"))
            {
                return Ok(false);
            }
        }
        if body.kind() == "anon_type_defn" {
            let complete = self.qualified_class_body(body, head, source, cancellation)?;
            if complete && !self.exhausted {
                self.qualified_records.insert(definition.id(), true);
            }
            return Ok(complete && !self.exhausted);
        }
        let mut type_name = false;
        let mut equal = false;
        let mut value = None;
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.is_extra() {
                continue;
            }
            match child.kind() {
                "type_name" if !type_name && child == head => type_name = true,
                "=" if !equal => equal = true,
                "access_modifier"
                    if body.kind() == "union_type_defn"
                        && source.get(child.byte_range()) == Some(b"public") => {}
                _ if child.is_named() && value.is_none() && valid(child) => value = Some(child),
                _ => return Ok(false),
            }
        }
        let Some(value) = value.filter(|_| type_name && equal) else {
            return Ok(false);
        };
        let complete = if body.kind() == "type_abbrev_defn" {
            // A bare name may be a single-case union, so it cannot prove that
            // the declaration introduces no unqualified module constructor.
            !ambiguous_type_abbreviation(body, cancellation)?
                && self.qualified_alias_rhs(value, cancellation)?
        } else if body.kind() == "union_type_defn" {
            self.qualified_union_cases(value, cancellation)?
        } else {
            self.qualified_enum_cases(value, cancellation)?
        };
        if complete && !self.exhausted {
            self.qualified_records.insert(definition.id(), true);
        }
        Ok(complete && !self.exhausted)
    }

    fn qualified_union_cases(
        &mut self,
        cases: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if cases.kind() != "union_type_cases" || !valid(cases) {
            return Ok(false);
        }
        let mut count = 0usize;
        let mut cursor = cases.walk();
        for case in cases.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if case.is_extra() || case.kind() == "|" {
                continue;
            }
            if case.kind() != "union_type_case" || !valid(case) {
                return Ok(false);
            }
            let mut name = false;
            let mut payload = false;
            let mut fields = None;
            let mut cursor = case.walk();
            for child in case.children(&mut cursor) {
                if !self.qualified_record_visit(cancellation)? {
                    return Ok(false);
                }
                if child.is_extra() {
                    continue;
                }
                match child.kind() {
                    "identifier" if !name && valid(child) => name = true,
                    "of" if !payload => payload = true,
                    "union_type_fields" if fields.is_none() && valid(child) => fields = Some(child),
                    _ => return Ok(false),
                }
            }
            if !name || payload != fields.is_some() {
                return Ok(false);
            }
            if let Some(fields) = fields {
                let mut cursor = fields.walk();
                loop {
                    if !self.qualified_record_visit(cancellation)? {
                        return Ok(false);
                    }
                    let node = cursor.node();
                    if !valid(node)
                        || matches!(
                            node.kind(),
                            "attributes" | "attribute" | "type_attributes" | "type_attribute"
                        )
                    {
                        return Ok(false);
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
                    if cursor.node() == fields {
                        break;
                    }
                }
            }
            count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
        Ok(count > 0)
    }

    fn qualified_class_body(
        &mut self,
        body: Node<'_>,
        head: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if object_type_kind(body, cancellation)? != "fsharp.class" {
            return Ok(false);
        }
        let mut type_name = false;
        let mut constructor = false;
        let mut equal = false;
        let mut begin = false;
        let mut end = false;
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if child.is_extra() {
                continue;
            }
            match child.kind() {
                "type_name" if !type_name && child == head => type_name = true,
                "primary_constr_args" if !constructor && valid(child) => constructor = true,
                "=" if !equal => equal = true,
                "class" | "begin" if !begin => begin = true,
                "end" if !end => end = true,
                "type_extension_elements" if valid(child) => {}
                _ => return Ok(false),
            }
        }
        if !type_name || !equal || begin != end {
            return Ok(false);
        }
        let mut cursor = body.walk();
        loop {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            let node = cursor.node();
            if !valid(node)
                || matches!(
                    node.kind(),
                    "attributes"
                        | "attribute"
                        | "type_attributes"
                        | "type_attribute"
                        | "preproc_if"
                        | "preproc_else"
                        | "type_argument_constraints"
                )
                || node.kind() == "access_modifier"
                    && source.get(node.byte_range()) != Some(b"public")
            {
                return Ok(false);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(true);
                }
            }
        }
    }

    fn qualified_alias_rhs(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut cursor = node.walk();
        loop {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            let current = cursor.node();
            if current.is_named()
                && !current.is_extra()
                && !matches!(
                    current.kind(),
                    "simple_type"
                        | "generic_type"
                        | "qualified_type"
                        | "paren_type"
                        | "function_type"
                        | "compound_type"
                        | "struct_type"
                        | "postfix_type"
                        | "list_type"
                        | "byref_type"
                        | "nullable_type"
                        | "type_argument"
                        | "type_attributes"
                        | "type_attribute"
                        | "long_identifier"
                        | "identifier"
                )
            {
                return Ok(false);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(true);
                }
            }
        }
    }

    fn qualified_enum_cases(
        &mut self,
        cases: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if cases.kind() != "enum_type_cases" || !valid(cases) {
            return Ok(false);
        }
        let mut count = 0usize;
        let mut cursor = cases.walk();
        for case in cases.children(&mut cursor) {
            if !self.qualified_record_visit(cancellation)? {
                return Ok(false);
            }
            if case.is_extra() || case.kind() == "|" {
                continue;
            }
            if case.kind() != "enum_type_case" || !valid(case) {
                return Ok(false);
            }
            let mut name = false;
            let mut equal = false;
            let mut literal = false;
            let mut cursor = case.walk();
            for child in case.children(&mut cursor) {
                if !self.qualified_record_visit(cancellation)? {
                    return Ok(false);
                }
                if child.is_extra() {
                    continue;
                }
                match child.kind() {
                    "identifier" if !name && valid(child) => name = true,
                    "=" if !equal => equal = true,
                    "const" if !literal && valid(child) => {
                        let mut scalar = None;
                        let mut cursor = child.walk();
                        for value in child.named_children(&mut cursor) {
                            if !self.qualified_record_visit(cancellation)? {
                                return Ok(false);
                            }
                            if !value.is_extra() && scalar.replace(value).is_some() {
                                return Ok(false);
                            }
                        }
                        if !scalar.is_some_and(|value| {
                            valid(value)
                                && matches!(
                                    value.kind(),
                                    "int"
                                        | "xint"
                                        | "sbyte"
                                        | "byte"
                                        | "int16"
                                        | "uint16"
                                        | "int32"
                                        | "uint32"
                                        | "int64"
                                        | "uint64"
                                        | "char"
                                )
                        }) {
                            return Ok(false);
                        }
                        literal = true;
                    }
                    _ => return Ok(false),
                }
            }
            if !(name && equal && literal) {
                return Ok(false);
            }
            count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
        Ok(count > 0)
    }
}

fn qualified_plain_open(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    if !valid(node) || node.kind() != "import_decl" {
        return Ok(false);
    }
    let mut keyword = false;
    let mut path = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        match child.kind() {
            "open" if !keyword => keyword = true,
            "long_identifier" if !path && valid(child) => {
                path = single_identifier_path(child, cancellation)?.is_some()
                    || qualified_identifier_path(child, cancellation)?.is_some();
                if !path {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(keyword && path)
}

fn signature_plain_value(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    if !valid(node) || selected_name(node, cancellation)?.is_none() {
        return Ok(false);
    }
    let mut keyword = false;
    let mut head = false;
    let mut colon = false;
    let mut specification = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        match child.kind() {
            "val" if !keyword => keyword = true,
            "value_declaration_left" if !head && valid(child) => {
                if !qualified_public_head(child, source, cancellation)? {
                    return Ok(false);
                }
                let mut cursor = child.walk();
                for part in child.named_children(&mut cursor) {
                    cancellation.check()?;
                    if !part.is_extra()
                        && !(part.kind() == "identifier_pattern" && valid(part))
                        && !(part.kind() == "access_modifier"
                            && source.get(part.byte_range()) == Some(b"public"))
                    {
                        return Ok(false);
                    }
                }
                head = true;
            }
            ":" if !colon => colon = true,
            "curried_spec" if !specification && valid(child) => {
                let mut cursor = child.walk();
                loop {
                    if !cache.qualified_record_visit(cancellation)? {
                        return Ok(false);
                    }
                    if matches!(
                        cursor.node().kind(),
                        "attributes" | "attribute" | "type_argument_constraints"
                    ) {
                        return Ok(false);
                    }
                    if cursor.goto_first_child() {
                        continue;
                    }
                    loop {
                        if cursor.goto_next_sibling() {
                            break;
                        }
                        if !cursor.goto_parent() {
                            specification = true;
                            break;
                        }
                    }
                    if specification {
                        break;
                    }
                }
            }
            _ => return Ok(false),
        }
    }
    // A val's arrow groups describe its type, not the implementation's head arity.
    Ok(keyword && head && colon && specification)
}

fn qualified_public_head(
    head: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = head.walk();
    for child in head.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if child.kind() == "access_modifier" && source.get(child.byte_range()) != Some(b"public") {
            return Ok(false);
        }
        if matches!(
            child.kind(),
            "attributes" | "attribute" | "inline" | "mutable"
        ) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn qualified_public_container(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if !valid(node) || selected_name(node, cancellation)?.is_none() {
        return Ok(false);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if matches!(child.kind(), "rec" | "attributes" | "begin" | "end")
            || child.kind() == "access_modifier"
                && source.get(child.byte_range()) != Some(b"public")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn match_parts<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<(Node<'tree>, Node<'tree>)>, AdapterError> {
    if node.kind() != "match_expression" || !valid(node) {
        return Ok(None);
    }
    let mut keyword = None;
    let mut scrutinee = None;
    let mut with = None;
    let mut rules = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(None);
        }
        let previous = match child.kind() {
            "match" => keyword.replace(child),
            "with" => with.replace(child),
            "rules" => rules.replace(child),
            _ if child.is_named() => scrutinee.replace(child),
            _ => return Ok(None),
        };
        if previous.is_some() {
            return Ok(None);
        }
    }
    let Some(((keyword, scrutinee), (with, rules))) = keyword.zip(scrutinee).zip(with.zip(rules))
    else {
        return Ok(None);
    };
    if keyword.start_byte() != node.start_byte()
        || keyword.end_byte() > scrutinee.start_byte()
        || scrutinee.end_byte() > with.start_byte()
        || with.end_byte() > rules.start_byte()
        || rules.end_byte() != node.end_byte()
    {
        return Ok(None);
    }
    let mut cursor = rules.walk();
    let mut count = 0usize;
    for child in rules.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) || !matches!(child.kind(), "rule" | "|") {
            return Ok(None);
        }
        if child.kind() == "rule" {
            count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
    }
    Ok((count != 0).then_some((scrutinee, rules)))
}

fn match_branch_parts<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<(Node<'tree>, Node<'tree>, Node<'tree>)>, AdapterError> {
    if node.kind() != "rule" || !valid(node) || node.child_by_field_name("guard").is_some() {
        return Ok(None);
    }
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return Ok(None);
    };
    let Some(body) = node.child_by_field_name("block") else {
        return Ok(None);
    };
    let mut arrow = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(None);
        }
        if child.kind() == "->" {
            if arrow.replace(child).is_some() {
                return Ok(None);
            }
        } else if child != pattern && child != body {
            return Ok(None);
        }
    }
    Ok(arrow.and_then(|arrow| {
        (pattern.start_byte() == node.start_byte()
            && pattern.end_byte() <= arrow.start_byte()
            && arrow.end_byte() <= body.start_byte()
            && body.end_byte() == node.end_byte())
        .then_some((pattern, arrow, body))
    }))
}

fn lambda_parts<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<(Node<'tree>, Node<'tree>, Node<'tree>)>, AdapterError> {
    let mut fun = None;
    let mut parameters = None;
    let mut arrow = None;
    let mut body = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(None);
        }
        let previous = match child.kind() {
            "fun" => fun.replace(child),
            "argument_patterns" => parameters.replace(child),
            "->" => arrow.replace(child),
            _ if child.is_named() => body.replace(child),
            _ => return Ok(None),
        };
        if previous.is_some() {
            return Ok(None);
        }
    }
    Ok(fun
        .zip(parameters)
        .zip(arrow.zip(body))
        .and_then(|((fun, parameters), (arrow, body))| {
            (fun.start_byte() == node.start_byte()
                && fun.end_byte() <= parameters.start_byte()
                && parameters.end_byte() <= arrow.start_byte()
                && arrow.end_byte() <= body.start_byte()
                && body.end_byte() == node.end_byte())
            .then_some((parameters, arrow, body))
        }))
}

fn unique_continuation<'tree>(
    wrapper: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = wrapper.walk();
    let mut continuation = None;
    for child in wrapper.children_by_field_name("in", &mut cursor) {
        cancellation.check()?;
        if child.is_extra() || !valid(child) || continuation.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(continuation)
}

fn direct_body(
    group: Node<'_>,
    body: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = group.walk();
    for child in group.children_by_field_name("body", &mut cursor) {
        cancellation.check()?;
        if child == body {
            return Ok(valid(child));
        }
    }
    Ok(false)
}

fn local_shape(
    wrapper: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<LocalSummary>, AdapterError> {
    let Some(continuation) = unique_continuation(wrapper, cancellation)? else {
        return Ok(None);
    };
    let Some(group) = direct_kind(wrapper, "function_or_value_defn", cancellation)? else {
        return Ok(None);
    };
    let mut cursor = wrapper.walk();
    for child in wrapper.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child != group && child != continuation && child.kind() != "in" {
            return Ok(None);
        }
    }
    let mut head = None;
    let mut equals = None;
    let mut body = None;
    let mut let_token = None;
    let mut cursor = group.walk();
    for child in group.children_by_field_name("body", &mut cursor) {
        cancellation.check()?;
        if child.is_extra() || !valid(child) || body.replace(child).is_some() {
            return Ok(None);
        }
    }
    let mut cursor = group.walk();
    for child in group.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child)
            || matches!(
                child.kind(),
                "rec" | "and" | "let!" | "use" | "use!" | "and!"
            )
        {
            return Ok(None);
        }
        if child.kind() == "let" && let_token.replace(child).is_some()
            || matches!(
                child.kind(),
                "function_declaration_left" | "value_declaration_left"
            ) && head.replace(child).is_some()
            || child.kind() == "=" && equals.replace(child).is_some()
        {
            return Ok(None);
        }
    }
    let Some(((head, equals), (body, let_token))) = head.zip(equals).zip(body.zip(let_token))
    else {
        return Ok(None);
    };
    let Some(name) = selected_name(head, cancellation)? else {
        return Ok(None);
    };
    if name.kind() != "identifier"
        || !formal_name_is_binding(name, source)
        || head.kind() == "function_declaration_left"
            && !local_formals(head, source, cancellation, cache)?
        || let_token.start_byte() != group.start_byte()
        || let_token.end_byte() > head.start_byte()
        || head.end_byte() > equals.start_byte()
        || equals.end_byte() > body.start_byte()
        || body.end_byte() > continuation.start_byte()
        || continuation.end_byte() != wrapper.end_byte()
    {
        return Ok(None);
    }
    Ok(Some(LocalSummary {
        group: group.id(),
        head: head.id(),
        continuation: continuation.id(),
        wrapper_bounds: wrapper.byte_range(),
        group_bounds: group.start_byte()..body.end_byte(),
        continuation_bounds: continuation.byte_range(),
        continuation_unsupported: None,
    }))
}

fn local_formals(
    head: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let Some(parameters) = direct_kind(head, "argument_patterns", cancellation)? else {
        return Ok(false);
    };
    let mut count = 0usize;
    let mut selector = FormalSelector {
        allow_tuples: true,
        ..FormalSelector::default()
    };
    let mut cursor = parameters.walk();
    for child in parameters.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(false);
        }
        count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        match selector.select(child, source, cancellation)? {
            FormalSelection::Tuple(tuple, _) => {
                if cache.tuple_formals(tuple, source, cancellation)?.is_none() {
                    return Ok(false);
                }
            }
            FormalSelection::Unsupported => return Ok(false),
            FormalSelection::Named(_, _) | FormalSelection::Unit | FormalSelection::Pending => {}
        }
    }
    Ok(count > 0 && !selector.parenthesized)
}

fn sequential_region(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    let mut local = false;
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        // A binding may occur only as the final expression: its native wrapper
        // then owns the complete continuation, never unenumerated later siblings.
        if local || !valid(child) {
            return Ok(false);
        }
        if child.kind() == "declaration_expression" {
            local = true;
        } else if !binder_free_expression(child.kind())
            && !literal_body_node(child.kind())
            && !matches!(
                child.kind(),
                "identifier" | "long_identifier" | "long_identifier_or_op"
            )
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn native_unit_slot<'tree>(
    pattern: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if pattern.kind() != "const" || !valid(pattern) {
        return Ok(None);
    }
    let mut unit = None;
    let mut cursor = pattern.walk();
    for child in pattern.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if child.kind() != "unit" || !valid(child) || unit.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(unit.filter(|unit| unit.byte_range() == pattern.byte_range()))
}

fn application_argument_bounds(
    node: Node<'_>,
    left: Node<'_>,
    argument: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    let mut cursor = node.walk();
    let mut left_seen = false;
    let mut argument_seen = false;
    let mut open = None;
    let mut close = None;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if child.id() == left.id() {
            left_seen = true;
        } else if child.id() == argument.id() {
            argument_seen = true;
        } else if child.kind() == "(" {
            if !left_seen || argument_seen || open.replace(child).is_some() || !valid(child) {
                return Ok(None);
            }
        } else if child.kind() == ")" {
            if !argument_seen || open.is_none() || close.replace(child).is_some() || !valid(child) {
                return Ok(None);
            }
        } else if open.is_some() {
            return Ok(None);
        }
    }
    Ok(match (open, close) {
        // The high-precedence application owns anonymous delimiters rather than
        // a parenthesized argument node; saturation must include that exact group.
        (Some(open), Some(close))
            if left.end_byte() <= open.start_byte()
                && open.end_byte() <= argument.start_byte()
                && argument.end_byte() <= close.start_byte()
                && close.end_byte() == node.end_byte() =>
        {
            Some(open.start_byte()..close.end_byte())
        }
        (None, None) => Some(argument.byte_range()),
        _ => None,
    })
}

fn application_operands<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<(Node<'tree>, Node<'tree>)>, AdapterError> {
    let mut cursor = node.walk();
    let mut left = None;
    let mut argument = None;
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(None);
        }
        if left.is_none() {
            left = Some(child);
        } else if argument.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(left.zip(argument))
}

fn nonrecursive_module_context(
    group: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(wrapper) = group.parent().filter(|parent| {
        parent.kind() == "declaration_expression"
            && valid(*parent)
            && parent.child_by_field_name("in").is_none()
    }) else {
        return Ok(false);
    };
    let mut owner = wrapper.parent();
    while let Some(node) = owner {
        cancellation.check()?;
        match node.kind() {
            "file" => return Ok(!node.is_missing()),
            "namespace" | "named_module" | "module_defn" => {
                if node.is_missing() {
                    return Ok(false);
                }
                let mut cursor = node.walk();
                let mut named = false;
                // A recursive header precedes the name. Never inspect the full
                // module body once its written header has been established.
                for child in node.children(&mut cursor) {
                    cancellation.check()?;
                    if child.is_extra() {
                        continue;
                    }
                    if child.kind() == "rec" {
                        return Ok(false);
                    }
                    if matches!(child.kind(), "identifier" | "long_identifier") {
                        named = valid(child);
                        break;
                    }
                }
                if !named {
                    return Ok(false);
                }
                owner = node.parent();
            }
            _ => return Ok(false),
        }
    }
    Ok(false)
}

fn binder_free_expression(kind: &str) -> bool {
    // Sequential blocks and unknown owners are barriers even when the binding
    // that shadows a formal is a sibling rather than an ancestor of the read.
    matches!(
        kind,
        "paren_expression"
            | "begin_end_expression"
            | "infix_expression"
            | "prefixed_expression"
            | "application_expression"
            | "if_expression"
            | "elif_expression"
            | "else_expression"
            | "while_expression"
            | "tuple_expression"
            | "list_expression"
            | "array_expression"
    )
}

fn literal_body_node(kind: &str) -> bool {
    matches!(
        kind,
        "const"
            | "sbyte"
            | "int16"
            | "int32"
            | "int64"
            | "byte"
            | "uint16"
            | "uint32"
            | "int"
            | "xint"
            | "nativeint"
            | "unativeint"
            | "decimal"
            | "float"
            | "uint64"
            | "ieee32"
            | "ieee64"
            | "bignum"
            | "char"
            | "bool"
            | "unit"
            | "string"
            | "verbatim_string"
            | "triple_quoted_string"
            | "bytearray"
            | "verbatim_bytearray"
    )
}

#[cfg(test)]
fn binding_bounds(
    head: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<BindingBounds>, AdapterError> {
    CaptureCache::new(usize::MAX, false, usize::MAX).bounds(head, b"", cancellation)
}

fn single_identifier_path<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let name = single_name(node, cancellation)?;
    Ok(name.filter(|name| name.kind() == "identifier" && name.byte_range() == node.byte_range()))
}

fn qualified_identifier_path<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if node.kind() == "long_identifier_or_op" {
        let Some(inner) = direct_kind(node, "long_identifier", cancellation)? else {
            return Ok(None);
        };
        if inner.byte_range() != node.byte_range() {
            return Ok(None);
        }
        node = inner;
    }
    if node.kind() != "long_identifier" || !valid(node) {
        return Ok(None);
    }
    let mut cursor = node.walk();
    let mut names = 0usize;
    let mut expects_name = true;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(None);
        }
        match child.kind() {
            "identifier" if expects_name => {
                names = names.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                if names > rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
                    return Ok(None);
                }
                expects_name = false;
            }
            "." if !expects_name => expects_name = true,
            _ => return Ok(None),
        }
    }
    Ok((names > 1 && !expects_name).then_some(node))
}

fn single_formal_pattern<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    if !valid(node) {
        return Ok(None);
    }
    if matches!(
        node.kind(),
        "long_identifier" | "long_identifier_or_op" | "identifier_pattern"
    ) {
        return single_identifier_path(node, cancellation);
    }
    if node.kind() != "typed_pattern" {
        return Ok(None);
    }
    let mut pattern = None;
    let mut components = 0usize;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if !valid(child) {
            return Ok(None);
        }
        match components {
            0 if matches!(child.kind(), "identifier_pattern" | "long_identifier") => {
                pattern = Some(child);
            }
            1 if child.kind() == ":" => {}
            2 if child.is_named() => {}
            _ => return Ok(None),
        }
        components = components
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
    }
    if components != 3 {
        return Ok(None);
    }
    match pattern {
        Some(pattern) => single_identifier_path(pattern, cancellation),
        None => Ok(None),
    }
}

fn formal_name_is_binding(node: Node<'_>, source: &[u8]) -> bool {
    let Some(bytes) = source.get(node.byte_range()) else {
        return false;
    };
    let Ok(name) = std::str::from_utf8(bytes) else {
        return false;
    };
    let name = name
        .strip_prefix("``")
        .and_then(|name| name.strip_suffix("``"))
        .unwrap_or(name);
    // Capitalized pattern names may denote constructors after compiler lookup.
    // This native-only slice admits unambiguous lower-case or underscore names.
    name != "_"
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_lowercase())
}

fn written_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    Ok(Some(match node.kind() {
        "namespace" => "fsharp.namespace",
        "named_module" => "fsharp.module",
        "module_defn" => "fsharp.nested_module",
        "record_type_defn" => "fsharp.record",
        "union_type_defn" => "fsharp.union",
        "type_abbrev_defn" if ambiguous_type_abbreviation(node, cancellation)? => {
            "fsharp.ambiguous_type"
        }
        "type_abbrev_defn" => "fsharp.type_alias",
        "enum_type_defn" => "fsharp.enum",
        "interface_type_defn" => "fsharp.interface",
        "anon_type_defn" => return object_type_kind(node, cancellation).map(Some),
        "record_field"
            if node
                .parent()
                .and_then(|fields| fields.parent())
                .is_some_and(|owner| owner.kind() == "record_type_defn") =>
        {
            "fsharp.record_field"
        }
        "union_type_case" => "fsharp.union_case",
        "union_type_field" => "fsharp.union_field",
        "enum_type_case" => "fsharp.enum_constant",
        "method_or_prop_defn" if node.child_by_field_name("args").is_some() => "fsharp.method",
        "method_or_prop_defn" => "fsharp.property",
        "additional_constr_defn" => "fsharp.constructor",
        "value_definition" => "fsharp.signature_val",
        "member_signature"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "member_defn") =>
        {
            return signature_member_kind(node, cancellation).map(Some);
        }
        "constructor_signature" => "fsharp.signature_new",
        "member_defn" if direct_kind(node, "property_or_ident", cancellation)?.is_some() => {
            "fsharp.property"
        }
        "member_defn" if direct_kind(node, "val", cancellation)?.is_some() => {
            "fsharp.signature_field"
        }
        _ => return Ok(None),
    }))
}

fn unsupported_declaration(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    Ok(match node.kind() {
        "module_abbrev" => Some("fsharp.unsupported_module_alias"),
        "exception_definition" => Some("fsharp.unsupported_exception"),
        "type_extension" => Some("fsharp.unsupported_type_extension"),
        "delegate_type_defn" => Some("fsharp.unsupported_delegate"),
        "type_declaration" => Some("fsharp.unsupported_abstract_type"),
        "argument_patterns" if !cache.reviewed_parameters(node, source, cancellation)? => {
            Some("fsharp.unsupported_parameters")
        }
        "argument_name_spec" => Some("fsharp.unsupported_parameters"),
        "union_type_field" if selected_name(node, cancellation)?.is_none() => {
            Some("fsharp.unsupported_union_field")
        }
        "object_expression" => Some("fsharp.unsupported_anonymous_object"),
        "function_declaration_left" | "value_declaration_left" | "value_definition"
            if cache.written_kind(node, source, cancellation)?.is_none()
                || selected_name(node, cancellation)?.is_none() =>
        {
            Some("fsharp.unsupported_binding")
        }
        "record_field" if written_kind(node, cancellation)?.is_none() => {
            Some("fsharp.unsupported_anonymous_record_field")
        }
        _ => None,
    })
}

/// Retains only captures with reviewed source names and unambiguous ownership.
///
/// Cancellation is propagated. Unsupported declarations, bodies and attributes
/// retain gap markers. Reads need a separate lexical proof before resolution.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if matches!(role, StructuralRole::Root | StructuralRole::Module) {
        return Ok(node.kind() == "file");
    }
    if role == StructuralRole::Scope && node.kind() == "rule" {
        return Ok(!node.is_missing() && node.start_byte() < node.end_byte());
    }
    if role == StructuralRole::Scope
        && matches!(
            node.kind(),
            "file" | "namespace" | "named_module" | "module_defn" | "preproc_if" | "preproc_else"
        )
    {
        return Ok(!node.is_missing());
    }
    if !valid(node) {
        return Ok(false);
    }
    if role == StructuralRole::Expression && node.kind() == "file" {
        return cache.qualified_file_context(node, source, cancellation);
    }
    if role == StructuralRole::Expression && node.kind() == "type_definition" {
        return cache.qualified_finite_type(node, source, cancellation);
    }
    if role == StructuralRole::Reference && node.kind() == "long_identifier" {
        return cache.qualified_read(node, source, cancellation);
    }
    if role == StructuralRole::Scope && node.kind() == "declaration_expression" {
        return cache.local_binding(node, source, cancellation);
    }
    if role == StructuralRole::Scope && node.kind() == "fun_expression" {
        return cache.lambda(node, source, cancellation);
    }
    if role == StructuralRole::Call {
        return cache.application_call(node, source, cancellation);
    }
    if role == StructuralRole::Expression
        && (cache
            .match_branch_capture(node, source, cancellation)?
            .is_some()
            || cache.lambda_capture(node, source, cancellation)?.is_some()
            || cache.local_capture(node, source, cancellation)?.is_some()
            || cache
                .recursive_capture(node, source, cancellation)?
                .is_some()
            || cache.unit_parameters(node, source, cancellation)?
            || cache.unit_parameter(node, source, cancellation)?
            || cache.compound_parameters(node, source, cancellation)?
            || cache.tuple_parameter(node, source, cancellation)?.is_some()
            || cache.nonrecursive_binding(node, source, cancellation)?
            || cache.application_argument(node, source, cancellation)?)
    {
        return Ok(true);
    }
    if role == StructuralRole::Expression && node.kind() == "rule" {
        return Ok(true);
    }
    if role == StructuralRole::Expression && node.kind() == "attribute" {
        return Ok(true);
    }
    if role == StructuralRole::Expression && cache.unsupported_body(node, source, cancellation)? {
        return Ok(true);
    }
    if node.kind() == "identifier" {
        return match role {
            StructuralRole::Declaration | StructuralRole::Definition => {
                Ok(cache.match_binder(node, source, cancellation)?
                    || cache.parameter_head(node, source, cancellation)?.is_some())
            }
            StructuralRole::Expression => Ok(cache
                .parameter_signature(node, source, cancellation)?
                .is_some()),
            StructuralRole::Reference => cache.simple_body_read(node, source, cancellation),
            _ => Ok(false),
        };
    }
    if node.kind() == "argument_patterns" && role == StructuralRole::Signature {
        return cache.simple_parameters(node, source, cancellation);
    }
    if role == StructuralRole::Declaration
        && unsupported_declaration(node, source, cancellation, cache)?.is_some()
    {
        return Ok(true);
    }
    if role == StructuralRole::Import {
        return Ok(node.kind() == "import_decl"
            && direct_kind(node, "long_identifier", cancellation)?.is_some());
    }
    if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        return Ok(cache.written_kind(node, source, cancellation)?.is_some()
            && selected_name(node, cancellation)?.is_some());
    }
    Ok(false)
}

/// Returns the syntax label before the common role suffix is attached.
///
/// All labels describe written syntax; cancellation is propagated and unknown
/// shapes return None. Import labels record `open type` without resolving it.
pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    if role == StructuralRole::Expression && node.kind() == "file" {
        return Ok(cache
            .qualified_file_context(node, source, cancellation)?
            .then_some(if node.language().name() == Some("fsharp_signature") {
                "fsharp.signature_context"
            } else {
                "fsharp.qualified_context"
            }));
    }
    if role == StructuralRole::Expression && node.kind() == "type_definition" {
        return Ok(cache
            .qualified_finite_type(node, source, cancellation)?
            .then_some(if node.language().name() == Some("fsharp_signature") {
                if direct_kind(node, "record_type_defn", cancellation)?.is_some() {
                    "fsharp.signature_record_type"
                } else {
                    "fsharp.signature_alias_type"
                }
            } else if direct_kind(node, "record_type_defn", cancellation)?.is_some() {
                "fsharp.qualified_record_type"
            } else if direct_kind(node, "enum_type_defn", cancellation)?.is_some() {
                "fsharp.qualified_enum_type"
            } else if direct_kind(node, "union_type_defn", cancellation)?.is_some() {
                "fsharp.qualified_union_type"
            } else if direct_kind(node, "anon_type_defn", cancellation)?.is_some() {
                "fsharp.qualified_class_type"
            } else {
                "fsharp.qualified_alias_type"
            }));
    }
    if role == StructuralRole::Reference && node.kind() == "long_identifier" {
        return Ok(cache
            .qualified_read(node, source, cancellation)?
            .then_some("fsharp.qualified_body_read"));
    }
    if role == StructuralRole::Scope {
        match node.kind() {
            "preproc_if" => return Ok(Some("fsharp.preprocessor")),
            "preproc_else" => return Ok(Some("fsharp.preprocessor_branch")),
            "rule" => return Ok(Some("fsharp.match_branch")),
            _ => {}
        }
    }
    if role == StructuralRole::Scope && node.kind() == "fun_expression" {
        return Ok(cache
            .lambda(node, source, cancellation)?
            .then_some("fsharp.lambda"));
    }
    if role == StructuralRole::Scope && node.kind() == "declaration_expression" {
        return Ok(cache
            .local_binding(node, source, cancellation)?
            .then_some("fsharp.local_binding"));
    }
    if role == StructuralRole::Call {
        return Ok(cache
            .application_call(node, source, cancellation)?
            .then_some("fsharp.application"));
    }
    if role == StructuralRole::Expression {
        if let Some((syntax, _)) = cache.match_branch_capture(node, source, cancellation)? {
            return Ok(Some(syntax));
        }
        if let Some((syntax, _)) = cache.lambda_capture(node, source, cancellation)? {
            return Ok(Some(syntax));
        }
        if let Some((syntax, _)) = cache.local_capture(node, source, cancellation)? {
            return Ok(Some(syntax));
        }
        if let Some((syntax, _)) = cache.recursive_capture(node, source, cancellation)? {
            return Ok(Some(syntax));
        }
        if cache.unit_parameters(node, source, cancellation)? {
            return Ok(Some("fsharp.unit_parameters"));
        }
        if cache.unit_parameter(node, source, cancellation)? {
            return Ok(Some("fsharp.unit_parameter"));
        }
        if cache.compound_parameters(node, source, cancellation)? {
            return Ok(Some("fsharp.compound_parameters"));
        }
        if cache.tuple_parameter(node, source, cancellation)?.is_some() {
            return Ok(Some("fsharp.tuple_parameter"));
        }
        if cache.nonrecursive_binding(node, source, cancellation)? {
            return Ok(Some("fsharp.nonrecursive_binding"));
        }
        if cache.application_argument(node, source, cancellation)? {
            return Ok(Some("fsharp.application_argument"));
        }
        if node.kind() == "rule" {
            return Ok(Some("fsharp.unsupported_body"));
        }
    }
    if role == StructuralRole::Expression && node.kind() == "attribute" {
        return Ok(valid(node).then_some("fsharp.unsupported_attribute"));
    }
    if role == StructuralRole::Expression && cache.unsupported_body(node, source, cancellation)? {
        return Ok(Some("fsharp.unsupported_body"));
    }
    if node.kind() == "file" {
        return Ok(Some("fsharp.file"));
    }
    if node.kind() == "identifier" {
        return Ok(Some(match role {
            StructuralRole::Declaration => {
                if cache.match_binder(node, source, cancellation)? {
                    "fsharp.variable"
                } else {
                    "fsharp.parameter"
                }
            }
            StructuralRole::Definition => "fsharp.name",
            StructuralRole::Expression => "fsharp.parameter_header",
            StructuralRole::Reference => "fsharp.simple_function_body_read",
            _ => return Ok(None),
        }));
    }
    if node.kind() == "argument_patterns" && role == StructuralRole::Signature {
        return Ok(Some("fsharp.simple_parameters"));
    }
    if role == StructuralRole::Declaration
        && let Some(unsupported) = unsupported_declaration(node, source, cancellation, cache)?
    {
        return Ok(Some(unsupported));
    }
    if role == StructuralRole::Import {
        return Ok(Some(
            if direct_kind(node, "type", cancellation)?.is_some() {
                "fsharp.open_type"
            } else {
                "fsharp.open"
            },
        ));
    }
    if role == StructuralRole::Definition {
        return Ok(
            selected_name(node, cancellation)?.map(|name| match name.kind() {
                "op_identifier" => "fsharp.operator_name",
                "new" => "fsharp.new_name",
                _ => "fsharp.name",
            }),
        );
    }
    cache.written_kind(node, source, cancellation)
}

/// Returns exact role-specific byte bounds with native binding ownership.
///
/// Unsupported head/body pairs return None. Declaration and scope spans for a
/// recursive head cover only that head and its own body; signatures stop at `=`.
/// Optional recursive witnesses partition the group through its last own body,
/// excluding unowned trailing native extras without parsing source text.
pub(super) fn capture_range(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    cancellation.check()?;
    if role == StructuralRole::Expression && node.kind() == "file" {
        return Ok(cache
            .qualified_file_context(node, source, cancellation)?
            .then_some(0..source.len()));
    }
    if role == StructuralRole::Expression && node.kind() == "type_definition" {
        return Ok(cache
            .qualified_finite_type(node, source, cancellation)?
            .then(|| node.byte_range()));
    }
    if role == StructuralRole::Scope && node.kind() == "fun_expression" {
        return Ok(cache
            .lambda(node, source, cancellation)?
            .then(|| node.byte_range()));
    }
    if role == StructuralRole::Expression
        && let Some((_, bounds)) = cache.match_branch_capture(node, source, cancellation)?
    {
        return Ok(Some(bounds));
    }
    if role == StructuralRole::Expression
        && let Some((_, bounds)) = cache.lambda_capture(node, source, cancellation)?
    {
        return Ok(Some(bounds));
    }
    if role == StructuralRole::Scope && node.kind() == "declaration_expression" {
        return Ok(cache
            .local_binding(node, source, cancellation)?
            .then(|| node.byte_range()));
    }
    if role == StructuralRole::Expression
        && let Some((_, bounds)) = cache.local_capture(node, source, cancellation)?
    {
        return Ok(Some(bounds));
    }
    if role == StructuralRole::Expression
        && let Some((_, bounds)) = cache.recursive_capture(node, source, cancellation)?
    {
        return Ok(Some(bounds));
    }
    if role == StructuralRole::Expression
        && let Some(bounds) = cache.tuple_parameter(node, source, cancellation)?
    {
        return Ok(Some(bounds));
    }
    if role == StructuralRole::Expression
        && cache.application_argument(node, source, cancellation)?
    {
        return Ok(node
            .parent()
            .and_then(|parent| cache.applications.get(&parent.id()))
            .map(|summary| summary.argument_bounds.clone()));
    }
    if role == StructuralRole::Expression && node.kind() == "attribute" {
        return Ok(valid(node).then(|| node.byte_range()));
    }
    if role == StructuralRole::Expression && cache.unsupported_body(node, source, cancellation)? {
        return Ok(Some(node.byte_range()));
    }
    if node.kind() == "identifier" {
        return if role == StructuralRole::Expression {
            cache.parameter_signature(node, source, cancellation)
        } else {
            Ok(Some(node.byte_range()))
        };
    }
    if node.kind() == "argument_patterns" && role == StructuralRole::Signature {
        return Ok(Some(node.byte_range()));
    }
    if role == StructuralRole::Definition {
        return Ok(selected_name(node, cancellation)?.map(|name| name.byte_range()));
    }
    if role == StructuralRole::Declaration
        && unsupported_declaration(node, source, cancellation, cache)?.is_some()
    {
        if matches!(
            node.kind(),
            "function_declaration_left" | "value_declaration_left"
        ) && let Some(bounds) = cache.bounds(node, source, cancellation)?
        {
            return Ok(Some(bounds.declaration));
        }
        return Ok(Some(node.byte_range()));
    }
    if matches!(
        node.kind(),
        "function_declaration_left" | "value_declaration_left"
    ) {
        return Ok(cache.bounds(node, source, cancellation)?.map(|bounds| {
            if role == StructuralRole::Signature {
                bounds.signature
            } else {
                bounds.declaration
            }
        }));
    }
    if role != StructuralRole::Signature {
        return Ok(Some(node.byte_range()));
    }
    if matches!(
        node.kind(),
        "type_abbrev_defn" | "member_signature" | "constructor_signature"
    ) {
        return Ok(Some(node.byte_range()));
    }
    if matches!(node.kind(), "namespace" | "named_module") {
        return Ok(
            selected_name(node, cancellation)?.map(|name| node.start_byte()..name.end_byte())
        );
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && matches!(child.kind(), "=" | "with") {
            return Ok((node.start_byte() < child.start_byte())
                .then_some(node.start_byte()..child.start_byte()));
        }
    }
    Ok(Some(node.byte_range()))
}

/// Identifies same-node lexical environments that need depth below a declaration.
///
/// Query integration must retain native depth for F# and add one only for these
/// scope captures, so declarations are not owned by their own same-span scope.
pub(super) fn scope_below_declaration(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "namespace"
            | "named_module"
            | "module_defn"
            | "function_declaration_left"
            | "value_declaration_left"
            | "record_type_defn"
            | "union_type_defn"
            | "enum_type_defn"
            | "anon_type_defn"
            | "interface_type_defn"
            | "method_or_prop_defn"
            | "additional_constr_defn"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &str, signature: bool) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = if signature {
            tree_sitter_fsharp::LANGUAGE_SIGNATURE
        } else {
            tree_sitter_fsharp::LANGUAGE_FSHARP
        };
        parser.set_language(&language.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source:?}: {}",
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

    fn binding_bodies<'tree>(root: Node<'tree>) -> Vec<Node<'tree>> {
        let mut output = Vec::new();
        for group in nodes(root, "function_or_value_defn") {
            let mut cursor = group.walk();
            if !cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.field_name() == Some("body") {
                    output.push(cursor.node());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        output
    }

    #[test]
    fn qualified_body_paths_keep_whole_reads_and_complete_application_slots() {
        for source in [
            "module Consumer\nlet invoke input = Library.carry input\n",
            "module Consumer\nlet invoke input = Library.carry input input\n",
            "module Consumer\nlet invoke input = Library.``carry.part`` input\n",
            "module Consumer\nlet rec invoke input = Library.carry input\nand neighbor input = invoke input\n",
        ] {
            let tree = parse(source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(1024, true, 1024);
            let path = nodes(tree.root_node(), "long_identifier")
                .into_iter()
                .find(|node| {
                    source
                        .get(node.byte_range())
                        .is_some_and(|text| text.starts_with("Library."))
                })
                .unwrap();
            assert!(
                cache
                    .qualified_read(path, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(
                cache
                    .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                    .unwrap()
            );
            let visits = cache.body_visits;
            for _ in 0..4 {
                assert!(
                    cache
                        .qualified_read(path, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(
                    cache
                        .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(cache.body_visits, visits);
            }
            assert_eq!(
                syntax(
                    path,
                    StructuralRole::Reference,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some("fsharp.qualified_body_read")
            );
            let calls = nodes(tree.root_node(), "application_expression")
                .into_iter()
                .filter(|node| {
                    source
                        .get(node.byte_range())
                        .is_some_and(|text| text.starts_with("Library."))
                })
                .filter(|node| {
                    cache
                        .application_call(*node, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 1);
            assert_eq!(cache.applications[&calls[0].id()].callee, Some(path.id()));
        }
    }

    #[test]
    fn qualified_file_contexts_reject_unreviewed_owners_and_preserve_bounds() {
        for source in [
            "let invoke input = Library.carry input\n",
            "module Consumer\nopen type Library\nlet invoke input = Library.carry input\n",
            "module private Consumer\nlet invoke input = Library.carry input\n",
            "module Consumer\nlet private invoke input = Library.carry input\n",
            "module Consumer\ntype Pair<'T> = One | Two of 'T\nlet invoke input = Library.carry input\n",
            "module Consumer\nmodule Other = Library\nlet invoke input = Library.carry input\n",
        ] {
            let tree = parse(source, false);
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                !cache
                    .qualified_file_context(
                        tree.root_node(),
                        source.as_bytes(),
                        &Cancellation::new()
                    )
                    .unwrap(),
                "{source}"
            );
        }
        let source = "module Consumer\nlet invoke input = Library.carry input\n";
        let tree = parse(source, false);
        let mut cache = CaptureCache::new(1, true, 1024);
        assert!(
            !cache
                .qualified_file_context(tree.root_node(), source.as_bytes(), &Cancellation::new())
                .unwrap()
        );
        assert!(cache.exhausted());
        let mut cache = CaptureCache::new(1024, true, 1);
        assert!(
            !cache
                .qualified_file_context(tree.root_node(), source.as_bytes(), &Cancellation::new())
                .unwrap()
        );
        assert!(cache.exhausted());
        let mut cache = CaptureCache::new(1024, true, 1024);
        assert!(
            cache
                .qualified_file_context(tree.root_node(), source.as_bytes(), &Cancellation::new())
                .unwrap()
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            cache.qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn qualified_plain_opens_keep_owned_containers_and_cached_work() {
        for newline in ["\n", "\r\n"] {
            for source in [
                "module Consumer\nopen Imports\nlet invoke input = Library.carry input\n",
                "module Consumer\nlet invoke input = Library.carry input\nopen Imports\n",
                "module Consumer\nopen Imports.Names\nmodule Uses =\n    let invoke input = Library.carry input\n",
                "module Consumer\nmodule Other =\n    open Imports\n    let seed = 0\nmodule Uses =\n    let invoke input = Library.carry input\n",
                "module Empty\n",
                "module Records\nmodule Child =\n    type Receiver = { Value: int }\n",
            ] {
                let source = source.replace('\n', newline);
                let tree = parse(&source, false);
                let cancellation = Cancellation::new();
                let mut cache = CaptureCache::new(1024, true, 1024);
                assert!(
                    cache
                        .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                        .unwrap(),
                    "{source}"
                );
                let visits = cache.body_visits;
                for _ in 0..3 {
                    assert!(
                        cache
                            .qualified_file_context(
                                tree.root_node(),
                                source.as_bytes(),
                                &cancellation
                            )
                            .unwrap()
                    );
                    assert_eq!(cache.body_visits, visits);
                }
                let cancellation = Cancellation::new();
                cancellation.cancel(CancellationReason::ClientRequest);
                assert!(matches!(
                    cache.qualified_file_context(
                        tree.root_node(),
                        source.as_bytes(),
                        &cancellation
                    ),
                    Err(AdapterError::Cancelled { .. })
                ));
            }
        }
    }

    #[test]
    fn qualified_records_keep_single_owned_names_without_module_values() {
        for newline in ["\n", "\r\n"] {
            for source in [
                "module Consumer\ntype Pair = { Value: int }\nlet invoke input = Library.carry input\n",
                "module Consumer\ntype Pair<'a> = { mutable Value: 'a }\nlet invoke input = Library.carry input\n",
                "module Consumer\ntype (* own head *) Pair = { Library: int; carry: int }\nlet invoke input = Library.carry input\n",
                "module Records\ntype Pair = { Value: int }\n",
                "module Library\nlet offset = 7\ntype Pair = { Value: int }\nlet carry value = value + offset\n",
            ] {
                let source = source.replace('\n', newline);
                let tree = parse(&source, false);
                let cancellation = Cancellation::new();
                let definition = nodes(tree.root_node(), "type_definition")[0];
                let mut cache = CaptureCache::new(1024, true, 1024);
                assert!(
                    cache
                        .qualified_record_type(definition, &cancellation)
                        .unwrap(),
                    "{source}"
                );
                assert!(
                    cache
                        .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                        .unwrap(),
                    "{source}"
                );
                let visits = cache.body_visits;
                for _ in 0..3 {
                    assert!(
                        cache
                            .qualified_record_type(definition, &cancellation)
                            .unwrap()
                    );
                    assert!(
                        cache
                            .qualified_file_context(
                                tree.root_node(),
                                source.as_bytes(),
                                &cancellation
                            )
                            .unwrap()
                    );
                    assert_eq!(cache.body_visits, visits);
                }
                assert_eq!(
                    capture_range(
                        definition,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache
                    )
                    .unwrap(),
                    Some(definition.byte_range())
                );
                let cancellation = Cancellation::new();
                cancellation.cancel(CancellationReason::ClientRequest);
                assert!(matches!(
                    cache.qualified_record_type(definition, &cancellation),
                    Err(AdapterError::Cancelled { .. })
                ));
            }
        }
        for source in [
            "module Consumer\n[<Marker>] type Pair = { Value: int }\nlet invoke input = Library.carry input\n",
            "module Consumer\ntype Pair = { [<Marker>] Value: int }\nlet invoke input = Library.carry input\n",
            "module Consumer\ntype Pair = { Value: int }\nand Other = { Value: int }\nlet invoke input = Library.carry input\n",
            "module Consumer\ntype Pair = { Value: int } with\n    member this.Read() = this.Value\nlet invoke input = Library.carry input\n",
        ] {
            let tree = parse(source, false);
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                !cache
                    .qualified_record_type(
                        nodes(tree.root_node(), "type_definition")[0],
                        &Cancellation::new()
                    )
                    .unwrap(),
                "{source}"
            );
            assert!(
                !cache
                    .qualified_file_context(
                        tree.root_node(),
                        source.as_bytes(),
                        &Cancellation::new()
                    )
                    .unwrap(),
                "{source}"
            );
        }
        let source = "module Records\ntype Pair = { Value: int }\n";
        let tree = parse(source, false);
        let definition = nodes(tree.root_node(), "type_definition")[0];
        for (entries, visits) in [(0, 1024), (1024, 1)] {
            let mut cache = CaptureCache::new(entries, true, visits);
            assert!(
                !cache
                    .qualified_record_type(definition, &Cancellation::new())
                    .unwrap()
            );
            assert!(cache.exhausted());
        }
    }

    #[test]
    fn signature_records_require_public_plain_heads_and_bounded_native_work() {
        for source in [
            "module Contracts\ntype Pair = { Value: int }\nval offset: int\n",
            "module Contracts\ntype public (* head *) Pair (* tail *) = { mutable Value: int; Callback: int -> int }\nval offset: int\n",
            "namespace Contracts\nmodule Library =\n    type ``Item.part`` = { Value: string }\n    val offset: int\n",
        ] {
            let tree = parse(source, true);
            let definition = nodes(tree.root_node(), "type_definition")[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                cache
                    .signature_record_type(definition, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(
                cache
                    .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                    .unwrap()
            );
            let visits = cache.body_visits;
            for _ in 0..3 {
                assert!(
                    cache
                        .signature_record_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(cache.body_visits, visits);
            }
            assert_eq!(
                syntax(
                    definition,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some("fsharp.signature_record_type")
            );
            assert_eq!(
                capture_range(
                    definition,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some(definition.byte_range())
            );
            for (entries, work) in [(0, 1024), (1024, 1)] {
                let mut bounded = CaptureCache::new(entries, true, work);
                assert!(
                    !bounded
                        .signature_record_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(bounded.exhausted());
            }
            let mut mandatory = CaptureCache::new(1024, false, 1024);
            assert!(
                !mandatory
                    .signature_record_type(definition, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            let cancelled = Cancellation::new();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                cache.signature_record_type(definition, source.as_bytes(), &cancelled),
                Err(AdapterError::Cancelled { .. })
            ));
        }
        for source in [
            "module Contracts\ntype Pair<'T> = { Value: 'T }\nval offset: int\n",
            "module Contracts\ntype private Pair = { Value: int }\nval offset: int\n",
            "module Contracts\ntype Pair = private { Value: int }\nval offset: int\n",
            "module Contracts\n[<Marker>] type Pair = { Value: int }\nval offset: int\n",
            "module Contracts\ntype Pair = { [<Marker>] Value: int }\nval offset: int\n",
            "module Contracts\ntype Pair = int\nval offset: int\n",
            "module Contracts\ntype Pair = First | Next\nval offset: int\n",
            "module Contracts\ntype Pair = { Value: int }\nand Other = { Value: int }\nval offset: int\n",
        ] {
            let tree = parse(source, true);
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                !cache
                    .signature_record_type(
                        nodes(tree.root_node(), "type_definition")[0],
                        source.as_bytes(),
                        &Cancellation::new()
                    )
                    .unwrap(),
                "{source}"
            );
            assert!(
                !cache
                    .qualified_file_context(
                        tree.root_node(),
                        source.as_bytes(),
                        &Cancellation::new()
                    )
                    .unwrap(),
                "{source}"
            );
        }
    }

    #[test]
    fn signature_aliases_require_public_plain_heads_and_bounded_native_work() {
        for source in [
            "module Contracts\ntype Alias = System.Int32\nval offset: int\n",
            "module Contracts\ntype public (* head *) Alias (* tail *) = System.Int32\nval offset: int\n",
            "namespace Contracts\nmodule Library =\n    type ``Item.part`` = int * string\n    val offset: int\n",
            "module Contracts\ntype Callback = int -> int\nval offset: int\n",
        ] {
            let tree = parse(source, true);
            let definition = nodes(tree.root_node(), "type_definition")[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                cache
                    .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                    .unwrap(),
                "{source}"
            );
            assert!(
                cache
                    .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                    .unwrap(),
                "{source}"
            );
            let visits = cache.body_visits;
            for _ in 0..3 {
                assert!(
                    cache
                        .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(cache.body_visits, visits);
            }
            assert_eq!(
                syntax(
                    definition,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                Some("fsharp.signature_alias_type")
            );
            assert_eq!(
                capture_range(
                    definition,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                Some(definition.byte_range())
            );
            for (entries, work) in [(0, 1024), (1024, 1)] {
                let mut bounded = CaptureCache::new(entries, true, work);
                assert!(
                    !bounded
                        .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(bounded.exhausted());
            }
            let mut mandatory = CaptureCache::new(1024, false, 1024);
            assert!(
                !mandatory
                    .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            let cancelled = Cancellation::new();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                cache.qualified_finite_type(definition, source.as_bytes(), &cancelled),
                Err(AdapterError::Cancelled { .. })
            ));
        }
        for declaration in [
            "type Alias = int",
            "type Alias<'T> = System.Int32",
            "type private Alias = System.Int32",
            "type internal Alias = System.Int32",
            "[<Marker>] type Alias = System.Int32",
            "type Alias = System.Int32\nand Other = System.Int64",
            "type Choice = First | Next",
            "type Choice = First = 1 | Next = 2",
            "type Object = class end",
        ] {
            let source = format!("module Contracts\n{declaration}\nval offset: int\n");
            let tree = parse(&source, true);
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                !cache
                    .qualified_finite_type(
                        nodes(tree.root_node(), "type_definition")[0],
                        source.as_bytes(),
                        &Cancellation::new(),
                    )
                    .unwrap(),
                "{source}"
            );
            assert!(
                !cache
                    .qualified_file_context(
                        tree.root_node(),
                        source.as_bytes(),
                        &Cancellation::new(),
                    )
                    .unwrap(),
                "{source}"
            );
        }
        let source = "module Contracts\ntype Alias = private System.Int32\nval offset: int\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_fsharp::LANGUAGE_SIGNATURE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(tree.root_node().has_error());
        let mut cache = CaptureCache::new(1024, true, 1024);
        assert!(
            !cache
                .qualified_file_context(tree.root_node(), source.as_bytes(), &Cancellation::new())
                .unwrap()
        );
    }

    #[test]
    fn qualified_union_and_class_native_proofs_are_cached_bounded_and_cancellable() {
        for source in [
            "module Consumer\ntype Choice = Empty | Named of value: int\nlet invoke input = Library.carry input\n",
            "module Consumer\ntype Choice = Empty | Named of int\nlet invoke input = Library.carry input\n",
            "module Consumer\ntype Holder(seed: int) =\n    member this.Read() = seed\nlet invoke input = Library.carry input\n",
            "module Consumer\ntype Object = class end\nlet invoke input = Library.carry input\n",
        ] {
            let tree = parse(source, false);
            let definition = nodes(tree.root_node(), "type_definition")[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(1024, true, 2048);
            assert!(
                cache
                    .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(
                cache
                    .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                    .unwrap()
            );
            let visits = cache.body_visits;
            for _ in 0..3 {
                assert!(
                    cache
                        .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(cache.body_visits, visits);
            }
            for (entries, work) in [(0, 2048), (1024, 1)] {
                let mut bounded = CaptureCache::new(entries, true, work);
                assert!(
                    !bounded
                        .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(bounded.exhausted());
            }
            let cancelled = Cancellation::new();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                cache.qualified_finite_type(definition, source.as_bytes(), &cancelled),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }

    #[test]
    fn qualified_alias_and_enum_native_work_is_bounded_cached_and_cancellable() {
        for (declaration, label) in [
            ("type Alias = System.Int32", "fsharp.qualified_alias_type"),
            ("type Pair = int * int", "fsharp.qualified_alias_type"),
            ("type Callback = int -> int", "fsharp.qualified_alias_type"),
            (
                "type Choice = First = 1 | Library = 2",
                "fsharp.qualified_enum_type",
            ),
        ] {
            let source =
                format!("module Consumer\n{declaration}\nlet invoke input = Library.carry input\n");
            let tree = parse(&source, false);
            let definition = nodes(tree.root_node(), "type_definition")[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                cache
                    .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(
                cache
                    .qualified_file_context(tree.root_node(), source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(
                syntax(
                    definition,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some(label)
            );
            assert_eq!(
                capture_range(
                    definition,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some(definition.byte_range())
            );
            let visits = cache.body_visits;
            for _ in 0..3 {
                assert!(
                    cache
                        .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(cache.body_visits, visits);
            }
            for (entries, visits) in [(0, 1024), (1024, 1)] {
                let mut bounded = CaptureCache::new(entries, true, visits);
                assert!(
                    !bounded
                        .qualified_finite_type(definition, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(bounded.exhausted());
            }
            let cancelled = Cancellation::new();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                cache.qualified_finite_type(definition, source.as_bytes(), &cancelled),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }

    #[test]
    fn simple_lambda_formals_keep_native_ownership_and_bounded_body_reads() {
        let source = "module Lambdas\nlet build input = (fun value -> value + input)\n";
        let tree = parse(source, false);
        let lambda = nodes(tree.root_node(), "fun_expression")[0];
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1024, false, 1024);
        assert!(
            retain_capture(
                lambda,
                StructuralRole::Scope,
                source.as_bytes(),
                &cancellation,
                &mut cache,
            )
            .unwrap()
        );
        let formal = nodes(lambda, "identifier")[0];
        assert!(
            retain_capture(
                formal,
                StructuralRole::Declaration,
                source.as_bytes(),
                &cancellation,
                &mut cache,
            )
            .unwrap()
        );
        assert!(
            !cache
                .simple_body_read(formal, source.as_bytes(), &cancellation)
                .unwrap()
        );
        let mut optional = CaptureCache::new(1024, true, 1024);
        for name in nodes(lambda, "identifier").into_iter().skip(1) {
            assert!(
                optional
                    .simple_body_read(name, source.as_bytes(), &cancellation)
                    .unwrap()
            );
        }
        let mut bounded = CaptureCache::new(1024, true, 1);
        let read = nodes(lambda, "identifier")[1];
        assert!(
            !bounded
                .simple_body_read(read, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(bounded.exhausted());
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::Shutdown);
        assert!(matches!(
            optional.simple_body_read(read, source.as_bytes(), &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn lambda_atomic_formal_slots_keep_full_headers_and_exact_unit_witnesses() {
        let cancellation = Cancellation::new();
        for (arguments, expected) in [
            ("(value)", vec![("value", "(value)")]),
            ("(value: int)", vec![("value", "(value: int)")]),
            (
                "left (value: int) () ()",
                vec![("left", "left"), ("value", "(value: int)")],
            ),
            ("() ()", vec![]),
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!(
                    "module Lambdas{newline}let build input = (fun {arguments} -> input){newline}"
                );
                let tree = parse(&source, false);
                let lambda = nodes(tree.root_node(), "fun_expression")[0];
                let parameters = nodes(lambda, "argument_patterns")[0];
                let mut cache = CaptureCache::new(1024, true, 1024);
                assert!(
                    cache
                        .lambda(lambda, source.as_bytes(), &cancellation)
                        .unwrap(),
                    "{arguments}"
                );
                assert_eq!(
                    cache
                        .simple_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap(),
                    !arguments.contains("()")
                );
                assert_eq!(
                    cache
                        .unit_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap(),
                    arguments.contains("()")
                );
                for (name, header) in &expected {
                    let leaf = nodes(parameters, "identifier")
                        .into_iter()
                        .find(|node| source.get(node.byte_range()) == Some(*name))
                        .unwrap();
                    let range = cache
                        .parameter_signature(leaf, source.as_bytes(), &cancellation)
                        .unwrap()
                        .unwrap();
                    assert_eq!(source.get(range), Some(*header));
                }
                for unit in nodes(parameters, "unit") {
                    assert!(
                        cache
                            .unit_parameter(unit, source.as_bytes(), &cancellation)
                            .unwrap()
                    );
                }
                let mut bounded = CaptureCache::new(1, true, 1024);
                assert!(
                    !bounded
                        .lambda(lambda, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(bounded.exhausted());
            }
        }
    }

    #[test]
    fn lambda_proofs_reject_patterns_and_unreviewed_outer_read_paths() {
        let cancellation = Cancellation::new();
        for body in [
            "fun (left, right) -> left",
            "fun ((value)) -> value",
            "fun (Some value) -> value",
            "fun Some value -> value",
        ] {
            let source = format!("module Lambdas\nlet build input = ({body})\n");
            let tree = parse(&source, false);
            let lambda = nodes(tree.root_node(), "fun_expression")[0];
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                !cache
                    .lambda(lambda, source.as_bytes(), &cancellation)
                    .unwrap(),
                "{body}"
            );
            for name in nodes(lambda, "identifier") {
                assert!(
                    !cache
                        .simple_body_read(name, source.as_bytes(), &cancellation)
                        .unwrap(),
                    "{body}"
                );
            }
        }
        let source = "module Lambdas\nlet build input = match input with | Some captured -> (fun value -> value + captured) | None -> (fun value -> value)\n";
        let tree = parse(source, false);
        let mut cache = CaptureCache::new(1024, true, 1024);
        for lambda in nodes(tree.root_node(), "fun_expression") {
            assert!(
                cache
                    .lambda(lambda, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(
                cache
                    .lambda_capture(lambda, source.as_bytes(), &cancellation)
                    .unwrap()
                    .is_none()
            );
            for name in nodes(lambda, "identifier").into_iter().skip(1) {
                assert!(
                    !cache
                        .simple_body_read(name, source.as_bytes(), &cancellation)
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn optional_attribute_headers_keep_exact_native_evidence_and_cancellation() {
        for (source, expected) in [
            (
                "[<AutoOpen>]\nmodule Attributes\nlet identity δ = δ\n",
                "AutoOpen",
            ),
            (
                "module Attributes\n[<Witness(17)>]\nlet value = 17\n",
                "Witness(17)",
            ),
            (
                "module Attributes\n[<Witness(17.25</duration>)>]\nlet value = 17.25\n",
                "Witness(17.25</duration>)",
            ),
        ] {
            for newline in ["\n", "\r\n"] {
                let source = source.replace('\n', newline);
                let tree = parse(&source, false);
                let attributes = nodes(tree.root_node(), "attribute");
                assert_eq!(attributes.len(), 1);
                let attribute = attributes[0];
                assert_eq!(source.get(attribute.byte_range()), Some(expected));
                let cancellation = Cancellation::new();
                let mut cache = CaptureCache::new(0, true, usize::MAX);
                for _ in 0..3 {
                    assert!(
                        retain_capture(
                            attribute,
                            StructuralRole::Expression,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache,
                        )
                        .unwrap()
                    );
                    assert_eq!(
                        syntax(
                            attribute,
                            StructuralRole::Expression,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache,
                        )
                        .unwrap(),
                        Some("fsharp.unsupported_attribute")
                    );
                    assert_eq!(
                        capture_range(
                            attribute,
                            StructuralRole::Expression,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache,
                        )
                        .unwrap(),
                        Some(attribute.byte_range())
                    );
                }
                assert_eq!(cache.entries, 0);
                assert_eq!(cache.body_visits, 0);
                assert!(!cache.exhausted());
                assert!(!StructuralRole::Expression.belongs_to_identity_closure());
                for role in [
                    StructuralRole::Declaration,
                    StructuralRole::Definition,
                    StructuralRole::Signature,
                    StructuralRole::Scope,
                ] {
                    assert!(
                        !retain_capture(
                            attribute,
                            role,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache,
                        )
                        .unwrap()
                    );
                }
                assert!(cancellation.cancel(CancellationReason::ClientRequest));
                assert!(matches!(
                    retain_capture(
                        attribute,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache,
                    ),
                    Err(AdapterError::Cancelled { .. })
                ));
                assert!(matches!(
                    syntax(
                        attribute,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache,
                    ),
                    Err(AdapterError::Cancelled { .. })
                ));
                assert!(matches!(
                    capture_range(
                        attribute,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache,
                    ),
                    Err(AdapterError::Cancelled { .. })
                ));
            }
        }
    }

    #[test]
    fn nonrecursive_markers_require_literal_module_binding_context() {
        let cases: [(&str, &[&str]); 8] = [
            ("let value = 1\n", &["value"]),
            (
                "module Example\nlet value = 1\nlet carry x = x\n",
                &["value", "carry"],
            ),
            (
                "namespace Example\nmodule Nested =\n    let value = 1\n",
                &["value"],
            ),
            (
                "module Example\nlet rec first x = x\nand second x = x\n",
                &[],
            ),
            ("module rec Example\nlet value = 1\n", &[]),
            (
                "namespace rec Example\nmodule Nested =\n    let value = 1\n",
                &[],
            ),
            (
                "module Example\nlet outer x =\n    let local = x\n    local\n",
                &["outer"],
            ),
            (
                "module Example\ntype Holder() =\n    let value = 1\n    member _.Value = value\n",
                &[],
            ),
        ];
        for (source, expected) in cases {
            let tree = parse(source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            let mut admitted = Vec::new();
            for kind in ["function_declaration_left", "value_declaration_left"] {
                for head in nodes(tree.root_node(), kind) {
                    if cache
                        .nonrecursive_binding(head, source.as_bytes(), &cancellation)
                        .unwrap()
                    {
                        let name = selected_name(head, &cancellation).unwrap().unwrap();
                        admitted.push(source.get(name.byte_range()).unwrap());
                        assert_eq!(
                            syntax(
                                head,
                                StructuralRole::Expression,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap(),
                            Some("fsharp.nonrecursive_binding")
                        );
                        assert_eq!(
                            capture_range(
                                head,
                                StructuralRole::Expression,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap(),
                            cache
                                .bounds(head, source.as_bytes(), &cancellation)
                                .unwrap()
                                .map(|bounds| bounds.declaration)
                        );
                    }
                }
            }
            admitted.sort_unstable();
            let mut expected = expected.to_vec();
            expected.sort_unstable();
            assert_eq!(admitted, expected, "{source}");
        }
    }

    #[test]
    fn local_wrappers_keep_exact_heads_and_separate_continuation_witnesses() {
        let source = "module Locals\nlet outer input =\n    let cached = input\n    let carry (value: int) () = cached + value\n    carry input ()\n";
        for newline in ["\n", "\r\n"] {
            let source = source.replace('\n', newline);
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            for optional in [false, true] {
                let mut cache = CaptureCache::new(usize::MAX, optional, usize::MAX);
                let mut local_count = 0;
                for wrapper in nodes(tree.root_node(), "declaration_expression") {
                    if unique_continuation(wrapper, &cancellation)
                        .unwrap()
                        .is_none()
                    {
                        continue;
                    }
                    assert!(
                        retain_capture(
                            wrapper,
                            StructuralRole::Scope,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap()
                    );
                    assert_eq!(
                        syntax(
                            wrapper,
                            StructuralRole::Scope,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap(),
                        Some("fsharp.local_binding")
                    );
                    assert_eq!(
                        capture_range(
                            wrapper,
                            StructuralRole::Scope,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap(),
                        Some(wrapper.byte_range())
                    );
                    assert!(!scope_below_declaration(wrapper));
                    let group = direct_kind(wrapper, "function_or_value_defn", &cancellation)
                        .unwrap()
                        .unwrap();
                    let head = group.named_child(0).unwrap();
                    assert!(matches!(
                        head.kind(),
                        "function_declaration_left" | "value_declaration_left"
                    ));
                    let continuation = unique_continuation(wrapper, &cancellation)
                        .unwrap()
                        .unwrap();
                    let bounds = cache
                        .bounds(head, source.as_bytes(), &cancellation)
                        .unwrap()
                        .unwrap();
                    assert!(bounds.declaration.end <= continuation.start_byte());
                    assert!(
                        retain_capture(
                            head,
                            StructuralRole::Declaration,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap()
                    );
                    assert!(
                        retain_capture(
                            head,
                            StructuralRole::Definition,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap()
                    );
                    for (node, label, expected) in [
                        (group, "fsharp.local_binding", wrapper.byte_range()),
                        (
                            head,
                            "fsharp.local_group",
                            group.start_byte()..bounds.declaration.end,
                        ),
                        (
                            wrapper,
                            "fsharp.local_continuation",
                            continuation.byte_range(),
                        ),
                    ] {
                        assert_eq!(
                            retain_capture(
                                node,
                                StructuralRole::Expression,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap(),
                            optional
                        );
                        if optional {
                            assert_eq!(
                                syntax(
                                    node,
                                    StructuralRole::Expression,
                                    source.as_bytes(),
                                    &cancellation,
                                    &mut cache
                                )
                                .unwrap(),
                                Some(label)
                            );
                            assert_eq!(
                                capture_range(
                                    node,
                                    StructuralRole::Expression,
                                    source.as_bytes(),
                                    &cancellation,
                                    &mut cache
                                )
                                .unwrap(),
                                Some(expected)
                            );
                        }
                    }
                    local_count += 1;
                }
                assert_eq!(local_count, 2);
                let reads = nodes(tree.root_node(), "identifier")
                    .into_iter()
                    .filter(|name| {
                        cache
                            .simple_body_read(*name, source.as_bytes(), &cancellation)
                            .unwrap()
                    })
                    .count();
                assert_eq!(reads, if optional { 5 } else { 0 });
                let visits = cache.body_visits;
                let entries = cache.entries;
                for _ in 0..3 {
                    for wrapper in nodes(tree.root_node(), "declaration_expression") {
                        cache
                            .local_binding(wrapper, source.as_bytes(), &cancellation)
                            .unwrap();
                        cache
                            .local_capture(wrapper, source.as_bytes(), &cancellation)
                            .unwrap();
                    }
                    for name in nodes(tree.root_node(), "identifier") {
                        cache
                            .simple_body_read(name, source.as_bytes(), &cancellation)
                            .unwrap();
                    }
                    assert_eq!(cache.body_visits, visits);
                    assert_eq!(cache.entries, entries);
                }
                assert!(!cache.exhausted());
            }
        }
    }

    #[test]
    fn local_continuations_keep_scoped_unsupported_body_evidence() {
        for (continuation, expected_reads) in [
            ("fun (other, extra) -> other", &[][..]),
            ("match input with | other -> other", &["input", "other"][..]),
            ("seq { let other = input in yield other }", &[][..]),
            ("let rec inner value = value in inner input", &[][..]),
        ] {
            let source =
                format!("module Locals\nlet outer input = let cached = input in {continuation}\n");
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            let wrapper = nodes(tree.root_node(), "declaration_expression")
                .into_iter()
                .find(|wrapper| {
                    let Some(group) =
                        direct_kind(*wrapper, "function_or_value_defn", &cancellation).unwrap()
                    else {
                        return false;
                    };
                    source
                        .get(group.byte_range())
                        .is_some_and(|text| text.starts_with("let cached"))
                })
                .unwrap();
            let child = unique_continuation(wrapper, &cancellation)
                .unwrap()
                .unwrap();
            assert!(
                cache
                    .local_binding(wrapper, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(
                syntax(
                    wrapper,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some("fsharp.local_continuation")
            );
            assert!(
                retain_capture(
                    child,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap()
            );
            assert_eq!(
                syntax(
                    child,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some("fsharp.unsupported_body")
            );
            assert_eq!(
                capture_range(
                    child,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some(child.byte_range())
            );
            let mut actual = nodes(child, "identifier")
                .into_iter()
                .filter(|name| {
                    cache
                        .simple_body_read(*name, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .map(|name| name.byte_range())
                .collect::<Vec<_>>();
            actual.sort_by_key(|span| span.start);
            let mut expected = expected_reads
                .iter()
                .map(|name| {
                    let start = child.start_byte() + continuation.rfind(name).unwrap();
                    start..start + name.len()
                })
                .collect::<Vec<_>>();
            expected.sort_by_key(|span| span.start);
            assert_eq!(actual, expected, "{continuation}");
        }
    }

    #[test]
    fn local_native_proofs_reject_unreviewed_binder_and_owner_boundaries() {
        for body in [
            "let rec inner value = value in inner input",
            "let (left, right) = input in left",
            "let inner (left, _) = left in inner input",
            "fun (other, extra) -> let inner = other in inner",
            "if true then (let inner = input in inner) else input",
            "seq { let inner = input in yield inner }",
        ] {
            let source = format!("module Locals\nlet outer input = {body}\n");
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            for optional in [false, true] {
                let mut cache = CaptureCache::new(usize::MAX, optional, usize::MAX);
                for wrapper in nodes(tree.root_node(), "declaration_expression") {
                    assert!(
                        !cache
                            .local_binding(wrapper, source.as_bytes(), &cancellation)
                            .unwrap(),
                        "{body}"
                    );
                    assert!(
                        !retain_capture(
                            wrapper,
                            StructuralRole::Scope,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap(),
                        "{body}"
                    );
                }
            }
        }
    }

    #[test]
    fn local_named_tuple_formals_reuse_bounded_owned_head_proofs() {
        for body in [
            "let inner (left, right) = left in inner (input, input)",
            "\n    let inner (left, right) = left\n    inner (input, input)",
        ] {
            let source = format!("module Locals\nlet outer input = {body}\n");
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            let wrapper = nodes(tree.root_node(), "declaration_expression")
                .into_iter()
                .find(|wrapper| {
                    unique_continuation(*wrapper, &cancellation)
                        .unwrap()
                        .is_some()
                })
                .unwrap();
            assert!(
                cache
                    .local_binding(wrapper, source.as_bytes(), &cancellation)
                    .unwrap(),
                "{body}"
            );
            assert_eq!(cache.formal_visits, 3, "{body}");
            // The local wrapper and its named head each establish ownership once.
            for parameters in nodes(wrapper, "argument_patterns") {
                assert!(
                    cache
                        .compound_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap()
                );
            }
            let visits = cache.formal_visits;
            assert_eq!(visits, 6, "{body}");
            for _ in 0..3 {
                assert!(
                    cache
                        .local_binding(wrapper, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                for parameters in nodes(wrapper, "argument_patterns") {
                    assert!(
                        cache
                            .compound_parameters(parameters, source.as_bytes(), &cancellation)
                            .unwrap()
                    );
                }
            }
            assert_eq!(cache.formal_visits, visits);
            let mut bounded = CaptureCache::new(usize::MAX, true, 1);
            assert!(
                !bounded
                    .local_binding(wrapper, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(bounded.exhausted());
        }
    }

    #[test]
    fn local_proofs_share_bounded_body_work_and_propagate_cancellation() {
        let source = "module Locals\nlet outer input = let cached = input in let carry () = cached in carry ()\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let wrappers = nodes(tree.root_node(), "declaration_expression")
            .into_iter()
            .filter(|wrapper| {
                unique_continuation(*wrapper, &cancellation)
                    .unwrap()
                    .is_some()
            })
            .collect::<Vec<_>>();
        assert_eq!(wrappers.len(), 2);
        let mut mandatory = CaptureCache::new(usize::MAX, false, 0);
        for wrapper in &wrappers {
            assert!(
                mandatory
                    .local_binding(*wrapper, source.as_bytes(), &cancellation)
                    .unwrap()
            );
        }
        assert_eq!(mandatory.body_visits, 0);
        assert!(!mandatory.exhausted());
        let mut bounded = CaptureCache::new(0, false, usize::MAX);
        assert!(
            !bounded
                .local_binding(wrappers[0], source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(bounded.exhausted());
        let mut body_bounded = CaptureCache::new(usize::MAX, true, 0);
        let read = nodes(tree.root_node(), "identifier")
            .into_iter()
            .find(|node| node.start_byte() == source.find("input in").unwrap())
            .unwrap();
        assert!(
            !body_bounded
                .simple_body_read(read, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(body_bounded.exhausted());
        assert_eq!(body_bounded.body_visits, 0);
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            mandatory.local_binding(wrappers[0], source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            retain_capture(
                wrappers[0],
                StructuralRole::Scope,
                source.as_bytes(),
                &cancellation,
                &mut mandatory
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            capture_range(
                wrappers[0],
                StructuralRole::Expression,
                source.as_bytes(),
                &cancellation,
                &mut mandatory
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn application_markers_keep_one_outer_spine_and_exact_immediate_arguments() {
        let source = "module Applications\nlet invoke first second = helper first second\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        let applications = nodes(tree.root_node(), "application_expression");
        assert_eq!(applications.len(), 2);
        let calls = applications
            .iter()
            .filter(|node| {
                cache
                    .application_call(**node, source.as_bytes(), &cancellation)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            source.get(calls[0].byte_range()),
            Some("helper first second")
        );
        let mut arguments = Vec::new();
        for application in applications {
            let (_, argument) = application_operands(application, &cancellation)
                .unwrap()
                .unwrap();
            assert!(
                cache
                    .application_argument(argument, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(
                syntax(
                    argument,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some("fsharp.application_argument")
            );
            assert_eq!(
                capture_range(
                    argument,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                Some(argument.byte_range())
            );
            arguments.push(source.get(argument.byte_range()).unwrap());
        }
        arguments.sort_unstable();
        assert_eq!(arguments, ["first", "second"]);
    }

    #[test]
    fn application_arguments_preserve_native_parenthesis_groups() {
        for (body, expected_calls, expected_arguments) in [
            ("helper(value)", vec!["helper(value)"], vec!["(value)"]),
            (
                "outer inner(value)",
                vec!["outer inner(value)", "inner(value)"],
                vec!["inner(value)", "(value)"],
            ),
            (
                "outer (inner value)",
                vec!["outer (inner value)", "inner value"],
                vec!["(inner value)", "value"],
            ),
        ] {
            let source = format!("module Applications\nlet invoke value = {body}\n");
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            let mut calls = Vec::new();
            let mut arguments = Vec::new();
            for application in nodes(tree.root_node(), "application_expression") {
                if cache
                    .application_call(application, source.as_bytes(), &cancellation)
                    .unwrap()
                {
                    calls.push(source.get(application.byte_range()).unwrap());
                }
                let (_, argument) = application_operands(application, &cancellation)
                    .unwrap()
                    .unwrap();
                assert!(
                    cache
                        .application_argument(argument, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                let span = capture_range(
                    argument,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
                .unwrap();
                arguments.push(source.get(span).unwrap());
            }
            calls.sort_unstable();
            arguments.sort_unstable();
            let mut expected_calls = expected_calls;
            let mut expected_arguments = expected_arguments;
            expected_calls.sort_unstable();
            expected_arguments.sort_unstable();
            assert_eq!(calls, expected_calls, "{body}");
            assert_eq!(arguments, expected_arguments, "{body}");
        }
    }

    #[test]
    fn optional_body_coverage_keeps_direct_reads_and_proven_literal_shapes() {
        for body in [
            "δ",
            "(δ)",
            "begin δ end",
            "begin (* comment *) δ end",
            "0",
            "true",
            "()",
            "'a'",
            "\"☃\"",
            "@\"plain\"",
            "\"\"\"plain\"\"\"",
            "\"plain\"B",
            "[0; 1]",
        ] {
            let source = format!("module Bodies\nlet identity δ = {body}\n");
            let tree = parse(&source, false);
            let bodies = binding_bodies(tree.root_node());
            assert_eq!(bodies.len(), 1, "{body}");
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            assert!(
                !cache
                    .unsupported_body(bodies[0], source.as_bytes(), &cancellation)
                    .unwrap(),
                "{body}"
            );
            assert!(
                !retain_capture(
                    bodies[0],
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                "{body}"
            );
            let visits = cache.body_visits;
            for _ in 0..3 {
                assert!(
                    !cache
                        .unsupported_body(bodies[0], source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(cache.body_visits, visits);
            }
            let reads = nodes(tree.root_node(), "identifier")
                .into_iter()
                .filter(|name| {
                    cache
                        .simple_body_read(*name, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .count();
            assert_eq!(reads, usize::from(body.contains('δ')), "{body}");
            assert!(!cache.exhausted());
        }
    }

    #[test]
    fn optional_body_coverage_marks_binding_call_and_interpolation_boundaries() {
        for (body, expected_reads) in [
            ("value.Length", 0),
            ("(fun (value, extra) -> value)", 0),
            ("match value with | other -> other", 2),
            ("value value", 2),
            ("value + 1", 1),
            ("-value", 1),
            ("$\"{value}\"", 0),
            ("$\"plain\"", 0),
            ("<@ value @>", 0),
        ] {
            let source = format!("module Bodies\nlet outer value = {body}\n");
            let tree = parse(&source, false);
            let body_node = binding_bodies(tree.root_node())[0];
            assert_eq!(source.get(body_node.byte_range()), Some(body));
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            assert!(
                retain_capture(
                    body_node,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                "{body}"
            );
            assert_eq!(
                syntax(
                    body_node,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                Some("fsharp.unsupported_body"),
                "{body}"
            );
            assert_eq!(
                capture_range(
                    body_node,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                Some(body_node.byte_range()),
                "{body}"
            );
            let reads = nodes(tree.root_node(), "identifier")
                .into_iter()
                .filter(|name| {
                    cache
                        .simple_body_read(*name, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .count();
            assert_eq!(reads, expected_reads, "{body}");
        }
    }

    #[test]
    fn optional_value_bodies_keep_rhs_reads_without_projecting_formal_headers() {
        for (body, unsupported, read_count) in [
            ("0", false, 0),
            ("\"plain\"", false, 0),
            ("other", false, 1),
            ("fn 1", true, 1),
        ] {
            let source = format!("module Values\nlet value = {body}\n");
            let tree = parse(&source, false);
            let body_node = binding_bodies(tree.root_node())[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            assert_eq!(
                retain_capture(
                    body_node,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap(),
                unsupported,
                "{body}"
            );
            if unsupported {
                assert_eq!(
                    syntax(
                        body_node,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache,
                    )
                    .unwrap(),
                    Some("fsharp.unsupported_body")
                );
            }
            let names = nodes(tree.root_node(), "identifier");
            assert_eq!(
                names
                    .iter()
                    .filter(|name| cache
                        .simple_body_read(**name, source.as_bytes(), &cancellation)
                        .unwrap())
                    .count(),
                read_count,
                "{body}"
            );
            for name in names {
                assert!(
                    cache
                        .parameter_signature(name, source.as_bytes(), &cancellation)
                        .unwrap()
                        .is_none()
                );
            }
            let head = nodes(tree.root_node(), "value_declaration_left")[0];
            let mut mandatory = CaptureCache::new(3, false, usize::MAX);
            assert_eq!(
                mandatory
                    .written_kind(head, source.as_bytes(), &cancellation)
                    .unwrap(),
                Some("fsharp.variable")
            );
            assert_eq!(mandatory.entries, 3);
            assert_eq!(mandatory.body_visits, 0);
            assert!(mandatory.bodies.is_empty());
            assert!(!mandatory.exhausted());
        }
    }

    #[test]
    fn optional_body_coverage_uses_exact_recursive_fields_and_separate_work_bound() {
        let source =
            "module Bodies\nlet rec first value = value.Length\nand second value = value\n";
        let tree = parse(source, false);
        let bodies = binding_bodies(tree.root_node());
        assert_eq!(bodies.len(), 2);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        assert!(
            cache
                .unsupported_body(bodies[0], source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(
            !cache
                .unsupported_body(bodies[1], source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert_eq!(source.get(bodies[0].byte_range()), Some("value.Length"));
        assert_eq!(source.get(bodies[1].byte_range()), Some("value"));
        assert!(bodies[0].end_byte() <= bodies[1].start_byte());
        let visits = cache.body_visits;
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            cache.unsupported_body(bodies[0], source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert_eq!(cache.body_visits, visits);

        let source = "module Bounded\nlet identity value = [0; 1; 2; 3; 4; 5]\n";
        let tree = parse(source, false);
        let body = binding_bodies(tree.root_node())[0];
        let cancellation = Cancellation::new();

        let mut no_body_work = CaptureCache::new(usize::MAX, true, 0);
        assert!(
            !no_body_work
                .unsupported_body(body, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(no_body_work.exhausted());
        assert_eq!(no_body_work.body_visits, 0);
        assert!(no_body_work.bodies.get(&body.id()).unwrap().unsupported);

        let mut bounded = CaptureCache::new(usize::MAX, true, 8);
        assert!(
            !bounded
                .unsupported_body(body, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(bounded.exhausted());
        assert_eq!(bounded.body_visits, 8);
        assert!(bounded.bodies.get(&body.id()).unwrap().unsupported);
        assert!(bounded.reads.is_empty());

        let mut storage_bounded = CaptureCache::new(6, true, usize::MAX);
        assert!(
            !storage_bounded
                .unsupported_body(body, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert_eq!(storage_bounded.entries, 6);
        assert!(storage_bounded.body_visits > 8);
        assert!(!storage_bounded.exhausted());
        let visits = storage_bounded.body_visits;
        assert!(
            !storage_bounded
                .unsupported_body(body, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert_eq!(storage_bounded.body_visits, visits);
    }

    #[test]
    fn recursive_heads_keep_disjoint_body_bounds() {
        let source = "module Rec\nlet rec first n = n + 1\nand second n = n + 2\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let heads = nodes(tree.root_node(), "function_declaration_left");
        assert_eq!(heads.len(), 2);
        let ranges: Vec<_> = heads
            .iter()
            .map(|head| {
                binding_bounds(*head, &cancellation)
                    .unwrap()
                    .unwrap()
                    .declaration
            })
            .collect();
        assert_eq!(source.get(ranges[0].clone()), Some("first n = n + 1"));
        assert_eq!(source.get(ranges[1].clone()), Some("second n = n + 2"));
        assert!(ranges[0].end <= ranges[1].start);
    }

    #[test]
    fn recursive_markers_partition_complete_native_module_groups() {
        for source in [
            "let rec carry value = carry value\n",
            "module Groups\nlet rec first value = second value\nand second value = first value\n",
            "namespace Groups\nmodule Nested =\n    let rec carry () = carry ()\n",
            "module Groups\nlet rec first () value = second value ()\nand second value () = first () value\n",
            "module Groups\nlet rec (* prefix *) first value = second value\n(* connector *) and second value = first value\n",
            "module Groups\nlet rec first (left, right) = second left\nand second value = first (value, value)\n",
        ] {
            for newline in ["\n", "\r\n"] {
                let source = source.replace('\n', newline);
                let tree = parse(&source, false);
                let group = nodes(tree.root_node(), "function_or_value_defn")[0];
                let heads = nodes(group, "function_declaration_left");
                let cancellation = Cancellation::new();
                let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
                let group_capture = cache
                    .recursive_capture(group, source.as_bytes(), &cancellation)
                    .unwrap()
                    .unwrap();
                assert_eq!(group_capture.0, "fsharp.recursive_group");
                assert_eq!(group_capture.1.start, group.start_byte());
                assert_eq!(
                    group_capture.1.end,
                    cache
                        .bounds(*heads.last().unwrap(), source.as_bytes(), &cancellation)
                        .unwrap()
                        .unwrap()
                        .body
                        .end
                );
                let mut parts = Vec::new();
                for kind in ["rec", "and", "function_declaration_left"] {
                    for node in nodes(group, kind) {
                        let (label, bounds) = cache
                            .recursive_capture(node, source.as_bytes(), &cancellation)
                            .unwrap()
                            .unwrap();
                        assert!(
                            retain_capture(
                                node,
                                StructuralRole::Expression,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap()
                        );
                        assert_eq!(
                            syntax(
                                node,
                                StructuralRole::Expression,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap(),
                            Some(label)
                        );
                        assert_eq!(
                            capture_range(
                                node,
                                StructuralRole::Expression,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap(),
                            Some(bounds.clone())
                        );
                        if node.kind() == "function_declaration_left" {
                            assert_eq!(label, "fsharp.recursive_member");
                            assert_eq!(
                                bounds,
                                cache
                                    .bounds(node, source.as_bytes(), &cancellation)
                                    .unwrap()
                                    .unwrap()
                                    .declaration
                            );
                        }
                        parts.push((bounds, label));
                    }
                }
                parts.sort_unstable_by_key(|(bounds, _)| bounds.start);
                assert_eq!(parts.len(), heads.len() * 2);
                let mut end = group_capture.1.start;
                for (index, (bounds, label)) in parts.iter().enumerate() {
                    assert_eq!(bounds.start, end, "{source}");
                    assert!(bounds.start < bounds.end);
                    assert_eq!(
                        *label,
                        if index == 0 {
                            "fsharp.recursive_prefix"
                        } else if index % 2 == 1 {
                            "fsharp.recursive_member"
                        } else {
                            "fsharp.recursive_separator"
                        }
                    );
                    end = bounds.end;
                }
                assert_eq!(end, group_capture.1.end);
                assert!(StructuralRole::Expression.container_rank().is_none());
                assert!(!StructuralRole::Expression.belongs_to_identity_closure());
                let expected_visits = group.child_count()
                    + nodes(group, "argument_patterns")
                        .iter()
                        .map(Node::child_count)
                        .sum::<usize>();
                assert_eq!(cache.direct_child_visits, expected_visits);
            }
        }
    }

    #[test]
    fn recursive_markers_reject_unreviewed_names_owners_and_value_groups() {
        for source in [
            "module Groups\nlet carry value = value\n",
            "module rec Groups\nlet rec carry value = carry value\n",
            "namespace rec Groups\nmodule Nested =\n    let rec carry value = carry value\n",
            "module Groups\nlet outer value =\n    let rec local item = local item\n    local value\n",
            "module Groups\nlet rec value = 1\n",
            "module Groups\nlet rec first value = value\nand other = 1\n",
            "module Groups\nlet rec (++) left right = left\n",
        ] {
            let tree = parse(source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            for kind in [
                "function_or_value_defn",
                "function_declaration_left",
                "rec",
                "and",
            ] {
                for node in nodes(tree.root_node(), kind) {
                    assert!(
                        cache
                            .recursive_capture(node, source.as_bytes(), &cancellation)
                            .unwrap()
                            .is_none(),
                        "{source}: {kind}"
                    );
                }
            }
        }
    }

    #[test]
    fn recursive_native_proof_keeps_optional_storage_separate_and_cancellable() {
        let source = "module Groups\nlet rec first () value = second value ()\nand second value () = first () value\n";
        let tree = parse(source, false);
        let group = nodes(tree.root_node(), "function_or_value_defn")[0];
        let parameters = nodes(group, "argument_patterns");
        let cancellation = Cancellation::new();
        let mut mandatory = CaptureCache::new(11, false, 0);
        for parameters in &parameters {
            assert!(
                mandatory
                    .reviewed_parameters(*parameters, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(
                unsupported_declaration(
                    *parameters,
                    source.as_bytes(),
                    &cancellation,
                    &mut mandatory
                )
                .unwrap(),
                None
            );
        }
        assert_eq!(mandatory.entries, 11);
        assert!(!mandatory.exhausted());
        assert_eq!(mandatory.body_visits, 0);
        assert!(mandatory.recursive_tokens.is_empty());
        assert!(
            mandatory
                .recursive_capture(group, source.as_bytes(), &cancellation)
                .unwrap()
                .is_none()
        );

        for maximum in [0, 1, 3] {
            let mut bounded = CaptureCache::new(maximum, true, usize::MAX);
            assert!(
                bounded
                    .recursive_capture(group, source.as_bytes(), &cancellation)
                    .unwrap()
                    .is_none()
            );
            assert!(bounded.exhausted());
            assert_eq!(bounded.entries, maximum);
        }
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        assert!(
            cache
                .recursive_capture(group, source.as_bytes(), &cancellation)
                .unwrap()
                .is_some()
        );
        let entries = cache.entries;
        let visits = cache.direct_child_visits;
        for _ in 0..3 {
            for kind in [
                "function_or_value_defn",
                "function_declaration_left",
                "rec",
                "and",
            ] {
                for node in nodes(group, kind) {
                    assert!(
                        cache
                            .recursive_capture(node, source.as_bytes(), &cancellation)
                            .unwrap()
                            .is_some()
                    );
                }
            }
            assert_eq!(cache.entries, entries);
            assert_eq!(cache.direct_child_visits, visits);
        }
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            cache.recursive_capture(group, source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            retain_capture(
                group,
                StructuralRole::Expression,
                source.as_bytes(),
                &cancellation,
                &mut cache
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            capture_range(
                group,
                StructuralRole::Expression,
                source.as_bytes(),
                &cancellation,
                &mut cache
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn body_read_selection_conserves_ownership_as_native_groups_grow() {
        use std::{fmt::Write, hint::black_box, time::Instant};

        for width in [16, 32, 64, 128] {
            for recursive in [false, true] {
                let mut source = String::from("module Scaling\n");
                if recursive {
                    for index in 0..width {
                        let keyword = if index == 0 { "let rec" } else { "and" };
                        writeln!(source, "{keyword} f{index} x = x").unwrap();
                    }
                } else {
                    source.push_str("let gather");
                    for index in 0..width {
                        write!(source, " p{index}").unwrap();
                    }
                    source.push_str(" = [");
                    for index in 0..width {
                        if index > 0 {
                            source.push_str("; ");
                        }
                        write!(source, "p{index}").unwrap();
                    }
                    source.push_str("]\n");
                }
                let tree = parse(&source, false);
                let names = nodes(tree.root_node(), "identifier");
                let expected_child_visits = nodes(tree.root_node(), "function_or_value_defn")
                    .iter()
                    .chain(nodes(tree.root_node(), "argument_patterns").iter())
                    .map(Node::child_count)
                    .sum::<usize>();
                let cancellation = Cancellation::new();
                let started = Instant::now();
                for _ in 0..3 {
                    let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
                    let count = names
                        .iter()
                        .filter(|name| {
                            cache
                                .simple_body_read(
                                    black_box(**name),
                                    black_box(source.as_bytes()),
                                    &cancellation,
                                )
                                .unwrap()
                        })
                        .count();
                    assert_eq!(count, width);
                    assert_eq!(cache.direct_child_visits, expected_child_visits);
                }
                eprintln!(
                    "fsharp body selection: recursive={recursive}, width={width}, iterations=3, elapsed_ns={}",
                    started.elapsed().as_nanos()
                );
            }
        }
    }

    #[test]
    fn repeated_role_normalization_reuses_native_group_and_formal_inspection() {
        let source =
            "module Reuse\nlet rec first left right = [left; right]\nand second item = (item)\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        let heads = nodes(tree.root_node(), "function_declaration_left");
        let formals = nodes(tree.root_node(), "argument_patterns");
        let names = nodes(tree.root_node(), "identifier");
        for _ in 0..3 {
            for head in &heads {
                for role in [
                    StructuralRole::Declaration,
                    StructuralRole::Definition,
                    StructuralRole::Signature,
                    StructuralRole::Scope,
                ] {
                    assert!(
                        retain_capture(*head, role, source.as_bytes(), &cancellation, &mut cache)
                            .unwrap()
                    );
                    assert!(
                        syntax(*head, role, source.as_bytes(), &cancellation, &mut cache)
                            .unwrap()
                            .is_some()
                    );
                    assert!(
                        capture_range(*head, role, source.as_bytes(), &cancellation, &mut cache)
                            .unwrap()
                            .is_some()
                    );
                }
            }
            for parameters in &formals {
                assert!(
                    cache
                        .simple_parameters(*parameters, source.as_bytes(), &cancellation)
                        .unwrap()
                );
            }
            assert_eq!(
                names
                    .iter()
                    .filter(|name| cache
                        .simple_body_read(**name, source.as_bytes(), &cancellation)
                        .unwrap())
                    .count(),
                3
            );
            let expected = nodes(tree.root_node(), "function_or_value_defn")
                .iter()
                .chain(formals.iter())
                .map(Node::child_count)
                .sum::<usize>();
            assert_eq!(cache.direct_child_visits, expected);
        }
    }

    #[test]
    fn begin_end_reads_keep_locals_and_simple_lambda_ownership() {
        for (body, expected) in [
            ("begin x end", 1),
            ("begin (fun x -> x) end", 1),
            ("begin (fun (x, y) -> x) end", 0),
            ("begin\n    let x = 1\n    x\nend", 1),
        ] {
            let source = format!("module Blocks\nlet outer x = {body}\n");
            let tree = parse(&source, false);
            assert_eq!(nodes(tree.root_node(), "begin_end_expression").len(), 1);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            let reads = nodes(tree.root_node(), "identifier")
                .into_iter()
                .filter(|name| {
                    cache
                        .simple_body_read(*name, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .count();
            assert_eq!(reads, expected, "{body}");
        }
    }

    #[test]
    fn body_read_capture_does_not_imply_an_all_simple_formal_marker() {
        let source = "module Patterns\nlet choose x (left, right) = x\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        let parameters = nodes(tree.root_node(), "argument_patterns")[0];
        assert!(
            !cache
                .simple_parameters(parameters, source.as_bytes(), &cancellation)
                .unwrap()
        );
        let reads = nodes(tree.root_node(), "identifier")
            .into_iter()
            .filter(|name| {
                cache
                    .simple_body_read(*name, source.as_bytes(), &cancellation)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        assert_eq!(source.get(reads[0].byte_range()), Some("x"));
        assert!(
            !retain_capture(
                parameters,
                StructuralRole::Signature,
                source.as_bytes(),
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
    }

    #[test]
    fn mandatory_binding_cache_is_independent_of_optional_rhs_read_pressure() {
        for width in [16, 64, 128] {
            let source = format!(
                "module Identity\nlet identity x = [{}]\n",
                vec!["x"; width].join("; ")
            );
            let tree = parse(&source, false);
            let head = nodes(tree.root_node(), "function_declaration_left")[0];
            let parameters = nodes(tree.root_node(), "argument_patterns")[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(6, false, usize::MAX);
            assert_eq!(
                cache
                    .written_kind(head, source.as_bytes(), &cancellation)
                    .unwrap(),
                Some("fsharp.function")
            );
            assert!(
                cache
                    .simple_parameters(parameters, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(cache.entries, 6);
            assert!(!cache.exhausted());
            assert!(cache.reads.is_empty());
        }
    }

    #[test]
    fn cache_admission_is_bounded_and_cached_lookups_remain_cancellable() {
        let source = "module Bounded\nlet identity x = x\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let head = nodes(tree.root_node(), "function_declaration_left")[0];
        let parameters = nodes(tree.root_node(), "argument_patterns")[0];
        let read = *nodes(tree.root_node(), "identifier").last().unwrap();
        let mut bounded = CaptureCache::new(5, true, usize::MAX);
        assert!(
            !bounded
                .simple_body_read(read, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(bounded.exhausted());
        assert_eq!(bounded.entries, 5);

        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        assert!(
            cache
                .simple_body_read(read, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            cache.simple_body_read(read, source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            cache.simple_parameters(parameters, source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            cache.bounds(head, source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn parenthesized_formal_cache_reuses_exact_leaf_proofs_with_bounded_admission() {
        for formal in ["(value)", "(value: int)"] {
            let source = format!("module Formals\nlet identity {formal} = value\n");
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            let head = nodes(tree.root_node(), "function_declaration_left")[0];
            let parameters = nodes(tree.root_node(), "argument_patterns")[0];
            let name = nodes(tree.root_node(), "identifier")
                .into_iter()
                .find(|node| source.get(node.byte_range()) == Some("value"))
                .unwrap();
            let mut bounded = CaptureCache::new(4, false, usize::MAX);
            assert!(
                !bounded
                    .simple_parameters(parameters, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(bounded.exhausted());
            assert_eq!(bounded.entries, 4);

            let mut cache = CaptureCache::new(6, false, usize::MAX);
            assert_eq!(
                cache
                    .parameter_head(name, source.as_bytes(), &cancellation)
                    .unwrap(),
                Some(head.id())
            );
            for _ in 0..3 {
                assert!(
                    cache
                        .simple_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(
                    cache
                        .parameter_head(name, source.as_bytes(), &cancellation)
                        .unwrap(),
                    Some(head.id())
                );
                assert!(
                    cache
                        .bounds(head, source.as_bytes(), &cancellation)
                        .unwrap()
                        .is_some()
                );
                assert_eq!(cache.entries, 6);
                assert!(!cache.exhausted());
                let expected = nodes(tree.root_node(), "function_or_value_defn")
                    .iter()
                    .map(Node::child_count)
                    .sum::<usize>()
                    + parameters.child_count();
                assert_eq!(cache.direct_child_visits, expected);
            }
            assert!(cancellation.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                cache.parameter_head(name, source.as_bytes(), &cancellation),
                Err(AdapterError::Cancelled { .. })
            ));
            assert!(matches!(
                cache.simple_parameters(parameters, source.as_bytes(), &cancellation),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }

    #[test]
    fn unit_formals_keep_exact_optional_slots_without_named_identities() {
        for (formals, expected_units) in [
            ("()", 1),
            ("( )", 1),
            ("value ()", 1),
            ("() (value: int)", 1),
            ("() ()", 2),
        ] {
            let source = format!("module Units\nlet carry {formals} = 0\n");
            let tree = parse(&source, false);
            assert!(!tree.root_node().has_error(), "{formals}");
            let parameters = nodes(tree.root_node(), "argument_patterns")[0];
            let cancellation = Cancellation::new();
            let mut mandatory = CaptureCache::new(usize::MAX, false, usize::MAX);
            assert!(
                mandatory
                    .reviewed_parameters(parameters, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(
                !mandatory
                    .simple_parameters(parameters, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(
                unsupported_declaration(
                    parameters,
                    source.as_bytes(),
                    &cancellation,
                    &mut mandatory
                )
                .unwrap(),
                None
            );
            assert!(
                !retain_capture(
                    parameters,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut mandatory
                )
                .unwrap()
            );
            let mut optional = CaptureCache::new(usize::MAX, true, usize::MAX);
            assert!(
                retain_capture(
                    parameters,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut optional
                )
                .unwrap()
            );
            assert_eq!(
                syntax(
                    parameters,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut optional
                )
                .unwrap(),
                Some("fsharp.unit_parameters")
            );
            assert_eq!(
                capture_range(
                    parameters,
                    StructuralRole::Expression,
                    source.as_bytes(),
                    &cancellation,
                    &mut optional
                )
                .unwrap(),
                Some(parameters.byte_range())
            );
            let mut units = 0usize;
            for unit in nodes(tree.root_node(), "unit") {
                if !optional
                    .unit_parameter(unit, source.as_bytes(), &cancellation)
                    .unwrap()
                {
                    continue;
                }
                units += 1;
                assert_eq!(
                    syntax(
                        unit,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut optional
                    )
                    .unwrap(),
                    Some("fsharp.unit_parameter")
                );
                assert_eq!(
                    capture_range(
                        unit,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut optional
                    )
                    .unwrap(),
                    Some(unit.byte_range())
                );
                for role in [StructuralRole::Declaration, StructuralRole::Definition] {
                    assert!(
                        !retain_capture(
                            unit,
                            role,
                            source.as_bytes(),
                            &cancellation,
                            &mut optional
                        )
                        .unwrap()
                    );
                }
            }
            assert_eq!(units, expected_units, "{formals}");
        }
    }

    #[test]
    fn unit_formals_do_not_admit_recursive_modules_local_rec_or_pattern_barriers() {
        for source in [
            "module rec Units\nlet carry () = 0\n",
            "module Units\nlet outer value =\n    let rec carry () = 0\n    carry ()\n",
            "module Units\nlet carry () _ = 0\n",
            "module Units\nlet carry () (left, _) = 0\n",
        ] {
            let tree = parse(source, false);
            assert!(!tree.root_node().has_error(), "{source}");
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            for parameters in nodes(tree.root_node(), "argument_patterns") {
                assert!(
                    !cache
                        .unit_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap(),
                    "{source}"
                );
            }
            for unit in nodes(tree.root_node(), "unit") {
                assert!(
                    !cache
                        .unit_parameter(unit, source.as_bytes(), &cancellation)
                        .unwrap(),
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn unit_formals_keep_cache_and_cancellation_bounds() {
        let source = "module Units\nlet carry () = 0\n";
        let tree = parse(source, false);
        let parameters = nodes(tree.root_node(), "argument_patterns")[0];
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(1, true, usize::MAX);
        assert!(
            !bounded
                .unit_parameters(parameters, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(bounded.exhausted());
        assert_eq!(bounded.entries, 1);
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        assert!(matches!(
            cache.unit_parameters(parameters, source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn optional_parameter_headers_keep_exact_authored_patterns() {
        for (formals, expected) in [
            ("value", vec![("value", "value")]),
            ("(value)", vec![("value", "(value)")]),
            ("(value: int)", vec![("value", "(value: int)")]),
            (
                "left (right: int)",
                vec![("left", "left"), ("right", "(right: int)")],
            ),
            ("(caf\u{e9}: int)", vec![("caf\u{e9}", "(caf\u{e9}: int)")]),
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!("module Headers{newline}let carry {formals} = 0{newline}");
                let tree = parse(&source, false);
                assert!(!tree.root_node().has_error(), "{formals}");
                let cancellation = Cancellation::new();
                let mut cache = CaptureCache::new(usize::MAX, false, usize::MAX);
                for (name, pattern) in &expected {
                    let leaf = nodes(tree.root_node(), "identifier")
                        .into_iter()
                        .filter(|node| source.get(node.byte_range()) == Some(*name))
                        .min_by_key(Node::start_byte)
                        .unwrap();
                    assert!(
                        retain_capture(
                            leaf,
                            StructuralRole::Expression,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap(),
                        "{formals}: {name}"
                    );
                    assert_eq!(
                        syntax(
                            leaf,
                            StructuralRole::Expression,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap(),
                        Some("fsharp.parameter_header")
                    );
                    let start = source.find(pattern).unwrap();
                    assert_eq!(
                        capture_range(
                            leaf,
                            StructuralRole::Expression,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache
                        )
                        .unwrap(),
                        Some(start..start + pattern.len())
                    );
                    for role in [StructuralRole::Declaration, StructuralRole::Definition] {
                        assert!(
                            retain_capture(
                                leaf,
                                role,
                                source.as_bytes(),
                                &cancellation,
                                &mut cache
                            )
                            .unwrap()
                        );
                        assert_eq!(
                            capture_range(leaf, role, source.as_bytes(), &cancellation, &mut cache)
                                .unwrap(),
                            Some(leaf.byte_range())
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn optional_parameter_header_range_keeps_annotation_and_parentheses() {
        let source = "module Headers\nlet carry (value: int) = value\n";
        let tree = parse(source, false);
        assert!(!tree.root_node().has_error());
        let leaf = nodes(tree.root_node(), "identifier")
            .into_iter()
            .find(|node| source.get(node.byte_range()) == Some("value"))
            .unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(usize::MAX, false, usize::MAX);
        let range = capture_range(
            leaf,
            StructuralRole::Expression,
            source.as_bytes(),
            &cancellation,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(source.get(range), Some("(value: int)"));
    }

    #[test]
    fn optional_parameter_headers_require_all_simple_formals() {
        for formals in ["value (left, _)", "value ((typed: int))", "value _"] {
            let source = format!("module Headers\nlet carry {formals} = 0\n");
            let tree = parse(&source, false);
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, false, usize::MAX);
            for name in nodes(tree.root_node(), "identifier") {
                assert!(
                    !retain_capture(
                        name,
                        StructuralRole::Expression,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache
                    )
                    .unwrap(),
                    "{formals}"
                );
            }
        }
    }

    #[test]
    fn named_tuple_headers_keep_leaf_first_ownership_and_disjoint_typed_spans() {
        for (formals, slot, expected) in [
            (
                "(left, right)",
                "(left, right)",
                vec![("left", "left"), ("right", "right")],
            ),
            (
                "(left: int, right: int)",
                "(left: int, right: int)",
                vec![("left", "left: int"), ("right", "right: int")],
            ),
            (
                "(left, (middle, right))",
                "(left, (middle, right))",
                vec![("left", "left"), ("middle", "middle"), ("right", "right")],
            ),
            (
                "() (left, right) extra",
                "(left, right)",
                vec![("left", "left"), ("right", "right"), ("extra", "extra")],
            ),
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!("module Tuples{newline}let combine {formals} = left{newline}");
                let tree = parse(&source, false);
                let parameters = nodes(tree.root_node(), "argument_patterns")[0];
                let cancellation = Cancellation::new();
                let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
                // Request deepest leaves before any owner/list capture.
                for (name, header) in expected.iter().rev() {
                    let leaf = nodes(parameters, "identifier")
                        .into_iter()
                        .find(|node| source.get(node.byte_range()) == Some(*name))
                        .unwrap();
                    assert!(
                        cache
                            .parameter_head(leaf, source.as_bytes(), &cancellation)
                            .unwrap()
                            .is_some()
                    );
                    let range = cache
                        .parameter_signature(leaf, source.as_bytes(), &cancellation)
                        .unwrap()
                        .unwrap();
                    assert_eq!(source.get(range), Some(*header), "{formals}: {name}");
                }
                assert!(
                    cache
                        .compound_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(
                    !cache
                        .simple_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert!(
                    !cache
                        .unit_parameters(parameters, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                let visits = cache.formal_visits;
                let slots = nodes(parameters, "repeat_pattern")
                    .into_iter()
                    .filter_map(|pattern| {
                        cache
                            .tuple_parameter(pattern, source.as_bytes(), &cancellation)
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(slots.len(), 1);
                assert_eq!(source.get(slots[0].clone()), Some(slot));
                for _ in 0..3 {
                    for leaf in nodes(parameters, "identifier") {
                        cache
                            .parameter_head(leaf, source.as_bytes(), &cancellation)
                            .unwrap();
                        cache
                            .parameter_signature(leaf, source.as_bytes(), &cancellation)
                            .unwrap();
                    }
                }
                assert_eq!(cache.formal_visits, visits);
                for unit in nodes(parameters, "unit") {
                    assert!(
                        cache
                            .unit_parameter(unit, source.as_bytes(), &cancellation)
                            .unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn named_tuple_proofs_keep_cache_work_and_cancellation_bounds() {
        let source = "module Tuples\nlet combine (left, (middle, right)) = left\n";
        let tree = parse(source, false);
        let parameters = nodes(tree.root_node(), "argument_patterns")[0];
        let cancellation = Cancellation::new();
        let mut storage_bounded = CaptureCache::new(1, true, usize::MAX);
        assert!(
            !storage_bounded
                .compound_parameters(parameters, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(storage_bounded.exhausted());
        let mut work_bounded = CaptureCache::new(usize::MAX, true, 1);
        assert!(
            !work_bounded
                .compound_parameters(parameters, source.as_bytes(), &cancellation)
                .unwrap()
        );
        assert!(work_bounded.exhausted());
        assert_eq!(work_bounded.formal_visits, 1);
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
        assert!(matches!(
            cache.compound_parameters(parameters, source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn named_tuple_proofs_reject_unaccounted_or_refutable_leaves() {
        for formals in [
            "(left, _)",
            "(left, Some right)",
            "(left, [right])",
            "((left, right): int * int)",
        ] {
            let source = format!("module Tuples\nlet combine {formals} = 0\n");
            let tree = parse(&source, false);
            let parameters = nodes(tree.root_node(), "argument_patterns")[0];
            let cancellation = Cancellation::new();
            let mut cache = CaptureCache::new(usize::MAX, true, usize::MAX);
            assert!(
                !cache
                    .reviewed_parameters(parameters, source.as_bytes(), &cancellation)
                    .unwrap(),
                "{formals}"
            );
            for leaf in nodes(parameters, "identifier") {
                assert!(
                    cache
                        .parameter_signature(leaf, source.as_bytes(), &cancellation)
                        .unwrap()
                        .is_none(),
                    "{formals}"
                );
            }
        }
    }

    #[test]
    fn optional_parameter_headers_keep_capture_and_cancellation_bounds() {
        let source = "module Headers\nlet carry (value: int) = value\n";
        let tree = parse(source, false);
        let leaf = nodes(tree.root_node(), "identifier")
            .into_iter()
            .find(|node| source.get(node.byte_range()) == Some("value"))
            .unwrap();
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(4, false, usize::MAX);
        assert!(
            !retain_capture(
                leaf,
                StructuralRole::Expression,
                source.as_bytes(),
                &cancellation,
                &mut bounded
            )
            .unwrap()
        );
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            retain_capture(
                leaf,
                StructuralRole::Expression,
                source.as_bytes(),
                &cancellation,
                &mut bounded
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn signature_contracts_keep_written_kind_labels() {
        let source = include_str!("../../tests/fixtures/fsharp/structural.fsi");
        let tree = parse(source, true);
        let cancellation = Cancellation::new();
        for (native, expected) in [
            ("value_definition", "fsharp.signature_val"),
            ("constructor_signature", "fsharp.signature_new"),
        ] {
            let captured = nodes(tree.root_node(), native);
            assert!(!captured.is_empty(), "{native}");
            for node in captured {
                assert_eq!(written_kind(node, &cancellation).unwrap(), Some(expected));
            }
        }
        let members = nodes(tree.root_node(), "member_signature");
        assert_eq!(members.len(), 2);
        assert_eq!(
            written_kind(members[0], &cancellation).unwrap(),
            Some("fsharp.signature_method")
        );
        assert_eq!(
            written_kind(members[1], &cancellation).unwrap(),
            Some("fsharp.signature_property")
        );
    }

    #[test]
    fn function_valued_property_does_not_acquire_a_method_label() {
        let source = "module Contracts\ntype I =\n    abstract member FunctionValue: (int -> int)\n    abstract member Curried: int -> int\n";
        let tree = parse(source, true);
        let cancellation = Cancellation::new();
        let members = nodes(tree.root_node(), "member_signature");
        assert_eq!(members.len(), 2);
        assert_eq!(
            written_kind(members[0], &cancellation).unwrap(),
            Some("fsharp.signature_property")
        );
        assert_eq!(
            written_kind(members[1], &cancellation).unwrap(),
            Some("fsharp.signature_method")
        );
    }

    #[test]
    fn unqualified_type_abbreviations_keep_representation_ambiguity() {
        let tree = parse(
            "module Types\ntype Ambiguous = string\ntype Qualified = System.String\n",
            false,
        );
        let cancellation = Cancellation::new();
        let types = nodes(tree.root_node(), "type_abbrev_defn");
        assert_eq!(types.len(), 2);
        assert_eq!(
            written_kind(types[0], &cancellation).unwrap(),
            Some("fsharp.ambiguous_type")
        );
        assert_eq!(
            written_kind(types[1], &cancellation).unwrap(),
            Some("fsharp.type_alias")
        );
    }

    #[test]
    fn parentheses_preserve_single_name_type_ambiguity_in_both_profiles() {
        let source = "module Types\ntype Parenthesized = (string)\ntype Nested = ((``QuotedCase``))\ntype Qualified = (System.String)\ntype Generic = (list<int>)\ntype 'a Identity = ('a)\n";
        for signature in [false, true] {
            let tree = parse(source, signature);
            let cancellation = Cancellation::new();
            let types = nodes(tree.root_node(), "type_abbrev_defn");
            assert_eq!(types.len(), 5);
            for (node, expected) in types.into_iter().zip([
                "fsharp.ambiguous_type",
                "fsharp.ambiguous_type",
                "fsharp.type_alias",
                "fsharp.type_alias",
                "fsharp.type_alias",
            ]) {
                assert_eq!(written_kind(node, &cancellation).unwrap(), Some(expected));
            }
        }
    }

    #[test]
    fn abstract_object_contracts_prove_interface_representation() {
        for signature in [false, true] {
            for body in [
                "abstract Read: int -> int",
                "abstract member Read: int -> int",
                "static abstract member Read: int -> int",
            ] {
                let source = format!("module Contracts\ntype Contract =\n    {body}\n");
                let tree = parse(&source, signature);
                let cancellation = Cancellation::new();
                let types = nodes(tree.root_node(), "anon_type_defn");
                assert_eq!(types.len(), 1);
                assert_eq!(
                    written_kind(types[0], &cancellation).unwrap(),
                    Some("fsharp.interface"),
                    "{signature}: {source}"
                );
            }
        }
    }

    #[test]
    fn implicit_object_concrete_bodies_prove_class_representation() {
        let cancellation = Cancellation::new();
        let mut observed = Vec::new();
        for declaration in [
            "type Utility =\n    static member Echo value = value",
            "type Holder =\n    member this.Value = 3",
            "type Store =\n    static member val Value = 1 with get, set",
            "type Stored =\n    static let value = 1",
            "type State =\n    val mutable Value: int",
            "type Base =\n    abstract Read: unit -> int\n    default this.Read() = 1",
            "type Wrapped = begin\n    interface Contract with\n        member this.Read() = 1\nend",
            "type Derived =\n    inherit Base(1)",
        ] {
            let source = format!("module Contracts\n{declaration}\n");
            let tree = parse(&source, false);
            let types = nodes(tree.root_node(), "anon_type_defn");
            assert_eq!(types.len(), 1, "{source}");
            observed.push((source, written_kind(types[0], &cancellation).unwrap()));
        }
        for (source, kind) in observed {
            assert_eq!(kind, Some("fsharp.class"), "{source}");
        }
    }

    #[test]
    fn implicit_object_member_contracts_keep_written_kinds() {
        for signature in [false, true] {
            for keyword in ["abstract", "abstract member"] {
                for newline in ["\n", "\r\n"] {
                    for (tail, expected_count) in [
                        (String::new(), 2),
                        (
                            format!(
                                "    {keyword} ``abstract``: int\n    {keyword} FunctionValue: (int -> int)\n    {keyword} Curried: int -> int\n"
                            ),
                            5,
                        ),
                    ] {
                        let source = format!("module ObjectTypes\ntype Contract =\n    {keyword} Read: unit -> int\n    {keyword} Value: int\n{tail}").replace('\n', newline);
                        let tree = parse(&source, signature);
                        let members = nodes(tree.root_node(), "member_signature");
                        assert_eq!(members.len(), expected_count);
                        for (member, (name, written, expected)) in members.into_iter().zip(
                            [
                                ("Read", "Read: unit -> int", "fsharp.signature_method"),
                                ("Value", "Value: int", "fsharp.signature_property"),
                                (
                                    "``abstract``",
                                    "``abstract``: int",
                                    "fsharp.signature_property",
                                ),
                                (
                                    "FunctionValue",
                                    "FunctionValue: (int -> int)",
                                    "fsharp.signature_property",
                                ),
                                ("Curried", "Curried: int -> int", "fsharp.signature_method"),
                            ]
                            .into_iter()
                            .take(expected_count),
                        ) {
                            let cancellation = Cancellation::new();
                            assert_eq!(
                                written_kind(member, &cancellation).unwrap(),
                                Some(expected),
                                "{signature} {keyword} {newline:?}: {}",
                                tree.root_node().to_sexp()
                            );
                            let start = source.find(written).unwrap();
                            assert_eq!(member.byte_range(), start..start + written.len());
                            assert_eq!(member.utf8_text(source.as_bytes()).unwrap(), written);
                            let selected = selected_name(member, &cancellation).unwrap().unwrap();
                            assert_eq!(selected.byte_range(), start..start + name.len());
                            assert_eq!(selected.utf8_text(source.as_bytes()).unwrap(), name);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn implicit_object_signature_fields_prove_class_representation() {
        let tree = parse("module Contracts\ntype State =\n    val Value: int\n", true);
        let types = nodes(tree.root_node(), "anon_type_defn");
        assert_eq!(types.len(), 1);
        assert_eq!(
            written_kind(types[0], &Cancellation::new()).unwrap(),
            Some("fsharp.class")
        );
    }

    #[test]
    fn implicit_object_signature_members_keep_representation_ambiguity() {
        for body in [
            "member Read: int -> int",
            "static member Read: int -> int",
            "inherit Contract",
        ] {
            let source = format!("module Contracts\ntype Opaque =\n    {body}\n");
            let tree = parse(&source, true);
            let types = nodes(tree.root_node(), "anon_type_defn");
            assert_eq!(types.len(), 1);
            assert_eq!(
                written_kind(types[0], &Cancellation::new()).unwrap(),
                Some("fsharp.ambiguous_type"),
                "{source}"
            );
        }
    }

    #[test]
    fn implicit_object_type_attributes_keep_local_ambiguity() {
        for (source, expected) in [
            (
                "module Contracts\n[<Unknown>]\ntype Marked =\n    member this.Value = 1\nand Plain =\n    member this.Value = 2\n",
                ["fsharp.ambiguous_type", "fsharp.class"],
            ),
            (
                "module Contracts\ntype Plain =\n    member this.Value = 1\nand [<Unknown>] Marked =\n    member this.Value = 2\n",
                ["fsharp.class", "fsharp.ambiguous_type"],
            ),
            (
                "module Contracts\ntype [<Unknown>] Marked =\n    abstract Read: int -> int\nand Plain =\n    abstract Read: int -> int\n",
                ["fsharp.ambiguous_type", "fsharp.interface"],
            ),
        ] {
            let tree = parse(source, false);
            let types = nodes(tree.root_node(), "anon_type_defn");
            assert_eq!(types.len(), 2);
            for (node, expected) in types.into_iter().zip(expected) {
                assert_eq!(
                    written_kind(node, &Cancellation::new()).unwrap(),
                    Some(expected),
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn implicit_object_unreviewed_bodies_keep_representation_ambiguity() {
        for source in [
            "module Contracts\ntype Unknown = // no owned type body\n",
            "module Contracts\ntype Conditional =\n#if FEATURE\n    member this.Value = 1\n#endif\n",
            "module Contracts\n[<Unknown>]\ntype Marked =\n    abstract Read: int -> int\n",
        ] {
            let tree = parse(source, false);
            let types = nodes(tree.root_node(), "anon_type_defn");
            assert_eq!(types.len(), 1);
            assert_eq!(
                written_kind(types[0], &Cancellation::new()).unwrap(),
                Some("fsharp.ambiguous_type"),
                "{source}"
            );
        }
    }

    #[test]
    fn direct_signature_constructor_wrapper_proves_class_representation() {
        let tree = parse(
            include_str!("../../tests/fixtures/fsharp/structural.fsi"),
            true,
        );
        let cancellation = Cancellation::new();
        let types = nodes(tree.root_node(), "anon_type_defn");
        assert_eq!(types.len(), 1);
        assert_eq!(
            written_kind(types[0], &cancellation).unwrap(),
            Some("fsharp.class")
        );
    }

    #[test]
    fn query_packs_compile_only_against_their_matching_factory() {
        for (language, source, implementation) in [
            (
                tree_sitter_fsharp::LANGUAGE_FSHARP,
                include_str!("../../queries/fsharp.scm"),
                true,
            ),
            (
                tree_sitter_fsharp::LANGUAGE_SIGNATURE,
                include_str!("../../queries/fsharp_signature.scm"),
                false,
            ),
        ] {
            let query = tree_sitter::Query::new(&language.into(), source).unwrap();
            let mut captures = query.capture_names().to_vec();
            captures.sort_unstable();
            let mut expected = vec![
                "declaration",
                "definition",
                "import",
                "module",
                "root",
                "scope",
                "signature",
            ];
            if implementation {
                expected.extend(["call", "expression", "reference"]);
                expected.sort_unstable();
            } else {
                expected.push("expression");
                expected.sort_unstable();
            }
            assert_eq!(captures, expected);
        }
    }

    #[test]
    fn match_branches_keep_exact_binders_markers_and_outer_scrutinee_reads() {
        let cancellation = Cancellation::new();
        for pattern in ["item", "δ", "_", "(item)", "(item, (_, extra))"] {
            let source = format!(
                "module Branches\nlet choose input = match input with | {pattern} -> input\n"
            );
            let tree = parse(&source, false);
            let branch = nodes(tree.root_node(), "rule")[0];
            let (pattern_node, arrow, body) =
                match_branch_parts(branch, &cancellation).unwrap().unwrap();
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                cache
                    .match_branch(branch, source.as_bytes(), &cancellation)
                    .unwrap(),
                "{pattern}"
            );
            for (node, label, span) in [
                (branch, "fsharp.match_branch_body", body.byte_range()),
                (
                    arrow,
                    "fsharp.match_branch_header",
                    branch.start_byte()..arrow.end_byte(),
                ),
                (
                    pattern_node,
                    "fsharp.match_branch_pattern",
                    pattern_node.byte_range(),
                ),
            ] {
                assert_eq!(
                    cache
                        .match_branch_capture(node, source.as_bytes(), &cancellation)
                        .unwrap(),
                    Some((label, span)),
                    "{pattern}"
                );
            }
            let identifiers = nodes(branch, "identifier");
            for name in identifiers
                .iter()
                .copied()
                .filter(|node| node.end_byte() <= pattern_node.end_byte())
            {
                assert!(
                    cache
                        .match_binder(name, source.as_bytes(), &cancellation)
                        .unwrap()
                );
                assert_eq!(
                    syntax(
                        name,
                        StructuralRole::Declaration,
                        source.as_bytes(),
                        &cancellation,
                        &mut cache
                    )
                    .unwrap(),
                    Some("fsharp.variable")
                );
                assert!(
                    !cache
                        .simple_body_read(name, source.as_bytes(), &cancellation)
                        .unwrap()
                );
            }
            let reads = nodes(tree.root_node(), "identifier")
                .into_iter()
                .filter(|node| {
                    cache
                        .simple_body_read(*node, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .count();
            assert_eq!(reads, 2, "{pattern}");
        }
    }

    #[test]
    fn unreviewed_match_arms_keep_scope_but_only_scrutinee_reads() {
        let cancellation = Cancellation::new();
        for pattern in [
            "Some item",
            "Item",
            "``item``",
            "(item: int)",
            "[item]",
            "item when item > 0",
        ] {
            let source = format!(
                "module Branches\nlet choose input = match input with | {pattern} -> input\n"
            );
            let tree = parse(&source, false);
            let branch = nodes(tree.root_node(), "rule")[0];
            let mut cache = CaptureCache::new(1024, true, 1024);
            assert!(
                retain_capture(
                    branch,
                    StructuralRole::Scope,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap(),
                "{pattern}"
            );
            assert!(
                !cache
                    .match_branch(branch, source.as_bytes(), &cancellation)
                    .unwrap(),
                "{pattern}"
            );
            assert_eq!(
                cache
                    .match_branch_capture(branch, source.as_bytes(), &cancellation)
                    .unwrap(),
                None
            );
            let reads = nodes(tree.root_node(), "identifier")
                .into_iter()
                .filter(|node| {
                    cache
                        .simple_body_read(*node, source.as_bytes(), &cancellation)
                        .unwrap()
                })
                .count();
            assert_eq!(reads, 1, "{pattern}");
        }
    }

    #[test]
    fn match_proof_caps_and_cancellation_preserve_closed_scopes() {
        let source =
            "module Branches\nlet choose input = match input with | (item, extra) -> item\n";
        let tree = parse(source, false);
        let branch = nodes(tree.root_node(), "rule")[0];
        let cancellation = Cancellation::new();
        for (entries, work) in [(0, 1024), (2, 1024), (1024, 0)] {
            let mut cache = CaptureCache::new(entries, true, work);
            assert!(
                retain_capture(
                    branch,
                    StructuralRole::Scope,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache
                )
                .unwrap()
            );
            assert!(
                !cache
                    .match_branch(branch, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert!(cache.exhausted());
            assert_eq!(
                cache
                    .match_branch_capture(branch, source.as_bytes(), &cancellation)
                    .unwrap(),
                None
            );
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(1024, true, 1024);
        assert!(matches!(
            cache.match_branch(branch, source.as_bytes(), &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            cache.match_expression(nodes(tree.root_node(), "match_expression")[0], &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn destructuring_is_not_a_single_value_declaration() {
        let tree = parse("module Patterns\nlet (left, right) = (1, 2)\n", false);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(usize::MAX, false, usize::MAX);
        let heads = nodes(tree.root_node(), "value_declaration_left");
        assert_eq!(heads.len(), 1);
        assert!(
            retain_capture(
                heads[0],
                StructuralRole::Declaration,
                b"",
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        assert_eq!(
            syntax(
                heads[0],
                StructuralRole::Declaration,
                b"",
                &cancellation,
                &mut cache
            )
            .unwrap(),
            Some("fsharp.unsupported_binding")
        );
        assert!(
            !retain_capture(
                heads[0],
                StructuralRole::Definition,
                b"",
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
    }
}
