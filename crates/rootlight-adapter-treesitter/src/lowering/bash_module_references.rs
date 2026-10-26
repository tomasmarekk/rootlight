//! Reconstruct module Reference binding from a separate current-source parse.
//! Exact native names/assignments, structural facts, materialized module ownership
//! and physical source joins are required; producer labels alone prove nothing.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, SourceRef, SourceSpan,
};
use tree_sitter::{ParseOptions, Tree};

use super::{MaterializedEntity, provider_failure, source_for_span};
use crate::bash_module_references::{self as native, Work};

pub(super) const PROVEN: &str = "bash.module_reference.reference";
pub(super) const UNPROVEN: &str = "bash.unproven_module_reference.reference";

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(fact.syntax_kind().as_str(), PROVEN | UNPROVEN)
}

fn limit() -> AdapterError {
    provider_failure("bash-reference-consumer-work-limit")
}

fn step(work: &mut Work, cancel: &Cancellation) -> Result<(), AdapterError> {
    if work.step(cancel)? {
        Ok(())
    } else {
        Err(limit())
    }
}

fn range(span: SourceSpan) -> Option<(usize, usize)> {
    Some((
        usize::try_from(span.start_byte()).ok()?,
        usize::try_from(span.end_byte()).ok()?,
    ))
}

fn unique<'a>(
    facts: &'a [SyntaxFact],
    work: &mut Work,
    cancel: &Cancellation,
    predicate: impl Fn(&SyntaxFact) -> bool,
) -> Result<Option<&'a SyntaxFact>, AdapterError> {
    let mut found = None;
    for fact in facts {
        step(work, cancel)?;
        if predicate(fact) && found.replace(fact).is_some() {
            return Ok(None);
        }
    }
    if let Some(found) = found {
        let mut identities = 0usize;
        for fact in facts {
            step(work, cancel)?;
            identities += usize::from(fact.local_id() == found.local_id());
        }
        if identities != 1 {
            return Ok(None);
        }
    }
    Ok(found)
}

/// Syntax facts and entities materialized by the same lowering pass.
pub(super) struct BindingFacts<'a> {
    pub(super) syntax: &'a [SyntaxFact],
    pub(super) entities: &'a HashMap<u64, MaterializedEntity>,
}

pub(super) struct Targets<'a> {
    tree: Tree,
    source: &'a [u8],
    full_source: &'a SourceRef,
    facts: &'a [SyntaxFact],
    entities: &'a HashMap<u64, MaterializedEntity>,
    work: Work,
}

