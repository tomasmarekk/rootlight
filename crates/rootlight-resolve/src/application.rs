//! Provenance-aware application of semantic decisions to normalized IR.
//!
//! Application recomputes every changed occurrence or relation identity and
//! fails closed when an existing derived-fact chain would require a wider
//! identity cascade than this resolver stage owns.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, EvidenceKind, ExtensionSupport, FactEvidence,
    FactRef, IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    ProducerIdentity, ProducerKind, ProvenanceRecord, RelationEndpoint, RelationPredicate,
    RelationRecord, SourceRef, canonicalize_ir_document, derive_occurrence_record_id,
    derive_provenance_record_id, derive_relation_record_id, validate_ir_document,
};

use crate::{
    AppliedResolution, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision,
    ResolutionEngine, ResolutionError, ResolutionOutcome, ResolutionRule, ResolverFactContext,
    engine::{CandidateIndex, ResolutionWorkBudget, resolvable_role},
};

impl ResolutionEngine {
    /// Applies semantic decisions to a normalized IR document.
    ///
    /// Exact decisions emit their role-specific relation. Ambiguous calls emit
    /// only [`RelationPredicate::DispatchCandidate`]; other ambiguous sites
    /// retain their candidates exclusively in [`OccurrenceTarget::Candidates`].
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionError`] when resolution, provenance construction,
    /// identity remapping, or final normalized-IR validation fails.
    pub fn apply(
        &self,
        mut document: NormalizedIrDocument,
        context: ResolverFactContext,
        cancellation: &Cancellation,
    ) -> Result<AppliedResolution, ResolutionError> {
        let batch = self.resolve(&document, cancellation)?;
        let lookup = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            cancellation,
        )?;
        let occurrence_indexes = document
            .occurrences
            .iter()
            .enumerate()
            .map(|(index, occurrence)| (occurrence.id, index))
            .collect::<BTreeMap<_, _>>();
        let mut occurrence_remap = BTreeMap::new();
        let mut pending_relations = Vec::new();
        let mut new_provenance = Vec::new();
        let mut known_provenance = document
            .provenance
            .iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        let producer = resolver_producer(self.limits, &self.policy)?;

        for decision in &batch.decisions {
            cancellation.check()?;
            if matches!(decision.outcome, ResolutionOutcome::Unresolved { .. }) {
                continue;
            }
            let index = *occurrence_indexes
                .get(&decision.occurrence)
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            let provenance = build_provenance(
                document.repository,
                document.generation,
                &lookup,
                &document.occurrences[index],
                decision,
                &producer,
                context,
            )?;
            let provenance_id = provenance.id;
            if known_provenance.insert(provenance_id) {
                new_provenance.push(provenance);
            }

            let occurrence = &mut document.occurrences[index];
            let old_id = occurrence.id;
            apply_target(occurrence, decision);
            occurrence.provenance = provenance_id;
            // Candidate identities already live in the target and resolver
            // provenance. Repeating them in occurrence evidence multiplies
            // storage without adding an independent derivation edge.
            occurrence.id =
                derive_occurrence_record_id(occurrence).map_err(ResolutionError::FactIdentity)?;
            occurrence_remap.insert(old_id, occurrence.id);
            pending_relations.extend(relation_specs(occurrence, decision));
        }
        drop(lookup);
        document.provenance.append(&mut new_provenance);

        ensure_nonrelation_remap_is_safe(&document, &occurrence_remap)?;
        let relation_remap =
            remap_existing_relations(&mut document.relations, &occurrence_remap, cancellation)?;
        ensure_relation_remap_is_safe(&document, &relation_remap)?;
        for spec in pending_relations {
            cancellation.check()?;
            document.relations.push(spec.into_record()?);
        }
        let document =
            canonicalize_ir_document(document, &IrLimits::default(), &Default::default())
                .map_err(ResolutionError::InvalidDocument)?;

        Ok(AppliedResolution { document, batch })
    }

    /// Applies semantic resolution without retaining a repository-sized
    /// explanation batch.
    ///
    /// This path preserves the same targets, provenance, relations, and
    /// canonical identities as [`Self::apply`], but streams one decision at a
    /// time so substantial repositories do not duplicate every candidate and
    /// rejection explanation in memory.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionError`] under the same validation, resolution,
    /// provenance, remapping, cancellation, and canonicalization conditions as
    /// [`Self::apply`].
    pub fn apply_document(
        &self,
        mut document: NormalizedIrDocument,
        context: ResolverFactContext,
        cancellation: &Cancellation,
    ) -> Result<NormalizedIrDocument, ResolutionError> {
        cancellation.check()?;
        validate_ir_document(
            &document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .map_err(ResolutionError::InvalidDocument)?;

        let lookup = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            cancellation,
        )?;
        let repository = document.repository;
        let generation = document.generation;
        let producer = resolver_producer(self.limits, &self.policy)?;
        let mut known_provenance = document
            .provenance
            .iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        let mut new_provenance = Vec::new();
        let mut occurrence_remap = BTreeMap::new();
        let mut work = ResolutionWorkBudget::new(self.limits.work_limit());

        for occurrence in &mut document.occurrences {
            cancellation.check()?;
            work.consume()?;
            if matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
                || !resolvable_role(occurrence.role)
            {
                continue;
            }
            let decision = self.resolve_occurrence(occurrence, &lookup, &mut work, cancellation)?;
            if matches!(decision.outcome, ResolutionOutcome::Unresolved { .. }) {
                continue;
            }
            let provenance = build_provenance(
                repository, generation, &lookup, occurrence, &decision, &producer, context,
            )?;
            let provenance_id = provenance.id;
            if known_provenance.insert(provenance_id) {
                new_provenance.push(provenance);
            }

            let old_id = occurrence.id;
            apply_target(occurrence, &decision);
            occurrence.provenance = provenance_id;
            occurrence.id =
                derive_occurrence_record_id(occurrence).map_err(ResolutionError::FactIdentity)?;
            occurrence_remap.insert(old_id, occurrence.id);
            for relation in relation_specs(occurrence, &decision) {
                document.relations.push(relation.into_record()?);
            }
        }
        drop(lookup);
        document.provenance.append(&mut new_provenance);

        ensure_nonrelation_remap_is_safe(&document, &occurrence_remap)?;
        let relation_remap =
            remap_existing_relations(&mut document.relations, &occurrence_remap, cancellation)?;
        ensure_relation_remap_is_safe(&document, &relation_remap)?;
        canonicalize_ir_document(document, &IrLimits::default(), &Default::default())
            .map_err(ResolutionError::InvalidDocument)
    }
}

