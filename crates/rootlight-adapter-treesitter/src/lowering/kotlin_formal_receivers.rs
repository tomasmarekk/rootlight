//! Prove a plain Kotlin formal receiver in a function's first call statement.
//! Native coordinates are geometry only; consumers must freshly parse current
//! bytes and join exact source, declaration, definition and owner identities.

use std::{
    collections::{HashMap, HashSet},
    ops::ControlFlow,
};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use tree_sitter::{Node, ParseOptions};

use super::MaterializedEntity;

type Span = (usize, usize);

/// Coordinates needed to join one receiver to its exact current formal owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Proof {
    pub(super) function: Span,
    pub(super) function_name: Span,
    pub(super) header: Span,
    pub(super) parameters: Span,
    pub(super) parameter: Span,
    pub(super) parameter_name: Span,
    pub(super) use_name: Span,
    pub(super) body: Span,
    pub(super) first_statement: Span,
}

/// Finite shared credit for native links, header children and inspected bytes.
pub(super) struct Work(pub(super) usize);

impl Work {
    /// Observe cancellation before consuming one checked unit of proof work.
    pub(super) fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
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

fn named<'tree>(node: Node<'tree>, index: usize) -> Option<Node<'tree>> {
    node.named_child(u32::try_from(index).ok()?)
}

fn first<'tree>(
    node: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    for index in 0..node.named_child_count() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let Some(child) = named(node, index) else {
            return Ok(None);
        };
        if !child.is_extra() {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn pair<'tree>(
    node: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<(Node<'tree>, Node<'tree>)>, AdapterError> {
    let mut left = None;
    let mut right = None;
    for index in 0..node.named_child_count() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let Some(child) = named(node, index) else {
            return Ok(None);
        };
        if child.is_extra() {
            continue;
        }
        if left.is_none() {
            left = Some(child);
        } else if right.is_none() {
            right = Some(child);
        } else {
            return Ok(None);
        }
    }
    Ok(left.zip(right))
}

fn bytes<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source [u8]> {
    source.get(node.byte_range())
}