impl<'a> Targets<'a> {
    /// Parse current complete bytes only when a positive label needs requalification.
    /// The same counter pays for fact selection, parse progress, census and joins.
    pub(super) fn new(
        source: &'a [u8],
        physical: &[u8],
        full_source: &'a SourceRef,
        bindings: BindingFacts<'a>,
        maximum: usize,
        complete_input: bool,
        cancel: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        cancel.check()?;
        let BindingFacts {
            syntax: facts,
            entities,
        } = bindings;
        if !complete_input
            || full_source.span().start_byte() != 0
            || usize::try_from(full_source.span().end_byte()).ok() != Some(physical.len())
        {
            return Ok(None);
        }
        let mut work = Work::new(maximum);
        let mut positive = false;
        for fact in facts {
            step(&mut work, cancel)?;
            positive |=
                fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == PROVEN;
        }
        if !positive {
            return Ok(None);
        }
        for _ in physical.chunks(64) {
            step(&mut work, cancel)?
        }
        if content_hash(physical) != full_source.content_hash() {
            return Ok(None);
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .map_err(|_| provider_failure("bash-reference-consumer-grammar"))?;
        let tree = {
            let mut progress = |_: &tree_sitter::ParseState| {
                if work.step(cancel).unwrap_or(false) {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                }
            };
            let mut input =
                |offset: usize, _: tree_sitter::Point| source.get(offset..).unwrap_or_default();
            parser.parse_with_options(
                &mut input,
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
        };
        cancel.check()?;
        if work.exhausted() {
            return Err(limit());
        }
        let Some(tree) = tree else { return Ok(None) };
        if !matches!(tree.included_ranges().as_slice(), [range]
            if range.start_byte == 0 && range.end_byte >= source.len())
            || tree.root_node().kind() != "program"
            || tree.root_node().has_error()
        {
            return Ok(None);
        }
        Ok(Some(Self {
            tree,
            source,
            full_source,
            facts,
            entities,
            work,
        }))
    }

    /// Return only a source-owned, unique module variable after all current joins.
    /// Unavailable proof retains uncertainty; cancellation/allocation/work errors propagate.
    pub(super) fn resolve(
        &mut self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        step(&mut self.work, cancel)?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != PROVEN
            || site.role != OccurrenceRole::Reference
            || site.syntax_kind != PROVEN
            || site.repository != self.full_source.repository()
            || site.generation != self.full_source.generation()
            || site.file != self.full_source.span().file()
            || fact.span().file() != self.full_source.span().file()
            || site.source != source_for_span(self.full_source, physical(fact.span())?)
        {
            return Ok(None);
        }
        // A repeated source certificate must not create identity authority even
        // when both copies independently describe the same clean native leaf.
        let Some(certificate) = unique(self.facts, &mut self.work, cancel, |candidate| {
            candidate.kind() == SyntaxFactKind::Occurrence
                && candidate.syntax_kind().as_str() == PROVEN
                && candidate.span() == fact.span()
        })?
        else {
            return Ok(None);
        };
        if certificate != fact {
            return Ok(None);
        }
        let Some((start, end)) = range(fact.span()) else {
            return Ok(None);
        };
        let root = self.tree.root_node();
        let Some(node) = root
            .descendant_for_byte_range(start, end)
            .filter(|node| node.kind() == "variable_name" && node.byte_range() == (start..end))
        else {
            return Ok(None);
        };
        let proven = native::prove(root, node, self.source, true, &mut self.work, cancel)?;
        if self.work.exhausted() {
            return Err(limit());
        }
        let Some(proven) = proven else {
            return Ok(None);
        };
        let Some(name) = self
            .source
            .get(start..end)
            .and_then(|name| std::str::from_utf8(name).ok())
        else {
            return Ok(None);
        };
        if site.syntactic_text_hash != content_hash(name.as_bytes()) {
            return Ok(None);
        }
        let all = self.facts;
        let work = &mut self.work;
        let full = (0, self.source.len());
        let Some(root_fact) = unique(all, work, cancel, |candidate| {
            candidate.kind() == SyntaxFactKind::Root
                && candidate.syntax_kind().as_str() == "bash.file.root"
                && candidate.span().file() == site.file
                && range(candidate.span()) == Some(full)
                && candidate.parent().is_none()
                && candidate.depth() == 0
        })?
        else {
            return Ok(None);
        };
        let Some(module) = unique(all, work, cancel, |candidate| {
            candidate.kind() == SyntaxFactKind::Module
                && candidate.syntax_kind().as_str() == "bash.file.module"
                && candidate.span() == root_fact.span()
                && candidate.parent() == Some(root_fact.local_id())
                && candidate.depth() == 1
        })?
        else {
            return Ok(None);
        };
        let Some(scope) = unique(all, work, cancel, |candidate| {
            candidate.kind() == SyntaxFactKind::Scope
                && candidate.syntax_kind().as_str() == "bash.file.scope"
                && candidate.span() == root_fact.span()
                && candidate.parent() == Some(module.local_id())
                && candidate.depth() == 2
        })?
        else {
            return Ok(None);
        };
        if fact.parent() != Some(scope.local_id()) || fact.depth() != 3 {
            return Ok(None);
        }
        let Some(declaration) = unique(all, work, cancel, |candidate| {
            candidate.kind() == SyntaxFactKind::Declaration
                && candidate.syntax_kind().as_str() == "bash.variable.declaration"
                && candidate.span().file() == site.file
                && range(candidate.span()) == Some(proven.assignment)
                && candidate.parent() == Some(scope.local_id())
                && candidate.depth() == 3
        })?
        else {
            return Ok(None);
        };
        let Some(definition) = unique(all, work, cancel, |candidate| {
            candidate.kind() == SyntaxFactKind::Occurrence
                && candidate.syntax_kind().as_str() == "bash.variable.definition"
                && candidate.span().file() == site.file
                && range(candidate.span()) == Some(proven.definition)
                && candidate.parent() == Some(declaration.local_id())
                && candidate.depth() == 4
        })?
        else {
            return Ok(None);
        };
        let Some(module_entity) = self.entities.get(&module.local_id()) else {
            return Ok(None);
        };
        let Some(target) = self.entities.get(&declaration.local_id()) else {
            return Ok(None);
        };
        if module_entity.record.language != "bash"
            || module_entity.record.kind != EntityKind::Module
            || module_entity.record.repository != self.full_source.repository()
            || module_entity.record.generation != self.full_source.generation()
            || module_entity.record.evidence.source.as_ref() != Some(self.full_source)
            || module_entity.direct_parent != ContainerRef::File(site.file)
            || module_entity.record.container != Some(module_entity.direct_parent)
            || target.record.language != "bash"
            || target.record.kind != EntityKind::Variable
            || target.record.canonical_name != name
            || target.definition_local_id != Some(definition.local_id())
            || target.direct_parent != ContainerRef::Entity(module_entity.record.id)
            || target.record.container != Some(target.direct_parent)
            || target.record.repository != self.full_source.repository()
            || target.record.generation != self.full_source.generation()
            || target.record.provenance != module_entity.record.provenance
            || target.record.provenance != site.provenance
            || site.enclosing != Some(module_entity.record.id)
            || target.record.evidence.source.as_ref()
                != Some(&source_for_span(
                    self.full_source,
                    physical(declaration.span())?,
                ))
            || physical(definition.span())?.file() != site.file
        {
            return Ok(None);
        }
        let mut names = 0usize;
        let mut target_ids = 0usize;
        let mut module_ids = 0usize;
        for entity in self.entities.values() {
            step(&mut self.work, cancel)?;
            names += usize::from(
                entity.record.language == "bash"
                    && entity.record.kind == EntityKind::Variable
                    && entity.record.canonical_name == name
                    && entity.direct_parent == ContainerRef::Entity(module_entity.record.id),
            );
            target_ids += usize::from(entity.record.id == target.record.id);
            module_ids += usize::from(entity.record.id == module_entity.record.id);
        }
        Ok((names == 1 && target_ids == 1 && module_ids == 1).then_some(target.record.id))
    }
}

#[cfg(test)]
mod tests;
