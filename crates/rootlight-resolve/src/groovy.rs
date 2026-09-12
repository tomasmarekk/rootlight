//! Explicit Groovy type imports over source-owned package/type declarations.
//! Links identify declarations in the admitted snapshot, not classpath selection
//! or dynamic dispatch; incomplete inventories and duplicate names veto promotion.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, EntityKind, EntityRecord,
    FactDomain, FileRecord, IrLimits, OccurrenceRecord, OccurrenceRole, ProducerKind,
    ProvenanceRecord,
};

use crate::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionOutcome, ResolutionRule, ResolutionSignal,
};

struct Context<'a> {
    file: &'a FileRecord,
    coverage: [&'a CoverageRecord; 2],
    retained_entities: u64,
}

struct Declaration<'a> {
    entity: &'a EntityRecord,
    owners: Vec<&'a EntityRecord>,
}

pub(crate) struct Proof<'a, 'index> {
    pub(crate) target: &'a EntityRecord,
    pub(crate) owners: &'index [&'a EntityRecord],
    pub(crate) coverage: [&'a CoverageRecord; 4],
    pub(crate) binding: Option<&'a rootlight_ir::ExtensionEnvelope>,
}

#[derive(Default)]
pub(crate) struct GroovyIndex<'a> {
    contexts: BTreeMap<FileId, Context<'a>>,
    declarations: BTreeMap<ContentHash, Option<Declaration<'a>>>,
    unavailable: bool,
    uses: BTreeMap<
        rootlight_ir::SourceSpan,
        Option<(
            &'a rootlight_ir::ExtensionEnvelope,
            rootlight_ir::GroovyImportedType,
        )>,
    >,
}

impl<'a> GroovyIndex<'a> {
    pub(crate) fn build(
        files: &BTreeMap<FileId, &'a FileRecord>,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        provenance: &BTreeMap<FactId, &'a ProvenanceRecord>,
        coverage: &'a [CoverageRecord],
        extensions: &'a [rootlight_ir::ExtensionEnvelope],
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        cancellation.check()?;
        let mut index = Self::default();
        if !files.values().any(|file| file.language == "groovy") {
            return Ok(index);
        }
        let mut domains = BTreeMap::new();
        for record in coverage {
            cancellation.check()?;
            if let CoverageScope::File(file) = record.scope
                && matches!(record.domain, FactDomain::Files | FactDomain::Entities)
            {
                domains
                    .entry((file, record.domain))
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(record));
            }
        }
        for file in files
            .values()
            .copied()
            .filter(|file| file.language == "groovy")
        {
            cancellation.check()?;
            let complete = |domain| {
                domains
                    .get(&(file.id, domain))
                    .copied()
                    .flatten()
                    .filter(|record| {
                        record.status == CoverageStatus::Complete
                            && record.skipped == 0
                            && record.discovered == record.indexed
                            && record.provenance == file.provenance
                            && record.evidence.source.is_some()
                            && record.evidence.source == file.evidence.source
                    })
            };
            if let (Some(file_coverage), Some(entity_coverage)) =
                (complete(FactDomain::Files), complete(FactDomain::Entities))
                && file_coverage.indexed == 1
                && provenance.get(&file.provenance).is_some_and(|record| {
                    record.producer_kind == ProducerKind::Parser && record.language == "groovy"
                })
            {
                index.contexts.insert(
                    file.id,
                    Context {
                        file,
                        coverage: [file_coverage, entity_coverage],
                        retained_entities: 0,
                    },
                );
            } else {
                // An omitted declaration could compete with an otherwise unique import.
                index.unavailable = true;
            }
        }
        if index.unavailable || index.contexts.is_empty() {
            return Ok(index);
        }
        for entity in entities
            .values()
            .copied()
            .filter(|entity| entity.language == "groovy")
        {
            cancellation.check()?;
            let context = entity
                .evidence
                .source
                .as_ref()
                .and_then(|source| index.contexts.get_mut(&source.span().file()));
            let Some(context) = context else {
                index.unavailable = true;
                continue;
            };
            if entity.provenance != context.file.provenance
                || entity
                    .evidence
                    .source
                    .as_ref()
                    .is_none_or(|source| source.content_hash() != context.file.content_hash)
            {
                index.unavailable = true;
                continue;
            }
            context.retained_entities = context
                .retained_entities
                .checked_add(1)
                .ok_or(ResolutionError::CountOverflow)?;
            if !is_type(entity.kind) {
                continue;
            }
            if let Some((name, declaration)) =
                declaration_path(entity, context, entities, cancellation)?
            {
                index
                    .declarations
                    .entry(name)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(declaration));
            } else {
                index.unavailable = true;
            }
        }
        // A complete producer count is not proof that composition retained every declaration.
        if index
            .contexts
            .values()
            .any(|context| context.retained_entities != context.coverage[1].indexed)
        {
            index.unavailable = true;
        }
        for envelope in extensions {
            cancellation.check()?;
            if envelope.namespace != rootlight_ir::GROOVY_BINDING_NAMESPACE {
                continue;
            }
            let Some(source) = envelope.evidence.source.as_ref() else {
                continue;
            };
            let binding = rootlight_ir::decode_groovy_binding_envelope(envelope)
                .ok()
                .filter(|binding| {
                    index
                        .contexts
                        .get(&source.span().file())
                        .is_some_and(|context| {
                            envelope.provenance == context.file.provenance
                                && source.content_hash() == context.file.content_hash
                                && binding.import.end_byte() <= context.file.byte_length
                                && source.span().end_byte() <= context.file.byte_length
                        })
                })
                .map(|binding| (envelope, binding));
            index
                .uses
                .entry(source.span())
                .and_modify(|prior| *prior = None)
                .or_insert(binding);
        }
        Ok(index)
    }

    pub(crate) fn proof(&self, occurrence: &OccurrenceRecord) -> Option<Proof<'a, '_>> {
        if self.unavailable {
            return None;
        }
        let source = self.contexts.get(&occurrence.file)?;
        if occurrence.provenance != source.file.provenance
            || occurrence.source.content_hash() != source.file.content_hash
        {
            return None;
        }
        let (name, binding) = if occurrence.role == OccurrenceRole::ImportUse
            && occurrence.syntax_kind == "groovy.type_import.reference"
        {
            (occurrence.syntactic_text_hash, None)
        } else if occurrence.role == OccurrenceRole::TypeUse
            && matches!(
                occurrence.syntax_kind.as_str(),
                "groovy.type_identifier.reference" | "groovy.qualified_type.reference"
            )
        {
            let (envelope, binding) = self.uses.get(&occurrence.source.span())?.as_ref()?;
            if envelope.evidence.source.as_ref() != Some(&occurrence.source) {
                return None;
            }
            (binding.target, Some(*envelope))
        } else {
            return None;
        };
        let target = self.declarations.get(&name)?.as_ref()?;
        let destination = self
            .contexts
            .get(&target.entity.evidence.source.as_ref()?.span().file())?;
        Some(Proof {
            target: target.entity,
            binding,
            owners: &target.owners,
            coverage: [
                source.coverage[0],
                source.coverage[1],
                destination.coverage[0],
                destination.coverage[1],
            ],
        })
    }

    pub(crate) fn resolve(&self, occurrence: &OccurrenceRecord) -> Option<ResolutionDecision> {
        let symbol = self.proof(occurrence)?.target.id;
        let confidence =
            rootlight_ir::Confidence::new(900).expect("fixed structural confidence is valid");
        Some(ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Resolved { symbol, confidence },
            explanation: ResolutionExplanation {
                rule: ResolutionRule::GroovyTypeImport,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: vec![CandidateExplanation {
                    symbol,
                    score: confidence,
                    positive_signals: vec![ResolutionSignal::NativeTypeImport],
                    penalties: Vec::new(),
                }],
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::NoRepositoryExecution,
                    CompletenessAssumption::IndexedPackageSnapshot,
                ],
            },
        })
    }
}

