//! Certify direct written abstractions and earlier same-file application heads.
//! Binding needs a complete clean Definition-only environment; classification
//! uses the declaration's explicit parameters or direct lambda syntax.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use rootlight_adapter_sdk::SinkError;

use super::*;

#[derive(Default)]
pub(super) struct Proofs {
    pub(super) callables: HashSet<usize>,
    pub(super) references: HashMap<usize, Range<usize>>,
    pub(super) markers: HashMap<usize, Range<usize>>,
}

fn push<T>(values: &mut Vec<T>, value: T) -> Result<(), AdapterError> {
    values
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    values.push(value);
    Ok(())
}

fn key(
    node: Node<'_>,
    source: &[u8],
    cache: &mut CaptureCache<'_>,
    cancel: &Cancellation,
) -> Result<Option<[u8; 32]>, AdapterError> {
    let Some(bytes) = source
        .get(node.byte_range())
        .filter(|bytes| !bytes.is_empty())
    else {
        return Ok(None);
    };
    let mut hash = blake3::Hasher::new();
    for chunk in bytes.chunks(64) {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        hash.update(chunk);
    }
    Ok(Some(*hash.finalize().as_bytes()))
}

fn children<'tree>(
    node: Node<'tree>,
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Vec<Node<'tree>>, AdapterError> {
    let mut values = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !cache.visit(cancel)? {
            return Ok(Vec::new());
        }
        if child.kind() != "comment" {
            push(&mut values, child)?;
        }
    }
    Ok(values)
}

fn formal_names<'tree>(
    binder: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Vec<[u8; 32]>>, AdapterError> {
    let mut parts = children(binder, cache, cancel)?;
    let implicit = parts.len() == 1 && parts[0].kind() == "implicit_binders";
    if implicit {
        parts = children(parts[0], cache, cancel)?;
    }
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return Ok(None);
    };
    if (implicit && !matches!((first.kind(), last.kind()), ("{", "}") | ("[", "]")))
        || (!implicit && (first.kind(), last.kind()) != ("(", ")"))
    {
        return Ok(None);
    }
    let (mut names, mut colon, mut ty) = (Vec::new(), false, false);
    for part in parts.iter().skip(1).take(parts.len().saturating_sub(2)) {
        if !clean(*part, source) {
            return Ok(None);
        }
        match part.kind() {
            "ident" if !colon => {
                let Some(name) = key(*part, source, cache, cancel)? else {
                    return Ok(None);
                };
                push(&mut names, name)?;
            }
            ":" if !colon && !names.is_empty() => colon = true,
            "type" if colon && !ty => ty = true,
            _ => return Ok(None),
        }
    }
    Ok((!names.is_empty() && (implicit || (colon && ty)) && (!colon || ty)).then_some(names))
}

struct Definition<'tree> {
    command: Node<'tree>,
    name: [u8; 32],
    body: Node<'tree>,
    formals: HashSet<[u8; 32]>,
    ordinary: bool,
    callable: bool,
}