#[derive(Clone)]
struct PendingRelation {
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    occurrence: FactId,
    subject: RelationEndpoint,
    target: SymbolId,
    predicate: RelationPredicate,
    confidence: Confidence,
    provenance: FactId,
    source: SourceRef,
}

impl PendingRelation {
    fn into_record(self) -> Result<RelationRecord, ResolutionError> {
        let mut derivation = vec![FactRef::Fact(self.occurrence), FactRef::Entity(self.target)];
        if let RelationEndpoint::Entity(subject) = self.subject {
            derivation.push(FactRef::Entity(subject));
        }
        derivation.sort_unstable();
        derivation.dedup();
        let mut record = RelationRecord {
            id: FactId::from_bytes([0; 20]),
            repository: self.repository,
            generation: self.generation,
            subject: self.subject,
            predicate: self.predicate,
            object: RelationEndpoint::Entity(self.target),
            confidence: self.confidence,
            evidence_kind: EvidenceKind::Derived,
            provenance: self.provenance,
            evidence: FactEvidence {
                source: Some(self.source),
                derivation,
            },
        };
        record.id = derive_relation_record_id(&record).map_err(ResolutionError::FactIdentity)?;
        Ok(record)
    }
}

fn resolver_producer(
    limits: crate::ResolutionLimits,
    policy: &crate::ResolutionPolicy,
) -> Result<ProducerIdentity, ResolutionError> {
    let mut configuration = Vec::with_capacity(RESOLVER_PROVIDER_VERSION.len() + 16);
    configuration.extend_from_slice(RESOLVER_PROVIDER_VERSION.as_bytes());
    let candidate_limit =
        u64::try_from(limits.candidate_limit()).map_err(|_| ResolutionError::CountOverflow)?;
    configuration.extend_from_slice(&candidate_limit.to_be_bytes());
    let work_limit =
        u64::try_from(limits.work_limit()).map_err(|_| ResolutionError::CountOverflow)?;
    configuration.extend_from_slice(&work_limit.to_be_bytes());
    policy.append_configuration(&mut configuration);
    ProducerIdentity::new(
        RESOLVER_PROVIDER_NAME,
        env!("CARGO_PKG_VERSION"),
        content_hash(&configuration),
    )
    .map_err(ResolutionError::InvalidProducer)
}

