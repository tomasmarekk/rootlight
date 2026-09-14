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
    FactRef, IrDocumentValidationError, IrLimits, NormalizedIrDocument, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, canonicalize_ir_document,
    derive_occurrence_record_id, derive_provenance_record_id, derive_relation_record_id,
    validate_ir_document,
};

use crate::{
    AppliedResolution, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision,
    ResolutionEngine, ResolutionError, ResolutionOutcome, ResolutionRule, ResolverFactContext,
    engine::{CandidateIndex, ResolutionWorkBudget},
};

impl ResolutionEngine {
    /// Applies semantic decisions to a normalized IR document.
    ///
    /// Exact decisions emit their role-specific relation; proven implicit
    /// getter reads also emit a call. Ambiguous calls emit
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
        let ir_limits = IrLimits::default();
        validate_ir_document(&document, &ir_limits, &ExtensionSupport::default())
            .map_err(ResolutionError::InvalidDocument)?;
        self.invalidate_perl_package_targets(&mut document, context, cancellation)?;
        let mut output_budget = OutputRecordBudget::from_document(&document, &ir_limits);
        let batch = self.resolve(&document, cancellation)?;
        let lookup = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            &document.extensions,
            &document.coverage_records,
            &document.occurrences,
            cancellation,
        )?;
        let occurrence_indexes = document
            .occurrences
            .iter()
            .enumerate()
            .map(|(index, occurrence)| (occurrence.id, index))
            .collect::<BTreeMap<_, _>>();
        let mut occurrence_remap = BTreeMap::new();
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
                &lookup,
                &document.occurrences[index],
                decision,
                &producer,
                context,
                None,
            )?;
            let provenance_id = provenance.id;
            if known_provenance.insert(provenance_id) {
                output_budget.reserve_provenance()?;
                new_provenance.push(provenance);
            }

            let semantics = relation_semantics(&document.occurrences[index], decision, &lookup);
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
            append_relation_specs(
                occurrence,
                decision,
                semantics,
                &mut document.relations,
                &mut output_budget,
                cancellation,
            )?;
        }
        drop(lookup);
        document.provenance.append(&mut new_provenance);

        ensure_nonrelation_remap_is_safe(&document, &occurrence_remap)?;
        let relation_remap =
            remap_existing_relations(&mut document.relations, &occurrence_remap, cancellation)?;
        ensure_relation_remap_is_safe(&document, &relation_remap)?;
        reconcile_perl_target_gaps(&mut document, cancellation)?;
        reconcile_groovy_import_gaps(&mut document, cancellation)?;
        let document = canonicalize_ir_document(document, &ir_limits, &Default::default())
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
        document: NormalizedIrDocument,
        context: ResolverFactContext,
        cancellation: &Cancellation,
    ) -> Result<NormalizedIrDocument, ResolutionError> {
        self.apply_document_with_work_selection(document, context, cancellation, false, &[])
            .map(|(document, _)| document)
    }

    /// Applies as many semantic decisions as fit the operation-wide work limit.
    ///
    /// Sites requiring fewer same-name candidate inspections are selected
    /// first, with occurrence identity as the deterministic tie-breaker. The
    /// returned estimate always describes the complete pass, so callers can
    /// explicitly mark facts that remain bounded.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionError`] when validation, bounded application,
    /// provenance construction, identity remapping, or canonicalization fails.
    pub fn apply_document_bounded(
        &self,
        document: NormalizedIrDocument,
        context: ResolverFactContext,
        cancellation: &Cancellation,
    ) -> Result<(NormalizedIrDocument, crate::ResolutionWorkEstimate), ResolutionError> {
        self.apply_document_with_work_selection(document, context, cancellation, true, &[])
    }

    /// Resolves native facts with exact retained classic BASIC project sources.
    ///
    /// Membership is read only from indexed declarative project files; source
    /// bytes must match the generation. Missing context never enables name scoring.
    ///
    /// # Errors
    /// Returns [`ResolutionError`] for invalid source identity or the same bounded
    /// validation, application and cancellation failures as [`Self::apply_document_bounded`].
    pub fn apply_document_bounded_with_classic_sources(
        &self,
        document: NormalizedIrDocument,
        context: ResolverFactContext,
        sources: &[crate::ClassicProjectSource<'_>],
        cancellation: &Cancellation,
    ) -> Result<(NormalizedIrDocument, crate::ResolutionWorkEstimate), ResolutionError> {
        self.apply_document_with_work_selection(document, context, cancellation, true, sources)
    }

    fn apply_document_with_work_selection(
        &self,
        mut document: NormalizedIrDocument,
        context: ResolverFactContext,
        cancellation: &Cancellation,
        bounded: bool,
        sources: &[crate::ClassicProjectSource<'_>],
    ) -> Result<(NormalizedIrDocument, crate::ResolutionWorkEstimate), ResolutionError> {
        cancellation.check()?;
        let ir_limits = IrLimits::default();
        validate_ir_document(&document, &ir_limits, &ExtensionSupport::default())
            .map_err(ResolutionError::InvalidDocument)?;
        self.invalidate_perl_package_targets(&mut document, context, cancellation)?;
        let mut output_budget = OutputRecordBudget::from_document(&document, &ir_limits);

        let mut lookup = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            &document.extensions,
            &document.coverage_records,
            &document.occurrences,
            cancellation,
        )?;
        let mut work = ResolutionWorkBudget::new(self.limits.work_limit());
        lookup.classic = match crate::classic::ClassicIndex::build(
            &lookup,
            &document.coverage_records,
            &document.occurrences,
            sources,
            cancellation,
            &mut work,
        ) {
            Ok(index) => index,
            Err(ResolutionError::WorkLimit { maximum }) if bounded => {
                // Partial membership cannot establish uniqueness. Preserve the
                // input facts and let the host expose bounded semantic coverage.
                drop(lookup);
                let document = canonicalize_ir_document(document, &ir_limits, &Default::default())
                    .map_err(ResolutionError::InvalidDocument)?;
                return Ok((
                    document,
                    crate::ResolutionWorkEstimate {
                        required: maximum
                            .checked_add(1)
                            .ok_or(ResolutionError::CountOverflow)?,
                        limit: maximum,
                    },
                ));
            }
            Err(error) => return Err(error),
        };
        let mut required = work.consumed();
        let mut sites = Vec::new();
        for occurrence in &document.occurrences {
            cancellation.check()?;
            let Some(site_work) = lookup.occurrence_work(occurrence)? else {
                continue;
            };
            required = required
                .checked_add(site_work)
                .ok_or(ResolutionError::CountOverflow)?;
            sites.push((site_work, occurrence.id));
        }
        let estimate = crate::ResolutionWorkEstimate {
            required,
            limit: self.limits.work_limit(),
        };
        let selected = if estimate.fits() {
            None
        } else if bounded {
            sites.sort_unstable();
            let mut remaining = work.remaining();
            let mut selected = BTreeSet::new();
            for (site_work, occurrence) in sites {
                if site_work > remaining {
                    break;
                }
                remaining -= site_work;
                selected.insert(occurrence);
            }
            Some(selected)
        } else {
            return Err(ResolutionError::WorkLimit {
                maximum: estimate.limit,
            });
        };
        let producer = resolver_producer(self.limits, &self.policy)?;
        let mut known_provenance = document
            .provenance
            .iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        let mut new_provenance = Vec::new();
        let mut occurrence_remap = BTreeMap::new();
        let mut project_roots = BTreeMap::new();

        for occurrence in &mut document.occurrences {
            cancellation.check()?;
            if lookup.occurrence_work(occurrence)?.is_none() {
                continue;
            }
            if selected
                .as_ref()
                .is_some_and(|selected| !selected.contains(&occurrence.id))
            {
                continue;
            }
            work.consume()?;
            let decision = self.resolve_occurrence(occurrence, &lookup, &mut work, cancellation)?;
            if matches!(decision.outcome, ResolutionOutcome::Unresolved { .. }) {
                continue;
            }
            let project_root = if let Some(proof) = lookup.classic.proof(occurrence) {
                let manifest = proof.context.manifest;
                if let Some(root) = project_roots.get(&manifest) {
                    Some(*root)
                } else {
                    let file = lookup
                        .files
                        .get(&manifest)
                        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                    let parent = lookup
                        .provenance
                        .get(&file.provenance)
                        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                    let mut template = (**parent).clone();
                    template.producer_kind = ProducerKind::Rule;
                    template.producer = producer.clone();
                    template.binary_digest = context.binary_digest();
                    template.frontend_version = Some(RESOLVER_PROVIDER_VERSION.to_owned());
                    let root = append_shared_provenance(
                        ProvenanceTree {
                            sources: &proof.context.sources,
                            facts: &proof.context.facts,
                            rule: "classic-project-membership-v1",
                        },
                        template,
                        &ir_limits,
                        &mut output_budget,
                        &mut known_provenance,
                        &mut new_provenance,
                        cancellation,
                    )?;
                    project_roots.insert(manifest, root);
                    Some(root)
                }
            } else {
                None
            };
            let provenance = build_provenance(
                &lookup,
                occurrence,
                &decision,
                &producer,
                context,
                project_root,
            )?;
            let provenance_id = provenance.id;
            if known_provenance.insert(provenance_id) {
                output_budget.reserve_provenance()?;
                new_provenance.push(provenance);
            }

            let semantics = relation_semantics(occurrence, &decision, &lookup);
            let old_id = occurrence.id;
            if lookup.classic.is_call(occurrence) {
                // Only the native invocation position plus a project-owned method
                // target establishes a call; an invoked field remains a reference.
                occurrence.role = OccurrenceRole::CallSite;
            }
            apply_target(occurrence, &decision);
            occurrence.provenance = provenance_id;
            occurrence.id =
                derive_occurrence_record_id(occurrence).map_err(ResolutionError::FactIdentity)?;
            occurrence_remap.insert(old_id, occurrence.id);
            append_relation_specs(
                occurrence,
                &decision,
                semantics,
                &mut document.relations,
                &mut output_budget,
                cancellation,
            )?;
        }
        drop(lookup);
        document.provenance.append(&mut new_provenance);

        ensure_nonrelation_remap_is_safe(&document, &occurrence_remap)?;
        let relation_remap =
            remap_existing_relations(&mut document.relations, &occurrence_remap, cancellation)?;
        ensure_relation_remap_is_safe(&document, &relation_remap)?;
        reconcile_perl_target_gaps(&mut document, cancellation)?;
        reconcile_groovy_import_gaps(&mut document, cancellation)?;
        reconcile_classic_target_gaps(&mut document, cancellation)?;
        let document = canonicalize_ir_document(document, &ir_limits, &Default::default())
            .map_err(ResolutionError::InvalidDocument)?;
        Ok((document, estimate))
    }

    fn invalidate_perl_package_targets(
        &self,
        document: &mut NormalizedIrDocument,
        context: ResolverFactContext,
        cancellation: &Cancellation,
    ) -> Result<(), ResolutionError> {
        if !document
            .extensions
            .iter()
            .any(|record| record.namespace == rootlight_ir::PERL_BINDING_NAMESPACE)
        {
            return Ok(());
        }
        let lookup = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            &document.extensions,
            &document.coverage_records,
            &document.occurrences,
            cancellation,
        )?;
        let producer = resolver_producer(self.limits, &self.policy)?;
        let mut invalidations = BTreeMap::new();
        for occurrence in &document.occurrences {
            cancellation.check()?;
            let Some(evidence) = lookup.perl.conflicting_write(occurrence) else {
                continue;
            };
            let mut provenance = (*lookup
                .provenance
                .get(&occurrence.provenance)
                .copied()
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?)
            .clone();
            let mut contexts = Vec::new();
            let mut sources = vec![occurrence.source.clone()];
            let mut parents = Vec::new();
            for envelope in evidence {
                let parent = lookup
                    .provenance
                    .get(&envelope.provenance)
                    .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                contexts.push(parent.build_context.digest());
                provenance.tier = lower_tier(provenance.tier, parent.tier);
                sources.push(
                    envelope
                        .evidence
                        .source
                        .clone()
                        .ok_or(ResolutionError::UnsupportedIdentityRemap)?,
                );
                parents.push(FactRef::Fact(envelope.id));
            }
            contexts.sort_unstable();
            contexts.dedup();
            let context_bytes: Vec<_> = contexts
                .iter()
                .flat_map(|hash| hash.as_bytes().iter().copied())
                .collect();
            sources.sort_unstable();
            sources.dedup();
            parents.sort_unstable();
            parents.dedup();
            provenance.producer_kind = ProducerKind::Rule;
            provenance.producer = producer.clone();
            provenance.binary_digest = context.binary_digest();
            provenance.frontend_version = Some(RESOLVER_PROVIDER_VERSION.to_owned());
            provenance.build_context = BuildContextIdentity::new(content_hash(&context_bytes));
            provenance.input_sources = sources.clone();
            provenance.evidence_sources = sources;
            provenance.derivation_parents = parents;
            provenance.rule = Some("perl-package-storage-v1.conflicting_write".to_owned());
            provenance.id =
                derive_provenance_record_id(&provenance).map_err(ResolutionError::FactIdentity)?;
            invalidations.insert(occurrence.id, provenance);
        }
        drop(lookup);
        if invalidations.is_empty() {
            return Ok(());
        }

        // Retracting contradicted certainty is mandatory, not a candidate search
        // that can be skipped when the bounded semantic work allowance runs out.
        let limits = IrLimits::default();
        let mut budget = OutputRecordBudget::from_document(document, &limits);
        let mut known_provenance: BTreeSet<_> =
            document.provenance.iter().map(|record| record.id).collect();
        let mut remap = BTreeMap::new();
        for occurrence in &mut document.occurrences {
            cancellation.check()?;
            let Some(provenance) = invalidations.remove(&occurrence.id) else {
                continue;
            };
            if known_provenance.insert(provenance.id) {
                budget.reserve_provenance()?;
                document.provenance.push(provenance.clone());
            }
            let old = occurrence.id;
            if let OccurrenceTarget::Resolved { symbol } = occurrence.target {
                occurrence
                    .evidence
                    .derivation
                    .retain(|parent| *parent != FactRef::Entity(symbol));
            }
            occurrence.target = OccurrenceTarget::Unresolved {
                text_hash: occurrence.syntactic_text_hash,
            };
            occurrence.confidence = Confidence::new(0).expect("zero confidence is valid");
            occurrence.provenance = provenance.id;
            occurrence.id =
                derive_occurrence_record_id(occurrence).map_err(ResolutionError::FactIdentity)?;
            remap.insert(old, occurrence.id);
            budget.reserve_skipped_region(document.skipped_regions.len(), &limits)?;
            let mut gap = rootlight_ir::SkippedRegion {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                source: occurrence.source.clone(),
                domain: rootlight_ir::FactDomain::Relations,
                reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
                detail: "perl-project-code-storage-write".to_owned(),
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(occurrence.source.clone()),
                    derivation: Vec::new(),
                },
            };
            gap.id = rootlight_ir::derive_skipped_region_id(&gap)
                .map_err(ResolutionError::FactIdentity)?;
            document.skipped_regions.push(gap);
        }
        let mut removed = BTreeMap::new();
        for relation in &document.relations {
            cancellation.check()?;
            if matches!(relation.subject, RelationEndpoint::Occurrence(id) if remap.contains_key(&id))
                && matches!(
                    relation.predicate,
                    RelationPredicate::Calls | RelationPredicate::DispatchCandidate
                )
            {
                removed.insert(relation.id, relation.id);
            }
        }
        document
            .relations
            .retain(|relation| !removed.contains_key(&relation.id));
        ensure_relation_remap_is_safe(document, &removed)?;
        ensure_nonrelation_remap_is_safe(document, &remap)?;
        let relations = remap_existing_relations(&mut document.relations, &remap, cancellation)?;
        ensure_relation_remap_is_safe(document, &relations)?;
        validate_ir_document(document, &limits, &ExtensionSupport::default())
            .map_err(ResolutionError::InvalidDocument)?;
        Ok(())
    }
}

