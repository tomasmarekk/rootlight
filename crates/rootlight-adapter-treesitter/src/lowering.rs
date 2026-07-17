//! Parser-independent syntax-fact lowering into normalized IR.
//!
//! The analyzer injects a `ParseProvider` and never observes native Tree-sitter
//! types, so extraction can evolve independently from stable IR construction.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::Arc,
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisReport, AnalysisRequest, CoverageReport,
    DiagnosticCode, DomainCoverage, IrBatch, IrBatchSink, IrRecord, LanguageAnalyzer,
    MemoryAdmissionPolicy, MemoryEnforcement, ParseOutput, ParseProvider, ParseRequest,
    ProducerDescriptor, RequestError, ResourceKind, ResourceUsage, SinkError, StreamEnd,
    StreamUsage, SyntaxFact, SyntaxFactKind, execute_parse,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{
    ContentHash, FactId, SymbolId, SymbolIdentity, content_hash, derive_fact, derive_symbol,
};
use rootlight_ir::{
    AnalysisTier, Confidence, ContainerRef, CoverageRecord, CoverageScope, CoverageStatus,
    DiagnosticRecord, DiagnosticSeverity, EntityFlag, EntityKind, EntityRecord, EntityVisibility,
    EvidenceKind, ExtensionEnvelope, FactDomain, FactEvidence, FactRef, FileRecord, IrLimits,
    LEXICAL_EXTENSION_NAMESPACE, LEXICAL_EXTENSION_VERSION, LexicalEvidenceFormat,
    LexicalEvidenceKind, LexicalEvidenceV1, MAX_LEXICAL_SIGNATURE_BYTES, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SkippedRegion, SkippedRegionReason,
    SourceRef, SourceSpan, new_lexical_evidence_envelope,
};

const ANALYZER_TIER: AnalysisTier = AnalysisTier::TierD;
const SYNTAX_CONFIDENCE: u16 = 900;
const CONTAINMENT_CONFIDENCE: u16 = 1_000;
const CANCELLATION_CHECK_INTERVAL: usize = 64;
const MAX_FRONTEND_VERSION_BYTES: usize = 128;

const PROVENANCE_DOMAIN: &str = "rootlight.provenance/v1";
const OCCURRENCE_DOMAIN: &str = "rootlight.occurrence/v1";
const RELATION_DOMAIN: &str = "rootlight.relation/v1";
const COVERAGE_DOMAIN: &str = "rootlight.coverage/v1";
const SKIPPED_REGION_DOMAIN: &str = "rootlight.skipped-region/v1";
const DIAGNOSTIC_DOMAIN: &str = "rootlight.diagnostic/v1";
const SCOPE_IDENTITY_CONTEXT: &str = "rootlight/scope-container/v1";
const SCOPE_HEADER_CONTEXT: &str = "rootlight/treesitter-scope-header/v1";
const SCOPE_COLLISION_GUARD_CONTEXT: &str = "rootlight/treesitter-scope-collision-guard/v1";
const ENTITY_IDENTITY_GUARD_CONTEXT: &str = "rootlight/treesitter-entity-identity-guard/v1";

/// Tree-sitter syntax analyzer backed by an injected parser-independent provider.
#[derive(Clone)]
pub struct TreeSitterAnalyzer {
    parser: Arc<dyn ParseProvider>,
    descriptor: ProducerDescriptor,
    frontend_version: String,
    binary_digest: ContentHash,
}

/// Invalid immutable configuration for [`TreeSitterAnalyzer`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TreeSitterAnalyzerConfigError {
    /// The grammar/frontend version was empty, oversized, or unsafe.
    #[error(
        "Tree-sitter frontend version must be a safe 1..={MAX_FRONTEND_VERSION_BYTES}-byte label"
    )]
    InvalidFrontendVersion,
}

impl TreeSitterAnalyzer {
    /// Creates a Tier D analyzer over an injected parser provider.
    ///
    /// Lowering currently has bounded admission but no complete transient
    /// memory accounting, so the composite analyzer advertises unavailable
    /// memory enforcement even when its injected parser is fully accounted.
    ///
    /// # Errors
    ///
    /// Returns [`TreeSitterAnalyzerConfigError`] when `frontend_version` is not
    /// a bounded source-free grammar/frontend version label.
    pub fn new(
        parser: Arc<dyn ParseProvider>,
        producer: ProducerIdentity,
        language: rootlight_adapter_sdk::LanguageId,
        frontend_version: &str,
        binary_digest: ContentHash,
    ) -> Result<Self, TreeSitterAnalyzerConfigError> {
        if frontend_version.is_empty()
            || frontend_version.len() > MAX_FRONTEND_VERSION_BYTES
            || !frontend_version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+')
            })
        {
            return Err(TreeSitterAnalyzerConfigError::InvalidFrontendVersion);
        }
        let descriptor = ProducerDescriptor::new(
            producer,
            ProducerKind::Parser,
            language,
            ANALYZER_TIER,
            MemoryEnforcement::Unavailable,
            true,
        );
        Ok(Self {
            parser,
            descriptor,
            frontend_version: frontend_version.to_owned(),
            binary_digest,
        })
    }
}

impl fmt::Debug for TreeSitterAnalyzer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TreeSitterAnalyzer")
            .field("descriptor", &self.descriptor)
            .field("frontend_version", &self.frontend_version)
            .field("binary_digest", &self.binary_digest)
            .finish_non_exhaustive()
    }
}

impl LanguageAnalyzer for TreeSitterAnalyzer {
    fn descriptor(&self) -> &ProducerDescriptor {
        &self.descriptor
    }

    fn analyze(
        &self,
        request: &AnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<AnalysisReport, AdapterError> {
        cancellation.check()?;
        if request.encoding().as_str() != "utf-8" {
            return Err(RequestError::UnsupportedEncoding.into());
        }
        if request.generated_status().is_none() {
            return Err(RequestError::GeneratedStatusRequired.into());
        }
        let parse_request = ParseRequest::new(
            request.source().clone(),
            request.language().clone(),
            request.encoding().clone(),
            request.included_ranges().to_vec(),
            request.limits(),
        )?;
        let parser_memory_policy =
            memory_policy_for(self.parser.capabilities().memory_enforcement());
        let parse_output = execute_parse(
            self.parser.as_ref(),
            &parse_request,
            parser_memory_policy,
            cancellation,
        )?;
        let lowered = Lowering::new(self, request, &parse_output)?.lower(cancellation)?;
        emit_records(lowered.records, request, sink, cancellation)?;
        cancellation.check()?;

        let usage = sink.staged_usage();
        let parse_resources = parse_output.report().resources();
        let coverage = CoverageReport::new(
            ANALYZER_TIER,
            lowered.coverage_status,
            request.source().bytes().len(),
            parse_output.report().coverage().covered_source_bytes(),
            lowered.skipped_regions,
            lowered.domain_coverage,
        )
        .map_err(AdapterError::from)?;
        let resources = ResourceUsage::new(
            request.source().bytes().len(),
            usage.records(),
            parse_resources.syntax_nodes(),
            parse_resources.max_syntax_depth(),
            parse_resources.reported_memory_bytes(),
            usage,
        );
        rootlight_adapter_sdk::WorkReport::new(
            coverage,
            resources,
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}

fn memory_policy_for(enforcement: MemoryEnforcement) -> MemoryAdmissionPolicy {
    match enforcement {
        MemoryEnforcement::Unavailable => MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        MemoryEnforcement::HardProcess | MemoryEnforcement::AccountedInProcess => {
            MemoryAdmissionPolicy::RequireHardOrAccounted
        }
        _ => MemoryAdmissionPolicy::RequireHardOrAccounted,
    }
}

fn preflight_lowering_limits(
    request: &AnalysisRequest<'_>,
    parse_output: &ParseOutput,
    producer: &ProducerIdentity,
    frontend_version: &str,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let limits = request.limits().ir();
    require_resource_limit(ResourceKind::Records, 1, limits.max_files)?;
    require_resource_limit(ResourceKind::Records, 1, limits.max_provenance_records)?;
    require_resource_limit(ResourceKind::Records, 8, limits.max_coverage_records)?;
    require_resource_limit(
        ResourceKind::NestedItems,
        1,
        limits.max_nested_items_per_record,
    )?;

    let mut entity_candidates = 0_usize;
    let mut occurrence_candidates = 0_usize;
    let mut extension_candidates = 0_usize;
    let mut skipped_candidates = included_range_gap_count(
        request.source().source_ref().span(),
        request.included_ranges(),
    )?;
    let mut string_bytes = 0_usize;

    for value in [
        request.source().path().as_str(),
        request.language().as_str(),
        request.encoding().as_str(),
        producer.name(),
        producer.version(),
        request.language().as_str(),
        frontend_version,
    ] {
        account_string(&mut string_bytes, value.len(), limits)?;
    }
    for (index, fact) in parse_output.facts().iter().enumerate() {
        check_periodically(index, cancellation)?;
        if let Some(kind) = entity_kind(fact) {
            entity_candidates = checked_add(entity_candidates, 1)?;
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
            // Capture association happens after graph validation. Reserve one
            // gap per candidate so missing or ambiguous definition captures
            // cannot bypass skipped-region and string quotas.
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "declaration-name-unavailable".len(),
                limits,
            )?;
            if supports_signature(kind) {
                skipped_candidates = checked_add(skipped_candidates, 1)?;
                account_string(
                    &mut string_bytes,
                    "signature-capture-unavailable".len(),
                    limits,
                )?;
            }
        } else if matches!(
            fact.kind(),
            SyntaxFactKind::Module | SyntaxFactKind::Declaration
        ) {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "declaration-name-unavailable".len(),
                limits,
            )?;
        }
        if occurrence_role(fact).is_some() {
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
        }
        if is_definition_capture(fact) {
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
        }
        if fact.kind() == SyntaxFactKind::Comment {
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "lexical-evidence-unavailable".len(),
                limits,
            )?;
        }
        if is_signature_capture(fact) {
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "signature-capture-unavailable".len(),
                limits,
            )?;
        }
        if fact.kind() == SyntaxFactKind::Import {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(&mut string_bytes, "unresolved-import-target".len(), limits)?;
        }
    }

    let diagnostic_count = parse_output.diagnostics().len();
    let mut diagnostic_bytes = 0_usize;
    let mut diagnostic_derivations = 0_usize;
    for (index, diagnostic) in parse_output.diagnostics().iter().enumerate() {
        check_periodically(index, cancellation)?;
        let code_length = diagnostic.code().as_str().len();
        let message_length = "parser reported "
            .len()
            .checked_add(code_length)
            .ok_or(SinkError::AccountingOverflow)?;
        require_resource_limit(
            ResourceKind::DiagnosticBytes,
            message_length,
            limits.max_diagnostic_message_bytes,
        )?;
        account_string(&mut string_bytes, code_length, limits)?;
        account_string(&mut string_bytes, message_length, limits)?;
        diagnostic_bytes = checked_add(diagnostic_bytes, code_length)?;
        diagnostic_bytes = checked_add(diagnostic_bytes, message_length)?;
        if diagnostic.source().is_some() {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(&mut string_bytes, code_length, limits)?;
        } else {
            diagnostic_derivations = checked_add(diagnostic_derivations, 1)?;
        }
    }
    require_resource_limit(
        ResourceKind::DiagnosticBytes,
        diagnostic_bytes,
        limits.max_total_diagnostic_bytes,
    )?;

    if parse_output.report().coverage().skipped_regions() > 0 {
        skipped_candidates = checked_add(skipped_candidates, 1)?;
        account_string(
            &mut string_bytes,
            "parser-coverage-incomplete".len(),
            limits,
        )?;
    }
    if !request.included_ranges().is_empty() {
        for _ in 0..included_range_gap_count(
            request.source().source_ref().span(),
            request.included_ranges(),
        )? {
            account_string(&mut string_bytes, "outside-included-ranges".len(), limits)?;
        }
    }
    for _ in 0..extension_candidates {
        account_string(&mut string_bytes, LEXICAL_EXTENSION_NAMESPACE.len(), limits)?;
        account_string(&mut string_bytes, LEXICAL_EXTENSION_VERSION.len(), limits)?;
    }

    require_resource_limit(
        ResourceKind::Records,
        entity_candidates,
        limits.max_entities,
    )?;
    require_resource_limit(
        ResourceKind::Records,
        occurrence_candidates,
        limits.max_occurrences,
    )?;
    require_resource_limit(
        ResourceKind::Records,
        entity_candidates,
        limits.max_relations,
    )?;
    require_resource_limit(
        ResourceKind::Records,
        skipped_candidates,
        limits.max_skipped_regions,
    )?;
    require_resource_limit(
        ResourceKind::Diagnostics,
        diagnostic_count,
        limits.max_diagnostics,
    )?;
    require_resource_limit(
        ResourceKind::Records,
        extension_candidates,
        limits.max_extensions,
    )?;
    let total_records = [
        2,
        entity_candidates,
        occurrence_candidates,
        entity_candidates,
        8,
        skipped_candidates,
        diagnostic_count,
        extension_candidates,
    ]
    .into_iter()
    .try_fold(0_usize, checked_add)?;
    require_resource_limit(
        ResourceKind::Records,
        total_records,
        limits.max_total_records,
    )?;
    let nested_items = checked_add(2, diagnostic_derivations)?;
    require_resource_limit(
        ResourceKind::NestedItems,
        nested_items,
        limits.max_total_nested_items,
    )?;
    Ok(string_bytes)
}