fn build_provenance(
    repository: rootlight_ids::RepositoryId,
    generation: rootlight_ids::GenerationId,
    lookup: &CandidateIndex<'_>,
    occurrence: &OccurrenceRecord,
    decision: &ResolutionDecision,
    producer: &ProducerIdentity,
    context: ResolverFactContext,
) -> Result<ProvenanceRecord, ResolutionError> {
    let parent = lookup
        .provenance
        .get(&occurrence.provenance)
        .copied()
        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
    let mut sources = vec![occurrence.source.clone()];
    let mut derivation_parents = Vec::new();
    let mut context_digests = vec![parent.build_context.digest()];
    let mut tier = parent.tier;

    for candidate in &decision.explanation.candidates {
        let entity = lookup
            .entities
            .get(&candidate.symbol)
            .copied()
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        derivation_parents.push(FactRef::Entity(entity.id));
        tier = lower_tier(tier, entity.tier);
        if let Some(source) = &entity.evidence.source
            && !sources.contains(source)
        {
            sources.push(source.clone());
        }
        let entity_provenance = lookup
            .provenance
            .get(&entity.provenance)
            .copied()
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        context_digests.push(entity_provenance.build_context.digest());
    }
    sources.sort_unstable();
    sources.dedup();
    derivation_parents.sort_unstable();
    derivation_parents.dedup();
    context_digests.sort_unstable();
    context_digests.dedup();

    let mut context_bytes = Vec::with_capacity(context_digests.len().saturating_mul(32));
    for digest in context_digests {
        context_bytes.extend_from_slice(digest.as_bytes());
    }
    let language = lookup
        .files
        .get(&occurrence.file)
        .map(|file| file.language.clone())
        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
    let rule = match decision.explanation.rule {
        ResolutionRule::LexicalScope => "scope-v1.lexical_scope",
        ResolutionRule::Import => "scope-v1.import",
    };
    let mut record = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Rule,
        producer: producer.clone(),
        binary_digest: context.binary_digest(),
        frontend_version: Some(RESOLVER_PROVIDER_VERSION.to_owned()),
        language,
        tier,
        build_context: BuildContextIdentity::new(content_hash(&context_bytes)),
        input_sources: sources.clone(),
        evidence_sources: sources,
        derivation_parents,
        rule: Some(rule.to_owned()),
    };
    record.id = derive_provenance_record_id(&record).map_err(ResolutionError::FactIdentity)?;
    Ok(record)
}

fn apply_target(occurrence: &mut OccurrenceRecord, decision: &ResolutionDecision) {
    match &decision.outcome {
        ResolutionOutcome::Resolved { symbol, confidence } => {
            occurrence.target = OccurrenceTarget::Resolved { symbol: *symbol };
            occurrence.confidence = *confidence;
        }
        ResolutionOutcome::Candidates {
            symbols,
            total_count,
            completeness,
            confidence,
        } => {
            let mut symbols = symbols.clone();
            symbols.sort_unstable();
            symbols.dedup();
            occurrence.target = OccurrenceTarget::Candidates {
                symbols,
                total_count: *total_count,
                completeness: *completeness,
            };
            occurrence.confidence = *confidence;
        }
        ResolutionOutcome::Unresolved { .. } => {}
    }
}

fn relation_specs(
    occurrence: &OccurrenceRecord,
    decision: &ResolutionDecision,
) -> Vec<PendingRelation> {
    match &decision.outcome {
        ResolutionOutcome::Resolved { symbol, confidence } => relation_predicate(occurrence.role)
            .map(|predicate| {
                vec![pending_relation(
                    occurrence,
                    *symbol,
                    predicate,
                    *confidence,
                )]
            })
            .unwrap_or_default(),
        ResolutionOutcome::Candidates { symbols, .. }
            if occurrence.role == OccurrenceRole::CallSite =>
        {
            symbols
                .iter()
                .map(|symbol| {
                    let confidence = decision
                        .explanation
                        .candidates
                        .iter()
                        .find(|candidate| candidate.symbol == *symbol)
                        .map_or(occurrence.confidence, |candidate| candidate.score);
                    pending_relation(
                        occurrence,
                        *symbol,
                        RelationPredicate::DispatchCandidate,
                        confidence,
                    )
                })
                .collect()
        }
        ResolutionOutcome::Candidates { .. } | ResolutionOutcome::Unresolved { .. } => Vec::new(),
    }
}

