//! Project-level Perl package storage over native, source-bound binding claims.
//! This links declarations in the indexed snapshot, not evaluated loader paths
//! or runtime bodies; missing contexts, writes and duplicate declarations veto exact links.

use crate::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionOutcome, ResolutionRule, ResolutionSignal,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId};
use rootlight_ir::{
    EntityKind, EntityRecord, ExtensionEnvelope, FileRecord, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, PERL_BINDING_NAMESPACE, PerlBinding, PerlCallableStorage, ProducerKind,
    ProvenanceRecord, SourceSpan, decode_perl_binding_envelope,
};
use std::collections::BTreeMap;

type Definition<'a> = (&'a EntityRecord, &'a ExtensionEnvelope);

#[derive(Default)]
pub(crate) struct PerlIndex<'a> {
    calls: BTreeMap<SourceSpan, Option<(PerlCallableStorage, &'a ExtensionEnvelope)>>,
    definitions: BTreeMap<PerlCallableStorage, BTreeMap<SourceSpan, Definition<'a>>>,
    contexts: BTreeMap<FileId, &'a ExtensionEnvelope>,
    loads: BTreeMap<(FileId, ContentHash), &'a ExtensionEnvelope>,
    writes: BTreeMap<PerlCallableStorage, &'a ExtensionEnvelope>,
    dynamic_write: Option<&'a ExtensionEnvelope>,
    unavailable: bool,
}

impl<'a> PerlIndex<'a> {
    pub(crate) fn build(
        files: &BTreeMap<FileId, &'a FileRecord>,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        provenance: &BTreeMap<FactId, &'a ProvenanceRecord>,
        extensions: &'a [ExtensionEnvelope],
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        let mut index = Self::default();
        for envelope in extensions {
            cancellation.check()?;
            if envelope.namespace != PERL_BINDING_NAMESPACE {
                continue;
            }
            let claim = decode_perl_binding_envelope(envelope)
                .map_err(|_| ResolutionError::UnsupportedIdentityRemap)?;
            let Some(file) = files
                .get(&claim.module().file())
                .filter(|file| file.language == "perl")
            else {
                continue;
            };
            let source = envelope
                .evidence
                .source
                .as_ref()
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            if claim.module().start_byte() != 0
                || claim.module().end_byte() != file.byte_length
                || source.content_hash() != file.content_hash
                || envelope.provenance != file.provenance
                || !provenance.get(&envelope.provenance).is_some_and(|record| {
                    record.producer_kind == ProducerKind::Parser && record.language == "perl"
                })
            {
                index.unavailable = true;
                continue;
            }
            match claim.binding() {
                PerlBinding::ModuleContext => {
                    index.contexts.insert(file.id, envelope);
                }
                PerlBinding::Definition { storage, symbol } => {
                    let entity = entities.get(&symbol).copied().filter(|entity| {
                        entity.language == "perl"
                            && entity.kind == EntityKind::Function
                            && entity.provenance == envelope.provenance
                            && entity
                                .evidence
                                .source
                                .as_ref()
                                .is_some_and(|source| source.span().file() == file.id)
                    });
                    if let Some(entity) = entity {
                        index
                            .definitions
                            .entry(storage)
                            .or_default()
                            .insert(source.span(), (entity, envelope));
                    } else {
                        index.unavailable = true;
                    }
                }
                PerlBinding::Call { storage } => {
                    index
                        .calls
                        .entry(source.span())
                        .and_modify(|prior| {
                            if prior.is_some_and(|(key, _)| key != storage) {
                                *prior = None;
                            }
                        })
                        .or_insert(Some((storage, envelope)));
                }
                PerlBinding::Write { storage } => {
                    index
                        .writes
                        .entry(storage)
                        .and_modify(|prior| {
                            if source_precedes(envelope, prior) {
                                *prior = envelope;
                            }
                        })
                        .or_insert(envelope);
                }
                PerlBinding::DynamicWrite => {
                    index.unavailable = true;
                    if index
                        .dynamic_write
                        .is_none_or(|prior| source_precedes(envelope, prior))
                    {
                        index.dynamic_write = Some(envelope);
                    }
                }
                PerlBinding::ModuleLoad { package } => {
                    index
                        .loads
                        .entry((file.id, package))
                        .and_modify(|prior| {
                            if source_precedes(envelope, prior) {
                                *prior = envelope;
                            }
                        })
                        .or_insert(envelope);
                }
            }
        }
        for file in files.values() {
            cancellation.check()?;
            if file.language == "perl" && !index.contexts.contains_key(&file.id) {
                index.unavailable = true;
            }
        }
        Ok(index)
    }

    fn proof(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Option<(SymbolId, [&'a ExtensionEnvelope; 5])> {
        if self.unavailable
            || occurrence.role != OccurrenceRole::CallSite
            || !occurrence.syntax_kind.starts_with("perl.")
        {
            return None;
        }
        let (storage, call) = self
            .calls
            .get(&occurrence.source.span())
            .copied()
            .flatten()?;
        if call.evidence.source.as_ref() != Some(&occurrence.source)
            || self.writes.contains_key(&storage)
        {
            return None;
        }
        let definitions = self.definitions.get(&storage)?;
        if definitions.len() != 1 {
            return None;
        }
        let (entity, declaration) = *definitions.values().next()?;
        let target_file = declaration.evidence.source.as_ref()?.span().file();
        if target_file == occurrence.file {
            return None;
        }
        let load = *self.loads.get(&(occurrence.file, storage.package))?;
        Some((
            entity.id,
            [
                call,
                declaration,
                load,
                *self.contexts.get(&occurrence.file)?,
                *self.contexts.get(&target_file)?,
            ],
        ))
    }

    pub(crate) fn eligible(&self, occurrence: &OccurrenceRecord) -> bool {
        self.proof(occurrence).is_some()
    }

    pub(crate) fn conflicting_write(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Option<[&'a ExtensionEnvelope; 2]> {
        if occurrence.role != OccurrenceRole::CallSite
            || !matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
        {
            return None;
        }
        let (storage, call) = self
            .calls
            .get(&occurrence.source.span())
            .copied()
            .flatten()?;
        if call.evidence.source.as_ref() != Some(&occurrence.source) {
            return None;
        }
        // An unavailable project context forbids promotion, but must not hide
        // a retained write that already contradicts a per-file exact target.
        let write = self.writes.get(&storage).copied().or(self.dynamic_write)?;
        Some([call, write])
    }

    pub(crate) fn evidence(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Option<[&'a ExtensionEnvelope; 5]> {
        self.proof(occurrence).map(|(_, evidence)| evidence)
    }

    pub(crate) fn resolve(&self, occurrence: &OccurrenceRecord) -> Option<ResolutionDecision> {
        let (symbol, _) = self.proof(occurrence)?;
        let confidence =
            rootlight_ir::Confidence::new(900).expect("fixed structural confidence is valid");
        Some(ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Resolved { symbol, confidence },
            explanation: ResolutionExplanation {
                rule: ResolutionRule::PerlPackageStorage,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: vec![CandidateExplanation {
                    symbol,
                    score: confidence,
                    positive_signals: vec![ResolutionSignal::NativePackageStorage],
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

fn source_precedes(candidate: &ExtensionEnvelope, prior: &ExtensionEnvelope) -> bool {
    // Envelope IDs include the generation. Selecting by them would change
    // otherwise identical project evidence after a clean rebuild or restart.
    candidate
        .evidence
        .source
        .as_ref()
        .map(|source| source.span())
        < prior.evidence.source.as_ref().map(|source| source.span())
}
