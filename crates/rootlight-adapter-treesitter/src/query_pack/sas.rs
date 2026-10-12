//! SAS captures preserve written native declarations without runtime expansion.
//! Owner proofs are bounded to one immutable query scan; malformed ancestors
//! cannot reparent formals, and opaque bodies do not establish call bindings.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    extent: Range<usize>,
    header: Range<usize>,
    outputs: Outputs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outputs {
    None,
    Single,
    Multiple,
}

/// Cached proofs share the caller's existing capture and body-node bounds.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
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
        let proof = owner_proof(node, source, cancellation)?;
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

fn child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == kind && !child.is_extra() {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn named_owner(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "fcmp_function_definition" | "fcmp_subroutine_definition" | "macro_definition"
    )
}

fn owner_kind(node: Node<'_>) -> bool {
    named_owner(node) || matches!(node.kind(), "data_step" | "proc_step" | "proc_sql_step")
}

fn name_node(node: Node<'_>) -> Option<Node<'_>> {
    if named_owner(node) {
        field(field(node, "header")?, "name")
    } else if matches!(node.kind(), "fcmp_parameter" | "macro_parameter") {
        field(node, "name")
    } else if node.kind() == "dataset_name" {
        Some(node)
    } else {
        None
    }
}

fn written_name<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    if node.is_missing() || node.has_error() {
        return None;
    }
    let text = std::str::from_utf8(source.get(node.byte_range())?).ok()?;
    (structural_captured_name_for_language("sas", text, text.len()).as_deref() == Some(text))
        .then_some(text)
}

fn semicolon(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing()
        && !node.has_error()
        && source
            .get(node.byte_range())
            .is_some_and(|bytes| bytes.last() == Some(&b';'))
}

