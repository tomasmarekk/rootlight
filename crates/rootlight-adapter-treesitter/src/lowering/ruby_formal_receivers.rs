//! Resolve Ruby formal receivers only after a fresh complete-source proof.
//! Raw ancestry, current materialized owners, physical source and provenance
//! must agree; a producer label supplies no independent binding authority.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use tree_sitter::ParseOptions;

use super::MaterializedEntity;
use crate::query_pack::ruby_formal_receivers::{MAX_SOURCE, Work, prove};

pub(super) const REFERENCE: &str = "ruby.formal_receiver.reference";
pub(super) const UNPROVEN: &str = "ruby.unproven_formal_receiver.reference";

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE
}

fn same_source(part: &SourceRef, whole: &SourceRef) -> bool {
    *part == super::source_for_span(whole, part.span())
}

struct Bound<'a> {
    parameter: &'a MaterializedEntity,
    method: &'a MaterializedEntity,
    reference: u64,
}

pub(super) struct Targets<'a> {
    bindings: HashMap<SourceSpan, Bound<'a>>,
    source: &'a SourceRef,
}

impl<'a> Targets<'a> {
    /// Join a fresh native census to unique raw facts and their current entities.
    /// Original bytes authenticate physical evidence separately from decoded CST spans.
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
        let mut work = Work(maximum);
        let mut present = false;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(empty());
            }
            if required(fact) {
                present = true;
                break;
            }
        }
        if !present {
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
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .map_err(|_| super::provider_failure("ruby-formal-consumer-grammar"))?;
        let mut interrupted = false;
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancel.check().is_err() {
                return ControlFlow::Break(());
            }
            match work.0.checked_sub(1) {
                Some(next) => {
                    work.0 = next;
                    ControlFlow::Continue(())
                }
                None => {
                    interrupted = true;
                    ControlFlow::Break(())
                }
            }
        };
        let mut input =
            |offset: usize, _: tree_sitter::Point| current.get(offset..).unwrap_or_default();
        let tree = parser.parse_with_options(
            &mut input,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        if interrupted {
            return Ok(empty());
        }
        let Some(tree) = tree else { return Ok(empty()) };
        let native = prove(tree.root_node(), current, true, &mut work, cancel)?;
        if native.is_empty() {
            return Ok(empty());
        }
        let mut ids = HashMap::new();
        let mut unique = HashMap::new();
        let mut roots = 0_usize;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(empty());
            }
            ids.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if ids.insert(fact.local_id(), ()).is_some() {
                return Ok(empty());
            }
            unique
                .entry((
                    fact.kind(),
                    fact.syntax_kind().as_str(),
                    physical(fact.span())?,
                ))
                .and_modify(|old| *old = None)
                .or_insert(Some(fact));
            if fact.kind() == SyntaxFactKind::ErrorRecovery {
                return Ok(empty());
            }
            if fact.kind() == SyntaxFactKind::Root {
                roots = roots.saturating_add(1).min(2)
            }
        }
        if roots != 1 {
            return Ok(empty());
        }
        let lookup = |kind, label, span| unique.get(&(kind, label, span)).copied().flatten();
        let native_span = |(start, end): (usize, usize)| {
            SourceSpan::new(
                source.span().file(),
                u64::try_from(start)
                    .map_err(|_| super::provider_failure("ruby-formal-native-span"))?,
                u64::try_from(end)
                    .map_err(|_| super::provider_failure("ruby-formal-native-span"))?,
            )
            .map_err(|_| super::provider_failure("ruby-formal-native-span"))
        };
        let mut output = empty();
        for (site, proof) in native {
            if !work.step(cancel)? {
                return Ok(empty());
            }
            let site = physical(native_span(site)?)?;
            let method_span = physical(native_span(proof.method)?)?;
            let method_name = physical(native_span(proof.method_name)?)?;
            let parameter_span = physical(native_span(proof.parameter)?)?;
            let header = physical(native_span(proof.header)?)?;
            let Some(reference) = lookup(SyntaxFactKind::Occurrence, REFERENCE, site) else {
                continue;
            };
            let Some(method_fact) = lookup(
                SyntaxFactKind::Declaration,
                "ruby.method.declaration",
                method_span,
            ) else {
                continue;
            };
            let Some(method_definition) = lookup(
                SyntaxFactKind::Occurrence,
                "ruby.identifier.definition",
                method_name,
            ) else {
                continue;
            };
            let Some(signature) =
                lookup(SyntaxFactKind::Signature, "ruby.method.signature", header)
            else {
                continue;
            };
            let Some(parameter_fact) = lookup(
                SyntaxFactKind::Declaration,
                "ruby.parameter.declaration",
                parameter_span,
            ) else {
                continue;
            };
            let Some(definition) = lookup(
                SyntaxFactKind::Occurrence,
                "ruby.identifier.definition",
                parameter_span,
            ) else {
                continue;
            };
            let Some(method) = entities.get(&method_fact.local_id()) else {
                continue;
            };
            let Some(parameter) = entities.get(&parameter_fact.local_id()) else {
                continue;
            };
            if method_definition.parent() != Some(method_fact.local_id())
                || method_fact.depth().checked_add(1) != Some(method_definition.depth())
                || signature.parent() != Some(method_fact.local_id())
                || signature.depth() != method_definition.depth()
                || parameter_fact.parent() != Some(method_fact.local_id())
                || parameter_fact.depth() != method_definition.depth()
                || definition.parent() != Some(parameter_fact.local_id())
                || parameter_fact.depth().checked_add(1) != Some(definition.depth())
                || method.definition_local_id != Some(method_definition.local_id())
                || parameter.definition_local_id != Some(definition.local_id())
                || method.record.kind != EntityKind::Method
                || parameter.record.kind != EntityKind::Parameter
                || method.record.language != "ruby"
                || parameter.record.language != "ruby"
                || method.record.canonical_name != text(method_definition.span())?
                || parameter.record.canonical_name != text(definition.span())?
                || parameter.record.canonical_name != text(reference.span())?
                || parameter.record.container != Some(ContainerRef::Entity(method.record.id))
                || parameter.direct_parent != ContainerRef::Entity(method.record.id)
                || parameter.record.provenance != method.record.provenance
            {
                continue;
            }
            let mut valid = true;
            for (entity, extent) in [(method, method_span), (parameter, parameter_span)] {
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
            let mut child = reference;
            let mut owner = None;
            for link in proof.links.iter().take(proof.link_count).flatten() {
                if !work.step(cancel)? {
                    return Ok(empty());
                }
                let extent = if link.label.starts_with("ruby.file.") {
                    source.span()
                } else {
                    physical(native_span(link.span)?)?
                };
                let Some(parent) = lookup(link.kind, link.label, extent) else {
                    valid = false;
                    break;
                };
                if child.parent() != Some(parent.local_id())
                    || parent.depth().checked_add(1) != Some(child.depth())
                {
                    valid = false;
                    break;
                }
                if parent.local_id() == method_fact.local_id() {
                    owner = Some(method)
                } else if owner.is_some()
                    && matches!(
                        link.kind,
                        SyntaxFactKind::Declaration | SyntaxFactKind::Module
                    )
                {
                    let Some(outer) = entities.get(&parent.local_id()) else {
                        valid = false;
                        break;
                    };
                    let expected = match link.label {
                        "ruby.namespace.declaration" => EntityKind::Namespace,
                        "ruby.class.declaration" => EntityKind::Class,
                        "ruby.file.module" => EntityKind::Module,
                        _ => {
                            valid = false;
                            break;
                        }
                    };
                    if expected == EntityKind::Module {
                        if link.name.is_some() || outer.definition_local_id.is_some() {
                            valid = false;
                            break;
                        }
                    } else {
                        let Some(name) = link.name else {
                            valid = false;
                            break;
                        };
                        let name = physical(native_span(name)?)?;
                        let Some(definition) = lookup(
                            SyntaxFactKind::Occurrence,
                            "ruby.identifier.definition",
                            name,
                        ) else {
                            valid = false;
                            break;
                        };
                        let written = text(definition.span())?;
                        for _ in written.as_bytes().chunks(64) {
                            if !work.step(cancel)? {
                                return Ok(empty());
                            }
                        }
                        if definition.parent() != Some(parent.local_id())
                            || parent.depth().checked_add(1) != Some(definition.depth())
                            || outer.definition_local_id != Some(definition.local_id())
                            || outer.record.canonical_name != written
                        {
                            valid = false;
                            break;
                        }
                    }
                    let inner = owner
                        .take()
                        .expect("the branch requires a current lexical owner");
                    if outer.record.kind != expected
                        || outer.record.language != "ruby"
                        || outer.record.repository != source.repository()
                        || outer.record.generation != source.generation()
                        || outer.record.provenance != method.record.provenance
                        || outer
                            .record
                            .evidence
                            .source
                            .as_ref()
                            .is_none_or(|part| part.span() != extent || !same_source(part, source))
                        || inner.direct_parent != ContainerRef::Entity(outer.record.id)
                        || inner.record.container != Some(ContainerRef::Entity(outer.record.id))
                    {
                        valid = false;
                        break;
                    }
                    owner = Some(outer);
                }
                child = parent;
            }
            let Some(file_owner) = owner else { continue };
            if !valid
                || child.kind() != SyntaxFactKind::Root
                || child.parent().is_some()
                || child.depth() != 0
                || file_owner.record.kind != EntityKind::Module
                || file_owner.direct_parent != ContainerRef::File(source.span().file())
                || file_owner.record.container != Some(ContainerRef::File(source.span().file()))
            {
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
                    method,
                    reference: reference.local_id(),
                },
            );
        }
        Ok(output)
    }

    /// Select the proven callable owner rather than an initializer's local variable.
    pub(super) fn owner(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
    ) -> Option<&'a MaterializedEntity> {
        self.bindings
            .get(&span)
            .filter(|bound| required(fact) && bound.reference == fact.local_id())
            .map(|bound| bound.method)
    }

    /// Resolve an exact current-source Reference; refused certificates stay unresolved.
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
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.syntax_kind != REFERENCE
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || owner.is_none_or(|entity| entity.record.id != bound.method.record.id)
            || occurrence.enclosing != Some(bound.method.record.id)
            || occurrence.provenance != bound.method.record.provenance
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
