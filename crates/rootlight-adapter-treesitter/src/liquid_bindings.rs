//! Certify written Liquid loop items and ordinary local value uses.
//! A bounded complete native tree supplies exact definition/use endpoints;
//! runtime objects, property names and ambiguous mutation never select targets.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

#[cfg(test)]
mod tests;

pub(crate) type Range = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Definition {
    pub(crate) name: Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) name: Range,
    pub(crate) definition: Range,
}

#[derive(Default)]
pub(crate) struct Census {
    pub(crate) definitions: Vec<Definition>,
    pub(crate) reads: Vec<Read>,
}

struct Work<'a> {
    remaining: usize,
    cancel: &'a Cancellation,
    exhausted: bool,
}

impl Work<'_> {
    fn step(&mut self) -> Result<bool, AdapterError> {
        self.cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }
}

fn range(node: Node<'_>) -> Range {
    (node.start_byte(), node.end_byte())
}

fn within(outer: Range, inner: Range) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

fn same(left: Node<'_>, right: Option<Node<'_>>) -> bool {
    right == Some(left)
}

pub(crate) fn value_identifier(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "for_loop_statement" | "tablerow_statement" => {
            !same(node, parent.child_by_field_name("item"))
        }
        "assignment_statement" => !same(node, parent.child_by_field_name("variable_name")),
        "capture_statement" => false,
        "increment_statement" | "decrement_statement" | "custom_unpaired_statement" => false,
        "filter" => !same(node, parent.child_by_field_name("name")),
        "argument" => !same(node, parent.child_by_field_name("key")),
        "access" if same(node, parent.child_by_field_name("property")) => parent
            .child(1)
            .is_some_and(|delimiter| delimiter.kind() == "["),
        "render_statement" | "include_statement" => false,
        _ => true,
    }
}

fn mutation_name(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "assignment_statement" => node.child_by_field_name("variable_name"),
        "capture_statement" => node.child_by_field_name("variable"),
        "increment_statement" | "decrement_statement" => node.named_child(0),
        _ => None,
    }
}

/// Return one atomic census or withhold all endpoints on unavailable context/work.
/// Every traversal, ancestor visit and compared source chunk spends finite credit.
pub(crate) fn census(
    root: Node<'_>,
    source: &[u8],
    maximum: usize,
    complete: bool,
    cancel: &Cancellation,
) -> Result<Option<Census>, AdapterError> {
    Ok(census_with_status(root, source, maximum, complete, cancel)?.0)
}

pub(crate) fn census_with_status(
    root: Node<'_>,
    source: &[u8],
    maximum: usize,
    complete: bool,
    cancel: &Cancellation,
) -> Result<(Option<Census>, bool), AdapterError> {
    cancel.check()?;
    if maximum == 0 {
        return Ok((None, true));
    }
    if !complete
        || root.kind() != "program"
        || root.parent().is_some()
        || root.start_byte() > root.end_byte()
        || root.end_byte() > source.len()
        || root.has_error()
        || root.is_error()
        || root.is_missing()
    {
        return Ok((None, false));
    }
    let mut work = Work {
        remaining: maximum,
        cancel,
        exhausted: false,
    };
    let prefix_start = if source.starts_with(b"\xef\xbb\xbf") && root.start_byte() >= 3 {
        3
    } else {
        0
    };
    for trivia in [
        source.get(prefix_start..root.start_byte()),
        source.get(root.end_byte()..),
    ] {
        let Some(trivia) = trivia else {
            return Ok((None, false));
        };
        for chunk in trivia.chunks(64) {
            if !work.step()? {
                return Ok((None, true));
            }
            if !chunk.iter().all(u8::is_ascii_whitespace) {
                return Ok((None, false));
            }
        }
    }
    let result = collect(root, source, &mut work)?;
    Ok((if work.exhausted { None } else { result }, work.exhausted))
}

