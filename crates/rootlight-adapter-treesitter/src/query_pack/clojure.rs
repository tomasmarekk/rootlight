//! Clojure declarations and direct call heads require source-form context.
//! Collections evaluate elements; reader data, macros and uncertain binders stay gaps.

use super::{QueryCandidate, StructuralRole, native_depth};
use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use std::{collections::HashMap, ops::Range};
use tree_sitter::Node;

#[derive(Clone)]
struct Owner {
    name: Range<usize>,
    header: Range<usize>,
    extent: Range<usize>,
    namespace: bool,
    body_start: usize,
}

pub(super) struct CaptureCache {
    remaining: usize,
    exhausted: bool,
    core: Option<bool>,
    owners: HashMap<usize, Option<Owner>>,
}
impl CaptureCache {
    pub(super) fn new(remaining: usize) -> Self {
        Self {
            remaining,
            exhausted: false,
            core: None,
            owners: HashMap::new(),
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(left) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = left;
        Ok(true)
    }
}
fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}
fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}
fn symbol(node: Node<'_>, source: &[u8]) -> Option<Range<usize>> {
    if node.kind() != "sym_lit" || !clean(node, source) {
        return None;
    }
    let leaf = node
        .child_count()
        .checked_sub(1)
        .and_then(|last| u32::try_from(last).ok())
        .and_then(|last| node.child(last))
        .unwrap_or(node);
    if matches!(leaf.kind(), "meta_lit" | "old_meta_lit") || leaf.end_byte() != node.end_byte() {
        return None;
    }
    let range = leaf.byte_range();
    let name = std::str::from_utf8(source.get(range.clone())?).ok()?;
    structural_captured_name_for_language("clojure", name, source.len())?;
    Some(range)
}
fn name<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(symbol(node, source)?)?).ok()
}
fn value<'t>(
    node: Node<'t>,
    index: usize,
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'t>>, AdapterError> {
    let mut cursor = node.walk();
    for (i, child) in node
        .children_by_field_name("value", &mut cursor)
        .enumerate()
    {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        if i == index {
            return Ok(Some(child));
        }
    }
    Ok(None)
}
fn head<'a>(
    node: Node<'_>,
    source: &'a [u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<&'a str>, AdapterError> {
    Ok(value(node, 0, cancel, cache)?.and_then(|n| name(n, source)))
}
fn root(mut node: Node<'_>) -> Node<'_> {
    while let Some(parent) = node.parent() {
        node = parent;
    }
    node
}
fn top(node: Node<'_>) -> bool {
    node.parent().is_some_and(|n| n.kind() == "source")
}
fn core_visible(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    if let Some(found) = cache.core {
        return Ok(found);
    }
    let document = root(node);
    let mut cursor = document.walk();
    let mut visible = true;
    for form in document.named_children(&mut cursor) {
        if !cache.visit(cancel)? {
            return Ok(false);
        }
        if form.kind() != "list_lit" {
            continue;
        }
        let op = head(form, source, cancel, cache)?;
        if matches!(op, Some("def" | "defonce" | "defmacro" | "defn" | "defn-"))
            && value(form, 1, cancel, cache)?
                .and_then(|n| name(n, source))
                .is_some_and(|n| matches!(n, "ns" | "defn" | "defn-"))
        {
            visible = false;
        }
        if op == Some("ns") {
            let mut nodes = Vec::new();
            nodes
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            nodes.push(form);
            while let Some(current) = nodes.pop() {
                if !cache.visit(cancel)? {
                    return Ok(false);
                }
                if current != form
                    && matches!(
                        text(current, source),
                        Some(":refer-clojure" | ":use" | ":all" | "defn" | "defn-")
                    )
                {
                    visible = false;
                }
                let mut c = current.walk();
                for child in current.named_children(&mut c) {
                    if !cache.visit(cancel)? {
                        return Ok(false);
                    }
                    nodes
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    nodes.push(child);
                }
            }
        }
    }
    cache.core = Some(visible);
    Ok(visible)
}
fn namespace_barrier(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut pending = Vec::new();
    pending
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    pending.push(node);
    while let Some(current) = pending.pop() {
        if !cache.visit(cancel)? {
            return Ok(true);
        }
        if data(current) {
            continue;
        }
        if current.kind() == "list_lit" {
            match head(current, source, cancel, cache)? {
                Some("ns" | "in-ns") => return Ok(true),
                Some("quote" | "comment" | "defn" | "defn-" | "defmacro" | "fn" | "fn*") => {
                    continue;
                }
                _ => {}
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            if !cache.visit(cancel)? {
                return Ok(true);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(child);
        }
    }
    Ok(false)
}

fn namespace_known(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        if !cache.visit(cancel)? {
            return Ok(false);
        }
        if previous.kind() == "list_lit" {
            match head(previous, source, cancel, cache)? {
                Some("in-ns") => return Ok(false),
                Some("ns") => {
                    return Ok(clean(previous, source)
                        && value(previous, 1, cancel, cache)?
                            .and_then(|n| name(n, source))
                            .is_some());
                }
                _ => {}
            }
        }
        if namespace_barrier(previous, source, cancel, cache)? {
            return Ok(false);
        }
        sibling = previous.prev_named_sibling();
    }
    Ok(true)
}
fn owner(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Owner>, AdapterError> {
    if let Some(result) = cache.owners.get(&node.id()) {
        return Ok(result.clone());
    }
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let result = owner_uncached(node, source, cancel, cache)?;
    cache
        .owners
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    cache.owners.insert(node.id(), result.clone());
    Ok(result)
}
fn owner_uncached(
    node: Node<'_>,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Owner>, AdapterError> {
    if node.kind() != "list_lit"
        || !top(node)
        || !clean(node, source)
        || !core_visible(node, source, cancel, cache)?
    {
        return Ok(None);
    }
    let op = head(node, source, cancel, cache)?;
    if !matches!(op, Some("ns" | "defn" | "defn-")) {
        return Ok(None);
    }
    let Some(name_node) = value(node, 1, cancel, cache)? else {
        return Ok(None);
    };
    let Some(name_range) = symbol(name_node, source) else {
        return Ok(None);
    };
    if name(name_node, source).is_none_or(|n| n.contains('/')) {
        return Ok(None);
    }
    if op == Some("ns") {
        let mut end = source.len();
        let mut sibling = node.next_named_sibling();
        while let Some(next) = sibling {
            if !cache.visit(cancel)? {
                return Ok(None);
            }
            if namespace_barrier(next, source, cancel, cache)? {
                end = next.start_byte();
                break;
            }
            sibling = next.next_named_sibling();
        }
        return Ok(Some(Owner {
            name: name_range,
            header: node.byte_range(),
            extent: node.start_byte()..end,
            namespace: true,
            body_start: node.end_byte(),
        }));
    }
    if !namespace_known(node, source, cancel, cache)? {
        return Ok(None);
    }
    let mut argument = 2;
    while value(node, argument, cancel, cache)?
        .is_some_and(|n| matches!(n.kind(), "str_lit" | "map_lit"))
    {
        argument += 1;
    }
    let Some(args) = value(node, argument, cancel, cache)? else {
        return Ok(None);
    };
    let body_start = if args.kind() == "vec_lit" {
        args.end_byte()
    } else if args.kind() == "list_lit"
        && value(args, 0, cancel, cache)?.is_some_and(|n| n.kind() == "vec_lit")
    {
        name_range.end
    } else {
        return Ok(None);
    };
    Ok(Some(Owner {
        name: name_range.clone(),
        header: node.start_byte()..body_start,
        extent: node.byte_range(),
        namespace: false,
        body_start,
    }))
}
fn data(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "quoting_lit"
            | "syn_quoting_lit"
            | "var_quoting_lit"
            | "unquoting_lit"
            | "unquote_splicing_lit"
            | "dis_expr"
            | "meta_lit"
            | "old_meta_lit"
            | "read_cond_lit"
            | "splicing_read_cond_lit"
            | "evaling_lit"
            | "tagged_or_ctor_lit"
            | "str_lit"
            | "regex_lit"
            | "comment"
            | "ERROR"
    )
}
fn function_owner<'t>(
    node: Node<'t>,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<(Node<'t>, Owner)>, AdapterError> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if !cache.visit(cancel)? || data(parent) {
            return Ok(None);
        }
        if parent.kind() == "list_lit" {
            if matches!(
                head(parent, source, cancel, cache)?,
                Some("quote" | "comment")
            ) {
                return Ok(None);
            }
            if let Some(proof) = owner(parent, source, cancel, cache)? {
                return Ok((!proof.namespace && node.start_byte() >= proof.body_start)
                    .then_some((parent, proof)));
            }
        }
        ancestor = parent.parent();
    }
    Ok(None)
}
fn vector_unshadowed(
    vector: Node<'_>,
    target: &str,
    pairs: bool,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut cursor = vector.walk();
    let mut count = 0;
    for child in vector.children_by_field_name("value", &mut cursor) {
        if !cache.visit(cancel)? {
            return Ok(false);
        }
        if !pairs || count % 2 == 0 {
            let Some(n) = name(child, source) else {
                return Ok(false);
            };
            if n == target {
                return Ok(false);
            }
        }
        count += 1;
    }
    Ok(!pairs || count % 2 == 0)
}
fn unshadowed(
    node: Node<'_>,
    function: Node<'_>,
    target: &str,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut i = 2;
    while value(function, i, cancel, cache)?
        .is_some_and(|n| matches!(n.kind(), "str_lit" | "map_lit"))
    {
        i += 1;
    }
    let Some(args) = value(function, i, cancel, cache)? else {
        return Ok(false);
    };
    if args.kind() == "vec_lit" && !vector_unshadowed(args, target, false, source, cancel, cache)? {
        return Ok(false);
    }
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if !cache.visit(cancel)? {
            return Ok(false);
        }
        if parent == function {
            break;
        }
        if parent.kind() == "anon_fn_lit" {
            return Ok(false);
        }
        if parent.kind() == "list_lit" {
            let op = head(parent, source, cancel, cache)?;
            if op.is_none() && parent.parent() == Some(function) {
                let Some(params) = value(parent, 0, cancel, cache)? else {
                    return Ok(false);
                };
                if params.kind() != "vec_lit"
                    || !vector_unshadowed(params, target, false, source, cancel, cache)?
                {
                    return Ok(false);
                }
            } else if matches!(op, Some("let" | "let*" | "loop" | "loop*")) {
                let Some(bindings) = value(parent, 1, cancel, cache)? else {
                    return Ok(false);
                };
                if bindings.kind() != "vec_lit"
                    || !vector_unshadowed(bindings, target, true, source, cancel, cache)?
                {
                    return Ok(false);
                }
            } else if matches!(
                op,
                Some(
                    "fn" | "fn*"
                        | "letfn"
                        | "binding"
                        | "with-redefs"
                        | "->"
                        | "->>"
                        | "as->"
                        | "case"
                        | "cond"
                )
            ) {
                return Ok(false);
            } else if parent != node.parent().unwrap_or(parent)
                && !matches!(op, Some("do" | "if" | "when" | "when-not" | "and" | "or"))
            {
                // An unknown macro may treat its argument forms as unevaluated data.
                return Ok(false);
            }
        }
        ancestor = parent.parent();
    }
    Ok(true)
}
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let syntax = if node.kind() == "source"
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        range = 0..source.len();
        "clojure.file"
    } else if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        let Some(proof) = owner(node, source, cancel, cache)? else {
            return Ok(None);
        };
        match role {
            StructuralRole::Definition => {
                range = proof.name;
                "clojure.name"
            }
            StructuralRole::Signature => {
                range = proof.header;
                "clojure.written_header"
            }
            _ => {
                range = proof.extent;
                if proof.namespace {
                    "clojure.namespace"
                } else {
                    "clojure.function"
                }
            }
        }
    } else if role == StructuralRole::Reference {
        let Some(name_range) = symbol(node, source) else {
            return Ok(None);
        };
        let Some((function, _)) = function_owner(node, source, cancel, cache)? else {
            return Ok(None);
        };
        range = name_range;
        let parent = node.parent();
        let call = if let Some(list) = parent.filter(|p| p.kind() == "list_lit") {
            value(list, 0, cancel, cache)? == Some(node)
                && !matches!(
                    name(node, source),
                    Some(
                        "let"
                            | "let*"
                            | "loop"
                            | "loop*"
                            | "if"
                            | "do"
                            | "recur"
                            | "fn"
                            | "fn*"
                            | "quote"
                            | "var"
                            | "try"
                            | "catch"
                            | "finally"
                            | "throw"
                            | "new"
                            | "set!"
                            | "def"
                            | "ns"
                            | "defn"
                            | "defn-"
                    )
                )
        } else {
            false
        };
        if call {
            if unshadowed(
                node,
                function,
                name(node, source).unwrap_or(""),
                source,
                cancel,
                cache,
            )? {
                "clojure.call"
            } else {
                "clojure.dynamic_call"
            }
        } else {
            "clojure.symbol"
        }
    } else if role == StructuralRole::Expression {
        if node.kind() == "list_lit"
            && top(node)
            && matches!(head(node, source, cancel, cache)?, Some("defn" | "defn-"))
            && !namespace_known(node, source, cancel, cache)?
        {
            "clojure.namespace_unavailable"
        } else if node.kind() == "list_lit"
            && owner(node, source, cancel, cache)?.is_none()
            && (top(node) && head(node, source, cancel, cache)? != Some("comment")
                || matches!(
                    head(node, source, cancel, cache)?,
                    Some("def" | "defonce" | "defmacro" | "declare")
                ) && function_owner(node, source, cancel, cache)?.is_some())
        {
            "clojure.binding_unavailable"
        } else if data(node)
            || node.kind() == "list_lit"
                && (top(node) && owner(node, source, cancel, cache)?.is_none()
                    || matches!(
                        head(node, source, cancel, cache)?,
                        Some(
                            "fn" | "fn*"
                                | "letfn"
                                | "binding"
                                | "with-redefs"
                                | "defmacro"
                                | "declare"
                                | "def"
                                | "defonce"
                                | "in-ns"
                                | "->"
                                | "->>"
                        )
                    ))
        {
            "clojure.unavailable"
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Comment && clean(node, source) {
        "clojure.comment"
    } else if role == StructuralRole::StringLiteral && clean(node, source) {
        "clojure.string"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancel)?.saturating_add(usize::from(
            role == StructuralRole::Scope && node.kind() != "source",
        )),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn owner_work_exhaustion_and_cancellation_remain_observable() {
        let source = b"(ns Sample) (defn written [x] x)";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_clojure::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source.as_slice(), None).unwrap();
        let definition = tree.root_node().named_child(1).unwrap();
        let cancellation = Cancellation::new();
        let mut exhausted = CaptureCache::new(0);
        assert_eq!(
            candidate(
                definition,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut exhausted
            )
            .unwrap(),
            None
        );
        assert!(exhausted.exhausted());
        let mut admitted = CaptureCache::new(256);
        assert!(
            candidate(
                definition,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut admitted
            )
            .unwrap()
            .is_some()
        );
        assert!(!admitted.exhausted());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            candidate(
                definition,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut admitted
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        );
    }
}
