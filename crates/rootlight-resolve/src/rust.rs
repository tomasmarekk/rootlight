//! Adopt exact producer-owned Rust call proofs without searching by spelling.
//! Namespace availability and lexical visibility remain the producer's source-backed obligations.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, EntityRecord, ExtensionEnvelope, FileRecord, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerKind, ProvenanceRecord, RUST_DIRECT_CALL_NAMESPACE,
    SourceRef, SourceSpan, decode_rust_direct_call_envelope,
};

use crate::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionOutcome, ResolutionRule, ResolutionSignal,
};

pub(crate) struct RustCallProof<'a> {
    pub(crate) envelope: &'a ExtensionEnvelope,
    pub(crate) definition: SourceRef,
    pub(crate) definition_id: FactId,
    pub(crate) module: Option<&'a EntityRecord>,
    pub(crate) example: Option<(&'a EntityRecord, FactId)>,
    target: SymbolId,
}

#[derive(Default)]
pub(crate) struct RustIndex<'a> {
    calls: BTreeMap<SourceSpan, Option<RustCallProof<'a>>>,
}

impl<'a> RustIndex<'a> {
    pub(crate) fn build(
        files: &BTreeMap<FileId, &'a FileRecord>,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        provenance: &BTreeMap<FactId, &'a ProvenanceRecord>,
        extensions: &'a [ExtensionEnvelope],
        occurrences: &[OccurrenceRecord],
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        let mut definitions = BTreeMap::new();
        let mut calls = BTreeMap::new();
        let mut anonymous = BTreeSet::new();
        let mut examples = BTreeMap::new();
        for occurrence in occurrences {
            cancellation.check()?;
            if occurrence.role == OccurrenceRole::Definition
                && occurrence.syntax_kind == "markdown.code_block.module"
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                examples
                    .entry(symbol)
                    .and_modify(|prior: &mut Option<&OccurrenceRecord>| {
                        if prior.is_some_and(|record| record != occurrence) {
                            *prior = None;
                        }
                    })
                    .or_insert(Some(occurrence));
            }
            if occurrence.role == OccurrenceRole::CallSite
                && occurrence.syntax_kind == "rust.direct_call.call"
            {
                calls
                    .entry(occurrence.source.span())
                    .and_modify(|prior: &mut Option<&OccurrenceRecord>| {
                        if prior.is_some_and(|record| record != occurrence) {
                            *prior = None;
                        }
                    })
                    .or_insert(Some(occurrence));
            }
            if occurrence.syntax_kind == "rust.anonymous_function.declaration"
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                anonymous.insert(symbol);
            }
            if occurrence.role == OccurrenceRole::Definition
                && matches!(
                    occurrence.syntax_kind.as_str(),
                    "rust.function.definition" | "rust.identifier.definition"
                )
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                definitions
                    .entry((occurrence.source.span(), symbol))
                    .and_modify(|witness: &mut Option<&OccurrenceRecord>| {
                        if witness.is_some_and(|record| record != occurrence) {
                            *witness = None;
                        }
                    })
                    .or_insert(Some(occurrence));
            }
        }
        let mut index = Self::default();
        for envelope in extensions {
            cancellation.check()?;
            if envelope.namespace != RUST_DIRECT_CALL_NAMESPACE {
                continue;
            }
            let claim = decode_rust_direct_call_envelope(envelope)
                .map_err(|_| ResolutionError::UnsupportedIdentityRemap)?;
            let source = envelope
                .evidence
                .source
                .as_ref()
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            let Some(file) = files.get(&source.span().file()).filter(|file| {
                matches!(file.language.as_str(), "rust" | "markdown")
                    && file.content_hash == source.content_hash()
                    && file.repository == envelope.repository
                    && file.generation == envelope.generation
                    && file.provenance == envelope.provenance
            }) else {
                continue;
            };
            let Some(producer) = provenance.get(&envelope.provenance).filter(|record| {
                record.language == file.language
                    && (record.producer_kind == ProducerKind::Parser
                        || (file.language == "rust"
                            && record.producer_kind == ProducerKind::Derivation
                            && record.frontend_version.as_deref()
                                == Some("rootlight-project-structural-v1")
                            && record.rule.as_deref() == Some("rootlight.project-structural.v1")))
            }) else {
                continue;
            };
            let Some(entity) = entities.get(&claim.target).filter(|entity| {
                entity.language == "rust"
                    && entity.kind == EntityKind::Function
                    && entity.provenance == producer.id
                    && content_hash(entity.canonical_name.as_bytes()) == claim.canonical_name
                    && entity.evidence.source.as_ref().is_some_and(|reference| {
                        reference.repository() == envelope.repository
                            && reference.generation() == envelope.generation
                            && reference.span().file() == file.id
                            && reference.content_hash() == file.content_hash
                            && reference.span().start_byte() <= claim.definition.start_byte()
                            && claim.definition.end_byte() <= reference.span().end_byte()
                    })
            }) else {
                continue;
            };
            let Some(owner) = module_owner(
                entity.container,
                entity
                    .evidence
                    .source
                    .as_ref()
                    .expect("source was validated"),
                file,
                producer.id,
                entities,
                &examples,
            ) else {
                continue;
            };
            let Some(definition) = definitions
                .get(&(claim.definition, claim.target))
                .copied()
                .flatten()
                .filter(|definition| {
                    definition.provenance == producer.id
                        && definition.source.repository() == envelope.repository
                        && definition.source.generation() == envelope.generation
                        && definition.source.content_hash() == file.content_hash
                        && definition.file == file.id
                })
            else {
                continue;
            };
            // The typed claim names its target. Existing written witnesses
            // constrain that target; no unrelated same-name entity is considered.
            if let Some(occurrence) = calls.get(&source.span()).copied().flatten() {
                if occurrence.source != *source
                    || occurrence.provenance != producer.id
                    || caller_module(
                        occurrence.enclosing,
                        &occurrence.source,
                        file,
                        producer.id,
                        entities,
                        &anonymous,
                        &examples,
                    ) != Some(owner)
                {
                    continue;
                }
                let proof = RustCallProof {
                    envelope,
                    definition: definition.source.clone(),
                    definition_id: definition.id,
                    module: match owner.module {
                        ContainerRef::Entity(symbol) => entities.get(&symbol).copied(),
                        _ => None,
                    },
                    example: owner.example.and_then(|symbol| {
                        Some((
                            entities.get(&symbol).copied()?,
                            examples.get(&symbol)?.as_ref()?.id,
                        ))
                    }),
                    target: claim.target,
                };
                index
                    .calls
                    .entry(source.span())
                    .and_modify(|previous| *previous = None)
                    .or_insert(Some(proof));
            }
        }
        Ok(index)
    }

    pub(crate) fn proof(&self, occurrence: &OccurrenceRecord) -> Option<&RustCallProof<'a>> {
        if occurrence.role != OccurrenceRole::CallSite
            || occurrence.syntax_kind != "rust.direct_call.call"
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
        {
            return None;
        }
        let proof = self
            .calls
            .get(&occurrence.source.span())
            .and_then(Option::as_ref)?;
        (proof.envelope.evidence.source.as_ref() == Some(&occurrence.source)
            && proof.envelope.provenance == occurrence.provenance)
            .then_some(proof)
    }

    pub(crate) fn eligible(&self, occurrence: &OccurrenceRecord) -> bool {
        self.proof(occurrence).is_some()
    }

    pub(crate) fn resolve(&self, occurrence: &OccurrenceRecord) -> Option<ResolutionDecision> {
        let proof = self.proof(occurrence)?;
        let confidence =
            rootlight_ir::Confidence::new(900).expect("structural confidence is valid");
        Some(ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Resolved {
                symbol: proof.target,
                confidence,
            },
            explanation: ResolutionExplanation {
                rule: ResolutionRule::RustDirectFunctionCall,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: vec![CandidateExplanation {
                    symbol: proof.target,
                    score: confidence,
                    positive_signals: vec![ResolutionSignal::NativeDirectFunction],
                    penalties: Vec::new(),
                }],
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::SourceDeclaredRustModule,
                    CompletenessAssumption::NoRepositoryExecution,
                ],
            },
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct WrittenOwner {
    module: ContainerRef,
    example: Option<SymbolId>,
}

fn contains_source(owner: &SourceRef, child: &SourceRef) -> bool {
    owner.repository() == child.repository()
        && owner.generation() == child.generation()
        && owner.content_hash() == child.content_hash()
        && owner.span().file() == child.span().file()
        && owner.span().start_byte() <= child.span().start_byte()
        && child.span().end_byte() <= owner.span().end_byte()
}

fn module_owner(
    container: Option<ContainerRef>,
    source: &SourceRef,
    file: &FileRecord,
    producer: FactId,
    entities: &BTreeMap<SymbolId, &EntityRecord>,
    examples: &BTreeMap<SymbolId, Option<&OccurrenceRecord>>,
) -> Option<WrittenOwner> {
    let mut container = container?;
    let mut contained = source;
    let mut nearest = None;
    for _ in 0..=entities.len() {
        match container {
            ContainerRef::File(owner) if owner == file.id && file.language == "rust" => {
                return Some(WrittenOwner {
                    module: nearest.unwrap_or(container),
                    example: None,
                });
            }
            ContainerRef::Entity(symbol) => {
                let entity = entities.get(&symbol)?;
                let extent = entity.evidence.source.as_ref()?;
                if entity.kind != EntityKind::Module
                    || entity.provenance != producer
                    || !contains_source(extent, contained)
                {
                    return None;
                }
                nearest.get_or_insert(container);
                if file.language == "markdown" && entity.language == "markdown" {
                    let witness = examples.get(&symbol)?.as_ref()?;
                    // A host Module alone is insufficient: only the native
                    // code-block definition establishes an isolated example.
                    if entity.canonical_name != "<code-block>"
                        || witness.provenance != producer
                        || witness.file != file.id
                        || witness.source != *extent
                        || witness.evidence.source.as_ref() != Some(extent)
                    {
                        return None;
                    }
                    return Some(WrittenOwner {
                        module: nearest.expect("the current module was recorded"),
                        example: Some(symbol),
                    });
                }
                if entity.language != "rust" {
                    return None;
                }
                contained = extent;
                container = entity.container?;
            }
            _ => return None,
        }
    }
    None
}

fn caller_module(
    enclosing: Option<SymbolId>,
    call: &SourceRef,
    file: &FileRecord,
    producer: FactId,
    entities: &BTreeMap<SymbolId, &EntityRecord>,
    anonymous: &BTreeSet<SymbolId>,
    examples: &BTreeMap<SymbolId, Option<&OccurrenceRecord>>,
) -> Option<WrittenOwner> {
    let Some(mut symbol) = enclosing else {
        return module_owner(
            Some(ContainerRef::File(file.id)),
            call,
            file,
            producer,
            entities,
            examples,
        );
    };
    let mut named_function = false;
    let mut contained = call;
    for _ in 0..entities.len() {
        let entity = entities.get(&symbol)?;
        if entity.provenance != producer
            || !entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| contains_source(source, contained))
        {
            return None;
        }
        if entity.kind == EntityKind::Module {
            return module_owner(
                Some(ContainerRef::Entity(symbol)),
                contained,
                file,
                producer,
                entities,
                examples,
            );
        }
        if entity.language != "rust"
            || !matches!(entity.kind, EntityKind::Function | EntityKind::Closure)
        {
            return None;
        }
        if entity.kind == EntityKind::Function && !anonymous.contains(&symbol) {
            if named_function {
                return None;
            }
            named_function = true;
        }
        contained = entity.evidence.source.as_ref()?;
        match entity.container? {
            ContainerRef::Entity(parent) => symbol = parent,
            ContainerRef::File(owner) if owner == file.id => {
                return module_owner(
                    Some(ContainerRef::File(owner)),
                    call,
                    file,
                    producer,
                    entities,
                    examples,
                );
            }
            _ => return None,
        }
    }
    None
}
