//! Certify plain method-local var operands from complete native JavaScript syntax.
//! A selected initializer never supplies its own callable owner. Other binding
//! forms, nested callables and dynamic scope remain outside this certificate.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::ecmascript;

#[derive(Default)]
struct Binding<'tree> {
    count: usize,
    declaration: Option<Node<'tree>>,
}

pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    remaining: usize,
    complete: bool,
    initialized: bool,
    exhausted: bool,
    sites: HashMap<(usize, usize), ()>,
}

fn name<'source>(
    node: Node<'_>,
    source: &'source [u8],
    cancel: &Cancellation,
) -> Result<Option<&'source str>, AdapterError> {
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    if !raw
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
    {
        return Ok(None);
    }
    for chunk in raw.chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            return Ok(None);
        }
    }
    Ok(std::str::from_utf8(raw).ok())
}

fn body(method: Node<'_>) -> Option<Node<'_>> {
    let owner = method.parent()?.parent()?;
    let class_body = method.parent()?;
    if method.kind() != "method_definition"
        || class_body.kind() != "class_body"
        || owner.kind() != "class_declaration"
        || owner.child_by_field_name("body") != Some(class_body)
    {
        return None;
    }
    let parent = owner.parent()?;
    if parent.kind() != "program"
        && !(parent.kind() == "export_statement"
            && parent.child_by_field_name("declaration") == Some(owner)
            && parent.parent().is_some_and(|root| root.kind() == "program"))
    {
        return None;
    }
    let method_name = method.child_by_field_name("name")?;
    let parameters = method.child_by_field_name("parameters")?;
    let body = method.child_by_field_name("body")?;
    if !matches!(method_name.kind(), "property_identifier" | "identifier")
        || parameters.kind() != "formal_parameters"
        || body.kind() != "statement_block"
    {
        return None;
    }
    let mut cursor = parameters.walk();
    if parameters
        .named_children(&mut cursor)
        .any(|child| !child.is_extra())
    {
        return None;
    }
    let mut cursor = method.walk();
    if method
        .children(&mut cursor)
        .filter(|child| !child.is_extra())
        .any(|child| child != method_name && child != parameters && child != body)
    {
        return None;
    }
    Some(body)
}

fn plain_var<'tree>(node: Node<'tree>, body: Node<'tree>) -> Option<Node<'tree>> {
    if node.kind() != "identifier" {
        return None;
    }
    let declaration = node.parent()?;
    let statement = declaration.parent()?;
    if declaration.kind() != "variable_declarator"
        || declaration.child_by_field_name("name") != Some(node)
        || declaration.child_by_field_name("value").is_none()
        || statement.kind() != "variable_declaration"
        || statement.parent() != Some(body)
        || statement.child(0)?.kind() != "var"
    {
        return None;
    }
    let mut cursor = statement.walk();
    if statement
        .named_children(&mut cursor)
        .filter(|child| !child.is_extra())
        .count()
        != 1
    {
        return None;
    }
    Some(declaration)
}

fn operand(node: Node<'_>, body: Node<'_>) -> bool {
    let Some(binary) = node
        .parent()
        .filter(|parent| parent.kind() == "binary_expression")
    else {
        return false;
    };
    if binary.child_by_field_name("left") != Some(node)
        && binary.child_by_field_name("right") != Some(node)
    {
        return false;
    }
    let Some(context) = binary.parent() else {
        return false;
    };
    if context.kind() == "variable_declarator"
        && context.child_by_field_name("value") == Some(binary)
    {
        return context
            .child_by_field_name("name")
            .and_then(|name| plain_var(name, body))
            .is_some();
    }
    if context.kind() == "return_statement" && context.parent() == Some(body) {
        let mut cursor = context.walk();
        let mut children = context
            .named_children(&mut cursor)
            .filter(|child| !child.is_extra());
        return children.next() == Some(binary) && children.next().is_none();
    }
    false
}

impl<'source, 'tree> CaptureCache<'source, 'tree> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            root,
            remaining: maximum,
            complete,
            initialized: false,
            exhausted: false,
            sites: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        match self.remaining.checked_sub(1) {
            Some(next) => {
                self.remaining = next;
                Ok(true)
            }
            None => {
                self.exhausted = true;
                self.sites.clear();
                Ok(false)
            }
        }
    }

    fn method(&mut self, method: Node<'tree>, cancel: &Cancellation) -> Result<(), AdapterError> {
        let Some(body) = body(method) else {
            return Ok(());
        };
        let Some(method_name) = method.child_by_field_name("name") else {
            return Ok(());
        };
        if name(method_name, self.source, cancel)?.is_none() {
            return Ok(());
        }
        let mut bindings = HashMap::<&str, Binding<'tree>>::new();
        let mut uses = Vec::new();
        let mut cursor = body.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            if node.is_error()
                || node.is_missing()
                || matches!(
                    node.kind(),
                    "with_statement"
                        | "function_declaration"
                        | "function_expression"
                        | "generator_function_declaration"
                        | "generator_function"
                        | "arrow_function"
                        | "method_definition"
                        | "class_declaration"
                        | "class"
                        | "type_annotation"
                        | "function_signature"
                        | "abstract_class_declaration"
                        | "module"
                        | "type_alias_declaration"
                        | "enum_declaration"
                        | "interface_declaration"
                        | "import_alias"
                        | "ambient_declaration"
                        | "internal_module"
                        | "namespace_declaration"
                )
            {
                return Ok(());
            }
            if matches!(
                node.kind(),
                "identifier" | "shorthand_property_identifier_pattern"
            ) {
                let Some(spelling) = name(node, self.source, cancel)? else {
                    return Ok(());
                };
                if spelling == "eval" {
                    return Ok(());
                }
                if ecmascript::binding_kind(node, cancel)?.is_some() {
                    bindings
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    let row = bindings.entry(spelling).or_default();
                    row.count = row
                        .count
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                    row.declaration = plain_var(node, body);
                }
                if operand(node, body) {
                    uses.try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    uses.push((node, spelling));
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
                    for (site, spelling) in uses {
                        cancel.check()?;
                        if bindings.get(spelling).is_some_and(|row| {
                            row.count == 1
                                && row.declaration.is_some_and(|declaration| {
                                    declaration.end_byte() <= site.start_byte()
                                })
                        }) {
                            self.sites
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.sites.insert((site.start_byte(), site.end_byte()), ());
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        let root = self.root;
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
        let mut methods = Vec::new();
        let mut cursor = root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            if node.kind() == "method_definition" && body(node).is_some() {
                methods
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                methods.push(node);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    for method in methods {
                        self.method(method, cancel)?;
                        if self.exhausted {
                            self.sites.clear();
                            return Ok(());
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    pub(super) fn certified(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !self.initialized {
            self.initialize(cancel)?
        }
        Ok(!self.exhausted
            && self
                .sites
                .contains_key(&(node.start_byte(), node.end_byte())))
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;

    #[test]
    fn complete_native_cache_limits_and_partial_authority_never_certify_an_operand() {
        let source = b"class A { run(){ var local=1; return local === other; } }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let start = source.windows(5).rposition(|raw| raw == b"local").unwrap();
        let site = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        assert_eq!(site.kind(), "identifier");
        let cancel = Cancellation::new();
        let mut complete = CaptureCache::new(source, tree.root_node(), 16_384, true);
        assert!(complete.certified(site, &cancel).unwrap());
        let mut partial = CaptureCache::new(source, tree.root_node(), 16_384, false);
        assert!(!partial.certified(site, &cancel).unwrap());
        let mut bounded = CaptureCache::new(source, tree.root_node(), 1, true);
        assert!(!bounded.certified(site, &cancel).unwrap());
        assert!(bounded.exhausted());
        assert!(!bounded.certified(site, &cancel).unwrap());
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(complete.certified(site, &cancel).is_err());
    }
}

#[cfg(test)]
mod tsx_declaration_refusal_tests {
    use super::*;

    #[test]
    fn native_method_var_certificate_refuses_complete_tsx_declaration_family() {
        for (kind, source) in [
            (
                "function_signature",
                "class A { run(){ var local=1; function local(); return local === other; } }",
            ),
            (
                "abstract_class_declaration",
                "class A { run(){ var local=1; abstract class local {} return local === other; } }",
            ),
            (
                "module",
                "class A { run(){ var local=1; module local {} return local === other; } }",
            ),
            (
                "internal_module",
                "class A { run(){ var local=1; namespace local {} return local === other; } }",
            ),
            (
                "type_alias_declaration",
                "class A { run(){ var local=1; type local = number; return local === other; } }",
            ),
            (
                "enum_declaration",
                "class A { run(){ var local=1; enum local { X } return local === other; } }",
            ),
            (
                "interface_declaration",
                "class A { run(){ var local=1; interface local {} return local === other; } }",
            ),
            (
                "import_alias",
                "class A { run(){ var local=1; import local = other; return local === other; } }",
            ),
            (
                "ambient_declaration",
                "class A { run(){ var local=1; declare global {} return local === other; } }",
            ),
            (
                "enum_declaration",
                "class A { run(){ var local=1; const enum local { X } return local === other; } }",
            ),
            (
                "module",
                "class A { run(){ var local=1; module local.nested {} return local === other; } }",
            ),
            (
                "module",
                "class A { run(){ var local=1; module \"local\" {} return local === other; } }",
            ),
            (
                "import_alias",
                "class A { run(){ var local=1; import local = other.member; return local === other; } }",
            ),
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            assert!(
                tree.root_node()
                    .to_sexp()
                    .contains(format!("({kind} ").as_str()),
                "expected {kind}: {source}"
            );
            let start = source.rfind("return local").unwrap() + 7;
            let site = tree
                .root_node()
                .descendant_for_byte_range(start, start + 5)
                .unwrap();
            assert_eq!(site.kind(), "identifier");
            let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true);
            assert!(
                !cache.certified(site, &Cancellation::new()).unwrap(),
                "{kind}: {source}"
            );
            assert!(
                !cache.exhausted(),
                "refusal must not borrow work exhaustion"
            );
        }
    }
}
