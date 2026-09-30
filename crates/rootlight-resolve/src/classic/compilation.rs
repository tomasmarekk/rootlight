//! Conditional activity gates over verified, immutable classic BASIC sources.
//! Unknown declarations remain lookup barriers; only excluded declarations are
//! removed from inventories, and existing parser-owned facts are never rewritten.

use super::*;
use rootlight_ir::{
    ExtensionEnvelope, SourceSpan, VBA_COMPILATION_NAMESPACE, VbaCompilation,
    VbaCompilationActivity, decode_vba_compilation_envelope,
};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct CompilationIndex {
    files: BTreeMap<FileId, FileActivity>,
    declarations: BTreeMap<SymbolId, VbaCompilationActivity>,
    uses: BTreeSet<FactId>,
    declaration_sources: BTreeSet<SourceRef>,
}

struct FileActivity {
    source: SourceRef,
    conditional: bool,
    blocked: bool,
    inventories: Vec<(FactId, VbaCompilation)>,
}

impl CompilationIndex {
    pub(super) fn build(
        index: &CandidateIndex<'_>,
        extensions: &[ExtensionEnvelope],
        occurrences: &[OccurrenceRecord],
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let mut result = Self::default();
        for (&id, bytes) in sources {
            cancellation.check()?;
            work.consume()?;
            let Some(file) = index
                .files
                .get(&id)
                .filter(|file| matches!(file.language.as_str(), "vba" | "visual-basic-classic"))
            else {
                continue;
            };
            let Some(source) = file.evidence.source.as_ref().filter(|source| {
                source.repository() == file.repository
                    && source.generation() == file.generation
                    && source.span().file() == id
                    && source.content_hash() == file.content_hash
                    && source.span().start_byte() == 0
                    && source.span().end_byte() == file.byte_length
            }) else {
                continue;
            };
            let mut conditional = false;
            for line in bytes.split(|byte| *byte == b'\n') {
                cancellation.check()?;
                work.consume_many(line.len().div_ceil(64).max(1))?;
                let line = line.trim_ascii_start();
                if let Some(directive) = line.strip_prefix(b"#") {
                    let word = directive
                        .trim_ascii_start()
                        .split(u8::is_ascii_whitespace)
                        .next()
                        .unwrap_or_default();
                    // #Const names are compile-time-only declarations, not
                    // conditional runtime membership. Other missing directives
                    // cannot be silently interpreted as an ordinary file.
                    conditional |= !word.eq_ignore_ascii_case(b"Const");
                }
            }
            result.files.insert(
                id,
                FileActivity {
                    source: source.clone(),
                    conditional,
                    blocked: false,
                    inventories: Vec::new(),
                },
            );
        }
        for envelope in extensions
            .iter()
            .filter(|envelope| envelope.namespace == VBA_COMPILATION_NAMESPACE)
        {
            cancellation.check()?;
            work.consume()?;
            let Some(source) = &envelope.evidence.source else {
                for file in result.files.values_mut() {
                    file.blocked = true;
                }
                continue;
            };
            let Some(file) = result.files.get_mut(&source.span().file()) else {
                continue;
            };
            work.consume_many(envelope.payload.len().div_ceil(64).max(1))?;
            let compilation = decode_vba_compilation_envelope(envelope);
            let parser = index.provenance.get(&envelope.provenance);
            if let Some(parser) = parser {
                work.consume_many(parser.input_sources.len())?;
            }
            let valid = same_identity(&file.source, source)
                && contains(file.source.span(), source.span())
                && parser.is_some_and(|parser| {
                    parser.repository == source.repository()
                        && parser.generation == source.generation()
                        && parser.producer_kind == ProducerKind::Parser
                        && index
                            .files
                            .get(&source.span().file())
                            .is_some_and(|file| parser.language == file.language)
                        && parser.input_sources.iter().any(|input| {
                            same_identity(input, source) && contains(input.span(), source.span())
                        })
                });
            let Ok(compilation) = compilation else {
                file.blocked = true;
                continue;
            };
            work.consume_many(compilation.branches.len().max(1))?;
            if !valid
                || file
                    .inventories
                    .iter()
                    .any(|(_, prior)| overlaps(prior.scope, compilation.scope))
            {
                file.blocked = true;
                continue;
            }
            file.inventories.push((envelope.id, compilation));
        }
        let mut definitions: BTreeMap<SymbolId, Option<&SourceRef>> = BTreeMap::new();
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume()?;
            if occurrence.role == OccurrenceRole::Definition
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                definitions
                    .entry(symbol)
                    .and_modify(|prior| {
                        if *prior != Some(&occurrence.source) {
                            *prior = None;
                        }
                    })
                    .or_insert(Some(&occurrence.source));
            }
        }
        for entity in index.entities.values() {
            cancellation.check()?;
            work.consume()?;
            let activity = if !matches!(entity.language.as_str(), "vba" | "visual-basic-classic") {
                VbaCompilationActivity::Active
            } else if let Some(source) = &entity.evidence.source {
                let declaration = match definitions.get(&entity.id) {
                    Some(Some(definition))
                        if same_identity(source, definition)
                            && contains(source.span(), definition.span()) =>
                    {
                        *definition
                    }
                    Some(_) => {
                        result
                            .declarations
                            .insert(entity.id, VbaCompilationActivity::Unproven);
                        continue;
                    }
                    None => source,
                };
                work.consume_many(result.source_cost(declaration))?;
                let activity = result.activity(declaration);
                if activity == VbaCompilationActivity::Unproven
                    && matches!(entity.kind, EntityKind::Module | EntityKind::Class)
                    && !matches!(entity.container, Some(ContainerRef::Entity(_)))
                    && result.module_scope_complete(source)
                {
                    // A source-written module is the lexical container of its
                    // groups, not a declaration selected by an inner branch.
                    VbaCompilationActivity::Active
                } else if activity == VbaCompilationActivity::Unproven
                    && matches!(
                        entity.kind,
                        EntityKind::Function | EntityKind::Method | EntityKind::Property
                    )
                {
                    let bytes = sources.get(&source.span().file()).and_then(|bytes| {
                        bytes.get(
                            usize::try_from(source.span().start_byte()).ok()?
                                ..usize::try_from(source.span().end_byte()).ok()?,
                        )
                    });
                    let header = if let Some(bytes) = bytes {
                        rootlight_vba::procedure_header(bytes, &mut || {
                            cancellation.check()?;
                            work.consume()
                        })?
                    } else {
                        None
                    };
                    if let Some(header) = header.filter(|header| {
                        header
                            .name()
                            .eq_ignore_ascii_case(entity.canonical_name.as_bytes())
                    }) {
                        let name = header.name_span();
                        let start = source
                            .span()
                            .start_byte()
                            .checked_add(
                                u64::try_from(name.start)
                                    .map_err(|_| ResolutionError::CountOverflow)?,
                            )
                            .ok_or(ResolutionError::CountOverflow)?;
                        let end = source
                            .span()
                            .start_byte()
                            .checked_add(
                                u64::try_from(name.end)
                                    .map_err(|_| ResolutionError::CountOverflow)?,
                            )
                            .ok_or(ResolutionError::CountOverflow)?;
                        result.activity(&SourceRef::new(
                            source.repository(),
                            source.generation(),
                            SourceSpan::new(source.span().file(), start, end)
                                .map_err(ResolutionError::InvalidProducer)?,
                            source.content_hash(),
                            None,
                        ))
                    } else {
                        activity
                    }
                } else {
                    activity
                }
            } else if index
                .provenance
                .get(&entity.provenance)
                .is_some_and(|parser| parser.rule.as_deref() == Some(standard::CATALOG_RULE))
            {
                VbaCompilationActivity::Active
            } else {
                VbaCompilationActivity::Unproven
            };
            result.declarations.insert(entity.id, activity);
        }
        // A child cannot acquire membership excluded or unknown at an owner.
        // The bounded parent walk also fails closed on malformed cycles.
        let own = result.declarations.clone();
        for entity in index.entities.values() {
            cancellation.check()?;
            work.consume()?;
            let mut activity = own[&entity.id];
            let mut owner = entity.container;
            let mut remaining = index.entities.len();
            while let Some(ContainerRef::Entity(id)) = owner {
                cancellation.check()?;
                work.consume()?;
                if remaining == 0 {
                    activity = VbaCompilationActivity::Unproven;
                    break;
                }
                remaining -= 1;
                let Some(parent) = index.entities.get(&id) else {
                    activity = VbaCompilationActivity::Unproven;
                    break;
                };
                activity = combine(activity, own[&id]);
                owner = parent.container;
            }
            result.declarations.insert(entity.id, activity);
            if activity == VbaCompilationActivity::Active
                && let Some(source) = &entity.evidence.source
            {
                result.declaration_sources.insert(source.clone());
            }
        }
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume_many(result.source_cost(&occurrence.source))?;
            if result.activity(&occurrence.source) == VbaCompilationActivity::Active
                && occurrence
                    .enclosing
                    .is_none_or(|id| result.entity_active(id))
                && match occurrence.target {
                    OccurrenceTarget::Resolved { symbol } => result.entity_active(symbol),
                    _ => true,
                }
            {
                result.uses.insert(occurrence.id);
            }
        }
        Ok(result)
    }

    pub(super) fn entity_active(&self, id: SymbolId) -> bool {
        self.declarations.get(&id) == Some(&VbaCompilationActivity::Active)
    }

    pub(super) fn entity_inactive(&self, id: SymbolId) -> bool {
        self.declarations.get(&id) == Some(&VbaCompilationActivity::Inactive)
    }

    pub(super) fn use_active(&self, id: FactId) -> bool {
        self.uses.contains(&id)
    }

    pub(super) fn support_active(&self, source: &SourceRef) -> bool {
        // A full declaration is retained as source.read context; its runtime
        // membership is proved by the definition token, not every body byte.
        self.declaration_sources.contains(source)
            || self.activity(source) == VbaCompilationActivity::Active
    }

    pub(super) fn activity(&self, source: &SourceRef) -> VbaCompilationActivity {
        let Some(file) = self.files.get(&source.span().file()) else {
            return VbaCompilationActivity::Unproven;
        };
        if file.blocked
            || !same_identity(&file.source, source)
            || !contains(file.source.span(), source.span())
        {
            return VbaCompilationActivity::Unproven;
        }
        if let Some((_, model)) = file
            .inventories
            .iter()
            .find(|(_, model)| contains(model.scope, source.span()))
        {
            return model.activity_at(source.span());
        }
        if file.conditional
            || file
                .inventories
                .iter()
                .any(|(_, model)| overlaps(model.scope, source.span()))
        {
            VbaCompilationActivity::Unproven
        } else {
            VbaCompilationActivity::Active
        }
    }

    pub(super) fn source_cost(&self, source: &SourceRef) -> usize {
        self.files.get(&source.span().file()).map_or(1, |file| {
            file.inventories
                .iter()
                .map(|(_, model)| model.branches.len().max(1))
                .sum::<usize>()
                .max(1)
        })
    }

    fn module_scope_complete(&self, source: &SourceRef) -> bool {
        self.files.get(&source.span().file()).is_some_and(|file| {
            !file.blocked
                && same_identity(&file.source, source)
                && file
                    .inventories
                    .iter()
                    .any(|(_, model)| model.complete && model.scope == source.span())
        })
    }

    pub(super) fn evidence(&self, file: FileId) -> impl Iterator<Item = FactRef> + '_ {
        self.files
            .get(&file)
            .into_iter()
            .filter(|file| !file.blocked)
            .flat_map(|file| file.inventories.iter().map(|(id, _)| FactRef::Fact(*id)))
    }
}

fn same_identity(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && inner.start_byte() < inner.end_byte()
        && outer.start_byte() <= inner.start_byte()
        && inner.end_byte() <= outer.end_byte()
}

fn overlaps(left: SourceSpan, right: SourceSpan) -> bool {
    left.file() == right.file()
        && left.start_byte() < right.end_byte()
        && right.start_byte() < left.end_byte()
}

fn combine(left: VbaCompilationActivity, right: VbaCompilationActivity) -> VbaCompilationActivity {
    match (left, right) {
        (VbaCompilationActivity::Inactive, _) | (_, VbaCompilationActivity::Inactive) => {
            VbaCompilationActivity::Inactive
        }
        (VbaCompilationActivity::Active, VbaCompilationActivity::Active) => {
            VbaCompilationActivity::Active
        }
        _ => VbaCompilationActivity::Unproven,
    }
}
