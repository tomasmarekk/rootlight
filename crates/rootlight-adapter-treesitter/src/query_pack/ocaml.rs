//! OCaml captures prove physical names and body-independent written headers.
//! Implementation and interface profiles share native shapes, not executable facts.
//! Concrete types use written native bodies; abstract and extension forms stay gaps.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};
use crate::GrammarFamily;

#[derive(Clone)]
struct OwnerProof {
    name: Range<usize>,
    header: Range<usize>,
    kind: &'static str,
}

#[derive(Default)]
struct Entry {
    owner: Option<Option<OwnerProof>>,
    visible: Option<bool>,
}

/// Memoizes native ownership under the caller's existing capture/body work bound.
pub(super) struct CaptureCache {
    entries: HashMap<usize, Entry>,
    maximum_entries: usize,
    remaining_work: usize,
    exhausted: bool,
}

impl CaptureCache {
    /// Creates a request-local cache without source-sized preallocation.
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            maximum_entries,
            remaining_work: maximum_entries,
            exhausted: false,
        }
    }

    /// Reports exhaustion that the caller must expose as a capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining_work.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining_work = remaining;
        Ok(true)
    }

    fn slot(&mut self, node: Node<'_>) -> Result<bool, AdapterError> {
        if self.entries.contains_key(&node.id()) {
            return Ok(true);
        }
        if self.entries.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(false);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.entries.insert(node.id(), Entry::default());
        Ok(true)
    }

    fn visible(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = Some(node);
        let mut pending = Vec::new();
        let mut visible = true;
        while let Some(ancestor) = current {
            cancellation.check()?;
            if !self.slot(ancestor)? {
                return Ok(false);
            }
            if let Some(value) = self
                .entries
                .get(&ancestor.id())
                .and_then(|entry| entry.visible)
            {
                visible = value;
                break;
            }
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(ancestor.id());
            if ancestor.is_missing()
                || matches!(
                    ancestor.kind(),
                    "ERROR"
                        | "extension"
                        | "item_extension"
                        | "attribute"
                        | "item_attribute"
                        | "floating_attribute"
                        | "quoted_string"
                        | "string"
                        | "character"
                        | "comment"
                )
            {
                visible = false;
                break;
            }
            current = ancestor.parent();
        }
        for id in pending {
            cancellation.check()?;
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.visible = Some(visible);
            }
        }
        Ok(visible)
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        language: &str,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if !self.slot(node)? {
            return Ok(None);
        }
        if let Some(value) = self
            .entries
            .get(&node.id())
            .and_then(|entry| entry.owner.as_ref())
        {
            return Ok(value.clone());
        }
        let value = owner_proof(node, language, source, cancellation, self)?;
        if let Some(entry) = self.entries.get_mut(&node.id()) {
            entry.owner = Some(value.clone());
        }
        Ok(value)
    }
}

fn physical(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing()
        && node.kind() != "ERROR"
        && node.start_byte() < node.end_byte()
        && source.get(node.byte_range()).is_some()
}

fn name(node: Node<'_>, language: &str, source: &[u8]) -> bool {
    physical(node, source)
        && !node.has_error()
        && node.named_child_count() == 0
        && node.utf8_text(source).ok().is_some_and(|raw| {
            raw != "_"
                && structural_captured_name_for_language(language, raw, raw.len()).as_deref()
                    == Some(raw)
        })
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut selected = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == kind && selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(selected)
}

fn header(
    node: Node<'_>,
    end: usize,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    let mut range = node.start_byte()..end;
    if range.end > node.end_byte() {
        return Ok(None);
    }
    let Some(bytes) = source.get(range.clone()) else {
        return Ok(None);
    };
    let mut bytes = bytes;
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        let Some(end) = range.end.checked_sub(1) else {
            return Ok(None);
        };
        range.end = end;
        let Some(prefix) = bytes.get(..bytes.len().saturating_sub(1)) else {
            return Ok(None);
        };
        bytes = prefix;
    }
    Ok((range.start < range.end).then_some(range))
}

