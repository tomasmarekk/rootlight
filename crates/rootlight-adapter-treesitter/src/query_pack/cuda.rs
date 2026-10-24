//! Certify finite written CUDA launches and scalar-formal arithmetic reads.
//! Certificates supplement ordinary References; CUDA execution, headers,
//! overload resolution and template instantiation remain outside this proof.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    complete: bool,
    initialized: bool,
    remaining: usize,
    exhausted: bool,
    reads: HashMap<(usize, usize), usize>,
    launches: HashMap<(usize, usize), usize>,
}

impl<'source, 'tree> CaptureCache<'source, 'tree> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        limit: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            root,
            complete,
            initialized: false,
            remaining: limit.min(super::HARD_MAX_QUERY_CAPTURES).saturating_mul(64),
            exhausted: false,
            reads: HashMap::new(),
            launches: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(value) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = value;
        Ok(true)
    }

    fn nodes(
        &mut self,
        root: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Vec<Node<'tree>>>, AdapterError> {
        let mut result = Vec::new();
        let mut cursor = root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(None);
            }
            let node = cursor.node();
            if node.is_error() || node.is_missing() || node.has_error() {
                return Ok(None);
            }
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.push(node);
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(Some(result));
                }
            }
        }
    }

    fn word(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.step(cancel)? {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, rest)) = bytes.split_first() else {
            return Ok(None);
        };
        Ok(((*first == b'_' || first.is_ascii_alphabetic())
            && rest
                .iter()
                .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric()))
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten())
    }

    fn function(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'tree>, Node<'tree>)>, AdapterError> {
        if !self.step(cancel)?
            || node.kind() != "function_definition"
            || !node.parent().is_some_and(|parent| parent == self.root)
        {
            return Ok(None);
        }
        let Some(declarator) = node
            .child_by_field_name("declarator")
            .filter(|node| node.kind() == "function_declarator")
        else {
            return Ok(None);
        };
        let Some(name) = declarator
            .child_by_field_name("declarator")
            .filter(|node| node.kind() == "identifier")
        else {
            return Ok(None);
        };
        if self.word(name, cancel)?.is_none() {
            return Ok(None);
        }
        Ok(Some((declarator, name)))
    }

    fn arity(
        &mut self,
        list: Node<'tree>,
        parameters: bool,
        cancel: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        let mut count = 0_usize;
        let mut cursor = list.walk();
        for child in list.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            if parameters && child.kind() != "parameter_declaration" {
                return Ok(None);
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| super::query_failure("cuda-arity-range"))?;
        }
        Ok(Some(count))
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        if !self.complete
            || self.root.kind() != "translation_unit"
            || !matches!(
                self.source.get(..self.root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
            || self.root.end_byte() != self.source.len()
        {
            return Ok(());
        }
        let Some(all) = self.nodes(self.root, cancel)? else {
            return Ok(());
        };
        // Even a later directive can replace a written callable or formal name.
        for node in &all {
            if !self.step(cancel)? {
                return Ok(());
            }
            if node.kind().starts_with("preproc_") && node.kind() != "preproc_include" {
                return Ok(());
            }
        }
        let mut function_names = HashMap::<&str, usize>::new();
        function_names
            .try_reserve(all.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for node in &all {
            if node.kind() == "function_declarator"
                && let Some(name) = node
                    .child_by_field_name("declarator")
                    .filter(|node| node.kind() == "identifier")
                && let Some(word) = self.word(name, cancel)?
            {
                *function_names.entry(word).or_default() += 1;
            }
        }
        for function in all
            .iter()
            .copied()
            .filter(|node| node.kind() == "function_definition")
        {
            let Some((declarator, function_name)) = self.function(function, cancel)? else {
                continue;
            };
            let Some(function_word) = self.word(function_name, cancel)? else {
                continue;
            };
            if function_names.get(function_word) != Some(&1) {
                continue;
            }
            let Some(parameters) = declarator.child_by_field_name("parameters") else {
                continue;
            };
            let Some(body) = function.child_by_field_name("body") else {
                continue;
            };
            let Some(body_nodes) = self.nodes(body, cancel)? else {
                continue;
            };
            let Some(parameter_nodes) = self.nodes(parameters, cancel)? else {
                continue;
            };
            let mut formals = HashMap::<&str, (usize, bool)>::new();
            formals
                .try_reserve(parameter_nodes.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            let mut valid = true;
            for parameter in parameter_nodes
                .iter()
                .copied()
                .filter(|node| node.kind() == "parameter_declaration")
            {
                let Some(written) = parameter.child_by_field_name("declarator") else {
                    valid = false;
                    continue;
                };
                let mut names = Vec::new();
                for node in parameter_nodes.iter().copied() {
                    if !self.step(cancel)? {
                        return Ok(());
                    }
                    if node.kind() == "identifier"
                        && parameter.start_byte() <= node.start_byte()
                        && node.end_byte() <= parameter.end_byte()
                    {
                        names
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        names.push(node);
                    }
                }
                if names.len() != 1 {
                    valid = false;
                    continue;
                }
                let Some(word) = self.word(names[0], cancel)? else {
                    valid = false;
                    continue;
                };
                let scalar = written == names[0]
                    && parameter
                        .child_by_field_name("type")
                        .is_some_and(|node| node.kind() == "primitive_type");
                valid &= formals
                    .insert(word, (names[0].start_byte(), scalar))
                    .is_none();
            }
            if !valid {
                continue;
            }
            let mut local_names = HashSet::new();
            local_names
                .try_reserve(body_nodes.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            let mut forbidden = false;
            for node in &body_nodes {
                if !self.step(cancel)? {
                    return Ok(());
                }
                forbidden |= matches!(
                    node.kind(),
                    "lambda_expression"
                        | "function_definition"
                        | "structured_binding_declarator"
                        | "for_range_loop"
                        | "catch_clause"
                        | "type_definition"
                        | "alias_declaration"
                        | "class_specifier"
                        | "struct_specifier"
                        | "union_specifier"
                );
                if node.kind() == "identifier"
                    && node.parent().is_some_and(|parent| {
                        matches!(
                            parent.kind(),
                            "init_declarator"
                                | "pointer_declarator"
                                | "reference_declarator"
                                | "declaration"
                        ) && parent.child_by_field_name("declarator") == Some(*node)
                    })
                    && let Some(word) = self.word(*node, cancel)?
                {
                    local_names.insert(word);
                }
                if node.kind() == "identifier"
                    && let Some(word) = self.word(*node, cancel)?
                    && formals.contains_key(word)
                    && node.parent().is_some_and(|parent| {
                        matches!(
                            parent.kind(),
                            "assignment_expression" | "update_expression" | "pointer_expression"
                        )
                    })
                {
                    forbidden = true;
                }
            }
            if forbidden {
                continue;
            }
            for node in body_nodes
                .iter()
                .copied()
                .filter(|node| node.kind() == "identifier")
            {
                let Some(word) = self.word(node, cancel)? else {
                    continue;
                };
                if let Some((start, true)) = formals.get(word).copied()
                    && !local_names.contains(word)
                    && node
                        .parent()
                        .is_some_and(|parent| parent.kind() == "binary_expression")
                {
                    self.reads
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.reads
                        .insert((node.start_byte(), node.end_byte()), start);
                }
                let Some(call) = node.parent().filter(|parent| {
                    parent.kind() == "call_expression"
                        && parent.child_by_field_name("function") == Some(node)
                }) else {
                    continue;
                };
                let mut cursor = call.walk();
                if !call
                    .named_children(&mut cursor)
                    .any(|child| child.kind() == "kernel_call_syntax")
                    || local_names.contains(word)
                    || formals.contains_key(word)
                    || function_names.get(word) != Some(&1)
                {
                    continue;
                }
                let mut target = None;
                for kernel in all
                    .iter()
                    .copied()
                    .filter(|node| node.kind() == "function_definition")
                {
                    let Some((declarator, name)) = self.function(kernel, cancel)? else {
                        continue;
                    };
                    let (Some(parameters), Some(arguments)) = (
                        declarator.child_by_field_name("parameters"),
                        call.child_by_field_name("arguments"),
                    ) else {
                        continue;
                    };
                    let (Some(formals), Some(actuals)) = (
                        self.arity(parameters, true, cancel)?,
                        self.arity(arguments, false, cancel)?,
                    ) else {
                        continue;
                    };
                    if formals != actuals {
                        continue;
                    }
                    let mut cursor = kernel.walk();
                    if self.word(name, cancel)? == Some(word)
                        && kernel
                            .children(&mut cursor)
                            .any(|child| child.kind() == "__global__")
                    {
                        target = Some(name.start_byte());
                    }
                }
                if let Some(target) = target {
                    self.launches
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.launches
                        .insert((node.start_byte(), node.end_byte()), target);
                }
            }
        }
        Ok(())
    }

    pub(super) fn refine(
        &mut self,
        node: Node<'tree>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        self.initialize(cancel)?;
        if !self.step(cancel)? {
            return Ok(Some(false));
        }
        if role == StructuralRole::Root
            && node == self.root
            && self.complete
            && !self.root.has_error()
            && matches!(
                self.source.get(..self.root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            candidate.start = 0;
            candidate.end = self.source.len();
            return Ok(Some(true));
        }
        if role == StructuralRole::Signature && node == self.root {
            if !self.complete || self.root.has_error() || self.exhausted {
                return Ok(Some(false));
            }
            candidate.syntax = "cuda.clean_file";
            candidate.start = 0;
            candidate.end = self.source.len();
            candidate.required = true;
            return Ok(Some(true));
        }
        if node.kind() == "identifier" {
            let key = (node.start_byte(), node.end_byte());
            let selected = self
                .reads
                .get(&key)
                .map(|start| (*start, "cuda.scalar_read_target"))
                .or_else(|| {
                    self.launches
                        .get(&key)
                        .map(|start| (*start, "cuda.kernel_launch_target"))
                });
            if let Some((start, syntax)) = selected {
                if role == StructuralRole::Signature {
                    candidate.start = if syntax == "cuda.kernel_launch_target" {
                        node.start_byte()
                    } else {
                        start
                    };
                    candidate.syntax = syntax;
                    candidate.required = true;
                    return Ok(Some(true));
                }
                if role == StructuralRole::Reference {
                    candidate.required = true;
                    return Ok(Some(true));
                }
            }
            if role == StructuralRole::Signature && candidate.syntax == "cuda.identifier" {
                return Ok(Some(false));
            }
        }
        if role == StructuralRole::Scope && self.complete {
            for (start, end) in self.reads.keys().chain(self.launches.keys()) {
                cancel.check()?;
                let Some(next) = self.remaining.checked_sub(1) else {
                    self.exhausted = true;
                    return Ok(Some(false));
                };
                self.remaining = next;
                if node.start_byte() <= *start && *end <= node.end_byte() {
                    candidate.required = true;
                    return Ok(Some(true));
                }
            }
        }
        if role == StructuralRole::Signature && node.kind() == "function_declarator" {
            if !self.complete {
                return Ok(Some(false));
            }
            let Some(function) = node
                .parent()
                .filter(|parent| parent.kind() == "function_definition")
            else {
                return Ok(Some(false));
            };
            candidate.start = function.start_byte();
            candidate.end = function.end_byte();
            candidate.syntax = "cuda.owned_function";
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Signature
            && node.kind() == "function_definition"
            && candidate.syntax == "cuda.function"
        {
            return Ok(Some(false));
        }
        if node.kind() == "field_declaration"
            && role == StructuralRole::Declaration
            && node
                .child_by_field_name("declarator")
                .is_some_and(|value| value.kind() == "field_identifier")
        {
            candidate.syntax = if candidate.syntax.starts_with("cuda.") {
                "cuda.field"
            } else {
                "cpp.field"
            };
            return Ok(Some(true));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(source: &[u8]) -> tree_sitter::Tree {
        let cancel = Cancellation::new();
        let scope = crate::runtime::cuda_scope::begin(&cancel, 1_048_576).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cuda::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        drop(parser);
        crate::runtime::cuda_scope::finish(scope, &cancel, None).unwrap();
        tree
    }

    #[test]
    fn original_native_launch_and_scalar_formal_certificates_use_unique_exact_sites() {
        let cancel = Cancellation::new();
        for (source, site, target, launch) in [
            (
                include_bytes!(
                    "../../tests/fixtures/cuda_original/dev/cuda/crossentropy_forward.cu"
                )
                .as_slice(),
                (2107, 2135),
                1259,
                true,
            ),
            (
                include_bytes!("../../tests/fixtures/cuda_original/llmc/fused_classifier.cuh")
                    .as_slice(),
                (682, 685),
                528,
                false,
            ),
        ] {
            let tree = native(source);
            let mut proof = CaptureCache::new(source, tree.root_node(), 16_384, true);
            proof.initialize(&cancel).unwrap();
            assert!(!proof.exhausted());
            let selected = if launch {
                &proof.launches
            } else {
                &proof.reads
            };
            assert_eq!(selected.get(&site), Some(&target));
            let mut cursor = tree.root_node().walk();
            let mut found = None;
            loop {
                let node = cursor.node();
                if node.kind() == "identifier" && (node.start_byte(), node.end_byte()) == site {
                    assert!(found.replace(node).is_none());
                }
                if cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break;
                    }
                }
                if cursor.node() == tree.root_node() {
                    break;
                }
            }
            let node = found.unwrap();
            let mut candidate = QueryCandidate {
                start: site.0,
                end: site.1,
                role: StructuralRole::Reference,
                syntax: "cpp.identifier",
                required: false,
                native_depth: 7,
            };
            assert_eq!(
                proof
                    .refine(node, StructuralRole::Reference, &mut candidate, &cancel)
                    .unwrap(),
                Some(true)
            );
            assert_eq!(
                (
                    candidate.start,
                    candidate.end,
                    candidate.syntax,
                    candidate.native_depth
                ),
                (site.0, site.1, "cpp.identifier", 7)
            );
            assert!(candidate.required);
        }
    }

    #[test]
    fn partial_input_and_exhausted_census_cannot_certify_native_names() {
        let source = b"int owned(int value) { return value * 2; }";
        let tree = native(source);
        let cancel = Cancellation::new();
        let mut partial = CaptureCache::new(source, tree.root_node(), 16_384, false);
        partial.initialize(&cancel).unwrap();
        assert!(partial.reads.is_empty());
        let mut empty = CaptureCache::new(source, tree.root_node(), 0, true);
        empty.initialize(&cancel).unwrap();
        assert!(empty.exhausted());
        assert!(empty.reads.is_empty());
    }
}
