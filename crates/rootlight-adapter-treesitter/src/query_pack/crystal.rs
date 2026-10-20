//! Prove Crystal names only at the file or in a written lexical type body.
//! Conditional, macro, call-block and method-body declarations remain unavailable.

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use std::collections::HashMap;
use tree_sitter::Node;

#[derive(Clone, Copy)]
struct Proof {
    name_start: usize,
    name_end: usize,
    header_end: usize,
    syntax: &'static str,
}

pub(super) struct CaptureCache {
    declarations: HashMap<usize, Option<Proof>>,
    remaining: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum: usize) -> Self {
        Self {
            declarations: HashMap::new(),
            remaining: maximum,
            exhausted: false,
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }
    fn proof(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Proof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self.declarations.get(&node.id()) {
            return Ok(*proof);
        }
        if !self.visit(cancellation)? {
            return Ok(None);
        }
        let proof = self.prove(node, source, cancellation)?;
        self.declarations
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.declarations.insert(node.id(), proof);
        Ok(proof)
    }
    fn prove(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Proof>, AdapterError> {
        if node.is_missing() || node.has_error() {
            return Ok(None);
        }
        let Some(syntax) = syntax(node.kind()) else {
            return Ok(None);
        };
        let Some(body) = node
            .parent()
            .filter(|parent| parent.kind() == "expressions")
        else {
            return Ok(None);
        };
        if let Some(owner) = body.parent()
            && (!matches!(
                owner.kind(),
                "module_def" | "class_def" | "struct_def" | "enum_def"
            ) || owner.child_by_field_name("body") != Some(body)
                || self.proof(owner, source, cancellation)?.is_none())
        {
            return Ok(None);
        }
        if node.child_by_field_name("class").is_some() {
            return Ok(None);
        }
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        if !matches!(name.kind(), "constant" | "identifier")
            || name.is_missing()
            || name.has_error()
        {
            return Ok(None);
        }
        let Some(raw) = source
            .get(name.byte_range())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            return Ok(None);
        };
        if structural_captured_name_for_language("crystal", raw, raw.len()).is_none() {
            return Ok(None);
        }
        let mut header_end = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |body| body.start_byte());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancellation)? {
                return Ok(None);
            }
            if matches!(child.kind(), "end" | "rescue" | "ensure" | "else") {
                header_end = header_end.min(child.start_byte());
            }
        }
        if header_end < name.end_byte() {
            return Ok(None);
        }
        Ok(Some(Proof {
            name_start: name.start_byte(),
            name_end: name.end_byte(),
            header_end,
            syntax,
        }))
    }
}

fn syntax(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "module_def" => "crystal.module",
        "class_def" => "crystal.class",
        "struct_def" => "crystal.struct",
        "enum_def" => "crystal.enum",
        "method_def" | "abstract_method_def" => "crystal.method",
        _ => return None,
    })
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let root = node.kind() == "expressions" && node.parent().is_none();
    let syntax = if root
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        range = 0..source.len();
        "crystal.file"
    } else if role == StructuralRole::Comment && node.kind() == "comment" && !node.has_error() {
        "crystal.comment"
    } else if role == StructuralRole::StringLiteral && node.kind() == "string" && !node.has_error()
    {
        "crystal.string"
    } else {
        let proof = cache.proof(node, source, cancellation)?;
        if role == StructuralRole::Expression {
            if proof.is_some() {
                return Ok(None);
            }
            if let Some(first) = node.child(0) {
                range.end = first.end_byte();
            }
            "crystal.unsupported"
        } else {
            let Some(proof) = proof else {
                return Ok(None);
            };
            match role {
                StructuralRole::Definition => {
                    range = proof.name_start..proof.name_end;
                    "crystal.name"
                }
                StructuralRole::Signature => {
                    range.end = proof.header_end;
                    "crystal.header"
                }
                StructuralRole::Declaration | StructuralRole::Scope => proof.syntax,
                _ => return Ok(None),
            }
        }
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && !root {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-crystal-depth"))?;
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        syntax,
        role,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_owner_proofs_obey_capacity_and_cancellation() {
        let source = b"module Written\nclass Nested\ndef call(value)\nvalue\nend\nend\nend\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_crystal::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let owner = tree.root_node().named_child(0).unwrap();
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(0);
        assert!(
            candidate(
                owner,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut bounded
            )
            .unwrap()
            .is_none()
        );
        assert!(bounded.exhausted());
        let mut admitted = CaptureCache::new(128);
        assert!(
            candidate(
                owner,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut admitted
            )
            .unwrap()
            .is_some()
        );
        let nested = owner
            .child_by_field_name("body")
            .unwrap()
            .named_child(0)
            .unwrap();
        let method = nested
            .child_by_field_name("body")
            .unwrap()
            .named_child(0)
            .unwrap();
        let header = candidate(
            method,
            StructuralRole::Signature,
            source,
            &cancellation,
            &mut admitted,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&source[header.start..header.end])
                .unwrap()
                .trim(),
            "def call(value)"
        );
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            candidate(
                owner,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut admitted
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