fn keyword(node: Node<'_>, source: &[u8], word: &[u8]) -> bool {
    source
        .get(node.byte_range())
        .and_then(|bytes| bytes.get(..word.len()))
        .is_some_and(|bytes| bytes.eq_ignore_ascii_case(word))
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if node.is_missing() || node.has_error() || source.get(node.byte_range()).is_none() {
        return Ok(None);
    }
    let (header, end, opening, closing) = match node.kind() {
        "fcmp_function_definition" => (
            field(node, "header"),
            field(node, "end"),
            b"function".as_slice(),
            b"endsub".as_slice(),
        ),
        "fcmp_subroutine_definition" => (
            field(node, "header"),
            field(node, "end"),
            b"subroutine".as_slice(),
            b"endsub".as_slice(),
        ),
        "macro_definition" => (
            field(node, "header"),
            field(node, "end"),
            b"%macro".as_slice(),
            b"%mend".as_slice(),
        ),
        "data_step" => (
            field(node, "header"),
            field(node, "end"),
            b"data".as_slice(),
            b"run".as_slice(),
        ),
        "proc_step" => (
            child(node, "proc_step_header", cancellation)?,
            child(node, "run_or_quit_statement", cancellation)?,
            b"proc".as_slice(),
            b"".as_slice(),
        ),
        "proc_sql_step" => (
            child(node, "proc_sql_header", cancellation)?,
            child(node, "run_or_quit_statement", cancellation)?,
            b"proc".as_slice(),
            b"".as_slice(),
        ),
        _ => return Ok(None),
    };
    let (Some(header), Some(end)) = (header, end) else {
        return Ok(None);
    };
    if header.start_byte() != node.start_byte()
        || end.end_byte() != node.end_byte()
        || header.end_byte() > end.start_byte()
        || !semicolon(header, source)
        || !semicolon(end, source)
        || !keyword(header, source, opening)
        || (!closing.is_empty() && !keyword(end, source, closing))
        || (closing.is_empty() && !keyword(end, source, b"run") && !keyword(end, source, b"quit"))
    {
        return Ok(None);
    }
    if named_owner(node) {
        let Some(name) = name_node(node).and_then(|name| written_name(name, source)) else {
            return Ok(None);
        };
        if node.kind() == "macro_definition"
            && let Some(closing) = field(end, "name")
            && written_name(closing, source)
                .is_none_or(|closing| !closing.eq_ignore_ascii_case(name))
        {
            return Ok(None);
        }
    }
    let outputs = if node.kind() == "data_step" {
        let mut cursor = header.walk();
        let mut outputs = header.children_by_field_name("output", &mut cursor);
        cancellation.check()?;
        match (outputs.next(), outputs.next()) {
            (None, _) => Outputs::None,
            (Some(_), None) => Outputs::Single,
            (Some(_), Some(_)) => Outputs::Multiple,
        }
    } else {
        Outputs::None
    };
    Ok(Some(OwnerProof {
        extent: node.byte_range(),
        header: header.byte_range(),
        outputs,
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
        if owner.kind() == "ERROR"
            || owner.is_missing()
            || (matches!(owner.kind(), "macro_if_statement" | "macro_do_statement")
                && owner.has_error())
        {
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
        current = owner.parent();
    }
    Ok(true)
}

fn output_owner<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    let Some(header) = node
        .parent()
        .filter(|parent| parent.kind() == "data_step_header")
    else {
        return Ok(None);
    };
    // Every direct dataset_name child is an output in the reviewed header rule.
    // Nested dataset references are not outputs; rescanning siblings per capture
    // would turn a multiple-output header into quadratic admission work.
    Ok(header
        .parent()
        .filter(|parent| parent.kind() == "data_step" && field(*parent, "header") == Some(header)))
}

fn static_output(node: Node<'_>, source: &[u8]) -> bool {
    written_name(node, source).is_some_and(|name| {
        !["_null_", "_data_", "_last_"]
            .iter()
            .any(|special| name.eq_ignore_ascii_case(special))
    })
}

fn valid_definition(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let Some(name) = name_node(node) else {
        return Ok(false);
    };
    if written_name(name, source).is_none()
        || !valid_owner_chain(node, source, cancellation, cache)?
    {
        return Ok(false);
    }
    if node.kind() == "dataset_name" {
        return Ok(output_owner(node, cancellation)?.is_some() && static_output(node, source));
    }
    if matches!(node.kind(), "fcmp_parameter" | "macro_parameter") {
        let parameters_kind = if node.kind() == "fcmp_parameter" {
            "fcmp_parameters"
        } else {
            "macro_parameters"
        };
        let Some(parameters) = node
            .parent()
            .filter(|parent| parent.kind() == parameters_kind)
        else {
            return Ok(false);
        };
        let Some(header) = parameters
            .parent()
            .filter(|header| field(*header, "parameters") == Some(parameters))
        else {
            return Ok(false);
        };
        return Ok(header
            .parent()
            .is_some_and(|owner| named_owner(owner) && field(owner, "header") == Some(header))
            && !node.has_error());
    }
    Ok(named_owner(node))
}

/// Admits only native field-owned identities and unresolved physical references.
/// Cancellation and bounded owner-cache failures propagate to the query scan.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "program" {
        return Ok(true);
    }
    if role == StructuralRole::Expression {
        if node.kind() == "dataset_name" {
            return Ok(output_owner(node, cancellation)?.is_some() && !static_output(node, source));
        }
        return Ok(true);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        if !valid_definition(node, source, cancellation, cache)? {
            return Ok(false);
        }
        if role == StructuralRole::Signature && node.kind() == "dataset_name" {
            let Some(owner) = output_owner(node, cancellation)? else {
                return Ok(false);
            };
            return Ok(cache
                .owner(owner, source, cancellation)?
                .is_some_and(|proof| proof.outputs == Outputs::Single));
        }
        return Ok(true);
    }
    if !valid_owner_chain(node, source, cancellation, cache)? {
        return Ok(false);
    }
    if role == StructuralRole::Reference {
        if node.is_missing() || node.has_error() {
            return Ok(false);
        }
        return Ok(node.kind() != "dataset_name" || output_owner(node, cancellation)?.is_none());
    }
    Ok(!node.is_missing())
}

