//! Certify direct returned self members within one complete written struct.
//! Source endpoints carry the exact member and self-formal declarations; wider
//! expressions, overloads and unmodeled namespaces retain their unproved names.

use super::*;
use rootlight_adapter_sdk::SinkError;

#[derive(Clone)]
pub(super) struct MemberProof {
    reference: Range<usize>,
    target: Range<usize>,
    self_name: Range<usize>,
    call: bool,
}

impl CaptureCache<'_> {
    fn only_named<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            if found.is_some() {
                return Ok(None);
            }
            found = Some(child);
        }
        Ok(found)
    }

    fn ordinary_method<'t>(
        &mut self,
        method: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if method.kind() != "function_definition"
            || !self.clean(method)
            || self.field(method, "type_parameters", cancel)?.is_some()
        {
            return Ok(false);
        }
        let Some(wrapper) = self.wrapper(method, cancel)? else {
            return Ok(false);
        };
        if wrapper != method {
            let mut cursor = wrapper.walk();
            for child in wrapper.named_children(&mut cursor) {
                if !self.visit(cancel)? {
                    return Ok(false);
                }
                if child == method || child.kind() == "comment" {
                    continue;
                }
                let Some(raw) = self.source.get(child.byte_range()) else {
                    return Ok(false);
                };
                for _ in raw.chunks(64) {
                    if !self.visit(cancel)? {
                        return Ok(false);
                    }
                }
                // Other decorators can generate or replace members.
                if child.kind() != "decorator" || raw.trim_ascii() != b"@always_inline" {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn self_formal<'t>(
        &mut self,
        method: Node<'t>,
        sole: bool,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let Some(parameters) = self.field(method, "parameters", cancel)? else {
            return Ok(None);
        };
        let mut found = None;
        let mut count = 0_usize;
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if parameter.kind() == "comment" {
                continue;
            }
            let Some(name) = self.parameter_name(parameter, cancel)? else {
                return Ok(None);
            };
            if !self.name(name, cancel)? {
                return Ok(None);
            }
            let is_self = self.source.get(name.byte_range()) == Some(b"self");
            if count == 0 {
                if parameter.kind() != "self_parameter"
                    || !is_self
                    || self.field(parameter, "type", cancel)?.is_some()
                {
                    return Ok(None);
                }
                found = Some(name);
            } else if is_self || sole {
                return Ok(None);
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| query_failure("mojo-native-parameter-count"))?;
        }
        Ok(found)
    }

    fn inspect_member(
        &mut self,
        attribute: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<MemberProof>, AdapterError> {
        if !self.complete || !self.visit(cancel)? || !self.clean(attribute) {
            return Ok(None);
        }
        let Some(object) = self.field(attribute, "object", cancel)? else {
            return Ok(None);
        };
        let Some(reference) = self.field(attribute, "attribute", cancel)? else {
            return Ok(None);
        };
        if !self.name(object, cancel)?
            || self.source.get(object.byte_range()) != Some(b"self")
            || !self.name(reference, cancel)?
        {
            return Ok(None);
        }
        let Some(parent) = attribute.parent() else {
            return Ok(None);
        };
        let call = parent.kind() == "call";
        let expression = if call {
            if self.field(parent, "function", cancel)? != Some(attribute) {
                return Ok(None);
            }
            let Some(arguments) = self.field(parent, "arguments", cancel)? else {
                return Ok(None);
            };
            let mut cursor = arguments.walk();
            for argument in arguments.named_children(&mut cursor) {
                if !self.visit(cancel)? || argument.kind() != "comment" {
                    return Ok(None);
                }
            }
            parent
        } else if parent.kind() == "subscript" {
            if self.field(parent, "value", cancel)? != Some(attribute) {
                return Ok(None);
            }
            parent
        } else {
            attribute
        };
        let Some(statement) = expression
            .parent()
            .filter(|node| node.kind() == "return_statement")
        else {
            return Ok(None);
        };
        if self.only_named(statement, cancel)? != Some(expression) {
            return Ok(None);
        }
        let Some(body) = statement.parent().filter(|node| node.kind() == "block") else {
            return Ok(None);
        };
        if self.only_named(body, cancel)? != Some(statement) {
            return Ok(None);
        }
        let Some(method) = body
            .parent()
            .filter(|node| node.kind() == "function_definition")
        else {
            return Ok(None);
        };
        if self.field(method, "body", cancel)? != Some(body)
            || !self.ordinary_method(method, cancel)?
        {
            return Ok(None);
        }
        let Some(self_name) = self.self_formal(method, false, cancel)? else {
            return Ok(None);
        };
        let Some(written) = self.wrapper(method, cancel)? else {
            return Ok(None);
        };
        let Some(members) = written.parent().filter(|node| node.kind() == "block") else {
            return Ok(None);
        };
        let Some(owner) = members
            .parent()
            .filter(|node| node.kind() == "class_definition")
        else {
            return Ok(None);
        };
        if !self.clean(owner)
            || self.prefix(owner, cancel)? != Some("mojo.struct")
            || self.field(owner, "type_parameters", cancel)?.is_some()
            || self.field(owner, "body", cancel)? != Some(members)
        {
            return Ok(None);
        }
        let Some(root) = owner.parent().filter(|node| node.kind() == "module") else {
            return Ok(None);
        };
        if !self.clean(root) || root.parent().is_some() || root.end_byte() != self.source.len() {
            return Ok(None);
        }
        // A single returned expression still needs a complete lexical census:
        // nested binders and assignment expressions cannot replace self.
        let mut walk = expression.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let node = walk.node();
            if !self.clean(node)
                || matches!(
                    node.kind(),
                    "lambda"
                        | "named_expression"
                        | "assignment"
                        | "augmented_assignment"
                        | "function_definition"
                        | "class_definition"
                )
            {
                return Ok(None);
            }
            if walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    break;
                }
            }
            if walk.node() == expression {
                break;
            }
        }
        let Some(written_name) = self.source.get(reference.byte_range()) else {
            return Ok(None);
        };
        let mut target = None;
        let mut cursor = members.walk();
        for child in members.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            let child = if child.kind() == "decorated_definition" {
                let Some(definition) = self.field(child, "definition", cancel)? else {
                    return Ok(None);
                };
                definition
            } else {
                child
            };
            let (name, is_method) = if child.kind() == "function_definition" {
                if !self.ordinary_method(child, cancel)? {
                    return Ok(None);
                }
                let Some(name) = self.field(child, "name", cancel)? else {
                    return Ok(None);
                };
                (name, true)
            } else if child.kind() == "assignment"
                && self.prefix(child, cancel)? == Some("mojo.field")
            {
                let Some(name) = self.field(child, "left", cancel)? else {
                    return Ok(None);
                };
                (name, false)
            } else {
                return Ok(None);
            };
            if !self.name(name, cancel)? {
                return Ok(None);
            }
            if self.source.get(name.byte_range()) != Some(written_name) {
                continue;
            }
            if target.is_some() || call != is_method || name.end_byte() > reference.start_byte() {
                return Ok(None);
            }
            if is_method && self.self_formal(child, true, cancel)?.is_none() {
                return Ok(None);
            }
            target = Some(name.byte_range());
        }
        Ok(target.map(|target| MemberProof {
            reference: reference.byte_range(),
            target,
            self_name: self_name.byte_range(),
            call,
        }))
    }

    pub(super) fn member_candidate(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        cancel: &Cancellation,
    ) -> Result<Option<QueryCandidate>, AdapterError> {
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature) {
            return Ok(None);
        }
        let attribute = if node.kind() == "attribute" && role == StructuralRole::Signature {
            node
        } else if node.kind() == "identifier"
            && let Some(attribute) = node.parent().filter(|parent| parent.kind() == "attribute")
        {
            attribute
        } else {
            return Ok(None);
        };
        if !self.members.contains_key(&attribute.id()) {
            let proof = self.inspect_member(attribute, cancel)?;
            self.members
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.members.insert(attribute.id(), proof);
        }
        let Some(proof) = self
            .members
            .get(&attribute.id())
            .and_then(Option::as_ref)
            .cloned()
        else {
            return Ok(None);
        };
        let (range, syntax, selected_role) = if node == attribute {
            (
                proof.target.start..proof.reference.end,
                if proof.call {
                    "mojo.self_method_target"
                } else {
                    "mojo.self_field_target"
                },
                StructuralRole::Signature,
            )
        } else if node.byte_range() == proof.reference {
            (
                proof.reference,
                if proof.call {
                    "mojo.self_method_call"
                } else {
                    "mojo.self_field_read"
                },
                if proof.call {
                    StructuralRole::Call
                } else {
                    StructuralRole::Reference
                },
            )
        } else if role == StructuralRole::Signature {
            (
                proof.self_name.start..proof.reference.end,
                "mojo.self_formal_target",
                StructuralRole::Signature,
            )
        } else {
            return Ok(None);
        };
        let mut depth = 0_usize;
        let mut parent = attribute.parent();
        while let Some(node) = parent {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            depth = depth
                .checked_add(1)
                .ok_or_else(|| query_failure("mojo-native-depth"))?;
            parent = node.parent();
        }
        Ok(Some(QueryCandidate {
            start: range.start,
            end: range.end,
            role: selected_role,
            syntax,
            required: true,
            native_depth: depth,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    const SOURCE: &str = "struct Written:\n    var data: List[Int]\n    fn size(self) -> Int:\n        return 1\n    fn read(self, index: Int) -> Int:\n        return self.data[index]\n    fn total(self) -> Int:\n        return self.size()\n";

    fn parsed(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Mojo))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn member<'t>(tree: &'t tree_sitter::Tree, source: &str, written: &str) -> Node<'t> {
        let start = source.rfind(written).unwrap() + 5;
        tree.root_node()
            .descendant_for_byte_range(start, start + written.len() - 5)
            .unwrap()
    }

    #[test]
    fn native_self_member_endpoints_are_distinct_required_identity_captures() {
        let tree = parsed(SOURCE);
        assert!(!tree.root_node().has_error());
        for (written, syntax, expected_role, target) in [
            (
                "self.data",
                "mojo.self_field_read",
                StructuralRole::Reference,
                SOURCE.find("data:").unwrap(),
            ),
            (
                "self.size",
                "mojo.self_method_call",
                StructuralRole::Call,
                SOURCE.find("size(self").unwrap(),
            ),
        ] {
            let node = member(&tree, SOURCE, written);
            let mut cache = CaptureCache::new(SOURCE.as_bytes(), 16_384, true);
            for role in [StructuralRole::Reference, StructuralRole::Signature] {
                let proof = cache
                    .member_candidate(node, role, &Cancellation::new())
                    .unwrap()
                    .unwrap();
                assert_eq!(proof.syntax, syntax);
                assert_eq!(proof.role, expected_role);
                assert!(proof.required);
                assert_eq!(
                    &SOURCE.as_bytes()[proof.start..proof.end],
                    &written.as_bytes()[5..]
                );
            }
            let attribute = node.parent().unwrap();
            let marker = cache
                .member_candidate(attribute, StructuralRole::Signature, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(marker.start, target);
            assert_eq!(marker.end, node.end_byte());
            assert_eq!(marker.role, StructuralRole::Signature);
            assert!(marker.required);
            let receiver = attribute.child_by_field_name("object").unwrap();
            let marker = cache
                .member_candidate(receiver, StructuralRole::Signature, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(marker.syntax, "mojo.self_formal_target");
            assert!(marker.start < receiver.start_byte());
            assert_eq!(&SOURCE.as_bytes()[marker.start..marker.start + 4], b"self");
        }
    }

    #[test]
    fn full_native_owner_formal_arity_shadow_and_work_barriers_withhold_member_proof() {
        for changed in [
            SOURCE.replace("struct Written", "class Written"),
            SOURCE.replace("fn total(self)", "fn total(other)"),
            SOURCE.replace("fn total(self)", "fn total(self: Other)"),
            SOURCE.replace("fn size(self)", "fn size(self, other: Int)"),
            SOURCE.replace("self.size()", "self.size(1)"),
            SOURCE.replace("self.size()", "self.size(1, 2)"),
            SOURCE.replace(
                "        return self.size()",
                "        var self = other\n        return self.size()",
            ),
            SOURCE.replace(
                "        return self.size()",
                "        fn nested(self):\n            return 1\n        return self.size()",
            ),
            SOURCE.replace(
                "        return self.size()",
                "        if True:\n            return self.size()\n        return 0",
            ),
            SOURCE.replace("return self.size()", "return 1 + self.size()"),
            SOURCE.replace("    fn size", "    @staticmethod\n    fn size"),
            SOURCE.replace("    fn size(self)", "    fn size[T: Int](self)"),
            SOURCE.replace("    var data:", "    if True:\n        var data:"),
        ] {
            let tree = parsed(&changed);
            assert!(
                !tree.root_node().has_error(),
                "barrier requires clean native syntax: {changed}"
            );
            let start = changed.rfind("self.size").unwrap() + 5;
            let node = tree
                .root_node()
                .descendant_for_byte_range(start, start + 4)
                .unwrap();
            let mut cache = CaptureCache::new(changed.as_bytes(), 16_384, true);
            assert!(
                cache
                    .member_candidate(node, StructuralRole::Reference, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{changed}"
            );
        }
        let recovered = format!("{SOURCE}\nfn broken( ->:\n");
        let tree = parsed(&recovered);
        assert!(tree.root_node().has_error());
        let node = member(&tree, &recovered, "self.size");
        let mut cache = CaptureCache::new(recovered.as_bytes(), 16_384, true);
        assert!(
            cache
                .member_candidate(node, StructuralRole::Reference, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let tree = parsed(SOURCE);
        let node = member(&tree, SOURCE, "self.size");
        let mut incomplete = CaptureCache::new(SOURCE.as_bytes(), 16_384, false);
        assert!(
            incomplete
                .member_candidate(node, StructuralRole::Signature, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let mut bounded = CaptureCache::new(SOURCE.as_bytes(), 0, true);
        assert!(
            bounded
                .member_candidate(node, StructuralRole::Reference, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(bounded.exhausted());
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(SOURCE.as_bytes(), 16_384, true);
        assert!(matches!(
            cache.member_candidate(node, StructuralRole::Reference, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
