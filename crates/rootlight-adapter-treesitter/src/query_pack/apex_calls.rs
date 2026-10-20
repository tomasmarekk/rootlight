//! Native Apex instance-call proofs share the existing owner/body work budget.
//! A private target is statically bound; a virtual target supplies only a
//! written base candidate and cannot establish the runtime receiver's class.

use super::{CaptureCache, boundary, clean, field, member_owner, written_name};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

#[derive(Clone, Copy)]
pub(super) enum CallKind {
    Private,
    Virtual,
}

#[derive(Default)]
struct Modifiers {
    private: bool,
    virtual_method: bool,
    abstract_method: bool,
    overridden: bool,
    static_method: bool,
}

/// Certifies a unique written zero-argument target in the caller's own class.
/// Allocation/cancellation failures propagate; exhausted work remains explicit.
pub(super) fn classify(
    invocation: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<CallKind>, AdapterError> {
    cancellation.check()?;
    if let Some(kind) = cache.calls.get(&invocation.id()) {
        return Ok(*kind);
    }
    if cache.entries() >= cache.maximum_entries {
        cache.exhausted = true;
        return Ok(None);
    }
    let proof = classify_uncached(invocation, source, cancellation, cache)?;
    if cache.exhausted || cache.entries() >= cache.maximum_entries {
        cache.exhausted = true;
        return Ok(None);
    }
    cache
        .calls
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    cache.calls.insert(invocation.id(), proof);
    Ok(proof)
}

fn classify_uncached(
    invocation: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<CallKind>, AdapterError> {
    if invocation.kind() != "method_invocation" || !clean(invocation, source) {
        return Ok(None);
    }
    let mut ancestor = invocation;
    while let Some(parent) = ancestor.parent() {
        if !cache.visit_body_node(cancellation)? {
            return Ok(None);
        }
        ancestor = parent;
    }
    if !clean(ancestor, source) || ancestor.start_byte() != 0 || ancestor.end_byte() != source.len()
    {
        return Ok(None);
    }
    let Some(name) = field(invocation, "name").filter(|node| written_name(*node, source)) else {
        return Ok(None);
    };
    let spelling = &source[name.byte_range()];
    if !spelling.is_ascii() || field(invocation, "type_arguments").is_some() {
        return Ok(None);
    }
    if let Some(receiver) = field(invocation, "object")
        && (receiver.kind() != "this"
            || !source[receiver.byte_range()].eq_ignore_ascii_case(b"this"))
    {
        return Ok(None);
    }
    // Safe navigation is not ordinary `this` dispatch, even with a zero-arg name.
    let mut cursor = invocation.walk();
    for child in invocation.named_children(&mut cursor) {
        if !cache.visit_body_node(cancellation)? || child.kind() == "safe_navigation_operator" {
            return Ok(None);
        }
    }
    let Some(arguments) = field(invocation, "arguments") else {
        return Ok(None);
    };
    if !empty_list(arguments, source, cancellation, cache)? {
        return Ok(None);
    }
    let mut owner = invocation.parent();
    let caller = loop {
        let Some(node) = owner else { return Ok(None) };
        if !cache.visit_body_node(cancellation)? {
            return Ok(None);
        }
        if node.kind() == "method_declaration" {
            break node;
        }
        if matches!(
            node.kind(),
            "constructor_declaration" | "class_declaration" | "interface_declaration"
        ) {
            return Ok(None);
        }
        owner = node.parent();
    };
    if cache.owner(caller, source, cancellation)?.is_none()
        || modifiers(caller, source, cancellation, cache)?.static_method
        || conflicting_binder(caller, spelling, source, cancellation, cache)?
    {
        return Ok(None);
    }
    let Some(class) = member_owner(caller).filter(|node| node.kind() == "class_declaration") else {
        return Ok(None);
    };
    if cache.owner(class, source, cancellation)?.is_none() {
        return Ok(None);
    }
    let Some(body) = field(class, "body") else {
        return Ok(None);
    };
    let mut target = None;
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if !cache.visit_body_node(cancellation)? {
            return Ok(None);
        }
        if member.kind() == "field_declaration"
            && conflicting_binder(member, spelling, source, cancellation, cache)?
        {
            return Ok(None);
        }
        if member.kind() == "method_declaration"
            && field(member, "name")
                .is_some_and(|name| source[name.byte_range()].eq_ignore_ascii_case(spelling))
        {
            if target.is_some() {
                return Ok(None);
            }
            target = Some(member);
        }
    }
    let Some(target) = target else {
        return Ok(None);
    };
    if cache.owner(target, source, cancellation)?.is_none()
        || field(target, "type_parameters").is_some()
        || field(target, "body").is_none()
    {
        return Ok(None);
    }
    let Some(parameters) = field(target, "parameters") else {
        return Ok(None);
    };
    if !empty_list(parameters, source, cancellation, cache)? {
        return Ok(None);
    }
    let modifiers = modifiers(target, source, cancellation, cache)?;
    if modifiers.static_method || modifiers.abstract_method {
        return Ok(None);
    }
    if modifiers.private && !modifiers.virtual_method && !modifiers.overridden {
        Ok(Some(CallKind::Private))
    } else if !modifiers.private && modifiers.virtual_method && !modifiers.overridden {
        Ok(Some(CallKind::Virtual))
    } else {
        Ok(None)
    }
}

fn empty_list(
    list: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    if !boundary(list, source, b"(", b")", cancellation)? {
        return Ok(false);
    }
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if !cache.visit_body_node(cancellation)? || !child.is_extra() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn modifiers(
    method: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Modifiers, AdapterError> {
    let mut result = Modifiers::default();
    let mut cursor = method.walk();
    for child in method.named_children(&mut cursor) {
        if !cache.visit_body_node(cancellation)? {
            return Ok(result);
        }
        if child.kind() != "modifiers" {
            continue;
        }
        let mut modifiers = child.walk();
        for modifier in child.named_children(&mut modifiers) {
            if !cache.visit_body_node(cancellation)? {
                return Ok(result);
            }
            let value = source[modifier.byte_range()].trim_ascii();
            result.private |= value.eq_ignore_ascii_case(b"private");
            result.virtual_method |= value.eq_ignore_ascii_case(b"virtual");
            result.abstract_method |= value.eq_ignore_ascii_case(b"abstract");
            result.overridden |= value.eq_ignore_ascii_case(b"override");
            result.static_method |= value.eq_ignore_ascii_case(b"static");
        }
    }
    Ok(result)
}

fn conflicting_binder(
    root: Node<'_>,
    spelling: &[u8],
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut cursor = root.walk();
    loop {
        if !cache.visit_body_node(cancellation)? {
            return Ok(true);
        }
        let node = cursor.node();
        if matches!(
            node.kind(),
            "formal_parameter"
                | "variable_declarator"
                | "catch_formal_parameter"
                | "enhanced_for_statement"
                | "when_sobject_type"
        ) && field(node, "name")
            .is_some_and(|name| source[name.byte_range()].eq_ignore_ascii_case(spelling))
        {
            return Ok(true);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_proof_shares_owner_capacity_body_work_and_cancellation() {
        let source =
            b"public class Handler { public void run() { target(); } private void target() {} }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Apex))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let start = source
            .windows(b"target();".len())
            .position(|part| part == b"target();")
            .unwrap();
        let node = tree
            .root_node()
            .named_descendant_for_byte_range(start, start + b"target()".len())
            .unwrap();
        assert_eq!(node.kind(), "method_invocation");
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(64, 1024);
        assert!(matches!(
            classify(node, source, &cancellation, &mut cache).unwrap(),
            Some(CallKind::Private)
        ));
        let remaining = cache.remaining_body_nodes;
        assert!(matches!(
            classify(node, source, &cancellation, &mut cache).unwrap(),
            Some(CallKind::Private)
        ));
        assert_eq!(cache.remaining_body_nodes, remaining);
        let mut no_body = CaptureCache::new(64, 0);
        assert!(
            classify(node, source, &cancellation, &mut no_body)
                .unwrap()
                .is_none()
        );
        assert!(no_body.exhausted());
        let mut no_entries = CaptureCache::new(0, 1024);
        assert!(
            classify(node, source, &cancellation, &mut no_entries)
                .unwrap()
                .is_none()
        );
        assert!(no_entries.exhausted());
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            classify(node, source, &cancellation, &mut cache),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }
}
