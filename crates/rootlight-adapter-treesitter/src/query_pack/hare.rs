//! Certify written Hare declarations and bare calls from a complete native file.
//! A file certificate does not establish a complete multi-file translation unit.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};

struct Declaration {
    extent: Range<usize>,
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    initialized: bool,
    root_id: Option<usize>,
    remaining: usize,
    exhausted: bool,
    declarations: HashMap<usize, Declaration>,
    calls: HashMap<usize, usize>,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && source.get(node.byte_range()).is_some()
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            root_id: None,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
            declarations: HashMap::new(),
            calls: HashMap::new(),
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

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if node.kind() != "identifier" || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(None);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language("hare", text, text.len())
                .filter(|name| name.as_ref() == text)
                .map(|_| text),
        )
    }

    fn equal(
        &mut self,
        left: &str,
        right: &str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for _ in left.as_bytes().chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
        }
        Ok(left == right)
    }

    fn children<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        let mut result = Vec::new();
        if node.child_count() > self.remaining {
            self.exhausted = true;
            return Ok(result);
        }
        result
            .try_reserve_exact(node.child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(Vec::new());
            }
            result.push(child);
        }
        Ok(result)
    }

    fn descendants<'tree>(
        &mut self,
        root: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        let mut result = Vec::new();
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(root);
        while let Some(node) = pending.pop() {
            if !self.visit(cancel)? {
                return Ok(Vec::new());
            }
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.push(node);
            let children = self.children(node, cancel)?;
            pending
                .try_reserve(children.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.extend(children.into_iter().rev());
        }
        Ok(result)
    }

    fn insert(
        &mut self,
        key: Node<'_>,
        extent: Range<usize>,
        name: Node<'_>,
        header: Range<usize>,
        syntax: &'static str,
    ) -> Result<(), AdapterError> {
        self.declarations
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.declarations.insert(
            key.id(),
            Declaration {
                extent,
                name: name.byte_range(),
                header,
                syntax,
            },
        );
        Ok(())
    }

    fn function(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !clean(node, self.source)
            || node
                .parent()
                .is_none_or(|parent| parent.kind() != "declarations")
        {
            return Ok(false);
        }
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(false);
        };
        if self.name(name, cancel)?.is_none() {
            return Ok(false);
        }
        let children = self.children(node, cancel)?;
        if children
            .iter()
            .any(|child| child.kind() == "function_attribute")
        {
            return Ok(false);
        }
        let end = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |body| body.start_byte());
        self.insert(
            node,
            node.byte_range(),
            name,
            node.start_byte()..end,
            "hare.function",
        )?;
        for child in children
            .into_iter()
            .filter(|child| child.kind() == "parameter")
        {
            let Some(name) = child.named_child(0) else {
                continue;
            };
            if self.name(name, cancel)?.is_some()
                && child.child(1).is_some_and(|colon| colon.kind() == ":")
            {
                self.insert(
                    child,
                    child.byte_range(),
                    name,
                    child.byte_range(),
                    "hare.parameter",
                )?;
            }
        }
        Ok(true)
    }

    fn types(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        if !clean(node, self.source)
            || node
                .parent()
                .is_none_or(|parent| parent.kind() != "declarations")
        {
            return Ok(());
        }
        let children = self.children(node, cancel)?;
        for group in children.windows(3) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let [name, equals, value] = group else {
                continue;
            };
            if equals.kind() != "="
                || self.name(*name, cancel)?.is_none()
                || value.kind() != "struct_type"
            {
                continue;
            }
            self.insert(
                *name,
                name.start_byte()..value.end_byte(),
                *name,
                name.start_byte()..value.start_byte(),
                "hare.struct",
            )?;
            for field in self
                .children(*value, cancel)?
                .into_iter()
                .filter(|child| child.kind() == "field")
            {
                let members = self.children(field, cancel)?;
                if let [written, colon, _] = members.as_slice()
                    && colon.kind() == ":"
                    && self.name(*written, cancel)?.is_some()
                {
                    self.insert(
                        field,
                        field.byte_range(),
                        *written,
                        field.byte_range(),
                        "hare.field",
                    )?;
                }
            }
        }
        Ok(())
    }

    fn shadowed(
        &mut self,
        function: Node<'_>,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for node in self.descendants(function, cancel)? {
            if !self.visit(cancel)? {
                return Ok(true);
            }
            if matches!(node.kind(), "tuple_literal" | "function_attribute") {
                return Ok(true);
            }
            let relevant = matches!(
                node.kind(),
                "parameter" | "let_declaration" | "const_declaration" | "let_expression"
            );
            if relevant {
                // Every direct identifier is a conservative competitor, including
                // type names. Nested call operands are not declaration binders.
                for child in self.children(node, cancel)? {
                    if let Some(written) = self.name(child, cancel)?
                        && self.equal(written, name, cancel)?
                    {
                        return Ok(true);
                    }
                }
            }
            if node.kind() == "iterable_binding" {
                let Some(binding) = node.child_by_field_name("binding") else {
                    return Ok(true);
                };
                let Some(written) = self.name(binding, cancel)? else {
                    return Ok(true);
                };
                if self.equal(written, name, cancel)? {
                    return Ok(true);
                }
            }
        }
        Ok(self.exhausted)
    }

    fn initialize(
        &mut self,
        mut root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if self.initialized {
            if self.root_id != Some(root.id()) {
                return Err(query_failure("hare-cache-tree-mismatch"));
            }
            return Ok(());
        }
        self.initialized = true;
        self.root_id = Some(root.id());
        if !self.complete || root.kind() != "module" || !clean(root, self.source) {
            return Ok(());
        }
        let nodes = self.descendants(root, cancel)?;
        for node in &nodes {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if node.kind() == "function_declaration" {
                self.function(*node, cancel)?;
            }
            if node.kind() == "type_declaration" {
                self.types(*node, cancel)?;
            }
        }
        for call in nodes.iter().filter(|node| node.kind() == "call_expression") {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let Some(callee) = call.child_by_field_name("callee") else {
                continue;
            };
            let Some(name) = self.name(callee, cancel)? else {
                continue;
            };
            let mut parent = call.parent();
            let mut owner = None;
            while let Some(node) = parent {
                if !self.visit(cancel)? {
                    return Ok(());
                }
                if node.kind() == "function_declaration" {
                    owner = Some(node);
                    break;
                }
                parent = node.parent();
            }
            let Some(owner) = owner.filter(|owner| {
                self.declarations
                    .get(&owner.id())
                    .is_some_and(|declaration| declaration.syntax == "hare.function")
            }) else {
                continue;
            };
            if self.shadowed(owner, name, cancel)? {
                continue;
            }
            let mut target = None;
            let mut count = 0_usize;
            let mut barrier = false;
            for node in &nodes {
                if !self.visit(cancel)? {
                    return Ok(());
                }
                if node.kind() == "function_declaration"
                    && let Some(written) = node.child_by_field_name("name")
                    && let Some(candidate) = self.name(written, cancel)?
                    && self.equal(candidate, name, cancel)?
                {
                    count = count.saturating_add(1);
                    target = Some(*node);
                }
                if node.kind() == "use_statement" {
                    for part in self.descendants(*node, cancel)? {
                        if part.kind() == "*" {
                            barrier = true;
                        }
                        if let Some(written) = self.name(part, cancel)?
                            && self.equal(written, name, cancel)?
                        {
                            barrier = true;
                        }
                    }
                }
                if matches!(
                    node.kind(),
                    "type_declaration" | "global_binding" | "constant_declaration"
                ) {
                    for child in self.children(*node, cancel)? {
                        if let Some(written) = self.name(child, cancel)?
                            && self.equal(written, name, cancel)?
                        {
                            barrier = true;
                        }
                    }
                }
            }
            let Some(target) = target.filter(|target| {
                count == 1
                    && !barrier
                    && self.declarations.contains_key(&target.id())
                    && target.child_by_field_name("body").is_some()
                    && target.start_byte() < callee.end_byte()
            }) else {
                continue;
            };
            if self.exhausted {
                return Ok(());
            }
            self.calls
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.calls.insert(callee.id(), target.start_byte());
        }
        if self.exhausted {
            self.calls.clear();
        }
        Ok(())
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if !std::ptr::eq(source, cache.source) {
        return Err(query_failure("hare-cache-source-mismatch"));
    }
    cache.initialize(node, cancel)?;
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let syntax = if node.parent().is_none()
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        range = 0..source.len();
        if role == StructuralRole::Root {
            "hare.source_file"
        } else {
            "hare.file"
        }
    } else if let Some(declaration) = cache.declarations.get(&node.id()) {
        match role {
            StructuralRole::Scope | StructuralRole::Declaration => {
                range = declaration.extent.clone();
                declaration.syntax
            }
            StructuralRole::Definition => {
                range = declaration.name.clone();
                "hare.name"
            }
            StructuralRole::Signature => {
                range = declaration.header.clone();
                "hare.header"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Declaration {
        "hare.unsupported"
    } else if matches!(role, StructuralRole::Reference | StructuralRole::Signature)
        && node.parent().is_some_and(|parent| {
            parent.kind() == "call_expression"
                && parent
                    .child_by_field_name("callee")
                    .is_some_and(|callee| callee.id() == node.id())
        })
        && cache.name(node, cancel)?.is_some()
    {
        if let Some(start) = cache.calls.get(&node.id()) {
            if role == StructuralRole::Signature {
                range = *start..node.end_byte();
                "hare.call_target"
            } else {
                "hare.direct_file_function"
            }
        } else if role == StructuralRole::Reference {
            "hare.unresolved_call"
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Expression {
        "hare.construct_unavailable"
    } else if role == StructuralRole::Comment {
        "hare.comment"
    } else if role == StructuralRole::StringLiteral {
        "hare.string"
    } else {
        return Ok(None);
    };
    let mut depth = 0_usize;
    let mut parent = node.parent();
    while let Some(current) = parent {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("hare-native-depth"))?;
        parent = current.parent();
    }
    if syntax == "hare.call_target" {
        depth = 0;
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure()
            || matches!(syntax, "hare.direct_file_function" | "hare.unresolved_call"),
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn census(
        source: &[u8],
        maximum: usize,
        complete: bool,
        check: impl FnOnce(&CaptureCache<'_>),
    ) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hare::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(source, maximum, complete);
        cache
            .initialize(tree.root_node(), &Cancellation::new())
            .unwrap();
        check(&cache);
    }

    #[test]
    fn original_native_names_and_call_marker_keep_the_authored_contract() {
        let source = include_bytes!("../../tests/fixtures/hare/original-part1.ha");
        assert_eq!(source.len(), 1742);
        census(source, 16_384, true, |cache| {
            assert!(!cache.exhausted);
            for (syntax, name) in [
                ("hare.struct", 63..71),
                ("hare.field", 85..89),
                ("hare.function", 121..131),
                ("hare.function", 1408..1412),
            ] {
                assert!(cache.declarations.values().any(|declaration| declaration.syntax == syntax && declaration.name == name));
            }
            assert!(cache.calls.values().any(|start| *start == 118));
        });
    }

    #[test]
    fn ordinary_functions_struct_fields_and_multiple_written_types_are_distinct() {
        for source in [
            "type first = struct { one: int, }, second = struct { two: int, }; fn target() void = {}; fn caller(value: int) void = { target(); };",
            "use fmt; fn target() void = {}; fn caller() void = { target(); };",
            "fn target() void = {}; fn caller() void = { for (const item .. values) { target(); }; };",
        ] {
            census(source.as_bytes(), 16_384, true, |cache| {
                assert!(!cache.exhausted, "{source}");
                assert_eq!(cache.calls.len(), 1, "{source}");
            });
        }
    }

    #[test]
    fn native_competitors_attributes_recovery_and_forward_targets_withhold_calls() {
        for source in [
            "fn target() void = {}; fn caller(target: fn() void) void = { target(); };",
            "fn target() void = {}; fn caller() void = { let target = 1; target(); };",
            "fn target() void = {}; fn caller() void = { for (const target .. values) { target(); }; };",
            "use other::{target}; fn target() void = {}; fn caller() void = { target(); };",
            "use other::*; fn target() void = {}; fn caller() void = { target(); };",
            "fn target() void = {}; fn target() void = {}; fn caller() void = { target(); };",
            "fn target() void; fn caller() void = { target(); };",
            "@init fn target() void = {}; fn caller() void = { target(); };",
            "let target: int = 1; fn target() void = {}; fn caller() void = { target(); };",
            "fn target() void = {}; fn caller() void = { other::target(); };",
            "fn target() void = {}; fn caller() void = { object.target(); };",
            "fn target() void = {}; fn caller() void = { target(; };",
            "fn caller() void = { target(); }; fn target() void = {};",
        ] {
            census(source.as_bytes(), 16_384, true, |cache| {
                assert!(!cache.exhausted, "{source}");
                assert!(cache.calls.is_empty(), "{source}");
            });
        }
    }

    #[test]
    fn finite_work_partial_input_and_cancel_never_keep_a_call_certificate() {
        let source = b"fn target() void = {}; fn caller() void = { target(); };";
        census(source, 1, true, |cache| {
            assert!(cache.exhausted);
            assert!(cache.calls.is_empty());
        });
        census(source, 4096, false, |cache| {
            assert!(cache.declarations.is_empty());
            assert!(cache.calls.is_empty());
        });
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hare::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            CaptureCache::new(source, 4096, true).initialize(tree.root_node(), &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        let mut cache = CaptureCache::new(source, 4096, true);
        assert!(
            candidate(
                tree.root_node(),
                StructuralRole::Root,
                b"different",
                &Cancellation::new(),
                &mut cache
            )
            .is_err()
        );
    }

    #[test]
    fn changing_only_function_body_preserves_written_signature() {
        let mut previous = None;
        for source in [
            "fn target() int = { return 1; };",
            "fn target() int = { return 123456; };",
        ] {
            census(source.as_bytes(), 4096, true, |cache| {
                let declaration = cache
                    .declarations
                    .values()
                    .find(|declaration| declaration.syntax == "hare.function")
                    .unwrap();
                let header = &source[declaration.header.clone()];
                if let Some(old) = &previous {
                    assert_eq!(old, header);
                }
                previous = Some(header.to_owned());
            });
        }
    }
}

#[cfg(test)]
mod native_capture_role_tests {
    use super::*;
    use tree_sitter::{Query, QueryCursor, StreamingIterator};

    #[test]
    fn original_function_and_type_keep_all_four_native_identity_roles() {
        let source = include_bytes!("../../tests/fixtures/hare/original-part1.ha");
        assert_eq!(source.len(), 1742);
        let language = tree_sitter_hare::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let written_type = tree.root_node().descendant_for_byte_range(63, 71).unwrap();
        assert_eq!(written_type.kind(), "identifier");
        let mut function = tree
            .root_node()
            .descendant_for_byte_range(121, 131)
            .unwrap();
        while function.kind() != "function_declaration" {
            function = function.parent().unwrap();
        }
        let query = Query::new(&language, include_str!("../../queries/hare.scm")).unwrap();
        let mut cache = CaptureCache::new(source, 16_384, true);
        for (node, declaration_syntax, name_span) in [
            (function, "hare.function", 121..131),
            (written_type, "hare.struct", 63..71),
        ] {
            let mut seen = [0_usize; 4];
            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());
            while let Some(matched) = matches.next() {
                for capture in matched.captures {
                    if capture.node.id() != node.id() {
                        continue;
                    }
                    let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
                    let (index, role, syntax) = match name {
                        "scope" => (0, StructuralRole::Scope, declaration_syntax),
                        "declaration" => (1, StructuralRole::Declaration, declaration_syntax),
                        "definition" => (2, StructuralRole::Definition, "hare.name"),
                        "signature" => (3, StructuralRole::Signature, "hare.header"),
                        _ => continue,
                    };
                    seen[index] += 1;
                    let proof =
                        candidate(capture.node, role, source, &Cancellation::new(), &mut cache)
                            .unwrap()
                            .unwrap();
                    assert_eq!(proof.role, role);
                    assert_eq!(proof.syntax, syntax);
                    assert!(proof.required);
                    assert!(proof.start <= proof.end && proof.end <= source.len());
                    if role == StructuralRole::Definition {
                        assert_eq!(proof.start..proof.end, name_span);
                    }
                }
            }
            assert_eq!(seen, [1, 1, 1, 1], "native roles for {declaration_syntax}");
        }
        assert!(!cache.exhausted());
    }

    #[test]
    fn unproved_bare_calls_remain_required_without_target_certificates() {
        let source = b"fn caller() void = { target(); };";
        let language = tree_sitter_hare::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let start = source
            .windows(6)
            .position(|bytes| bytes == b"target")
            .unwrap();
        let callee = tree
            .root_node()
            .descendant_for_byte_range(start, start + 6)
            .unwrap();
        assert_eq!(callee.kind(), "identifier");
        let mut cache = CaptureCache::new(source, 16_384, true);
        let call = candidate(
            callee,
            StructuralRole::Reference,
            source,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(call.syntax, "hare.unresolved_call");
        assert!(
            call.required,
            "written call identity survives zero optional retention"
        );
        assert_eq!(call.start..call.end, start..start + 6);
        assert!(
            cache.calls.is_empty(),
            "a required occurrence does not prove a target"
        );
        let pack = super::super::QueryPack::compile(
            crate::registry::GrammarFamily::Hare,
            include_str!("../../queries/hare.scm"),
        )
        .unwrap();
        let extracted = pack
            .extract(
                crate::registry::GrammarFamily::Hare,
                &tree,
                source,
                16_384,
                0,
                &Cancellation::new(),
            )
            .unwrap();
        let calls: Vec<_> = extracted
            .candidates
            .iter()
            .filter(|fact| {
                fact.role == StructuralRole::Reference && fact.syntax == "hare.unresolved_call"
            })
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "required identity query retains the unproved call"
        );
        assert!(calls[0].required);
        assert_eq!(calls[0].start..calls[0].end, start..start + 6);
        assert!(!extracted.candidates.iter().any(|fact| matches!(
            fact.syntax,
            "hare.direct_file_function" | "hare.call_target"
        )));
    }
}
