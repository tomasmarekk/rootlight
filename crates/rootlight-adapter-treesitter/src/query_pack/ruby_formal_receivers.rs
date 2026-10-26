//! Certify plain method formals used as ordinary Ruby call receivers.
//! Complete method/source geometry is lexical evidence only. Calls and runtime
//! values remain outside this proof, including a method reached through a formal.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(crate) const MAX_SOURCE: usize = 64 * 1024;
const MAX_LINKS: usize = 48;
type Span = (usize, usize);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Link {
    pub(crate) kind: SyntaxFactKind,
    pub(crate) label: &'static str,
    pub(crate) span: Span,
    pub(crate) name: Option<Span>,
}

type LinkChain = ([Option<Link>; MAX_LINKS], usize);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Proof {
    pub(crate) method: Span,
    pub(crate) method_name: Span,
    pub(crate) header: Span,
    pub(crate) parameter: Span,
    pub(crate) links: [Option<Link>; MAX_LINKS],
    pub(crate) link_count: usize,
}

pub(crate) struct Work(pub(crate) usize);

impl Work {
    pub(crate) fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.0.checked_sub(1) else {
            return Ok(false);
        };
        self.0 = next;
        Ok(true)
    }
}

fn span(node: Node<'_>) -> Span {
    (node.start_byte(), node.end_byte())
}

pub(crate) fn ordinary(raw: &[u8]) -> bool {
    !raw.is_empty()
        && raw.len() <= 128
        && raw
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || *b == b'_')
        && raw.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && !matches!(
            raw,
            b"it" | b"_1" | b"_2" | b"_3" | b"_4" | b"_5" | b"_6" | b"_7" | b"_8" | b"_9"
        )
}

fn receiver(node: Node<'_>) -> bool {
    node.kind() == "identifier"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "call" && parent.child_by_field_name("receiver") == Some(node)
        })
}

fn method_ancestor<'tree>(
    mut node: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    while let Some(parent) = node.parent() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if matches!(parent.kind(), "method" | "singleton_method") {
            return Ok(Some(parent));
        }
        if matches!(
            parent.kind(),
            "module" | "class" | "singleton_class" | "program"
        ) {
            return Ok(None);
        }
        node = parent;
    }
    Ok(None)
}