impl Proofs {
    pub(super) fn build<'tree>(
        source: &[u8],
        complete: bool,
        cache: &mut CaptureCache<'tree>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self::default();
        let root = cache.root;
        if root.kind() != "source_file" || !clean(root, source) {
            return Ok(result);
        }
        let mut environment = complete
            && root.end_byte() == source.len()
            && source
                .get(..root.start_byte())
                .is_some_and(|prefix| matches!(prefix, b"" | b"\xef\xbb\xbf"));
        let mut definitions = Vec::new();
        let mut names = HashMap::new();
        for sentence in children(root, cache, cancel)? {
            if sentence.kind() != "sentence" {
                environment = false;
                continue;
            }
            let members = children(sentence, cache, cancel)?;
            let mut commands = members.iter().filter(|node| node.is_named());
            let Some(command) = commands.next().copied() else {
                environment = false;
                continue;
            };
            if commands.next().is_some()
                || command.kind() != "definition_command"
                || !clean(command, source)
                || members
                    .iter()
                    .any(|node| !node.is_named() && node.kind() != ".")
            {
                environment = false;
                continue;
            }
            let Some(body) = command.child_by_field_name("body") else {
                environment = false;
                continue;
            };
            let mut head = None;
            let mut formals = HashSet::new();
            let (mut ordinary, mut has_formals) = (true, false);
            for member in children(command, cache, cancel)? {
                if member.kind() == "ident_decl" {
                    let ident = direct_name(member, cancel, cache)?;
                    if head.replace(ident).is_some() {
                        ordinary = false;
                        environment = false;
                    }
                }
                if member.kind() == "binder" {
                    has_formals = true;
                    match formal_names(member, source, cache, cancel)? {
                        Some(values) => {
                            for name in values {
                                formals
                                    .try_reserve(1)
                                    .map_err(|_| SinkError::AllocationFailed)?;
                                if !formals.insert(name) {
                                    ordinary = false;
                                }
                            }
                        }
                        None => ordinary = false,
                    }
                }
            }
            let Some(name) = head.flatten() else {
                environment = false;
                continue;
            };
            let Some(name) = key(name, source, cache, cancel)? else {
                return Ok(Self::default());
            };
            let lambda = body.kind() == "lambda_function"
                && children(body, cache, cancel)?
                    .iter()
                    .any(|node| node.kind() == "open_binders" && node.named_child_count() > 0);
            let callable = ordinary && (has_formals || lambda);
            if callable {
                result
                    .callables
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.callables.insert(command.start_byte());
            }
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            names
                .entry(name)
                .and_modify(|value| *value = None)
                .or_insert(Some(definitions.len()));
            push(
                &mut definitions,
                Definition {
                    command,
                    name,
                    body,
                    formals,
                    ordinary,
                    callable,
                },
            )?;
        }
        if !environment || cache.exhausted() {
            return Ok(result);
        }
        for owner in &definitions {
            if !owner.ordinary || !owner.callable || owner.body.kind() != "application" {
                continue;
            }
            let parts = children(owner.body, cache, cancel)?;
            let Some(head) = parts.first().copied().filter(|node| node.kind() == "ident") else {
                continue;
            };
            if parts.len() < 2 || parts.iter().any(|node| !node.is_named()) {
                continue;
            }
            let Some(name) = key(head, source, cache, cancel)? else {
                return Ok(Self::default());
            };
            if owner.formals.contains(&name) || name == owner.name {
                continue;
            }
            let Some(index) = names.get(&name).copied().flatten() else {
                continue;
            };
            let target = &definitions[index];
            if !target.callable || target.command.end_byte() >= owner.command.start_byte() {
                continue;
            }
            result
                .references
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.references.insert(head.end_byte(), head.byte_range());
            result
                .markers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.markers.insert(
                owner.body.start_byte(),
                target.command.start_byte()..head.end_byte(),
            );
        }
        if cache.exhausted() {
            return Ok(Self::default());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn inspect(
        source: &[u8],
        credit: usize,
        cancel: &Cancellation,
    ) -> Result<Proofs, AdapterError> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rocq::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(tree.root_node(), credit);
        Proofs::build(source, true, &mut cache, cancel)
    }

    #[test]
    fn original_geometry_certifies_only_direct_v0_head_and_written_callable_definitions() {
        let source = include_bytes!("../../../tests/fixtures/coq-written-callables/stlc_small.v");
        let proof = inspect(source, 100_000, &Cancellation::new()).unwrap();
        assert!(proof.callables.contains(&1215));
        assert!(proof.callables.contains(&1611));
        assert!(!proof.callables.contains(&0));
        assert_eq!(proof.references.get(&1659), Some(&(1656..1659)));
        assert_eq!(proof.markers.get(&1656), Some(&(1215..1659)));
        assert!(
            !proof
                .references
                .values()
                .any(|range| range.start >= 1264 && range.end <= 1303)
        );
    }

    #[test]
    fn unknown_environment_shadowing_and_duplicate_heads_never_issue_a_reference_certificate() {
        for source in [
            "Require Import Other. Definition target (x : nat) := x. Definition owned (y : nat) := target y.",
            "Definition target (x : nat) := x. Definition owned (target : nat) := target 0.",
            "Definition target (x : nat) := x. Definition target (x : nat) := x. Definition owned (y : nat) := target y.",
            "Definition target (x : nat) := x. Definition owned (y : nat) := let target := y in target 0.",
        ] {
            let proof = inspect(source.as_bytes(), 100_000, &Cancellation::new()).unwrap();
            assert!(proof.references.is_empty(), "{source}");
            assert!(proof.markers.is_empty(), "{source}");
        }
    }

    #[test]
    fn cancellation_and_exhaustion_never_return_partial_binding_certificates() {
        let source = b"Definition target (x : nat) := x. Definition owned (y : nat) := target y.";
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            inspect(source, 100_000, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        for credit in [0, 1, 8] {
            let proof = inspect(source, credit, &Cancellation::new()).unwrap();
            assert!(proof.references.is_empty());
            assert!(proof.markers.is_empty());
        }
    }
}