struct Lowering<'context, 'source> {
    analyzer: &'context TreeSitterAnalyzer,
    request: &'context AnalysisRequest<'source>,
    parse_output: &'context ParseOutput,
    source_text: &'context str,
    full_source: &'context SourceRef,
}

impl<'context, 'source> Lowering<'context, 'source> {
    fn new(
        analyzer: &'context TreeSitterAnalyzer,
        request: &'context AnalysisRequest<'source>,
        parse_output: &'context ParseOutput,
    ) -> Result<Self, AdapterError> {
        let source_text = std::str::from_utf8(request.source().bytes())
            .map_err(|_| provider_failure("treesitter-lowering-invalid-utf8"))?;
        Ok(Self {
            analyzer,
            request,
            parse_output,
            source_text,
            full_source: request.source().source_ref(),
        })
    }

    fn lower(self, cancellation: &Cancellation) -> Result<LoweredOutput, AdapterError> {
        let mut total_string_bytes = preflight_lowering_limits(
            self.request,
            self.parse_output,
            self.analyzer.descriptor.identity(),
            &self.analyzer.frontend_version,
            cancellation,
        )?;
        validate_fact_graph(
            self.parse_output.facts(),
            self.request.included_ranges(),
            self.full_source,
            cancellation,
        )?;
        let provenance = self.provenance()?;
        let provenance_id = provenance.id;
        let file = self.file(provenance_id)?;

        let entity_plan = self.entity_drafts(cancellation, &mut total_string_bytes)?;
        let drafts = &entity_plan.drafts;
        let mut materialized = HashMap::new();
        for (index, draft) in drafts.iter().enumerate() {
            check_periodically(index, cancellation)?;
            let entity = materialize_entity(
                draft,
                self.full_source,
                self.request.build_context(),
                provenance_id,
                self.request.limits().ir().max_string_bytes,
                &materialized,
            )?;
            materialized.insert(draft.local_id, entity);
        }

        let mut entities = BTreeMap::<SymbolId, EntityRecord>::new();
        let mut identity_guards = BTreeMap::<SymbolId, [u8; 32]>::new();
        for entity in materialized.values() {
            ensure_symbol_identity_collision_free(
                &mut identity_guards,
                entity.record.id,
                entity.identity_guard,
            )?;
            match entities.get(&entity.record.id) {
                Some(existing) => {
                    if !equivalent_entity_projection(existing, &entity.record) {
                        return Err(provider_failure("treesitter-lowering-symbol-collision"));
                    }
                    if entity_source_span(existing) > entity_source_span(&entity.record) {
                        entities.insert(entity.record.id, entity.record.clone());
                    }
                }
                _ => {
                    entities.insert(entity.record.id, entity.record.clone());
                }
            }
        }

        let syntax_confidence = confidence(SYNTAX_CONFIDENCE)?;
        let containment_confidence = confidence(CONTAINMENT_CONFIDENCE)?;
        let mut occurrences = BTreeMap::<FactId, OccurrenceRecord>::new();
        let mut relations = BTreeMap::<FactId, RelationRecord>::new();
        let mut extensions = BTreeMap::<FactId, ExtensionEnvelope>::new();
        let mut extension_bytes = 0_usize;
        let mut skipped = BTreeMap::<FactId, SkippedRegion>::new();
        let mut diagnostics = BTreeMap::<FactId, DiagnosticRecord>::new();
        let facts_by_id: HashMap<_, _> = self
            .parse_output
            .facts()
            .iter()
            .map(|fact| (fact.local_id(), fact))
            .collect();

        for (index, fact) in self.parse_output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            let source = source_for_span(self.full_source, fact.span());
            if let Some(entity) = materialized.get(&fact.local_id()) {
                let entity_source = source_for_span(
                    self.full_source,
                    entity
                        .record
                        .evidence
                        .source
                        .as_ref()
                        .ok_or_else(|| provider_failure("treesitter-lowering-definition"))?
                        .span(),
                );
                if let Some(definition_local_id) = entity.definition_local_id {
                    let definition_fact = facts_by_id
                        .get(&definition_local_id)
                        .ok_or_else(|| provider_failure("treesitter-lowering-definition"))?;
                    let occurrence = declaration_occurrence(
                        definition_fact,
                        entity,
                        provenance_id,
                        syntax_confidence,
                        &entity_source,
                    )?;
                    occurrences.insert(occurrence.id, occurrence);
                }
                let relation = containment_relation(
                    entity,
                    provenance_id,
                    containment_confidence,
                    &entity_source,
                )?;
                relations.insert(relation.id, relation);
                if let Some(signature) = entity.signature_evidence.as_deref() {
                    let signature_span = entity
                        .signature_span
                        .ok_or_else(|| provider_failure("treesitter-lowering-signature"))?;
                    let signature_source = source_for_span(self.full_source, signature_span);
                    if let Some(envelope) = lexical_extension(
                        self.full_source,
                        provenance_id,
                        signature_source,
                        LexicalEvidenceKind::Signature,
                        FactRef::Entity(entity.record.id),
                        LexicalEvidenceFormat::SourceText,
                        signature,
                    ) {
                        if !extensions.contains_key(&envelope.id) {
                            ensure_extension_budget(
                                &envelope,
                                &mut extension_bytes,
                                self.request.limits().ir(),
                            )?;
                        }
                        extensions.insert(envelope.id, envelope);
                    } else {
                        let region = skipped_region(
                            self.full_source,
                            signature_span,
                            FactDomain::Extensions,
                            SkippedRegionReason::UnsupportedConstruct,
                            "lexical-evidence-unavailable",
                            provenance_id,
                        )?;
                        skipped.insert(region.id, region);
                    }
                } else if supports_signature(entity.record.kind) {
                    let region = skipped_region(
                        self.full_source,
                        entity
                            .record
                            .evidence
                            .source
                            .as_ref()
                            .ok_or_else(|| provider_failure("treesitter-lowering-definition"))?
                            .span(),
                        FactDomain::Extensions,
                        SkippedRegionReason::UnsupportedConstruct,
                        "signature-capture-unavailable",
                        provenance_id,
                    )?;
                    skipped.insert(region.id, region);
                }
                continue;
            }

            if let Some(role) = occurrence_role(fact) {
                let text = self.text_for_span(fact.span())?;
                let enclosing = entity_plan
                    .nearest_entity_ancestor
                    .get(&fact.local_id())
                    .copied()
                    .flatten()
                    .and_then(|local_id| materialized.get(&local_id))
                    .map(|entity| entity.record.id);
                let occurrence = unresolved_occurrence(
                    fact,
                    role,
                    enclosing,
                    provenance_id,
                    syntax_confidence,
                    source.clone(),
                    text,
                )?;
                let occurrence_id = occurrence.id;
                occurrences.insert(occurrence_id, occurrence);
                if fact.kind() == SyntaxFactKind::Comment
                    && let Some(comment) = comment_text(text)
                {
                    let kind = if fact.syntax_kind().as_str().contains("doc") {
                        LexicalEvidenceKind::DocumentationSummary
                    } else {
                        LexicalEvidenceKind::CommentSummary
                    };
                    if let Some(envelope) = lexical_extension(
                        self.full_source,
                        provenance_id,
                        source.clone(),
                        kind,
                        FactRef::Fact(occurrence_id),
                        LexicalEvidenceFormat::PlainText,
                        comment,
                    ) {
                        if !extensions.contains_key(&envelope.id) {
                            ensure_extension_budget(
                                &envelope,
                                &mut extension_bytes,
                                self.request.limits().ir(),
                            )?;
                        }
                        extensions.insert(envelope.id, envelope);
                    } else {
                        let region = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Extensions,
                            SkippedRegionReason::UnsupportedConstruct,
                            "lexical-evidence-unavailable",
                            provenance_id,
                        )?;
                        skipped.insert(region.id, region);
                    }
                }
            }

