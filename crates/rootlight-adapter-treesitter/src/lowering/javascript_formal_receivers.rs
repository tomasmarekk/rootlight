//! Prove direct formal receivers in ordinary module-level JavaScript functions.
//! Fresh source syntax owns the closed body census; native facts and materialized
//! entities must independently agree before a lexical Reference becomes exact.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use tree_sitter::{Node, ParseOptions};

use super::{MaterializedEntity, NativeSourceView, provider_failure, source_for_span};

const REFERENCE: &str = "javascript.identifier.reference";

/// Bounds the possible new RefersTo relations without granting name authority.
pub(super) fn candidate(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE
}

struct Work {
    remaining: usize,
}

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }
}

type Facts<'a> = HashMap<(SyntaxFactKind, SourceSpan), Option<&'a SyntaxFact>>;

fn at<'a>(
    facts: &Facts<'a>,
    kind: SyntaxFactKind,
    span: SourceSpan,
    label: &str,
) -> Option<&'a SyntaxFact> {
    facts
        .get(&(kind, span))
        .copied()
        .flatten()
        .filter(|fact| fact.syntax_kind().as_str() == label)
}

fn child(fact: &SyntaxFact, parent: &SyntaxFact) -> bool {
    fact.parent() == Some(parent.local_id()) && parent.depth().checked_add(1) == Some(fact.depth())
}

fn span(node: Node<'_>, source: &SourceRef) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        source.span().file(),
        u64::try_from(node.start_byte()).map_err(|_| SinkError::AccountingOverflow)?,
        u64::try_from(node.end_byte()).map_err(|_| SinkError::AccountingOverflow)?,
    )
    .map_err(|_| provider_failure("javascript-formal-receiver-span"))
}

fn word<'a>(
    node: Node<'_>,
    source: &'a [u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<&'a str>, AdapterError> {
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    for chunk in raw.chunks(64) {
        if !work.step(cancel)?
            || !chunk
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            return Ok(None);
        }
    }
    if !raw
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        || matches!(
            raw,
            b"eval"
                | b"arguments"
                | b"await"
                | b"yield"
                | b"let"
                | b"static"
                | b"implements"
                | b"interface"
                | b"package"
                | b"private"
                | b"protected"
                | b"public"
                | b"enum"
        )
    {
        return Ok(None);
    }
    Ok(std::str::from_utf8(raw).ok())
}

