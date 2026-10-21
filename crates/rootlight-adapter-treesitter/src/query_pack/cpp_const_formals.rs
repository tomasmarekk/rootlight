//! Certify const lvalue-reference formals used as ordinary subscript receivers.
//! A complete selected-function census excludes hidden binders and mutations;
//! recovery elsewhere, includes, overloads and template calls remain separate gaps.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

struct FunctionProof<'source> {
    names: HashSet<&'source str>,
    receivers: HashSet<(usize, usize)>,
    complete: bool,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete_input: bool,
    remaining: usize,
    exhausted: bool,
    functions: HashMap<usize, FunctionProof<'source>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete_input: bool) -> Self {
        Self {
            source,
            complete_input,
            remaining: maximum
                .min(super::HARD_MAX_QUERY_CAPTURES)
                .saturating_mul(64),
            exhausted: false,
            functions: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
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
        if !self.step(cancel)?
            || node.kind() != "identifier"
            || node.has_error()
            || node.is_missing()
        {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, rest)) = raw.split_first() else {
            return Ok(None);
        };
        Ok(((*first == b'_' || first.is_ascii_alphabetic())
            && rest
                .iter()
                .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric()))
        .then(|| std::str::from_utf8(raw).ok())
        .flatten())
    }

    fn owner<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut current = Some(node);
        while let Some(node) = current {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if node.kind() == "function_definition" {
                return Ok(Some(node));
            }
            current = node.parent();
        }
        Ok(None)
    }

    fn parameter<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'tree>, bool)>, AdapterError> {
        if !self.step(cancel)?
            || node.kind() != "parameter_declaration"
            || node.has_error()
            || node.is_missing()
        {
            return Ok(None);
        }
        let Some(list) = node
            .parent()
            .filter(|parent| parent.kind() == "parameter_list")
        else {
            return Ok(None);
        };
        let Some(callable) = list.parent().filter(|parent| {
            parent.kind() == "function_declarator"
                && parent.child_by_field_name("parameters") == Some(list)
        }) else {
            return Ok(None);
        };
        if !callable.parent().is_some_and(|parent| {
            parent.kind() == "function_definition"
                && parent.child_by_field_name("declarator") == Some(callable)
        }) {
            return Ok(None);
        }
        let Some(declarator) = node.child_by_field_name("declarator") else {
            return Ok(None);
        };
        let name = match declarator.kind() {
            "identifier" => declarator,
            "reference_declarator" | "pointer_declarator" => {
                let mut cursor = declarator.walk();
                let mut selected = None;
                for child in declarator.named_children(&mut cursor) {
                    if !self.step(cancel)? || child.kind() != "identifier" || selected.is_some() {
                        return Ok(None);
                    }
                    selected = Some(child);
                }
                let Some(name) = selected else {
                    return Ok(None);
                };
                name
            }
            _ => return Ok(None),
        };
        if self.word(name, cancel)?.is_none() {
            return Ok(None);
        }
        let mut constant = false;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if child.kind() == "type_qualifier" {
                let Some(raw) = self.source.get(child.byte_range()) else {
                    return Ok(None);
                };
                if raw != b"const" || constant {
                    return Ok(None);
                }
                constant = true;
            } else if Some(child) != node.child_by_field_name("type") && child != declarator {
                return Ok(None);
            }
        }
        // An rvalue reference has the same node kind as an lvalue reference.
        // Check the actual anonymous operator instead of borrowing its name.
        let reference = declarator.kind() == "reference_declarator"
            && declarator
                .child(0)
                .is_some_and(|operator| operator.kind() == "&");
        Ok(Some((name, constant && reference)))
    }

    fn written_parameter(
        &mut self,
        parameter: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some((name, _)) = self.parameter(parameter, cancel)? else {
            return Ok(false);
        };
        let Some(list) = parameter.parent() else {
            return Ok(false);
        };
        let Some(callable) = list.parent() else {
            return Ok(false);
        };
        let Some(function) = callable.parent() else {
            return Ok(false);
        };
        if !self.complete_input || function.has_error() || function.is_missing() {
            return Ok(false);
        }
        let Some(function_name) = callable.child_by_field_name("declarator") else {
            return Ok(false);
        };
        if self.word(function_name, cancel)?.is_none() {
            return Ok(false);
        }
        let Some(name_text) = self.word(name, cancel)? else {
            return Ok(false);
        };
        let mut names = 0_usize;
        let mut cursor = list.walk();
        for peer in list.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if let Some((peer_name, _)) = self.parameter(peer, cancel)?
                && self.word(peer_name, cancel)? == Some(name_text)
            {
                names = names.saturating_add(1);
            }
        }
        if names != 1 {
            return Ok(false);
        }
        let mut current = function;
        while let Some(parent) = current.parent() {
            if !self.step(cancel)? || parent.is_error() || parent.is_missing() {
                return Ok(false);
            }
            match parent.kind() {
                "namespace_definition"
                | "declaration_list"
                | "preproc_if"
                | "preproc_ifdef"
                | "preproc_else"
                | "preproc_elif"
                | "preproc_elifdef" => {}
                // Written ownership survives a conditional source branch;
                // active preprocessing and receiver binding remain unproved.
                "translation_unit" => return Ok(parent.parent().is_none()),
                _ => return Ok(false),
            }
            current = parent;
        }
        Ok(false)
    }

    fn context<'tree>(
        &mut self,
        owner: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.complete_input || owner.has_error() || owner.is_missing() {
            return Ok(None);
        }
        let mut current = owner;
        while let Some(parent) = current.parent() {
            if !self.step(cancel)? || parent.is_missing() || parent.is_error() {
                return Ok(None);
            }
            match parent.kind() {
                "namespace_definition" | "declaration_list" => {}
                "translation_unit" => {
                    let Some(prefix) = self.source.get(..parent.start_byte()) else {
                        return Ok(None);
                    };
                    let Some(suffix) = self.source.get(parent.end_byte()..) else {
                        return Ok(None);
                    };
                    for raw in [
                        prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix),
                        suffix,
                    ] {
                        for chunk in raw.chunks(64) {
                            if !self.step(cancel)? || !chunk.iter().all(u8::is_ascii_whitespace) {
                                return Ok(None);
                            }
                        }
                    }
                    // Unrelated recovery is retained by the parse report. Only
                    // the selected owner must be clean for this lexical proof.
                    return Ok(parent.parent().is_none().then_some(parent));
                }
                _ => return Ok(None),
            }
            current = parent;
        }
        Ok(None)
    }

    fn macros_clear(
        &mut self,
        root: Node<'_>,
        names: &HashSet<&str>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut walk = root.walk();
        loop {
            let node = walk.node();
            if !self.step(cancel)? {
                return Ok(false);
            }
            if matches!(node.kind(), "preproc_def" | "preproc_function_def") {
                let Some(name) = node.child_by_field_name("name") else {
                    return Ok(false);
                };
                let Some(word) = self.word(name, cancel)? else {
                    return Ok(false);
                };
                if names.contains(word) {
                    return Ok(false);
                }
            }
            if walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    return Ok(true);
                }
            }
        }
    }

    fn inspect(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<FunctionProof<'source>, AdapterError> {
        let mut proof = FunctionProof {
            names: HashSet::new(),
            receivers: HashSet::new(),
            complete: false,
        };
        let Some(callable) = owner
            .child_by_field_name("declarator")
            .filter(|node| node.kind() == "function_declarator")
        else {
            return Ok(proof);
        };
        let Some(function_name) = callable.child_by_field_name("declarator") else {
            return Ok(proof);
        };
        let Some(function_word) = self.word(function_name, cancel)? else {
            return Ok(proof);
        };
        let Some(list) = callable.child_by_field_name("parameters") else {
            return Ok(proof);
        };
        let mut eligible = HashSet::new();
        let mut valid = true;
        let mut cursor = list.walk();
        for parameter in list.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(proof);
            }
            if parameter.kind() == "comment" {
                continue;
            }
            let Some((name, constant)) = self.parameter(parameter, cancel)? else {
                valid = false;
                continue;
            };
            let Some(word) = self.word(name, cancel)? else {
                valid = false;
                continue;
            };
            proof
                .names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !proof.names.insert(word) {
                valid = false
            }
            if constant {
                eligible
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                eligible.insert(word);
            }
        }
        if !valid || eligible.is_empty() {
            return Ok(proof);
        }
        let Some(root) = self.context(owner, cancel)? else {
            return Ok(proof);
        };
        let mut macro_names = HashSet::new();
        macro_names
            .try_reserve(proof.names.len().saturating_add(1))
            .map_err(|_| SinkError::AllocationFailed)?;
        macro_names.extend(proof.names.iter().copied());
        macro_names.insert(function_word);
        if !self.macros_clear(root, &macro_names, cancel)? {
            return Ok(proof);
        }
        let Some(body) = owner
            .child_by_field_name("body")
            .filter(|node| node.kind() == "compound_statement")
        else {
            return Ok(proof);
        };
        let mut receivers = HashSet::new();
        let mut walk = body.walk();
        loop {
            let node = walk.node();
            if !self.step(cancel)? || node.has_error() || node.is_missing() || node.is_error() {
                return Ok(proof);
            }
            if node.is_named() {
                match node.kind() {
                    "compound_statement" if node == body => {}
                    "comment"
                    | "expression_statement"
                    | "return_statement"
                    | "call_expression"
                    | "argument_list"
                    | "field_expression"
                    | "subscript_expression"
                    | "subscript_argument_list"
                    | "number_literal"
                    | "string_literal"
                    | "string_content"
                    | "escape_sequence"
                    | "char_literal"
                    | "true"
                    | "false"
                    | "null"
                    | "qualified_identifier"
                    | "namespace_identifier"
                    | "type_identifier"
                    | "field_identifier"
                    | "template_method"
                    | "template_argument_list"
                    | "type_descriptor"
                    | "primitive_type" => {}
                    "identifier" => {
                        let Some(word) = self.word(node, cancel)? else {
                            return Ok(proof);
                        };
                        if eligible.contains(word)
                            && node.parent().is_some_and(|parent| {
                                parent.kind() == "subscript_expression"
                                    && parent.child_by_field_name("argument") == Some(node)
                            })
                        {
                            receivers
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            receivers.insert((node.start_byte(), node.end_byte()));
                        }
                    }
                    // No local declarations, nested scopes, lambdas, templates
                    // that introduce binders, directives or mutation operators.
                    _ => return Ok(proof),
                }
            }
            if walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    proof.receivers = receivers;
                    proof.complete = true;
                    return Ok(proof);
                }
            }
        }
    }

    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.step(cancel)? {
            return Ok(false);
        }
        if role == StructuralRole::Declaration && node.kind() == "parameter_declaration" {
            let Some((_, eligible)) = self.parameter(node, cancel)? else {
                return Ok(false);
            };
            if !eligible {
                if !self.written_parameter(node, cancel)? {
                    return Ok(false);
                }
                candidate.syntax = "cpp.ordinary_parameter";
                return Ok(true);
            }
            let Some(owner) = self.owner(node, cancel)? else {
                return Ok(false);
            };
            if self.context(owner, cancel)?.is_none() {
                return Ok(false);
            }
            candidate.syntax = "cpp.const_reference_parameter";
            return Ok(true);
        }
        if role == StructuralRole::Definition
            && node.kind() == "identifier"
            && node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    "parameter_declaration" | "reference_declarator" | "pointer_declarator"
                )
            })
        {
            let parameter = node.parent().and_then(|parent| {
                if parent.kind() == "parameter_declaration" {
                    Some(parent)
                } else {
                    parent.parent()
                }
            });
            let Some(parameter) = parameter else {
                return Ok(false);
            };
            let Some((name, eligible)) = self.parameter(parameter, cancel)? else {
                return Ok(false);
            };
            if name != node {
                return Ok(false);
            }
            if !eligible {
                return self.written_parameter(parameter, cancel);
            }
            let Some(owner) = self.owner(node, cancel)? else {
                return Ok(false);
            };
            return Ok(self.context(owner, cancel)?.is_some());
        }
        let context = role == StructuralRole::Signature && node.kind() == "function_definition";
        if !(context
            || node.kind() == "identifier"
                && matches!(role, StructuralRole::Reference | StructuralRole::Signature))
        {
            return Ok(true);
        }
        let Some(owner) = self.owner(node, cancel)? else {
            return Ok(role != StructuralRole::Signature);
        };
        if !self.functions.contains_key(&owner.id()) {
            let proof = self.inspect(owner, cancel)?;
            self.functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.functions.insert(owner.id(), proof);
        }
        let proof = self.functions.get(&owner.id());
        if context {
            if proof.is_none_or(|proof| !proof.complete) {
                return Ok(false);
            }
            candidate.syntax = "cpp.const_formal_context";
            candidate.required = true;
            return Ok(true);
        } else if proof.is_some_and(|proof| {
            proof
                .receivers
                .contains(&(node.start_byte(), node.end_byte()))
        }) {
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "cpp.const_formal_receiver";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature {
            return Ok(false);
        }
        let word = self.word(node, cancel)?;
        if self
            .functions
            .get(&owner.id())
            .is_some_and(|proof| word.is_some_and(|word| proof.names.contains(word)))
        {
            candidate.syntax = "cpp.unproven_const_formal";
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn function<'tree>(tree: &'tree tree_sitter::Tree, start: usize, end: usize) -> Node<'tree> {
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(start, end)
            .unwrap();
        while node.kind() != "function_definition" {
            node = node.parent().unwrap();
        }
        node
    }

    #[test]
    fn original_const_reference_receiver_is_certified_despite_unrelated_recovery() {
        let source = include_bytes!("../../tests/fixtures/cpp_waforth_formal/main.cpp");
        assert_eq!(source.len(), 7028);
        let parsed = tree(source);
        assert!(parsed.root_node().has_error());
        let owner = function(&parsed, 814, 821);
        assert_eq!((owner.start_byte(), owner.end_byte()), (801, 995));
        assert!(!owner.has_error());
        let mut cache = CaptureCache::new(source, 16_384, true);
        let proof = cache.inspect(owner, &Cancellation::new()).unwrap();
        assert!(proof.complete);
        assert_eq!(proof.names.len(), 4);
        assert_eq!(proof.receivers, HashSet::from([(935, 941)]));
        let parameter = parsed
            .root_node()
            .descendant_for_byte_range(845, 872)
            .unwrap();
        let (name, eligible) = cache
            .parameter(parameter, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert!(eligible);
        assert_eq!((name.start_byte(), name.end_byte()), (866, 872));
        let leaf = parsed
            .root_node()
            .descendant_for_byte_range(935, 941)
            .unwrap();
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let mut candidate = QueryCandidate {
                start: leaf.start_byte(),
                end: leaf.end_byte(),
                role,
                syntax: "cpp.identifier",
                required: false,
                native_depth: 0,
            };
            assert!(
                cache
                    .refine(leaf, role, &mut candidate, &Cancellation::new())
                    .unwrap()
            );
            assert_eq!(candidate.role, StructuralRole::Reference);
            assert_eq!(candidate.syntax, "cpp.const_formal_receiver");
            assert!(candidate.required);
        }
    }

    #[test]
    fn unsupported_binders_mutations_and_directive_owners_withhold_receiver_certificates() {
        for source in [
            "void Owned(const Value &value) { Value value; External(value[0]); }",
            "void Owned(const Value &value) { { External(value[0]); } }",
            "void Owned(const Value &value) { auto f = [&] { return value[0]; }; External(value[0]); }",
            "template<class T> void Owned(const T &value) { External(value[0]); }",
            "void Owned(const Value &value, const Value &value) { External(value[0]); }",
            "void Owned(const Value &&value) { External(value[0]); }",
            "void Owned(Value &value) { External(value[0]); }",
            "void Owned(const Value &value) { value[0] = 1; External(value[0]); }",
            "#define value other\nvoid Owned(const Value &value) { External(value[0]); }",
            "#if ENABLED\nvoid Owned(const Value &value) { External(value[0]); }\n#endif",
        ] {
            let parsed = tree(source.as_bytes());
            assert!(!parsed.root_node().has_error(), "native premise: {source}");
            let at = source.find("Owned").unwrap();
            let owner = function(&parsed, at, at + 5);
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            assert!(
                !cache.inspect(owner, &Cancellation::new()).unwrap().complete,
                "{source}"
            );
        }
    }

    #[test]
    fn partial_work_exhaustion_and_cancellation_cannot_certify_const_receivers() {
        let source = b"void Owned(const Value &value) { External(value[0]); }";
        let parsed = tree(source);
        let owner = function(&parsed, 5, 10);
        let mut partial = CaptureCache::new(source, 16_384, false);
        assert!(
            !partial
                .inspect(owner, &Cancellation::new())
                .unwrap()
                .complete
        );
        let mut exhausted = CaptureCache::new(source, 16_384, true);
        exhausted.remaining = 3;
        assert!(
            !exhausted
                .inspect(owner, &Cancellation::new())
                .unwrap()
                .complete
        );
        assert!(exhausted.exhausted());
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cancelled = CaptureCache::new(source, usize::MAX, true);
        assert_eq!(
            cancelled.remaining,
            super::super::HARD_MAX_QUERY_CAPTURES * 64
        );
        assert!(matches!(
            cancelled.inspect(owner, &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}

#[cfg(test)]
mod written_parameter_tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn admitted(source: &[u8], position: usize, complete: bool) -> bool {
        let tree = parse(source);
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(position, position + 1)
            .unwrap();
        while node.kind() != "parameter_declaration" {
            node = node.parent().unwrap()
        }
        let mut cache = CaptureCache::new(source, 16_384, complete);
        cache.written_parameter(node, &Cancellation::new()).unwrap()
    }

    #[test]
    fn written_parameter_admission_accepts_ordinary_and_conditional_headers() {
        for source in [
            "void Owned(Value input) { External(input); }",
            "void Owned(Value *input) { External(input); }",
            "void Owned(Value &input) { External(input); }",
            "#if ENABLED\nvoid Owned(Value input) { External(input); }\n#endif\n",
        ] {
            assert!(admitted(
                source.as_bytes(),
                source.find("input").unwrap(),
                true
            ));
        }
    }

    #[test]
    fn prototypes_lambdas_duplicates_templates_and_partial_context_do_not_donate_formals() {
        for source in [
            "void Owned(Value input);",
            "auto lambda = [](Value input) { return input; };",
            "void Owned(Value input, Value input) {}",
            "template<class T> void Owned(T input) {}",
        ] {
            assert!(!admitted(
                source.as_bytes(),
                source.find("input").unwrap(),
                true
            ));
        }
        let source = "void Owned(Value input) {}";
        assert!(!admitted(
            source.as_bytes(),
            source.find("input").unwrap(),
            false
        ));
    }

    #[test]
    fn ordinary_parameters_do_not_gain_const_receiver_certificates_and_work_is_strict() {
        let source = b"void Owned(Value input) { External(input[0]); }";
        let tree = parse(source);
        let owner = tree.root_node().named_child(0).unwrap();
        let mut cache = CaptureCache::new(source, 16_384, true);
        assert!(!cache.inspect(owner, &Cancellation::new()).unwrap().complete);
        let mut node = tree.root_node().descendant_for_byte_range(17, 22).unwrap();
        while node.kind() != "parameter_declaration" {
            node = node.parent().unwrap()
        }
        cache.remaining = 0;
        assert!(!cache.written_parameter(node, &Cancellation::new()).unwrap());
        assert!(cache.exhausted());
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            CaptureCache::new(source, 16_384, true).written_parameter(node, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn full_original_ordinary_formal_keeps_clean_owner_despite_unrelated_recovery() {
        let source = include_bytes!("../../tests/fixtures/written_residuals/cpp/study.cpp");
        assert_eq!(source.len(), 9196);
        let parsed = parse(source);
        let root = parsed.root_node();
        assert_eq!(root.end_byte(), source.len());
        assert!(root.has_error());
        let name = root.descendant_for_byte_range(744, 746).unwrap();
        assert_eq!(name.kind(), "identifier");
        assert_eq!(name.byte_range(), 744..746);
        let parameter = name.parent().unwrap();
        assert_eq!(parameter.kind(), "parameter_declaration");
        assert_eq!(parameter.byte_range(), 724..746);
        let owner = parameter
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert_eq!(owner.kind(), "function_definition");
        assert_eq!(owner.byte_range(), 699..1505);
        assert!(!owner.has_error());
        assert_eq!(owner.parent().unwrap().kind(), "preproc_if");
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, 16_384, true);
        assert_eq!(
            cache.parameter(parameter, &cancel).unwrap(),
            Some((name, false))
        );
        for (node, role) in [
            (parameter, StructuralRole::Declaration),
            (name, StructuralRole::Definition),
        ] {
            let mut candidate = QueryCandidate {
                start: node.start_byte(),
                end: node.end_byte(),
                role,
                syntax: "cpp.parameter",
                required: true,
                native_depth: 0,
            };
            assert!(cache.refine(node, role, &mut candidate, &cancel).unwrap());
            if role == StructuralRole::Declaration {
                assert_eq!(candidate.syntax, "cpp.ordinary_parameter");
            }
            assert_eq!(candidate.role, role);
            assert_eq!(candidate.start..candidate.end, node.byte_range());
        }
        let mut candidate = QueryCandidate {
            start: 744,
            end: 746,
            role: StructuralRole::Reference,
            syntax: "cpp.identifier",
            required: false,
            native_depth: 0,
        };
        assert!(
            cache
                .refine(name, StructuralRole::Reference, &mut candidate, &cancel)
                .unwrap()
        );
        assert_eq!(candidate.syntax, "cpp.unproven_const_formal");
        assert!(!cache.exhausted());
    }
}