fn collect(
    root: Node<'_>,
    source: &[u8],
    work: &mut Work<'_>,
) -> Result<Option<Census>, AdapterError> {
    let mut items = Vec::new();
    let mut mutations = Vec::new();
    let mut opaque = Vec::new();
    let mut identifiers = Vec::new();
    let mut cursor = root.walk();
    loop {
        if !work.step()? {
            return Ok(None);
        }
        let node = cursor.node();
        if node.is_error() || node.is_missing() || source.get(node.byte_range()).is_none() {
            return Ok(None);
        }
        if node.kind() == "for_loop_statement" {
            let Some(item) = node
                .child_by_field_name("item")
                .filter(|item| item.kind() == "identifier")
            else {
                return Ok(None);
            };
            let Some(body) = node
                .child_by_field_name("body")
                .filter(|body| body.kind() == "block")
            else {
                return Ok(None);
            };
            if !within(range(node), range(body)) {
                return Ok(None);
            }
            items
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            items.push(Definition { name: range(item) });
        }
        if matches!(
            node.kind(),
            "include_statement"
                | "render_statement"
                | "custom_unpaired_statement"
                | "liquid_tag"
                | "form_statement"
                | "paginate_statement"
        ) {
            opaque
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            opaque.push(range(node));
        }
        if let Some(name) = mutation_name(node) {
            mutations
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            mutations.push(range(name));
        }
        if node.kind() == "identifier" && value_identifier(node) {
            identifiers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            identifiers.push(node);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if !work.step()? {
                return Ok(None);
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                let mut result = Census {
                    definitions: items,
                    reads: Vec::new(),
                };
                for identifier in identifiers {
                    if !work.step()? {
                        return Ok(None);
                    }
                    let mut parent = identifier.parent();
                    while let Some(owner) = parent {
                        if !work.step()? {
                            return Ok(None);
                        }
                        if matches!(
                            owner.kind(),
                            "tablerow_statement"
                                | "capture_statement"
                                | "form_statement"
                                | "paginate_statement"
                                | "liquid_tag"
                                | "custom_paired_statement"
                        ) {
                            break;
                        }
                        if owner.kind() == "for_loop_statement" {
                            let Some(item) = owner.child_by_field_name("item") else {
                                return Ok(None);
                            };
                            let Some(body) = owner.child_by_field_name("body") else {
                                return Ok(None);
                            };
                            if within(range(body), range(identifier))
                                && equal_text(source, range(item), range(identifier), work)?
                                    == Some(true)
                            {
                                let mut ambiguous = false;
                                for statement in &opaque {
                                    if !work.step()? {
                                        return Ok(None);
                                    }
                                    ambiguous |= within(range(body), *statement);
                                }
                                for mutation in &mutations {
                                    if !work.step()? {
                                        return Ok(None);
                                    }
                                    if within(range(body), *mutation) {
                                        match equal_text(source, range(item), *mutation, work)? {
                                            Some(equal) => ambiguous |= equal,
                                            None => return Ok(None),
                                        }
                                    }
                                }
                                if !ambiguous {
                                    result
                                        .reads
                                        .try_reserve(1)
                                        .map_err(|_| SinkError::AllocationFailed)?;
                                    result.reads.push(Read {
                                        name: range(identifier),
                                        definition: range(item),
                                    });
                                }
                                // A nearer same-name item shadows outer binders even
                                // when its mutation makes the value target unavailable.
                                break;
                            }
                        }
                        parent = owner.parent();
                    }
                }
                return Ok(Some(result));
            }
        }
    }
}

fn equal_text(
    source: &[u8],
    left: Range,
    right: Range,
    work: &mut Work<'_>,
) -> Result<Option<bool>, AdapterError> {
    let Some(left) = source.get(left.0..left.1) else {
        return Ok(None);
    };
    let Some(right) = source.get(right.0..right.1) else {
        return Ok(None);
    };
    if left.len() != right.len() {
        return Ok(Some(false));
    }
    for (left, right) in left.chunks(64).zip(right.chunks(64)) {
        if !work.step()? {
            return Ok(None);
        }
        if left != right {
            return Ok(Some(false));
        }
    }
    Ok(Some(true))
}