fn pending_relation(
    occurrence: &OccurrenceRecord,
    target: SymbolId,
    predicate: RelationPredicate,
    confidence: Confidence,
) -> PendingRelation {
    PendingRelation {
        repository: occurrence.repository,
        generation: occurrence.generation,
        occurrence: occurrence.id,
        subject: relation_subject(occurrence, predicate),
        target,
        predicate,
        confidence,
        provenance: occurrence.provenance,
        source: occurrence.source.clone(),
    }
}

fn relation_subject(
    occurrence: &OccurrenceRecord,
    predicate: RelationPredicate,
) -> RelationEndpoint {
    if matches!(
        predicate,
        RelationPredicate::Extends
            | RelationPredicate::Implements
            | RelationPredicate::Satisfies
            | RelationPredicate::Embeds
            | RelationPredicate::MixesIn
            | RelationPredicate::Overrides
    ) && let Some(enclosing) = occurrence.enclosing
    {
        RelationEndpoint::Entity(enclosing)
    } else {
        RelationEndpoint::Occurrence(occurrence.id)
    }
}

fn relation_predicate(role: OccurrenceRole) -> Option<RelationPredicate> {
    match role {
        OccurrenceRole::Reference | OccurrenceRole::DecoratorUse | OccurrenceRole::MacroUse => {
            Some(RelationPredicate::RefersTo)
        }
        OccurrenceRole::CallSite => Some(RelationPredicate::Calls),
        OccurrenceRole::TypeUse => Some(RelationPredicate::UsesType),
        OccurrenceRole::ImportUse => Some(RelationPredicate::Imports),
        OccurrenceRole::InheritanceUse => Some(RelationPredicate::Extends),
        OccurrenceRole::ImplementationUse => Some(RelationPredicate::Implements),
        OccurrenceRole::RouteUse => Some(RelationPredicate::CallsRoute),
        OccurrenceRole::TestUse => Some(RelationPredicate::Tests),
        OccurrenceRole::Read => Some(RelationPredicate::Reads),
        OccurrenceRole::Write => Some(RelationPredicate::Writes),
        OccurrenceRole::Definition
        | OccurrenceRole::Declaration
        | OccurrenceRole::Documentation
        | OccurrenceRole::StringEvidence => None,
    }
}

fn remap_existing_relations(
    relations: &mut [RelationRecord],
    occurrence_remap: &BTreeMap<FactId, FactId>,
    cancellation: &Cancellation,
) -> Result<BTreeMap<FactId, FactId>, ResolutionError> {
    let mut relation_remap = BTreeMap::new();
    for relation in relations {
        cancellation.check()?;
        let old_id = relation.id;
        let mut changed = remap_endpoint(&mut relation.subject, occurrence_remap);
        changed |= remap_endpoint(&mut relation.object, occurrence_remap);
        changed |= remap_fact_refs(&mut relation.evidence.derivation, occurrence_remap);
        if changed {
            relation.id =
                derive_relation_record_id(relation).map_err(ResolutionError::FactIdentity)?;
            relation_remap.insert(old_id, relation.id);
        }
    }
    Ok(relation_remap)
}

fn remap_endpoint(
    endpoint: &mut RelationEndpoint,
    occurrence_remap: &BTreeMap<FactId, FactId>,
) -> bool {
    let RelationEndpoint::Occurrence(id) = endpoint else {
        return false;
    };
    let Some(replacement) = occurrence_remap.get(id) else {
        return false;
    };
    *id = *replacement;
    true
}

fn remap_fact_refs(references: &mut [FactRef], remap: &BTreeMap<FactId, FactId>) -> bool {
    let mut changed = false;
    for reference in references {
        let FactRef::Fact(id) = reference else {
            continue;
        };
        if let Some(replacement) = remap.get(id) {
            *id = *replacement;
            changed = true;
        }
    }
    changed
}

