//! Certify plain GLSL formals and exact-type calls from a complete native census.
//! Only clean flat bodies and ordinary scalar/vector input headers participate;
//! preprocessing, overloads, qualifiers, writes and other scopes remain unknown.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{clean, field, native_depth, text, written_name};
use crate::query_pack::{QueryCandidate, StructuralRole};

#[derive(Clone)]
struct Certificate {
    start: usize,
    owner_depth: usize,
    call: bool,
}

struct Parameter<'tree> {
    declaration: Node<'tree>,
    name: Node<'tree>,
    ty: Node<'tree>,
}

struct Header<'tree> {
    owner: Node<'tree>,
    name: Node<'tree>,
    parameters: Vec<Parameter<'tree>>,
    body: Node<'tree>,
}

pub(super) struct CaptureCache {
    complete: bool,
    initialized: bool,
    maximum: usize,
    used: usize,
    exhausted: bool,
    certificates: HashMap<usize, Certificate>,
}

impl CaptureCache {
    pub(super) fn new(maximum: usize, complete: bool) -> Self {
        Self {
            complete,
            initialized: false,
            maximum,
            used: 0,
            exhausted: false,
            certificates: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn charge(&mut self, amount: usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        if amount > self.maximum.saturating_sub(self.used) {
            self.exhausted = true;
            return Ok(false);
        }
        self.used += amount;
        Ok(true)
    }

    fn nodes<'tree>(
        &mut self,
        start: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        let mut nodes = Vec::new();
        let mut cursor = start.walk();
        loop {
            if !self.charge(1, cancel)? {
                return Ok(Vec::new());
            }
            nodes
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            nodes.push(cursor.node());
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(nodes);
                }
            }
        }
    }

    fn header<'tree>(
        &mut self,
        owner: Node<'tree>,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<Option<Header<'tree>>, AdapterError> {
        if !self.charge(1, cancel)?
            || !clean(owner, source)
            || owner.kind() != "function_definition"
        {
            return Ok(None);
        }
        let Some(declarator) =
            field(owner, "declarator").filter(|n| n.kind() == "function_declarator")
        else {
            return Ok(None);
        };
        let Some(name) = field(declarator, "declarator").filter(|n| ordinary(*n, source)) else {
            return Ok(None);
        };
        let Some(body) = field(owner, "body").filter(|n| n.kind() == "compound_statement") else {
            return Ok(None);
        };
        let Some(ty) = field(owner, "type") else {
            return Ok(None);
        };
        if !plain_type(ty, source, true) {
            return Ok(None);
        }
        let mut children = owner.walk();
        for child in owner.children(&mut children) {
            if !self.charge(1, cancel)? {
                return Ok(None);
            }
            if child != declarator && child != body && child != ty && child.kind() != "comment" {
                return Ok(None);
            }
        }
        let Some(list) = field(declarator, "parameters").filter(|n| n.kind() == "parameter_list")
        else {
            return Ok(None);
        };
        let mut parameters: Vec<Parameter<'tree>> = Vec::new();
        let mut cursor = list.walk();
        for declaration in list.named_children(&mut cursor) {
            if !self.charge(1, cancel)? {
                return Ok(None);
            }
            if declaration.kind() == "comment" {
                continue;
            }
            if declaration.kind() != "parameter_declaration" {
                return Ok(None);
            }
            let Some(name) = field(declaration, "declarator")
                .filter(|n| n.kind() == "identifier" && ordinary(*n, source))
            else {
                return Ok(None);
            };
            let Some(ty) = field(declaration, "type").filter(|n| plain_type(*n, source, false))
            else {
                return Ok(None);
            };
            let mut parts = declaration.walk();
            for part in declaration.children(&mut parts) {
                if !self.charge(1, cancel)? {
                    return Ok(None);
                }
                if part != name && part != ty && part.kind() != "comment" {
                    return Ok(None);
                }
            }
            for previous in &parameters {
                if !self.charge(name.end_byte() - name.start_byte() + 1, cancel)? {
                    return Ok(None);
                }
                if text(previous.name, source) == text(name, source) {
                    return Ok(None);
                }
            }
            parameters
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            parameters.push(Parameter {
                declaration,
                name,
                ty,
            });
        }
        Ok(Some(Header {
            owner,
            name,
            parameters,
            body,
        }))
    }

    fn initialize(
        &mut self,
        mut root: Node<'_>,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        while let Some(parent) = root.parent() {
            if !self.charge(1, cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if !self.complete
            || root.kind() != "translation_unit"
            || !clean(root, source)
            || root.end_byte() != source.len()
            || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
        {
            return Ok(());
        }
        let nodes = self.nodes(root, cancel)?;
        if self.exhausted {
            return Ok(());
        }
        let mut headers = Vec::new();
        let mut names = HashMap::<String, usize>::new();
        let mut version = false;
        let mut substantive = false;
        for node in &nodes {
            if !self.charge(1, cancel)? {
                return Ok(());
            }
            if super::unsupported_c(*node) {
                return Ok(());
            }
            if node.parent() == Some(root) && node.is_named() && node.kind() != "comment" {
                if node.kind() == "preproc_call" {
                    if version || substantive {
                        return Ok(());
                    }
                    version = true;
                } else {
                    substantive = true;
                }
            }
            if node.kind().starts_with("preproc_") {
                let allowed = match node.kind() {
                    "preproc_call" => {
                        node.parent() == Some(root)
                            && field(*node, "directive").and_then(|n| text(n, source))
                                == Some("#version")
                            && field(*node, "argument")
                                .and_then(|n| text(n, source))
                                .is_some_and(|s| {
                                    s.len() <= 16
                                        && matches!(
                                            s.trim(),
                                            "110"
                                                | "120"
                                                | "330"
                                                | "330 core"
                                                | "430"
                                                | "430 core"
                                                | "450"
                                                | "450 core"
                                                | "460"
                                                | "460 core"
                                        )
                                })
                    }
                    "preproc_directive" | "preproc_arg" => {
                        node.parent().is_some_and(|p| p.kind() == "preproc_call")
                    }
                    _ => false,
                };
                if !allowed {
                    return Ok(());
                }
            }
            if node.kind() == "function_definition"
                && node.parent() == Some(root)
                && let Some(header) = self.header(*node, source, cancel)?
            {
                headers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                headers.push(header);
            }
            if !matches!(
                node.kind(),
                "identifier" | "type_identifier" | "field_identifier"
            ) {
                continue;
            }
            let mut ancestor = node.parent();
            let mut in_body = false;
            while let Some(parent) = ancestor {
                if !self.charge(1, cancel)? {
                    return Ok(());
                }
                if parent.kind() == "function_definition" {
                    in_body = field(parent, "body").is_some_and(|body| {
                        body.start_byte() <= node.start_byte() && node.end_byte() <= body.end_byte()
                    });
                    break;
                }
                ancestor = parent.parent();
            }
            if !in_body && let Some(name) = text(*node, source) {
                if !self.charge(name.len(), cancel)? {
                    return Ok(());
                }
                let mut owned = String::new();
                owned
                    .try_reserve_exact(name.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                owned.push_str(name);
                names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                names
                    .entry(owned)
                    .and_modify(|count| *count = count.saturating_add(1))
                    .or_insert(1);
            }
        }
        for header in &headers {
            let body = self.nodes(header.body, cancel)?;
            if self.exhausted {
                self.certificates.clear();
                return Ok(());
            }
            let mut safe = true;
            for node in &body {
                if !self.charge(1, cancel)? {
                    self.certificates.clear();
                    return Ok(());
                }
                if super::unsupported_c(*node)
                    || matches!(
                        node.kind(),
                        "assignment_expression"
                            | "update_expression"
                            | "if_statement"
                            | "for_statement"
                            | "while_statement"
                            | "do_statement"
                            | "switch_statement"
                            | "function_definition"
                            | "struct_specifier"
                            | "ERROR"
                    )
                    || (node.kind() == "compound_statement" && *node != header.body)
                {
                    safe = false;
                    break;
                }
                if node.kind() == "declaration" {
                    let Some(declarator) = field(*node, "declarator") else {
                        safe = false;
                        break;
                    };
                    let declared = if declarator.kind() == "init_declarator" {
                        field(declarator, "declarator")
                    } else {
                        Some(declarator)
                    };
                    let Some(declared) =
                        declared.filter(|n| n.kind() == "identifier" && ordinary(*n, source))
                    else {
                        safe = false;
                        break;
                    };
                    let Some(ty) = field(*node, "type") else {
                        safe = false;
                        break;
                    };
                    if !plain_type(ty, source, false)
                        || node.child_by_field_name("declarator").is_none()
                    {
                        safe = false;
                        break;
                    }
                    // Every declaration's raw identifiers are inspected, including shared declarators.
                    let declaration_nodes = self.nodes(*node, cancel)?;
                    for local in declaration_nodes {
                        if local.kind() == "identifier"
                            && field(local.parent().unwrap_or(local), "declarator") == Some(local)
                        {
                            for parameter in &header.parameters {
                                if !self
                                    .charge(local.end_byte() - local.start_byte() + 1, cancel)?
                                {
                                    self.certificates.clear();
                                    return Ok(());
                                }
                                if text(local, source) == text(parameter.name, source) {
                                    safe = false;
                                }
                            }
                            for function in &headers {
                                if !self
                                    .charge(local.end_byte() - local.start_byte() + 1, cancel)?
                                {
                                    self.certificates.clear();
                                    return Ok(());
                                }
                                if text(local, source) == text(function.name, source) {
                                    safe = false;
                                }
                            }
                        }
                    }
                    let _ = declared;
                }
            }
            if !safe {
                continue;
            }
            for node in &body {
                if node.kind() != "identifier" || !ordinary(*node, source) {
                    continue;
                }
                let Some(parent) = node.parent() else {
                    continue;
                };
                if parent.kind() == "argument_list" {
                    for parameter in &header.parameters {
                        if !self.charge(node.end_byte() - node.start_byte() + 1, cancel)? {
                            self.certificates.clear();
                            return Ok(());
                        }
                        if text(*node, source) == text(parameter.name, source) {
                            self.certificates
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.certificates.insert(
                                node.id(),
                                Certificate {
                                    start: parameter.declaration.start_byte(),
                                    owner_depth: native_depth(header.owner, cancel)?,
                                    call: false,
                                },
                            );
                        }
                    }
                }
                if parent.kind() != "call_expression" || field(parent, "function") != Some(*node) {
                    continue;
                }
                let Some(name) = text(*node, source) else {
                    continue;
                };
                if names.get(name) != Some(&1) {
                    continue;
                }
                let mut target = None;
                for candidate in &headers {
                    if !self.charge(name.len() + 1, cancel)? {
                        self.certificates.clear();
                        return Ok(());
                    }
                    if text(candidate.name, source) == Some(name) {
                        target = Some(candidate);
                    }
                }
                let Some(target) = target else { continue };
                if name == "main" || target.owner.start_byte() > header.owner.start_byte() {
                    continue;
                }
                let mut shadowed = false;
                for parameter in &header.parameters {
                    if !self.charge(name.len() + 1, cancel)? {
                        self.certificates.clear();
                        return Ok(());
                    }
                    shadowed |= text(parameter.name, source) == Some(name);
                }
                if shadowed {
                    continue;
                }
                let Some(arguments) = field(parent, "arguments") else {
                    continue;
                };
                let mut cursor = arguments.walk();
                let mut count = 0;
                let mut exact = true;
                for argument in arguments.named_children(&mut cursor) {
                    if !self.charge(1, cancel)? {
                        self.certificates.clear();
                        return Ok(());
                    }
                    if argument.kind() == "comment" {
                        continue;
                    }
                    let target_parameter = target.parameters.get(count);
                    count += 1;
                    if argument.kind() != "identifier" {
                        exact = false;
                        continue;
                    }
                    let mut argument_parameter = None;
                    for parameter in &header.parameters {
                        if !self.charge(argument.end_byte() - argument.start_byte() + 1, cancel)? {
                            self.certificates.clear();
                            return Ok(());
                        }
                        if text(parameter.name, source) == text(argument, source) {
                            argument_parameter = Some(parameter);
                        }
                    }
                    exact &= matches!((target_parameter, argument_parameter), (Some(target), Some(argument)) if text(target.ty, source) == text(argument.ty, source));
                }
                if exact && count == target.parameters.len() {
                    self.certificates
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.certificates.insert(
                        node.id(),
                        Certificate {
                            start: target.owner.start_byte(),
                            owner_depth: native_depth(header.owner, cancel)?,
                            call: true,
                        },
                    );
                }
            }
        }
        if self.exhausted {
            self.certificates.clear();
        }
        Ok(())
    }

    pub(super) fn candidate(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<Option<QueryCandidate>, AdapterError> {
        self.initialize(node, source, cancel)?;
        if self.exhausted {
            return Ok(None);
        }
        let Some(certificate) = self.certificates.get(&node.id()) else {
            return Ok(None);
        };
        let signature = role == StructuralRole::Signature;
        if !signature && role != StructuralRole::Reference {
            return Ok(None);
        }
        let range: Range<usize> = if signature {
            certificate.start..node.end_byte()
        } else {
            node.byte_range()
        };
        Ok(Some(QueryCandidate {
            start: range.start,
            end: range.end,
            role,
            syntax: match (signature, certificate.call) {
                (true, true) => "glsl.call_target",
                (true, false) => "glsl.formal_target",
                (false, true) => "glsl.direct_file_function",
                (false, false) => "glsl.formal_value",
            },
            required: true,
            native_depth: if signature {
                if certificate.call {
                    1
                } else {
                    certificate.owner_depth + 1
                }
            } else {
                native_depth(node, cancel)?
            },
        }))
    }
}

fn ordinary(node: Node<'_>, source: &[u8]) -> bool {
    written_name(node, source)
        && text(node, source).is_some_and(|name| !name.starts_with("gl_") && !name.contains("__"))
}

fn plain_type(node: Node<'_>, source: &[u8], allow_void: bool) -> bool {
    clean(node, source)
        && matches!(node.kind(), "primitive_type" | "type_identifier")
        && text(node, source).is_some_and(|ty| {
            matches!(
                ty,
                "float"
                    | "int"
                    | "bool"
                    | "vec2"
                    | "vec3"
                    | "vec4"
                    | "ivec2"
                    | "ivec3"
                    | "ivec4"
                    | "bvec2"
                    | "bvec3"
                    | "bvec4"
            ) || allow_void && ty == "void"
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_glsl::LANGUAGE_GLSL.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn actual_original_native_formal_and_exact_call_witnesses_are_required() {
        let source = include_bytes!("../../../tests/fixtures/glsl_written_binding/common.glsl");
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let mut cache = CaptureCache::new(16_384, true);
        for (start, end, syntax, witness) in [
            (2473, 2478, "glsl.formal_value", 2444),
            (2788, 2797, "glsl.direct_file_function", 2428),
        ] {
            let node = tree
                .root_node()
                .descendant_for_byte_range(start, end)
                .unwrap();
            assert_eq!(node.kind(), "identifier");
            for role in [StructuralRole::Reference, StructuralRole::Signature] {
                let candidate = cache
                    .candidate(node, role, source, &Cancellation::new())
                    .unwrap()
                    .unwrap();
                assert!(candidate.required);
                assert_eq!(candidate.end, end);
                if role == StructuralRole::Reference {
                    assert_eq!(candidate.start, start);
                    assert_eq!(candidate.syntax, syntax);
                } else {
                    assert_eq!(candidate.start, witness);
                }
            }
        }
        assert!(!cache.exhausted());
    }

    #[test]
    fn native_overload_macro_shadow_type_and_scope_barriers_refuse_call_certificates() {
        for source in [
            "float target(float x); float target(float x){return x;} float owned(float x){return target(x);}",
            "float target(int x){return x;} float owned(float x){return target(x);}",
            "float target(float x){return x;} float owned(float x){float target=0; return target(x);}",
            "#define target replacement\nfloat target(float x){return x;} float owned(float x){return target(x);}",
            "float target(float x){return x;} float owned(float x){if(true){return target(x);}return x;}",
            "float target(out float x){return x;} float owned(float x){return target(x);}",
            "float target(float x){return x;} float owned(float x){return target(unknown);}",
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "{source}");
            let start = source.rfind("target(").unwrap();
            let node = tree
                .root_node()
                .descendant_for_byte_range(start, start + 6)
                .unwrap();
            let mut cache = CaptureCache::new(16_384, true);
            assert!(
                cache
                    .candidate(
                        node,
                        StructuralRole::Reference,
                        source.as_bytes(),
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn native_partial_work_and_cancel_stops_never_publish_cached_certificates() {
        let source = b"float owned(float value){return external(value);}";
        let tree = parse(source);
        let start = source.windows(5).rposition(|s| s == b"value").unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        for (maximum, complete) in [(1, true), (16_384, false)] {
            let mut cache = CaptureCache::new(maximum, complete);
            assert!(
                cache
                    .candidate(
                        node,
                        StructuralRole::Reference,
                        source,
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none()
            );
            assert_eq!(cache.exhausted(), maximum == 1);
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            CaptureCache::new(16_384, true).candidate(
                node,
                StructuralRole::Reference,
                source,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
    #[test]
    fn anonymous_qualifier_tokens_are_native_header_barriers() {
        for qualifier in ["out", "inout", "in"] {
            let source = format!(
                "float target({qualifier} float value){{return value;}} float caller(float value){{return target(value);}}"
            );
            let tree = parse(source.as_bytes());
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            let owner = tree.root_node().named_child(0).unwrap();
            assert_eq!(owner.kind(), "function_definition");
            let declarator = field(owner, "declarator").unwrap();
            let parameter = field(declarator, "parameters")
                .unwrap()
                .named_child(0)
                .unwrap();
            let mut cursor = parameter.walk();
            assert!(
                parameter
                    .children(&mut cursor)
                    .any(|part| part.kind() == qualifier && !part.is_named()),
                "{source}"
            );
            assert!(
                CaptureCache::new(16_384, true)
                    .header(owner, source.as_bytes(), &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
        let source = b"subroutine float target(float value){return value;}";
        let tree = parse(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let owner = tree.root_node().named_child(0).unwrap();
        let mut cursor = owner.walk();
        assert!(
            owner
                .children(&mut cursor)
                .any(|part| part.kind() == "subroutine" && !part.is_named())
        );
        assert!(
            CaptureCache::new(16_384, true)
                .header(owner, source, &Cancellation::new())
                .unwrap()
                .is_none()
        );
    }
}