fn entity_matches(
    entity: &MaterializedEntity,
    definition: &SyntaxFact,
    kind: EntityKind,
    parent: ContainerRef,
    source: &SourceRef,
    physical: SourceSpan,
    name: &str,
) -> bool {
    entity.record.kind == kind
        && entity.record.language == "javascript"
        && entity.record.repository == source.repository()
        && entity.record.generation == source.generation()
        && entity.record.evidence.source.as_ref() == Some(&source_for_span(source, physical))
        && entity.definition_local_id == Some(definition.local_id())
        && entity.direct_parent == parent
        && entity.record.container == Some(parent)
        && entity.record.canonical_name == name
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

/// Complete physical bytes and their caller-validated logical coordinate view.
pub(super) struct CurrentSource<'a, 'text> {
    pub(super) reference: &'a SourceRef,
    pub(super) view: &'a NativeSourceView<'text>,
    pub(super) original: &'a [u8],
    pub(super) maximum: usize,
}

impl Targets {
    /// Reconstructs a bounded complete-source proof and joins unique native facts.
    /// Unsupported bodies or exhausted work publish no exact bindings.
    ///
    /// # Errors
    /// Returns cancellation, allocation, coordinate or accounting failures.
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: CurrentSource<'_, '_>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let CurrentSource {
            reference: source,
            view,
            original,
            maximum,
        } = current;
        let mut output = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let mut work = Work { remaining: maximum };
        let mut symbols = HashMap::new();
        let mut has_parameter = false;
        for entity in entities.values() {
            if !work.step(cancel)? {
                return Ok(output);
            }
            if entity.record.language == "javascript"
                && matches!(
                    entity.record.kind,
                    EntityKind::Function | EntityKind::Parameter
                )
            {
                has_parameter |= entity.record.kind == EntityKind::Parameter;
                symbols
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                // Coalesced semantic IDs may retain only the first physical
                // declaration; they cannot certify another declaration's formal.
                if symbols.insert(entity.record.id, ()).is_some() {
                    return Ok(output);
                }
            }
        }
        if !has_parameter {
            return Ok(output);
        }
        if source.span().start_byte() != 0
            || u64::try_from(original.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(output);
        }
        let mut hash = blake3::Hasher::new();
        for chunk in original.chunks(4096) {
            if !work.step(cancel)? {
                return Ok(output);
            }
            hash.update(chunk);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash() {
            return Ok(output);
        }
        let logical = view.as_bytes();
        let envelope = SourceSpan::new(
            source.span().file(),
            0,
            u64::try_from(logical.len()).map_err(|_| SinkError::AccountingOverflow)?,
        )
        .map_err(|_| provider_failure("javascript-formal-receiver-envelope"))?;
        if view.physical_span(envelope)? != source.span() {
            return Ok(output);
        }
        let mut shapes = Facts::new();
        let mut ids = HashMap::new();
        let mut has_reference = false;
        for fact in facts {
            if !work.step(cancel)?
                || fact.kind() == SyntaxFactKind::ErrorRecovery
                || fact.span().file() != source.span().file()
            {
                return Ok(output);
            }
            ids.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if ids.insert(fact.local_id(), fact).is_some() {
                return Ok(output);
            }
            shapes
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            shapes
                .entry((fact.kind(), fact.span()))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            has_reference |= candidate(fact);
        }
        if !has_reference {
            return Ok(output);
        }
        let (Some(root), Some(module)) = (
            at(
                &shapes,
                SyntaxFactKind::Root,
                envelope,
                "javascript.program.root",
            ),
            at(
                &shapes,
                SyntaxFactKind::Module,
                envelope,
                "javascript.file.module",
            ),
        ) else {
            return Ok(output);
        };
        if root.parent().is_some() || root.depth() != 0 || !child(module, root) {
            return Ok(output);
        }
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.kind == EntityKind::Module
                && entity.record.language == "javascript"
                && entity.record.repository == source.repository()
                && entity.record.generation == source.generation()
                && entity.record.evidence.source.as_ref() == Some(source)
                && entity.direct_parent == ContainerRef::File(source.span().file())
                && entity.record.container == Some(entity.direct_parent)
        }) else {
            return Ok(output);
        };
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .map_err(|_| provider_failure("javascript-formal-receiver-grammar"))?;
        let mut exhausted = false;
        let mut progress = |_: &tree_sitter::ParseState| {
            if matches!(work.step(cancel), Ok(true)) {
                ControlFlow::Continue(())
            } else {
                exhausted = true;
                ControlFlow::Break(())
            }
        };
        let mut input =
            |offset: usize, _: tree_sitter::Point| logical.get(offset..).unwrap_or_default();
        let tree = parser.parse_with_options(
            &mut input,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        let Some(tree) = tree.filter(|_| !exhausted) else {
            return Ok(output);
        };
        let native_root = tree.root_node();
        if native_root.kind() != "program"
            || native_root.has_error()
            || native_root.is_missing()
            || native_root.end_byte() != logical.len()
            || !matches!(
                logical.get(..native_root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(output);
        }
        let mut cursor = native_root.walk();
        for function in native_root.named_children(&mut cursor) {
            if !work.step(cancel)? {
                output.bindings.clear();
                return Ok(output);
            }
            if function.kind() != "function_declaration" {
                continue;
            }
            if !output.function(
                function,
                module,
                module_entity,
                &shapes,
                entities,
                view,
                &mut work,
                cancel,
            )? {
                output.bindings.clear();
                return Ok(output);
            }
        }
        Ok(output)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the native and materialized ownership proofs remain distinct"
    )]
    fn function(
        &mut self,
        function: Node<'_>,
        module: &SyntaxFact,
        module_entity: &MaterializedEntity,
        facts: &Facts<'_>,
        entities: &HashMap<u64, MaterializedEntity>,
        view: &NativeSourceView<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let source = &self.source;
        let logical = view.as_bytes();
        let (Some(name), Some(parameters), Some(body)) = (
            function.child_by_field_name("name"),
            function.child_by_field_name("parameters"),
            function.child_by_field_name("body"),
        ) else {
            return Ok(true);
        };
        let Some(name_text) = word(name, logical, work, cancel)? else {
            return Ok(work.remaining != 0);
        };
        let mut cursor = function.walk();
        for node in function.children(&mut cursor) {
            if !work.step(cancel)? {
                return Ok(false);
            }
            if !node.is_extra()
                && node != name
                && node != parameters
                && node != body
                && node.kind() != "function"
            {
                return Ok(true);
            }
        }
        let function_span = span(function, source)?;
        let (Some(scope), Some(owner), Some(definition), Some(parameter_list), Some(block)) = (
            at(
                facts,
                SyntaxFactKind::Scope,
                function_span,
                "javascript.function.scope",
            ),
            at(
                facts,
                SyntaxFactKind::Declaration,
                function_span,
                "javascript.function.declaration",
            ),
            at(
                facts,
                SyntaxFactKind::Occurrence,
                span(name, source)?,
                "javascript.identifier.definition",
            ),
            at(
                facts,
                SyntaxFactKind::Signature,
                span(parameters, source)?,
                "javascript.parameters.signature",
            ),
            at(
                facts,
                SyntaxFactKind::Scope,
                span(body, source)?,
                "javascript.block.scope",
            ),
        ) else {
            return Ok(true);
        };
        if !child(scope, module)
            || !child(owner, scope)
            || !child(definition, owner)
            || !child(parameter_list, owner)
            || !child(block, owner)
        {
            return Ok(true);
        }
        let owner_physical = view.physical_span(function_span)?;
        let Some(owner_entity) = entities.get(&owner.local_id()).filter(|entity| {
            entity_matches(
                entity,
                definition,
                EntityKind::Function,
                ContainerRef::Entity(module_entity.record.id),
                source,
                owner_physical,
                name_text,
            )
        }) else {
            return Ok(true);
        };
        let mut formals = HashMap::new();
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if !work.step(cancel)? {
                return Ok(false);
            }
            if parameter.is_extra() {
                continue;
            }
            let Some(identifier) = parameter
                .child_by_field_name("pattern")
                .or_else(|| parameter.child_by_field_name("name"))
            else {
                return Ok(true);
            };
            if parameter.kind() != "required_parameter"
                || identifier.kind() != "identifier"
                || parameter.byte_range() != identifier.byte_range()
            {
                return Ok(true);
            }
            let Some(text) = word(identifier, logical, work, cancel)? else {
                return Ok(work.remaining != 0);
            };
            let physical = view.physical_span(span(identifier, source)?)?;
            let (Some(declaration), Some(definition)) = (
                at(
                    facts,
                    SyntaxFactKind::Declaration,
                    span(identifier, source)?,
                    "javascript.parameter.declaration",
                ),
                at(
                    facts,
                    SyntaxFactKind::Occurrence,
                    span(identifier, source)?,
                    "javascript.identifier.definition",
                ),
            ) else {
                return Ok(true);
            };
            let Some(entity) = entities.get(&declaration.local_id()).filter(|entity| {
                entity_matches(
                    entity,
                    definition,
                    EntityKind::Parameter,
                    ContainerRef::Entity(owner_entity.record.id),
                    source,
                    physical,
                    text,
                )
            }) else {
                return Ok(true);
            };
            if !child(declaration, owner)
                || !child(definition, declaration)
                || entity.record.provenance != owner_entity.record.provenance
            {
                return Ok(true);
            }
            formals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if formals.insert(text, entity).is_some() {
                return Ok(true);
            }
        }
        if formals.is_empty() {
            return Ok(true);
        }
        let mut receivers = Vec::new();
        let mut cursor = body.walk();
        loop {
            if !work.step(cancel)? {
                return Ok(false);
            }
            let node = cursor.node();
            // A closed expression-only body has no nested scope, binding, write,
            // dynamic environment or TSX-only syntax that could defeat this join.
            if node.is_named()
                && !matches!(
                    node.kind(),
                    "statement_block"
                        | "expression_statement"
                        | "return_statement"
                        | "call_expression"
                        | "member_expression"
                        | "arguments"
                        | "binary_expression"
                        | "ternary_expression"
                        | "parenthesized_expression"
                        | "object"
                        | "pair"
                        | "array"
                        | "template_string"
                        | "template_substitution"
                        | "string"
                        | "string_fragment"
                        | "escape_sequence"
                        | "number"
                        | "true"
                        | "false"
                        | "null"
                        | "identifier"
                        | "property_identifier"
                        | "comment"
                )
            {
                return Ok(true);
            }
            if node.kind() == "statement_block" && node != body {
                return Ok(true);
            }
            if node.kind() == "identifier" {
                let Some(text) = word(node, logical, work, cancel)? else {
                    return Ok(work.remaining != 0);
                };
                if let Some(target) = formals.get(text)
                    && let Some(member) = node.parent().filter(|parent| {
                        parent.kind() == "member_expression"
                            && parent.child_by_field_name("object") == Some(node)
                    })
                    && member
                        .child_by_field_name("property")
                        .is_some_and(|property| property.kind() == "property_identifier")
                    && member.parent().is_some_and(|call| {
                        call.kind() == "call_expression"
                            && call.child_by_field_name("function") == Some(member)
                    })
                {
                    let Some(reference) = at(
                        facts,
                        SyntaxFactKind::Occurrence,
                        span(node, source)?,
                        REFERENCE,
                    )
                    .filter(|fact| child(fact, block)) else {
                        return Ok(true);
                    };
                    receivers
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    receivers.push((
                        reference.local_id(),
                        Binding {
                            target: target.record.id,
                            owner: owner_entity.record.id,
                            span: view.physical_span(reference.span())?,
                            spelling: content_hash(text.as_bytes()),
                            provenance: target.record.provenance,
                        },
                    ));
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    self.bindings
                        .try_reserve(receivers.len())
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.bindings.extend(receivers);
                    return Ok(true);
                }
            }
        }
    }

    /// Adopts only the unchanged, source-owned unresolved native Reference.
    ///
    /// # Errors
    /// Returns cancellation before exposing a target.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok((candidate(fact) && physical == binding.span && occurrence.source == source_for_span(&self.source, physical)
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation()
            && occurrence.file == physical.file() && occurrence.role == OccurrenceRole::Reference
            && occurrence.syntax_kind == REFERENCE && occurrence.syntactic_text_hash == binding.spelling
            && occurrence.enclosing == Some(binding.owner) && occurrence.provenance == binding.provenance
            && occurrence.evidence.source.as_ref() == Some(&occurrence.source)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling)).then_some(binding.target))
    }
}