fn ensure_nonrelation_remap_is_safe(
    document: &NormalizedIrDocument,
    occurrence_remap: &BTreeMap<FactId, FactId>,
) -> Result<(), ResolutionError> {
    let unsafe_reference = document
        .files
        .iter()
        .map(|record| &record.evidence)
        .chain(document.entities.iter().map(|record| &record.evidence))
        .chain(document.occurrences.iter().map(|record| &record.evidence))
        .chain(
            document
                .source_mappings
                .iter()
                .map(|record| &record.evidence),
        )
        .chain(
            document
                .coverage_records
                .iter()
                .map(|record| &record.evidence),
        )
        .chain(
            document
                .skipped_regions
                .iter()
                .map(|record| &record.evidence),
        )
        .chain(document.diagnostics.iter().map(|record| &record.evidence))
        .chain(document.extensions.iter().map(|record| &record.evidence))
        .any(|evidence| contains_remapped_fact(&evidence.derivation, occurrence_remap))
        || document
            .provenance
            .iter()
            .any(|record| contains_remapped_fact(&record.derivation_parents, occurrence_remap));
    if unsafe_reference {
        Err(ResolutionError::UnsupportedIdentityRemap)
    } else {
        Ok(())
    }
}

fn ensure_relation_remap_is_safe(
    document: &NormalizedIrDocument,
    relation_remap: &BTreeMap<FactId, FactId>,
) -> Result<(), ResolutionError> {
    if relation_remap.is_empty() {
        return Ok(());
    }
    let relation_dependency = document
        .relations
        .iter()
        .any(|record| contains_remapped_fact(&record.evidence.derivation, relation_remap));
    if relation_dependency {
        return Err(ResolutionError::UnsupportedIdentityRemap);
    }
    ensure_nonrelation_remap_is_safe(document, relation_remap)
}

fn contains_remapped_fact(references: &[FactRef], remap: &BTreeMap<FactId, FactId>) -> bool {
    references
        .iter()
        .any(|reference| matches!(reference, FactRef::Fact(id) if remap.contains_key(id)))
}

fn lower_tier(left: AnalysisTier, right: AnalysisTier) -> AnalysisTier {
    if tier_rank(left) <= tier_rank(right) {
        left
    } else {
        right
    }
}

fn tier_rank(tier: AnalysisTier) -> u8 {
    match tier {
        AnalysisTier::TierA => 4,
        AnalysisTier::TierB => 3,
        AnalysisTier::TierC => 2,
        AnalysisTier::TierD => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId, content_hash};
    use rootlight_ir::{
        Confidence, CoverageStatus, FactEvidence, OccurrenceRecord, OccurrenceRole,
        OccurrenceTarget, SourceRef, SourceSpan,
    };

    use crate::{CompletenessAssumption, ResolutionExplanation};

    use super::{
        RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionOutcome,
        ResolutionRule, apply_target,
    };

    #[test]
    fn candidate_targets_are_canonical_before_identity_derivation() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let spelling = content_hash(b"candidate");
        let mut occurrence = OccurrenceRecord {
            id: FactId::from_bytes([4; 20]),
            repository,
            generation,
            file,
            source: SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, 1).expect("source span is valid"),
                content_hash(b"x"),
                None,
            ),
            role: OccurrenceRole::Reference,
            enclosing: None,
            target: OccurrenceTarget::Unresolved {
                text_hash: spelling,
            },
            syntactic_text_hash: spelling,
            syntax_kind: "identifier".to_owned(),
            provenance: FactId::from_bytes([5; 20]),
            confidence: Confidence::new(0).expect("confidence is valid"),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        };
        let lower = SymbolId::from_bytes([6; 20]);
        let higher = SymbolId::from_bytes([7; 20]);
        let decision = ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Candidates {
                symbols: vec![higher, lower],
                total_count: 2,
                completeness: CoverageStatus::Complete,
                confidence: Confidence::new(800).expect("confidence is valid"),
            },
            explanation: ResolutionExplanation {
                rule: ResolutionRule::LexicalScope,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: Vec::new(),
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![CompletenessAssumption::ValidatedNormalizedDocument],
            },
        };

        apply_target(&mut occurrence, &decision);

        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Candidates {
                symbols: vec![lower, higher],
                total_count: 2,
                completeness: CoverageStatus::Complete,
            }
        );
    }
}
