//! Provenance-aware application of semantic decisions to normalized IR.
//!
//! Application recomputes every changed occurrence or relation identity and
//! fails closed when an existing derived-fact chain would require a wider
//! identity cascade than this resolver stage owns.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, EvidenceKind, FactEvidence, FactRef, IrLimits,
    NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    ProducerKind, ProvenanceRecord, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef,
    canonicalize_ir_document, derive_occurrence_record_id, derive_provenance_record_id,
    derive_relation_record_id,
};

use crate::{
    AppliedResolution, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision,
    ResolutionEngine, ResolutionError, ResolutionOutcome, ResolutionRule, ResolverFactContext,
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
        let occurrence_indexes = document
            .occurrences
            .iter()
            .enumerate()
            .map(|(index, occurrence)| (occurrence.id, index))
            .collect::<BTreeMap<_, _>>();
        let mut occurrence_remap = BTreeMap::new();
        let mut pending_relations = Vec::new();
        let producer = resolver_producer(self.limits.candidate_limit())?;

        for decision in &batch.decisions {
            cancellation.check()?;
            if matches!(decision.outcome, ResolutionOutcome::Unresolved { .. }) {
                continue;
            }
            let index = *occurrence_indexes
                .get(&decision.occurrence)
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            let provenance = build_provenance(
                &document,
                &document.occurrences[index],
                decision,
                &producer,
                context,
            )?;
            let provenance_id = provenance.id;
            if !document
                .provenance
                .iter()
                .any(|existing| existing.id == provenance_id)
            {
                document.provenance.push(provenance);
            }

            let occurrence = &mut document.occurrences[index];
            let old_id = occurrence.id;
            apply_target(occurrence, decision);
            occurrence.provenance = provenance_id;
            merge_candidate_derivations(&mut occurrence.evidence, decision);
            occurrence.id =
                derive_occurrence_record_id(occurrence).map_err(ResolutionError::FactIdentity)?;
            occurrence_remap.insert(old_id, occurrence.id);
            pending_relations.extend(relation_specs(occurrence, decision));
        }

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

fn resolver_producer(candidate_limit: usize) -> Result<ProducerIdentity, ResolutionError> {
    let mut configuration = Vec::with_capacity(RESOLVER_PROVIDER_VERSION.len() + 8);
    configuration.extend_from_slice(RESOLVER_PROVIDER_VERSION.as_bytes());
    let limit = u64::try_from(candidate_limit).map_err(|_| ResolutionError::CountOverflow)?;
    configuration.extend_from_slice(&limit.to_be_bytes());
    ProducerIdentity::new(
        RESOLVER_PROVIDER_NAME,
        env!("CARGO_PKG_VERSION"),
        content_hash(&configuration),
    )
    .map_err(ResolutionError::InvalidProducer)
}

fn build_provenance(
    document: &NormalizedIrDocument,
    occurrence: &OccurrenceRecord,
    decision: &ResolutionDecision,
    producer: &ProducerIdentity,
    context: ResolverFactContext,
) -> Result<ProvenanceRecord, ResolutionError> {
    let parent = document
        .provenance
        .iter()
        .find(|record| record.id == occurrence.provenance)
        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
    let mut sources = vec![occurrence.source.clone()];
    let mut derivation_parents = Vec::new();
    let mut context_digests = vec![parent.build_context.digest()];
    let mut tier = parent.tier;

    for candidate in &decision.explanation.candidates {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == candidate.symbol)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        derivation_parents.push(FactRef::Entity(entity.id));
        tier = lower_tier(tier, entity.tier);
        if let Some(source) = &entity.evidence.source
            && !sources.contains(source)
        {
            sources.push(source.clone());
        }
        let entity_provenance = document
            .provenance
            .iter()
            .find(|record| record.id == entity.provenance)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        context_digests.push(entity_provenance.build_context.digest());
    }
    derivation_parents.sort_unstable();
    derivation_parents.dedup();
    context_digests.sort_unstable();
    context_digests.dedup();

    let mut context_bytes = Vec::with_capacity(context_digests.len().saturating_mul(32));
    for digest in context_digests {
        context_bytes.extend_from_slice(digest.as_bytes());
    }
    let language = document
        .files
        .iter()
        .find(|file| file.id == occurrence.file)
        .map(|file| file.language.clone())
        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
    let rule = match decision.explanation.rule {
        ResolutionRule::LexicalScope => "scope-v1.lexical_scope",
        ResolutionRule::Import => "scope-v1.import",
    };
    let mut record = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
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
            occurrence.target = OccurrenceTarget::Candidates {
                symbols: symbols.clone(),
                total_count: *total_count,
                completeness: *completeness,
            };
            occurrence.confidence = *confidence;
        }
        ResolutionOutcome::Unresolved { .. } => {}
    }
}

fn merge_candidate_derivations(evidence: &mut FactEvidence, decision: &ResolutionDecision) {
    evidence.derivation.extend(
        decision
            .explanation
            .candidates
            .iter()
            .map(|candidate| FactRef::Entity(candidate.symbol)),
    );
    evidence.derivation.sort_unstable();
    evidence.derivation.dedup();
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