fn reconcile_classic_target_gaps(
    document: &mut NormalizedIrDocument,
    cancellation: &Cancellation,
) -> Result<(), ResolutionError> {
    use rootlight_ir::{CoverageScope, CoverageStatus, FactDomain, SkippedRegionReason};

    let mut producers = BTreeMap::new();
    for record in &document.provenance {
        cancellation.check()?;
        let selection: (&[&str], &str) = match record.rule.as_deref() {
            Some("classic-project-type-v1.indexed_snapshot") => (
                &["vba.direct_type_identifier.reference"],
                "vba-type-target-unavailable",
            ),
            Some("classic-project-member-v1.indexed_snapshot") => (
                &["vba.member.reference", "vba.invoked_member.reference"],
                "vba-member-target-unavailable",
            ),
            _ => continue,
        };
        producers.insert(record.id, (selection, record));
    }
    if producers.is_empty() {
        return Ok(());
    }
    let files: BTreeMap<_, _> = document.files.iter().map(|file| (file.id, file)).collect();
    let mut resolved = BTreeMap::new();
    for site in &document.occurrences {
        cancellation.check()?;
        if let Some(((syntax, detail), _)) = producers.get(&site.provenance)
            && syntax.contains(&site.syntax_kind.as_str())
            && matches!(site.target, OccurrenceTarget::Resolved { .. })
        {
            resolved.insert((site.source.clone(), *detail), site.provenance);
        }
    }
    let mut removed = BTreeMap::new();
    let mut native_counts = BTreeMap::<_, u64>::new();
    let mut recovered = BTreeMap::<_, (u64, BTreeSet<FactRef>)>::new();
    for gap in &document.skipped_regions {
        cancellation.check()?;
        let Some(file) = files.get(&gap.source.span().file()) else {
            continue;
        };
        if gap.domain != FactDomain::Relations || gap.provenance != file.provenance {
            continue;
        }
        let count = native_counts.entry(file.id).or_default();
        *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
        if gap.reason != SkippedRegionReason::UnsupportedConstruct {
            continue;
        }
        if let Some(proof) = resolved.get(&(gap.source.clone(), gap.detail.as_str())) {
            removed.insert(gap.id, gap.id);
            let (count, proofs) = recovered.entry(file.id).or_default();
            *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
            proofs.insert(FactRef::Fact(*proof));
        }
    }
    if removed.is_empty() {
        return Ok(());
    }
    let limits = IrLimits::default();
    let mut budget = OutputRecordBudget::from_document(document, &limits);
    let mut known = document.provenance.iter().map(|p| p.id).collect();
    let mut added = Vec::new();
    let mut updated_files = BTreeSet::new();
    for coverage in &mut document.coverage_records {
        cancellation.check()?;
        let CoverageScope::File(file_id) = coverage.scope else {
            continue;
        };
        if coverage.domain != FactDomain::Relations {
            continue;
        }
        let Some((count, proofs)) = recovered.get(&file_id) else {
            continue;
        };
        let file = files
            .get(&file_id)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        if coverage.provenance != file.provenance
            || coverage.evidence.source != file.evidence.source
            || coverage.status != CoverageStatus::Bounded
            || native_counts.get(&file_id) != Some(&coverage.skipped)
            || !updated_files.insert(file_id)
        {
            return Err(ResolutionError::UnsupportedIdentityRemap);
        }
        let proof = proofs
            .iter()
            .find_map(|proof| match proof {
                FactRef::Fact(id) => producers.get(id).map(|(_, record)| *record),
                _ => None,
            })
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        let mut facts = proofs.clone();
        facts.insert(FactRef::Fact(file.provenance));
        let facts: Vec<_> = facts.into_iter().collect();
        let sources: Vec<_> = file.evidence.source.iter().cloned().collect();
        let root = append_shared_provenance(
            ProvenanceTree {
                sources: &sources,
                facts: &facts,
                rule: "classic-project-coverage-v1",
            },
            proof.clone(),
            &limits,
            &mut budget,
            &mut known,
            &mut added,
            cancellation,
        )?;
        let old = coverage.id;
        // One native target gap represents one absent role-specific relation.
        // Recover that unit without changing the discovered-unit denominator.
        coverage.skipped = coverage
            .skipped
            .checked_sub(*count)
            .ok_or(ResolutionError::CountOverflow)?;
        coverage.indexed = coverage
            .indexed
            .checked_add(*count)
            .ok_or(ResolutionError::CountOverflow)?;
        if coverage.skipped == 0 {
            coverage.status = CoverageStatus::Complete;
        }
        coverage.provenance = root;
        coverage.evidence.derivation.push(FactRef::Fact(root));
        coverage.evidence.derivation.sort_unstable();
        coverage.evidence.derivation.dedup();
        coverage.id = rootlight_ir::derive_coverage_record_id(coverage)
            .map_err(ResolutionError::FactIdentity)?;
        removed.insert(old, coverage.id);
    }
    if updated_files.len() != recovered.len() {
        return Err(ResolutionError::UnsupportedIdentityRemap);
    }
    document.provenance.extend(added);
    ensure_relation_remap_is_safe(document, &removed)?;
    document
        .skipped_regions
        .retain(|gap| !removed.contains_key(&gap.id));
    Ok(())
}