fn is_type(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Class
            | EntityKind::Struct
            | EntityKind::Interface
            | EntityKind::Trait
            | EntityKind::Enum
    )
}

fn declaration_path<'a>(
    entity: &'a EntityRecord,
    context: &Context<'a>,
    entities: &BTreeMap<SymbolId, &'a EntityRecord>,
    cancellation: &Cancellation,
) -> Result<Option<(ContentHash, Declaration<'a>)>, ResolutionError> {
    let mut current = entity;
    let mut parts = Vec::new();
    let mut owners = Vec::new();
    let mut package_seen = false;
    let mut length = 0_usize;
    for _ in 0..super::engine::MAX_SCOPE_DEPTH {
        cancellation.check()?;
        let Some(source) = current.evidence.source.as_ref() else {
            return Ok(None);
        };
        if current.language != "groovy"
            || current.provenance != context.file.provenance
            || source.span().file() != context.file.id
            || source.content_hash() != context.file.content_hash
        {
            return Ok(None);
        }
        if current.kind == EntityKind::Module
            && current.container == Some(ContainerRef::File(context.file.id))
        {
            if current.canonical_name != context.file.path
                || source.span().start_byte() != 0
                || source.span().end_byte() != context.file.byte_length
            {
                return Ok(None);
            }
            owners.push(current);
            parts.reverse();
            return Ok(Some((
                content_hash(parts.join(".").as_bytes()),
                Declaration { entity, owners },
            )));
        }
        if current.kind == EntityKind::Module && !package_seen {
            package_seen = true;
        } else if package_seen || !is_type(current.kind) {
            return Ok(None);
        }
        length = length
            .checked_add(current.canonical_name.len())
            .and_then(|length| length.checked_add(usize::from(!parts.is_empty())))
            .ok_or(ResolutionError::CountOverflow)?;
        if length > IrLimits::default().max_string_bytes {
            return Ok(None);
        }
        parts.push(current.canonical_name.as_str());
        if current.id != entity.id {
            owners.push(current);
        }
        let Some(ContainerRef::Entity(parent)) = current.container else {
            return Ok(None);
        };
        let Some(parent) = entities.get(&parent).copied() else {
            return Ok(None);
        };
        current = parent;
    }
    Ok(None)
}