fn names_formal(
    method: Node<'_>,
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(written) = source.get(node.byte_range()).filter(|raw| ordinary(raw)) else {
        return Ok(false);
    };
    let Some(parameters) = method.child_by_field_name("parameters") else {
        return Ok(false);
    };
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let name = if parameter.kind() == "identifier" {
            Some(parameter)
        } else {
            parameter.child_by_field_name("name")
        };
        if name
            .filter(|name| name.kind() == "identifier")
            .and_then(|name| source.get(name.byte_range()))
            .is_some_and(|raw| ordinary(raw) && raw == written)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn links(
    mut node: Node<'_>,
    method: Node<'_>,
    source_len: usize,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<LinkChain>, AdapterError> {
    let mut result = [None; MAX_LINKS];
    let mut count = 0;
    let mut append = |kind, label, span, name| {
        let Some(slot) = result.get_mut(count) else {
            return false;
        };
        *slot = Some(Link {
            kind,
            label,
            span,
            name,
        });
        count += 1;
        true
    };
    let mut seen_method = false;
    while let Some(parent) = node.parent() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let accepted = match parent.kind() {
            "body_statement" => append(
                SyntaxFactKind::Scope,
                "ruby.block.scope",
                span(parent),
                None,
            ),
            "assignment" if !seen_method => append(
                SyntaxFactKind::Declaration,
                "ruby.variable.declaration",
                span(parent),
                None,
            ),
            "method" if parent == method && !seen_method => {
                seen_method = true;
                append(
                    SyntaxFactKind::Declaration,
                    "ruby.method.declaration",
                    span(parent),
                    None,
                ) && append(
                    SyntaxFactKind::Scope,
                    "ruby.method.scope",
                    span(parent),
                    None,
                )
            }
            "module" | "class" if seen_method => {
                let Some(name) = parent.child_by_field_name("name") else {
                    return Ok(None);
                };
                if name.kind() != "constant" {
                    return Ok(None);
                }
                let (declaration, scope) = if parent.kind() == "module" {
                    ("ruby.namespace.declaration", "ruby.namespace.scope")
                } else {
                    ("ruby.class.declaration", "ruby.class.scope")
                };
                append(
                    SyntaxFactKind::Declaration,
                    declaration,
                    span(parent),
                    Some(span(name)),
                ) && append(SyntaxFactKind::Scope, scope, span(parent), None)
            }
            "program" if seen_method => {
                if !append(
                    SyntaxFactKind::Scope,
                    "ruby.file.scope",
                    (0, source_len),
                    None,
                ) || !append(
                    SyntaxFactKind::Module,
                    "ruby.file.module",
                    (0, source_len),
                    None,
                ) || !append(
                    SyntaxFactKind::Root,
                    "ruby.file.root",
                    (0, source_len),
                    None,
                ) {
                    return Ok(None);
                }
                return Ok(Some((result, count)));
            }
            "call"
            | "argument_list"
            | "unless_modifier"
            | "if_modifier"
            | "return"
            | "binary"
            | "unary"
            | "parenthesized_statements"
                if !seen_method =>
            {
                true
            }
            _ => false,
        };
        if !accepted {
            return Ok(None);
        }
        node = parent;
    }
    Ok(None)
}

fn inspect<'source>(
    method: Node<'_>,
    source: &'source [u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<HashMap<&'source [u8], Span>>, AdapterError> {
    if method.kind() != "method" || method.has_error() || method.is_missing() {
        return Ok(None);
    }
    let Some(parameters) = method.child_by_field_name("parameters") else {
        return Ok(None);
    };
    let mut names = HashMap::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let Some(raw) = source.get(parameter.byte_range()) else {
            return Ok(None);
        };
        if parameter.kind() != "identifier" || !ordinary(raw) {
            return Ok(None);
        }
        names
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        if names.insert(raw, span(parameter)).is_some() {
            return Ok(None);
        }
    }
    let Some(body) = method.child_by_field_name("body") else {
        return Ok(None);
    };
    let mut cursor = body.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let node = cursor.node();
        if node.is_error()
            || node.is_missing()
            || (node.is_named()
                && !matches!(
                    node.kind(),
                    "body_statement"
                        | "assignment"
                        | "identifier"
                        | "constant"
                        | "call"
                        | "argument_list"
                        | "return"
                        | "unless_modifier"
                        | "if_modifier"
                        | "binary"
                        | "unary"
                        | "parenthesized_statements"
                        | "element_reference"
                        | "integer"
                        | "float"
                        | "true"
                        | "false"
                        | "nil"
                        | "self"
                        | "string"
                        | "string_content"
                        | "escape_sequence"
                        | "regex"
                        | "comment"
                ))
        {
            return Ok(None);
        }
        if node.kind() == "assignment" {
            let Some(left) = node.child_by_field_name("left") else {
                return Ok(None);
            };
            let Some(raw) = source.get(left.byte_range()) else {
                return Ok(None);
            };
            if left.kind() != "identifier" || !ordinary(raw) || names.contains_key(raw) {
                return Ok(None);
            }
        }
        if node.kind() == "call" {
            let Some(head) = node.child_by_field_name("method") else {
                return Ok(None);
            };
            let Some(raw) = source.get(head.byte_range()) else {
                return Ok(None);
            };
            if head.kind() != "identifier"
                || matches!(
                    raw,
                    b"eval"
                        | b"binding"
                        | b"local_variable_set"
                        | b"instance_eval"
                        | b"class_eval"
                        | b"module_eval"
                        | b"send"
                        | b"__send__"
                        | b"public_send"
                        | b"method"
                        | b"define_method"
                        | b"define_singleton_method"
                )
            {
                return Ok(None);
            }
        }
        if node.kind() == "regex" {
            let Some(raw) = source.get(node.byte_range()) else {
                return Ok(None);
            };
            for _ in raw.chunks(64) {
                if !work.step(cancel)? {
                    return Ok(None);
                }
            }
            if raw.windows(3).any(|part| part == b"(?<" || part == b"(?'") {
                return Ok(None);
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
                return Ok(Some(names));
            }
        }
    }
}

