//! Join Ruby receiver certificates to independently reconstructed current source.
//! Fresh native geometry, unique raw ancestry, and materialized declaration
//! evidence must agree before a written receiver gains a lexical target.

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
use crate::query_pack::ruby_bindings::{MAX_SOURCE, Work, prove};

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "ruby.lexical_receiver.reference"
}

fn same_source(part: &SourceRef, whole: &SourceRef) -> bool {
    *part == super::source_for_span(whole, part.span())
}

struct Bound<'a> {
    variable: &'a MaterializedEntity,
    owner: &'a MaterializedEntity,
}

pub(super) struct ReceiverTargets<'a> {
    bindings: HashMap<SourceSpan, Bound<'a>>,
    source: &'a SourceRef,
}

impl<'a> ReceiverTargets<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "decoded and original physical source are separately authenticated"
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
        let mut output = Self {
            bindings: HashMap::new(),
            source,
        };
        cancel.check()?;
        if maximum == 0
            || current.len() > MAX_SOURCE
            || original.len() > MAX_SOURCE.saturating_mul(4)
            || source.span().start_byte() != 0
            || u64::try_from(original.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(output);
        }
        let mut work = Work(maximum);
        let mut present = false;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(output);
            }
            if required(fact) {
                present = true;
                break;
            }
        }
        if !present {
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
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .map_err(|_| super::provider_failure("ruby-receiver-consumer-grammar"))?;
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
            return Ok(output);
        }
        let Some(tree) = tree else { return Ok(output) };
        let native = prove(tree.root_node(), current, true, &mut work, cancel)?;
        if native.is_empty() {
            return Ok(output);
        }

        let mut by_id = HashMap::new();
        let mut unique = HashMap::new();
        let mut roots = 0_usize;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(output);
            }
            by_id
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if by_id.insert(fact.local_id(), ()).is_some() {
                return Ok(output);
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
                return Ok(output);
            }
            if fact.kind() == SyntaxFactKind::Root {
                roots = roots.saturating_add(1).min(2)
            }
        }
        let lookup = |kind, label, span| unique.get(&(kind, label, span)).copied().flatten();
        let Some(root) = lookup(SyntaxFactKind::Root, "ruby.file.root", source.span()) else {
            return Ok(output);
        };
        let Some(module) = lookup(SyntaxFactKind::Module, "ruby.file.module", source.span()) else {
            return Ok(output);
        };
        let Some(file_scope) = lookup(SyntaxFactKind::Scope, "ruby.file.scope", source.span())
        else {
            return Ok(output);
        };
        let Some(context) = lookup(
            SyntaxFactKind::Signature,
            "ruby.receiver_file_context.signature",
            source.span(),
        ) else {
            return Ok(output);
        };
        if root.parent().is_some()
            || root.depth() != 0
            || module.parent() != Some(root.local_id())
            || module.depth() != 1
            || file_scope.parent() != Some(module.local_id())
            || file_scope.depth() != 2
            || roots != 1
            || context.parent() != Some(file_scope.local_id())
            || context.depth() != 3
        {
            return Ok(output);
        }
        let Some(owner) = entities.get(&module.local_id()) else {
            return Ok(output);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(output);
        };
        if owner.record.kind != EntityKind::Module
            || owner.record.language != "ruby"
            || owner_source != source
            || owner.record.container != Some(ContainerRef::File(source.span().file()))
            || owner.direct_parent != ContainerRef::File(source.span().file())
            || owner.record.repository != source.repository()
            || owner.record.generation != source.generation()
        {
            return Ok(output);
        }
        let native_span = |(start, end): (usize, usize)| {
            SourceSpan::new(
                source.span().file(),
                u64::try_from(start)
                    .map_err(|_| super::provider_failure("ruby-receiver-native-span"))?,
                u64::try_from(end)
                    .map_err(|_| super::provider_failure("ruby-receiver-native-span"))?,
            )
            .map_err(|_| super::provider_failure("ruby-receiver-native-span"))
        };
        let expected_bindings = native.len();
        for (site, proof) in native {
            if !work.step(cancel)? {
                return Ok(Self {
                    bindings: HashMap::new(),
                    source,
                });
            }
            let site = physical(native_span(site)?)?;
            let declaration = physical(native_span(proof.declaration)?)?;
            let name = physical(native_span(proof.name)?)?;
            let window = SourceSpan::new(
                source.span().file(),
                declaration.start_byte(),
                site.end_byte(),
            )
            .map_err(|_| super::provider_failure("ruby-receiver-target-window"))?;
            let Some(reference) = lookup(
                SyntaxFactKind::Occurrence,
                "ruby.lexical_receiver.reference",
                site,
            ) else {
                continue;
            };
            let Some(raw_declaration) = lookup(
                SyntaxFactKind::Declaration,
                "ruby.variable.declaration",
                declaration,
            ) else {
                continue;
            };
            let Some(definition) = lookup(
                SyntaxFactKind::Occurrence,
                "ruby.identifier.definition",
                name,
            ) else {
                continue;
            };
            let Some(variable) = entities.get(&raw_declaration.local_id()) else {
                continue;
            };
            let Some(variable_source) = variable.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(target_window) = lookup(
                SyntaxFactKind::Signature,
                "ruby.receiver_target.signature",
                window,
            ) else {
                continue;
            };
            if target_window.parent() != Some(file_scope.local_id())
                || target_window.depth() != 3
                || raw_declaration.parent() != Some(file_scope.local_id())
                || raw_declaration.depth() != 3
                || definition.parent() != Some(raw_declaration.local_id())
                || definition.depth() != 4
                || variable.definition_local_id != Some(definition.local_id())
                || variable.record.kind != EntityKind::Variable
                || variable.record.language != "ruby"
                || variable_source.span() != declaration
                || !same_source(variable_source, source)
                || variable.record.canonical_name != text(definition.span())?
                || variable.record.canonical_name != text(reference.span())?
                || variable.record.container != Some(ContainerRef::Entity(owner.record.id))
                || variable.direct_parent != ContainerRef::Entity(owner.record.id)
                || variable.record.provenance != owner.record.provenance
                || variable.record.repository != source.repository()
                || variable.record.generation != source.generation()
            {
                continue;
            }
            let mut child = reference;
            let mut valid = proof.scope_count > 0;
            for (index, scope) in proof
                .scopes
                .iter()
                .take(proof.scope_count)
                .flatten()
                .enumerate()
            {
                if !work.step(cancel)? {
                    return Ok(Self {
                        bindings: HashMap::new(),
                        source,
                    });
                }
                let file = index + 1 == proof.scope_count;
                let scope_span = if file {
                    source.span()
                } else {
                    physical(native_span(*scope)?)?
                };
                let label = if file {
                    "ruby.file.scope"
                } else {
                    "ruby.block.scope"
                };
                let Some(parent) = lookup(SyntaxFactKind::Scope, label, scope_span) else {
                    valid = false;
                    break;
                };
                if child.parent() != Some(parent.local_id())
                    || parent.depth().checked_add(1) != Some(child.depth())
                {
                    valid = false;
                    break;
                }
                child = parent;
            }
            if !valid || child.local_id() != file_scope.local_id() {
                continue;
            }
            output
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            output.bindings.insert(site, Bound { variable, owner });
        }
        // The file context certifies one complete census, so no failed site may
        // leave unrelated receivers with a partially admitted certificate.
        if output.bindings.len() != expected_bindings {
            return Ok(Self {
                bindings: HashMap::new(),
                source,
            });
        }
        Ok(output)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        written: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(bound) = self.bindings.get(&span) else {
            return Ok(None);
        };
        if !required(fact)
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || owner.is_none_or(|owner| owner.record.id != bound.owner.record.id)
            || occurrence.enclosing != Some(bound.owner.record.id)
            || occurrence.provenance != bound.owner.record.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span() != span
            || !same_source(&occurrence.source, self.source)
            || occurrence.syntactic_text_hash != content_hash(written.as_bytes())
            || written != bound.variable.record.canonical_name
        {
            return Ok(None);
        }
        Ok(Some(bound.variable.record.id))
    }
}
