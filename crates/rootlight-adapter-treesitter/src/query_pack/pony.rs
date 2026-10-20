//! Prove Pony type and member names from direct native children and owners.
//! Anonymous objects and other unmodeled declarations cannot acquire static owners.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

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
        let syntax = if let Some(syntax) = type_syntax(node.kind()) {
            if !node
                .parent()
                .is_some_and(|parent| parent.kind() == "source_file")
            {
                return Ok(None);
            }
            syntax
        } else {
            let syntax = match node.kind() {
                "constructor" => "pony.constructor",
                "method" => "pony.method",
                "behavior" => "pony.behavior",
                _ => return Ok(None),
            };
            let Some(owner) = node
                .parent()
                .filter(|parent| parent.kind() == "members")
                .and_then(|members| members.parent())
            else {
                return Ok(None);
            };
            if type_syntax(owner.kind()).is_none()
                || self.proof(owner, source, cancellation)?.is_none()
            {
                return Ok(None);
            }
            syntax
        };
        let mut name = None;
        let mut header_end = node.end_byte();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancellation)? {
                return Ok(None);
            }
            if child.kind() == "identifier" {
                if name.is_some() || !identifier(child, source) {
                    return Ok(None);
                }
                name = Some((child.start_byte(), child.end_byte()));
            }
            if matches!(
                child.kind(),
                "members" | "block" | "string" | "=>" | "line_comment" | "block_comment"
            ) {
                header_end = header_end.min(child.start_byte());
            }
        }
        let Some((name_start, name_end)) = name else {
            return Ok(None);
        };
        if header_end < name_end {
            return Ok(None);
        }
        Ok(Some(Proof {
            name_start,
            name_end,
            header_end,
            syntax,
        }))
    }
}

fn type_syntax(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "class_definition" => "pony.class",
        "actor_definition" => "pony.actor",
        "struct_definition" => "pony.struct",
        "primitive_definition" => "pony.primitive",
        "trait_definition" => "pony.trait",
        "interface_definition" => "pony.interface",
        _ => return None,
    })
}

fn identifier(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing()
        && !node.has_error()
        && source
            .get(node.byte_range())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .is_some_and(|raw| {
                structural_captured_name_for_language("pony", raw, raw.len()).is_some()
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
    let syntax = if node.kind() == "source_file" {
        range = 0..source.len();
        "pony.file"
    } else if role == StructuralRole::Comment
        && matches!(node.kind(), "line_comment" | "block_comment")
        && !node.has_error()
    {
        "pony.comment"
    } else if role == StructuralRole::StringLiteral && node.kind() == "string" && !node.has_error()
    {
        "pony.string"
    } else {
        let proof = cache.proof(node, source, cancellation)?;
        if role == StructuralRole::Expression {
            if proof.is_some() {
                return Ok(None);
            }
            // A gap needs its written anchor, not another owned copy of a body.
            if let Some(first) = node.child(0) {
                range.end = first.end_byte();
            }
            "pony.unsupported"
        } else {
            let Some(proof) = proof else {
                return Ok(None);
            };
            match role {
                StructuralRole::Definition => {
                    range = proof.name_start..proof.name_end;
                    "pony.name"
                }
                StructuralRole::Signature => {
                    range.end = proof.header_end;
                    "pony.header"
                }
                StructuralRole::Declaration | StructuralRole::Scope => proof.syntax,
                _ => return Ok(None),
            }
        }
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "source_file" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-pony-depth"))?;
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
        let source = b"class Written\n  fun ref connected() => None\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_pony::LANGUAGE.into())
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
        let members = owner.named_child(1).unwrap();
        assert_eq!(members.kind(), "members");
        let method = members.named_child(0).unwrap();
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
            "fun ref connected()"
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