/// Projects admitted native captures to exact source spans and syntax roles.
/// No SAS reference role establishes a call, binding, or runtime dataset target.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    if matches!(
        role,
        StructuralRole::Call | StructuralRole::CallName | StructuralRole::ScopedCall
    ) {
        return Err(query_failure("query-sas-unproven-call"));
    }
    let syntax = match role {
        _ if node.kind() == "program" => "sas.file",
        StructuralRole::Expression
            if owner_kind(node) && !valid_owner_chain(node, source, cancellation, cache)? =>
        {
            "sas.unsupported_owner"
        }
        StructuralRole::Expression if node.kind() == "macro_definition" => "sas.macro_expansion",
        StructuralRole::Expression if named_owner(node) => "sas.fcmp_semantics",
        StructuralRole::Expression if node.kind() == "data_step" => {
            if cache
                .owner(node, source, cancellation)?
                .is_some_and(|proof| proof.outputs == Outputs::Multiple)
            {
                "sas.shared_data_header"
            } else {
                "sas.data_semantics"
            }
        }
        StructuralRole::Expression if node.kind() == "dataset_name" => "sas.dynamic_dataset",
        StructuralRole::Expression if matches!(node.kind(), "proc_step" | "proc_sql_step") => {
            "sas.proc_semantics"
        }
        StructuralRole::Expression
            if matches!(node.kind(), "macro_if_statement" | "macro_do_statement") =>
        {
            "sas.conditional_selection"
        }
        StructuralRole::Expression if node.kind() == "include_statement" => "sas.runtime_include",
        StructuralRole::Expression => "sas.unknown_statement",
        StructuralRole::Reference
            if matches!(node.kind(), "macro_call" | "macro_call_statement") =>
        {
            "sas.macro_call"
        }
        StructuralRole::Reference if node.kind() == "macro_variable_ref" => "sas.macro_variable",
        StructuralRole::Reference => "sas.dataset",
        StructuralRole::Comment => "sas.comment",
        StructuralRole::StringLiteral => "sas.string",
        _ => match node.kind() {
            "fcmp_function_definition" => "sas.fcmp_function",
            "fcmp_subroutine_definition" => "sas.fcmp_subroutine",
            "macro_definition" => "sas.macro",
            "fcmp_parameter" => "sas.fcmp_parameter",
            "macro_parameter" => "sas.macro_parameter",
            "dataset_name" => "sas.data_target",
            "data_step" => "sas.data",
            "proc_step" | "proc_sql_step" => "sas.proc",
            "macro_if_statement" | "macro_do_statement" => "sas.conditional",
            _ => return Err(query_failure("query-sas-node-kind")),
        },
    };
    let mut depth_node = node;
    let range = match role {
        StructuralRole::Definition => {
            if node.kind() == "dataset_name"
                && let Some(owner) = output_owner(node, cancellation)?
                && cache
                    .owner(owner, source, cancellation)?
                    .is_some_and(|proof| proof.outputs == Outputs::Single)
            {
                depth_node = owner;
            }
            name_node(node)
                .ok_or_else(|| query_failure("query-sas-definition"))?
                .byte_range()
        }
        StructuralRole::Declaration | StructuralRole::Signature
            if node.kind() == "dataset_name" =>
        {
            let owner = output_owner(node, cancellation)?
                .ok_or_else(|| query_failure("query-sas-output-owner"))?;
            let proof = cache
                .owner(owner, source, cancellation)?
                .ok_or_else(|| query_failure("query-sas-output-header"))?;
            if proof.outputs == Outputs::Single {
                depth_node = owner;
                if role == StructuralRole::Signature {
                    proof.header
                } else {
                    proof.extent
                }
            } else {
                node.byte_range()
            }
        }
        StructuralRole::Signature if named_owner(node) => {
            cache
                .owner(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-sas-header"))?
                .header
        }
        StructuralRole::Reference
            if matches!(node.kind(), "macro_call" | "macro_call_statement") =>
        {
            field(node, "name")
                .ok_or_else(|| query_failure("query-sas-macro-call"))?
                .byte_range()
        }
        _ => node.byte_range(),
    };
    let mut depth = native_depth(depth_node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "program" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-sas-depth"))?;
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

    use super::*;

    #[test]
    fn owner_cache_reuses_proofs_obeys_capture_bound_and_checks_cancellation() {
        let source = b"function First(Value); return Value; endsub;\nfunction Second(Input); return Input; endsub;\n";
        let language = tree_sitter_sas::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let cancellation = Cancellation::new();
        let mut cursor = tree.root_node().walk();
        let owners = tree
            .root_node()
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "fcmp_function_definition")
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 2);
        let mut cache = CaptureCache::new(1);
        for _ in 0..20 {
            assert!(valid_owner_chain(owners[0], source, &cancellation, &mut cache).unwrap());
            assert_eq!(cache.owners.len(), 1);
        }
        assert!(!valid_owner_chain(owners[1], source, &cancellation, &mut cache).unwrap());
        assert!(cache.exhausted());
        assert_eq!(cache.owners.len(), 1);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(valid_owner_chain(owners[0], source, &cancellation, &mut cache).is_err());
    }
}
