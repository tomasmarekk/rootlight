//! Certify program-local receivers of element assignment through capturing blocks.
//! This proves a written lexical Reference only; mutation does not establish a
//! value Read, a dispatched method, or the runtime success of an assignment.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(crate) const MAX_SOURCE: usize = 64 * 1024;
const MAX_SCOPES: usize = 32;
type Binding = ((usize, usize), (usize, usize));
type ScopePath = ([Option<(usize, usize)>; MAX_SCOPES], usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Proof {
    pub(crate) declaration: (usize, usize),
    pub(crate) name: (usize, usize),
    pub(crate) scopes: [Option<(usize, usize)>; MAX_SCOPES],
    pub(crate) scope_count: usize,
    native_depth: usize,
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

fn span(node: Node<'_>) -> (usize, usize) {
    (node.start_byte(), node.end_byte())
}

pub(crate) fn ordinary(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || *b == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && !matches!(
            bytes,
            b"_1" | b"_2" | b"_3" | b"_4" | b"_5" | b"_6" | b"_7" | b"_8" | b"_9" | b"it"
        )
}

fn name<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source [u8]> {
    (node.kind() == "identifier")
        .then(|| source.get(node.byte_range()))
        .flatten()
        .filter(|bytes| ordinary(bytes))
}

fn receiver_assignment<'tree>(
    node: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if node.kind() != "identifier" {
        return Ok(None);
    }
    let mut child = node;
    let mut indexed = false;
    while let Some(parent) = child.parent() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if parent.kind() == "element_reference"
            && parent.child_by_field_name("object") == Some(child)
        {
            indexed = true;
            child = parent;
        } else {
            return Ok((indexed
                && parent.kind() == "assignment"
                && parent.child_by_field_name("left") == Some(child))
            .then_some(parent));
        }
    }
    Ok(None)
}

fn scopes(
    mut child: Node<'_>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<ScopePath>, AdapterError> {
    let mut result = [None; MAX_SCOPES];
    let mut count = 0;
    while let Some(parent) = child.parent() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        match parent.kind() {
            "body_statement" | "block" | "do_block" | "program" => {
                let Some(slot) = result.get_mut(count) else {
                    return Ok(None);
                };
                *slot = Some(span(parent));
                count += 1;
                if parent.kind() == "program" {
                    return Ok(Some((result, count)));
                }
            }
            // Brace body wrappers have no scope capture; their enclosing block does.
            "block_body"
                if parent.parent().is_some_and(|block| {
                    block.kind() == "block" && block.child_by_field_name("body") == Some(parent)
                }) => {}
            "call" if parent.child_by_field_name("block") == Some(child) => {}
            "if" | "unless" | "while" | "until" | "do" | "then" | "else" | "elsif" => {}
            _ => return Ok(None),
        }
        child = parent;
    }
    Ok(None)
}

