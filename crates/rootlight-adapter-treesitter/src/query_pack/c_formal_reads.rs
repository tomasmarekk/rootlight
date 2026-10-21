//! Certify written C formal arguments in one ordinary call statement.
//! The complete native census fences directives, recovery and unsupported bodies;
//! includes, callee binding, macro expansion and execution remain unevaluated.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, c_parameters};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    checked: Option<bool>,
    owners: HashMap<&'source str, bool>,
    functions: HashMap<usize, HashSet<(usize, usize)>>,
}

impl<'source> CaptureCache<'source> {
    /// Share one checked native-work allowance across both census and proofs.
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining,
            exhausted: false,
            checked: None,
            owners: HashMap::new(),
            functions: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
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

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.visit(cancel)?
            || node.kind() != "identifier"
            || node.has_error()
            || node.is_missing()
        {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, rest)) = bytes.split_first() else {
            return Ok(None);
        };
        Ok(((*first == b'_' || first.is_ascii_alphabetic())
            && rest.iter().all(|b| *b == b'_' || b.is_ascii_alphanumeric()))
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten())
    }

    fn declarator<'tree>(
        &mut self,
        function: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let Some(mut node) = function.child_by_field_name("declarator") else {
            return Ok(None);
        };
        loop {
            if !self.visit(cancel)? || node.has_error() || node.is_missing() {
                return Ok(None);
            }
            match node.kind() {
                "function_declarator" => return Ok(Some(node)),
                "pointer_declarator" => match node.child_by_field_name("declarator") {
                    Some(inner) => node = inner,
                    None => return Ok(None),
                },
                _ => return Ok(None),
            }
        }
    }

    fn census(&mut self, mut root: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if let Some(allowed) = self.checked {
            return Ok(allowed);
        }
        self.checked = Some(false);
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            root = parent;
        }
        if !self.complete
            || root.kind() != "translation_unit"
            || root.has_error()
            || root.is_missing()
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
            || root.end_byte() != self.source.len()
        {
            return Ok(false);
        }
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)?
                || node.is_error()
                || node.is_missing()
                || (node.kind().starts_with("preproc_")
                    && node.kind() != "preproc_include"
                    && node
                        .parent()
                        .is_none_or(|parent| parent.kind() != "preproc_include"))
            {
                return Ok(false);
            }
            if node.kind() == "function_definition" {
                if node.parent() != Some(root) {
                    return Ok(false);
                }
                let Some(declarator) = self.declarator(node, cancel)? else {
                    return Ok(false);
                };
                let Some(identifier) = declarator.child_by_field_name("declarator") else {
                    return Ok(false);
                };
                let Some(name) = self.name(identifier, cancel)? else {
                    return Ok(false);
                };
                self.owners
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.owners
                    .entry(name)
                    .and_modify(|unique| *unique = false)
                    .or_insert(true);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.checked = Some(true);
                    return Ok(true);
                }
            }
        }
    }

    fn only<'tree>(
        &mut self,
        node: Node<'tree>,
        kind: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            if found.is_some() || child.kind() != kind {
                return Ok(None);
            }
            found = Some(child);
        }
        Ok(found)
    }

    fn inspect(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<HashSet<(usize, usize)>, AdapterError> {
        let mut reads = HashSet::new();
        if !self.census(function, cancel)?
            || !self.visit(cancel)?
            || function.has_error()
            || function.is_missing()
        {
            return Ok(reads);
        }
        let Some(declarator) = self.declarator(function, cancel)? else {
            return Ok(reads);
        };
        let Some(identifier) = declarator.child_by_field_name("declarator") else {
            return Ok(reads);
        };
        let Some(owner) = self.name(identifier, cancel)? else {
            return Ok(reads);
        };
        if self.owners.get(owner) != Some(&true) {
            return Ok(reads);
        }
        let Some(parameters) = declarator.child_by_field_name("parameters") else {
            return Ok(reads);
        };
        let mut names = HashSet::new();
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(reads);
            }
            if parameter.kind() == "comment" {
                continue;
            }
            let Some(identifier) = c_parameters::written_name(parameter, cancel)? else {
                return Ok(reads);
            };
            let Some(name) = self.name(identifier, cancel)? else {
                return Ok(reads);
            };
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !names.insert(name) {
                return Ok(reads);
            }
        }
        let Some(body) = function
            .child_by_field_name("body")
            .filter(|node| node.kind() == "compound_statement")
        else {
            return Ok(reads);
        };
        let Some(statement) = self.only(body, "expression_statement", cancel)? else {
            return Ok(reads);
        };
        let Some(call) = self.only(statement, "call_expression", cancel)? else {
            return Ok(reads);
        };
        let Some(callee) = call.child_by_field_name("function") else {
            return Ok(reads);
        };
        let Some(callee) = self.name(callee, cancel)? else {
            return Ok(reads);
        };
        if names.contains(callee) {
            return Ok(reads);
        }
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Ok(reads);
        };
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(HashSet::new());
            }
            if argument.kind() == "comment" {
                continue;
            }
            let Some(name) = self.name(argument, cancel)? else {
                return Ok(HashSet::new());
            };
            if !names.contains(name) {
                return Ok(HashSet::new());
            }
            reads
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            reads.insert((argument.start_byte(), argument.end_byte()));
        }
        Ok(reads)
    }

    /// Keep required native proof references distinct from optional spelling uses.
    /// The paired signature capture enters the identity query, then deduplicates
    /// with the reference; it never becomes a symbol-signature discriminator.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "function_definition" {
            candidate.syntax = "c.formal_binding_context";
            candidate.required = true;
            return Ok(true);
        }
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature)
            || node.kind() != "identifier"
        {
            return Ok(true);
        }
        // Only direct argument identifiers can receive this certificate. Global
        // initializers and other names keep ordinary syntax without walking owners.
        if node
            .parent()
            .is_none_or(|parent| parent.kind() != "argument_list")
        {
            return Ok(role != StructuralRole::Signature);
        }
        let mut ancestor = node.parent();
        let mut function = None;
        while let Some(parent) = ancestor {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if parent.kind() == "function_definition" {
                function = Some(parent);
                break;
            }
            ancestor = parent.parent();
        }
        let Some(function) = function else {
            return Ok(role != StructuralRole::Signature);
        };
        if !self.functions.contains_key(&function.id()) {
            let proof = self.inspect(function, cancel)?;
            self.functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.functions.insert(function.id(), proof);
        }
        if self
            .functions
            .get(&function.id())
            .is_some_and(|proof| proof.contains(&(node.start_byte(), node.end_byte())))
        {
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "c.formal_value";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature {
            return Ok(false);
        }
        // Refused direct argument reads cannot later acquire a name-only target.
        if node
            .parent()
            .is_some_and(|parent| parent.kind() == "argument_list")
        {
            candidate.syntax = "c.unproven_formal_value";
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }
    fn argument<'a>(tree: &'a tree_sitter::Tree, source: &[u8]) -> Node<'a> {
        let start = source
            .windows(5)
            .rposition(|part| part == b"value")
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        node
    }
    fn capture(node: Node<'_>, role: StructuralRole) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "c.identifier",
            required: false,
            native_depth: 0,
        }
    }
    #[test]
    fn impossible_global_identifier_positions_preserve_references_without_formal_work() {
        let source = b"int original; int table[] = { original };\n";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let at = source
            .windows(8)
            .rposition(|part| part == b"original")
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(at, at + 8)
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        let mut cache = CaptureCache::new(source, 1, true);
        for role in [StructuralRole::Signature, StructuralRole::Reference] {
            let mut output = capture(node, role);
            let retained = cache
                .refine(node, role, &mut output, &Cancellation::new())
                .unwrap();
            assert_eq!(retained, role == StructuralRole::Reference);
            assert!(
                !cache.exhausted(),
                "a global initializer cannot need a formal proof"
            );
            assert!(!output.required);
            assert_eq!(output.syntax, "c.identifier");
        }
    }

    #[test]
    fn bypassed_global_names_do_not_bypass_the_complete_formal_census() {
        let source = b"int ignored; int table[] = { ignored };\n#define value other\nvoid owned(int value) { external(value); }\n";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let at = source
            .windows(7)
            .position(|part| part == b"ignored")
            .unwrap();
        let global = tree
            .root_node()
            .descendant_for_byte_range(at, at + 7)
            .unwrap();
        let mut cache = CaptureCache::new(source, 4096, true);
        for role in [StructuralRole::Signature, StructuralRole::Reference] {
            let mut output = capture(global, role);
            assert_eq!(
                cache
                    .refine(global, role, &mut output, &Cancellation::new())
                    .unwrap(),
                role == StructuralRole::Reference
            );
        }
        let node = argument(&tree, source);
        for role in [StructuralRole::Signature, StructuralRole::Reference] {
            let mut output = capture(node, role);
            cache
                .refine(node, role, &mut output, &Cancellation::new())
                .unwrap();
            assert!(!output.required);
            assert_ne!(output.syntax, "c.formal_value");
            if role == StructuralRole::Reference {
                assert_eq!(output.syntax, "c.unproven_formal_value");
            }
        }
        assert!(!cache.exhausted());
    }

    #[test]
    fn full_census_certifies_only_written_direct_formal_arguments() {
        let source = b"#include \"external.h\"\nvoid owned(int value) { external(value); }\n";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let node = argument(&tree, source);
        let mut cache = CaptureCache::new(source, 4096, true);
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let mut output = capture(node, role);
            assert!(
                cache
                    .refine(node, role, &mut output, &Cancellation::new())
                    .unwrap()
            );
            assert_eq!(output.role, StructuralRole::Reference);
            assert_eq!(output.syntax, "c.formal_value");
            assert!(output.required);
        }
    }
    #[test]
    fn unsupported_bodies_directives_and_competing_headers_cannot_certify_a_formal() {
        for source in [
            "void owned(int value) { int value; external(value); }",
            "void owned(int value) { { external(value); } }",
            "void owned(int value) { if (1) external(value); }",
            "void owned(int value, int value) { external(value); }",
            "void owned(int value[2]) { external(value); }",
            "void owned(int value) { external(value); } void owned(int value) { external(value); }",
            "#define value other\nvoid owned(int value) { external(value); }",
            "#if ACTIVE\nvoid owned(int value) { external(value); }\n#endif",
            "void owned(int value) { external(value);",
        ] {
            let tree = parse(source.as_bytes());
            let node = argument(&tree, source.as_bytes());
            let mut cache = CaptureCache::new(source.as_bytes(), 4096, true);
            let mut output = capture(node, StructuralRole::Reference);
            cache
                .refine(
                    node,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_ne!(output.syntax, "c.formal_value", "{source}");
        }
    }
    #[test]
    fn partial_census_exhaustion_and_cancellation_cannot_leak_a_certificate() {
        let source = b"void owned(int value) { external(value); }";
        let tree = parse(source);
        let node = argument(&tree, source);
        for (maximum, complete) in [(4096, false), (1, true)] {
            let mut cache = CaptureCache::new(source, maximum, complete);
            let mut output = capture(node, StructuralRole::Reference);
            cache
                .refine(
                    node,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_ne!(output.syntax, "c.formal_value");
            assert!(!output.required);
            assert_eq!(cache.exhausted(), maximum == 1);
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut output = capture(node, StructuralRole::Reference);
        assert!(matches!(
            CaptureCache::new(source, 4096, true).refine(
                node,
                StructuralRole::Reference,
                &mut output,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(!output.required);
    }
}
