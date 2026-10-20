//! Written Mojo identities require clean native declarations and lexical owners.
//! An unrelated recovered sibling does not invalidate a clean declaration.

use std::{collections::HashMap, ops::Range};

mod self_members;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};

struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
    members: HashMap<usize, Option<self_members::MemberProof>>,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES) * 32,
            exhausted: false,
            members: HashMap::new(),
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

    fn clean(&self, node: Node<'_>) -> bool {
        !node.is_error()
            && !node.is_missing()
            && !node.has_error()
            && node.start_byte() < node.end_byte()
            && node.end_byte() <= self.source.len()
    }

    fn field<'t>(
        &mut self,
        node: Node<'t>,
        field: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let index =
                u32::try_from(index).map_err(|_| query_failure("mojo-native-child-index"))?;
            if node.field_name_for_child(index) == Some(field) {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(child);
            }
        }
        Ok(found)
    }

    fn name(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.clean(node) || !matches!(node.kind(), "identifier" | "self") {
            return Ok(false);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(false);
        };
        for _ in raw.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(false);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language("mojo", text, text.len())
                .is_some_and(|name| name.as_ref() == text),
        )
    }

    fn prefix(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'static str>, AdapterError> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            return Ok(match (child.kind(), self.source.get(child.byte_range())) {
                ("class", Some(b"class")) => Some("mojo.class"),
                ("struct", Some(b"struct")) => Some("mojo.struct"),
                ("var", Some(b"var")) => Some("mojo.field"),
                _ => None,
            });
        }
        Ok(None)
    }

    fn wrapper<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let Some(parent) = node.parent() else {
            return Ok(None);
        };
        if parent.kind() == "decorated_definition" {
            if !self.clean(parent) || self.field(parent, "definition", cancel)? != Some(node) {
                return Ok(None);
            }
            Ok(Some(parent))
        } else {
            Ok(Some(node))
        }
    }

    fn owner_header(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.clean(node) {
            return Ok(false);
        }
        let Some(name) = self.field(node, "name", cancel)? else {
            return Ok(false);
        };
        if !self.name(name, cancel)? {
            return Ok(false);
        }
        let Some(body) = self.field(node, "body", cancel)? else {
            return Ok(false);
        };
        if body.kind() != "block" || body.start_byte() <= name.end_byte() {
            return Ok(false);
        }
        match node.kind() {
            "class_definition" => Ok(matches!(
                self.prefix(node, cancel)?,
                Some("mojo.class" | "mojo.struct")
            )),
            "function_definition" => Ok(self.field(node, "parameters", cancel)?.is_some()),
            _ => Ok(false),
        }
    }

    fn lexical_owner<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        let Some(written) = self.wrapper(node, cancel)? else {
            return Ok(None);
        };
        let Some(parent) = written.parent() else {
            return Ok(None);
        };
        if parent.kind() == "module" && parent.parent().is_none() && !parent.is_missing() {
            return Ok(Some(parent));
        }
        if parent.kind() != "block" || !self.clean(parent) {
            return Ok(None);
        }
        let Some(owner) = parent.parent() else {
            return Ok(None);
        };
        if !matches!(owner.kind(), "class_definition" | "function_definition")
            || !self.owner_header(owner, cancel)?
            || self.field(owner, "body", cancel)? != Some(parent)
        {
            return Ok(None);
        }
        // Every defining ancestor must itself have an admitted written owner.
        // Control-flow blocks and recovered wrappers never donate ownership.
        let mut current = owner;
        loop {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            let Some(written) = self.wrapper(current, cancel)? else {
                return Ok(None);
            };
            let Some(parent) = written.parent() else {
                return Ok(None);
            };
            if parent.kind() == "module" && parent.parent().is_none() && !parent.is_missing() {
                return Ok(Some(owner));
            }
            if parent.kind() != "block" || !self.clean(parent) {
                return Ok(None);
            }
            let Some(next) = parent.parent() else {
                return Ok(None);
            };
            if !matches!(next.kind(), "class_definition" | "function_definition")
                || !self.owner_header(next, cancel)?
                || self.field(next, "body", cancel)? != Some(parent)
            {
                return Ok(None);
            }
            current = next;
        }
    }

    fn parameter_name<'t>(
        &mut self,
        node: Node<'t>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'t>>, AdapterError> {
        if matches!(node.kind(), "identifier" | "self") {
            return Ok(Some(node));
        }
        if matches!(node.kind(), "default_parameter" | "typed_default_parameter") {
            return self.field(node, "name", cancel);
        }
        if !matches!(node.kind(), "typed_parameter" | "self_parameter") {
            return Ok(None);
        }
        let mut found = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(child.kind(), "identifier" | "self") {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(child);
            }
        }
        Ok(found)
    }

    fn declaration(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Declaration>, AdapterError> {
        if !self.complete || !self.clean(node) {
            return Ok(None);
        }
        if matches!(node.kind(), "class_definition" | "function_definition") {
            let Some(name) = self.field(node, "name", cancel)? else {
                return Ok(None);
            };
            if !self.name(name, cancel)? {
                return Ok(None);
            }
            let Some(body) = self.field(node, "body", cancel)? else {
                return Ok(None);
            };
            let Some(owner) = self.lexical_owner(node, cancel)? else {
                return Ok(None);
            };
            let syntax = if node.kind() == "class_definition" {
                let Some(kind) = self.prefix(node, cancel)? else {
                    return Ok(None);
                };
                kind
            } else {
                // Optional capture headers can repeat the native name/parameter fields.
                // The unique-field checks withhold those unproved declaration shapes.
                if self.field(node, "parameters", cancel)?.is_none() {
                    return Ok(None);
                }
                if owner.kind() == "class_definition" {
                    "mojo.method"
                } else {
                    "mojo.function"
                }
            };
            if body.kind() != "block" || body.start_byte() <= name.end_byte() {
                return Ok(None);
            }
            return Ok(Some(Declaration {
                name: name.byte_range(),
                header: node.start_byte()..body.start_byte(),
                syntax,
            }));
        }
        if node.kind() == "assignment" {
            let Some(name) = self.field(node, "left", cancel)? else {
                return Ok(None);
            };
            if !self.name(name, cancel)? || self.prefix(node, cancel)? != Some("mojo.field") {
                return Ok(None);
            }
            let Some(block) = node.parent().filter(|p| p.kind() == "block") else {
                return Ok(None);
            };
            let Some(owner) = block.parent().filter(|p| p.kind() == "class_definition") else {
                return Ok(None);
            };
            if !self.clean(owner)
                || self.field(owner, "body", cancel)? != Some(block)
                || self.declaration(owner, cancel)?.is_none()
            {
                return Ok(None);
            }
            let end = self
                .field(node, "type", cancel)?
                .map_or(name.end_byte(), |ty| ty.end_byte());
            return Ok(Some(Declaration {
                name: name.byte_range(),
                header: node.start_byte()..end,
                syntax: "mojo.field",
            }));
        }
        let Some(parameters) = node.parent().filter(|p| p.kind() == "parameters") else {
            return Ok(None);
        };
        let Some(owner) = parameters
            .parent()
            .filter(|p| p.kind() == "function_definition")
        else {
            return Ok(None);
        };
        if self.field(owner, "parameters", cancel)? != Some(parameters)
            || self.declaration(owner, cancel)?.is_none()
        {
            return Ok(None);
        }
        let Some(name) = self.parameter_name(node, cancel)? else {
            return Ok(None);
        };
        if !self.name(name, cancel)? {
            return Ok(None);
        }
        Ok(Some(Declaration {
            name: name.byte_range(),
            header: node.byte_range(),
            syntax: "mojo.parameter",
        }))
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
    if let Some(proof) = cache.member_candidate(node, role, cancel)? {
        return Ok(Some(proof));
    }
    let mut range = node.byte_range();
    let syntax = if (node.kind() == "module" || node.is_error())
        && node.parent().is_none()
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        if cache.complete {
            range = 0..cache.source.len();
        }
        match role {
            StructuralRole::Root if node.is_error() => "mojo.recovered_file",
            StructuralRole::Root => "mojo.source_file",
            StructuralRole::Module | StructuralRole::Scope => "mojo.file",
            _ => return Ok(None),
        }
    } else if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        if let Some(declaration) = cache.declaration(node, cancel)? {
            match role {
                StructuralRole::Definition => {
                    range = declaration.name;
                    "mojo.name"
                }
                StructuralRole::Signature => {
                    range = declaration.header;
                    "mojo.header"
                }
                _ => declaration.syntax,
            }
        } else if role == StructuralRole::Declaration {
            "mojo.unsupported"
        } else {
            return Ok(None);
        }
    } else if node.kind() == "identifier" && role == StructuralRole::Reference {
        if !cache.name(node, cancel)? {
            return Ok(None);
        }
        if let Some(parent) = node.parent()
            && ((matches!(parent.kind(), "class_definition" | "function_definition")
                && cache.field(parent, "name", cancel)? == Some(node))
                || (parent.kind() == "assignment"
                    && cache.field(parent, "left", cancel)? == Some(node))
                || matches!(
                    parent.kind(),
                    "parameters"
                        | "typed_parameter"
                        | "typed_default_parameter"
                        | "default_parameter"
                ))
        {
            return Ok(None);
        }
        let mut current = node.parent();
        while let Some(parent) = current {
            if !cache.visit(cancel)? || parent.is_error() || parent.is_missing() {
                return Ok(None);
            }
            current = parent.parent();
        }
        "mojo.unresolved_name"
    } else if role == StructuralRole::Expression {
        "mojo.construct_unavailable"
    } else if node.kind() == "comment" && role == StructuralRole::Comment {
        "mojo.comment"
    } else if node.kind() == "string" && role == StructuralRole::StringLiteral {
        "mojo.string"
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
            .ok_or_else(|| query_failure("mojo-native-depth"))?;
        parent = current.parent();
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
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
            .set_language(&crate::registry::language_for(crate::GrammarFamily::Mojo))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn find<'t>(node: Node<'t>, kind: &str, start: usize) -> Node<'t> {
        if node.kind() == kind && node.start_byte() == start {
            return node;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.start_byte() <= start && start < child.end_byte() {
                return find(child, kind, start);
            }
        }
        panic!("missing exact native {kind}@{start}")
    }

    #[test]
    fn native_literal_kind_and_all_four_declaration_roles_share_one_owner() {
        for (keyword, syntax) in [("struct", "mojo.struct"), ("class", "mojo.class")] {
            let source = format!(
                "{keyword} Written:\n    var field: Int\n    @always_inline\n    fn measure(self, value: Int) -> Int:\n        return value\n"
            );
            let parsed = tree(&source);
            assert!(!parsed.root_node().has_error(), "{source}");
            let class = find(parsed.root_node(), "class_definition", 0);
            for role in [
                StructuralRole::Scope,
                StructuralRole::Declaration,
                StructuralRole::Definition,
                StructuralRole::Signature,
            ] {
                let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
                let captured = candidate(
                    class,
                    role,
                    source.as_bytes(),
                    &Cancellation::new(),
                    &mut cache,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    captured.syntax,
                    match role {
                        StructuralRole::Definition => "mojo.name",
                        StructuralRole::Signature => "mojo.header",
                        _ => syntax,
                    }
                );
                if role == StructuralRole::Definition {
                    assert_eq!(&source.as_bytes()[captured.start..captured.end], b"Written");
                }
                assert!(!cache.exhausted());
            }
            let method = find(
                parsed.root_node(),
                "function_definition",
                source.find("fn measure").unwrap(),
            );
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            assert_eq!(
                candidate(
                    method,
                    StructuralRole::Declaration,
                    source.as_bytes(),
                    &Cancellation::new(),
                    &mut cache
                )
                .unwrap()
                .unwrap()
                .syntax,
                "mojo.method"
            );
        }
    }

    #[test]
    fn only_the_actual_function_parameters_can_supply_formal_declarations() {
        let source = "fn measure(value: Int, next: Int = 1) -> Int:\n    return value\n";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let parameter = find(
            parsed.root_node(),
            "typed_parameter",
            source.find("value:").unwrap(),
        );
        let type_name = find(
            parsed.root_node(),
            "identifier",
            source.find("Int").unwrap(),
        );
        let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
        assert_eq!(
            candidate(
                parameter,
                StructuralRole::Declaration,
                source.as_bytes(),
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .unwrap()
            .syntax,
            "mojo.parameter"
        );
        assert!(
            candidate(
                type_name,
                StructuralRole::Definition,
                source.as_bytes(),
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn incomplete_source_work_exhaustion_and_typed_cancellation_withhold_definitions() {
        let source = "struct Written:\n    var field: Int\n";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let class = find(parsed.root_node(), "class_definition", 0);
        let mut partial = CaptureCache::new(source.as_bytes(), 16_384, false);
        assert!(
            candidate(
                class,
                StructuralRole::Definition,
                source.as_bytes(),
                &Cancellation::new(),
                &mut partial
            )
            .unwrap()
            .is_none()
        );
        let mut limited = CaptureCache::new(source.as_bytes(), 0, true);
        assert!(
            candidate(
                class,
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
        let mut live = CaptureCache::new(source.as_bytes(), usize::MAX, true);
        assert_eq!(live.remaining, HARD_MAX_QUERY_CAPTURES * 32);
        assert!(matches!(
            candidate(
                class,
                StructuralRole::Definition,
                source.as_bytes(),
                &cancel,
                &mut live
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