            if fact.kind() == SyntaxFactKind::Import {
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Relations,
                    SkippedRegionReason::UnsupportedConstruct,
                    "unresolved-import-target",
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            } else if matches!(
                fact.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Module
            ) {
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Entities,
                    SkippedRegionReason::UnsupportedConstruct,
                    "declaration-name-unavailable",
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            }
        }

        for range in included_range_gaps(self.full_source.span(), self.request.included_ranges()) {
            let region = skipped_region(
                self.full_source,
                range,
                FactDomain::Files,
                SkippedRegionReason::UnsupportedConstruct,
                "outside-included-ranges",
                provenance_id,
            )?;
            skipped.insert(region.id, region);
        }

        for diagnostic in self.parse_output.diagnostics() {
            let normalized = diagnostic_record(self.full_source, diagnostic, provenance_id)?;
            diagnostics.insert(normalized.id, normalized);
            if let Some(source) = diagnostic.source() {
                let reason = if diagnostic.code().as_str().contains("limit") {
                    SkippedRegionReason::ResourceLimit
                } else {
                    SkippedRegionReason::ParseError
                };
                let region = skipped_region(
                    self.full_source,
                    source.span(),
                    FactDomain::Diagnostics,
                    reason,
                    diagnostic.code().as_str(),
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            }
        }

        if self.parse_output.report().coverage().skipped_regions() > 0 && skipped.is_empty() {
            let reason =
                if self.parse_output.report().coverage().status() == CoverageStatus::Bounded {
                    SkippedRegionReason::ResourceLimit
                } else {
                    SkippedRegionReason::ParseError
                };
            let region = skipped_region(
                self.full_source,
                self.full_source.span(),
                FactDomain::Diagnostics,
                reason,
                "parser-coverage-incomplete",
                provenance_id,
            )?;
            skipped.insert(region.id, region);
        }

        cancellation.check()?;
        let stats = DomainStats::new(
            entities.len(),
            occurrences.len(),
            relations.len(),
            diagnostics.len(),
            &skipped,
            extensions.len(),
        )?;
        let parse_status = self.parse_output.report().coverage().status();
        let coverage_status = if skipped.is_empty() {
            parse_status
        } else if parse_status == CoverageStatus::Unknown {
            CoverageStatus::Unknown
        } else {
            CoverageStatus::Bounded
        };
        let domain_coverage = stats.domain_coverage(parse_status)?;
        let coverage_records = coverage_records(self.full_source, provenance_id, &domain_coverage)?;

        let mut records = Vec::new();
        records.push(IrRecord::File(file));
        records.push(IrRecord::Provenance(provenance));
        records.extend(entities.into_values().map(IrRecord::Entity));
        records.extend(occurrences.into_values().map(IrRecord::Occurrence));
        records.extend(relations.into_values().map(IrRecord::Relation));
        records.extend(coverage_records.into_iter().map(IrRecord::Coverage));
        records.extend(skipped.into_values().map(IrRecord::SkippedRegion));
        records.extend(diagnostics.into_values().map(IrRecord::Diagnostic));
        records.extend(extensions.into_values().map(IrRecord::Extension));

        Ok(LoweredOutput {
            records,
            coverage_status,
            skipped_regions: stats.skipped_regions,
            domain_coverage,
        })
    }

    fn provenance(&self) -> Result<ProvenanceRecord, AdapterError> {
        let producer = self.analyzer.descriptor.identity();
        let mut identity = Vec::new();
        push_source_identity(&mut identity, self.full_source)?;
        push_text(&mut identity, producer.name())?;
        push_text(&mut identity, producer.version())?;
        push_bytes(&mut identity, producer.configuration_hash().as_bytes())?;
        push_bytes(&mut identity, self.analyzer.binary_digest.as_bytes())?;
        push_text(&mut identity, &self.analyzer.frontend_version)?;
        push_text(&mut identity, self.request.language().as_str())?;
        push_bytes(
            &mut identity,
            self.request.build_context().digest().as_bytes(),
        )?;
        let id = derive_fact(PROVENANCE_DOMAIN, &identity).id();
        Ok(ProvenanceRecord {
            id,
            repository: self.full_source.repository(),
            generation: self.full_source.generation(),
            producer_kind: ProducerKind::Parser,
            producer: producer.clone(),
            binary_digest: self.analyzer.binary_digest,
            frontend_version: Some(self.analyzer.frontend_version.clone()),
            language: self.request.language().as_str().to_owned(),
            tier: ANALYZER_TIER,
            build_context: self.request.build_context(),
            input_sources: vec![self.full_source.clone()],
            evidence_sources: vec![self.full_source.clone()],
            derivation_parents: Vec::new(),
            rule: None,
        })
    }

    fn file(&self, provenance: FactId) -> Result<FileRecord, AdapterError> {
        let generated = self
            .request
            .generated_status()
            .ok_or(RequestError::GeneratedStatusRequired)?;
        Ok(FileRecord {
            id: self.full_source.span().file(),
            repository: self.full_source.repository(),
            generation: self.full_source.generation(),
            path: self.request.source().path().as_str().to_owned(),
            content_hash: self.full_source.content_hash(),
            byte_length: self.full_source.span().end_byte(),
            language: self.request.language().as_str().to_owned(),
            encoding: self.request.encoding().as_str().to_owned(),
            generated,
            provenance,
            evidence: direct_evidence(self.full_source.clone()),
        })
    }

    fn entity_drafts(
        &self,
        cancellation: &Cancellation,
        total_string_bytes: &mut usize,
    ) -> Result<EntityPlan, AdapterError> {
        let mut ordered_facts: Vec<_> = self.parse_output.facts().iter().collect();
        ordered_facts.sort_by(|left, right| {
            (
                left.depth(),
                left.span().start_byte(),
                left.span().end_byte(),
                syntax_fact_kind_tag(left.kind()),
                left.syntax_kind().as_str(),
            )
                .cmp(&(
                    right.depth(),
                    right.span().start_byte(),
                    right.span().end_byte(),
                    syntax_fact_kind_tag(right.kind()),
                    right.syntax_kind().as_str(),
                ))
        });
        let mut nearest_declaration = HashMap::new();
        let mut captures = HashMap::<u64, AssociatedCaptures>::new();
        for fact in &ordered_facts {
            let parent_declaration = fact
                .parent()
                .and_then(|parent| nearest_declaration.get(&parent).copied().flatten());
            if entity_kind(fact).is_some() {
                nearest_declaration.insert(fact.local_id(), Some(fact.local_id()));
                captures.entry(fact.local_id()).or_default();
            } else {
                nearest_declaration.insert(fact.local_id(), parent_declaration);
                if let Some(owner) = parent_declaration {
                    if is_definition_capture(fact) {
                        captures.entry(owner).or_default().definitions.push(*fact);
                    } else if is_signature_capture(fact) {
                        captures.entry(owner).or_default().signatures.push(*fact);
                    }
                }
            }
        }

        let mut first_entity_child_start = HashMap::<u64, u64>::new();
        for fact in &ordered_facts {
            if entity_kind(fact).is_some()
                && let Some(parent) = fact.parent()
            {
                first_entity_child_start
                    .entry(parent)
                    .and_modify(|start| *start = (*start).min(fact.span().start_byte()))
                    .or_insert(fact.span().start_byte());
            }
        }
        let mut drafts = HashMap::<u64, EntityDraft>::new();
        let mut nearest_entity_ancestor = HashMap::new();
        let mut nearest_scope_ancestor = HashMap::<u64, Option<ScopeContext>>::new();
        for (index, fact) in ordered_facts.into_iter().enumerate() {
            check_periodically(index, cancellation)?;
            let parent_entity = fact.parent().and_then(|parent| {
                drafts
                    .contains_key(&parent)
                    .then_some(parent)
                    .or_else(|| nearest_entity_ancestor.get(&parent).copied().flatten())
            });
            let parent_scope = fact
                .parent()
                .and_then(|parent| nearest_scope_ancestor.get(&parent).cloned().flatten());
            if fact.kind() == SyntaxFactKind::Scope {
                let stable_header = self.stable_scope_header(
                    fact,
                    first_entity_child_start.get(&fact.local_id()).copied(),
                )?;
                // Only reviewed semantic headers enter public symbol identity. Positional
                // ordinals would silently rebind unchanged symbols after sibling edits.
                let stable_identity = stable_header
                    .as_ref()
                    .map(|header| {
                        scope_identity(
                            parent_scope
                                .as_ref()
                                .and_then(|scope| scope.stable_identity),
                            fact.syntax_kind().as_str(),
                            header.digest,
                        )
                    })
                    .transpose()?
                    .or_else(|| {
                        parent_scope
                            .as_ref()
                            .and_then(|scope| scope.stable_identity)
                    });
                // Anonymous scopes intentionally inherit their nearest stable identity. The
                // span-local guard prevents distinct anonymous declarations from coalescing
                // silently without making source position part of the durable SymbolId.
                let collision_guard = scope_collision_guard(
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.collision_guard),
                    fact.syntax_kind().as_str(),
                    fact.span(),
                )?;
                let context = ScopeContext {
                    stable_identity,
                    collision_guard: Some(collision_guard),
                    qualified_prefix: stable_header
                        .as_ref()
                        .map(|header| Arc::<str>::from(header.qualified_prefix.as_str()))
                        .or_else(|| {
                            parent_scope
                                .as_ref()
                                .and_then(|scope| scope.qualified_prefix.clone())
                        }),
                    kind: stable_header
                        .as_ref()
                        .map(|header| header.kind)
                        .or_else(|| parent_scope.as_ref().and_then(|scope| scope.kind)),
                };
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), Some(context));
                continue;
            }
            let Some(mut kind) = entity_kind(fact) else {
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            };
            if parent_scope.as_ref().and_then(|scope| scope.kind) == Some(StableScopeKind::RustImpl)
                && fact.syntax_kind().as_str() == "rust.function.declaration"
            {
                kind = EntityKind::Method;
            }
            let capture = captures.get(&fact.local_id()).cloned().unwrap_or_default();
            let definition = select_unique_capture(&capture.definitions);
            let (name, definition_local_id, definition_span) = if let Some(definition) = definition
            {
                let text = self.text_for_span(definition.span())?;
                let Some(name) = captured_name(text, self.request.limits().ir().max_string_bytes)
                else {
                    nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                    nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                    continue;
                };
                (name, Some(definition.local_id()), definition.span())
            } else if is_explicit_file_module(fact, self.request.language().as_str()) {
                let name = self.request.source().path().as_str();
                (name, None, fact.span())
            } else {
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            };
            let signature_capture = select_unique_capture(&capture.signatures);
            let (signature, signature_evidence, signature_span) = if supports_signature(kind)
                && let Some(signature) = signature_capture
            {
                let text = self.text_for_span(signature.span())?;
                match canonical_signature(text, self.request.limits().ir().max_string_bytes)? {
                    Some(canonical) => (canonical, Some(text.to_owned()), Some(signature.span())),
                    None => (String::new(), None, None),
                }
            } else {
                (String::new(), None, None)
            };
            let language = language_for_fact(self.request, fact).as_str().to_owned();
            let qualified_prefix = parent_scope
                .as_ref()
                .and_then(|scope| scope.qualified_prefix.as_deref());
            let qualified_length = match parent_entity.and_then(|parent| drafts.get(&parent)) {
                Some(parent) => parent
                    .qualified_length
                    .checked_add(2)
                    .and_then(|length| length.checked_add(name.len()))
                    .ok_or(SinkError::AccountingOverflow)?,
                None => match qualified_prefix {
                    Some(prefix) => prefix
                        .len()
                        .checked_add(2)
                        .and_then(|length| length.checked_add(name.len()))
                        .ok_or(SinkError::AccountingOverflow)?,
                    None => name.len(),
                },
            };
            require_resource_limit(
                ResourceKind::StringBytes,
                qualified_length,
                self.request.limits().ir().max_string_bytes,
            )?;
            for length in [language.len(), name.len(), name.len(), qualified_length] {
                account_string(total_string_bytes, length, self.request.limits().ir())?;
            }
            drafts.insert(
                fact.local_id(),
                EntityDraft {
                    local_id: fact.local_id(),
                    parent_entity,
                    scope_identity: parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    scope_collision_guard: parent_scope
                        .as_ref()
                        .and_then(|scope| scope.collision_guard),
                    qualified_prefix: qualified_prefix.map(str::to_owned),
                    synthetic: definition_local_id.is_none(),
                    definition_local_id,
                    span: definition_span,
                    depth: fact.depth(),
                    kind,
                    name: name.to_owned(),
                    signature,
                    signature_evidence,
                    signature_span,
                    language,
                    qualified_length,
                },
            );
            nearest_entity_ancestor.insert(fact.local_id(), Some(fact.local_id()));
            nearest_scope_ancestor.insert(fact.local_id(), None);
        }
        let mut drafts: Vec<_> = drafts.into_values().collect();
        drafts.sort_by(|left, right| {
            (
                left.depth,
                left.span.start_byte(),
                left.span.end_byte(),
                entity_kind_label(left.kind),
                left.name.as_str(),
                left.signature.as_str(),
            )
                .cmp(&(
                    right.depth,
                    right.span.start_byte(),
                    right.span.end_byte(),
                    entity_kind_label(right.kind),
                    right.name.as_str(),
                    right.signature.as_str(),
                ))
        });
        Ok(EntityPlan {
            drafts,
            nearest_entity_ancestor,
        })
    }

    fn text_for_span(&self, span: SourceSpan) -> Result<&str, AdapterError> {
        let start = usize::try_from(span.start_byte())
            .map_err(|_| provider_failure("treesitter-lowering-span"))?;
        let end = usize::try_from(span.end_byte())
            .map_err(|_| provider_failure("treesitter-lowering-span"))?;
        self.source_text
            .get(start..end)
            .ok_or_else(|| provider_failure("treesitter-lowering-span"))
    }

    fn stable_scope_header(
        &self,
        scope: &SyntaxFact,
        first_entity_start: Option<u64>,
    ) -> Result<Option<StableScopeHeader>, AdapterError> {
        if scope.syntax_kind().as_str() != "rust.impl.scope" {
            return Ok(None);
        }
        let Some(end) = first_entity_start else {
            return Ok(None);
        };
        let start = usize::try_from(scope.span().start_byte())
            .map_err(|_| provider_failure("treesitter-lowering-scope"))?;
        let end =
            usize::try_from(end).map_err(|_| provider_failure("treesitter-lowering-scope"))?;
        let Some(prefix) = self.source_text.get(start..end) else {
            return Err(provider_failure("treesitter-lowering-scope"));
        };
        let Some(opening_brace) = prefix.rfind('{') else {
            return Ok(None);
        };
        let Some(canonical) = canonical_signature(
            &prefix[..opening_brace],
            self.request.limits().ir().max_string_bytes,
        )?
        else {
            return Ok(None);
        };
        let Some(qualified_prefix) = canonical.strip_prefix("impl") else {
            return Ok(None);
        };
        if qualified_prefix.is_empty() {
            return Ok(None);
        }
        let digest = blake3::derive_key(SCOPE_HEADER_CONTEXT, canonical.as_bytes());
        Ok(Some(StableScopeHeader {
            digest,
            qualified_prefix: qualified_prefix.to_owned(),
            kind: StableScopeKind::RustImpl,
        }))
    }
}

