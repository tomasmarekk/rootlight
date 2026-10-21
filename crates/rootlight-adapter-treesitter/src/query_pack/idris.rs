//! Written Idris equations and direct RHS names retain native scope evidence.
//! Lambda, pattern, signature and dependent elaboration contexts prove no global use.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'tree, 'source> {
    root: Node<'tree>,
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    global_census: bool,
    names: HashMap<ContentHash, Option<Node<'tree>>>,
    remaining: usize,
    exhausted: bool,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && node.start_byte() < node.end_byte()
        && node.end_byte() <= source.len()
}

impl<'tree, 'source> CaptureCache<'tree, 'source> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            root,
            source,
            complete: complete
                && root.kind() == "idris"
                && (root.start_byte() == 0
                    || (root.start_byte() == 3 && source.starts_with(b"\xef\xbb\xbf")))
                && root.end_byte() == source.len()
                && clean(root, source),
            initialized: false,
            global_census: true,
            names: HashMap::new(),
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(64),
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<ContentHash>, AdapterError> {
        if !self.step(cancel)?
            || !matches!(node.kind(), "loname" | "caname")
            || !clean(node, self.source)
        {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        if !raw
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            return Ok(None);
        }
        let mut digest = blake3::Hasher::new();
        for chunk in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if !chunk
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\''))
            {
                return Ok(None);
            }
            digest.update(chunk);
        }
        Ok(Some(ContentHash::from_bytes(*digest.finalize().as_bytes())))
    }

    fn same_name(
        &mut self,
        left: Node<'_>,
        right: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.step(cancel)? {
            return Ok(false);
        }
        let (Some(left), Some(right)) = (
            self.source.get(left.byte_range()),
            self.source.get(right.byte_range()),
        ) else {
            return Ok(false);
        };
        if left.len() != right.len() {
            return Ok(false);
        }
        for (left, right) in left.chunks(64).zip(right.chunks(64)) {
            if !self.step(cancel)? || left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn children(
        &mut self,
        node: Node<'tree>,
        kind: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut result = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.step(cancel)? || !clean(child, self.source) {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            if result.is_some() || child.kind() != kind {
                return Ok(None);
            }
            result = Some(child);
        }
        Ok(result)
    }

    fn function_name(
        &mut self,
        function: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.step(cancel)?
            || function.kind() != "function"
            || function.parent() != Some(self.root)
            || !clean(function, self.source)
        {
            return Ok(None);
        }
        let mut lhs = None;
        let mut rhs = None;
        let mut cursor = function.walk();
        for child in function.named_children(&mut cursor) {
            if !self.step(cancel)? || !clean(child, self.source) {
                return Ok(None);
            }
            match child.kind() {
                "lhs" if lhs.is_none() => lhs = Some(child),
                "rhs" if rhs.is_none() => rhs = Some(child),
                "comment" => {}
                _ => return Ok(None),
            }
        }
        let (Some(lhs), Some(_)) = (lhs, rhs) else {
            return Ok(None);
        };
        let Some(funvar) = self.children(lhs, "funvar", cancel)? else {
            return Ok(None);
        };
        let Some(name) = funvar.child_by_field_name("subject") else {
            return Ok(None);
        };
        if funvar.named_child_count() != 1
            || name.byte_range() != funvar.byte_range()
            || self.name(name, cancel)?.is_none()
        {
            return Ok(None);
        }
        Ok(Some(name))
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        if !self.complete {
            return Ok(());
        }
        let mut cursor = self.root.walk();
        for node in self.root.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(());
            }
            match node.kind() {
                "function" => {
                    let Some(name) = self.function_name(node, cancel)? else {
                        self.global_census = false;
                        continue;
                    };
                    let Some(digest) = self.name(name, cancel)? else {
                        self.global_census = false;
                        continue;
                    };
                    self.names
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.names
                        .entry(digest)
                        .and_modify(|old| *old = None)
                        .or_insert(Some(node));
                }
                "signature" | "comment" => {}
                _ => self.global_census = false,
            }
        }
        Ok(())
    }

    fn admitted_function(
        &mut self,
        function: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(name) = self.function_name(function, cancel)? else {
            return Ok(false);
        };
        let Some(digest) = self.name(name, cancel)? else {
            return Ok(false);
        };
        Ok(self.names.get(&digest).copied().flatten() == Some(function))
    }

    fn direct_global(
        &mut self,
        name: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.global_census {
            return Ok(false);
        }
        let Some(digest) = self.name(name, cancel)? else {
            return Ok(false);
        };
        let Some(exp) = name
            .parent()
            .filter(|node| node.kind() == "exp_name" && node.named_child_count() == 1)
        else {
            return Ok(false);
        };
        let Some(rhs) = exp.parent().filter(|node| node.kind() == "rhs") else {
            return Ok(false);
        };
        let Some(owner) = rhs.parent() else {
            return Ok(false);
        };
        if !self.admitted_function(owner, cancel)? {
            return Ok(false);
        }
        let mut cursor = rhs.walk();
        for child in rhs.named_children(&mut cursor) {
            if !self.step(cancel)? || !clean(child, self.source) || child.kind() != "exp_name" {
                return Ok(false);
            }
        }
        let Some(target) = self.names.get(&digest).copied().flatten() else {
            return Ok(false);
        };
        let Some(target_name) = self.function_name(target, cancel)? else {
            return Ok(false);
        };
        if !self.same_name(name, target_name, cancel)? {
            return Ok(false);
        }
        let mut previous = owner.prev_named_sibling();
        while let Some(node) = previous.filter(|node| node.kind() == "comment") {
            if !self.step(cancel)? {
                return Ok(false);
            }
            previous = node.prev_named_sibling();
        }
        let Some(signature) =
            previous.filter(|node| node.kind() == "signature" && clean(*node, self.source))
        else {
            return Ok(false);
        };
        let (Some(signature_name), Some(owner_name)) = (
            signature.child_by_field_name("name"),
            self.function_name(owner, cancel)?,
        ) else {
            return Ok(false);
        };
        if !self.same_name(signature_name, owner_name, cancel)? {
            return Ok(false);
        }
        // Any same-spelled source name anywhere in the signature can be an
        // implicit/dependent binder. Deferring it avoids elaboration guesses.
        let mut cursor = signature.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(false);
            }
            let node = cursor.node();
            if node != signature_name
                && matches!(node.kind(), "loname" | "caname")
                && self.same_name(node, name, cancel)?
            {
                return Ok(false);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if !self.step(cancel)? {
                    return Ok(false);
                }
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(true);
                }
            }
        }
    }
}

