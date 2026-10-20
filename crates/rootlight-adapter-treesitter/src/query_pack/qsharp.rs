//! Written Q# declarations and finite same-owner direct-argument reads.
//! External invocation, specialization execution and quantum effects stay unknown.
use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::ops::Range;
use tree_sitter::Node;

struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    label: &'static str,
}
pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
}
impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES) * 64,
            exhausted: false,
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(left) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = left;
        Ok(true)
    }
    fn clean(&self, n: Node<'_>) -> bool {
        !n.has_error()
            && !n.is_missing()
            && !n.is_error()
            && n.start_byte() < n.end_byte()
            && n.end_byte() <= self.source.len()
    }
    fn text(
        &mut self,
        n: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if !self.clean(n) {
            return Ok(None);
        }
        let Some(raw) = self.source.get(n.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
        }
        Ok(std::str::from_utf8(raw).ok())
    }
    fn name(
        &mut self,
        n: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if !matches!(n.kind(), "identifier" | "path_or_identifier") {
            return Ok(None);
        }
        let Some(text) = self.text(n, cancel)? else {
            return Ok(None);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language(
                "qsharp",
                text,
                text.len(),
            )
            .filter(|name| name.as_ref() == text)
            .map(|_| text),
        )
    }
    fn field<'t>(
        &mut self,
        n: Node<'t>,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut selected = None;
        let mut cursor = n.walk();
        for (i, child) in n.children(&mut cursor).enumerate() {
            if !self.step(cancel)? {
                return Ok(None);
            }
            let i = u32::try_from(i).map_err(|_| query_failure("qsharp-native-child-index"))?;
            if n.field_name_for_child(i) == Some(name) {
                if selected.is_some() {
                    return Ok(None);
                }
                selected = Some(child)
            }
        }
        Ok(selected)
    }
    fn only<'t>(
        &mut self,
        n: Node<'t>,
        kind: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut result = None;
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if matches!(child.kind(), "comment" | "doc_comment") {
                continue;
            }
            if child.kind() != kind || result.is_some() {
                return Ok(None);
            }
            result = Some(child)
        }
        Ok(result)
    }
    fn callable<'t>(
        &mut self,
        n: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'t>, Node<'t>, Node<'t>)>, AdapterError> {
        if !self.complete
            || !self.clean(n)
            || n.kind() != "callable_decl"
            || !n
                .parent()
                .is_some_and(|p| matches!(p.kind(), "source_file" | "namespace"))
        {
            return Ok(None);
        }
        let Some(name) = self.field(n, "name", cancel)? else {
            return Ok(None);
        };
        let Some(params) = self.field(n, "params", cancel)? else {
            return Ok(None);
        };
        let Some(body) = self.field(n, "body", cancel)? else {
            return Ok(None);
        };
        if name.kind() != "identifier"
            || self.name(name, cancel)?.is_none()
            || params.kind() != "param_list"
            || body.kind() != "callable_body"
            || !self.clean(params)
            || !self.clean(body)
        {
            return Ok(None);
        }
        Ok(Some((name, params, body)))
    }
    fn declaration(
        &mut self,
        n: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Declaration>, AdapterError> {
        if !self.complete || !self.clean(n) {
            return Ok(None);
        }
        match n.kind() {
            "namespace" => {
                if !n
                    .parent()
                    .is_some_and(|p| p.kind() == "source_file" && p.parent().is_none())
                {
                    return Ok(None);
                }
                let Some(name) = self.field(n, "name", cancel)? else {
                    return Ok(None);
                };
                if self.name(name, cancel)?.is_none() {
                    return Ok(None);
                }
                Ok(Some(Declaration {
                    name: name.byte_range(),
                    header: n.start_byte()..name.end_byte(),
                    label: "qsharp.namespace",
                }))
            }
            "callable_decl" => {
                let Some((name, _, body)) = self.callable(n, cancel)? else {
                    return Ok(None);
                };
                Ok(Some(Declaration {
                    name: name.byte_range(),
                    header: n.start_byte()..body.start_byte(),
                    label: "qsharp.function",
                }))
            }
            "typed_param" => {
                let Some(params) = n.parent().filter(|p| p.kind() == "param_list") else {
                    return Ok(None);
                };
                let Some(owner) = params.parent() else {
                    return Ok(None);
                };
                let Some((_, actual, _)) = self.callable(owner, cancel)? else {
                    return Ok(None);
                };
                if actual != params {
                    return Ok(None);
                }
                let Some(name) = self.field(n, "name", cancel)? else {
                    return Ok(None);
                };
                if name.kind() != "identifier" || self.name(name, cancel)?.is_none() {
                    return Ok(None);
                }
                Ok(Some(Declaration {
                    name: name.byte_range(),
                    header: n.byte_range(),
                    label: "qsharp.parameter",
                }))
            }
            _ => Ok(None),
        }
    }
    fn namespace_name(
        &mut self,
        n: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        let Some(p) = n.parent() else { return Ok(None) };
        if p.kind() == "source_file" {
            return Ok(Some(""));
        }
        if p.kind() != "namespace" {
            return Ok(None);
        }
        let Some(name) = self.field(p, "name", cancel)? else {
            return Ok(None);
        };
        self.name(name, cancel)
    }
    fn unique_owner(
        &mut self,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some((name, _, _)) = self.callable(owner, cancel)? else {
            return Ok(false);
        };
        let Some(name) = self.name(name, cancel)? else {
            return Ok(false);
        };
        let Some(namespace) = self.namespace_name(owner, cancel)? else {
            return Ok(false);
        };
        let mut root = owner;
        while let Some(parent) = root.parent() {
            if !self.step(cancel)? {
                return Ok(false);
            }
            root = parent
        }
        if root.kind() != "source_file" || !self.clean(root) {
            return Ok(false);
        }
        let mut count = 0;
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if !self.step(cancel)? {
                return Ok(false);
            }
            if matches!(node.kind(), "newtype_decl" | "struct_decl") {
                return Ok(false);
            }
            if node.kind() == "callable_decl" {
                let Some((candidate, _, _)) = self.callable(node, cancel)? else {
                    return Ok(false);
                };
                if self.name(candidate, cancel)? == Some(name)
                    && self.namespace_name(node, cancel)? == Some(namespace)
                {
                    count += 1;
                    if count > 1 {
                        return Ok(false);
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
                    return Ok(count == 1);
                }
            }
        }
    }
    fn formals(&mut self, owner: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some((_, params, _)) = self.callable(owner, cancel)? else {
            return Ok(false);
        };
        let mut outer = params.walk();
        for param in params.named_children(&mut outer) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if matches!(param.kind(), "comment" | "doc_comment") {
                continue;
            }
            if param.kind() != "typed_param" {
                return Ok(false);
            }
            let Some(name) = self.field(param, "name", cancel)? else {
                return Ok(false);
            };
            let Some(spelling) = self.name(name, cancel)? else {
                return Ok(false);
            };
            let Some(ty) = self.field(param, "type", cancel)? else {
                return Ok(false);
            };
            if ty.kind() != "builtin_type" || !self.clean(ty) {
                return Ok(false);
            }
            let mut inner = params.walk();
            let mut count = 0;
            for other in params.named_children(&mut inner) {
                if !self.step(cancel)? {
                    return Ok(false);
                }
                if other.kind() != "typed_param" {
                    continue;
                }
                let Some(other) = self.field(other, "name", cancel)? else {
                    return Ok(false);
                };
                if self.name(other, cancel)? == Some(spelling) {
                    count += 1;
                    if count > 1 {
                        return Ok(false);
                    }
                }
            }
            if count != 1 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn formal_name(
        &mut self,
        owner: Node<'_>,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some((_, params, _)) = self.callable(owner, cancel)? else {
            return Ok(false);
        };
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if param.kind() != "typed_param" {
                continue;
            }
            if let Some(node) = self.field(param, "name", cancel)?
                && self.name(node, cancel)? == Some(name)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn specialization_kind(
        &mut self,
        n: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<u8>, AdapterError> {
        if n.kind() != "spec_name" || !self.clean(n) {
            return Ok(None);
        }
        let mut bits = 0;
        let mut cursor = n.walk();
        for token in n.children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            let flag = match token.kind() {
                "body" => 1,
                "adjoint" => 2,
                "controlled" => 4,
                _ => return Ok(None),
            };
            if bits & flag != 0 {
                return Ok(None);
            }
            bits |= flag
        }
        Ok(matches!(bits, 1 | 2 | 4 | 6).then_some(bits))
    }
    fn body_block<'t>(
        &mut self,
        body: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        if let Some(block) = self.only(body, "block", cancel)? {
            return Ok(Some(block));
        }
        let Some(list) = self.only(body, "specialization_list", cancel)? else {
            return Ok(None);
        };
        let mut cursor = list.walk();
        let mut seen = 0u8;
        let mut actual = None;
        for spec in list.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if matches!(spec.kind(), "comment" | "doc_comment") {
                continue;
            }
            if spec.kind() != "specialization" {
                return Ok(None);
            }
            let mut children = spec.walk();
            let (mut name, mut param, mut block, mut generator) = (None, None, None, None);
            for child in spec.named_children(&mut children) {
                if !self.step(cancel)? {
                    return Ok(None);
                }
                let slot = match child.kind() {
                    "spec_name" => &mut name,
                    "spec_param" => &mut param,
                    "block" => &mut block,
                    "spec_generator" => &mut generator,
                    "comment" | "doc_comment" => continue,
                    _ => return Ok(None),
                };
                if slot.is_some() {
                    return Ok(None);
                }
                *slot = Some(child)
            }
            let Some(name) = name else { return Ok(None) };
            let Some(kind) = self.specialization_kind(name, cancel)? else {
                return Ok(None);
            };
            let mask = 1u8 << kind;
            if seen & mask != 0 {
                return Ok(None);
            }
            seen |= mask;
            if kind == 1 {
                if generator.is_some() || actual.is_some() {
                    return Ok(None);
                }
                let Some(block) = block else { return Ok(None) };
                let Some(param) = param else { return Ok(None) };
                let valid = match param.child_count() {
                    1 => param.child(0).is_some_and(|c| c.kind() == "..."),
                    3 => {
                        param.child(0).is_some_and(|c| c.kind() == "(")
                            && param.child(1).is_some_and(|c| c.kind() == "...")
                            && param.child(2).is_some_and(|c| c.kind() == ")")
                    }
                    _ => false,
                };
                if !self.step(cancel)? || !valid {
                    return Ok(None);
                }
                actual = Some(block)
            } else if generator.is_none() || block.is_some() || param.is_some() {
                return Ok(None);
            }
        }
        Ok(actual)
    }
    fn proof<'t>(
        &mut self,
        owner: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        if !self.complete || !self.unique_owner(owner, cancel)? || !self.formals(owner, cancel)? {
            return Ok(None);
        }
        let Some((_, _, body)) = self.callable(owner, cancel)? else {
            return Ok(None);
        };
        let mut cursor = owner.walk();
        for child in owner.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if matches!(child.kind(), "attribute" | "type_params") {
                return Ok(None);
            }
        }
        let Some(block) = self.body_block(body, cancel)? else {
            return Ok(None);
        };
        let mut cursor = block.walk();
        for statement in block.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if matches!(
                statement.kind(),
                "comment" | "doc_comment" | "empty_statement"
            ) {
                continue;
            }
            if statement.kind() != "expression_statement" {
                return Ok(None);
            }
            let Some(call) = self.only(statement, "call_expression", cancel)? else {
                return Ok(None);
            };
            let mut children = call.walk();
            let mut index = 0;
            for value in call.named_children(&mut children) {
                if !self.step(cancel)? {
                    return Ok(None);
                }
                if matches!(value.kind(), "comment" | "doc_comment") {
                    continue;
                }
                if value.kind() != "identifier" {
                    return Ok(None);
                }
                let Some(name) = self.name(value, cancel)? else {
                    return Ok(None);
                };
                if index == 0 && self.formal_name(owner, name, cancel)? {
                    return Ok(None);
                }
                index += 1;
            }
            if index == 0 {
                return Ok(None);
            }
        }
        if self.exhausted {
            return Ok(None);
        }
        Ok(Some(block))
    }
    fn read(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if node.kind() != "identifier" {
            return Ok(false);
        }
        let Some(call) = node.parent().filter(|n| n.kind() == "call_expression") else {
            return Ok(false);
        };
        let Some(statement) = call.parent().filter(|n| n.kind() == "expression_statement") else {
            return Ok(false);
        };
        let Some(block) = statement.parent().filter(|n| n.kind() == "block") else {
            return Ok(false);
        };
        let mut current = block;
        let owner = loop {
            if !self.step(cancel)? {
                return Ok(false);
            }
            let Some(parent) = current.parent() else {
                return Ok(false);
            };
            if parent.kind() == "callable_decl" {
                break parent;
            }
            current = parent
        };
        if self.proof(owner, cancel)? != Some(block) {
            return Ok(false);
        }
        let mut cursor = call.walk();
        let mut first = None;
        for n in call.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if !matches!(n.kind(), "comment" | "doc_comment") {
                first = Some(n);
                break;
            }
        }
        if first == Some(node) {
            return Ok(false);
        }
        let Some(name) = self.name(node, cancel)? else {
            return Ok(false);
        };
        self.formal_name(owner, name, cancel)
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    mut role: StructuralRole,
    _source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.step(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let mut required = role.belongs_to_identity_closure();
    let label = if node.parent().is_none()
        && (node.kind() == "source_file" || node.is_error())
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        if cache.complete {
            range = 0..cache.source.len()
        }
        if role == StructuralRole::Root {
            "qsharp.file"
        } else {
            "qsharp.file_context"
        }
    } else if node.kind() == "callable_body" && role == StructuralRole::Signature {
        let Some(owner) = node.parent() else {
            return Ok(None);
        };
        if cache.proof(owner, cancel)?.is_none() {
            return Ok(None);
        }
        range = owner.byte_range();
        required = true;
        "qsharp.formal_binding_context"
    } else if node.kind() == "identifier"
        && matches!(role, StructuralRole::Reference | StructuralRole::Signature)
        && node.parent().is_some_and(|p| p.kind() == "call_expression")
    {
        if cache.name(node, cancel)?.is_none() {
            return Ok(None);
        }
        if cache.read(node, cancel)? {
            required = true;
            role = StructuralRole::Reference;
            "qsharp.formal_argument"
        } else if role == StructuralRole::Reference {
            "qsharp.unresolved_name"
        } else {
            return Ok(None);
        }
    } else if matches!(
        role,
        StructuralRole::Scope
            | StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
    ) {
        if let Some(declaration) = cache.declaration(node, cancel)? {
            match role {
                StructuralRole::Definition => {
                    range = declaration.name;
                    "qsharp.name"
                }
                StructuralRole::Signature => {
                    range = declaration.header;
                    "qsharp.header"
                }
                _ => declaration.label,
            }
        } else if role == StructuralRole::Declaration {
            "qsharp.unsupported"
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Expression {
        "qsharp.construct_unavailable"
    } else if role == StructuralRole::Comment {
        "qsharp.comment"
    } else if role == StructuralRole::StringLiteral {
        "qsharp.string"
    } else {
        return Ok(None);
    };
    let mut depth = 0usize;
    let mut parent = node.parent();
    while let Some(p) = parent {
        if !cache.step(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("qsharp-native-depth"))?;
        parent = p.parent()
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax: label,
        required,
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    fn tree(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_qsharp::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }
    fn find<'t>(root: Node<'t>, kind: &str, start: usize) -> Node<'t> {
        let mut nodes = vec![root];
        while let Some(node) = nodes.pop() {
            if node.kind() == kind && node.start_byte() == start {
                return node;
            }
            let mut cursor = node.walk();
            nodes.extend(node.named_children(&mut cursor));
        }
        panic!("missing {kind}@{start}")
    }
    #[test]
    fn original_native_declarations_and_required_read_share_exact_written_owners() {
        let source = include_str!("../../tests/fixtures/qsharp/Shorthand.qs");
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        for (kind, start, name) in [
            ("namespace", 0, "Microsoft.Quantum.Arithmetic"),
            ("callable_decl", 766, "MAJ"),
            ("typed_param", 780, "input0"),
            ("typed_param", 796, "input1"),
        ] {
            let node = find(parsed.root_node(), kind, start);
            for role in [
                StructuralRole::Declaration,
                StructuralRole::Definition,
                StructuralRole::Signature,
            ] {
                let mut cache = CaptureCache::new(source.as_bytes(), 16384, true);
                let result = candidate(
                    node,
                    role,
                    source.as_bytes(),
                    &Cancellation::new(),
                    &mut cache,
                )
                .unwrap()
                .unwrap();
                assert!(result.required);
                if role == StructuralRole::Definition {
                    assert_eq!(&source[result.start..result.end], name)
                }
            }
        }
        let leaf = find(parsed.root_node(), "identifier", 920);
        let mut cache = CaptureCache::new(source.as_bytes(), 16384, true);
        let result = candidate(
            leaf,
            StructuralRole::Reference,
            source.as_bytes(),
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (result.start, result.end, result.syntax, result.required),
            (920, 926, "qsharp.formal_argument", true)
        );
        let body = find(parsed.root_node(), "callable_body", 835);
        let result = candidate(
            body,
            StructuralRole::Signature,
            source.as_bytes(),
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (result.start, result.end, result.syntax),
            (766, 1073, "qsharp.formal_binding_context")
        );
    }
    #[test]
    fn direct_body_and_both_specialization_orders_keep_lexical_reads_without_callee_targets() {
        for tail in [
            "",
            " adjoint auto; controlled auto; controlled adjoint auto;",
            " adjoint auto; controlled auto; adjoint controlled auto;",
        ] {
            let source = if tail.is_empty() {
                "namespace Written { operation Measure(value : Int) : Unit { External(value); } }"
                    .to_owned()
            } else {
                format!(
                    "namespace Written {{ operation Measure(value : Int) : Unit {{ body(...) {{ External(value); }}{tail} }} }}"
                )
            };
            let parsed = tree(&source);
            assert!(!parsed.root_node().has_error(), "{source}");
            let mut cache = CaptureCache::new(source.as_bytes(), 16384, true);
            let usage = find(
                parsed.root_node(),
                "identifier",
                source.rfind("value").unwrap(),
            );
            assert!(cache.read(usage, &Cancellation::new()).unwrap());
            let callee = find(
                parsed.root_node(),
                "identifier",
                source.find("External").unwrap(),
            );
            assert!(!cache.read(callee, &Cancellation::new()).unwrap());
        }
    }
    #[test]
    fn native_shadow_duplicate_and_unsupported_expression_shapes_withhold_the_complete_read_proof()
    {
        for text in [
            "namespace Written { operation Measure(value : Int) : Unit { let other = value; External(value); } }",
            "namespace Written { operation Measure(value : Int, value : Int) : Unit { External(value); } }",
            "namespace Written { operation Measure(value : Int) : Unit { External(value + value); } }",
            "namespace Written { operation Measure(value : Int) : Unit { body(alias) { External(value); } } }",
        ] {
            let parsed = tree(text);
            assert!(!parsed.root_node().has_error(), "{text}");
            let owner = find(
                parsed.root_node(),
                "callable_decl",
                text.find("operation").unwrap(),
            );
            let mut cache = CaptureCache::new(text.as_bytes(), 16384, true);
            assert!(
                cache.proof(owner, &Cancellation::new()).unwrap().is_none(),
                "{text}"
            );
        }
    }
    #[test]
    fn native_work_partial_context_and_typed_cancellation_remain_authoritative() {
        let source =
            "namespace Written { operation Measure(value : Int) : Unit { External(value); } }";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let owner = find(
            parsed.root_node(),
            "callable_decl",
            source.find("operation").unwrap(),
        );
        let mut partial = CaptureCache::new(source.as_bytes(), 16384, false);
        assert!(
            partial
                .proof(owner, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let mut stopped = CaptureCache::new(source.as_bytes(), 1, true);
        stopped.remaining = 3;
        assert!(
            stopped
                .proof(owner, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(stopped.exhausted());
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source.as_bytes(), usize::MAX, true);
        assert_eq!(cache.remaining, HARD_MAX_QUERY_CAPTURES * 64);
        assert!(matches!(
            cache.proof(owner, &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
    // A signature capture keeps this native proof in the required query while
    // retaining the same occurrence role and source geometry after deduplication.

    #[test]
    fn paired_identity_signature_and_reference_select_the_same_required_formal_read() {
        let source = include_str!("../../tests/fixtures/qsharp/Shorthand.qs");
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let leaf = find(parsed.root_node(), "identifier", 920);
        let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
        for role in [StructuralRole::Reference, StructuralRole::Signature] {
            let result = candidate(
                leaf,
                role,
                source.as_bytes(),
                &Cancellation::new(),
                &mut cache,
            )
            .unwrap()
            .expect("the same native proof participates in identity extraction");
            assert_eq!((result.start, result.end), (920, 926));
            assert_eq!(result.syntax, "qsharp.formal_argument");
            assert_eq!(result.role, StructuralRole::Reference);
            assert!(result.required);
        }
        let callee = find(
            parsed.root_node(),
            "identifier",
            source.find("CNOT(").unwrap(),
        );
        assert!(
            candidate(
                callee,
                StructuralRole::Signature,
                source.as_bytes(),
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_none(),
            "a paired capture cannot create an unproved callee identity"
        );
    }
}