/// Rebuild exact receiver geometry from a clean complete native source tree.
/// No label or partial root supplied by another producer grants proof authority.
pub(crate) fn prove(
    root: Node<'_>,
    source: &[u8],
    complete: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<HashMap<Span, Proof>, AdapterError> {
    cancel.check()?;
    let mut output = HashMap::new();
    if !complete
        || source.len() > MAX_SOURCE
        || root.kind() != "program"
        || root.parent().is_some()
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(output);
    }
    let mut methods = HashMap::new();
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        let node = cursor.node();
        if receiver(node) {
            let Some(method) = method_ancestor(node, work, cancel)? else {
                if work.0 == 0 {
                    return Ok(HashMap::new());
                }
                if cursor.goto_first_child() {
                    continue;
                }
                loop {
                    if cursor.goto_next_sibling() {
                        break;
                    }
                    if !cursor.goto_parent() {
                        return Ok(output);
                    }
                }
                continue;
            };
            if !methods.contains_key(&method.id()) {
                let inspected = inspect(method, source, work, cancel)?;
                methods
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                methods.insert(method.id(), inspected);
            }
            let raw = source.get(node.byte_range()).unwrap_or_default();
            if let Some(parameter) = methods
                .get(&method.id())
                .and_then(Option::as_ref)
                .and_then(|names| names.get(raw))
                .copied()
                && node
                    .parent()
                    .and_then(|call| call.child_by_field_name("operator"))
                    .and_then(|operator| source.get(operator.byte_range()))
                    == Some(b".")
                && let Some((path, count)) = links(node, method, source.len(), work, cancel)?
                && let Some(method_name) = method.child_by_field_name("name")
                && let Some(parameters) = method.child_by_field_name("parameters")
            {
                output
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                output.insert(
                    span(node),
                    Proof {
                        method: span(method),
                        method_name: span(method_name),
                        header: (method.start_byte(), parameters.end_byte()),
                        parameter,
                        links: path,
                        link_count: count,
                    },
                );
            }
        }
        if work.0 == 0 {
            return Ok(HashMap::new());
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(output);
            }
        }
    }
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    work: Work,
    checked: bool,
    proofs: HashMap<Span, Proof>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            work: Work(maximum),
            checked: false,
            proofs: HashMap::new(),
        }
    }

    /// Refuse exhausted work before retaining either a positive or generic fact.
    /// Only an independently rechecked label can later gain a formal target.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !matches!(
            candidate.role,
            StructuralRole::Reference | StructuralRole::Signature
        ) {
            return Ok(true);
        }
        if !self.checked {
            self.checked = true;
            let mut root = node;
            while let Some(parent) = root.parent() {
                if !self.work.step(cancel)? {
                    return Ok(false);
                }
                root = parent;
            }
            self.proofs = prove(root, self.source, self.complete, &mut self.work, cancel)?;
        }
        if self.work.0 == 0 {
            return Ok(false);
        }
        if candidate.role == StructuralRole::Signature && node.kind() == "method" {
            for proof in self.proofs.values() {
                if !self.work.step(cancel)? {
                    return Ok(false);
                }
                if proof.method == span(node) {
                    candidate.required = true;
                    break;
                }
            }
        }
        if candidate.role == StructuralRole::Reference
            && receiver(node)
            && let Some(method) = method_ancestor(node, &mut self.work, cancel)?
            && names_formal(method, node, self.source, &mut self.work, cancel)?
        {
            let proved = self.proofs.contains_key(&span(node));
            candidate.required = proved;
            candidate.syntax = if proved {
                "ruby.formal_receiver"
            } else {
                "ruby.unproven_formal_receiver"
            };
        }
        Ok(self.work.0 != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn full_original_census_retains_formal_and_exact_initializer_method_path() {
        let source = include_bytes!("../../tests/fixtures/ruby_formal_receivers/html_helper.rb");
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let proofs = prove(
            tree.root_node(),
            source,
            true,
            &mut Work(50_000),
            &Cancellation::new(),
        )
        .unwrap();
        let selected = proofs.get(&(1419, 1420)).unwrap();
        assert_eq!(selected.method, (1368, 1497));
        assert_eq!(selected.method_name, (1372, 1388));
        assert_eq!(selected.header, (1368, 1391));
        assert_eq!(selected.parameter, (1389, 1390));
        assert_eq!(
            selected.links[0].unwrap().label,
            "ruby.variable.declaration"
        );
        assert_eq!(selected.links[0].unwrap().span, (1411, 1455));
        assert_eq!(
            selected.links[selected.link_count - 1].unwrap().label,
            "ruby.file.root"
        );
    }

    #[test]
    fn partial_recovery_work_and_cancellation_do_not_supply_formal_authority() {
        let source = b"def owned(value)\nvalue.message\nend\n";
        let tree = parse(source);
        assert!(
            prove(
                tree.root_node(),
                source,
                false,
                &mut Work(50_000),
                &Cancellation::new()
            )
            .unwrap()
            .is_empty()
        );
        for maximum in [0, 1, 8] {
            assert!(
                prove(
                    tree.root_node(),
                    source,
                    true,
                    &mut Work(maximum),
                    &Cancellation::new()
                )
                .unwrap()
                .is_empty()
            );
        }
        let malformed = b"def owned(value\nvalue.message\nend\n";
        let tree = parse(malformed);
        assert!(tree.root_node().has_error());
        assert!(
            prove(
                tree.root_node(),
                malformed,
                true,
                &mut Work(50_000),
                &Cancellation::new()
            )
            .unwrap()
            .is_empty()
        );
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            prove(tree.root_node(), malformed, false, &mut Work(0), &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn clean_rebinding_pattern_block_default_and_dynamic_methods_refuse_native_labels() {
        for source in [
            "def owned(value)\nvalue = other\nvalue.message\nend\n",
            "def owned(value)\nvalue += other\nvalue.message\nend\n",
            "def owned(value)\n[1].each { |value| value.message }\nend\n",
            "def owned(value)\ncase input\nin value\nend\nvalue.message\nend\n",
            "def owned(value = other)\nvalue.message\nend\n",
            "def owned(value)\neval('value = other')\nvalue.message\nend\n",
            "def owned(value)\n/(?<value>a)/ =~ text\nvalue.message\nend\n",
            "def self.owned(value)\nvalue.message\nend\n",
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "{source}");
            assert!(
                prove(
                    tree.root_node(),
                    source.as_bytes(),
                    true,
                    &mut Work(50_000),
                    &Cancellation::new()
                )
                .unwrap()
                .is_empty(),
                "{source}"
            );
        }
        let source = b"def owned(value)\nresult = value.message\nend\n";
        let tree = parse(source);
        assert_eq!(
            prove(
                tree.root_node(),
                source,
                true,
                &mut Work(50_000),
                &Cancellation::new()
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn exhausted_classification_refuses_before_generic_reference_retention() {
        let source = b"def owned(value)\nvalue.message\nend\n";
        let tree = parse(source);
        let at = source.windows(6).rposition(|raw| raw == b"value.").unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(at, at + 5)
            .unwrap();
        let mut candidate = QueryCandidate {
            start: at,
            end: at + 5,
            role: StructuralRole::Reference,
            syntax: "ruby.identifier",
            native_depth: 0,
            required: false,
        };
        let mut cache = CaptureCache::new(source, 0, true);
        cache.checked = true;
        assert!(
            !cache
                .refine(node, &mut candidate, &Cancellation::new())
                .unwrap()
        );
        assert_eq!(candidate.syntax, "ruby.identifier");
    }
}

#[cfg(test)]
mod outer_owner_tests {
    use super::*;

    #[test]
    fn nested_outer_owners_keep_native_name_geometry_separate_from_comment_spelling() {
        let source = b"module Outer\nclass Written\n# Written\ndef owned(value)\nvalue.message\nend\nend\nend\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let site = source
            .windows(13)
            .position(|raw| raw == b"value.message")
            .unwrap();
        let proofs = prove(
            tree.root_node(),
            source,
            true,
            &mut Work(50_000),
            &Cancellation::new(),
        )
        .unwrap();
        let proof = proofs.get(&(site, site + 5)).unwrap();
        let names: Vec<_> = proof
            .links
            .iter()
            .take(proof.link_count)
            .flatten()
            .filter_map(|link| link.name.map(|name| (link.label, name)))
            .collect();
        let class_start = source.windows(7).position(|raw| raw == b"Written").unwrap();
        let namespace_start = source.windows(5).position(|raw| raw == b"Outer").unwrap();
        assert_eq!(
            names,
            [
                ("ruby.class.declaration", (class_start, class_start + 7)),
                (
                    "ruby.namespace.declaration",
                    (namespace_start, namespace_start + 5)
                ),
            ]
        );
        let comment = source
            .windows(9)
            .position(|raw| raw == b"# Written")
            .unwrap()
            + 2;
        assert_ne!(class_start, comment);
        assert_eq!(
            &source[class_start..class_start + 7],
            &source[comment..comment + 7]
        );
        assert!(
            proof
                .links
                .iter()
                .take(proof.link_count)
                .flatten()
                .filter(|link| link.label.starts_with("ruby.file."))
                .all(|link| link.name.is_none())
        );
    }
}