fn owner_proof(
    node: Node<'_>,
    language: &str,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<OwnerProof>, AdapterError> {
    if !physical(node, source) {
        return Ok(None);
    }
    let (selected, end, kind) = match node.kind() {
        "type_binding" => {
            let (Some(selected), Some(body)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("body"),
            ) else {
                return Ok(None);
            };
            if selected.kind() != "type_constructor"
                || node.child_by_field_name("synonym").is_some()
                || !physical(body, source)
                || body.has_error()
                || !body.is_named()
                || body.kind() == "external_declaration"
            {
                return Ok(None);
            }
            let mut equals = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                if child.end_byte() <= body.start_byte() {
                    if child.has_error()
                        || child.is_missing()
                        || matches!(child.kind(), ":=" | "+=")
                    {
                        return Ok(None);
                    }
                    equals |= child.kind() == "=";
                }
            }
            if !equals {
                return Ok(None);
            }
            let kind = match body.kind() {
                "record_declaration" => "struct",
                "variant_declaration" => "enum",
                _ => "type_alias",
            };
            (selected, node.end_byte(), kind)
        }
        "let_binding" => {
            let (Some(pattern), Some(body)) = (
                node.child_by_field_name("pattern"),
                node.child_by_field_name("body"),
            ) else {
                return Ok(None);
            };
            if pattern.kind() != "value_name" || !physical(body, source) {
                return Ok(None);
            }
            let mut function = matches!(body.kind(), "fun_expression" | "function_expression");
            let mut equals = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                if child.end_byte() <= body.start_byte() {
                    if child.has_error() || child.is_missing() {
                        return Ok(None);
                    }
                    function |= child.kind() == "parameter";
                    equals |= child.kind() == "=";
                }
            }
            if !equals {
                return Ok(None);
            }
            (
                pattern,
                body.start_byte(),
                if function { "function" } else { "variable" },
            )
        }
        "value_specification" => {
            let Some(selected) = direct(node, "value_name", cancellation, cache)? else {
                return Ok(None);
            };
            let Some(written_type) = node.child_by_field_name("type") else {
                return Ok(None);
            };
            if written_type.has_error() || !physical(written_type, source) {
                return Ok(None);
            }
            let kind = if written_type.kind() == "function_type"
                || written_type.kind() == "polymorphic_type"
                    && written_type
                        .child_by_field_name("type")
                        .is_some_and(|inner| inner.kind() == "function_type")
            {
                "function"
            } else {
                "variable"
            };
            (selected, node.end_byte(), kind)
        }
        "module_binding" => {
            let Some(selected) = direct(node, "module_name", cancellation, cache)? else {
                return Ok(None);
            };
            let body = node.child_by_field_name("body");
            if body.is_none() && node.child_by_field_name("module_type").is_none() {
                return Ok(None);
            }
            (
                selected,
                body.map_or(node.end_byte(), |body| body.start_byte()),
                "module",
            )
        }
        "module_type_definition" => {
            let Some(selected) = direct(node, "module_type_name", cancellation, cache)? else {
                return Ok(None);
            };
            let end = node
                .child_by_field_name("body")
                .map_or(node.end_byte(), |body| body.start_byte());
            (selected, end, "module_type")
        }
        "parameter" => {
            let Some(selected) = node.child_by_field_name("pattern") else {
                return Ok(None);
            };
            let Some(parent) = node
                .parent()
                .filter(|parent| parent.kind() == "let_binding")
            else {
                return Ok(None);
            };
            if selected.kind() != "value_pattern"
                || !cache
                    .owner(parent, language, source, cancellation)?
                    .is_some_and(|owner| owner.kind == "function")
            {
                return Ok(None);
            }
            let end = node
                .child_by_field_name("default")
                .map_or(node.end_byte(), |default| default.start_byte());
            (selected, end, "parameter")
        }
        _ => return Ok(None),
    };
    if !name(selected, language, source) {
        return Ok(None);
    }
    Ok(
        header(node, end, source, cancellation, cache)?.map(|header| OwnerProof {
            name: selected.byte_range(),
            header,
            kind,
        }),
    )
}

