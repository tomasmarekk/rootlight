//! Reconstruct lexical block-parameter endpoints from complete native templates.
//! Matching delimiters and an atomic bounded census prevent recovery, inverse
//! branches or unavailable nearer aliases from leaking an outer lexical target.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(crate) type Range = (usize, usize);
pub(crate) struct Definition {
    pub(crate) name: Range,
    body: Range,
}
pub(crate) struct Read {
    pub(crate) name: Range,
    pub(crate) definition: Range,
}
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
fn last_named(node: Node<'_>) -> Option<Node<'_>> {
    let index = u32::try_from(node.named_child_count().checked_sub(1)?).ok()?;
    node.named_child(index)
}
fn within(outer: Range, inner: Range) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}
fn identifier(source: &[u8], node: Node<'_>) -> bool {
    source.get(node.byte_range()).is_some_and(|name| {
        name.first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && name
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            && name != b"this"
    })
}

pub(crate) fn value_identifier(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "path_expression" => {
            parent.named_child(0) == Some(node)
                && parent.parent().is_some_and(|owner| {
                    owner.kind() != "helper_invocation"
                        || owner.child_by_field_name("helper") != Some(parent)
                })
        }
        "mustache_statement" | "sub_expression" => true,
        "helper_invocation" => parent.child_by_field_name("helper") != Some(node),
        "block_statement_start" => parent.child_by_field_name("path") != Some(node),
        "hash_pair" => parent.child_by_field_name("value") == Some(node),
        _ => false,
    }
}

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
        || root.kind() != "template"
        || root.parent().is_some()
        || root.start_byte() > root.end_byte()
        || root.end_byte() > source.len()
        || root.has_error()
    {
        return Ok((None, false));
    }
    let mut work = Work {
        remaining: maximum,
        cancel,
        exhausted: false,
    };
    let prefix = if source.starts_with(b"\xef\xbb\xbf") && root.start_byte() >= 3 {
        3
    } else {
        0
    };
    for trivia in [
        source.get(prefix..root.start_byte()),
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

fn equal(
    source: &[u8],
    left: Range,
    right: Range,
    work: &mut Work<'_>,
) -> Result<Option<bool>, AdapterError> {
    let (Some(left), Some(right)) = (source.get(left.0..left.1), source.get(right.0..right.1))
    else {
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

fn collect(
    root: Node<'_>,
    source: &[u8],
    work: &mut Work<'_>,
) -> Result<Option<Census>, AdapterError> {
    let mut definitions = Vec::<Definition>::new();
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
        if node.kind() == "block_statement" {
            let (Some(start), Some(end)) = (node.named_child(0), last_named(node)) else {
                return Ok(None);
            };
            let (Some(open), Some(close)) = (
                start.child_by_field_name("path"),
                end.child_by_field_name("path"),
            ) else {
                return Ok(None);
            };
            if start.kind() != "block_statement_start"
                || end.kind() != "block_statement_end"
                || equal(source, range(open), range(close), work)? != Some(true)
            {
                return Ok(None);
            }
        }
        if node.kind() == "mustache_statement" {
            let Some(mut head) = node.named_child(0) else {
                return Ok(None);
            };
            if head.kind() == "helper_invocation" {
                let Some(helper) = head.child_by_field_name("helper") else {
                    return Ok(None);
                };
                head = helper;
            }
            if source.get(head.byte_range()) == Some(b"else") {
                return Ok(None);
            }
        }
        if node.kind() == "block_params" {
            let Some(start) = node
                .parent()
                .filter(|parent| parent.kind() == "block_statement_start")
            else {
                return Ok(None);
            };
            let Some(block) = start
                .parent()
                .filter(|parent| parent.kind() == "block_statement")
            else {
                return Ok(None);
            };
            let Some(end) = last_named(block) else {
                return Ok(None);
            };
            let body = (start.end_byte(), end.start_byte());
            let first = definitions.len();
            let Ok(count) = u32::try_from(node.named_child_count()) else {
                return Ok(None);
            };
            for index in 0..count {
                if !work.step()? {
                    return Ok(None);
                }
                let Some(name) = node
                    .named_child(index)
                    .filter(|name| identifier(source, *name))
                else {
                    return Ok(None);
                };
                for previous in definitions.get(first..).unwrap_or_default() {
                    if !work.step()?
                        || equal(source, previous.name, range(name), work)? != Some(false)
                    {
                        return Ok(None);
                    }
                }
                definitions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                definitions.push(Definition {
                    name: range(name),
                    body,
                });
            }
        } else if node.kind() == "identifier" && value_identifier(node) && identifier(source, node)
        {
            identifiers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            identifiers.push(node);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if cursor.goto_parent() {
                continue;
            }
            let mut reads = Vec::new();
            for read in identifiers {
                if !work.step()? {
                    return Ok(None);
                }
                let mut parent = read.parent();
                'ancestors: while let Some(owner) = parent {
                    if !work.step()? {
                        return Ok(None);
                    }
                    if owner.kind() == "block_statement" {
                        for definition in &definitions {
                            if !work.step()? {
                                return Ok(None);
                            }
                            if within(range(owner), definition.name)
                                && within(definition.body, range(read))
                                && equal(source, definition.name, range(read), work)? == Some(true)
                            {
                                // Only aliases declared by this owner can stop the
                                // ancestor search; nested declarations are separate scopes.
                                let start = owner.named_child(0).map(range);
                                if !start.is_some_and(|start| within(start, definition.name)) {
                                    continue;
                                }
                                reads
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                reads.push(Read {
                                    name: range(read),
                                    definition: definition.name,
                                });
                                break 'ancestors;
                            }
                        }
                    }
                    parent = owner.parent();
                }
            }
            return Ok(Some(Census { definitions, reads }));
        }
    }
}
