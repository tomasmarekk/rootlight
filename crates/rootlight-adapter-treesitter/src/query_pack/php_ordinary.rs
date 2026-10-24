//! Certify plain physical-file PHP functions without importing an evaluated scope.
//! Every capture shares a finite census; incomplete or ambiguous files issue no proof.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

type Span = (usize, usize);

#[derive(Clone, Copy)]
struct Folded<'a>(&'a str);
impl PartialEq for Folded<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}
impl Eq for Folded<'_> {}
impl Hash for Folded<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_lowercase());
        }
        state.write_u8(0);
    }
}

struct Function<'source> {
    span: Span,
    formals: HashMap<&'source str, Span>,
    admitted: bool,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    admitted: bool,
    remaining: usize,
    exhausted: bool,
    functions: Vec<Function<'source>>,
    contexts: HashMap<Span, ()>,
    declarations: HashMap<Span, ()>,
    references: HashMap<Span, ()>,
    calls: HashMap<Span, ()>,
    full_calls: HashMap<Span, ()>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            admitted: false,
            remaining: maximum,
            exhausted: false,
            functions: Vec::new(),
            contexts: HashMap::new(),
            declarations: HashMap::new(),
            references: HashMap::new(),
            calls: HashMap::new(),
            full_calls: HashMap::new(),
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
        variable: bool,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.step(cancel)? || node.is_missing() || node.has_error() {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        let bare = if variable {
            bytes.strip_prefix(b"$")
        } else {
            Some(bytes)
        };
        let Some(bare) = bare else { return Ok(None) };
        let Some((&first, _)) = bare.split_first() else {
            return Ok(None);
        };
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return Ok(None);
        }
        for chunk in bare.chunks(64) {
            if !self.step(cancel)?
                || !chunk
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                return Ok(None);
            }
        }
        Ok(std::str::from_utf8(bytes).ok())
    }

    fn function(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Function<'source>>, AdapterError> {
        let Some(parameters) = node.child_by_field_name("parameters") else {
            return Ok(None);
        };
        let Some(body) = node.child_by_field_name("body") else {
            return Ok(None);
        };
        if parameters.kind() != "formal_parameters"
            || body.kind() != "compound_statement"
            || node.child_by_field_name("attributes").is_some()
            || node.child_by_field_name("return_type").is_some()
        {
            return Ok(None);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.step(cancel)?
                || !matches!(
                    child.kind(),
                    "name" | "formal_parameters" | "compound_statement"
                )
            {
                return Ok(None);
            }
        }
        let mut result = Function {
            span: (node.start_byte(), node.end_byte()),
            formals: HashMap::new(),
            admitted: true,
        };
        let mut cursor = parameters.walk();
        for formal in parameters.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if formal.kind() != "simple_parameter"
                || formal.child_by_field_name("reference_modifier").is_some()
                || formal.child_by_field_name("attributes").is_some()
                || formal.child_by_field_name("type").is_some()
                || formal
                    .child_by_field_name("default_value")
                    .is_some_and(|value| !matches!(value.kind(), "integer" | "float"))
            {
                return Ok(None);
            }
            let Some(variable) = formal
                .child_by_field_name("name")
                .filter(|n| n.kind() == "variable_name")
            else {
                return Ok(None);
            };
            let Some(name) = self.name(variable, true, cancel)? else {
                return Ok(None);
            };
            if name == "$this" || name == "$GLOBALS" || name.starts_with("$_") {
                return Ok(None);
            }
            result
                .formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if result
                .formals
                .insert(name, (variable.start_byte(), variable.end_byte()))
                .is_some()
            {
                return Ok(None);
            }
        }
        let mut cursor = body.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(None);
            }
            let current = cursor.node();
            match current.kind() {
                "anonymous_function"
                | "arrow_function"
                | "anonymous_class"
                | "function_definition"
                | "foreach_statement"
                | "catch_clause"
                | "global_declaration"
                | "function_static_declaration"
                | "unset_statement"
                | "reference_assignment_expression"
                | "dynamic_variable_name"
                | "reference_modifier"
                | "variadic_unpacking" => return Ok(None),
                "assignment_expression"
                | "augmented_assignment_expression"
                | "update_expression" => {
                    let field = if current.kind() == "update_expression" {
                        "argument"
                    } else {
                        "left"
                    };
                    let Some(left) = current.child_by_field_name(field) else {
                        return Ok(None);
                    };
                    if left.kind() != "variable_name" {
                        return Ok(None);
                    }
                    let Some(name) = self.name(left, true, cancel)? else {
                        return Ok(None);
                    };
                    if result.formals.contains_key(name) {
                        return Ok(None);
                    }
                }
                "function_call_expression" => {
                    let Some(callee) = current
                        .child_by_field_name("function")
                        .filter(|n| n.kind() == "name")
                    else {
                        return Ok(None);
                    };
                    let Some(name) = self.name(callee, false, cancel)? else {
                        return Ok(None);
                    };
                    if ["eval", "extract", "parse_str"]
                        .iter()
                        .any(|effect| name.eq_ignore_ascii_case(effect))
                    {
                        return Ok(None);
                    }
                }
                _ => {}
            }
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

    fn initialize(
        &mut self,
        mut root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.initialized = true;
        while let Some(parent) = root.parent() {
            if !self.step(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if !self.complete
            || root.kind() != "program"
            || root.has_error()
            || root.is_missing()
            || root.end_byte() != self.source.len()
            || !matches!(
                self.source.get(..root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(());
        }
        let mut names = HashMap::new();
        let mut nodes = Vec::new();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(());
            }
            match child.kind() {
                "comment" | "php_tag" => {}
                "text"
                    if self.source.get(child.byte_range()).is_some_and(|bytes| {
                        bytes
                            .strip_prefix(b"\xef\xbb\xbf")
                            .unwrap_or(bytes)
                            .iter()
                            .all(u8::is_ascii_whitespace)
                    }) => {}
                "function_definition" => {
                    let Some(name) = child.child_by_field_name("name") else {
                        return Ok(());
                    };
                    let Some(name) = self.name(name, false, cancel)? else {
                        return Ok(());
                    };
                    names
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    if names.insert(Folded(name), nodes.len()).is_some() {
                        return Ok(());
                    }
                    nodes
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    nodes.push(child);
                }
                _ => return Ok(()),
            }
        }
        if nodes.is_empty() {
            return Ok(());
        }
        let mut cursor = root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            if matches!(
                node.kind(),
                "include_expression"
                    | "include_once_expression"
                    | "require_expression"
                    | "require_once_expression"
                    | "eval_expression"
                    | "namespace_definition"
                    | "namespace_use_declaration"
            ) || (node.kind() == "function_definition" && node.parent() != Some(root))
            {
                return Ok(());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node() == root {
                break;
            }
        }
        for node in &nodes {
            let function = self.function(*node, cancel)?;
            self.functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.functions.push(function.unwrap_or(Function {
                span: (node.start_byte(), node.end_byte()),
                formals: HashMap::new(),
                admitted: false,
            }));
        }
        for (index, function_node) in nodes.iter().enumerate() {
            if !self.step(cancel)? {
                return Ok(());
            }
            if !self.functions[index].admitted {
                continue;
            }
            self.contexts
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.contexts.insert(self.functions[index].span, ());
            for span in self.functions[index].formals.values() {
                cancel.check()?;
                self.declarations
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.declarations.insert(*span, ());
            }
            let Some(body) = function_node.child_by_field_name("body") else {
                continue;
            };
            let mut cursor = body.walk();
            loop {
                if !self.step(cancel)? {
                    return Ok(());
                }
                let node = cursor.node();
                if node.kind() == "variable_name" {
                    let Some(name) = self.name(node, true, cancel)? else {
                        return Ok(());
                    };
                    if self.functions[index].formals.contains_key(name) {
                        self.references
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.references
                            .insert((node.start_byte(), node.end_byte()), ());
                    }
                }
                if node.kind() == "function_call_expression" {
                    let Some(callee) = node
                        .child_by_field_name("function")
                        .filter(|n| n.kind() == "name")
                    else {
                        continue;
                    };
                    let Some(name) = self.name(callee, false, cancel)? else {
                        return Ok(());
                    };
                    if let Some(target) = names
                        .get(&Folded(name))
                        .copied()
                        .filter(|target| self.functions[*target].admitted)
                    {
                        let Some(arguments) = node.child_by_field_name("arguments") else {
                            continue;
                        };
                        let mut arguments_cursor = arguments.walk();
                        let mut ordinary = true;
                        for argument in arguments.named_children(&mut arguments_cursor) {
                            if !self.step(cancel)? {
                                return Ok(());
                            }
                            ordinary &= argument.kind() == "comment"
                                || (argument.kind() == "argument"
                                    && argument.child_by_field_name("name").is_none()
                                    && argument
                                        .child_by_field_name("reference_modifier")
                                        .is_none());
                        }
                        if ordinary && target < self.functions.len() {
                            self.calls
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.calls
                                .insert((callee.start_byte(), callee.end_byte()), ());
                            self.full_calls
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.full_calls
                                .insert((node.start_byte(), node.end_byte()), ());
                        }
                    }
                }
                if cursor.goto_first_child() {
                    continue;
                }
                loop {
                    if cursor.goto_next_sibling() {
                        break;
                    }
                    if !cursor.goto_parent() {
                        break;
                    }
                }
                if cursor.node() == body {
                    break;
                }
            }
        }
        self.admitted = !self.exhausted;
        Ok(())
    }

    /// Refine only the new ordinary captures; legacy full calls remain untouched.
    /// Exhaustion is a transaction refusal, so no partial certificate may escape.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        cancel.check()?;
        if !self.initialized {
            self.initialize(node, cancel)?;
        }
        let span = (node.start_byte(), node.end_byte());
        if matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        ) && node.kind() == "variable_name"
        {
            if !self.admitted || !self.declarations.contains_key(&span) {
                return Ok(Some(false));
            }
            candidate.syntax = if role == StructuralRole::Declaration {
                "php.ordinary_parameter"
            } else {
                "php.variable_name"
            };
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Signature {
            let syntax = match node.kind() {
                "program" if self.admitted => Some("php.ordinary_clean_file"),
                "function_definition" if self.admitted && self.contexts.contains_key(&span) => {
                    Some("php.ordinary_function_context")
                }
                "variable_name" if self.admitted && self.references.contains_key(&span) => {
                    Some("php.ordinary_formal_target")
                }
                "name" if self.admitted && self.calls.contains_key(&span) => {
                    Some("php.ordinary_call_target")
                }
                "function_call_expression"
                    if self.admitted && self.full_calls.contains_key(&span) =>
                {
                    Some("php.ordinary_call_context")
                }
                _ => None,
            };
            if let Some(syntax) = syntax {
                candidate.syntax = syntax;
                candidate.required = true;
                return Ok(Some(true));
            }
            if matches!(
                node.kind(),
                "program" | "function_definition" | "variable_name" | "function_call_expression"
            ) || (node.kind() == "name"
                && node
                    .parent()
                    .is_some_and(|p| p.kind() == "function_call_expression"))
            {
                return Ok(Some(false));
            }
        }
        if self.admitted && role == StructuralRole::Reference && node.kind() == "variable_name" {
            candidate.syntax = if self.references.contains_key(&span) {
                "php.ordinary_formal"
            } else {
                "php.ordinary_unproved_variable"
            };
            candidate.required = self.references.contains_key(&span);
            return Ok(Some(true));
        }
        if self.admitted
            && role == StructuralRole::Reference
            && self.calls.contains_key(&span)
            && node.kind() == "name"
        {
            candidate.required = true;
        }
        if self.admitted && role == StructuralRole::Scope {
            candidate.required = true;
        }
        if self.admitted && role == StructuralRole::Call && self.full_calls.contains_key(&span) {
            candidate.required = true;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &[u8] =
        include_bytes!("../../tests/fixtures/php_ordinary_functions/geo_helpers.php");

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn initialized(source: &[u8], maximum: usize, complete: bool) -> CaptureCache<'_> {
        let parsed = tree(source);
        let mut cache = CaptureCache::new(source, maximum, complete);
        cache
            .initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        cache
    }

    #[test]
    fn complete_original_census_keeps_distinct_owner_formals_and_one_written_call() {
        let cache = initialized(ORIGINAL, 16_384, true);
        assert!(cache.admitted && !cache.exhausted);
        assert_eq!(cache.contexts.len(), 2);
        assert_eq!(cache.declarations.len(), 6);
        assert!(cache.declarations.contains_key(&(358, 362)));
        assert!(cache.references.contains_key(&(440, 444)));
        assert_eq!(
            cache.calls.keys().copied().collect::<Vec<_>>(),
            [(838, 849)]
        );
        assert_eq!(
            cache.full_calls.keys().copied().collect::<Vec<_>>(),
            [(838, 869)]
        );
    }

    #[test]
    fn same_spelling_in_another_function_never_donates_a_formal() {
        let source =
            b"<?php function owner($lat) { return $lat; } function other($lng) { return $lat; }";
        let cache = initialized(source, 16_384, true);
        assert!(cache.admitted);
        let last = source
            .windows(4)
            .rposition(|bytes| bytes == b"$lat")
            .unwrap();
        assert!(!cache.references.contains_key(&(last, last + 4)));
        assert_eq!(cache.references.len(), 1);
    }

    #[test]
    fn collisions_and_nonordinary_function_forms_cannot_emit_parameter_captures() {
        for source in [
            "<?php function owned($lat) {} function OWNED($lng) {}",
            "<?php namespace Place; function owned($lat) { return $lat; }",
            "<?php if (true) { function owned($lat) { return $lat; } }",
            "<?php class Place { function owned($lat) { return $lat; } }",
            "<?php function owned(&$lat) { return $lat; }",
            "<?php function owned(...$lat) { return $lat; }",
            "<?php function owned($lat) { $lat = 0; return $lat; }",
            "<?php function owned($lat) { global $lat; return $lat; }",
            "<?php function owned($lat) { static $value; return $lat; }",
            "<?php function owned($lat) { unset($lat); return $lat; }",
            "<?php function owned($lat) { return fn() => $lat; }",
            "<?php function owned($lat) { return function () use ($lat) { return $lat; }; }",
            "<?php function owned($lat) { include 'other.php'; return $lat; }",
            "<?php function owned($lat) { eval('changed'); return $lat; }",
            "<?php function owned($lat, $call) { return $call($lat); }",
        ] {
            assert!(
                !tree(source.as_bytes()).root_node().has_error(),
                "native premise: {source}"
            );
            let cache = initialized(source.as_bytes(), 16_384, true);
            assert!(
                cache.declarations.is_empty(),
                "no ordinary formal: {source}"
            );
            assert!(cache.references.is_empty(), "no ordinary binding: {source}");
            assert!(cache.calls.is_empty(), "no written Calls facet: {source}");
        }
    }

    #[test]
    fn exact_census_work_one_less_partial_recovery_and_cancellation_refuse_atomically() {
        let maximum = 16_384;
        let observed = initialized(ORIGINAL, maximum, true);
        assert!(observed.admitted);
        let exact = maximum - observed.remaining;
        assert!(initialized(ORIGINAL, exact, true).admitted);
        let less = initialized(ORIGINAL, exact - 1, true);
        assert!(less.exhausted && !less.admitted);
        assert!(!initialized(ORIGINAL, maximum, false).admitted);
        assert!(!initialized(b"<?php function broken($lat) { return ;", maximum, true).admitted);
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let parsed = tree(ORIGINAL);
        assert!(
            CaptureCache::new(ORIGINAL, maximum, true)
                .initialize(parsed.root_node(), &cancel)
                .is_err()
        );
    }

    #[test]
    fn full_call_and_name_reference_keep_legacy_syntax_beside_the_certificate() {
        let parsed = tree(ORIGINAL);
        let mut cache = initialized(ORIGINAL, 16_384, true);
        for (range, role, syntax) in [
            ((838, 869), StructuralRole::Call, "php.call"),
            ((838, 849), StructuralRole::Reference, "php.name"),
        ] {
            let node = parsed
                .root_node()
                .descendant_for_byte_range(range.0, range.1)
                .unwrap();
            let mut candidate = QueryCandidate {
                start: range.0,
                end: range.1,
                role,
                syntax,
                required: false,
                native_depth: 0,
            };
            cache
                .refine(node, role, &mut candidate, &Cancellation::new())
                .unwrap();
            assert!(candidate.required);
            assert_eq!(candidate.syntax, syntax);
            assert_eq!(candidate.role, role);
        }
        assert!(!cache.calls.contains_key(&(738, 744)));
    }

    #[test]
    fn many_ordinary_functions_exhaust_one_shared_census_without_partial_admission() {
        let mut source = String::from("<?php ");
        for index in 0..128 {
            source.push_str(&format!("function owned_{index}($lat) {{ return $lat; }} "));
        }
        let cache = initialized(source.as_bytes(), 64, true);
        assert!(cache.exhausted && !cache.admitted);
        assert!(cache.contexts.is_empty());
    }
}
