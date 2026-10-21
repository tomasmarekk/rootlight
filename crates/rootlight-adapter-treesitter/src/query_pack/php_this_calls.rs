//! Certify a literal instance call's written method declaration in its own class.
//! Runtime overrides, inherited lookup and trait composition remain separate gaps.

use super::{QueryCandidate, StructuralRole};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    clean_root: bool,
    remaining: usize,
    exhausted: bool,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            clean_root: false,
            remaining: maximum,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn initialize(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.initialized = true;
        while let Some(parent) = node.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            node = parent;
        }
        self.clean_root = self.complete
            && node.kind() == "program"
            && !node.is_error()
            && !node.is_missing()
            && !node.has_error()
            && node.end_byte() == self.source.len()
            && matches!(
                self.source.get(..node.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            );
        Ok(())
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.visit(cancel)? || node.kind() != "name" || node.has_error() || node.is_missing() {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        let Some((&first, _)) = raw.split_first() else {
            return Ok(None);
        };
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return Ok(None);
        }
        for chunk in raw.chunks(64) {
            if !self.visit(cancel)?
                || !chunk
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                return Ok(None);
            }
        }
        Ok(std::str::from_utf8(raw).ok())
    }

    fn static_method(
        &mut self,
        method: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut cursor = method.walk();
        for child in method.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(true);
            }
            if matches!(child.kind(), "static_modifier" | "abstract_modifier") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn class_for<'tree>(
        &mut self,
        method: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.visit(cancel)? || method.kind() != "method_declaration" {
            return Ok(None);
        }
        let Some(body) = method
            .parent()
            .filter(|node| node.kind() == "declaration_list")
        else {
            return Ok(None);
        };
        Ok(body.parent().filter(|owner| {
            owner.kind() == "class_declaration" && owner.child_by_field_name("body") == Some(body)
        }))
    }

    fn class_admitted(
        &mut self,
        class: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.visit(cancel)? || class.kind() != "class_declaration" || class.has_error() {
            return Ok(false);
        }
        let Some(class_name) = class.child_by_field_name("name") else {
            return Ok(false);
        };
        if self.name(class_name, cancel)?.is_none() {
            return Ok(false);
        }
        let mut cursor = class.walk();
        for child in class.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if matches!(child.kind(), "base_clause" | "abstract_modifier") {
                return Ok(false);
            }
        }
        let Some(body) = class
            .child_by_field_name("body")
            .filter(|body| body.kind() == "declaration_list")
        else {
            return Ok(false);
        };
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            match member.kind() {
                "comment" | "property_declaration" | "const_declaration" => {}
                "method_declaration" => {
                    let Some(name_node) = member.child_by_field_name("name") else {
                        return Ok(false);
                    };
                    let Some(name) = self.name(name_node, cancel)? else {
                        return Ok(false);
                    };
                    let mut peers = body.walk();
                    for other in body.named_children(&mut peers) {
                        if !self.visit(cancel)? {
                            return Ok(false);
                        }
                        if other.kind() != "method_declaration" || other.id() == member.id() {
                            continue;
                        }
                        let Some(other_name) = other.child_by_field_name("name") else {
                            return Ok(false);
                        };
                        let Some(other_name) = self.name(other_name, cancel)? else {
                            return Ok(false);
                        };
                        if name.eq_ignore_ascii_case(other_name) {
                            return Ok(false);
                        }
                    }
                }
                // Trait use and future class-member forms need their own lookup proof.
                _ => return Ok(false),
            }
        }
        Ok(!self.exhausted)
    }

    fn owner<'tree>(
        &mut self,
        mut node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        while let Some(parent) = node.parent() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            match parent.kind() {
                "method_declaration" => return Ok(Some(parent)),
                "anonymous_function"
                | "arrow_function"
                | "function_definition"
                | "anonymous_class" => return Ok(None),
                _ => node = parent,
            }
        }
        Ok(None)
    }

    fn parameters_stable(
        &mut self,
        method: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(parameters) = method.child_by_field_name("parameters") else {
            return Ok(false);
        };
        let mut cursor = parameters.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            let current = cursor.node();
            if current.kind() == "variable_name"
                && self.source.get(current.byte_range()) == Some(b"$this")
            {
                return Ok(false);
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
            if cursor.node() == parameters {
                break;
            }
        }
        Ok(!self.exhausted)
    }

    fn receiver_stable(
        &mut self,
        method: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.parameters_stable(method, cancel)? {
            return Ok(false);
        }
        let Some(body) = method.child_by_field_name("body") else {
            return Ok(false);
        };
        let mut cursor = body.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            let current = cursor.node();
            match current.kind() {
                "anonymous_function"
                | "arrow_function"
                | "function_definition"
                | "anonymous_class"
                | "foreach_statement"
                | "catch_clause"
                | "global_declaration"
                | "function_static_declaration"
                | "unset_statement"
                | "reference_assignment_expression" => return Ok(false),
                "assignment_expression"
                | "augmented_assignment_expression"
                | "update_expression" => {
                    let field = if current.kind() == "update_expression" {
                        "argument"
                    } else {
                        "left"
                    };
                    let Some(left) = current.child_by_field_name(field) else {
                        return Ok(false);
                    };
                    match left.kind() {
                        "variable_name" if self.source.get(left.byte_range()) != Some(b"$this") => {
                        }
                        "member_access_expression"
                        | "subscript_expression"
                        | "scoped_property_access_expression" => {}
                        _ => return Ok(false),
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
                    return Ok(!self.exhausted);
                }
            }
        }
    }

    fn certified(&mut self, name: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(call) = name.parent().filter(|call| {
            call.kind() == "member_call_expression"
                && call.child_by_field_name("name") == Some(name)
        }) else {
            return Ok(false);
        };
        let Some(arguments) = call
            .child_by_field_name("arguments")
            .filter(|node| node.kind() == "arguments" && !node.has_error() && !node.is_missing())
        else {
            return Ok(false);
        };
        if !self.visit(cancel)? {
            return Ok(false);
        }
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            if !self.visit(cancel)? || !matches!(argument.kind(), "argument" | "comment") {
                // A variadic placeholder acquires a Closure instead of calling.
                // Unpacking is an argument child and retains ordinary invocation.
                return Ok(false);
            }
        }
        let Some(object) = call.child_by_field_name("object") else {
            return Ok(false);
        };
        if object.kind() != "variable_name"
            || self.source.get(object.byte_range()) != Some(b"$this")
        {
            return Ok(false);
        }
        let Some(callee) = self.name(name, cancel)? else {
            return Ok(false);
        };
        let Some(owner) = self.owner(name, cancel)? else {
            return Ok(false);
        };
        if self.static_method(owner, cancel)? {
            return Ok(false);
        }
        if !self.receiver_stable(owner, cancel)? {
            return Ok(false);
        }
        let Some(owner_body) = owner.child_by_field_name("body") else {
            return Ok(false);
        };
        if owner_body.kind() != "compound_statement"
            || call.start_byte() < owner_body.start_byte()
            || call.end_byte() > owner_body.end_byte()
        {
            return Ok(false);
        }
        let Some(class) = self.class_for(owner, cancel)? else {
            return Ok(false);
        };
        if !self.class_admitted(class, cancel)? {
            return Ok(false);
        }
        let Some(body) = class.child_by_field_name("body") else {
            return Ok(false);
        };
        let mut cursor = body.walk();
        for method in body.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if method.kind() != "method_declaration" {
                continue;
            }
            let Some(node) = method.child_by_field_name("name") else {
                return Ok(false);
            };
            let Some(target) = self.name(node, cancel)? else {
                return Ok(false);
            };
            if !target.eq_ignore_ascii_case(callee) {
                continue;
            }
            return Ok(!self.static_method(method, cancel)?
                && self.parameters_stable(method, cancel)?
                && method.child_by_field_name("body").is_some()
                && !target.eq_ignore_ascii_case("__construct")
                && !self.exhausted);
        }
        Ok(false)
    }

    /// Refine only native instance-call carriers and full owner metadata.
    /// The caller propagates typed errors and turns exhaustion into a helper refusal.
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
        if role == StructuralRole::Signature
            && matches!(node.kind(), "class_declaration" | "method_declaration")
        {
            let class = if node.kind() == "class_declaration" {
                Some(node)
            } else {
                self.class_for(node, cancel)?
            };
            if !self.clean_root || self.exhausted {
                return Ok(Some(false));
            }
            let Some(class) = class else {
                return Ok(Some(false));
            };
            if !self.class_admitted(class, cancel)? {
                return Ok(Some(false));
            }
            if node.kind() == "method_declaration"
                && (self.static_method(node, cancel)?
                    || node.child_by_field_name("body").is_none()
                    || !self.parameters_stable(node, cancel)?)
            {
                return Ok(Some(false));
            }
            candidate.syntax = if node.kind() == "class_declaration" {
                "php.this_class_context"
            } else {
                "php.this_method_context"
            };
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Call
            && matches!(
                node.kind(),
                "member_call_expression" | "nullsafe_member_call_expression"
            )
        {
            candidate.syntax = "php.unproven_this_method";
            return Ok(Some(true));
        }
        if node.kind() != "name"
            || !node.parent().is_some_and(|call| {
                matches!(
                    call.kind(),
                    "member_call_expression" | "nullsafe_member_call_expression"
                ) && call.child_by_field_name("name") == Some(node)
            })
        {
            return Ok(None);
        }
        if !matches!(
            role,
            StructuralRole::CallName | StructuralRole::Reference | StructuralRole::Signature
        ) {
            return Ok(None);
        }
        if self.clean_root && !self.exhausted && self.certified(node, cancel)? {
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "php.this_method";
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Signature {
            return Ok(Some(false));
        }
        candidate.syntax = "php.unproven_this_method";
        Ok(Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn proof(
        source: &[u8],
        maximum: usize,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<(Vec<(usize, usize)>, bool), AdapterError> {
        let tree = parse(source);
        let mut cache = CaptureCache::new(source, maximum, complete);
        let mut found = Vec::new();
        let mut cursor = tree.root_node().walk();
        loop {
            let node = cursor.node();
            if node.kind() == "name" {
                for role in [
                    StructuralRole::Reference,
                    StructuralRole::Signature,
                    StructuralRole::CallName,
                ] {
                    let mut candidate = QueryCandidate {
                        start: node.start_byte(),
                        end: node.end_byte(),
                        role,
                        syntax: "php.name",
                        required: false,
                        native_depth: 0,
                    };
                    cache.refine(node, role, &mut candidate, cancel)?;
                    if candidate.syntax == "php.this_method" {
                        assert!(candidate.required);
                        assert_eq!(candidate.role, StructuralRole::Reference);
                        found.push((candidate.start, candidate.end));
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
                    found.sort_unstable();
                    found.dedup();
                    if cache.exhausted() {
                        found.clear();
                    }
                    return Ok((found, cache.exhausted()));
                }
            }
        }
    }

    #[test]
    fn full_original_native_owner_proof_keeps_both_written_instance_calls() {
        let source = include_bytes!("../../tests/fixtures/php_constructors/HtmlString.php");
        assert_eq!(source.len(), 1145);
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let (sites, exhausted) = proof(source, 16_384, true, &Cancellation::new()).unwrap();
        assert!(!exhausted);
        assert_eq!(sites, vec![(956, 963), (1118, 1124)]);
        let name = tree
            .root_node()
            .descendant_for_byte_range(1118, 1124)
            .unwrap();
        let mut cache = CaptureCache::new(source, 16_384, true);
        cache.initialize(name, &Cancellation::new()).unwrap();
        let method = cache.owner(name, &Cancellation::new()).unwrap().unwrap();
        let class = cache
            .class_for(method, &Cancellation::new())
            .unwrap()
            .unwrap();
        for (node, expected, span) in [
            (method, "php.this_method_context", 1059..1140),
            (class, "php.this_class_context", 105..1143),
        ] {
            let mut candidate = QueryCandidate {
                start: node.start_byte(),
                end: node.end_byte(),
                role: StructuralRole::Signature,
                syntax: "php.name",
                required: true,
                native_depth: 0,
            };
            assert_eq!(
                cache
                    .refine(
                        node,
                        StructuralRole::Signature,
                        &mut candidate,
                        &Cancellation::new()
                    )
                    .unwrap(),
                Some(true)
            );
            assert_eq!(candidate.syntax, expected);
            assert_eq!(candidate.start..candidate.end, span);
        }
    }

    #[test]
    fn ordinary_casefolded_methods_and_child_overrides_preserve_the_lexical_target() {
        for source in [
            "<?php class Base { function target() {} function caller() { return $this->TARGET(); } } class Child extends Base { function target() {} }",
            "<?php class Owner { function again() { return $this->again(); } }",
            "<?php class Owner { function target() {} function __construct() { $this->field = 'x'; $this->target(); } }",
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "native premise: {source}");
            let (sites, exhausted) =
                proof(source.as_bytes(), 16_384, true, &Cancellation::new()).unwrap();
            assert!(!exhausted);
            assert_eq!(sites.len(), 1, "one written source dependency: {source}");
        }
    }

    #[test]
    fn receiver_type_scope_and_duplicate_boundaries_never_certify_a_spelling() {
        for source in [
            "<?php class Owner { function target() {} function caller($x) { return $x->target(); } }",
            "<?php class Owner { function target() {} function caller($name) { return $this->$name(); } }",
            "<?php class Owner { function target() {} function caller() { return $this?->target(); } }",
            "<?php class Owner { function target() {} static function caller() { return $this->target(); } }",
            "<?php class Owner { static function target() {} function caller() { return $this->target(); } }",
            "<?php class Owner { function target() {} function TARGET() {} function caller() { return $this->target(); } }",
            "<?php trait Extension { function target() {} } class Owner { use Extension; function caller() { return $this->target(); } }",
            "<?php class Base { function target() {} } class Child extends Base { function caller() { return $this->target(); } }",
            "<?php class Owner { function target() {} function caller() { return fn() => $this->target(); } }",
            "<?php class Owner { function target() {} function caller() { return function () { return $this->target(); }; } }",
            "<?php class Owner { function target() {} function caller($other) { $this = $other; return $this->target(); } }",
            "<?php class Owner { function target() {} function caller() { unset($this); return $this->target(); } }",
        ] {
            assert!(
                !parse(source.as_bytes()).root_node().has_error(),
                "native premise only, no PHP semantic-validity claim: {source}"
            );
            let (sites, exhausted) =
                proof(source.as_bytes(), 16_384, true, &Cancellation::new()).unwrap();
            assert!(!exhausted);
            assert!(sites.is_empty(), "unproved receiver/member: {source}");
        }
    }

    #[test]
    fn partial_recovered_work_exhaustion_and_cancellation_cannot_issue_a_certificate() {
        let source = b"<?php class Owner { function target() {} function caller() { return $this->target(); } }";
        assert_eq!(
            proof(source, 16_384, true, &Cancellation::new())
                .unwrap()
                .0
                .len(),
            1
        );
        assert!(
            proof(source, 16_384, false, &Cancellation::new())
                .unwrap()
                .0
                .is_empty()
        );
        assert!(proof(source, 0, true, &Cancellation::new()).unwrap().1);
        let broken = b"<?php class Owner { function target() {} function caller() { return $this->target( ; } }";
        assert!(parse(broken).root_node().has_error());
        assert!(
            proof(broken, 16_384, true, &Cancellation::new())
                .unwrap()
                .0
                .is_empty()
        );
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            proof(source, 16_384, true, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }
    #[test]
    fn first_class_callable_placeholder_never_becomes_an_invocation_certificate() {
        for arguments in ["...", " /* acquisition */ ... /* closure */ "] {
            let source = format!(
                "<?php class Owner {{ function target() {{}} function caller() {{ return $this->target({arguments}); }} }}"
            );
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error());
            let site = source.find("$this->target").unwrap() + "$this->".len();
            let name = tree
                .root_node()
                .descendant_for_byte_range(site, site + 6)
                .unwrap();
            let args = name
                .parent()
                .unwrap()
                .child_by_field_name("arguments")
                .unwrap();
            let mut cursor = args.walk();
            assert!(
                args.named_children(&mut cursor)
                    .any(|node| node.kind() == "variadic_placeholder")
            );
            let (sites, exhausted) =
                proof(source.as_bytes(), 16_384, true, &Cancellation::new()).unwrap();
            assert!(!exhausted);
            assert!(
                sites.is_empty(),
                "Closure acquisition has no invocation certificate"
            );
        }
        let source = b"<?php class Owner { function target(...$values) {} function caller($args) { return $this->target(...$args); } }";
        let (sites, exhausted) = proof(source, 16_384, true, &Cancellation::new()).unwrap();
        assert!(!exhausted);
        assert_eq!(
            sites.len(),
            1,
            "an unpacked argument is still an invocation"
        );
    }
}