/// Reconstructs a complete bounded census without accepting producer certificates.
/// `complete` must come from actual input ranges, independently of the tree extent.
pub(crate) fn prove(
    root: Node<'_>,
    source: &[u8],
    complete: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<HashMap<(usize, usize), Proof>, AdapterError> {
    cancel.check()?;
    let mut result = HashMap::new();
    if !complete
        || source.len() > MAX_SOURCE
        || root.kind() != "program"
        || root.parent().is_some()
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(result);
    }
    let mut bindings: HashMap<&[u8], Option<Binding>> = HashMap::new();
    let mut blocked = HashMap::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if !work.step(cancel)? || node.is_error() || node.is_missing() {
            return Ok(result);
        }
        if matches!(
            node.kind(),
            "operator_assignment"
                | "left_assignment_list"
                | "destructured_left_assignment"
                | "rest_assignment"
                | "for"
                | "rescue"
                | "rescue_modifier"
                | "case_match"
                | "in_clause"
                | "match_pattern"
                | "test_pattern"
                | "as_pattern"
                | "variable_reference_pattern"
                | "begin_block"
                | "end_block"
                | "alias"
                | "undef"
        ) {
            return Ok(result);
        }
        if node.kind() == "call"
            && node
                .child_by_field_name("method")
                .and_then(|n| source.get(n.byte_range()))
                .is_some_and(|raw| {
                    matches!(
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
                })
        {
            return Ok(result);
        }
        if node.kind() == "regex" {
            let Some(raw) = source.get(node.byte_range()) else {
                return Ok(result);
            };
            for _ in raw.chunks(64) {
                if !work.step(cancel)? {
                    return Ok(result);
                }
            }
            if raw.windows(3).any(|part| part == b"(?<" || part == b"(?'") {
                return Ok(result);
            }
        }
        if node.kind() == "assignment" {
            let Some(left) = node.child_by_field_name("left") else {
                return Ok(result);
            };
            if !matches!(
                left.kind(),
                "identifier"
                    | "constant"
                    | "scope_resolution"
                    | "instance_variable"
                    | "class_variable"
                    | "global_variable"
                    | "call"
                    | "element_reference"
            ) {
                return Ok(result);
            }
        }
        if node.kind() == "assignment"
            && let Some(left) = node.child_by_field_name("left")
            && let Some(raw) = name(left, source)
        {
            bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            let binding = (node.parent() == Some(root)).then_some((span(node), span(left)));
            bindings
                .entry(raw)
                .and_modify(|entry| *entry = None)
                .or_insert(binding);
        }
        if node.kind() == "identifier" {
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                if !work.step(cancel)? {
                    return Ok(result);
                }
                if matches!(
                    parent.kind(),
                    "block_parameters" | "method_parameters" | "lambda_parameters"
                ) {
                    if let Some(raw) = name(node, source) {
                        blocked
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        blocked.insert(raw, ());
                    }
                    break;
                }
                if !matches!(
                    parent.kind(),
                    "optional_parameter"
                        | "keyword_parameter"
                        | "splat_parameter"
                        | "hash_splat_parameter"
                        | "block_parameter"
                        | "destructured_parameter"
                ) {
                    break;
                }
                ancestor = parent.parent();
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
                break;
            }
        }
        if cursor.node() == root {
            break;
        }
    }
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        if let Some(raw) = name(node, source)
            && !blocked.contains_key(raw)
            && let Some((declaration, target_name)) = bindings.get(raw).copied().flatten()
            && declaration.1 <= node.start_byte()
            && let Some(assignment) = receiver_assignment(node, work, cancel)?
            && let Some((scopes, scope_count)) = scopes(assignment, work, cancel)?
        {
            let mut ancestor = node;
            let mut native_depth = 0;
            while let Some(parent) = ancestor.parent() {
                if !work.step(cancel)? {
                    return Ok(HashMap::new());
                }
                native_depth += 1;
                ancestor = parent;
            }
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.insert(
                span(node),
                Proof {
                    declaration,
                    name: target_name,
                    scopes,
                    scope_count,
                    native_depth,
                },
            );
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
                break;
            }
        }
        if cursor.node() == root {
            break;
        }
    }
    Ok(result)
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    work: Work,
    checked: bool,
    proofs: HashMap<(usize, usize), Proof>,
    context_emitted: bool,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            work: Work(maximum),
            checked: false,
            proofs: HashMap::new(),
            context_emitted: false,
        }
    }

    // A false result refuses the query before a generic Reference is retained.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
        retain: &mut impl FnMut(QueryCandidate) -> Result<(), AdapterError>,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        let file = candidate.role == StructuralRole::Scope && node.kind() == "program";
        if !file && (candidate.role != StructuralRole::Reference || node.kind() != "identifier") {
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
        if file {
            if !self.context_emitted && !self.proofs.is_empty() {
                self.context_emitted = true;
                retain(QueryCandidate {
                    start: 0,
                    end: self.source.len(),
                    role: StructuralRole::Signature,
                    syntax: "ruby.receiver_file_context",
                    required: true,
                    native_depth: 0,
                })?;
                for (&(start, end), proof) in &self.proofs {
                    retain(QueryCandidate {
                        start,
                        end,
                        role: StructuralRole::Reference,
                        syntax: "ruby.lexical_receiver",
                        required: true,
                        native_depth: proof.native_depth,
                    })?;
                    retain(QueryCandidate {
                        start: proof.declaration.0,
                        end,
                        role: StructuralRole::Signature,
                        syntax: "ruby.receiver_target",
                        required: true,
                        native_depth: proof.native_depth,
                    })?;
                }
            }
            return Ok(true);
        }
        let proof = self.proofs.get(&span(node)).copied();
        if let Some(proof) = proof {
            candidate.syntax = "ruby.lexical_receiver";
            candidate.required = true;
            candidate.native_depth = proof.native_depth;
            return Ok(true);
        }
        // Unsupported indexed mutation receivers must not gain a target later
        // from a generic same-spelling resolver candidate.
        let receiver = receiver_assignment(node, &mut self.work, cancel)?;
        if self.work.0 == 0 {
            return Ok(false);
        }
        if receiver.is_some() {
            candidate.syntax = "ruby.unproven_receiver";
        }
        Ok(true)
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
    fn complete_native_census_keeps_exact_declaration_site_and_scope_geometry() {
        let source = b"records = {}\n[1].each do |key|\nrecords[key][0] = 7\nend\n";
        let tree = parse(source);
        let proofs = prove(
            tree.root_node(),
            source,
            true,
            &mut Work(20_000),
            &Cancellation::new(),
        )
        .unwrap();
        let site = source
            .windows(7)
            .rposition(|part| part == b"records")
            .unwrap();
        let proof = proofs.get(&(site, site + 7)).unwrap();
        assert_eq!(proof.declaration, (0, 12));
        assert_eq!(proof.name, (0, 7));
        assert_eq!(proof.scopes[proof.scope_count - 1], Some((0, source.len())));
        assert_eq!(proofs.len(), 1);
    }

    #[test]
    fn full_extent_root_never_substitutes_for_complete_input_authority() {
        let source = b"records = {}\nrecords[0] = 7\n";
        let tree = parse(source);
        assert_eq!(
            (tree.root_node().start_byte(), tree.root_node().end_byte()),
            (0, source.len())
        );
        assert!(
            prove(
                tree.root_node(),
                source,
                false,
                &mut Work(20_000),
                &Cancellation::new()
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            prove(
                tree.root_node(),
                source,
                true,
                &mut Work(1),
                &Cancellation::new()
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn cancellation_precedes_census_even_for_refused_input() {
        let source = b"records = {}\nrecords[0] = 7\n";
        let tree = parse(source);
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            prove(tree.root_node(), source, false, &mut Work(0), &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn clean_case_pattern_binders_never_certify_program_receivers() {
        for (source, hash_pattern) in [
            (
                "records = {}\ncase input\nin records\nend\nrecords[0] = 7\n",
                false,
            ),
            (
                "records = {}\ncase input\nin {records:}\nend\nrecords[0] = 7\n",
                true,
            ),
        ] {
            let tree = parse(source.as_bytes());
            assert!(!tree.root_node().has_error(), "{source}");
            let shape = tree.root_node().to_sexp();
            assert!(shape.contains("case_match"), "{shape}");
            assert!(shape.contains("in_clause"), "{shape}");
            assert_eq!(shape.contains("keyword_pattern"), hash_pattern, "{shape}");
            assert!(
                prove(
                    tree.root_node(),
                    source.as_bytes(),
                    true,
                    &mut Work(20_000),
                    &Cancellation::new()
                )
                .unwrap()
                .is_empty()
            );
            let start = source.rfind("records").unwrap();
            let node = tree
                .root_node()
                .descendant_for_byte_range(start, start + 7)
                .unwrap();
            assert_eq!(node.kind(), "identifier");
            let mut candidate = QueryCandidate {
                start,
                end: start + 7,
                role: StructuralRole::Reference,
                syntax: "ruby.identifier",
                required: false,
                native_depth: 0,
            };
            let mut cache = CaptureCache::new(source.as_bytes(), 20_000, true);
            let mut extra = Vec::new();
            assert!(
                cache
                    .refine(node, &mut candidate, &Cancellation::new(), &mut |fact| {
                        extra.push(fact);
                        Ok(())
                    })
                    .unwrap()
            );
            assert_eq!(candidate.syntax, "ruby.unproven_receiver");
            assert!(extra.is_empty());
        }
    }

    #[test]
    fn exhausted_receiver_classification_refuses_before_generic_retention_and_honors_cancel() {
        let source = b"records = {}\nrecords[0] = 7\n";
        let tree = parse(source);
        let start = source
            .windows(7)
            .rposition(|part| part == b"records")
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 7)
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        for remaining in [0, 1] {
            let mut cache = CaptureCache::new(source, remaining, true);
            // Isolate classification after an already declined census.
            cache.checked = true;
            let mut candidate = QueryCandidate {
                start,
                end: start + 7,
                role: StructuralRole::Reference,
                syntax: "ruby.identifier",
                required: false,
                native_depth: 0,
            };
            assert!(
                !cache
                    .refine(node, &mut candidate, &Cancellation::new(), &mut |_| panic!(
                        "exhaustion cannot emit a certificate"
                    ))
                    .unwrap()
            );
            assert_eq!(cache.work.0, 0);
            let cancel = Cancellation::new();
            cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                cache.refine(node, &mut candidate, &cancel, &mut |_| panic!(
                    "cancellation cannot emit"
                )),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }
}
