//! Native written SML/WAT identities without inferred SML pattern bindings.
//! Structural headers do not borrow semantic closure from unrelated siblings.

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::ops::Range;
use tree_sitter::Node;

struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    wat: bool,
    remaining: usize,
    exhausted: bool,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool, wat: bool) -> Self {
        Self {
            source,
            complete,
            wat,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES) * 32,
            exhausted: false,
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
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
    fn clean(&self, node: Node<'_>) -> bool {
        !node.has_error()
            && !node.is_error()
            && !node.is_missing()
            && node.start_byte() < node.end_byte()
            && node.end_byte() <= self.source.len()
    }
    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if !self.clean(node) || node.kind() != if self.wat { "identifier" } else { "vid" } {
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
        let language = if self.wat {
            "webassembly"
        } else {
            "standard-ml"
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language(
                language,
                text,
                text.len(),
            )
            .filter(|name| name.as_ref() == text)
            .map(|_| text),
        )
    }
    fn only<'t>(
        &mut self,
        node: Node<'t>,
        kind: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut result = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(child.kind(), "comment" | "block_comment" | "line_comment") {
                continue;
            }
            if child.kind() != kind || result.is_some() {
                return Ok(None);
            }
            result = Some(child);
        }
        Ok(result)
    }
    fn field<'t>(
        &mut self,
        node: Node<'t>,
        field: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for (i, child) in node.children(&mut cursor).enumerate() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let i = u32::try_from(i).map_err(|_| query_failure("sml-wat-native-child-index"))?;
            if node.field_name_for_child(i) == Some(field) {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(child);
            }
        }
        Ok(found)
    }
    fn sml_header<'t>(
        &mut self,
        rule: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'t>, Node<'t>)>, AdapterError> {
        if !self.clean(rule) || rule.kind() != "fmrule" {
            return Ok(None);
        }
        let Some(binding) = rule.parent().filter(|p| p.kind() == "fvalbind") else {
            return Ok(None);
        };
        let Some(group) = binding.parent().filter(|p| p.kind() == "fun_dec") else {
            return Ok(None);
        };
        if !group
            .parent()
            .is_some_and(|p| p.kind() == "source_file" && p.parent().is_none())
            || self.only(binding, "fmrule", cancel)? != Some(rule)
            || self.only(group, "fvalbind", cancel)? != Some(binding)
        {
            return Ok(None);
        }
        let Some(name) = self.field(rule, "name", cancel)? else {
            return Ok(None);
        };
        let Some(body) = self.field(rule, "def", cancel)? else {
            return Ok(None);
        };
        if self.name(name, cancel)?.is_none()
            || !self.clean(body)
            || rule.start_byte() != name.start_byte()
            || self.field(rule, "argl", cancel)?.is_some()
            || self.field(rule, "argr", cancel)?.is_some()
        {
            return Ok(None);
        }
        // The native prefix names a function; argument patterns require the incoming value environment.
        // Do not turn a bare pattern identifier into a Parameter or a binding certificate.
        Ok(Some((name, body)))
    }
    fn wat_header<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        if !self.clean(node) {
            return Ok(None);
        }
        let allowed = match node.kind() {
            "module" => node
                .parent()
                .is_some_and(|p| p.kind() == "root" && p.parent().is_none()),
            "module_field_func" => node.parent().is_some_and(|p| p.kind() == "module"),
            "param" => node
                .parent()
                .is_some_and(|p| p.kind() == "module_field_func"),
            _ => false,
        };
        if !allowed {
            return Ok(None);
        }
        let mut name = None;
        let mut scalar_types = 0;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "identifier" {
                if name.is_some() || self.name(child, cancel)?.is_none() {
                    return Ok(None);
                }
                name = Some(child);
            }
            if node.kind() == "param" {
                match child.kind() {
                    "identifier" => {}
                    "num_type" => scalar_types += 1,
                    "comment" | "block_comment" | "line_comment" => {}
                    _ => return Ok(None),
                }
            }
        }
        if node.kind() == "param" && scalar_types != 1 {
            return Ok(None);
        }
        let Some(name) = name else { return Ok(None) };
        if node.kind() == "module_field_func"
            && !node.parent().is_some_and(|parent| {
                self.clean(parent)
                    && parent.kind() == "module"
                    && parent
                        .parent()
                        .is_some_and(|root| root.kind() == "root" && root.parent().is_none())
            })
        {
            return Ok(None);
        }
        if node.kind() == "param" {
            let Some(parent) = node.parent() else {
                return Ok(None);
            };
            if self.wat_header(parent, cancel)?.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(name))
    }
    fn declaration(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Declaration>, AdapterError> {
        if !self.complete {
            return Ok(None);
        }
        if self.wat {
            let Some(name) = self.wat_header(node, cancel)? else {
                return Ok(None);
            };
            let syntax = match node.kind() {
                "module" => "wat.module",
                "module_field_func" => "wat.function",
                "param" => "wat.parameter",
                _ => return Ok(None),
            };
            let mut end = name.end_byte();
            if node.kind() == "module_field_func" {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if !self.visit(cancel)? {
                        return Ok(None);
                    }
                    if matches!(
                        child.kind(),
                        "identifier"
                            | "export"
                            | "param"
                            | "result"
                            | "type_use"
                            | "comment"
                            | "block_comment"
                            | "line_comment"
                    ) {
                        end = child.end_byte();
                    } else {
                        break;
                    }
                }
            } else if node.kind() == "param" {
                end = node.end_byte();
            }
            return Ok(Some(Declaration {
                name: name.byte_range(),
                header: node.start_byte()..end,
                syntax,
            }));
        }
        if node.kind() == "fmrule" {
            let Some((name, body)) = self.sml_header(node, cancel)? else {
                return Ok(None);
            };
            return Ok(Some(Declaration {
                name: name.byte_range(),
                header: node.start_byte()..body.start_byte(),
                syntax: "sml.function",
            }));
        }
        Ok(None)
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    _source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let actual_role = role;
    let required = role.belongs_to_identity_closure();
    let root_kind = if cache.wat { "root" } else { "source_file" };
    let syntax = if node.parent().is_none()
        && (node.kind() == root_kind || node.is_error())
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        if cache.complete {
            range = 0..cache.source.len();
        }
        if cache.wat {
            if role == StructuralRole::Root {
                "wat.source_file"
            } else {
                "wat.file"
            }
        } else if role == StructuralRole::Root {
            "sml.source_file"
        } else {
            "sml.file"
        }
    } else if !cache.wat && node.kind() == "vid" && role == StructuralRole::Reference {
        if !node.parent().is_some_and(|p| {
            p.kind() == "longvid" && p.parent().is_some_and(|v| v.kind() == "vid_exp")
        }) {
            return Ok(None);
        }
        "sml.unresolved_name"
    } else if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        if let Some(d) = cache.declaration(node, cancel)? {
            match role {
                StructuralRole::Definition => {
                    range = d.name;
                    if cache.wat { "wat.name" } else { "sml.name" }
                }
                StructuralRole::Signature => {
                    range = d.header;
                    if cache.wat {
                        "wat.header"
                    } else {
                        "sml.header"
                    }
                }
                _ => d.syntax,
            }
        } else if role == StructuralRole::Declaration {
            if cache.wat {
                "wat.unsupported"
            } else {
                "sml.unsupported"
            }
        } else {
            return Ok(None);
        }
    } else if cache.wat && role == StructuralRole::Reference && node.kind() == "identifier" {
        if cache.name(node, cancel)?.is_none() {
            return Ok(None);
        }
        if let Some(parent) = node.parent()
            && cache
                .declaration(parent, cancel)?
                .is_some_and(|d| d.name == node.byte_range())
        {
            return Ok(None);
        }
        "wat.unresolved_name"
    } else if role == StructuralRole::Expression {
        if cache.wat {
            "wat.construct_unavailable"
        } else {
            "sml.construct_unavailable"
        }
    } else if role == StructuralRole::Comment {
        if cache.wat {
            "wat.comment"
        } else {
            "sml.comment"
        }
    } else if role == StructuralRole::StringLiteral {
        if cache.wat {
            "wat.string"
        } else {
            "sml.string"
        }
    } else {
        return Ok(None);
    };
    let mut depth = 0_usize;
    let mut parent = node.parent();
    while let Some(p) = parent {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("sml-wat-depth"))?;
        parent = p.parent();
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role: actual_role,
        syntax,
        required,
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    fn tree(source: &str, wat: bool) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = if wat {
            tree_sitter_wat::LANGUAGE.into()
        } else {
            tree_sitter_sml::LANGUAGE.into()
        };
        parser.set_language(&language).unwrap();
        parser.parse(source, None).unwrap()
    }
    fn find<'t>(root: Node<'t>, kind: &str, start: usize) -> Node<'t> {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if node.kind() == kind && node.start_byte() == start {
                return node;
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        panic!("missing native {kind}@{start}")
    }
    #[test]
    fn original_native_written_identities_keep_all_four_roles_and_exact_leaves() {
        for (source, wat, kind, start, expected) in [
            (
                include_str!("../../tests/fixtures/standard_ml/deephandle.sml"),
                false,
                "fmrule",
                4,
                "f",
            ),
            (
                include_str!("../../tests/fixtures/webassembly/sieve.wat"),
                true,
                "module",
                0,
                "$sieve-raw",
            ),
            (
                include_str!("../../tests/fixtures/webassembly/sieve.wat"),
                true,
                "module_field_func",
                39,
                "$sieve",
            ),
            (
                include_str!("../../tests/fixtures/webassembly/sieve.wat"),
                true,
                "param",
                69,
                "$n",
            ),
        ] {
            let parsed = tree(source, wat);
            assert!(!parsed.root_node().has_error());
            let node = find(parsed.root_node(), kind, start);
            for role in [
                StructuralRole::Scope,
                StructuralRole::Declaration,
                StructuralRole::Definition,
                StructuralRole::Signature,
            ] {
                let mut cache = CaptureCache::new(source.as_bytes(), 16384, true, wat);
                let proof = candidate(
                    node,
                    role,
                    source.as_bytes(),
                    &Cancellation::new(),
                    &mut cache,
                )
                .unwrap()
                .unwrap();
                assert!(proof.required);
                assert!(!cache.exhausted());
                if role == StructuralRole::Definition {
                    assert_eq!(&source[proof.start..proof.end], expected);
                }
            }
        }
    }
    #[test]
    fn sml_pattern_names_never_become_parameters_without_incoming_basis_authority() {
        let source = "fun measure value = value;";
        let parsed = tree(source, false);
        assert!(!parsed.root_node().has_error());
        let parameter = find(parsed.root_node(), "vid", source.find("value").unwrap());
        let mut cache = CaptureCache::new(source.as_bytes(), 16384, true, false);
        assert_eq!(
            cache
                .declaration(parameter, &Cancellation::new())
                .unwrap()
                .map(|d| d.syntax),
            None
        );
        let usage = find(parsed.root_node(), "vid", source.rfind("value").unwrap());
        assert_eq!(
            candidate(
                usage,
                StructuralRole::Reference,
                source.as_bytes(),
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .unwrap()
            .syntax,
            "sml.unresolved_name"
        );
    }
    #[test]
    fn wat_type_formals_and_locals_cannot_donate_function_parameters() {
        let source = "(module (type $signature (func (param $documentation i32))) (func $measure (param $value i32) (local $local i32) local.get $value))";
        let parsed = tree(source, true);
        assert!(!parsed.root_node().has_error());
        let mut cache = CaptureCache::new(source.as_bytes(), 16384, true, true);
        for (kind, start) in [
            ("param", source.find("(param").unwrap()),
            ("local", source.find("(local").unwrap()),
        ] {
            assert!(
                cache
                    .declaration(find(parsed.root_node(), kind, start), &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
        let param = find(parsed.root_node(), "param", source.rfind("(param").unwrap());
        assert_eq!(
            cache
                .declaration(param, &Cancellation::new())
                .unwrap()
                .unwrap()
                .syntax,
            "wat.parameter"
        );
    }
    #[test]
    fn incomplete_input_work_exhaustion_and_cancellation_withhold_written_identity() {
        let source = "fun measure value = value;";
        let parsed = tree(source, false);
        let node = find(parsed.root_node(), "fmrule", 4);
        let mut partial = CaptureCache::new(source.as_bytes(), 16384, false, false);
        assert!(
            candidate(
                node,
                StructuralRole::Definition,
                source.as_bytes(),
                &Cancellation::new(),
                &mut partial
            )
            .unwrap()
            .is_none()
        );
        let mut limited = CaptureCache::new(source.as_bytes(), 0, true, false);
        assert!(
            candidate(
                node,
                StructuralRole::Definition,
                source.as_bytes(),
                &Cancellation::new(),
                &mut limited
            )
            .unwrap()
            .is_none()
        );
        assert!(limited.exhausted());
        let cancel = Cancellation::new();
        assert!(cancel.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source.as_bytes(), usize::MAX, true, false);
        assert_eq!(cache.remaining, HARD_MAX_QUERY_CAPTURES * 32);
        assert!(matches!(
            candidate(
                node,
                StructuralRole::Definition,
                source.as_bytes(),
                &cancel,
                &mut cache
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