struct LoweredOutput {
    records: Vec<IrRecord>,
    coverage_status: CoverageStatus,
    skipped_regions: usize,
    domain_coverage: Vec<DomainCoverage>,
}

#[derive(Clone, Default)]
struct AssociatedCaptures<'a> {
    definitions: Vec<&'a SyntaxFact>,
    signatures: Vec<&'a SyntaxFact>,
}

struct EntityPlan {
    drafts: Vec<EntityDraft>,
    nearest_entity_ancestor: HashMap<u64, Option<u64>>,
}

#[derive(Clone)]
struct EntityDraft {
    local_id: u64,
    parent_entity: Option<u64>,
    scope_identity: Option<[u8; 32]>,
    scope_collision_guard: Option<[u8; 32]>,
    qualified_prefix: Option<String>,
    synthetic: bool,
    definition_local_id: Option<u64>,
    span: SourceSpan,
    depth: usize,
    kind: EntityKind,
    name: String,
    signature: String,
    signature_evidence: Option<String>,
    signature_span: Option<SourceSpan>,
    language: String,
    qualified_length: usize,
}

#[derive(Clone)]
struct MaterializedEntity {
    record: EntityRecord,
    direct_parent: ContainerRef,
    identity_guard: [u8; 32],
    definition_local_id: Option<u64>,
    signature_evidence: Option<String>,
    signature_span: Option<SourceSpan>,
}

#[derive(Clone, Default)]
struct ScopeContext {
    stable_identity: Option<[u8; 32]>,
    collision_guard: Option<[u8; 32]>,
    qualified_prefix: Option<Arc<str>>,
    kind: Option<StableScopeKind>,
}