fn ordinary<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source [u8]> {
    let name = bytes(node, source)?;
    (node.kind() == "identifier"
        && !name.is_empty()
        && name.len() <= 128
        && name != b"_"
        && name
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_'))
    .then_some(name)
}

fn charged(bytes: &[u8], work: &mut Work, cancel: &Cancellation) -> Result<bool, AdapterError> {
    for _ in bytes.chunks(64) {
        if !work.step(cancel)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn layout(bytes: &[u8], work: &mut Work, cancel: &Cancellation) -> Result<bool, AdapterError> {
    Ok(charged(bytes, work, cancel)? && bytes.iter().all(u8::is_ascii_whitespace))
}

fn plain_navigation(
    node: Node<'_>,
    child: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some((left, right)) = pair(node, work, cancel)? else {
        return Ok(false);
    };
    let Some(gap) = source.get(left.end_byte()..right.start_byte()) else {
        return Ok(false);
    };
    Ok(left == child
        && ordinary(right, source).is_some()
        && charged(gap, work, cancel)?
        && gap
            .iter()
            .filter(|b| !b.is_ascii_whitespace())
            .copied()
            .eq(*b"."))
}

/// Refuse unsafe scopes while walking only the use's own ancestor branch.
fn owner<'tree>(
    receiver: Node<'tree>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<(Node<'tree>, Node<'tree>, Node<'tree>)>, AdapterError> {
    let mut child = receiver;
    while let Some(parent) = child.parent() {
        if !work.step(cancel)? || parent.has_error() || parent.is_missing() {
            return Ok(None);
        }
        match parent.kind() {
            "navigation_expression" => {
                if !plain_navigation(parent, child, source, work, cancel)? {
                    return Ok(None);
                }
            }
            "value_argument" => {
                if first(parent, work, cancel)? != Some(child)
                    || parent.named_child_count() != 1
                    || span(parent) != span(child)
                {
                    return Ok(None);
                }
            }
            "value_arguments" if child.kind() == "value_argument" => {}
            "call_expression" => {
                // A trailing lambda is a sibling of the callee/arguments. Its body
                // must never become an ancestor admitted through this branch.
                if child.kind() != "value_arguments" && first(parent, work, cancel)? != Some(child)
                {
                    return Ok(None);
                }
            }
            "block" => {
                if child.kind() != "call_expression" || first(parent, work, cancel)? != Some(child)
                {
                    return Ok(None);
                }
                let Some(body) = parent
                    .parent()
                    .filter(|body| body.kind() == "function_body")
                else {
                    return Ok(None);
                };
                let Some(function) = body
                    .parent()
                    .filter(|node| node.kind() == "function_declaration")
                else {
                    return Ok(None);
                };
                let Some(prefix_start) = parent.start_byte().checked_add(1) else {
                    return Ok(None);
                };
                let Some(prefix) = source.get(prefix_start..child.start_byte()) else {
                    return Ok(None);
                };
                if source.get(parent.start_byte()) != Some(&b'{')
                    || !layout(prefix, work, cancel)?
                    || first(body, work, cancel)? != Some(parent)
                {
                    return Ok(None);
                }
                return Ok(Some((function, parent, child)));
            }
            _ => return Ok(None),
        }
        child = parent;
    }
    Ok(None)
}

/// Prove one complete-source, top-level ordinary formal used as a plain receiver.
/// Later statements and trailing lambda bodies are not traversed. Unsupported
/// headers/scopes, ambiguity or finite-work exhaustion return no proof; cancellation
/// and allocation failures remain errors. The caller supplies freshly parsed nodes.
pub(super) fn prove(
    root: Node<'_>,
    receiver: Node<'_>,
    source: &[u8],
    complete: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Proof>, AdapterError> {
    if !work.step(cancel)?
        || !complete
        || root.kind() != "source_file"
        || root.parent().is_some()
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(None);
    }
    let Some(use_name) = ordinary(receiver, source) else {
        return Ok(None);
    };
    if receiver
        .parent()
        .is_none_or(|node| node.kind() != "navigation_expression")
    {
        return Ok(None);
    }
    let Some((function, body, statement)) = owner(receiver, source, work, cancel)? else {
        return Ok(None);
    };
    if function.parent() != Some(root) || function.has_error() || function.is_missing() {
        return Ok(None);
    }
    // The pinned grammar can parse an unsupported context(...) header as a separate
    // top-level call. Such a prefix cannot certify an ordinary function declaration.
    for index in 0..root.named_child_count() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let Some(node) = named(root, index) else {
            return Ok(None);
        };
        if node == function {
            break;
        }
        if !node.is_extra()
            && !matches!(
                node.kind(),
                "package_header"
                    | "import"
                    | "function_declaration"
                    | "class_declaration"
                    | "object_declaration"
                    | "property_declaration"
                    | "type_alias"
                    | "file_annotation"
            )
        {
            return Ok(None);
        }
    }
    let Some(function_name) = function.child_by_field_name("name") else {
        return Ok(None);
    };
    if ordinary(function_name, source).is_none() {
        return Ok(None);
    }
    let Some(prefix) = source.get(function.start_byte()..function_name.start_byte()) else {
        return Ok(None);
    };
    if !charged(prefix, work, cancel)? {
        return Ok(None);
    }
    let mut tokens = prefix
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty());
    let mut fun_seen = false;
    for token in &mut tokens {
        if token == b"fun" && !fun_seen {
            fun_seen = true;
        } else if fun_seen || !matches!(token, b"internal" | b"public" | b"private" | b"protected")
        {
            return Ok(None);
        }
    }
    if !fun_seen {
        return Ok(None);
    }
    let mut parameters = None;
    for index in 0..function.named_child_count() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let Some(node) = named(function, index) else {
            return Ok(None);
        };
        match node.kind() {
            "function_value_parameters" if parameters.is_none() => parameters = Some(node),
            "type_parameters" | "type_constraints" => return Ok(None),
            _ => {}
        }
    }
    let Some(parameters) = parameters else {
        return Ok(None);
    };
    let Some(gap) = source.get(function_name.end_byte()..parameters.start_byte()) else {
        return Ok(None);
    };
    if !layout(gap, work, cancel)? {
        return Ok(None);
    }
    let mut names = HashSet::new();
    let mut selected = None;
    for index in 0..parameters.named_child_count() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let Some(parameter) = named(parameters, index) else {
            return Ok(None);
        };
        if parameter.is_extra() {
            continue;
        }
        if parameter.kind() != "parameter" {
            return Ok(None);
        }
        let Some((name, _)) = pair(parameter, work, cancel)? else {
            return Ok(None);
        };
        let Some(written) = ordinary(name, source) else {
            return Ok(None);
        };
        names
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        if !names.insert(written) {
            return Ok(None);
        }
        if written == use_name {
            selected = Some((span(parameter), span(name)));
        }
    }
    let Some((parameter, parameter_name)) = selected else {
        return Ok(None);
    };
    Ok(Some(Proof {
        function: span(function),
        function_name: span(function_name),
        header: (function.start_byte(), body.start_byte()),
        parameters: span(parameters),
        parameter,
        parameter_name,
        use_name: span(receiver),
        body: span(body),
        first_statement: span(statement),
    }))
}

