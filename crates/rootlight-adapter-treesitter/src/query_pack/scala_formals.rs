//! Certify ordinary written Scala value formals in a closed expression body.
//! Imports and external calls stay unevaluated. Native fields establish lexical
//! ownership; unsupported binders or mutations invalidate the entire method proof.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    methods: HashMap<usize, HashSet<(usize, usize)>>,
}

impl<'source> CaptureCache<'source> {
    /// Share one checked work allowance across header, body and name comparisons.
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining,
            exhausted: false,
            methods: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.visit(cancel)? || node.is_missing() || node.has_error() {
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
        if bytes == b"_" {
            return Ok(None);
        }
        let Some((first, rest)) = bytes.split_first() else {
            return Ok(None);
        };
        Ok(((*first == b'_' || first.is_ascii_alphabetic())
            && rest.iter().all(|b| *b == b'_' || b.is_ascii_alphanumeric()))
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten())
    }

    fn context(&mut self, function: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.complete
            || function.kind() != "function_definition"
            || function.has_error()
            || function.is_missing()
        {
            return Ok(false);
        }
        let mut current = function;
        while let Some(parent) = current.parent() {
            if !self.visit(cancel)? || parent.is_error() || parent.is_missing() {
                return Ok(false);
            }
            match parent.kind() {
                "compilation_unit" => {
                    let Some(prefix) = self.source.get(..parent.start_byte()) else {
                        return Ok(false);
                    };
                    let Some(suffix) = self.source.get(parent.end_byte()..) else {
                        return Ok(false);
                    };
                    for part in [
                        prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix),
                        suffix,
                    ] {
                        for bytes in part.chunks(64) {
                            if !self.visit(cancel)? || !bytes.iter().all(u8::is_ascii_whitespace) {
                                return Ok(false);
                            }
                        }
                    }
                    return Ok(parent.parent().is_none());
                }
                "template_body" => {}
                "object_definition" | "class_definition" | "trait_definition"
                | "package_object" => {
                    let mut cursor = parent.walk();
                    let mut body = false;
                    for candidate in parent.children_by_field_name("body", &mut cursor) {
                        if !self.visit(cancel)? {
                            return Ok(false);
                        }
                        body |= candidate == current;
                    }
                    if !body {
                        return Ok(false);
                    }
                }
                "package_clause" => {
                    if parent.child_by_field_name("body") != Some(current) {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
            current = parent;
        }
        Ok(false)
    }

    fn parameters(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<HashSet<&'source str>>, AdapterError> {
        let Some(name) = function.child_by_field_name("name") else {
            return Ok(None);
        };
        if name.kind() != "identifier" || self.word(name, cancel)?.is_none() {
            return Ok(None);
        }
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(None);
        };
        let mut parameter_list = None;
        let mut cursor = function.walk();
        for child in function.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child == name || child == body || child.kind() == "comment" {
                continue;
            }
            if function.child_by_field_name("return_type") == Some(child) {
                if child.kind() != "type_identifier" || self.word(child, cancel)?.is_none() {
                    return Ok(None);
                }
            } else if child.kind() == "parameters" && parameter_list.is_none() {
                parameter_list = Some(child);
            } else {
                return Ok(None);
            }
        }
        let Some(parameters) = parameter_list else {
            return Ok(None);
        };
        let mut names = HashSet::new();
        let mut cursor = parameters.walk();
        for parameter in parameters.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(parameter.kind(), "(" | ")" | "," | "comment") {
                continue;
            }
            if parameter.kind() != "parameter" {
                return Ok(None);
            }
            let Some(identifier) = parameter.child_by_field_name("name") else {
                return Ok(None);
            };
            let Some(ty) = parameter.child_by_field_name("type") else {
                return Ok(None);
            };
            if identifier.kind() != "identifier"
                || ty.kind() != "type_identifier"
                || self.word(ty, cancel)?.is_none()
            {
                return Ok(None);
            }
            let mut inner = parameter.walk();
            for child in parameter.children(&mut inner) {
                if !self.visit(cancel)? {
                    return Ok(None);
                }
                if child != identifier && child != ty && !matches!(child.kind(), ":" | "comment") {
                    return Ok(None);
                }
            }
            let Some(name) = self.word(identifier, cancel)? else {
                return Ok(None);
            };
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !names.insert(name) {
                return Ok(None);
            }
        }
        Ok(Some(names))
    }

    fn inspect(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<HashSet<(usize, usize)>, AdapterError> {
        let mut reads = HashSet::new();
        if !self.context(function, cancel)? {
            return Ok(reads);
        }
        let Some(names) = self.parameters(function, cancel)? else {
            return Ok(reads);
        };
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(reads);
        };
        let mut cursor = body.walk();
        loop {
            let node = cursor.node();
            if !self.visit(cancel)? || node.is_missing() || node.is_error() {
                return Ok(HashSet::new());
            }
            let mut descend = true;
            if node.is_named() {
                match node.kind() {
                    "identifier" => {
                        let Some(name) = self.word(node, cancel)? else {
                            return Ok(HashSet::new());
                        };
                        let excluded = node.parent().is_some_and(|parent| {
                            (parent.kind() == "field_expression"
                                && parent.child_by_field_name("field") == Some(node))
                                || (parent.kind() == "call_expression"
                                    && parent.child_by_field_name("function") == Some(node))
                                || (parent.kind() == "infix_expression"
                                    && parent.child_by_field_name("operator") == Some(node))
                        });
                        if !excluded && names.contains(name) {
                            reads
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            reads.insert((node.start_byte(), node.end_byte()));
                        }
                    }
                    "operator_identifier" => {
                        if !node.parent().is_some_and(|parent| {
                            parent.kind() == "infix_expression"
                                && parent.child_by_field_name("operator") == Some(node)
                        }) || !matches!(
                            self.source.get(node.byte_range()),
                            Some(
                                b"+" | b"-"
                                    | b"*"
                                    | b"/"
                                    | b"%"
                                    | b"=="
                                    | b"!="
                                    | b"<"
                                    | b">"
                                    | b"<="
                                    | b">="
                                    | b"&&"
                                    | b"||"
                                    | b"&"
                                    | b"|"
                            )
                        ) {
                            return Ok(HashSet::new());
                        }
                    }
                    "infix_expression"
                    | "field_expression"
                    | "call_expression"
                    | "arguments"
                    | "parenthesized_expression" => {}
                    "string"
                    | "character_literal"
                    | "integer_literal"
                    | "floating_point_literal"
                    | "boolean_literal"
                    | "null_literal"
                    | "unit"
                    | "comment" => descend = false,
                    _ => return Ok(HashSet::new()),
                }
            } else if matches!(node.kind(), "using" | "=" | "implicit") {
                return Ok(HashSet::new());
            }
            if descend && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(reads);
                }
            }
        }
    }

    /// Convert separate identity carriers without suppressing ordinary references.
    /// A failed proof retains the optional spelling use behind a resolver fence.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Expression && node.kind() == "function_definition" {
            if !self.methods.contains_key(&node.id()) {
                let reads = self.inspect(node, cancel)?;
                self.methods
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.methods.insert(node.id(), reads);
            }
            if self.exhausted || self.methods.get(&node.id()).is_none_or(HashSet::is_empty) {
                return Ok(false);
            }
            candidate.role = StructuralRole::Signature;
            candidate.syntax = "scala.formal_context";
            candidate.required = true;
            return Ok(true);
        }
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature)
            || node.kind() != "identifier"
        {
            return Ok(true);
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
        if !self.methods.contains_key(&function.id()) {
            let reads = self.inspect(function, cancel)?;
            self.methods
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.methods.insert(function.id(), reads);
        }
        if !self.exhausted
            && self
                .methods
                .get(&function.id())
                .is_some_and(|reads| reads.contains(&(node.start_byte(), node.end_byte())))
        {
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "scala.ordinary_formal";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature {
            return Ok(false);
        }
        candidate.syntax = "scala.unproven_formal";
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_scala::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn candidate(node: Node<'_>, role: StructuralRole) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "scala.identifier",
            required: false,
            native_depth: 0,
        }
    }

    #[test]
    fn original_native_fields_certify_both_reference_roles_and_full_context() {
        let source = include_bytes!("../../tests/fixtures/scala_formal/endpoint.scala");
        let tree = parse(source);
        let leaf = tree
            .root_node()
            .descendant_for_byte_range(1425, 1429)
            .unwrap();
        assert_eq!(leaf.kind(), "identifier");
        let mut function = leaf;
        while function.kind() != "function_definition" {
            function = function.parent().unwrap();
        }
        assert_eq!(function.byte_range(), 1375..1440);
        assert!(!function.has_error());
        let mut cache = CaptureCache::new(source, 16_384, true);
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let mut output = candidate(leaf, role);
            assert!(
                cache
                    .refine(leaf, role, &mut output, &Cancellation::new())
                    .unwrap()
            );
            assert_eq!(output.role, StructuralRole::Reference);
            assert_eq!(output.syntax, "scala.ordinary_formal");
            assert!(output.required);
        }
        let mut context = candidate(function, StructuralRole::Expression);
        assert!(
            cache
                .refine(
                    function,
                    StructuralRole::Expression,
                    &mut context,
                    &Cancellation::new()
                )
                .unwrap()
        );
        assert_eq!(context.role, StructuralRole::Signature);
        assert_eq!(context.syntax, "scala.formal_context");
        assert_eq!((context.start, context.end), (1375, 1440));
    }

    #[test]
    fn native_whole_method_barriers_preserve_optional_spelling_uses() {
        for source in [
            "object Catalog { def owned(value: Int) = { val value = 1; value } }",
            "object Catalog { def owned(value: Int) = { import other._; value } }",
            "object Catalog { def owned(value: Int) = ((value: Int) => value) }",
            "object Catalog { def owned(value: Int = other) = value }",
            "object Catalog { def owned(value: Int, value: Int) = value }",
            "object Catalog { def owned(implicit value: Int) = value }",
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "{source}");
            let start = source.rfind("value").unwrap();
            let leaf = tree
                .root_node()
                .descendant_for_byte_range(start, start + 5)
                .unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), 4096, true);
            let mut optional = candidate(leaf, StructuralRole::Reference);
            assert!(
                cache
                    .refine(
                        leaf,
                        StructuralRole::Reference,
                        &mut optional,
                        &Cancellation::new()
                    )
                    .unwrap()
            );
            assert_eq!(optional.syntax, "scala.unproven_formal");
            assert!(!optional.required);
            let mut carrier = candidate(leaf, StructuralRole::Signature);
            assert!(
                !cache
                    .refine(
                        leaf,
                        StructuralRole::Signature,
                        &mut carrier,
                        &Cancellation::new()
                    )
                    .unwrap()
            );
        }
    }

    #[test]
    fn partial_context_work_exhaustion_and_cancellation_cannot_certify_a_formal() {
        let source = b"object Catalog { def owned(value: Int) = external(value) }";
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let start = source
            .windows(5)
            .rposition(|part| part == b"value")
            .unwrap();
        let leaf = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        for (maximum, complete) in [(4096, false), (1, true)] {
            let mut cache = CaptureCache::new(source, maximum, complete);
            let mut output = candidate(leaf, StructuralRole::Reference);
            cache
                .refine(
                    leaf,
                    StructuralRole::Reference,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_ne!(output.syntax, "scala.ordinary_formal");
            assert!(!output.required);
            assert_eq!(cache.exhausted(), maximum == 1);
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut output = candidate(leaf, StructuralRole::Reference);
        assert!(matches!(
            CaptureCache::new(source, 4096, true).refine(
                leaf,
                StructuralRole::Reference,
                &mut output,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(!output.required);
    }
}
