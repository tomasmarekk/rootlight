//! Source categories and a closed local-set proof do not imply evaluated configuration.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.syntax_kind().as_str() == "cmake.normal_variable.reference"
        || (fact.kind() == SyntaxFactKind::Signature
            && matches!(
                fact.syntax_kind().as_str(),
                "cmake.binding_context.expression"
                    | "cmake.local_read.expression"
                    | "cmake.local_set.expression"
                    | "cmake.binding_preserves.expression"
                    | "cmake.binding_values_barrier.expression"
                    | "cmake.binding_barrier.expression"
            ))
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

fn bytes_for(source: &[u8], span: SourceSpan) -> Option<&[u8]> {
    source.get(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
}

// A missing command certificate leaves non-comment source between events and
// refuses the proof; bounded extraction cannot erase an intervening write.
fn separators(bytes: &[u8], beginning: bool, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let mut at = usize::from(beginning && bytes.starts_with(b"\xef\xbb\xbf")) * 3;
    while let Some(byte) = bytes.get(at).copied() {
        cancel.check()?;
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte != b'#' {
            return Ok(false);
        }
        at += 1;
        if bytes.get(at) == Some(&b'[') {
            let opening = at;
            at += 1;
            while bytes.get(at) == Some(&b'=') {
                cancel.check()?;
                at += 1;
            }
            if bytes.get(at) == Some(&b'[') {
                let equals = at - opening - 1;
                at += 1;
                let mut closed = false;
                while let Some(byte) = bytes.get(at) {
                    cancel.check()?;
                    if *byte == b']'
                        && bytes
                            .get(at + 1..at + 1 + equals)
                            .is_some_and(|raw| raw.iter().all(|byte| *byte == b'='))
                        && bytes.get(at + 1 + equals) == Some(&b']')
                    {
                        at += equals + 2;
                        closed = true;
                        break;
                    }
                    at += 1;
                }
                if !closed {
                    return Ok(false);
                }
                continue;
            }
        }
        while bytes.get(at).is_some_and(|byte| *byte != b'\n') {
            cancel.check()?;
            at += 1;
        }
    }
    Ok(true)
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct LocalTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl LocalTargets {
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        bytes: &[u8],
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut output = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let mut roots = facts
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Root);
        let mut modules = facts
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Module);
        let (Some(root), Some(module)) = (roots.next(), modules.next()) else {
            return Ok(output);
        };
        if roots.next().is_some()
            || modules.next().is_some()
            || root.parent().is_some()
            || root.syntax_kind().as_str() != "cmake.file.root"
            || module.syntax_kind().as_str() != "cmake.file.module"
            || module.parent() != Some(root.local_id())
            || root.span() != source.span()
            || module.span() != source.span()
        {
            return Ok(output);
        }
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(output);
        };
        if module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "cmake"
            || module_entity.direct_parent != ContainerRef::File(source.span().file())
            || module_entity.record.evidence.source.as_ref() != Some(source)
        {
            return Ok(output);
        }
        let mut contexts = facts
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "cmake.binding_context.expression");
        let Some(context) = contexts.next() else {
            return Ok(output);
        };
        if contexts.next().is_some()
            || context.span() != source.span()
            || context.parent() != Some(module.local_id())
        {
            return Ok(output);
        }
        // Written overrides invalidate the builtin semantics used by this proof.
        if entities.values().any(|entity| {
            entity.record.language == "cmake"
                && entity.record.kind == EntityKind::Function
                && crate::query_pack::cmake_bindings::guarded_builtin(
                    entity.record.canonical_name.as_bytes(),
                )
        }) {
            return Ok(output);
        }
        let mut commands = Vec::new();
        let mut reads = Vec::new();
        let mut definitions = HashMap::new();
        let mut declarations = HashMap::new();
        let mut references = HashMap::new();
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != source.span().file() {
                return Ok(output);
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (
                    SyntaxFactKind::Signature,
                    "cmake.local_set.expression"
                    | "cmake.binding_preserves.expression"
                    | "cmake.binding_barrier.expression"
                    | "cmake.binding_values_barrier.expression",
                ) => {
                    commands
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    commands.push(fact);
                }
                (SyntaxFactKind::Signature, "cmake.local_read.expression") => {
                    reads
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    reads.push(fact);
                }
                (SyntaxFactKind::Occurrence, "cmake.name.definition") => {
                    definitions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    if definitions.insert(fact.local_id(), fact).is_some() {
                        return Ok(output);
                    }
                }
                (SyntaxFactKind::Declaration, "cmake.normal_assignment.declaration") => {
                    declarations
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    if declarations.insert(fact.local_id(), fact).is_some() {
                        return Ok(output);
                    }
                }
                (SyntaxFactKind::Occurrence, "cmake.normal_variable.reference") => {
                    references
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    references
                        .entry((fact.span(), fact.parent()))
                        .and_modify(|old| *old = None)
                        .or_insert(Some(fact));
                }
                _ => {}
            }
        }
        commands.sort_unstable_by_key(|fact| fact.span().start_byte());
        reads.sort_unstable_by_key(|fact| fact.span().start_byte());
        let mut end = 0usize;
        for command in &commands {
            let start = usize::try_from(command.span().start_byte())
                .map_err(|_| SinkError::AccountingOverflow)?;
            let Some(gap) = bytes.get(end..start) else {
                return Ok(output);
            };
            if !separators(gap, end == 0, cancel)? {
                return Ok(output);
            }
            end = usize::try_from(command.span().end_byte())
                .map_err(|_| SinkError::AccountingOverflow)?;
        }
        if !bytes
            .get(end..)
            .map(|tail| separators(tail, end == 0, cancel))
            .transpose()?
            .unwrap_or(false)
        {
            return Ok(output);
        }
        let mut writes = HashMap::new();
        for (local, entity) in entities {
            cancel.check()?;
            let Some(declaration) = declarations.get(local).copied() else {
                continue;
            };
            if declaration.parent() != Some(module.local_id()) {
                continue;
            }
            let Some(definition) = entity
                .definition_local_id
                .and_then(|id| definitions.get(&id).copied())
            else {
                continue;
            };
            let Some(name) = bytes_for(bytes, definition.span()) else {
                continue;
            };
            if entity.record.kind != EntityKind::Variable
                || entity.record.language != "cmake"
                || definition.parent() != Some(*local)
                || !crate::query_pack::cmake_bindings::variable_name(name)
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_none_or(|evidence| {
                        !same_source(evidence, source) || evidence.span() != declaration.span()
                    })
                || entity.record.canonical_name.as_bytes() != name
            {
                continue;
            }
            writes
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            let value = (definition.span(), entity.record.id);
            writes
                .entry(declaration.span())
                .and_modify(|old| *old = None)
                .or_insert(Some(value));
        }
        let mut current = HashMap::new();
        let mut read_index = 0usize;
        let mut builtin_dispatch = true;
        for command in commands {
            cancel.check()?;
            while let Some(read) = reads
                .get(read_index)
                .copied()
                .filter(|read| read.span().start_byte() < command.span().end_byte())
            {
                read_index += 1;
                if !super::span_contains(command.span(), read.span()) {
                    return Ok(Self {
                        bindings: HashMap::new(),
                        ..output
                    });
                }
                let Some(raw) = bytes_for(bytes, read.span())
                    .filter(|raw| raw.starts_with(b"${") && raw.ends_with(b"}"))
                else {
                    continue;
                };
                let Some(name) = raw
                    .get(2..raw.len().saturating_sub(1))
                    .filter(|name| crate::query_pack::cmake_bindings::variable_name(name))
                else {
                    continue;
                };
                let spelling = content_hash(name);
                let Some((definition_span, target)) = current.get(&spelling).copied() else {
                    continue;
                };
                if bytes_for(bytes, definition_span) != Some(name) {
                    continue;
                }
                let Some(span) = read
                    .span()
                    .start_byte()
                    .checked_add(2)
                    .zip(read.span().end_byte().checked_sub(1))
                    .and_then(|(start, end)| {
                        SourceSpan::new(source.span().file(), start, end).ok()
                    })
                else {
                    continue;
                };
                let Some(reference) = references.get(&(span, read.parent())).copied().flatten()
                else {
                    continue;
                };
                let Some(owner) = reference
                    .parent()
                    .and_then(|parent| entities.get(&parent))
                    .map(|entity| entity.record.id)
                else {
                    continue;
                };
                output
                    .bindings
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                if output
                    .bindings
                    .insert(
                        reference.local_id(),
                        Binding {
                            target,
                            owner,
                            span,
                            spelling,
                            provenance: module_entity.record.provenance,
                        },
                    )
                    .is_some()
                {
                    return Ok(Self {
                        bindings: HashMap::new(),
                        ..output
                    });
                }
            }
            let Some(raw) = bytes_for(bytes, command.span()) else {
                return Ok(Self {
                    bindings: HashMap::new(),
                    ..output
                });
            };
            match command.syntax_kind().as_str() {
                "cmake.local_set.expression" => {
                    if !builtin_dispatch {
                        continue;
                    }
                    let Some(name) =
                        crate::query_pack::cmake_bindings::ordinary_set_name(raw, cancel)?
                    else {
                        return Ok(Self {
                            bindings: HashMap::new(),
                            ..output
                        });
                    };
                    let Some((definition_span, target)) =
                        writes.get(&command.span()).copied().flatten()
                    else {
                        current.clear();
                        continue;
                    };
                    if command.span().start_byte().checked_add(
                        u64::try_from(name.start).map_err(|_| SinkError::AccountingOverflow)?,
                    ) != Some(definition_span.start_byte())
                        || command.span().start_byte().checked_add(
                            u64::try_from(name.end).map_err(|_| SinkError::AccountingOverflow)?,
                        ) != Some(definition_span.end_byte())
                    {
                        return Ok(Self {
                            bindings: HashMap::new(),
                            ..output
                        });
                    }
                    let Some(name) = bytes_for(bytes, definition_span) else {
                        return Ok(output);
                    };
                    current
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    current.insert(content_hash(name), (definition_span, target));
                }
                "cmake.binding_preserves.expression" => {
                    if !crate::query_pack::cmake_bindings::preserves_bindings(raw, cancel)? {
                        return Ok(Self {
                            bindings: HashMap::new(),
                            ..output
                        });
                    }
                }
                "cmake.binding_values_barrier.expression" => current.clear(),
                _ => {
                    current.clear();
                    builtin_dispatch = false;
                }
            }
        }
        Ok(output)
    }

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
        Ok((fact.syntax_kind().as_str() == "cmake.normal_variable.reference" && fact.span() == binding.span && physical == binding.span
            && occurrence.source.span() == physical && same_source(&occurrence.source,&self.source)
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation()
            && occurrence.file == self.source.span().file() && occurrence.role == OccurrenceRole::Reference
            && occurrence.syntax_kind == "cmake.normal_variable.reference" && occurrence.enclosing == Some(binding.owner)
            && occurrence.provenance == binding.provenance && occurrence.syntactic_text_hash == binding.spelling
            && matches!(occurrence.target,OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling)
            && occurrence.evidence.source.as_ref() == Some(&occurrence.source)).then_some(binding.target))
    }
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Function,
            "cmake.function.declaration" | "cmake.macro.declaration"
        ) | (
            EntityKind::Parameter,
            "cmake.parameter.declaration" | "cmake.macro_placeholder.declaration"
        ) | (
            EntityKind::Variable,
            "cmake.normal_assignment.declaration"
                | "cmake.cache_assignment.declaration"
                | "cmake.environment_assignment.declaration"
                | "cmake.option.declaration"
        ) | (EntityKind::BuildTarget, "cmake.target.declaration")
    )
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "cmake.unproven_name.signature" => Some("cmake-declaration-name-analysis-unavailable"),
        "cmake.unproven_parameter.signature" => Some("cmake-formal-argument-analysis-unavailable"),
        "cmake.unsupported_owner.signature" => Some("cmake-complete-written-owner-unavailable"),
        "cmake.unsupported_option_arguments.signature" => {
            Some("cmake-option-written-argument-shape-unavailable")
        }
        "cmake.control_flow.signature" => {
            Some("cmake-control-flow-and-variable-analysis-unavailable")
        }
        "cmake.configure_placeholder.signature" => {
            Some("cmake-configure-substitution-and-generated-syntax-unavailable")
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if let Some(detail) = owner_gap(fact) {
        return Some((FactDomain::Entities, detail));
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "cmake.file.module") => Some((
            FactDomain::Relations,
            "cmake-variable-command-condition-macro-and-template-evaluation-unavailable",
        )),
        (
            SyntaxFactKind::Declaration,
            "cmake.macro.declaration" | "cmake.macro_placeholder.declaration",
        ) => Some((
            FactDomain::Relations,
            "cmake-macro-expansion-and-placeholder-binding-unavailable",
        )),
        (SyntaxFactKind::Declaration, "cmake.target.declaration") => Some((
            FactDomain::Relations,
            "cmake-effective-target-configuration-unavailable",
        )),
        (SyntaxFactKind::Declaration, "cmake.option.declaration") => Some((
            FactDomain::Relations,
            "cmake-option-initialization-and-effective-cache-state-unavailable",
        )),
        (SyntaxFactKind::Signature, "cmake.unset_binding.signature") => Some((
            FactDomain::Occurrences,
            "cmake-unset-variable-write-unavailable",
        )),
        (SyntaxFactKind::Signature, "cmake.dynamic_evaluation.signature") => Some((
            FactDomain::Relations,
            "cmake-generated-source-and-dynamic-command-evaluation-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "cmake.command.call" | "cmake.template_command.call") => {
            Some("cmake-command-dispatch-target-unavailable")
        }
        (
            SyntaxFactKind::Occurrence,
            "cmake.normal_variable.reference"
            | "cmake.cache_variable.reference"
            | "cmake.environment_variable.reference",
        ) => Some("cmake-variable-evaluation-and-binding-unavailable"),
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "cmake.command.call" | "cmake.template_command.call") => {
            Some(OccurrenceRole::CallSite)
        }
        (
            SyntaxFactKind::Occurrence,
            "cmake.normal_variable.reference"
            | "cmake.cache_variable.reference"
            | "cmake.environment_variable.reference",
        ) => Some(OccurrenceRole::Reference),
        (SyntaxFactKind::Import, "cmake.written_import.import") => Some(OccurrenceRole::ImportUse),
        (SyntaxFactKind::Comment, "cmake.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "cmake.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