fn reconcile_perl_target_gaps(
    document: &mut NormalizedIrDocument,
    cancellation: &Cancellation,
) -> Result<(), ResolutionError> {
    let mut producers = BTreeSet::new();
    for provenance in &document.provenance {
        cancellation.check()?;
        if provenance.rule.as_deref() == Some("perl-package-storage-v1.indexed_snapshot") {
            producers.insert(provenance.id);
        }
    }
    if producers.is_empty() {
        return Ok(());
    }
    let mut sources = BTreeMap::new();
    for occurrence in &document.occurrences {
        cancellation.check()?;
        if producers.contains(&occurrence.provenance)
            && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
        {
            sources.insert(occurrence.source.clone(), occurrence.provenance);
        }
    }
    let mut remap = BTreeMap::new();
    for gap in &mut document.skipped_regions {
        cancellation.check()?;
        if gap.detail == "perl-function-target-unavailable"
            && let Some(&provenance) = sources.get(&gap.source)
        {
            let old = gap.id;
            // The declaration is now known, but static package matching did not
            // execute the loader or establish its runtime CODE value.
            gap.detail = "perl-runtime-code-value-unverified".to_owned();
            gap.provenance = provenance;
            gap.id = rootlight_ir::derive_skipped_region_id(gap)
                .map_err(ResolutionError::FactIdentity)?;
            remap.insert(old, gap.id);
        }
    }
    ensure_nonrelation_remap_is_safe(document, &remap)?;
    if document
        .relations
        .iter()
        .any(|relation| contains_remapped_fact(&relation.evidence.derivation, &remap))
    {
        return Err(ResolutionError::UnsupportedIdentityRemap);
    }
    Ok(())
}