pub(super) fn candidate<'tree>(
    node: Node<'tree>,
    role: StructuralRole,
    cancel: &Cancellation,
    cache: &mut CaptureCache<'tree, '_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cache.initialize(cancel)?;
    if !cache.step(cancel)? {
        return Ok(None);
    }
    let root = role == StructuralRole::Root && node.kind() == "idris";
    let (role, syntax) = if !cache.complete {
        (
            if root {
                role
            } else {
                StructuralRole::Signature
            },
            "idris.native_context_unavailable",
        )
    } else {
        match (role, node.kind()) {
            (StructuralRole::Root | StructuralRole::Module, "idris") => (role, "idris.file"),
            (
                StructuralRole::Declaration | StructuralRole::Scope | StructuralRole::Signature,
                "function",
            ) if cache.admitted_function(node, cancel)? => (role, "idris.function"),
            (StructuralRole::Definition, "loname" | "caname") => {
                let function = node
                    .parent()
                    .and_then(|n| n.parent())
                    .and_then(|n| n.parent());
                if let Some(function) = function
                    && cache.admitted_function(function, cancel)?
                    && cache.function_name(function, cancel)? == Some(node)
                {
                    (role, "idris.function_name")
                } else {
                    (StructuralRole::Signature, "idris.equation_unavailable")
                }
            }
            (StructuralRole::Reference, "loname" | "caname")
                if cache.name(node, cancel)?.is_some() =>
            {
                if cache.direct_global(node, cancel)? {
                    (role, "idris.own_body_global")
                } else {
                    (role, "idris.unproved_name")
                }
            }
            (StructuralRole::Signature, "loname" | "caname") => {
                if cache.direct_global(node, cancel)? {
                    (role, "idris.direct_global_context")
                } else {
                    return Ok(None);
                }
            }
            (StructuralRole::Signature, "signature") => (role, "idris.type_signature"),
            (
                StructuralRole::Declaration | StructuralRole::Scope | StructuralRole::Signature,
                "function",
            ) => (StructuralRole::Signature, "idris.equation_unavailable"),
            (StructuralRole::Comment, "comment") => (role, "idris.comment"),
            (StructuralRole::StringLiteral, "string" | "raw_string" | "triple_quote_string") => {
                (role, "idris.string")
            }
            _ => return Ok(None),
        }
    };
    let mut result = QueryCandidate {
        start: if !cache.complete && !root {
            0
        } else {
            node.start_byte()
        },
        end: if !cache.complete && !root {
            cache.source.len()
        } else {
            node.end_byte()
        },
        role,
        syntax,
        required: role.belongs_to_identity_closure() || role == StructuralRole::Reference,
        native_depth: 0,
    };
    super::canonicalize_file_root_preamble(&mut result, node, cache.source, cache.complete);
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    struct Probe(Cancellation);
    impl tree_sitter_idris::CancellationProbe for Probe {
        fn is_cancelled(&self) -> bool {
            self.0.check().is_err()
        }
    }
    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let scope =
            tree_sitter_idris::ScannerScope::begin(Probe(Cancellation::new()), 10_000_000).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_idris::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        tree
    }
    fn named(root: Node<'_>, start: usize, end: usize) -> Node<'_> {
        root.descendant_for_byte_range(start, end).unwrap()
    }

    #[test]
    fn original_equations_certify_only_direct_signature_disjoint_global_uses() {
        let source = include_bytes!("../../tests/fixtures/idris/stlc_small.idr");
        let tree = parse(source);
        let root = tree.root_node();
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, root, 10000, true);
        for (start, end, role, syntax) in [
            (
                1215,
                1218,
                StructuralRole::Definition,
                "idris.function_name",
            ),
            (
                1587,
                1589,
                StructuralRole::Definition,
                "idris.function_name",
            ),
            (
                1592,
                1595,
                StructuralRole::Reference,
                "idris.own_body_global",
            ),
            (1247, 1250, StructuralRole::Reference, "idris.unproved_name"),
        ] {
            let fact = candidate(named(root, start, end), role, &cancel, &mut cache)
                .unwrap()
                .unwrap();
            assert_eq!(
                (fact.start, fact.end, fact.role, fact.syntax),
                (start, end, role, syntax)
            );
            assert!(fact.required);
        }
        let signature = candidate(
            named(root, 1168, 1171),
            StructuralRole::Definition,
            &cancel,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(signature.role, StructuralRole::Signature);
    }

    #[test]
    fn duplicate_equations_do_not_create_distinct_written_global_bindings() {
        let source =
            b"global : Type\nglobal = Type\nglobal = Type\nreader : Type\nreader = global\n";
        let tree = parse(source);
        let root = tree.root_node();
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, root, 10000, true);
        let first = source.windows(6).position(|x| x == b"global").unwrap();
        let definition = source[first + 6..]
            .windows(6)
            .position(|x| x == b"global")
            .unwrap()
            + first
            + 6;
        let fact = candidate(
            named(root, definition, definition + 6),
            StructuralRole::Definition,
            &cancel,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fact.role, StructuralRole::Signature);
        let at = source.windows(6).rposition(|x| x == b"global").unwrap();
        let fact = candidate(
            named(root, at, at + 6),
            StructuralRole::Reference,
            &cancel,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fact.syntax, "idris.unproved_name");
    }

    #[test]
    fn written_signature_names_block_implicit_global_binding_guesses() {
        let source =
            b"global : Type\nglobal = Type\nreader : {global : Type} -> Type\nreader = global\n";
        let tree = parse(source);
        let root = tree.root_node();
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, root, 10000, true);
        let at = source.windows(6).rposition(|x| x == b"global").unwrap();
        let fact = candidate(
            named(root, at, at + 6),
            StructuralRole::Reference,
            &cancel,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fact.syntax, "idris.unproved_name");
        assert_eq!((fact.start, fact.end), (at, at + 6));
    }

    #[test]
    fn missing_signature_and_nested_rhs_contexts_remain_unproved() {
        for source in [
            b"global = Type\nreader = global\n".as_slice(),
            b"global : Type\nglobal = Type\nreader : Type\nreader = \\global => global\n"
                .as_slice(),
        ] {
            let tree = parse(source);
            let root = tree.root_node();
            let cancel = Cancellation::new();
            let mut cache = CaptureCache::new(source, root, 10000, true);
            let at = source.windows(6).rposition(|x| x == b"global").unwrap();
            let fact = candidate(
                named(root, at, at + 6),
                StructuralRole::Reference,
                &cancel,
                &mut cache,
            )
            .unwrap()
            .unwrap();
            assert_eq!(fact.syntax, "idris.unproved_name");
        }
    }

    #[test]
    fn exact_bom_root_is_canonicalized_but_partial_context_has_no_definitions() {
        let source = b"\xef\xbb\xbfvalue : Type\nvalue = Type\n";
        let tree = parse(source);
        let root = tree.root_node();
        let cancel = Cancellation::new();
        assert_eq!(root.start_byte(), 3);
        let mut cache = CaptureCache::new(source, root, 10000, true);
        for role in [StructuralRole::Root, StructuralRole::Module] {
            let fact = candidate(root, role, &cancel, &mut cache).unwrap().unwrap();
            assert_eq!((fact.start, fact.end), (0, source.len()));
        }
        let mut incomplete = CaptureCache::new(source, root, 10000, false);
        let at = source.windows(5).rposition(|x| x == b"value").unwrap();
        let fact = candidate(
            named(root, at, at + 5),
            StructuralRole::Definition,
            &cancel,
            &mut incomplete,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (fact.role, fact.syntax, fact.start, fact.end),
            (
                StructuralRole::Signature,
                "idris.native_context_unavailable",
                0,
                source.len()
            )
        );
    }

    #[test]
    fn helper_exhaustion_and_cancellation_remain_typed_refusals() {
        let source = b"value : Type\nvalue = Type\n";
        let tree = parse(source);
        let root = tree.root_node();
        let cancel = Cancellation::new();
        let mut empty = CaptureCache::new(source, root, 0, true);
        assert!(
            candidate(root, StructuralRole::Root, &cancel, &mut empty)
                .unwrap()
                .is_none()
        );
        assert!(empty.exhausted());
        cancel.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, root, 10000, true);
        assert!(matches!(
            candidate(root, StructuralRole::Root, &cancel, &mut cache),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn direct_body_markers_require_the_actual_signature_disjoint_global_operand() {
        let source = include_bytes!("../../tests/fixtures/idris/stlc_small.idr");
        let tree = parse(source);
        let root = tree.root_node();
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, root, 10000, true);
        let marker = candidate(
            named(root, 1592, 1595),
            StructuralRole::Signature,
            &cancel,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (marker.role, marker.syntax, marker.start, marker.end),
            (
                StructuralRole::Signature,
                "idris.direct_global_context",
                1592,
                1595
            )
        );
        assert!(marker.required);
        assert!(
            candidate(
                named(root, 1247, 1250),
                StructuralRole::Signature,
                &cancel,
                &mut cache
            )
            .unwrap()
            .is_none()
        );
        let source =
            b"global : Type\nglobal = Type\nreader : {global : Type} -> Type\nreader = global\n";
        let tree = parse(source);
        let root = tree.root_node();
        let mut cache = CaptureCache::new(source, root, 10000, true);
        let at = source
            .windows(6)
            .rposition(|part| part == b"global")
            .unwrap();
        assert!(
            candidate(
                named(root, at, at + 6),
                StructuralRole::Signature,
                &cancel,
                &mut cache
            )
            .unwrap()
            .is_none()
        );
    }
}