struct StableScopeHeader {
    digest: [u8; 32],
    qualified_prefix: String,
    kind: StableScopeKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StableScopeKind {
    RustImpl,
}

fn materialize_entity(
    draft: &EntityDraft,
    full_source: &SourceRef,
    build_context: rootlight_ir::BuildContextIdentity,
    provenance: FactId,
    maximum_string_bytes: usize,
    materialized: &HashMap<u64, MaterializedEntity>,
) -> Result<MaterializedEntity, AdapterError> {
    let (container, mut container_identity, qualified_name) = match draft
        .parent_entity
        .and_then(|parent| materialized.get(&parent))
    {
        Some(parent) => {
            let mut identity = Vec::with_capacity(1 + parent.record.id.as_bytes().len());
            identity.push(2);
            identity.extend_from_slice(parent.record.id.as_bytes());
            let qualified_length = parent
                .record
                .qualified_name
                .len()
                .checked_add(2)
                .and_then(|length| length.checked_add(draft.name.len()))
                .ok_or(SinkError::AccountingOverflow)?;
            if qualified_length > maximum_string_bytes {
                return Err(stream_limit(
                    ResourceKind::StringBytes,
                    qualified_length,
                    maximum_string_bytes,
                ));
            }
            debug_assert_eq!(qualified_length, draft.qualified_length);
            let mut qualified = String::with_capacity(qualified_length);
            qualified.push_str(&parent.record.qualified_name);
            qualified.push_str("::");
            qualified.push_str(&draft.name);
            (ContainerRef::Entity(parent.record.id), identity, qualified)
        }
        None => {
            let file = full_source.span().file();
            let mut identity = Vec::with_capacity(1 + file.as_bytes().len());
            identity.push(1);
            identity.extend_from_slice(file.as_bytes());
            let qualified_name = match draft.qualified_prefix.as_deref() {
                Some(prefix) => {
                    let mut qualified = String::with_capacity(draft.qualified_length);
                    qualified.push_str(prefix);
                    qualified.push_str("::");
                    qualified.push_str(&draft.name);
                    qualified
                }
                None => draft.name.clone(),
            };
            (ContainerRef::File(file), identity, qualified_name)
        }
    };
    if let Some(scope_identity) = draft.scope_identity {
        container_identity.push(3);
        container_identity.extend_from_slice(&scope_identity);
    }
    debug_assert_eq!(qualified_name.len(), draft.qualified_length);
    let semantic_kind = entity_kind_label(draft.kind);
    let identity_guard = entity_identity_guard(
        full_source.repository(),
        &draft.language,
        semantic_kind,
        &container_identity,
        &draft.name,
        draft.signature.as_bytes(),
        build_context.digest().as_bytes(),
        draft.scope_collision_guard.as_ref(),
    )?;
    let id = derive_symbol(SymbolIdentity {
        repository: full_source.repository(),
        language: &draft.language,
        semantic_kind,
        container_identity: &container_identity,
        declared_identity: &draft.name,
        signature_discriminator: draft.signature.as_bytes(),
        build_context_discriminator: build_context.digest().as_bytes(),
    })
    .id();
    let source = source_for_span(full_source, draft.span);
    let record = EntityRecord {
        id,
        repository: full_source.repository(),
        generation: full_source.generation(),
        kind: draft.kind,
        language: draft.language.clone(),
        tier: ANALYZER_TIER,
        canonical_name: draft.name.clone(),
        display_name: draft.name.clone(),
        qualified_name,
        container: Some(container),
        visibility: EntityVisibility::Unknown,
        flags: if draft.synthetic {
            vec![EntityFlag::Synthetic]
        } else {
            Vec::new()
        },
        provenance,
        evidence: direct_evidence(source),
    };
    let entity = MaterializedEntity {
        direct_parent: container,
        identity_guard,
        definition_local_id: draft.definition_local_id,
        signature_evidence: draft.signature_evidence.clone(),
        signature_span: draft.signature_span,
        record,
    };
    Ok(entity)
}

#[allow(clippy::too_many_arguments)]
fn entity_identity_guard(
    repository: rootlight_ids::RepositoryId,
    language: &str,
    semantic_kind: &str,
    container_identity: &[u8],
    declared_identity: &str,
    signature_discriminator: &[u8],
    build_context_discriminator: &[u8],
    scope_collision_guard: Option<&[u8; 32]>,
) -> Result<[u8; 32], AdapterError> {
    let mut hasher = blake3::Hasher::new_derive_key(ENTITY_IDENTITY_GUARD_CONTEXT);
    for field in [
        repository.as_bytes().as_slice(),
        language.as_bytes(),
        semantic_kind.as_bytes(),
        container_identity,
        declared_identity.as_bytes(),
        signature_discriminator,
        build_context_discriminator,
    ] {
        let length = u64::try_from(field.len())
            .map_err(|_| provider_failure("treesitter-lowering-accounting"))?;
        hasher.update(&length.to_be_bytes());
        hasher.update(field);
    }
    match scope_collision_guard {
        Some(guard) => {
            hasher.update(&[1]);
            hasher.update(guard);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn ensure_symbol_identity_collision_free(
    guards: &mut BTreeMap<SymbolId, [u8; 32]>,
    symbol: SymbolId,
    guard: [u8; 32],
) -> Result<(), AdapterError> {
    match guards.get(&symbol) {
        Some(existing) if existing != &guard => {
            Err(provider_failure("treesitter-lowering-symbol-collision"))
        }
        Some(_) => Ok(()),
        None => {
            guards.insert(symbol, guard);
            Ok(())
        }
    }
}

fn equivalent_entity_projection(left: &EntityRecord, right: &EntityRecord) -> bool {
    left.id == right.id
        && left.repository == right.repository
        && left.generation == right.generation
        && left.kind == right.kind
        && left.language == right.language
        && left.tier == right.tier
        && left.canonical_name == right.canonical_name
        && left.display_name == right.display_name
        && left.qualified_name == right.qualified_name
        && left.container == right.container
        && left.visibility == right.visibility
        && left.flags == right.flags
        && left.provenance == right.provenance
}

fn scope_identity(
    parent: Option<[u8; 32]>,
    syntax_kind: &str,
    header: [u8; 32],
) -> Result<[u8; 32], AdapterError> {
    let mut hasher = blake3::Hasher::new_derive_key(SCOPE_IDENTITY_CONTEXT);
    match parent {
        Some(parent) => {
            hasher.update(&[1]);
            hasher.update(&parent);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    let syntax_length = u64::try_from(syntax_kind.len())
        .map_err(|_| provider_failure("treesitter-lowering-scope"))?;
    hasher.update(&syntax_length.to_be_bytes());
    hasher.update(syntax_kind.as_bytes());
    hasher.update(&header);
    Ok(*hasher.finalize().as_bytes())
}

fn scope_collision_guard(
    parent: Option<[u8; 32]>,
    syntax_kind: &str,
    span: SourceSpan,
) -> Result<[u8; 32], AdapterError> {
    let mut hasher = blake3::Hasher::new_derive_key(SCOPE_COLLISION_GUARD_CONTEXT);
    match parent {
        Some(parent) => {
            hasher.update(&[1]);
            hasher.update(&parent);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    let syntax_length = u64::try_from(syntax_kind.len())
        .map_err(|_| provider_failure("treesitter-lowering-scope"))?;
    hasher.update(&syntax_length.to_be_bytes());
    hasher.update(syntax_kind.as_bytes());
    hasher.update(&span.start_byte().to_be_bytes());
    hasher.update(&span.end_byte().to_be_bytes());
    Ok(*hasher.finalize().as_bytes())
}

fn entity_source_span(entity: &EntityRecord) -> Option<SourceSpan> {
    entity.evidence.source.as_ref().map(SourceRef::span)
}

fn declaration_occurrence(
    fact: &SyntaxFact,
    entity: &MaterializedEntity,
    provenance: FactId,
    confidence: Confidence,
    source: &SourceRef,
) -> Result<OccurrenceRecord, AdapterError> {
    let mut identity = Vec::new();
    push_source_identity(&mut identity, source)?;
    push_bytes(&mut identity, entity.record.id.as_bytes())?;
    push_text(&mut identity, fact.syntax_kind().as_str())?;
    let id = derive_fact(OCCURRENCE_DOMAIN, &identity).id();
    Ok(OccurrenceRecord {
        id,
        repository: source.repository(),
        generation: source.generation(),
        file: source.span().file(),
        source: source.clone(),
        role: OccurrenceRole::Definition,
        enclosing: match entity.direct_parent {
            ContainerRef::Entity(parent) => Some(parent),
            ContainerRef::Repository(_) | ContainerRef::File(_) => None,
        },
        target: OccurrenceTarget::Resolved {
            symbol: entity.record.id,
        },
        syntactic_text_hash: content_hash(entity.record.display_name.as_bytes()),
        syntax_kind: fact.syntax_kind().as_str().to_owned(),
        provenance,
        confidence,
        evidence: direct_evidence(source.clone()),
    })
}

fn unresolved_occurrence(
    fact: &SyntaxFact,
    role: OccurrenceRole,
    enclosing: Option<SymbolId>,
    provenance: FactId,
    confidence: Confidence,
    source: SourceRef,
    text: &str,
) -> Result<OccurrenceRecord, AdapterError> {
    let text_hash = content_hash(text.as_bytes());
    let mut identity = Vec::new();
    push_source_identity(&mut identity, &source)?;
    identity.push(occurrence_role_tag(role));
    push_bytes(&mut identity, text_hash.as_bytes())?;
    push_text(&mut identity, fact.syntax_kind().as_str())?;
    if let Some(enclosing) = enclosing {
        identity.push(1);
        push_bytes(&mut identity, enclosing.as_bytes())?;
    } else {
        identity.push(0);
    }
    let id = derive_fact(OCCURRENCE_DOMAIN, &identity).id();
    Ok(OccurrenceRecord {
        id,
        repository: source.repository(),
        generation: source.generation(),
        file: source.span().file(),
        source: source.clone(),
        role,
        enclosing,
        target: OccurrenceTarget::Unresolved { text_hash },
        syntactic_text_hash: text_hash,
        syntax_kind: fact.syntax_kind().as_str().to_owned(),
        provenance,
        confidence,
        evidence: direct_evidence(source),
    })
}

fn containment_relation(
    entity: &MaterializedEntity,
    provenance: FactId,
    confidence: Confidence,
    source: &SourceRef,
) -> Result<RelationRecord, AdapterError> {
    let subject = match entity.direct_parent {
        ContainerRef::Repository(repository) => RelationEndpoint::Repository(repository),
        ContainerRef::File(file) => RelationEndpoint::File(file),
        ContainerRef::Entity(parent) => RelationEndpoint::Entity(parent),
    };
    let object = RelationEndpoint::Entity(entity.record.id);
    let mut identity = Vec::new();
    push_source_identity(&mut identity, source)?;
    push_endpoint(&mut identity, subject)?;
    identity.push(relation_predicate_tag(RelationPredicate::Contains));
    push_endpoint(&mut identity, object)?;
    let id = derive_fact(RELATION_DOMAIN, &identity).id();
    Ok(RelationRecord {
        id,
        repository: source.repository(),
        generation: source.generation(),
        subject,
        predicate: RelationPredicate::Contains,
        object,
        confidence,
        evidence_kind: EvidenceKind::Syntax,
        provenance,
        evidence: direct_evidence(source.clone()),
    })
}

fn diagnostic_record(
    full_source: &SourceRef,
    diagnostic: &AdapterDiagnostic,
    provenance: FactId,
) -> Result<DiagnosticRecord, AdapterError> {
    let source = diagnostic.source().cloned();
    let mut identity = Vec::new();
    push_source_identity(&mut identity, source.as_ref().unwrap_or(full_source))?;
    push_text(&mut identity, diagnostic.code().as_str())?;
    identity.push(diagnostic_severity_tag(diagnostic.severity()));
    identity.push(coverage_status_tag(diagnostic.coverage_effect()));
    let id = derive_fact(DIAGNOSTIC_DOMAIN, &identity).id();
    Ok(DiagnosticRecord {
        id,
        repository: full_source.repository(),
        generation: full_source.generation(),
        code: diagnostic.code().as_str().to_owned(),
        message: format!("parser reported {}", diagnostic.code().as_str()),
        severity: diagnostic.severity(),
        source: source.clone(),
        coverage_effect: diagnostic.coverage_effect(),
        provenance,
        evidence: source.map_or_else(
            || FactEvidence {
                source: None,
                derivation: vec![FactRef::File(full_source.span().file())],
            },
            direct_evidence,
        ),
    })
}

fn skipped_region(
    full_source: &SourceRef,
    span: SourceSpan,
    domain: FactDomain,
    reason: SkippedRegionReason,
    detail: &str,
    provenance: FactId,
) -> Result<SkippedRegion, AdapterError> {
    let source = source_for_span(full_source, span);
    let mut identity = Vec::new();
    push_source_identity(&mut identity, &source)?;
    identity.push(fact_domain_tag(domain));
    identity.push(skipped_reason_tag(reason));
    push_text(&mut identity, detail)?;
    let id = derive_fact(SKIPPED_REGION_DOMAIN, &identity).id();
    Ok(SkippedRegion {
        id,
        repository: full_source.repository(),
        generation: full_source.generation(),
        source: source.clone(),
        domain,
        reason,
        detail: detail.to_owned(),
        provenance,
        evidence: direct_evidence(source),
    })
}

fn coverage_records(
    source: &SourceRef,
    provenance: FactId,
    coverage: &[DomainCoverage],
) -> Result<Vec<CoverageRecord>, AdapterError> {
    coverage
        .iter()
        .map(|domain| {
            let discovered = u64::try_from(domain.discovered())
                .map_err(|_| provider_failure("treesitter-lowering-accounting"))?;
            let indexed = u64::try_from(domain.indexed())
                .map_err(|_| provider_failure("treesitter-lowering-accounting"))?;
            let skipped = u64::try_from(domain.skipped())
                .map_err(|_| provider_failure("treesitter-lowering-accounting"))?;
            let mut identity = Vec::new();
            push_source_identity(&mut identity, source)?;
            identity.push(fact_domain_tag(domain.domain()));
            identity.push(coverage_status_tag(domain.status()));
            identity.extend_from_slice(&discovered.to_be_bytes());
            identity.extend_from_slice(&indexed.to_be_bytes());
            identity.extend_from_slice(&skipped.to_be_bytes());
            let id = derive_fact(COVERAGE_DOMAIN, &identity).id();
            Ok(CoverageRecord {
                id,
                repository: source.repository(),
                generation: source.generation(),
                scope: CoverageScope::File(source.span().file()),
                domain: domain.domain(),
                tier: ANALYZER_TIER,
                status: domain.status(),
                discovered,
                indexed,
                skipped,
                provenance,
                evidence: direct_evidence(source.clone()),
            })
        })
        .collect()
}

struct DomainStats {
    entities_indexed: usize,
    occurrences_indexed: usize,
    relations_indexed: usize,
    diagnostics: usize,
    skipped_regions: usize,
    skipped_by_domain: BTreeMap<FactDomain, usize>,
    extensions: usize,
}

impl DomainStats {
    #[allow(clippy::too_many_arguments)]
    fn new(
        entities_indexed: usize,
        occurrences_indexed: usize,
        relations_indexed: usize,
        diagnostics: usize,
        skipped_regions: &BTreeMap<FactId, SkippedRegion>,
        extensions: usize,
    ) -> Result<Self, AdapterError> {
        let mut skipped_by_domain = BTreeMap::new();
        for region in skipped_regions.values() {
            let count = skipped_by_domain
                .get(&region.domain)
                .copied()
                .unwrap_or(0_usize)
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            skipped_by_domain.insert(region.domain, count);
        }
        Ok(Self {
            entities_indexed,
            occurrences_indexed,
            relations_indexed,
            diagnostics,
            skipped_regions: skipped_regions.len(),
            skipped_by_domain,
            extensions,
        })
    }

    fn domain_coverage(
        &self,
        parse_status: CoverageStatus,
    ) -> Result<Vec<DomainCoverage>, AdapterError> {
        let file_skipped = self.skipped(FactDomain::Files);
        let provenance_skipped = self.skipped(FactDomain::Provenance);
        let source_mapping_skipped = self.skipped(FactDomain::SourceMappings);
        let diagnostics_skipped = self.skipped(FactDomain::Diagnostics);
        let extension_skipped = self.skipped(FactDomain::Extensions);
        let files_discovered = 1_usize
            .checked_add(file_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let provenance_discovered = 1_usize
            .checked_add(provenance_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let source_mappings_discovered = source_mapping_skipped;
        let entities_skipped = self.skipped(FactDomain::Entities);
        let occurrences_skipped = self.skipped(FactDomain::Occurrences);
        let relations_skipped = self.skipped(FactDomain::Relations);
        let entities_discovered = self
            .entities_indexed
            .checked_add(entities_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let occurrences_discovered = self
            .occurrences_indexed
            .checked_add(occurrences_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let relations_discovered = self
            .relations_indexed
            .checked_add(relations_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let diagnostics_discovered = self
            .diagnostics
            .checked_add(diagnostics_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let extensions_discovered = self
            .extensions
            .checked_add(extension_skipped)
            .ok_or(SinkError::AccountingOverflow)?;
        let domains = [
            (FactDomain::Files, files_discovered, 1, file_skipped),
            (
                FactDomain::Entities,
                entities_discovered,
                self.entities_indexed,
                entities_skipped,
            ),
            (
                FactDomain::Occurrences,
                occurrences_discovered,
                self.occurrences_indexed,
                occurrences_skipped,
            ),
            (
                FactDomain::Relations,
                relations_discovered,
                self.relations_indexed,
                relations_skipped,
            ),
            (
                FactDomain::Provenance,
                provenance_discovered,
                1,
                provenance_skipped,
            ),
            (
                FactDomain::SourceMappings,
                source_mappings_discovered,
                0,
                source_mapping_skipped,
            ),
            (
                FactDomain::Diagnostics,
                diagnostics_discovered,
                self.diagnostics,
                diagnostics_skipped,
            ),
            (
                FactDomain::Extensions,
                extensions_discovered,
                self.extensions,
                extension_skipped,
            ),
        ];
        domains
            .into_iter()
            .map(|(domain, discovered, indexed, skipped)| {
                let status = match (parse_status, skipped) {
                    (CoverageStatus::Unknown, _) => CoverageStatus::Unknown,
                    (status, 0) => status,
                    (_, _) => CoverageStatus::Bounded,
                };
                DomainCoverage::new(domain, status, discovered, indexed, skipped)
                    .map_err(AdapterError::from)
            })
            .collect()
    }

    fn skipped(&self, domain: FactDomain) -> usize {
        self.skipped_by_domain
            .get(&domain)
            .copied()
            .unwrap_or_default()
    }
}

fn validate_fact_graph(
    facts: &[SyntaxFact],
    included_ranges: &[rootlight_adapter_sdk::IncludedRange],
    source: &SourceRef,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let facts_by_id: HashMap<_, _> = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
    if facts_by_id.len() != facts.len() {
        return Err(provider_failure("treesitter-lowering-duplicate-local-id"));
    }
    for (index, fact) in facts.iter().enumerate() {
        check_periodically(index, cancellation)?;
        if fact.span().file() != source.span().file()
            || fact.span().start_byte() < source.span().start_byte()
            || fact.span().end_byte() > source.span().end_byte()
        {
            return Err(provider_failure("treesitter-lowering-span"));
        }
        if fact.kind() != SyntaxFactKind::Root
            && !included_ranges.is_empty()
            && containing_range(included_ranges, fact.span()).is_none()
        {
            return Err(provider_failure("treesitter-lowering-fact-outside-range"));
        }
        if let Some(parent_id) = fact.parent() {
            let parent = facts_by_id
                .get(&parent_id)
                .ok_or_else(|| provider_failure("treesitter-lowering-parent"))?;
            if parent.depth() >= fact.depth()
                || parent.span().start_byte() > fact.span().start_byte()
                || parent.span().end_byte() < fact.span().end_byte()
            {
                return Err(provider_failure("treesitter-lowering-parent"));
            }
        }
    }
    Ok(())
}

fn language_for_fact<'a>(
    request: &'a AnalysisRequest<'_>,
    fact: &SyntaxFact,
) -> &'a rootlight_adapter_sdk::LanguageId {
    containing_range(request.included_ranges(), fact.span()).map_or(
        request.language(),
        rootlight_adapter_sdk::IncludedRange::language,
    )
}

fn containing_range(
    ranges: &[rootlight_adapter_sdk::IncludedRange],
    span: SourceSpan,
) -> Option<&rootlight_adapter_sdk::IncludedRange> {
    let insertion = ranges.partition_point(|range| range.span().start_byte() <= span.start_byte());
    let candidate = ranges.get(insertion.checked_sub(1)?)?;
    (span.end_byte() <= candidate.span().end_byte()).then_some(candidate)
}

fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    let label = fact.syntax_kind().as_str();
    match fact.kind() {
        SyntaxFactKind::Module => Some(EntityKind::Module),
        SyntaxFactKind::Declaration if label == "java.annotation.declaration" => {
            Some(EntityKind::Interface)
        }
        SyntaxFactKind::Declaration if label == "java.annotation_element.declaration" => {
            Some(EntityKind::Method)
        }
        SyntaxFactKind::Declaration if label.contains("constructor") => {
            Some(EntityKind::Constructor)
        }
        SyntaxFactKind::Declaration if label.contains("record") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("method") => Some(EntityKind::Method),
        SyntaxFactKind::Declaration if label.contains("function") => Some(EntityKind::Function),
        SyntaxFactKind::Declaration if label.contains("class") => Some(EntityKind::Class),
        SyntaxFactKind::Declaration if label.contains("struct") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("enum") => Some(EntityKind::Enum),
        SyntaxFactKind::Declaration if label.contains("trait") => Some(EntityKind::Trait),
        SyntaxFactKind::Declaration if label.contains("interface") => Some(EntityKind::Interface),
        SyntaxFactKind::Declaration
            if label.contains("type_alias")
                || label.contains("type_item")
                || label.contains("type.declaration") =>
        {
            Some(EntityKind::TypeAlias)
        }
        SyntaxFactKind::Declaration
            if label.contains("constant")
                || label.contains("const_item")
                || label.contains("const.declaration") =>
        {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if label.contains("static_item") || label.contains("static.declaration") =>
        {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label.contains("field") => Some(EntityKind::Field),
        SyntaxFactKind::Declaration if label.contains("parameter") => Some(EntityKind::Parameter),
        SyntaxFactKind::Declaration if label.contains("variable") => Some(EntityKind::Variable),
        _ => None,
    }
}

fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let candidate = text.trim();
    (!candidate.is_empty()
        && candidate.len() <= maximum_bytes
        && candidate.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '/' | '\\' | '(' | ')' | '{' | '}' | '[' | ']')
        }))
    .then_some(candidate)
}

fn is_explicit_file_module(fact: &SyntaxFact, language: &str) -> bool {
    fact.kind() == SyntaxFactKind::Module
        && matches!(
            fact.syntax_kind().as_str(),
            "python.file.module" | "javascript.file.module"
        )
        && matches!(language, "python" | "javascript" | "typescript")
}

fn is_definition_capture(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str().ends_with(".definition")
}

fn is_signature_capture(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str().ends_with(".signature")
}

fn select_unique_capture<'a>(captures: &[&'a SyntaxFact]) -> Option<&'a SyntaxFact> {
    let selected = captures.first().copied()?;
    captures
        .iter()
        .copied()
        .all(|candidate| {
            candidate.span() == selected.span()
                && candidate.syntax_kind().as_str() == selected.syntax_kind().as_str()
        })
        .then_some(selected)
}

const fn supports_signature(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Function
            | EntityKind::Method
            | EntityKind::Constructor
            | EntityKind::Class
            | EntityKind::Struct
            | EntityKind::Enum
            | EntityKind::Trait
            | EntityKind::Interface
    )
}

fn canonical_signature(
    text: &str,
    maximum_string_bytes: usize,
) -> Result<Option<String>, AdapterError> {
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > MAX_LEXICAL_SIGNATURE_BYTES || text.len() > maximum_string_bytes {
        return Ok(None);
    }
    let mut canonical = String::with_capacity(text.len());
    canonical.extend(text.chars().filter(|character| !character.is_whitespace()));
    if canonical.is_empty() {
        return Ok(None);
    }
    Ok(Some(canonical))
}

fn comment_text(text: &str) -> Option<&str> {
    let text = text
        .trim()
        .trim_start_matches("/*")
        .trim_end_matches("*/")
        .trim_start_matches("///")
        .trim_start_matches("//!")
        .trim_start_matches("//")
        .trim_start_matches('#')
        .trim();
    (!text.is_empty()).then_some(text)
}

fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match fact.kind() {
        SyntaxFactKind::Import => Some(OccurrenceRole::ImportUse),
        SyntaxFactKind::Occurrence if is_definition_capture(fact) => None,
        SyntaxFactKind::Occurrence if fact.syntax_kind().as_str().contains("call") => {
            Some(OccurrenceRole::CallSite)
        }
        SyntaxFactKind::Occurrence if fact.syntax_kind().as_str().ends_with(".reference") => {
            Some(OccurrenceRole::Reference)
        }
        SyntaxFactKind::Occurrence if fact.syntax_kind().as_str().contains("type") => {
            Some(OccurrenceRole::TypeUse)
        }
        SyntaxFactKind::Occurrence => Some(OccurrenceRole::Reference),
        SyntaxFactKind::Comment => Some(OccurrenceRole::Documentation),
        SyntaxFactKind::StringLiteral => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}

fn included_range_gaps(
    full: SourceSpan,
    ranges: &[rootlight_adapter_sdk::IncludedRange],
) -> Vec<SourceSpan> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    let mut cursor = full.start_byte();
    for range in ranges {
        let span = range.span();
        if cursor < span.start_byte()
            && let Ok(gap) = SourceSpan::new(full.file(), cursor, span.start_byte())
        {
            gaps.push(gap);
        }
        cursor = span.end_byte();
    }
    if cursor < full.end_byte()
        && let Ok(gap) = SourceSpan::new(full.file(), cursor, full.end_byte())
    {
        gaps.push(gap);
    }
    gaps
}

fn included_range_gap_count(
    full: SourceSpan,
    ranges: &[rootlight_adapter_sdk::IncludedRange],
) -> Result<usize, AdapterError> {
    if ranges.is_empty() {
        return Ok(0);
    }
    let mut count = 0_usize;
    let mut cursor = full.start_byte();
    for range in ranges {
        if cursor < range.span().start_byte() {
            count = checked_add(count, 1)?;
        }
        cursor = range.span().end_byte();
    }
    if cursor < full.end_byte() {
        count = checked_add(count, 1)?;
    }
    Ok(count)
}

fn account_string(total: &mut usize, length: usize, limits: &IrLimits) -> Result<(), AdapterError> {
    require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
    *total = checked_add(*total, length)?;
    require_resource_limit(
        ResourceKind::StringBytes,
        *total,
        limits.max_total_string_bytes,
    )
}

fn ensure_extension_budget(
    envelope: &ExtensionEnvelope,
    total: &mut usize,
    limits: &IrLimits,
) -> Result<(), AdapterError> {
    require_resource_limit(
        ResourceKind::ExtensionBytes,
        envelope.payload.len(),
        limits.max_extension_payload_bytes,
    )?;
    *total = checked_add(*total, envelope.payload.len())?;
    require_resource_limit(
        ResourceKind::ExtensionBytes,
        *total,
        limits.max_total_extension_bytes,
    )
}

fn lexical_extension(
    full_source: &SourceRef,
    provenance: FactId,
    source: SourceRef,
    kind: LexicalEvidenceKind,
    subject: FactRef,
    format: LexicalEvidenceFormat,
    text: &str,
) -> Option<ExtensionEnvelope> {
    let evidence = LexicalEvidenceV1::from_complete_text(kind, subject, format, text).ok()?;
    new_lexical_evidence_envelope(
        full_source.repository(),
        full_source.generation(),
        provenance,
        source,
        &evidence,
    )
    .ok()
}

fn checked_add(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_add(right)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn require_resource_limit(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<(), AdapterError> {
    if observed > limit {
        Err(stream_limit(resource, observed, limit))
    } else {
        Ok(())
    }
}

const fn stream_limit(resource: ResourceKind, observed: usize, limit: usize) -> AdapterError {
    AdapterError::Sink(SinkError::StreamLimit {
        resource,
        observed,
        limit,
    })
}

fn source_for_span(full: &SourceRef, span: SourceSpan) -> SourceRef {
    SourceRef::new(
        full.repository(),
        full.generation(),
        span,
        full.content_hash(),
        None,
    )
}

fn direct_evidence(source: SourceRef) -> FactEvidence {
    FactEvidence {
        source: Some(source),
        derivation: Vec::new(),
    }
}

fn confidence(value: u16) -> Result<Confidence, AdapterError> {
    Confidence::new(value).map_err(|_| provider_failure("treesitter-lowering-confidence"))
}

fn check_periodically(index: usize, cancellation: &Cancellation) -> Result<(), AdapterError> {
    if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
        cancellation.check()?;
    }
    Ok(())
}

fn emit_records(
    records: Vec<IrRecord>,
    request: &AnalysisRequest<'_>,
    sink: &mut dyn IrBatchSink,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut batch = Vec::new();
    let mut usage = empty_batch_usage();
    for (index, record) in records.into_iter().enumerate() {
        check_periodically(index, cancellation)?;
        let item_usage = IrBatch::new(sink.next_sequence(), vec![record.clone()])
            .usage(request.limits().ir())?;
        let candidate = combine_batch_usage(usage, item_usage)?;
        if !batch.is_empty() && !usage_fits(candidate, sink.remaining_budget()) {
            cancellation.check()?;
            sink.push(IrBatch::new(
                sink.next_sequence(),
                std::mem::take(&mut batch),
            ))?;
            usage = empty_batch_usage();
        }
        usage = combine_batch_usage(usage, item_usage)?;
        batch.push(record);
    }
    if !batch.is_empty() {
        cancellation.check()?;
        sink.push(IrBatch::new(sink.next_sequence(), batch))?;
    }
    Ok(())
}

const fn empty_batch_usage() -> StreamUsage {
    StreamUsage::new(1, 0, 0, 0, 0, 0)
}

fn combine_batch_usage(current: StreamUsage, item: StreamUsage) -> Result<StreamUsage, SinkError> {
    Ok(StreamUsage::new(
        1,
        current
            .records()
            .checked_add(item.records())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .output_bytes()
            .checked_add(item.output_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .diagnostics()
            .checked_add(item.diagnostics())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .diagnostic_bytes()
            .checked_add(item.diagnostic_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .string_bytes()
            .checked_add(item.string_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
    ))
}

fn usage_fits(usage: StreamUsage, budget: rootlight_adapter_sdk::RemainingBudget) -> bool {
    let batch = budget.batch();
    let remaining = budget.remaining();
    usage.batches() <= remaining.batches()
        && usage.records() <= batch.max_records().min(remaining.records())
        && usage.output_bytes() <= batch.max_output_bytes().min(remaining.output_bytes())
        && usage.diagnostics() <= batch.max_diagnostics().min(remaining.diagnostics())
        && usage.diagnostic_bytes()
            <= batch
                .max_diagnostic_bytes()
                .min(remaining.diagnostic_bytes())
        && usage.string_bytes() <= remaining.string_bytes()
}

fn push_source_identity(identity: &mut Vec<u8>, source: &SourceRef) -> Result<(), AdapterError> {
    push_bytes(identity, source.repository().as_bytes())?;
    push_bytes(identity, source.generation().as_bytes())?;
    push_bytes(identity, source.span().file().as_bytes())?;
    identity.extend_from_slice(&source.span().start_byte().to_be_bytes());
    identity.extend_from_slice(&source.span().end_byte().to_be_bytes());
    push_bytes(identity, source.content_hash().as_bytes())
}

fn push_endpoint(identity: &mut Vec<u8>, endpoint: RelationEndpoint) -> Result<(), AdapterError> {
    match endpoint {
        RelationEndpoint::Repository(id) => {
            identity.push(1);
            push_bytes(identity, id.as_bytes())
        }
        RelationEndpoint::File(id) => {
            identity.push(2);
            push_bytes(identity, id.as_bytes())
        }
        RelationEndpoint::Entity(id) => {
            identity.push(3);
            push_bytes(identity, id.as_bytes())
        }
        RelationEndpoint::Occurrence(id) => {
            identity.push(4);
            push_bytes(identity, id.as_bytes())
        }
    }
}

fn push_text(identity: &mut Vec<u8>, text: &str) -> Result<(), AdapterError> {
    push_bytes(identity, text.as_bytes())
}

fn push_bytes(identity: &mut Vec<u8>, bytes: &[u8]) -> Result<(), AdapterError> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| provider_failure("treesitter-lowering-accounting"))?;
    identity.extend_from_slice(&length.to_be_bytes());
    identity.extend_from_slice(bytes);
    Ok(())
}

const fn entity_kind_label(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Module => "module",
        EntityKind::Namespace => "namespace",
        EntityKind::Class => "class",
        EntityKind::Struct => "struct",
        EntityKind::Enum => "enum",
        EntityKind::Trait => "trait",
        EntityKind::Interface => "interface",
        EntityKind::TypeAlias => "type-alias",
        EntityKind::Function => "function",
        EntityKind::Method => "method",
        EntityKind::Constructor => "constructor",
        EntityKind::Field => "field",
        EntityKind::Constant => "constant",
        EntityKind::Variable => "variable",
        EntityKind::Parameter => "parameter",
        _ => "entity",
    }
}

const fn syntax_fact_kind_tag(kind: SyntaxFactKind) -> u8 {
    match kind {
        SyntaxFactKind::Root => 1,
        SyntaxFactKind::Module => 2,
        SyntaxFactKind::Declaration => 3,
        SyntaxFactKind::Signature => 4,
        SyntaxFactKind::Import => 5,
        SyntaxFactKind::Scope => 6,
        SyntaxFactKind::Occurrence => 7,
        SyntaxFactKind::Comment => 8,
        SyntaxFactKind::StringLiteral => 9,
        SyntaxFactKind::EmbeddedRegion => 10,
        SyntaxFactKind::ErrorRecovery => 11,
        _ => 255,
    }
}

const fn occurrence_role_tag(role: OccurrenceRole) -> u8 {
    match role {
        OccurrenceRole::Definition => 1,
        OccurrenceRole::Declaration => 2,
        OccurrenceRole::Reference => 3,
        OccurrenceRole::CallSite => 4,
        OccurrenceRole::TypeUse => 5,
        OccurrenceRole::ImportUse => 6,
        OccurrenceRole::Write => 7,
        OccurrenceRole::Read => 8,
        OccurrenceRole::InheritanceUse => 9,
        OccurrenceRole::ImplementationUse => 10,
        OccurrenceRole::DecoratorUse => 11,
        OccurrenceRole::MacroUse => 12,
        OccurrenceRole::RouteUse => 13,
        OccurrenceRole::TestUse => 14,
        OccurrenceRole::Documentation => 15,
        OccurrenceRole::StringEvidence => 16,
    }
}

const fn relation_predicate_tag(predicate: RelationPredicate) -> u8 {
    match predicate {
        RelationPredicate::Contains => 1,
        _ => 255,
    }
}

const fn fact_domain_tag(domain: FactDomain) -> u8 {
    match domain {
        FactDomain::Files => 1,
        FactDomain::Entities => 2,
        FactDomain::Occurrences => 3,
        FactDomain::Relations => 4,
        FactDomain::Provenance => 5,
        FactDomain::SourceMappings => 6,
        FactDomain::Diagnostics => 7,
        FactDomain::Extensions => 8,
    }
}

const fn coverage_status_tag(status: CoverageStatus) -> u8 {
    match status {
        CoverageStatus::Complete => 1,
        CoverageStatus::Bounded => 2,
        CoverageStatus::Sampled => 3,
        CoverageStatus::Unknown => 4,
        _ => 255,
    }
}

const fn diagnostic_severity_tag(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Info => 1,
        DiagnosticSeverity::Warning => 2,
        DiagnosticSeverity::Error => 3,
    }
}

const fn skipped_reason_tag(reason: SkippedRegionReason) -> u8 {
    match reason {
        SkippedRegionReason::ResourceLimit => 1,
        SkippedRegionReason::ParseError => 2,
        SkippedRegionReason::MissingBuildContext => 3,
        SkippedRegionReason::AdapterFailure => 4,
        SkippedRegionReason::UnsupportedEncoding => 5,
        SkippedRegionReason::UnsupportedConstruct => 6,
    }
}

fn provider_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("hard-coded lowering diagnostic code is valid"),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, SyntaxKindLabel};
    use rootlight_ids::{FileId, GenerationId, RepositoryId};
    use rootlight_ir::{Confidence, SourceRef, SourceSpan};

    use super::*;

    fn declaration(label: &str) -> SyntaxFact {
        let file = FileId::from_bytes([3; 20]);
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(file, 0, 1).expect("test span is ordered"),
            0,
            SyntaxKindLabel::new(label).expect("test label is valid"),
        )
    }

    #[test]
    fn language_specific_declarations_map_conservatively() {
        for (label, expected) in [
            ("java.constructor.declaration", EntityKind::Constructor),
            ("java.record.declaration", EntityKind::Struct),
            ("java.annotation.declaration", EntityKind::Interface),
            ("java.annotation_element.declaration", EntityKind::Method),
            ("rust.type.declaration", EntityKind::TypeAlias),
            ("rust.const.declaration", EntityKind::Constant),
            ("rust.static.declaration", EntityKind::Variable),
        ] {
            assert_eq!(entity_kind(&declaration(label)), Some(expected));
        }
    }

    #[test]
    fn file_modules_use_only_closed_language_labels() {
        let file = FileId::from_bytes([3; 20]);
        let module = |label| {
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Module,
                SourceSpan::new(file, 0, 1).expect("test span is ordered"),
                0,
                SyntaxKindLabel::new(label).expect("test label is valid"),
            )
        };

        assert!(is_explicit_file_module(
            &module("python.file.module"),
            "python"
        ));
        assert!(is_explicit_file_module(
            &module("javascript.file.module"),
            "javascript"
        ));
        assert!(!is_explicit_file_module(&module("python.module"), "python"));
        assert!(!is_explicit_file_module(
            &module("java.file.module"),
            "java"
        ));
    }

