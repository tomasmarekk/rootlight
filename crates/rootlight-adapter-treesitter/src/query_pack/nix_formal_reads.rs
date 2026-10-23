//! Certify ordinary Nix value uses against exact written formal ancestors.
//! Lexical shadowing remains the binding plan's responsibility; the certificate
//! authorizes a Read facet only when both independently select the same formal.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    clean: bool,
    initial: usize,
    remaining: usize,
    exhausted: bool,
    targets: HashMap<usize, Option<usize>>,
}

impl<'source> CaptureCache<'source> {
    /// Share one checked native-work allowance between references and metadata.
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'_>,
        remaining: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            clean: complete
                && root.kind() == "source_code"
                && !root.has_error()
                && !root.is_missing()
                && (root.start_byte() == 0
                    || (root.start_byte() == 3 && source.starts_with(b"\xef\xbb\xbf")))
                && root.end_byte() == source.len(),
            initial: remaining,
            remaining,
            exhausted: false,
            targets: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(super) const fn initial_work(&self) -> usize {
        self.initial
    }

    pub(super) const fn remaining_work(&self) -> usize {
        self.remaining
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

    fn same_name(
        &mut self,
        candidate: Node<'_>,
        name: &[u8],
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.visit(cancel)? || candidate.kind() != "identifier" {
            return Ok(false);
        }
        let Some(written) = self.source.get(candidate.byte_range()) else {
            return Ok(false);
        };
        if written.len() != name.len() {
            return Ok(false);
        }
        for (left, right) in written.chunks(64).zip(name.chunks(64)) {
            if !self.visit(cancel)? || left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn inspect(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        if !self.clean || !self.visit(cancel)? {
            return Ok(None);
        }
        let Some(identifier) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let Some(name) = self.source.get(identifier.byte_range()) else {
            return Ok(None);
        };
        let mut expression = node;
        while let Some(parent) = expression.parent()
            && parent.kind() == "parenthesized_expression"
        {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            expression = parent;
        }
        if expression.parent().is_some_and(|parent| {
            parent.kind() == "apply_expression"
                && parent.child_by_field_name("function") == Some(expression)
        }) {
            return Ok(None);
        }
        let mut ancestor = node.parent();
        while let Some(owner) = ancestor {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if owner.kind() == "function_expression"
                && owner.child_by_field_name("body").is_some_and(|body| {
                    body.start_byte() <= node.start_byte() && node.end_byte() <= body.end_byte()
                })
            {
                let mut target = None;
                if let Some(universal) = owner.child_by_field_name("universal")
                    && self.same_name(universal, name, cancel)?
                {
                    target = Some(universal.start_byte());
                }
                if let Some(formals) = owner.child_by_field_name("formals") {
                    let mut cursor = formals.walk();
                    for formal in formals.named_children(&mut cursor) {
                        if !self.visit(cancel)? {
                            return Ok(None);
                        }
                        if formal.kind() == "formal"
                            && let Some(identifier) = formal.child_by_field_name("name")
                            && self.same_name(identifier, name, cancel)?
                        {
                            if target.is_some() {
                                return Ok(None);
                            }
                            target = Some(identifier.start_byte());
                        }
                    }
                }
                if target.is_some() {
                    return Ok(target);
                }
            }
            ancestor = owner.parent();
        }
        Ok(None)
    }

    /// Retain exact native endpoints beside the original Reference spelling.
    /// Signature metadata never contributes a declaration signature or identity.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "source_code" {
            if !self.clean {
                return Ok(false);
            }
            candidate.start = 0;
            candidate.end = self.source.len();
            candidate.syntax = "nix.formal_clean_file";
            candidate.required = true;
            return Ok(true);
        }
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature)
            || node.kind() != "variable_expression"
        {
            return Ok(true);
        }
        let target = if let Some(target) = self.targets.get(&node.id()) {
            *target
        } else {
            let target = self.inspect(node, cancel)?;
            self.targets
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.targets.insert(node.id(), target);
            target
        };
        let Some(target) = target else {
            return Ok(role != StructuralRole::Signature);
        };
        if self.exhausted {
            return Ok(false);
        }
        candidate.required = true;
        if role == StructuralRole::Signature {
            candidate.start = target;
            candidate.syntax = "nix.formal_read_target";
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_nix::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn certificate(source: &str, complete: bool, work: usize) -> (Option<usize>, bool) {
        let tree = parse(source);
        let start = source.rfind("value").unwrap();
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        while node.kind() != "variable_expression" {
            let Some(parent) = node.parent() else {
                return (None, false);
            };
            node = parent;
        }
        let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), work, complete);
        let mut candidate = QueryCandidate {
            start,
            end: start + 5,
            role: StructuralRole::Signature,
            syntax: "nix.function_header",
            required: false,
            native_depth: 0,
        };
        let retained = cache
            .refine(
                node,
                StructuralRole::Signature,
                &mut candidate,
                &Cancellation::new(),
            )
            .unwrap();
        (
            (retained && candidate.syntax == "nix.formal_read_target").then_some(candidate.start),
            cache.exhausted(),
        )
    }

    #[test]
    fn native_formal_endpoint_survives_renaming_nested_capture_and_expression_contexts() {
        for source in [
            "value: value",
            "{ value }: (inner: value)",
            "{ value }: import value {}",
            "{ value }: value.field",
            "{ value }: \"${value}\"",
            "{ value }: ''${value}''",
        ] {
            assert_eq!(
                certificate(source, true, 4096),
                (source.find("value"), false),
                "{source}"
            );
        }
    }

    #[test]
    fn invocation_defaults_recovery_and_partial_inputs_never_authorize_a_formal_read() {
        for source in [
            "value: value 1",
            "value: (value) 1",
            "{ value ? value }: 1",
            "{ value }: value +",
            "{ value, value }: value",
            "let value = 1; in value",
        ] {
            assert_eq!(certificate(source, true, 4096).0, None, "{source}");
        }
        assert_eq!(certificate("value: value", false, 4096), (None, false));
    }

    #[test]
    fn helper_work_and_cancellation_are_checked_before_publishing_certificates() {
        for work in 0..4 {
            assert_eq!(
                certificate("{ value }: (inner: value)", true, work),
                (None, true)
            );
        }
        let source = "value: value";
        let tree = parse(source);
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 4096, true);
        let mut candidate = QueryCandidate {
            start: 0,
            end: source.len(),
            role: StructuralRole::Signature,
            syntax: "nix.function_header",
            required: false,
            native_depth: 0,
        };
        assert!(matches!(
            cache.refine(
                tree.root_node(),
                StructuralRole::Signature,
                &mut candidate,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn complete_original_native_capture_points_to_the_outer_formal() {
        let source = include_str!("../../tests/fixtures/nix_captured_formal/flake.nix");
        let tree = parse(source);
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(317, 324)
            .unwrap();
        while node.kind() != "variable_expression" {
            node = node.parent().unwrap();
        }
        let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true);
        assert_eq!(
            cache.inspect(node, &Cancellation::new()).unwrap(),
            Some(194),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut candidate = QueryCandidate {
            start: 0,
            end: 582,
            role: StructuralRole::Signature,
            syntax: "nix.function_header",
            required: false,
            native_depth: 0,
        };
        assert!(
            cache
                .refine(
                    tree.root_node(),
                    StructuralRole::Signature,
                    &mut candidate,
                    &Cancellation::new()
                )
                .unwrap()
        );
        assert_eq!((candidate.start, candidate.end), (0, source.len()));
    }
}