fn syntax(interface: bool, kind: &str) -> Result<&'static str, AdapterError> {
    Ok(match (interface, kind) {
        (false, "file") => "ocaml.file",
        (true, "file") => "ocaml_interface.file",
        (false, "function") => "ocaml.function",
        (true, "function") => "ocaml_interface.function",
        (false, "variable") => "ocaml.variable",
        (true, "variable") => "ocaml_interface.variable",
        (false, "struct") => "ocaml.struct",
        (true, "struct") => "ocaml_interface.struct",
        (false, "enum") => "ocaml.enum",
        (true, "enum") => "ocaml_interface.enum",
        (false, "type_alias") => "ocaml.type_alias",
        (true, "type_alias") => "ocaml_interface.type_alias",
        (false, "parameter") => "ocaml.parameter",
        (false, "module") => "ocaml.module",
        (true, "module") => "ocaml_interface.module",
        (false, "module_type") => "ocaml.module_type",
        (true, "module_type") => "ocaml_interface.module_type",
        (false, "definition") => "ocaml.name",
        (true, "definition") => "ocaml_interface.name",
        (false, "unsupported_type") => "ocaml.unsupported_type",
        (true, "unsupported_type") => "ocaml_interface.unsupported_type",
        (false, "unsupported_type_context") => "ocaml.unsupported_type_context",
        (true, "unsupported_type_context") => "ocaml_interface.unsupported_type_context",
        (false, "unsupported_type_members") => "ocaml.unsupported_type_members",
        (true, "unsupported_type_members") => "ocaml_interface.unsupported_type_members",
        (false, "source_semantics") => "ocaml.source_availability",
        (true, "source_semantics") => "ocaml_interface.source_availability",
        (false, "unsupported_construct") => "ocaml.unsupported_construct",
        (true, "unsupported_construct") => "ocaml_interface.unsupported_construct",
        (false, "binding_scope") => "ocaml.binding_scope",
        (true, "binding_scope") => "ocaml_interface.binding_scope",
        (false, "call") => "ocaml.application",
        (false, "reference") => "ocaml.value",
        (false, "comment") => "ocaml.comment",
        (true, "comment") => "ocaml_interface.comment",
        (false, "string") => "ocaml.source_text",
        (true, "string") => "ocaml_interface.source_text",
        _ => return Err(query_failure("query-ocaml-syntax-contract")),
    })
}