const MAX_SOURCE: usize = 64 * 1024;

/// Recognize an ordinary capture without treating its label as binding authority.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "kotlin.identifier.reference"
}

fn same_source(part: &SourceRef, whole: &SourceRef) -> bool {
    *part == super::source_for_span(whole, part.span())
}

fn native_span(source: &SourceRef, span: Span) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        source.span().file(),
        u64::try_from(span.0).map_err(|_| SinkError::InvalidIr)?,
        u64::try_from(span.1).map_err(|_| SinkError::InvalidIr)?,
    )
    .map_err(|_| SinkError::InvalidIr.into())
}

struct Bound<'a> {
    parameter: &'a MaterializedEntity,
    function: &'a MaterializedEntity,
    reference: u64,
    raw_span: SourceSpan,
    raw_parent: Option<u64>,
    raw_depth: usize,
}

/// Fresh native proofs joined to unique raw facts and current materialized owners.
pub(super) struct Targets<'a> {
    bindings: HashMap<SourceSpan, Bound<'a>>,
    source: &'a SourceRef,
}

impl<'a> Targets<'a> {
    /// Authenticate physical bytes, parse current decoded bytes once, and join direct witnesses.
    /// Partial native inputs and exhausted work leave no optimistic bindings.
    #[expect(
        clippy::too_many_arguments,
        reason = "physical and decoded source authority are separate"
    )]
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        current: &[u8],
        original: &[u8],
        maximum: usize,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let empty = || Self {
            bindings: HashMap::new(),
            source,
        };
        if maximum == 0
            || current.len() > MAX_SOURCE
            || original.len() > MAX_SOURCE * 4
            || source.span().start_byte() != 0
            || u64::try_from(original.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(empty());
        }
        let Ok(decoded) = std::str::from_utf8(current) else {
            return Ok(empty());
        };
        let mut work = Work(maximum);
        let mut ids = HashSet::new();
        let mut unique = HashMap::new();
        let mut root = None;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(empty());
            }
            if fact.kind() == SyntaxFactKind::ErrorRecovery
                || fact.span().file() != source.span().file()
                || fact.span().end_byte()
                    > u64::try_from(current.len()).map_err(|_| SinkError::InvalidIr)?
            {
                return Ok(empty());
            }
            ids.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            let mapped = physical(fact.span())?;
            if mapped.file() != source.span().file()
                || mapped.end_byte() > source.span().end_byte()
                || !ids.insert(fact.local_id())
                || unique
                    .insert((fact.kind(), fact.syntax_kind().as_str(), mapped), fact)
                    .is_some()
            {
                return Ok(empty());
            }
            if fact.kind() == SyntaxFactKind::Root && root.replace(fact).is_some() {
                return Ok(empty());
            }
        }
        let Some(root) = root else { return Ok(empty()) };
        if root.syntax_kind().as_str() != "kotlin.file.root"
            || root.parent().is_some()
            || root.depth() != 0
        {
            return Ok(empty());
        }
        let mut hash = blake3::Hasher::new();
        for chunk in original.chunks(4096) {
            if !work.step(cancel)? {
                return Ok(empty());
            }
            hash.update(chunk);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash() {
            return Ok(empty());
        }
        let whole = native_span(source, (0, current.len()))?;
        if !charged(current, &mut work, cancel)?
            || physical(whole)? != source.span()
            || text(whole)? != decoded
        {
            return Ok(empty());
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .map_err(|_| super::provider_failure("kotlin-formal-consumer-grammar"))?;
        let mut exhausted = false;
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancel.check().is_err() {
                return ControlFlow::Break(());
            }
            let Some(next) = work.0.checked_sub(1) else {
                exhausted = true;
                return ControlFlow::Break(());
            };
            work.0 = next;
            ControlFlow::Continue(())
        };
        let tree = parser.parse_with_options(
            &mut |offset, _| current.get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        if exhausted {
            return Ok(empty());
        }
        let Some(tree) = tree else { return Ok(empty()) };
        let native_root = tree.root_node();
        if native_root.has_error()
            || native_root.is_missing()
            || native_root.end_byte() != current.len()
            || root.span().end_byte() != whole.end_byte()
            || root.span().start_byte() != 0
        {
            return Ok(empty());
        }
        let lookup = |kind, label, extent| unique.get(&(kind, label, extent)).copied();
        let mut output = empty();
        for reference in facts.iter().filter(|fact| required(fact)) {
            if !work.step(cancel)? {
                return Ok(empty());
            }
            let Ok(start) = usize::try_from(reference.span().start_byte()) else {
                return Ok(empty());
            };
            let Ok(end) = usize::try_from(reference.span().end_byte()) else {
                return Ok(empty());
            };
            let Some(node) = native_root.descendant_for_byte_range(start, end) else {
                continue;
            };
            if span(node) != (start, end) {
                continue;
            }
            let Some(proof) = prove(native_root, node, current, true, &mut work, cancel)? else {
                if work.0 == 0 {
                    return Ok(empty());
                }
                continue;
            };
            let mut mapped = |value| physical(native_span(source, value)?);
            let function_span = mapped(proof.function)?;
            let function_name = mapped(proof.function_name)?;
            let parameters_span = mapped(proof.parameters)?;
            let parameter_span = mapped(proof.parameter)?;
            let parameter_name = mapped(proof.parameter_name)?;
            let body_span = mapped(proof.body)?;
            let site = mapped(proof.use_name)?;
            let Some(function_fact) = lookup(
                SyntaxFactKind::Declaration,
                "kotlin.function.declaration",
                function_span,
            ) else {
                continue;
            };
            let Some(function_definition) = lookup(
                SyntaxFactKind::Occurrence,
                "kotlin.identifier.definition",
                function_name,
            ) else {
                continue;
            };
            let Some(signature) = lookup(
                SyntaxFactKind::Signature,
                "kotlin.parameters.signature",
                parameters_span,
            ) else {
                continue;
            };
            let Some(parameter_fact) = lookup(
                SyntaxFactKind::Declaration,
                "kotlin.parameter.declaration",
                parameter_span,
            ) else {
                continue;
            };
            let Some(parameter_definition) = lookup(
                SyntaxFactKind::Occurrence,
                "kotlin.identifier.definition",
                parameter_name,
            ) else {
                continue;
            };
            let Some(scope) = lookup(SyntaxFactKind::Scope, "kotlin.block.scope", body_span) else {
                continue;
            };
            let Some(function) = entities.get(&function_fact.local_id()) else {
                continue;
            };
            let Some(parameter) = entities.get(&parameter_fact.local_id()) else {
                continue;
            };
            if function_fact.parent() != Some(root.local_id())
                || function_fact.depth() != 1
                || function_definition.parent() != Some(function_fact.local_id())
                || function_definition.depth() != 2
                || signature.parent() != Some(function_fact.local_id())
                || signature.depth() != 2
                || parameter_fact.parent() != Some(function_fact.local_id())
                || parameter_fact.depth() != 2
                || parameter_definition.parent() != Some(parameter_fact.local_id())
                || parameter_definition.depth() != 3
                || scope.parent() != Some(function_fact.local_id())
                || scope.depth() != 2
                || reference.parent() != Some(scope.local_id())
                || reference.depth() != 3
                || function.definition_local_id != Some(function_definition.local_id())
                || parameter.definition_local_id != Some(parameter_definition.local_id())
                || function.record.kind != EntityKind::Function
                || parameter.record.kind != EntityKind::Parameter
                || function.record.language != "kotlin"
                || parameter.record.language != "kotlin"
                || function.record.canonical_name != text(function_definition.span())?
                || parameter.record.canonical_name != text(parameter_definition.span())?
                || parameter.record.canonical_name != text(reference.span())?
                || function.direct_parent != ContainerRef::File(source.span().file())
                || function.record.container != Some(function.direct_parent)
                || parameter.direct_parent != ContainerRef::Entity(function.record.id)
                || parameter.record.container != Some(parameter.direct_parent)
                || parameter.record.provenance != function.record.provenance
            {
                continue;
            }
            let mut valid = true;
            for (entity, extent) in [(function, function_span), (parameter, parameter_span)] {
                if !work.step(cancel)? {
                    return Ok(empty());
                }
                valid &= entity.record.repository == source.repository()
                    && entity.record.generation == source.generation()
                    && entity
                        .record
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|part| part.span() == extent && same_source(part, source));
            }
            if !valid {
                continue;
            }
            output
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            output.bindings.insert(
                site,
                Bound {
                    parameter,
                    function,
                    reference: reference.local_id(),
                    raw_span: reference.span(),
                    raw_parent: reference.parent(),
                    raw_depth: reference.depth(),
                },
            );
        }
        Ok(output)
    }

    /// Resolve only the exact unresolved Reference and current certified Function owner.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        written: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(bound) = self.bindings.get(&occurrence.source.span()) else {
            return Ok(None);
        };
        if !required(fact)
            || fact.local_id() != bound.reference
            || fact.span() != bound.raw_span
            || fact.parent() != bound.raw_parent
            || fact.depth() != bound.raw_depth
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.syntax_kind != "kotlin.identifier.reference"
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || owner.is_none_or(|entity| {
                entity.record != bound.function.record
                    || entity.direct_parent != bound.function.direct_parent
                    || entity.definition_local_id != bound.function.definition_local_id
                    || entity.identity_guard != bound.function.identity_guard
            })
            || occurrence.enclosing != Some(bound.function.record.id)
            || occurrence.provenance != bound.function.record.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || !same_source(&occurrence.source, self.source)
            || occurrence.syntactic_text_hash != content_hash(written.as_bytes())
            || written != bound.parameter.record.canonical_name
        {
            return Ok(None);
        }
        Ok(Some(bound.parameter.record.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn receiver<'tree>(
        tree: &'tree tree_sitter::Tree,
        source: &str,
        witness: &str,
        offset: usize,
        length: usize,
    ) -> Node<'tree> {
        let start = source.find(witness).unwrap() + offset;
        tree.root_node()
            .descendant_for_byte_range(start, start + length)
            .unwrap()
    }

    #[test]
    fn plain_receivers_before_trailing_lambdas_keep_exact_formal_geometry() {
        for name in ["destination", "output", "parcel"] {
            let source = format!(
                "internal fun zipFiles({name}: File, contents: List<File>) {{\r\n  ZipOutputStream(BufferedOutputStream(FileOutputStream({name}.absolutePath))).use {{ {name} -> consume({name}) }}\r\n  val later = 1\r\n}}\r\n"
            );
            let tree = tree(source.as_bytes());
            let site = receiver(
                &tree,
                &source,
                &format!("{name}.absolutePath"),
                0,
                name.len(),
            );
            let proof = prove(
                tree.root_node(),
                site,
                source.as_bytes(),
                true,
                &mut Work(4096),
                &Cancellation::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                &source.as_bytes()[proof.parameter_name.0..proof.parameter_name.1],
                name.as_bytes()
            );
            assert_eq!(
                &source.as_bytes()[proof.use_name.0..proof.use_name.1],
                name.as_bytes()
            );
            assert_eq!(
                &source.as_bytes()[proof.function_name.0..proof.function_name.1],
                b"zipFiles"
            );
            assert_eq!(
                &source.as_bytes()[proof.parameter.0..proof.parameter.1],
                format!("{name}: File").as_bytes()
            );
            assert_eq!(proof.header, (proof.function.0, proof.body.0));
            assert!(
                proof.parameters.0 < proof.parameter.0 && proof.parameter.1 < proof.parameters.1
            );
            assert!(
                proof.body.0 < proof.first_statement.0 && proof.first_statement.1 < proof.body.1
            );
        }
    }

    #[test]
    fn unsupported_lexical_paths_and_headers_never_supply_a_proof() {
        let cases = [
            (
                "fun f(destination: File) { consume(other.destination) }",
                "other.destination",
                6,
            ),
            (
                "fun f(destination: File) { consume(1).use { destination -> consume(destination.path) } }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File) { val before = 1; consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File) { val (a, b) = pair; consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File) { if (flag) { consume(destination.path) } }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File) { val value = consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun File.f(destination: File) { consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun <T> f(destination: File) { consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File = fallback) { consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "class C { fun f(destination: File) { consume(destination.path) } }",
                "destination.path",
                0,
            ),
            (
                "class C(destination: File) { init { consume(destination.path) } }",
                "destination.path",
                0,
            ),
            (
                "context(File) fun f(destination: File) { consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File, destination: Other) { consume(destination.path) }",
                "destination.path",
                0,
            ),
            (
                "fun f(destination: File) { consume(destination?.path) }",
                "destination?.path",
                0,
            ),
            (
                "fun f(destination: File) { consume(destination.path)",
                "destination.path",
                0,
            ),
        ];
        for (source, witness, offset) in cases {
            let tree = tree(source.as_bytes());
            let site = receiver(&tree, source, witness, offset, "destination".len());
            assert!(
                prove(
                    tree.root_node(),
                    site,
                    source.as_bytes(),
                    true,
                    &mut Work(4096),
                    &Cancellation::new()
                )
                .unwrap()
                .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn completeness_tree_identity_work_and_cancellation_are_checked() {
        let source = "fun f(destination: File) { consume(destination.path) }";
        let tree = tree(source.as_bytes());
        let site = receiver(&tree, source, "destination.path", 0, 11);
        let cancel = Cancellation::new();
        let mut work = Work(4096);
        assert!(
            prove(
                tree.root_node(),
                site,
                source.as_bytes(),
                true,
                &mut work,
                &cancel
            )
            .unwrap()
            .is_some()
        );
        let used = 4096 - work.0;
        assert!(
            prove(
                tree.root_node(),
                site,
                source.as_bytes(),
                true,
                &mut Work(used - 1),
                &cancel
            )
            .unwrap()
            .is_none()
        );
        assert!(
            prove(
                tree.root_node(),
                site,
                source.as_bytes(),
                false,
                &mut Work(4096),
                &cancel
            )
            .unwrap()
            .is_none()
        );
        assert!(
            prove(
                tree.root_node(),
                site,
                &source.as_bytes()[..source.len() - 1],
                true,
                &mut Work(4096),
                &cancel
            )
            .unwrap()
            .is_none()
        );
        let foreign = super::tests::tree(source.as_bytes());
        assert!(
            prove(
                tree.root_node(),
                receiver(&foreign, source, "destination.path", 0, 11),
                source.as_bytes(),
                true,
                &mut Work(4096),
                &cancel
            )
            .unwrap()
            .is_none()
        );
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            prove(
                tree.root_node(),
                site,
                source.as_bytes(),
                true,
                &mut Work(4096),
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