fn reconcile_groovy_import_gaps(
    document: &mut NormalizedIrDocument,
    cancellation: &Cancellation,
) -> Result<(), ResolutionError> {
    let mut producers = BTreeSet::new();
    for provenance in &document.provenance {
        cancellation.check()?;
        if matches!(
            provenance.rule.as_deref(),
            Some(
                "groovy-type-import-v1.indexed_snapshot"
                    | "groovy-static-import-v1.indexed_snapshot"
            )
        ) {
            producers.insert(provenance.id);
        }
    }
    if producers.is_empty() {
        return Ok(());
    }
    let mut files = BTreeMap::new();
    for file in &document.files {
        cancellation.check()?;
        if file.language == "groovy" {
            files.insert(file.id, file.provenance);
        }
    }
    let mut sources = BTreeMap::new();
    for occurrence in &document.occurrences {
        cancellation.check()?;
        if matches!(
            occurrence.syntax_kind.as_str(),
            "groovy.type_import.reference"
                | "groovy.static_import.reference"
                | "groovy.static_star_import.reference"
        ) && occurrence.role == OccurrenceRole::ImportUse
            && producers.contains(&occurrence.provenance)
        {
            let detail = match &occurrence.target {
                OccurrenceTarget::Resolved { .. } => "groovy-import-build-context-unverified",
                OccurrenceTarget::Candidates { total_count, .. }
                    if occurrence.syntax_kind == "groovy.static_import.reference"
                        && *total_count > 1 =>
                {
                    "groovy-import-candidates-build-context-unverified"
                }
                _ => continue,
            };
            sources
                .entry((occurrence.file, occurrence.source.span().start_byte()))
                .and_modify(|prior| *prior = None)
                .or_insert(Some((&occurrence.source, occurrence.provenance, detail)));
        }
    }
    let mut remap = BTreeMap::new();
    for gap in &mut document.skipped_regions {
        cancellation.check()?;
        let span = gap.source.span();
        let native_gap = gap.detail == "unresolved-import-target"
            && files.get(&span.file()) == Some(&gap.provenance);
        let derived_gap = matches!(
            gap.detail.as_str(),
            "groovy-import-build-context-unverified"
                | "groovy-import-candidates-build-context-unverified"
        ) && producers.contains(&gap.provenance);
        if gap.domain != rootlight_ir::FactDomain::Relations || !(native_gap || derived_gap) {
            continue;
        }
        // Import gaps cover the statement, while target proofs cover only its name.
        // At most two lookups detect ambiguity without scanning overlapping ranges.
        let mut enclosed =
            sources.range((span.file(), span.start_byte())..=(span.file(), span.end_byte()));
        let Some((source, provenance, detail)) = enclosed.next().and_then(|(_, entry)| *entry)
        else {
            continue;
        };
        if enclosed.next().is_some()
            || source.span().end_byte() > span.end_byte()
            || source.content_hash() != gap.source.content_hash()
        {
            continue;
        }
        let old = gap.id;
        gap.detail = detail.to_owned();
        gap.reason = rootlight_ir::SkippedRegionReason::MissingBuildContext;
        gap.provenance = provenance;
        gap.id =
            rootlight_ir::derive_skipped_region_id(gap).map_err(ResolutionError::FactIdentity)?;
        if old != gap.id {
            remap.insert(old, gap.id);
        }
    }
    ensure_relation_remap_is_safe(document, &remap)
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

struct OutputRecordBudget {
    relations: usize,
    provenance: usize,
    total_records: usize,
    max_relations: usize,
    max_provenance: usize,
    max_total_records: usize,
}

impl OutputRecordBudget {
    fn from_document(document: &NormalizedIrDocument, limits: &IrLimits) -> Self {
        let total_records = [
            document.files.len(),
            document.entities.len(),
            document.occurrences.len(),
            document.relations.len(),
            document.provenance.len(),
            document.source_mappings.len(),
            document.coverage_records.len(),
            document.skipped_regions.len(),
            document.diagnostics.len(),
            document.extensions.len(),
        ]
        .into_iter()
        .fold(0usize, usize::saturating_add);
        Self {
            relations: document.relations.len(),
            provenance: document.provenance.len(),
            total_records,
            max_relations: limits.max_relations,
            max_provenance: limits.max_provenance_records,
            max_total_records: limits.max_total_records,
        }
    }

    fn reserve_provenance(&mut self) -> Result<(), ResolutionError> {
        let provenance = self.provenance.saturating_add(1);
        if provenance > self.max_provenance {
            return Err(ResolutionError::InvalidDocument(
                IrDocumentValidationError::CollectionLimit {
                    collection: "provenance",
                    observed: provenance,
                    limit: self.max_provenance,
                },
            ));
        }
        let total_records = self.reserve_total()?;
        self.provenance = provenance;
        self.total_records = total_records;
        Ok(())
    }

    fn reserve_relation(&mut self) -> Result<(), ResolutionError> {
        let relations = self.relations.saturating_add(1);
        if relations > self.max_relations {
            return Err(ResolutionError::InvalidDocument(
                IrDocumentValidationError::CollectionLimit {
                    collection: "relations",
                    observed: relations,
                    limit: self.max_relations,
                },
            ));
        }
        let total_records = self.reserve_total()?;
        self.relations = relations;
        self.total_records = total_records;
        Ok(())
    }

    fn reserve_skipped_region(
        &mut self,
        current: usize,
        limits: &IrLimits,
    ) -> Result<(), ResolutionError> {
        let observed = current.saturating_add(1);
        if observed > limits.max_skipped_regions {
            return Err(ResolutionError::InvalidDocument(
                IrDocumentValidationError::CollectionLimit {
                    collection: "skipped_regions",
                    observed,
                    limit: limits.max_skipped_regions,
                },
            ));
        }
        self.total_records = self.reserve_total()?;
        Ok(())
    }

    fn reserve_total(&self) -> Result<usize, ResolutionError> {
        let total_records = self.total_records.saturating_add(1);
        if total_records > self.max_total_records {
            return Err(ResolutionError::InvalidDocument(
                IrDocumentValidationError::TotalRecordLimit {
                    observed: total_records,
                    limit: self.max_total_records,
                },
            ));
        }
        Ok(total_records)
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

struct ProvenanceTree<'a> {
    sources: &'a [SourceRef],
    facts: &'a [FactRef],
    rule: &'static str,
}

fn append_shared_provenance(
    evidence: ProvenanceTree<'_>,
    mut template: ProvenanceRecord,
    limits: &IrLimits,
    budget: &mut OutputRecordBudget,
    known: &mut BTreeSet<FactId>,
    output: &mut Vec<ProvenanceRecord>,
    cancellation: &Cancellation,
) -> Result<FactId, ResolutionError> {
    let width = limits.max_nested_items_per_record;
    if width < 2 {
        return Err(ResolutionError::InvalidDocument(
            IrDocumentValidationError::CollectionLimit {
                collection: "provenance.derivation_parents",
                observed: 2,
                limit: width,
            },
        ));
    }
    template.input_sources.clear();
    template.evidence_sources.clear();
    template.derivation_parents.clear();
    template.rule = Some(format!("{}.leaf", evidence.rule));
    let count = evidence
        .sources
        .len()
        .max(evidence.facts.len())
        .div_ceil(width);
    let mut level = Vec::new();
    for index in 0..count {
        cancellation.check()?;
        // The inventory is shared across all uses. Sharding retains the entire
        // proof without raising IR limits or copying the inventory per reference.
        let mut record = template.clone();
        let start = index
            .checked_mul(width)
            .ok_or(ResolutionError::CountOverflow)?;
        record.input_sources = evidence
            .sources
            .iter()
            .skip(start)
            .take(width)
            .cloned()
            .collect();
        record.evidence_sources = record.input_sources.clone();
        record.derivation_parents = evidence
            .facts
            .iter()
            .skip(start)
            .take(width)
            .copied()
            .collect();
        record.id = derive_provenance_record_id(&record).map_err(ResolutionError::FactIdentity)?;
        level.push(FactRef::Fact(record.id));
        if known.insert(record.id) {
            budget.reserve_provenance()?;
            output.push(record);
        }
    }
    template.rule = Some(format!("{}.branch", evidence.rule));
    while level.len() > 1 {
        cancellation.check()?;
        level.sort_unstable();
        level.dedup();
        let mut next = Vec::new();
        for chunk in level.chunks(width) {
            cancellation.check()?;
            let mut record = template.clone();
            record.derivation_parents = chunk.to_vec();
            record.id =
                derive_provenance_record_id(&record).map_err(ResolutionError::FactIdentity)?;
            next.push(FactRef::Fact(record.id));
            if known.insert(record.id) {
                budget.reserve_provenance()?;
                output.push(record);
            }
        }
        level = next;
    }
    match level.first() {
        Some(FactRef::Fact(id)) => Ok(*id),
        _ => Err(ResolutionError::UnsupportedIdentityRemap),
    }
}

fn build_provenance(
    lookup: &CandidateIndex<'_>,
    occurrence: &OccurrenceRecord,
    decision: &ResolutionDecision,
    producer: &ProducerIdentity,
    context: ResolverFactContext,
    project_root: Option<FactId>,
) -> Result<ProvenanceRecord, ResolutionError> {
    // Candidate imports are selected again from immutable native evidence. Using
    // a prior resolver context here would nest its digest on every repeated pass.
    let parent_id = if decision.explanation.rule == ResolutionRule::GroovyStaticImport {
        lookup
            .files
            .get(&occurrence.file)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?
            .provenance
    } else {
        occurrence.provenance
    };
    let parent = lookup
        .provenance
        .get(&parent_id)
        .copied()
        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
    let mut sources = vec![occurrence.source.clone()];
    let mut derivation_parents = Vec::new();
    let mut context_digests = vec![parent.build_context.digest()];
    let mut tier = parent.tier;

    if matches!(
        decision.explanation.rule,
        ResolutionRule::ClassicProjectType | ResolutionRule::ClassicProjectMember
    ) {
        let proof = lookup
            .classic
            .proof(occurrence)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        derivation_parents.push(FactRef::Fact(
            project_root.ok_or(ResolutionError::UnsupportedIdentityRemap)?,
        ));
        if let Some((owner, source)) = &proof.receiver {
            derivation_parents.push(FactRef::Entity(owner.id));
            sources.push(source.clone());
        }
    }

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
    if matches!(
        decision.explanation.rule,
        ResolutionRule::PerlPackageStorage
    ) {
        for envelope in lookup
            .perl
            .evidence(occurrence)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?
        {
            derivation_parents.push(FactRef::Fact(envelope.id));
            if let Some(source) = &envelope.evidence.source {
                sources.push(source.clone());
            }
        }
    }
    if matches!(
        decision.explanation.rule,
        ResolutionRule::GroovyInheritedType | ResolutionRule::GroovyStaticImport
    ) {
        let entry = lookup
            .groovy
            .inherited_entry(occurrence)
            .or_else(|| lookup.groovy.static_import_entry(occurrence))
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        sources.extend(entry.selection.sources.iter().cloned());
        derivation_parents.extend(entry.selection.facts.iter().copied());
        derivation_parents.push(FactRef::Fact(entry.envelope.id));
        if let Some(source) = &entry.envelope.evidence.source {
            sources.push(source.clone());
        }
    }
    if matches!(
        decision.explanation.rule,
        ResolutionRule::GroovyTypeImport | ResolutionRule::GroovyQualifiedHeader
    ) {
        let proof = lookup
            .groovy
            .proof(occurrence)
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
        if let Some((envelope, owner)) = proof.qualified_header {
            derivation_parents.push(FactRef::Fact(envelope.id));
            derivation_parents.push(FactRef::Entity(owner.id));
            sources.extend(envelope.evidence.source.iter().cloned());
            sources.extend(owner.evidence.source.iter().cloned());
        }
        for entity in proof.owners {
            derivation_parents.push(FactRef::Entity(entity.id));
            if let Some(source) = &entity.evidence.source {
                sources.push(source.clone());
            }
        }
        if let Some(binding) = proof.binding {
            derivation_parents.push(FactRef::Fact(binding.id));
            if let Some(source) = &binding.evidence.source {
                sources.push(source.clone());
                let selection = rootlight_ir::decode_groovy_binding_envelope(binding)
                    .map_err(|_| ResolutionError::UnsupportedIdentityRemap)?;
                sources.push(rootlight_ir::SourceRef::new(
                    source.repository(),
                    source.generation(),
                    selection.import,
                    source.content_hash(),
                    None,
                ));
            }
        }
        for coverage in proof.coverage {
            derivation_parents.push(FactRef::Fact(coverage.id));
            if let Some(source) = &coverage.evidence.source {
                sources.push(source.clone());
            }
        }
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
        ResolutionRule::ClassicProjectType => "classic-project-type-v1.indexed_snapshot",
        ResolutionRule::ClassicProjectMember => "classic-project-member-v1.indexed_snapshot",
        ResolutionRule::GroovyStaticImport => "groovy-static-import-v1.indexed_snapshot",
        ResolutionRule::GroovyInheritedType => "groovy-inherited-type-v1.indexed_snapshot",
        ResolutionRule::GroovyQualifiedHeader => "groovy-qualified-header-v1.indexed_snapshot",
        ResolutionRule::GroovyTypeImport => "groovy-type-import-v1.indexed_snapshot",
        ResolutionRule::PerlPackageStorage => "perl-package-storage-v1.indexed_snapshot",
        ResolutionRule::LexicalScope => "scope-v1.lexical_scope",
        ResolutionRule::Import => "scope-v1.import",
    };
    let mut record = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository: occurrence.repository,
        generation: occurrence.generation,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RelationSemantics {
    RoleOnly,
    ImplicitAccessor,
}

fn relation_semantics(
    occurrence: &OccurrenceRecord,
    decision: &ResolutionDecision,
    lookup: &CandidateIndex<'_>,
) -> RelationSemantics {
    let ResolutionOutcome::Resolved { symbol, .. } = decision.outcome else {
        return RelationSemantics::RoleOnly;
    };
    // Selection belongs to the original occurrence identity, before application
    // remaps it. A method name or a common-IR target alone cannot prove a call.
    if occurrence.id == decision.occurrence
        && occurrence.role == OccurrenceRole::Reference
        && occurrence.syntax_kind == "groovy.identifier.reference"
        && decision.explanation.rule == ResolutionRule::GroovyStaticImport
        && lookup
            .groovy
            .static_import_entry(occurrence)
            .is_some_and(|entry| {
                entry.envelope.namespace == rootlight_ir::GROOVY_VALUE_LOOKUP_NAMESPACE
                    && entry.envelope.evidence.source.as_ref() == Some(&occurrence.source)
                    && entry.selection.target.is_some_and(|target| {
                        target.id == symbol && target.kind == rootlight_ir::EntityKind::Method
                    })
            })
    {
        RelationSemantics::ImplicitAccessor
    } else {
        RelationSemantics::RoleOnly
    }
}

fn append_relation_specs(
    occurrence: &OccurrenceRecord,
    decision: &ResolutionDecision,
    semantics: RelationSemantics,
    relations: &mut Vec<RelationRecord>,
    budget: &mut OutputRecordBudget,
    cancellation: &Cancellation,
) -> Result<(), ResolutionError> {
    let mut append = |relation: PendingRelation| -> Result<(), ResolutionError> {
        cancellation.check()?;
        budget.reserve_relation()?;
        relations.push(relation.into_record()?);
        Ok(())
    };
    match &decision.outcome {
        ResolutionOutcome::Resolved { symbol, confidence } => {
            if let Some(predicate) = relation_predicate(occurrence.role) {
                append(pending_relation(
                    occurrence,
                    *symbol,
                    predicate,
                    *confidence,
                    decision.explanation.rule,
                ))?;
            }
            if semantics == RelationSemantics::ImplicitAccessor {
                append(pending_relation(
                    occurrence,
                    *symbol,
                    RelationPredicate::Calls,
                    *confidence,
                    decision.explanation.rule,
                ))?;
            }
        }
        ResolutionOutcome::Candidates { symbols, .. }
            if occurrence.role == OccurrenceRole::CallSite =>
        {
            for symbol in symbols {
                let confidence = decision
                    .explanation
                    .candidates
                    .iter()
                    .find(|candidate| candidate.symbol == *symbol)
                    .map_or(occurrence.confidence, |candidate| candidate.score);
                append(pending_relation(
                    occurrence,
                    *symbol,
                    RelationPredicate::DispatchCandidate,
                    confidence,
                    decision.explanation.rule,
                ))?;
            }
        }
        ResolutionOutcome::Candidates { .. } | ResolutionOutcome::Unresolved { .. } => {}
    }
    Ok(())
}

fn pending_relation(
    occurrence: &OccurrenceRecord,
    target: SymbolId,
    predicate: RelationPredicate,
    confidence: Confidence,
    rule: ResolutionRule,
) -> PendingRelation {
    PendingRelation {
        repository: occurrence.repository,
        generation: occurrence.generation,
        occurrence: occurrence.id,
        subject: relation_subject(occurrence, predicate, rule),
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
    rule: ResolutionRule,
) -> RelationEndpoint {
    // Static-import selection proves the native enclosing scope as well as the
    // target. Other call rules retain their occurrence-based endpoint contract.
    let proven_call_owner =
        rule == ResolutionRule::GroovyStaticImport && predicate == RelationPredicate::Calls;
    if (proven_call_owner
        || matches!(
            predicate,
            RelationPredicate::Extends
                | RelationPredicate::Implements
                | RelationPredicate::Satisfies
                | RelationPredicate::Embeds
                | RelationPredicate::MixesIn
                | RelationPredicate::Overrides
        ))
        && let Some(enclosing) = occurrence.enclosing
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
    use rootlight_cancel::Cancellation;
    use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId, content_hash};
    use rootlight_ir::{
        Confidence, CoverageStatus, FactEvidence, IrDocumentValidationError, OccurrenceRecord,
        OccurrenceRole, OccurrenceTarget, SourceRef, SourceSpan,
    };

    use crate::{CompletenessAssumption, ResolutionError, ResolutionExplanation};

    use super::{
        OutputRecordBudget, RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision,
        ResolutionOutcome, ResolutionRule, append_relation_specs, apply_target,
    };

    #[test]
    fn shared_provenance_shards_preserve_all_inputs_and_reuse_identity() {
        use rootlight_ir::{
            AnalysisTier, BuildContextIdentity, FactRef, IrLimits, NormalizedIrDocument,
            ProducerIdentity, ProducerKind, ProvenanceRecord,
        };
        use std::collections::BTreeSet;

        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let mut limits = IrLimits::default();
        limits.max_nested_items_per_record = 2;
        let sources: Vec<_> = (0..9_u8)
            .map(|index| {
                SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(FileId::from_bytes([index; 20]), 0, 1).unwrap(),
                    content_hash(b"x"),
                    None,
                )
            })
            .collect();
        let facts: Vec<_> = sources
            .iter()
            .map(|source| FactRef::File(source.span().file()))
            .collect();
        let template = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            producer_kind: ProducerKind::Rule,
            producer: ProducerIdentity::new("rootlight-resolve", "1", content_hash(b"config"))
                .unwrap(),
            binary_digest: content_hash(b"binary"),
            frontend_version: None,
            language: "visual-basic-classic".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(content_hash(b"context")),
            input_sources: Vec::new(),
            evidence_sources: Vec::new(),
            derivation_parents: Vec::new(),
            rule: None,
        };
        let document = NormalizedIrDocument::empty(repository, generation);
        let mut budget = OutputRecordBudget::from_document(&document, &limits);
        let mut known = BTreeSet::new();
        let mut output = Vec::new();
        let mut roots = Vec::new();
        for _ in 0..2 {
            roots.push(
                super::append_shared_provenance(
                    super::ProvenanceTree {
                        sources: &sources,
                        facts: &facts,
                        rule: "classic-project-membership-v1",
                    },
                    template.clone(),
                    &limits,
                    &mut budget,
                    &mut known,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap(),
            );
        }
        assert_eq!(roots[0], roots[1]);
        assert_eq!(budget.provenance, output.len());
        let records: std::collections::BTreeMap<_, _> = output.iter().map(|p| (p.id, p)).collect();
        let mut pending = vec![roots[0]];
        let mut visited = BTreeSet::new();
        let mut recovered_sources = BTreeSet::new();
        let mut recovered_facts = BTreeSet::new();
        while let Some(id) = pending.pop() {
            assert!(visited.insert(id));
            let record = records[&id];
            assert_eq!(
                record.id,
                rootlight_ir::derive_provenance_record_id(record).unwrap()
            );
            assert!(record.input_sources.len() <= 2);
            assert!(record.evidence_sources.len() <= 2);
            assert!(record.derivation_parents.len() <= 2);
            recovered_sources.extend(record.input_sources.iter().cloned());
            for parent in &record.derivation_parents {
                if let FactRef::Fact(id) = parent {
                    pending.push(*id);
                } else {
                    recovered_facts.insert(*parent);
                }
            }
        }
        assert_eq!(visited.len(), output.len());
        assert_eq!(recovered_sources, sources.into_iter().collect());
        assert_eq!(recovered_facts, facts.into_iter().collect());
        let mut exhausted_limits = limits.clone();
        exhausted_limits.max_provenance_records = 0;
        let mut exhausted = OutputRecordBudget::from_document(&document, &exhausted_limits);
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cancelled_output = Vec::new();
        let error = super::append_shared_provenance(
            super::ProvenanceTree {
                sources: &[],
                facts: &[FactRef::File(FileId::from_bytes([1; 20]))],
                rule: "classic-project-membership-v1",
            },
            template.clone(),
            &limits,
            &mut budget,
            &mut BTreeSet::new(),
            &mut cancelled_output,
            &cancelled,
        )
        .unwrap_err();
        assert!(matches!(error, ResolutionError::Cancelled(_)));
        assert!(cancelled_output.is_empty());
        let error = super::append_shared_provenance(
            super::ProvenanceTree {
                sources: &[],
                facts: &[FactRef::File(FileId::from_bytes([1; 20]))],
                rule: "classic-project-membership-v1",
            },
            template,
            &limits,
            &mut exhausted,
            &mut BTreeSet::new(),
            &mut Vec::new(),
            &Cancellation::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ResolutionError::InvalidDocument(IrDocumentValidationError::CollectionLimit {
                collection: "provenance",
                observed: 1,
                limit: 0,
            })
        ));
    }

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

    #[test]
    fn relation_fanout_stops_at_collection_and_total_limits() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let spelling = content_hash(b"candidate");
        let occurrence = OccurrenceRecord {
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
            role: OccurrenceRole::CallSite,
            enclosing: None,
            target: OccurrenceTarget::Unresolved {
                text_hash: spelling,
            },
            syntactic_text_hash: spelling,
            syntax_kind: "call".to_owned(),
            provenance: FactId::from_bytes([5; 20]),
            confidence: Confidence::new(800).expect("confidence is valid"),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        };
        let decision = ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Candidates {
                symbols: vec![
                    SymbolId::from_bytes([6; 20]),
                    SymbolId::from_bytes([7; 20]),
                    SymbolId::from_bytes([8; 20]),
                ],
                total_count: 3,
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
        let mut relations = Vec::new();
        let mut relation_budget = OutputRecordBudget {
            relations: 0,
            provenance: 0,
            total_records: 0,
            max_relations: 2,
            max_provenance: 0,
            max_total_records: 10,
        };

        let cancellation = Cancellation::new();
        let relation_error = append_relation_specs(
            &occurrence,
            &decision,
            super::RelationSemantics::RoleOnly,
            &mut relations,
            &mut relation_budget,
            &cancellation,
        )
        .expect_err("third candidate exceeds the relation limit");
        assert_eq!(relations.len(), 2);
        assert!(matches!(
            relation_error,
            ResolutionError::InvalidDocument(IrDocumentValidationError::CollectionLimit {
                collection: "relations",
                observed: 3,
                limit: 2,
            })
        ));

        relations.clear();
        let mut total_budget = OutputRecordBudget {
            relations: 0,
            provenance: 0,
            total_records: 0,
            max_relations: 10,
            max_provenance: 0,
            max_total_records: 1,
        };
        let total_error = append_relation_specs(
            &occurrence,
            &decision,
            super::RelationSemantics::RoleOnly,
            &mut relations,
            &mut total_budget,
            &cancellation,
        )
        .expect_err("second candidate exceeds the total record limit");
        assert_eq!(relations.len(), 1);
        assert!(matches!(
            total_error,
            ResolutionError::InvalidDocument(IrDocumentValidationError::TotalRecordLimit {
                observed: 2,
                limit: 1,
            })
        ));

        let mut read = occurrence;
        read.role = OccurrenceRole::Reference;
        let mut getter = decision;
        getter.outcome = ResolutionOutcome::Resolved {
            symbol: SymbolId::from_bytes([6; 20]),
            confidence: read.confidence,
        };
        getter.explanation.rule = ResolutionRule::GroovyStaticImport;
        for (max_relations, max_total_records) in [(1, 10), (10, 1)] {
            relations.clear();
            let mut budget = OutputRecordBudget {
                relations: 0,
                provenance: 0,
                total_records: 0,
                max_relations,
                max_provenance: 0,
                max_total_records,
            };
            let error = append_relation_specs(
                &read,
                &getter,
                super::RelationSemantics::ImplicitAccessor,
                &mut relations,
                &mut budget,
                &cancellation,
            )
            .expect_err("implicit call must reserve its own output record");
            assert_eq!(relations.len(), 1);
            assert_eq!(
                relations[0].predicate,
                rootlight_ir::RelationPredicate::RefersTo
            );
            if max_relations == 1 {
                assert!(matches!(
                    error,
                    ResolutionError::InvalidDocument(IrDocumentValidationError::CollectionLimit {
                        collection: "relations",
                        observed: 2,
                        limit: 1,
                    })
                ));
            } else {
                assert!(matches!(
                    error,
                    ResolutionError::InvalidDocument(IrDocumentValidationError::TotalRecordLimit {
                        observed: 2,
                        limit: 1
                    })
                ));
            }
        }
    }
}