/// Selects exact written captures without resolving types, modules or extensions.
///
/// The caller must surface cache exhaustion; returned identity captures retain the
/// required closure flag. Errors propagate cancellation, allocation and depth limits.
pub(super) fn candidate(
    family: GrammarFamily,
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let interface = match family {
        GrammarFamily::Ocaml => false,
        GrammarFamily::OcamlInterface => true,
        _ => return Ok(None),
    };
    let language = if interface {
        "ocaml-interface"
    } else {
        "ocaml"
    };
    let mut range = node.byte_range();
    let kind = if node.kind() == "compilation_unit" {
        range = 0..source.len();
        match role {
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => "file",
            StructuralRole::Expression => "source_semantics",
            _ => return Ok(None),
        }
    } else if matches!(
        role,
        StructuralRole::Comment | StructuralRole::Documentation
    ) {
        if node.kind() != "comment" || !physical(node, source) {
            return Ok(None);
        }
        "comment"
    } else if role == StructuralRole::StringLiteral {
        if !matches!(node.kind(), "string" | "quoted_string" | "character")
            || !physical(node, source)
        {
            return Ok(None);
        }
        "string"
    } else if role == StructuralRole::Expression {
        if !physical(node, source) {
            return Ok(None);
        }
        if node.kind() == "type_binding" {
            "unsupported_type_context"
        } else if matches!(node.kind(), "record_declaration" | "variant_declaration") {
            let Some(owner) = node.parent().filter(|owner| {
                owner.kind() == "type_binding"
                    && owner
                        .child_by_field_name("body")
                        .is_some_and(|body| body.id() == node.id())
            }) else {
                return Ok(None);
            };
            let expected = if node.kind() == "record_declaration" {
                "struct"
            } else {
                "enum"
            };
            if cache
                .owner(owner, language, source, cancellation)?
                .is_none_or(|proof| proof.kind != expected)
            {
                return Ok(None);
            }
            "unsupported_type_members"
        } else if node.kind() == "let_binding"
            && cache.owner(node, language, source, cancellation)?.is_some()
        {
            return Ok(None);
        } else {
            "unsupported_construct"
        }
    } else {
        if !physical(node, source) || !cache.visible(node, cancellation)? {
            return Ok(None);
        }
        match role {
            StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature => {
                if node.kind() == "type_binding"
                    && role == StructuralRole::Signature
                    && cache.owner(node, language, source, cancellation)?.is_none()
                {
                    "unsupported_type"
                } else {
                    let Some(owner) = cache.owner(node, language, source, cancellation)? else {
                        return Ok(None);
                    };
                    match role {
                        StructuralRole::Definition => {
                            range = owner.name;
                            "definition"
                        }
                        StructuralRole::Signature => {
                            range = owner.header;
                            owner.kind
                        }
                        _ => owner.kind,
                    }
                }
            }
            StructuralRole::Scope => {
                if matches!(
                    node.kind(),
                    "module_binding" | "module_type_definition" | "let_binding" | "type_binding"
                ) {
                    let Some(owner) = cache.owner(node, language, source, cancellation)? else {
                        return Ok(None);
                    };
                    if matches!(owner.kind, "variable" | "type_alias") {
                        return Ok(None);
                    }
                    owner.kind
                } else {
                    "binding_scope"
                }
            }
            StructuralRole::Call if !interface => {
                if node.kind() != "application_expression" || node.has_error() {
                    return Ok(None);
                }
                let Some(function) = node.child_by_field_name("function") else {
                    return Ok(None);
                };
                if function.kind() != "value_path" || function.has_error() {
                    return Ok(None);
                }
                range = function.byte_range();
                "call"
            }
            StructuralRole::Reference if !interface => {
                if node.kind() != "value_path"
                    || node.has_error()
                    || node.parent().is_some_and(|parent| {
                        parent.kind() == "application_expression"
                            && parent.child_by_field_name("function") == Some(node)
                    })
                {
                    return Ok(None);
                }
                "reference"
            }
            _ => return Ok(None),
        }
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax: syntax(interface, kind)?,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use tree_sitter::{Parser, Query};

    use super::*;
    use crate::query_pack::QueryPackRegistry;

    fn parse(family: GrammarFamily, source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn captures(family: GrammarFamily, source: &str) -> Vec<QueryCandidate> {
        let tree = parse(family, source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let extraction = QueryPackRegistry::audited()
            .unwrap()
            .get(family)
            .unwrap()
            .extract(
                family,
                &tree,
                source.as_bytes(),
                4096,
                4096,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(extraction.limit.is_none());
        extraction.candidates
    }

    fn texts<'a>(source: &'a str, facts: &[QueryCandidate], role: StructuralRole) -> Vec<&'a str> {
        let mut values = facts
            .iter()
            .filter(|fact| fact.role == role)
            .map(|fact| source.get(fact.start..fact.end).unwrap())
            .collect::<Vec<_>>();
        values.sort_unstable();
        values
    }

    #[test]
    fn ocaml_profiles_compile_against_distinct_factories() {
        for (family, source, implementation) in [
            (
                GrammarFamily::Ocaml,
                include_str!("../../queries/ocaml.scm"),
                true,
            ),
            (
                GrammarFamily::OcamlInterface,
                include_str!("../../queries/ocaml-interface.scm"),
                false,
            ),
        ] {
            let query = Query::new(&crate::registry::language_for(family), source).unwrap();
            let mut actual = query.capture_names().to_vec();
            actual.sort_unstable();
            let mut expected = vec![
                "comment",
                "declaration",
                "definition",
                "documentation",
                "expression",
                "module",
                "root",
                "scope",
                "signature",
                "string",
            ];
            if implementation {
                expected.extend(["call", "reference"]);
            }
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn ocaml_written_names_headers_and_applications_remain_physical() {
        let source =
            "module Worker = struct\nlet increment x = x + 1\nlet result = increment 2\nend\n";
        let facts = captures(GrammarFamily::Ocaml, source);
        assert_eq!(
            texts(source, &facts, StructuralRole::Definition),
            ["Worker", "increment", "result", "x"]
        );
        assert_eq!(texts(source, &facts, StructuralRole::Call), ["increment"]);
        assert_eq!(texts(source, &facts, StructuralRole::Reference), ["x"]);
        let headers = texts(source, &facts, StructuralRole::Signature);
        for header in ["Worker =", "increment x =", "result =", "x"] {
            assert!(headers.contains(&header), "{header}: {headers:?}");
        }
        for fact in &facts {
            // The shared identity scan makes scopes mandatory after ownership
            // closure, not merely because the native query captured a scope.
            assert_eq!(
                fact.required,
                fact.role != StructuralRole::Scope && fact.role.belongs_to_identity_closure()
            );
            assert!(source.get(fact.start..fact.end).is_some());
        }
        for role in [
            StructuralRole::Root,
            StructuralRole::Module,
            StructuralRole::Scope,
        ] {
            assert!(facts.iter().any(|fact| fact.role == role
                && fact.syntax == "ocaml.file"
                && fact.start == 0
                && fact.end == source.len()));
        }
    }

    #[test]
    fn ocaml_interface_abstract_types_and_extension_payloads_cannot_fabricate_owners() {
        let interface =
            "type item\nmodule type Reader = sig val read : item -> int end\nval count : int\n";
        let facts = captures(GrammarFamily::OcamlInterface, interface);
        assert_eq!(
            texts(interface, &facts, StructuralRole::Definition),
            ["Reader", "count", "read"]
        );
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact.role, StructuralRole::Call | StructuralRole::Reference))
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.syntax == "ocaml_interface.unsupported_type"
                    && fact.role == StructuralRole::Signature
                    && &interface[fact.start..fact.end] == "item")
        );
        let implementation = "let visible x = x\nlet text = {aéz|let string_name x = x|aéz}\n(* let comment_name x = x *)\n[%%opaque let payload_name x = x]\n";
        let facts = captures(GrammarFamily::Ocaml, implementation);
        assert_eq!(
            texts(implementation, &facts, StructuralRole::Definition),
            ["text", "visible", "x"]
        );
        assert!(
            texts(implementation, &facts, StructuralRole::StringLiteral)
                .contains(&"{aéz|let string_name x = x|aéz}")
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.role == StructuralRole::Expression
                    && fact.syntax == "ocaml.unsupported_construct"
                    && &implementation[fact.start..fact.end]
                        == "[%%opaque let payload_name x = x]")
        );
    }

    #[test]
    fn ocaml_concrete_types_use_native_bodies_not_abstract_aliases() {
        let source = "type point = { x: int; y: int }\ntype direction = North | South of int\ntype count = int\ntype abstract\ntype replacement := int\ntype extensible = ..\ntype foreign = external \"int\"\n";
        for family in [GrammarFamily::Ocaml, GrammarFamily::OcamlInterface] {
            let facts = captures(family, source);
            assert_eq!(
                texts(source, &facts, StructuralRole::Definition),
                ["count", "direction", "point"]
            );
            let prefix = if family == GrammarFamily::Ocaml {
                "ocaml"
            } else {
                "ocaml_interface"
            };
            for (name, kind) in [
                ("point", "struct"),
                ("direction", "enum"),
                ("count", "type_alias"),
            ] {
                assert!(
                    facts
                        .iter()
                        .any(|fact| fact.role == StructuralRole::Declaration
                            && fact.syntax == format!("{prefix}.{kind}")
                            && source.get(fact.start..fact.end).unwrap().starts_with(name))
                );
            }
            for name in ["abstract", "replacement", "extensible", "foreign"] {
                assert!(
                    facts
                        .iter()
                        .any(|fact| fact.role == StructuralRole::Signature
                            && fact.syntax == format!("{prefix}.unsupported_type")
                            && source.get(fact.start..fact.end).unwrap().starts_with(name))
                );
            }
            let members = facts
                .iter()
                .filter(|fact| {
                    fact.role == StructuralRole::Expression
                        && fact.syntax == format!("{prefix}.unsupported_type_members")
                })
                .collect::<Vec<_>>();
            assert_eq!(members.len(), 2);
            for body in ["{ x: int; y: int }", "North | South of int"] {
                assert!(
                    members
                        .iter()
                        .any(|fact| source.get(fact.start..fact.end) == Some(body))
                );
            }
        }
    }

    #[test]
    fn ocaml_owner_proof_preserves_headers_cancellation_and_work_bounds() {
        let before = "let increment x = x + 1\n";
        let after = "let increment x = x + 100\n";
        let first = captures(GrammarFamily::Ocaml, before);
        let second = captures(GrammarFamily::Ocaml, after);
        assert_eq!(
            texts(before, &first, StructuralRole::Definition),
            texts(after, &second, StructuralRole::Definition)
        );
        assert_eq!(
            texts(before, &first, StructuralRole::Signature),
            texts(after, &second, StructuralRole::Signature)
        );
        let tree = parse(GrammarFamily::Ocaml, before);
        let node = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .named_child(0)
            .unwrap();
        assert_eq!(node.kind(), "let_binding");
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(0);
        assert!(
            candidate(
                GrammarFamily::Ocaml,
                node,
                StructuralRole::Declaration,
                before.as_bytes(),
                &cancellation,
                &mut bounded
            )
            .unwrap()
            .is_none()
        );
        assert!(bounded.exhausted());
        let mut cache = CaptureCache::new(1024);
        for _ in 0..8 {
            assert!(
                cache
                    .owner(node, "ocaml", before.as_bytes(), &cancellation)
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(cache.entries.len(), 1);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            candidate(
                GrammarFamily::Ocaml,
                node,
                StructuralRole::Declaration,
                before.as_bytes(),
                &cancellation,
                &mut cache
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