    #[test]
    fn signature_discriminator_ignores_formatting_but_keeps_overloads_distinct() {
        let compact = canonical_signature("(x:i32)", 128)
            .expect("compact signature is checked")
            .expect("compact signature is usable");
        let spaced = canonical_signature("( x : i32 )", 128)
            .expect("spaced signature is checked")
            .expect("spaced signature is usable");
        let overload = canonical_signature("(x:u64)", 128)
            .expect("overload signature is checked")
            .expect("overload signature is usable");

        assert_eq!(compact, spaced);
        assert_ne!(compact, overload);
    }

    #[test]
    fn signature_capture_requires_both_role_and_signature_kind() {
        let file = FileId::from_bytes([3; 20]);
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(file, 0, 1).expect("test span is ordered"),
                0,
                SyntaxKindLabel::new(label).expect("test label is valid"),
            )
        };

        assert!(is_signature_capture(&fact(
            SyntaxFactKind::Signature,
            "rust.function.signature"
        )));
        assert!(!is_signature_capture(&fact(
            SyntaxFactKind::Occurrence,
            "rust.function.signature"
        )));
        assert!(!is_signature_capture(&fact(
            SyntaxFactKind::Signature,
            "rust.function.declaration"
        )));
    }

    #[test]
    fn symbol_identity_guard_rejects_distinct_inputs_for_one_id() {
        let symbol = SymbolId::from_bytes([7; 20]);
        let mut guards = BTreeMap::new();
        ensure_symbol_identity_collision_free(&mut guards, symbol, [1; 32])
            .expect("first identity input is admitted");
        ensure_symbol_identity_collision_free(&mut guards, symbol, [1; 32])
            .expect("equivalent identity input deduplicates");
        let error = ensure_symbol_identity_collision_free(&mut guards, symbol, [2; 32])
            .expect_err("distinct identity inputs cannot share a symbol ID");
        assert!(matches!(
            error,
            AdapterError::ProviderFailed { code }
                if code.as_str() == "treesitter-lowering-symbol-collision"
        ));
    }

    proptest! {
        #[test]
        fn occurrence_identity_ignores_parser_local_ids(first in any::<u64>(), second in any::<u64>()) {
            let repository = RepositoryId::from_bytes([1; 16]);
            let generation = GenerationId::from_bytes([2; 20]);
            let file = FileId::from_bytes([3; 20]);
            let span = SourceSpan::new(file, 4, 8).expect("property span is ordered");
            let source = SourceRef::new(
                repository,
                generation,
                span,
                content_hash(b"name"),
                None,
            );
            let label = SyntaxKindLabel::new("identifier").expect("property label is valid");
            let fact = |local_id| {
                SyntaxFact::new(
                    local_id,
                    None,
                    SyntaxFactKind::Occurrence,
                    span,
                    1,
                    label.clone(),
                )
            };
            let confidence = Confidence::new(SYNTAX_CONFIDENCE)
                .expect("hard-coded syntax confidence is valid");
            let provenance = derive_fact("property-provenance", b"fixture").id();
            let first = unresolved_occurrence(
                &fact(first),
                OccurrenceRole::Reference,
                None,
                provenance,
                confidence,
                source.clone(),
                "name",
            )
            .expect("first occurrence lowers");
            let second = unresolved_occurrence(
                &fact(second),
                OccurrenceRole::Reference,
                None,
                provenance,
                confidence,
                source,
                "name",
            )
            .expect("second occurrence lowers");

            prop_assert_eq!(first.id, second.id);
        }
    }
}
