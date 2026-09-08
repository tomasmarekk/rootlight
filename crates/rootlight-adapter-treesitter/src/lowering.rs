//! Parser-independent syntax-fact lowering into normalized IR.
//!
//! The analyzer injects a `ParseProvider` and never observes native Tree-sitter
//! types, so extraction can evolve independently from stable IR construction.

mod toml;
mod yaml;

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::Arc,
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisOutput, AnalysisReport,
    AnalysisRequest, CoverageReport, DiagnosticCode, DomainCoverage, IrBatch, IrBatchSink,
    IrRecord, LanguageAnalyzer, MemoryAdmissionPolicy, MemoryEnforcement, ParseOutput,
    ParseProvider, ProducerDescriptor, RequestError, ResourceKind, ResourceUsage, SinkError,
    StreamEnd, StreamUsage, SyntaxFact, SyntaxFactKind, execute_analysis, execute_parse,
    structural_entity_kind, structural_entity_kind_from_source, structural_syntax_fact_order,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{
    ContentHash, FactId, FileId, RepositoryId, SymbolId, SymbolIdentity, content_hash,
    derive_symbol,
};
use rootlight_ir::{
    AnalysisTier, Confidence, ContainerRef, CoverageRecord, CoverageScope, CoverageStatus,
    DiagnosticRecord, DiagnosticSeverity, EntityFlag, EntityKind, EntityRecord, EntityVisibility,
    EvidenceKind, ExtensionEnvelope, ExtensionSupport, FactDomain, FactEvidence, FactRef,
    FileIdentityClaim, FileRecord, IrLimits, LEXICAL_EXTENSION_NAMESPACE,
    LEXICAL_EXTENSION_VERSION, LexicalEvidenceFormat, LexicalEvidenceKind, LexicalEvidenceV1,
    OccurrenceRecord, OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind,
    ProvenanceRecord, RelationEndpoint, RelationPredicate, RelationRecord, SkippedRegion,
    SkippedRegionReason, SourceRef, SourceSpan, SymbolIdentityClaim, canonical_rust_impl_scope,
    canonical_symbol_signature, derive_coverage_record_id, derive_diagnostic_record_id,
    derive_occurrence_record_id, derive_provenance_record_id, derive_relation_record_id,
    derive_rust_impl_scope_identity, derive_skipped_region_id, entity_kind_identity_label,
    new_file_identity_claim_envelope, new_lexical_evidence_envelope,
    new_symbol_identity_claim_envelope,
};

const SYNTAX_FALLBACK_TIER: AnalysisTier = AnalysisTier::TierD;
const RUST_STRUCTURAL_TIER: AnalysisTier = AnalysisTier::TierB;
const SYNTAX_CONFIDENCE: u16 = 900;
const CONTAINMENT_CONFIDENCE: u16 = 1_000;
const CANCELLATION_CHECK_INTERVAL: usize = 64;
const MAX_FRONTEND_VERSION_BYTES: usize = 128;

const SCOPE_COLLISION_GUARD_CONTEXT: &str = "rootlight/treesitter-scope-collision-guard/v1";
const ENTITY_IDENTITY_GUARD_CONTEXT: &str = "rootlight/treesitter-entity-identity-guard/v1";
const STABLE_SCOPE_IDENTITY_UNAVAILABLE: &str = "stable-scope-identity-unavailable";

/// Tree-sitter syntax analyzer backed by an injected parser-independent provider.
#[derive(Clone)]
pub struct TreeSitterAnalyzer {
    parser: Arc<dyn ParseProvider>,
    instance: Arc<()>,
    descriptor: ProducerDescriptor,
    frontend_version: String,
    binary_digest: ContentHash,
}

/// Reusable, source-free Tree-sitter parse output for one immutable file.
///
/// The artifact retains parser-local syntax facts and stable diagnostics, not
/// source bytes. Its identity intentionally excludes the generation so an
/// exact dependency match can reuse parsing while lowering fresh generation-
/// bound IR and fact identities.
pub struct TreeSitterStructuralArtifact {
    analyzer_instance: Arc<()>,
    repository: RepositoryId,
    file: FileId,
    content_hash: ContentHash,
    parse_context_hash: ContentHash,
    limits: AnalysisLimits,
    parse_output: ParseOutput,
    accounted_bytes: usize,
}

impl TreeSitterStructuralArtifact {
    /// Returns the stable repository-scoped file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the immutable source-content digest.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns the deterministic logical retained-byte charge for admission.
    #[must_use]
    pub const fn accounted_bytes(&self) -> usize {
        self.accounted_bytes
    }

    /// Returns the exact parser-local syntax facts retained by this artifact.
    #[must_use]
    pub fn syntax_fact_count(&self) -> usize {
        self.parse_output.facts().len()
    }

    /// Returns the exact retained declaration-identity syntax-fact demand.
    ///
    /// Complete identity facts are retained even when optional extraction is
    /// bounded, so an exact-match incremental successor can reuse this count
    /// without reparsing unchanged source.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for cancellation, invalid retained parent
    /// identity, accounting overflow, or bounded allocation failure.
    pub fn required_syntax_fact_count(
        &self,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        required_syntax_fact_count_from_output(&self.parse_output, cancellation)
    }

    /// Returns whether this artifact can be replayed under the supplied limits.
    ///
    /// An exact limit match is always compatible. A changed syntax-record
    /// partition is also compatible when the retained parse was complete,
    /// still fits, and every other parser and lowering limit is unchanged.
    #[must_use]
    pub fn is_compatible_with_limits(&self, limits: &AnalysisLimits) -> bool {
        self.limits == *limits
            || (self.parse_output.facts().len() < limits.syntax_stream().max_records()
                && self
                    .parse_output
                    .diagnostics()
                    .iter()
                    .all(|diagnostic| diagnostic.code().as_str() != "syntax-extraction-limit")
                && analysis_limit_shape_matches(&self.limits, limits))
    }
}

impl fmt::Debug for TreeSitterStructuralArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TreeSitterStructuralArtifact")
            .field("file", &self.file)
            .field("content_hash", &self.content_hash)
            .field("syntax_facts", &self.parse_output.facts().len())
            .field("diagnostics", &self.parse_output.diagnostics().len())
            .field("accounted_bytes", &self.accounted_bytes)
            .finish()
    }
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
    /// The reviewed Rust structural profile was requested for another language.
    #[error("the reviewed Rust structural profile requires the rust language identity")]
    UnsupportedRustStructuralLanguage,
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
        Self::new_with_tier(
            parser,
            producer,
            language,
            frontend_version,
            binary_digest,
            SYNTAX_FALLBACK_TIER,
        )
    }

    /// Creates the reviewed Rust Tier B structural analyzer.
    ///
    /// This profile combines Rootlight's audited Rust query pack, stable
    /// lexical-scope lowering, and the separate resolver. It does not claim
    /// compiler or build-context precision and therefore cannot advertise
    /// Tier A.
    ///
    /// # Errors
    ///
    /// Returns [`TreeSitterAnalyzerConfigError`] for an invalid frontend label
    /// or a language identity other than `rust`.
    pub fn new_rust_structural(
        parser: Arc<dyn ParseProvider>,
        producer: ProducerIdentity,
        language: rootlight_adapter_sdk::LanguageId,
        frontend_version: &str,
        binary_digest: ContentHash,
    ) -> Result<Self, TreeSitterAnalyzerConfigError> {
        if language.as_str() != "rust" {
            return Err(TreeSitterAnalyzerConfigError::UnsupportedRustStructuralLanguage);
        }
        Self::new_with_tier(
            parser,
            producer,
            language,
            frontend_version,
            binary_digest,
            RUST_STRUCTURAL_TIER,
        )
    }

    fn new_with_tier(
        parser: Arc<dyn ParseProvider>,
        producer: ProducerIdentity,
        language: rootlight_adapter_sdk::LanguageId,
        frontend_version: &str,
        binary_digest: ContentHash,
        tier: AnalysisTier,
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
            tier,
            MemoryEnforcement::Unavailable,
            true,
        );
        Ok(Self {
            parser,
            instance: Arc::new(()),
            descriptor,
            frontend_version: frontend_version.to_owned(),
            binary_digest,
        })
    }

    /// Parses and lowers one file while returning reusable structural work.
    ///
    /// The returned artifact is safe to retain only under a bounded caller-
    /// owned policy. A later reuse still performs full generation-bound
    /// lowering, validation, and transactional IR admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for the same request, parser, analyzer,
    /// resource, report, cancellation, or sink failures as the standard SDK
    /// execution path.
    pub fn analyze_and_capture(
        &self,
        request: &AnalysisRequest<'_>,
        extensions: ExtensionSupport,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<(AnalysisOutput, TreeSitterStructuralArtifact), AdapterError> {
        cancellation.check()?;
        self.validate_request_identity(request)?;
        let parse_output = self.parse_request(request, cancellation)?;
        let prepared = PreparsedTreeSitterAnalyzer {
            analyzer: self,
            parse_output: &parse_output,
        };
        let output = execute_analysis(&prepared, request, extensions, memory_policy, cancellation)?;
        let artifact =
            TreeSitterStructuralArtifact::capture(self, request, parse_output, cancellation)?;
        Ok((output, artifact))
    }

    /// Lowers one exact-match structural artifact into a new generation.
    ///
    /// Repository, file, content, path, parse context, limits, producer,
    /// frontend, and binary identity must all match. Only the generation may
    /// differ.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the artifact is stale or belongs to
    /// another analysis context, or for the same bounded lowering failures as
    /// [`Self::analyze_and_capture`].
    pub fn analyze_from_artifact(
        &self,
        request: &AnalysisRequest<'_>,
        artifact: &TreeSitterStructuralArtifact,
        extensions: ExtensionSupport,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<AnalysisOutput, AdapterError> {
        cancellation.check()?;
        self.validate_request_identity(request)?;
        if !artifact.matches(self, request, cancellation)? {
            return Err(provider_failure("treesitter-structural-artifact-mismatch"));
        }
        let prepared = PreparsedTreeSitterAnalyzer {
            analyzer: self,
            parse_output: &artifact.parse_output,
        };
        execute_analysis(&prepared, request, extensions, memory_policy, cancellation)
    }

    /// Emits bounded file identity and coverage facts for invalid UTF-8 input.
    ///
    /// The ordinary parser contract continues to reject malformed UTF-8. A
    /// repository orchestrator may use this explicit fallback to preserve the
    /// rest of a generation while declaring the affected file incomplete.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the request does not belong to this
    /// analyzer, the source is valid UTF-8, cancellation wins, or the normal
    /// bounded sink and report contracts reject the fallback output.
    pub fn analyze_unsupported_encoding(
        &self,
        request: &AnalysisRequest<'_>,
        extensions: ExtensionSupport,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<AnalysisOutput, AdapterError> {
        cancellation.check()?;
        self.validate_request_identity(request)?;
        if std::str::from_utf8(request.source().bytes()).is_ok() {
            return Err(provider_failure(
                "treesitter-unsupported-encoding-fallback-requires-invalid-utf8",
            ));
        }
        execute_analysis(
            &UnsupportedEncodingAnalyzer { analyzer: self },
            request,
            extensions,
            memory_policy,
            cancellation,
        )
    }

    fn validate_request_identity(&self, request: &AnalysisRequest<'_>) -> Result<(), AdapterError> {
        if self.descriptor.language() != request.language() {
            return Err(RequestError::UnsupportedLanguage.into());
        }
        if self.descriptor.tier() != request.tier() {
            return Err(RequestError::UnsupportedTier.into());
        }
        if request.encoding().as_str() != "utf-8" {
            return Err(RequestError::UnsupportedEncoding.into());
        }
        if request.generated_status().is_none() {
            return Err(RequestError::GeneratedStatusRequired.into());
        }
        Ok(())
    }

    fn parse_request(
        &self,
        request: &AnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<ParseOutput, AdapterError> {
        cancellation.check()?;
        let parse_request = request.to_parse_request();
        execute_parse(
            self.parser.as_ref(),
            &parse_request,
            memory_policy_for(self.parser.capabilities().memory_enforcement()),
            cancellation,
        )
    }

    fn lower_parse_output(
        &self,
        request: &AnalysisRequest<'_>,
        parse_output: &ParseOutput,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<AnalysisReport, AdapterError> {
        let lowered = Lowering::new(self, request, parse_output)?.lower(cancellation)?;
        emit_records(lowered.records, request, sink, cancellation)?;
        cancellation.check()?;

        let usage = sink.staged_usage();
        let parse_resources = parse_output.report().resources();
        let coverage = CoverageReport::new(
            self.descriptor.tier(),
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

impl TreeSitterStructuralArtifact {
    fn capture(
        analyzer: &TreeSitterAnalyzer,
        request: &AnalysisRequest<'_>,
        parse_output: ParseOutput,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let parse_context_hash = structural_parse_context_hash(request, cancellation)?;
        let accounted_bytes = structural_artifact_bytes(&parse_output)?;
        Ok(Self {
            analyzer_instance: Arc::clone(&analyzer.instance),
            repository: request.source().source_ref().repository(),
            file: request.source().source_ref().span().file(),
            content_hash: request.source().source_ref().content_hash(),
            parse_context_hash,
            limits: request.limits().clone(),
            parse_output,
            accounted_bytes,
        })
    }

    fn matches(
        &self,
        analyzer: &TreeSitterAnalyzer,
        request: &AnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let source = request.source().source_ref();
        let context_matches =
            structural_parse_context_hash(request, cancellation)? == self.parse_context_hash;
        Ok(Arc::ptr_eq(&self.analyzer_instance, &analyzer.instance)
            && self.repository == source.repository()
            && self.file == source.span().file()
            && self.content_hash == source.content_hash()
            && context_matches
            && self.is_compatible_with_limits(request.limits()))
    }
}

fn required_syntax_fact_count_from_output(
    output: &ParseOutput,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    cancellation.check()?;
    let facts = output.facts();
    let mut contains_declaration = Vec::new();
    contains_declaration
        .try_reserve_exact(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    contains_declaration.resize(facts.len(), false);
    for (index, fact) in facts.iter().enumerate() {
        check_periodically(index, cancellation)?;
        contains_declaration[index] = fact.kind() == SyntaxFactKind::Declaration;
    }
    let mut propagation_order = Vec::new();
    propagation_order
        .try_reserve_exact(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    propagation_order.extend(0..facts.len());
    cancellation.check()?;
    // Parent-local IDs are canonical but not ordered before their children in
    // every grammar. Descending depth makes one propagation pass sufficient;
    // the atomic sort is bounded by the retained syntax-fact ceiling.
    propagation_order
        .sort_unstable_by_key(|index| Reverse((facts[*index].depth(), facts[*index].local_id())));
    cancellation.check()?;
    for (visit, index) in propagation_order.into_iter().enumerate() {
        check_periodically(visit, cancellation)?;
        if !contains_declaration[index] {
            continue;
        }
        let Some(parent_local_id) = facts[index].parent() else {
            continue;
        };
        let parent = usize::try_from(parent_local_id)
            .ok()
            .and_then(|parent| parent.checked_sub(1))
            .ok_or_else(|| provider_failure("treesitter-structural-artifact-parent"))?;
        facts
            .get(parent)
            .filter(|parent_fact| {
                parent_fact.local_id() == parent_local_id
                    && parent_fact.depth() < facts[index].depth()
            })
            .ok_or_else(|| provider_failure("treesitter-structural-artifact-parent"))?;
        contains_declaration[parent] = true;
    }

    let mut required = 0usize;
    for (index, fact) in facts.iter().enumerate() {
        check_periodically(index, cancellation)?;
        let identity_fact = matches!(
            fact.kind(),
            SyntaxFactKind::Root
                | SyntaxFactKind::Module
                | SyntaxFactKind::Declaration
                | SyntaxFactKind::Signature
        ) || (fact.kind() == SyntaxFactKind::Scope
            && (contains_declaration[index]
                || fact.syntax_kind().as_str().starts_with("json.")
                || fact.syntax_kind().as_str().starts_with("toml.")
                || fact.syntax_kind().as_str().starts_with("yaml.")))
            || (fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition"));
        if identity_fact {
            required = required
                .checked_add(1)
                .ok_or_else(|| provider_failure("treesitter-structural-artifact-accounting"))?;
        }
    }
    cancellation.check()?;
    Ok(required)
}

fn analysis_limit_shape_matches(left: &AnalysisLimits, right: &AnalysisLimits) -> bool {
    let left_syntax = left.syntax_stream();
    let right_syntax = right.syntax_stream();
    let left_batch = left_syntax.batch();
    let right_batch = right_syntax.batch();
    left.max_source_bytes() == right.max_source_bytes()
        && left.max_syntax_nodes() == right.max_syntax_nodes()
        && left.max_syntax_depth() == right.max_syntax_depth()
        && left.max_embedded_ranges() == right.max_embedded_ranges()
        && left.max_reported_memory_bytes() == right.max_reported_memory_bytes()
        && left_syntax.max_batches() == right_syntax.max_batches()
        && left_syntax.max_output_bytes() == right_syntax.max_output_bytes()
        && left_syntax.max_diagnostics() == right_syntax.max_diagnostics()
        && left_syntax.max_diagnostic_bytes() == right_syntax.max_diagnostic_bytes()
        && left_syntax.max_string_bytes() == right_syntax.max_string_bytes()
        && left_batch.max_output_bytes() == right_batch.max_output_bytes()
        && left_batch.max_diagnostics() == right_batch.max_diagnostics()
        && left_batch.max_diagnostic_bytes() == right_batch.max_diagnostic_bytes()
        && left.ir_stream() == right.ir_stream()
        && left.ir() == right.ir()
        && left.project() == right.project()
}

struct PreparsedTreeSitterAnalyzer<'a> {
    analyzer: &'a TreeSitterAnalyzer,
    parse_output: &'a ParseOutput,
}

impl LanguageAnalyzer for PreparsedTreeSitterAnalyzer<'_> {
    fn descriptor(&self) -> &ProducerDescriptor {
        &self.analyzer.descriptor
    }

    fn analyze(
        &self,
        request: &AnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<AnalysisReport, AdapterError> {
        self.analyzer
            .lower_parse_output(request, self.parse_output, sink, cancellation)
    }
}

struct UnsupportedEncodingAnalyzer<'a> {
    analyzer: &'a TreeSitterAnalyzer,
}

impl LanguageAnalyzer for UnsupportedEncodingAnalyzer<'_> {
    fn descriptor(&self) -> &ProducerDescriptor {
        &self.analyzer.descriptor
    }

    fn analyze(
        &self,
        request: &AnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<AnalysisReport, AdapterError> {
        let source = request.source().source_ref();
        let provenance = parser_provenance(self.analyzer, request, source)?;
        let provenance_id = provenance.id;
        let file = parser_file(request, source, provenance_id)?;
        let file_claim = FileIdentityClaim {
            file: file.id,
            repository: file.repository,
            path: file.path.clone(),
            path_identity: request.source().path().identity_bytes().to_vec(),
            content_hash: file.content_hash,
            byte_length: file.byte_length,
        };
        let file_claim = new_file_identity_claim_envelope(
            &file_claim,
            source.generation(),
            provenance_id,
            source.clone(),
        )
        .map_err(|_| provider_failure("treesitter-file-identity-claim"))?;
        let mut extension_bytes = 0;
        ensure_extension_budget(&file_claim, 0, &mut extension_bytes, request.limits().ir())?;

        let skipped_domains = [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ];
        let mut skipped_regions = Vec::new();
        skipped_regions
            .try_reserve_exact(skipped_domains.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for domain in skipped_domains {
            skipped_regions.push(skipped_region(
                source,
                source.span(),
                domain,
                SkippedRegionReason::UnsupportedEncoding,
                "invalid-utf8",
                provenance_id,
            )?);
        }

        let domain_coverage = [
            DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0),
            DomainCoverage::new(FactDomain::Entities, CoverageStatus::Bounded, 1, 0, 1),
            DomainCoverage::new(FactDomain::Occurrences, CoverageStatus::Bounded, 1, 0, 1),
            DomainCoverage::new(FactDomain::Relations, CoverageStatus::Bounded, 1, 0, 1),
            DomainCoverage::new(FactDomain::Provenance, CoverageStatus::Complete, 1, 1, 0),
            DomainCoverage::new(
                FactDomain::SourceMappings,
                CoverageStatus::Complete,
                0,
                0,
                0,
            ),
            DomainCoverage::new(FactDomain::Diagnostics, CoverageStatus::Complete, 1, 1, 0),
            DomainCoverage::new(FactDomain::Extensions, CoverageStatus::Complete, 1, 1, 0),
        ]
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(AdapterError::from)?;
        let coverage_records = coverage_records(
            source,
            provenance_id,
            self.analyzer.descriptor.tier(),
            &domain_coverage,
        )?;
        let mut diagnostic = DiagnosticRecord {
            id: FactId::from_bytes([0; 20]),
            repository: source.repository(),
            generation: source.generation(),
            code: "invalid-utf8".to_owned(),
            message: "source is not valid utf-8".to_owned(),
            severity: DiagnosticSeverity::Error,
            source: Some(source.clone()),
            coverage_effect: CoverageStatus::Bounded,
            provenance: provenance_id,
            evidence: direct_evidence(source.clone()),
        };
        diagnostic.id = derive_diagnostic_record_id(&diagnostic)
            .map_err(|_| provider_failure("treesitter-diagnostic-identity"))?;

        let mut records = Vec::new();
        records
            .try_reserve_exact(
                3_usize
                    .checked_add(skipped_regions.len())
                    .and_then(|count| count.checked_add(coverage_records.len()))
                    .ok_or(SinkError::AccountingOverflow)?,
            )
            .map_err(|_| SinkError::AllocationFailed)?;
        records.push(IrRecord::File(file));
        records.push(IrRecord::Provenance(provenance));
        records.extend(coverage_records.into_iter().map(IrRecord::Coverage));
        records.extend(skipped_regions.into_iter().map(IrRecord::SkippedRegion));
        records.push(IrRecord::Diagnostic(diagnostic));
        records.push(IrRecord::Extension(file_claim));
        emit_records(records, request, sink, cancellation)?;
        cancellation.check()?;

        let usage = sink.staged_usage();
        let coverage = CoverageReport::new(
            self.analyzer.descriptor.tier(),
            CoverageStatus::Bounded,
            request.source().bytes().len(),
            0,
            skipped_domains.len(),
            domain_coverage,
        )
        .map_err(AdapterError::from)?;
        let resources = ResourceUsage::new(
            request.source().bytes().len(),
            usage.records(),
            0,
            0,
            None,
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
        self.validate_request_identity(request)?;
        let parse_output = self.parse_request(request, cancellation)?;
        self.lower_parse_output(request, &parse_output, sink, cancellation)
    }
}

fn structural_parse_context_hash(
    request: &AnalysisRequest<'_>,
    cancellation: &Cancellation,
) -> Result<ContentHash, AdapterError> {
    cancellation.check()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.treesitter.structural-artifact-context/1\0");
    hash_sized_bytes(&mut hasher, request.source().path().identity_bytes())?;
    hash_sized_bytes(&mut hasher, request.language().as_str().as_bytes())?;
    hash_sized_bytes(&mut hasher, request.encoding().as_str().as_bytes())?;
    let range_count = u64::try_from(request.included_ranges().len())
        .map_err(|_| provider_failure("treesitter-structural-artifact-accounting"))?;
    hasher.update(&range_count.to_be_bytes());
    for (index, range) in request.included_ranges().iter().enumerate() {
        check_periodically(index, cancellation)?;
        hasher.update(range.span().file().as_bytes());
        hasher.update(&range.span().start_byte().to_be_bytes());
        hasher.update(&range.span().end_byte().to_be_bytes());
        hash_sized_bytes(&mut hasher, range.language().as_str().as_bytes())?;
    }
    cancellation.check()?;
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn hash_sized_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) -> Result<(), AdapterError> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| provider_failure("treesitter-structural-artifact-accounting"))?;
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

fn structural_artifact_bytes(output: &ParseOutput) -> Result<usize, AdapterError> {
    let stream = output.report().resources().stream();
    std::mem::size_of::<TreeSitterStructuralArtifact>()
        .checked_add(stream.output_bytes())
        .and_then(|bytes| {
            output
                .facts()
                .len()
                .checked_mul(std::mem::size_of::<SyntaxFact>())
                .and_then(|fact_bytes| bytes.checked_add(fact_bytes))
        })
        .and_then(|bytes| {
            output
                .diagnostics()
                .len()
                .checked_mul(std::mem::size_of::<AdapterDiagnostic>())
                .and_then(|diagnostic_bytes| bytes.checked_add(diagnostic_bytes))
        })
        .ok_or_else(|| provider_failure("treesitter-structural-artifact-accounting"))
}

fn memory_policy_for(enforcement: MemoryEnforcement) -> MemoryAdmissionPolicy {
    match enforcement {
        MemoryEnforcement::Unavailable => {
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback
        }
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
    let mut lexical_relation_candidates = 0_usize;
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
        if let Some((_, detail)) = source_coverage_gap(fact) {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(&mut string_bytes, detail.len(), limits)?;
        }
        if request.language().as_str() == "yaml"
            && fact.syntax_kind().as_str() == "yaml.tag.signature"
        {
            // Tag construction can be unavailable even when lexical signature
            // retention succeeds; both gaps require independent reservations.
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "yaml-node-tag-construction-unavailable".len(),
                limits,
            )?;
        }
        if request.language().as_str() == "yaml"
            && matches!(
                fact.syntax_kind().as_str(),
                "yaml.document.scope" | "yaml.node_key.definition"
            )
        {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "yaml-document-schema-uncertain".len(),
                limits,
            )?;
        }
        if let Some(kind) = structural_entity_kind(fact) {
            entity_candidates = checked_add(entity_candidates, 1)?;
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
            // Capture association happens after graph validation. Reserve one
            // maximum-sized entity gap per candidate so missing definitions or
            // unsupported stable scopes or duplicate data keys cannot bypass
            // string quotas.
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                STABLE_SCOPE_IDENTITY_UNAVAILABLE.len(),
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
            if request.language().as_str() == "lua"
                && parse_output.report().coverage().status() == CoverageStatus::Complete
                && fact.syntax_kind().as_str() == "lua.identifier.reference"
            {
                lexical_relation_candidates = checked_add(lexical_relation_candidates, 1)?;
            }
            if request.language().as_str() == "yaml"
                && fact.syntax_kind().as_str() == "yaml.alias.reference"
            {
                lexical_relation_candidates = checked_add(lexical_relation_candidates, 1)?;
                skipped_candidates = checked_add(skipped_candidates, 1)?;
                account_string(
                    &mut string_bytes,
                    "yaml-alias-target-unavailable".len(),
                    limits,
                )?;
            }
        }
        if is_definition_capture(fact) {
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
        }
        if fact.kind() == SyntaxFactKind::Comment {
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "lexical-evidence-unavailable"
                    .len()
                    .max("lexical-evidence-resource-limit".len()),
                limits,
            )?;
        }
        if is_signature_capture(fact) {
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "signature-capture-unavailable"
                    .len()
                    .max("lexical-evidence-resource-limit".len()),
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
    let required_extension_candidates = checked_add(1, entity_candidates)?;
    require_resource_limit(
        ResourceKind::Records,
        required_extension_candidates,
        limits.max_extensions,
    )?;
    let admitted_optional_extensions =
        extension_candidates.min(limits.max_extensions - required_extension_candidates);
    for _ in 0..admitted_optional_extensions {
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
    let relation_candidates = checked_add(entity_candidates, lexical_relation_candidates)?;
    require_resource_limit(
        ResourceKind::Records,
        relation_candidates,
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
    let extension_records =
        checked_add(required_extension_candidates, admitted_optional_extensions)?;
    let total_records = [
        2,
        entity_candidates,
        occurrence_candidates,
        relation_candidates,
        8,
        skipped_candidates,
        diagnostic_count,
        extension_records,
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

        let mut entity_plan = self.entity_drafts(cancellation, &mut total_string_bytes)?;
        let drafts = &entity_plan.drafts;
        let mut materialized = HashMap::new();
        for (index, draft) in drafts.iter().enumerate() {
            check_periodically(index, cancellation)?;
            let entity = materialize_entity(
                draft,
                self.full_source,
                self.request.build_context(),
                provenance_id,
                self.analyzer.descriptor.tier(),
                self.request.limits().ir().max_string_bytes,
                &materialized,
            )?;
            materialized.insert(draft.local_id, entity);
        }
        entity_plan
            .unsupported_scope_entities
            .extend(exclude_ambiguous_entities(&mut materialized));

        let mut entities = BTreeMap::<SymbolId, EntityRecord>::new();
        let mut symbol_claims = BTreeMap::<SymbolId, SymbolIdentityClaim>::new();
        let mut identity_guards = BTreeMap::<SymbolId, [u8; 32]>::new();
        for entity in materialized.values() {
            if record_symbol_identity_guard(
                &mut identity_guards,
                entity.record.id,
                entity.identity_guard,
            ) {
                return Err(provider_failure("treesitter-lowering-symbol-collision"));
            }
            match entities.get(&entity.record.id) {
                Some(existing) => {
                    if !equivalent_entity_projection(existing, &entity.record) {
                        return Err(provider_failure("treesitter-lowering-symbol-collision"));
                    }
                    if entity_source_span(existing) > entity_source_span(&entity.record) {
                        entities.insert(entity.record.id, entity.record.clone());
                        symbol_claims.insert(entity.record.id, entity.identity_claim.clone());
                    }
                }
                _ => {
                    entities.insert(entity.record.id, entity.record.clone());
                    symbol_claims.insert(entity.record.id, entity.identity_claim.clone());
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
        for &(span, detail) in &entity_plan.yaml_warnings {
            cancellation.check()?;
            let region = skipped_region(
                self.full_source,
                span,
                FactDomain::Entities,
                SkippedRegionReason::UnsupportedConstruct,
                detail,
                provenance_id,
            )?;
            skipped.insert(region.id, region);
        }
        let facts_by_id: HashMap<_, _> = self
            .parse_output
            .facts()
            .iter()
            .map(|fact| (fact.local_id(), fact))
            .collect();
        let terminal_call_names = terminal_call_names(self.parse_output.facts(), cancellation)?;
        // A truncated capture plan may omit a shadowing declaration. Exact lexical
        // targets require a complete plan even when remaining text is readable.
        let lua_bindings = if self.request.language().as_str() == "lua"
            && self.parse_output.report().coverage().status() == CoverageStatus::Complete
        {
            let symbols = materialized
                .values()
                .filter_map(|entity| {
                    entity
                        .definition_local_id
                        .map(|definition| (definition, entity.record.id))
                })
                .collect();
            Some(crate::lua_bindings::LuaBindings::new(
                self.parse_output.facts(),
                self.request.source().bytes(),
                &symbols,
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?)
        } else {
            None
        };

        let file_claim = FileIdentityClaim {
            file: file.id,
            repository: file.repository,
            path: file.path.clone(),
            path_identity: self.request.source().path().identity_bytes().to_vec(),
            content_hash: file.content_hash,
            byte_length: file.byte_length,
        };
        let file_claim_envelope = new_file_identity_claim_envelope(
            &file_claim,
            self.full_source.generation(),
            provenance_id,
            self.full_source.clone(),
        )
        .map_err(|_| provider_failure("treesitter-file-identity-claim"))?;
        ensure_extension_budget(
            &file_claim_envelope,
            extensions.len(),
            &mut extension_bytes,
            self.request.limits().ir(),
        )?;
        extensions.insert(file_claim_envelope.id, file_claim_envelope);
        for claim in symbol_claims.values() {
            let entity = entities
                .get(&claim.symbol)
                .ok_or_else(|| provider_failure("treesitter-symbol-identity-claim"))?;
            let source = entity
                .evidence
                .source
                .clone()
                .ok_or_else(|| provider_failure("treesitter-symbol-identity-claim"))?;
            let envelope = new_symbol_identity_claim_envelope(
                claim,
                self.full_source.generation(),
                provenance_id,
                source,
            )
            .map_err(|_| provider_failure("treesitter-symbol-identity-claim"))?;
            ensure_extension_budget(
                &envelope,
                extensions.len(),
                &mut extension_bytes,
                self.request.limits().ir(),
            )?;
            extensions.insert(envelope.id, envelope);
        }

        for (index, fact) in self.parse_output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            let source = source_for_span(self.full_source, fact.span());
            if let Some((domain, detail)) = source_coverage_gap(fact) {
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    domain,
                    SkippedRegionReason::UnsupportedConstruct,
                    detail,
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            }
            if entity_plan.duplicate_data_keys.contains(&fact.local_id()) {
                // Both written occurrences remain searchable, but their data
                // meaning is not a valid unique YAML mapping entry.
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Entities,
                    SkippedRegionReason::UnsupportedConstruct,
                    "yaml-duplicate-mapping-key",
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            }
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
                    // The entity retains the full declaration for source.read;
                    // its definition occurrence identifies the original name token.
                    let definition_span = entity_plan
                        .yaml_key_sources
                        .get(&definition_local_id)
                        .copied()
                        .unwrap_or(definition_fact.span());
                    let occurrence = declaration_occurrence(
                        definition_fact,
                        entity,
                        provenance_id,
                        syntax_confidence,
                        &source_for_span(self.full_source, definition_span),
                        content_hash(self.text_for_span(definition_span)?.as_bytes()),
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
                        if !insert_optional_extension(
                            envelope,
                            &mut extensions,
                            &mut extension_bytes,
                            self.request.limits().ir(),
                        )? {
                            let region = skipped_region(
                                self.full_source,
                                signature_span,
                                FactDomain::Extensions,
                                SkippedRegionReason::ResourceLimit,
                                "lexical-evidence-resource-limit",
                                provenance_id,
                            )?;
                            skipped.insert(region.id, region);
                        }
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
                let terminal_call_name = terminal_call_names
                    .get(&fact.local_id())
                    .and_then(|local_id| facts_by_id.get(local_id))
                    .map(|terminal| self.text_for_span(terminal.span()))
                    .transpose()?;
                let resolution_text = structural_resolution_text(
                    self.request.language().as_str(),
                    fact,
                    text,
                    terminal_call_name,
                    self.request.limits().ir().max_string_bytes,
                );
                let enclosing = entity_plan
                    .nearest_entity_ancestor
                    .get(&fact.local_id())
                    .copied()
                    .flatten()
                    .and_then(|local_id| materialized.get(&local_id))
                    .map(|entity| entity.record.id);
                let mut occurrence = unresolved_occurrence(
                    fact,
                    role,
                    enclosing,
                    provenance_id,
                    syntax_confidence,
                    source.clone(),
                    resolution_text.as_deref().unwrap_or(text),
                )?;
                if resolution_text.is_none() {
                    // An undecodable name is not an alternative raw spelling. Keep its
                    // source, but prevent name-only resolution from guessing a target.
                    occurrence.syntax_kind = if role == OccurrenceRole::CallSite {
                        "r.unavailable_name.call"
                    } else {
                        "r.unavailable_name.reference"
                    }
                    .to_owned();
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let region = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Occurrences,
                        SkippedRegionReason::UnsupportedConstruct,
                        "r-reference-name-unavailable",
                        provenance_id,
                    )?;
                    skipped.insert(region.id, region);
                }
                if let Some(bindings) = &lua_bindings
                    && let Some(symbol) = bindings.resolve(fact, text, cancellation)?
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let relation = lexical_reference_relation(&occurrence, symbol)?;
                    relations.insert(relation.id, relation);
                }
                if self.request.language().as_str() == "yaml"
                    && fact.syntax_kind().as_str() == "yaml.alias.reference"
                {
                    let target = (self.parse_output.report().coverage().status()
                        == CoverageStatus::Complete)
                        .then(|| entity_plan.yaml_aliases.get(&fact.local_id()))
                        .flatten()
                        .and_then(|local| materialized.get(local));
                    if let Some(target) = target {
                        occurrence.target = OccurrenceTarget::Resolved {
                            symbol: target.record.id,
                        };
                        occurrence.id = derive_occurrence_record_id(&occurrence)
                            .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                        let relation = lexical_reference_relation(&occurrence, target.record.id)?;
                        relations.insert(relation.id, relation);
                    } else {
                        let region = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Relations,
                            SkippedRegionReason::UnsupportedConstruct,
                            "yaml-alias-target-unavailable",
                            provenance_id,
                        )?;
                        skipped.insert(region.id, region);
                    }
                }
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
                        if !insert_optional_extension(
                            envelope,
                            &mut extensions,
                            &mut extension_bytes,
                            self.request.limits().ir(),
                        )? {
                            let region = skipped_region(
                                self.full_source,
                                fact.span(),
                                FactDomain::Extensions,
                                SkippedRegionReason::ResourceLimit,
                                "lexical-evidence-resource-limit",
                                provenance_id,
                            )?;
                            skipped.insert(region.id, region);
                        }
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
                let detail = if entity_plan
                    .unsupported_scope_entities
                    .contains(&fact.local_id())
                {
                    STABLE_SCOPE_IDENTITY_UNAVAILABLE
                } else {
                    "declaration-name-unavailable"
                };
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Entities,
                    SkippedRegionReason::UnsupportedConstruct,
                    detail,
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
        let coverage_records = coverage_records(
            self.full_source,
            provenance_id,
            self.analyzer.descriptor.tier(),
            &domain_coverage,
        )?;

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
        parser_provenance(self.analyzer, self.request, self.full_source)
    }

    fn file(&self, provenance: FactId) -> Result<FileRecord, AdapterError> {
        parser_file(self.request, self.full_source, provenance)
    }

    fn entity_drafts(
        &self,
        cancellation: &Cancellation,
        total_string_bytes: &mut usize,
    ) -> Result<EntityPlan, AdapterError> {
        let mut ordered_facts: Vec<_> = self.parse_output.facts().iter().collect();
        ordered_facts.sort_by(|left, right| structural_syntax_fact_order(left, right));
        let yaml_names = if self.request.language().as_str() == "yaml" {
            yaml::names::Names::new(
                self.parse_output.facts(),
                self.source_text,
                self.request.limits().ir().max_string_bytes,
                self.parse_output.report().coverage().status() == CoverageStatus::Complete,
                cancellation,
            )?
        } else {
            yaml::names::Names::default()
        };
        let rust_test_declarations = rust_test_declarations(&ordered_facts);
        let file_is_test = test_source_path(self.request.source().path().as_str());
        let mut nearest_declaration = HashMap::new();
        let mut captures = HashMap::<u64, AssociatedCaptures>::new();
        let mut scope_identity_captures = HashMap::<u64, ScopeIdentityCaptures>::new();
        for (index, fact) in ordered_facts.iter().enumerate() {
            check_periodically(index, cancellation)?;
            let parent_declaration = fact
                .parent()
                .and_then(|parent| nearest_declaration.get(&parent).copied().flatten());
            if structural_entity_kind(fact).is_some() {
                nearest_declaration.insert(fact.local_id(), Some(fact.local_id()));
                captures.entry(fact.local_id()).or_default();
            } else {
                nearest_declaration.insert(fact.local_id(), parent_declaration);
                if let Some(owner) = yaml_names
                    .owners
                    .get(&fact.local_id())
                    .copied()
                    .or(parent_declaration)
                {
                    if is_definition_capture(fact) {
                        captures.entry(owner).or_default().definitions.push(*fact);
                    } else if is_signature_capture(fact) {
                        captures.entry(owner).or_default().signatures.push(*fact);
                    }
                }
            }
            if let Some(parent) = fact.parent() {
                let identity = scope_identity_captures.entry(parent).or_default();
                match fact.syntax_kind().as_str() {
                    "rust.impl_trait.scope_trait" => identity.insert_trait(fact),
                    "rust.impl_type.scope_type" => identity.insert_type(fact),
                    "swift.extension_target.scope_trait" => identity.insert_trait(fact),
                    "swift.extension_header.scope_type" => identity.insert_type(fact),
                    "css.context_header.scope_type" => identity.insert_type(fact),
                    _ => {}
                }
            }
        }
        let mut drafts = HashMap::<u64, EntityDraft>::new();
        let mut nearest_entity_ancestor = HashMap::new();
        let mut nearest_scope_ancestor = HashMap::<u64, Option<ScopeContext>>::new();
        let mut unsupported_scope_entities = BTreeSet::new();
        let mut json_positions = HashMap::<Option<u64>, u64>::new();
        let mut json_members = HashMap::<(Option<u64>, String), u64>::new();
        let mut markup_members = HashMap::<(Option<u64>, EntityKind, String), u64>::new();
        let mut sql_declarations = HashMap::<(Option<u64>, String, String), u64>::new();
        let mut written_declarations = HashMap::<(Option<u64>, String, String), u64>::new();
        let mut written_scopes = HashMap::<Option<u64>, u64>::new();
        let mut anonymous_scopes = HashMap::<Option<u64>, u64>::new();
        for (index, fact) in ordered_facts.into_iter().enumerate() {
            check_periodically(index, cancellation)?;
            let mut parent_entity = fact.parent().and_then(|parent| {
                drafts
                    .contains_key(&parent)
                    .then_some(parent)
                    .or_else(|| nearest_entity_ancestor.get(&parent).copied().flatten())
            });
            let parent_scope = fact
                .parent()
                .and_then(|parent| nearest_scope_ancestor.get(&parent).cloned().flatten());
            if parent_scope.as_ref().and_then(|scope| scope.kind)
                == Some(StableScopeKind::SwiftExtension)
                && parent_entity
                    .and_then(|parent| drafts.get(&parent))
                    .is_some_and(|parent| parent.kind == EntityKind::Module)
            {
                // An extension names a target type without defining it. Keep its
                // members file-contained with the reviewed target prefix; a file
                // module must not override that prefix or invent type ownership.
                parent_entity = None;
            }
            if fact.kind() == SyntaxFactKind::Scope {
                let json_position = if matches!(
                    fact.syntax_kind().as_str(),
                    "json.array_element.scope" | "json.document_value.scope"
                ) {
                    let next = json_positions.entry(fact.parent()).or_default();
                    let position = *next;
                    *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    Some(position)
                } else {
                    None
                };
                let stable_header =
                    self.stable_scope_header(fact, scope_identity_captures.get(&fact.local_id()))?;
                let unsupported_semantic_identity = parent_scope
                    .as_ref()
                    .is_some_and(|scope| scope.unsupported_semantic_identity)
                    || matches!(
                        fact.syntax_kind().as_str(),
                        "rust.impl.scope"
                            | "swift.extension.scope"
                            | "css.media.scope"
                            | "css.supports.scope"
                            | "css.scope.scope"
                            | "css.at_rule.scope"
                            | "css.keyframe_step.scope"
                    ) && stable_header.is_none();
                // Lua lexical boundaries distinguish nested bindings without offsets or
                // body text. Identical sibling scopes remain guarded as ambiguous below.
                let scope_digest =
                    stable_header
                        .as_ref()
                        .map(|header| header.digest)
                        .or_else(|| {
                            (self.request.language().as_str() == "lua"
                                && matches!(
                                    fact.syntax_kind().as_str(),
                                    "lua.file.scope"
                                        | "lua.block.scope"
                                        | "lua.function.scope"
                                        | "lua.for.scope"
                                        | "lua.repeat.scope"
                                ))
                            .then(|| *blake3::hash(b"rootlight.lua-lexical-scope/1\0").as_bytes())
                        });
                // Lexical bindings must not depend on sibling positions. JSON
                // data is different: array positions are part of its address,
                // while whitespace and value-body edits are not.
                // R and PowerShell record source occurrences, not evaluated environments. Anonymous
                // sibling functions need distinct parameter owners, including when their
                // headers match. Offsets and function bodies must not affect identity.
                let written_scope_identity =
                    if matches!(self.request.language().as_str(), "r" | "powershell") {
                        let next = written_scopes.entry(fact.parent()).or_default();
                        let position = *next;
                        *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                        Some(written_source_identity(
                            self.request.language().as_str(),
                            parent_scope
                                .as_ref()
                                .and_then(|scope| scope.stable_identity),
                            fact.syntax_kind().as_str(),
                            position,
                        ))
                    } else {
                        None
                    };
                // Anonymous source blocks have no declared name. Their lexical
                // position distinguishes disjoint bindings without hashing body
                // contents or byte offsets; this is source identity, not dispatch.
                let anonymous_scope_identity = if matches!(
                    fact.syntax_kind().as_str(),
                    "solidity.block.scope"
                        | "solidity.for.scope"
                        | "scala.block.scope"
                        | "scala.for.scope"
                        | "scala.case.scope"
                        | "scala.lambda.scope"
                        | "scala.extension.scope"
                        | "scala.anonymous_given.scope"
                        | "dart.block.scope"
                        | "dart.for.scope"
                        | "dart.case.scope"
                        | "dart.catch.scope"
                        | "dart.lambda.scope"
                        | "dart.extension.scope"
                ) {
                    let next = anonymous_scopes.entry(fact.parent()).or_default();
                    let position = *next;
                    *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    Some(source_occurrence_identity(
                        if self.request.language().as_str() == "dart" {
                            "rootlight.dart-lexical-scope/1"
                        } else if self.request.language().as_str() == "scala" {
                            "rootlight.scala-lexical-scope/1"
                        } else {
                            "rootlight.solidity-lexical-scope/1"
                        },
                        parent_scope
                            .as_ref()
                            .and_then(|scope| scope.stable_identity),
                        fact.syntax_kind().as_str(),
                        position,
                    ))
                } else {
                    None
                };
                let stable_identity =
                    anonymous_scope_identity
                        .or(written_scope_identity)
                        .or(json_position
                            .map(|position| {
                                json_data_identity(
                                    parent_scope
                                        .as_ref()
                                        .and_then(|scope| scope.stable_identity),
                                    fact.syntax_kind().as_str(),
                                    position,
                                )
                            })
                            .or(scope_digest
                                .map(|digest| {
                                    scope_identity(
                                        parent_scope
                                            .as_ref()
                                            .and_then(|scope| scope.stable_identity),
                                        fact.syntax_kind().as_str(),
                                        digest,
                                    )
                                })
                                .transpose()?
                                .or_else(|| {
                                    parent_scope
                                        .as_ref()
                                        .and_then(|scope| scope.stable_identity)
                                })));
                // CSS groups repeated declarations by raw name and enclosing
                // headers; each declaration retains its source-bound occurrence.
                // JSON data positions already distinguish repeated members.
                // Lexical bindings in other languages need a span-local ambiguity
                // guard without making position part of their durable SymbolId.
                let collision_guard = if fact.syntax_kind().as_str().starts_with("css.")
                    || self.request.language().as_str() == "json"
                {
                    None
                } else {
                    Some(scope_collision_guard(
                        parent_scope
                            .as_ref()
                            .and_then(|scope| scope.collision_guard),
                        fact.syntax_kind().as_str(),
                        fact.span(),
                    )?)
                };
                let context = ScopeContext {
                    stable_identity,
                    collision_guard,
                    qualified_prefix: stable_header
                        .as_ref()
                        .filter(|header| header.kind != StableScopeKind::CssContext)
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
                    unsupported_semantic_identity,
                };
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), Some(context));
                continue;
            }
            let declaration_text = self.text_for_span(fact.span())?;
            let Some(mut kind) = structural_entity_kind_from_source(fact, declaration_text) else {
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            };
            if parent_scope
                .as_ref()
                .is_some_and(|scope| scope.unsupported_semantic_identity)
            {
                unsupported_scope_entities.insert(fact.local_id());
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            }
            if parent_scope.as_ref().and_then(|scope| scope.kind) == Some(StableScopeKind::RustImpl)
                && fact.syntax_kind().as_str() == "rust.function.declaration"
            {
                kind = EntityKind::Method;
            }
            if parent_scope.as_ref().and_then(|scope| scope.kind)
                == Some(StableScopeKind::SwiftExtension)
                && fact.syntax_kind().as_str() == "swift.function.declaration"
                && parent_entity.is_none()
            {
                kind = EntityKind::Method;
            }
            if kind == EntityKind::Function
                && parent_entity
                    .and_then(|parent| drafts.get(&parent))
                    .is_some_and(|parent| {
                        matches!(
                            parent.kind,
                            EntityKind::Class
                                | EntityKind::Struct
                                | EntityKind::Enum
                                | EntityKind::Trait
                                | EntityKind::Interface
                                | EntityKind::Protocol
                        )
                    })
            {
                kind = EntityKind::Method;
            }
            let capture = captures.get(&fact.local_id()).cloned().unwrap_or_default();
            let definition = select_unique_capture(&capture.definitions);
            let (name, definition_local_id) = if let Some(definition) = definition {
                let text = self.text_for_span(definition.span())?;
                let name = if matches!(
                    definition.syntax_kind().as_str(),
                    "yaml.node_key.definition" | "yaml.empty_key.definition"
                ) {
                    yaml_names
                        .keys
                        .get(&definition.local_id())
                        .and_then(Option::as_ref)
                        .map(|key| std::borrow::Cow::Borrowed(key.name.as_str()))
                } else {
                    rootlight_adapter_sdk::structural_captured_name_for_fact(
                        language_for_fact(self.request, definition).as_str(),
                        definition,
                        text,
                        self.request.limits().ir().max_string_bytes,
                    )
                };
                let Some(name) = name else {
                    nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                    nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                    continue;
                };
                (name, Some(definition.local_id()))
            } else if is_explicit_file_module(fact, self.request.language().as_str()) {
                let name = self.request.source().path().as_str();
                (std::borrow::Cow::Borrowed(name), None)
            } else if fact.syntax_kind().as_str() == "r.anonymous_function.declaration" {
                // This label is synthetic, not a written binding. The native
                // callable span and scope identity retain its source ownership.
                (std::borrow::Cow::Borrowed("<anonymous>"), None)
            } else {
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            };
            let signature_capture = select_unique_capture(&capture.signatures);
            // JSON permits duplicate keys. Distinguish their source-order
            // occurrences within this object, without hashing offsets or values.
            let member_scope_identity = if fact.syntax_kind().as_str()
                == "json.property.declaration"
            {
                let next = json_members
                    .entry((fact.parent(), name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(json_data_identity(
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    "json.property",
                    position,
                ))
            } else if self.request.language().as_str() == "sql" && kind != EntityKind::Module {
                // Source DDL is not a final live catalog. Separate repeated
                // declarations and object categories without body/offset hashes.
                let label = fact.syntax_kind().as_str();
                let next = sql_declarations
                    .entry((parent_entity, label.to_owned(), name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                let mut hash = blake3::Hasher::new_derive_key("rootlight.sql-source-declaration/1");
                hash.update(label.as_bytes());
                hash.update(&position.to_be_bytes());
                Some(*hash.finalize().as_bytes())
            } else if matches!(self.request.language().as_str(), "r" | "powershell")
                && kind != EntityKind::Module
            {
                // Reassignment can replace a binding at runtime. Preserve each written
                // declaration and its children without claiming runtime binding identity.
                let label = fact.syntax_kind().as_str();
                let next = written_declarations
                    .entry((fact.parent(), label.to_owned(), name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(written_source_identity(
                    self.request.language().as_str(),
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    label,
                    position,
                ))
            } else if matches!(
                kind,
                EntityKind::MarkupElement | EntityKind::MarkupAttribute
            ) {
                // Repeated source tags and even duplicate attributes are distinct
                // occurrences. Only same-name sibling order affects identity;
                // text bodies, attribute values and byte offsets do not.
                let next = markup_members
                    .entry((parent_entity, kind, name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(blake3::derive_key(
                    "rootlight.html-source-occurrence/1",
                    &position.to_be_bytes(),
                ))
            } else {
                parent_scope
                    .as_ref()
                    .and_then(|scope| scope.stable_identity)
            };
            let (signature, signature_evidence, signature_span) = if supports_signature(kind)
                && let Some(signature) = signature_capture
            {
                let text = self.text_for_span(signature.span())?;
                let maximum = self.request.limits().ir().max_string_bytes;
                let canonical = if kind == EntityKind::Constructor
                    && self.request.language().as_str() == "dart"
                    && let Some(definition) = definition
                {
                    rootlight_adapter_sdk::canonical_dart_constructor_signature(
                        text,
                        signature.span(),
                        definition.span(),
                        &name,
                        maximum,
                    )
                } else {
                    canonical_symbol_signature(text, maximum)
                };
                match canonical {
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
            let qualified_length = if matches!(language.as_str(), "toml" | "yaml") {
                // Data lowering replaces lexical nesting with its bounded address
                // below; charging a filename/lexical prefix here can reject an
                // otherwise representable data path.
                name.len()
            } else {
                match parent_entity.and_then(|parent| drafts.get(&parent)) {
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
                }
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
                    scope_identity: member_scope_identity,
                    data_identity: None,
                    data_qualified_name: None,
                    scope_collision_guard: parent_scope
                        .as_ref()
                        .and_then(|scope| scope.collision_guard),
                    qualified_prefix: qualified_prefix.map(str::to_owned),
                    synthetic: definition_local_id.is_none(),
                    is_test: file_is_test || rust_test_declarations.contains(&fact.local_id()),
                    definition_local_id,
                    span: fact.span(),
                    depth: fact.depth(),
                    kind,
                    name: name.into_owned(),
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
        if self.request.language().as_str() == "toml" {
            toml::resolve(
                self.parse_output.facts(),
                &mut drafts,
                &mut unsupported_scope_entities,
                total_string_bytes,
                self.request.limits().ir(),
                cancellation,
            )?;
        }
        let (duplicate_data_keys, yaml_aliases) = if self.request.language().as_str() == "yaml" {
            let plan = yaml::resolve(
                self.parse_output.facts(),
                yaml_names.bindings,
                &mut drafts,
                &mut unsupported_scope_entities,
                total_string_bytes,
                self.request.limits().ir(),
                cancellation,
            )?;
            (plan.duplicates, plan.aliases)
        } else {
            (BTreeSet::new(), HashMap::new())
        };
        let mut drafts: Vec<_> = drafts.into_values().collect();
        drafts.sort_by(|left, right| {
            (
                left.depth,
                left.span.start_byte(),
                left.span.end_byte(),
                entity_kind_identity_label(left.kind),
                left.name.as_str(),
                left.signature.as_str(),
            )
                .cmp(&(
                    right.depth,
                    right.span.start_byte(),
                    right.span.end_byte(),
                    entity_kind_identity_label(right.kind),
                    right.name.as_str(),
                    right.signature.as_str(),
                ))
        });
        Ok(EntityPlan {
            drafts,
            nearest_entity_ancestor,
            unsupported_scope_entities,
            duplicate_data_keys,
            yaml_aliases,
            yaml_key_sources: yaml_names
                .keys
                .into_iter()
                .filter_map(|(id, key)| key.map(|key| (id, key.source)))
                .collect(),
            yaml_warnings: yaml_names.warnings,
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
        captures: Option<&ScopeIdentityCaptures<'_>>,
    ) -> Result<Option<StableScopeHeader>, AdapterError> {
        if matches!(
            scope.syntax_kind().as_str(),
            "css.media.scope"
                | "css.supports.scope"
                | "css.scope.scope"
                | "css.at_rule.scope"
                | "css.keyframe_step.scope"
        ) {
            let Some(header) = captures
                .filter(|captures| !captures.invalid)
                .and_then(|captures| captures.self_type)
            else {
                return Ok(None);
            };
            let text = self.text_for_span(header.span())?;
            if text.is_empty() || text.len() > self.request.limits().ir().max_string_bytes {
                return Ok(None);
            }
            let mut digest = blake3::Hasher::new();
            digest.update(b"rootlight.css-context-scope/1\0");
            digest.update(text.as_bytes());
            return Ok(Some(StableScopeHeader {
                digest: *digest.finalize().as_bytes(),
                qualified_prefix: String::new(),
                kind: StableScopeKind::CssContext,
            }));
        }
        if !matches!(
            scope.syntax_kind().as_str(),
            "rust.impl.scope" | "swift.extension.scope"
        ) {
            return Ok(None);
        }
        let Some(captures) = captures.filter(|captures| !captures.invalid) else {
            return Ok(None);
        };
        let Some(self_type) = captures.self_type else {
            return Ok(None);
        };
        let self_type = self.text_for_span(self_type.span())?;
        let trait_type = captures
            .trait_type
            .map(|fact| self.text_for_span(fact.span()))
            .transpose()?;
        if scope.syntax_kind().as_str() == "swift.extension.scope" {
            let maximum = self.request.limits().ir().max_string_bytes;
            let Some(target) = trait_type
                .and_then(|text| rootlight_adapter_sdk::structural_captured_name(text, maximum))
            else {
                return Ok(None);
            };
            if self_type.len() > maximum {
                return Ok(None);
            }
            // Conformance and where-clauses distinguish extension scopes; their
            // bounded header is retained while body edits leave identity stable.
            let header = self_type.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut digest = blake3::Hasher::new();
            digest.update(b"rootlight.swift-extension-scope/1\0");
            digest.update(header.as_bytes());
            return Ok(Some(StableScopeHeader {
                digest: *digest.finalize().as_bytes(),
                qualified_prefix: target.to_owned(),
                kind: StableScopeKind::SwiftExtension,
            }));
        }
        let identity = canonical_rust_impl_scope(
            self_type,
            trait_type,
            self.request.limits().ir().max_string_bytes,
        )
        .map_err(|_| provider_failure("treesitter-lowering-scope"))?;
        Ok(identity.map(|identity| StableScopeHeader {
            digest: identity.header(),
            qualified_prefix: identity.display().to_owned(),
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
    unsupported_scope_entities: BTreeSet<u64>,
    duplicate_data_keys: BTreeSet<u64>,
    yaml_aliases: HashMap<u64, u64>,
    yaml_key_sources: HashMap<u64, SourceSpan>,
    yaml_warnings: Vec<(SourceSpan, &'static str)>,
}

#[derive(Clone)]
struct EntityDraft {
    local_id: u64,
    parent_entity: Option<u64>,
    scope_identity: Option<[u8; 32]>,
    data_identity: Option<[u8; 32]>,
    data_qualified_name: Option<String>,
    scope_collision_guard: Option<[u8; 32]>,
    qualified_prefix: Option<String>,
    synthetic: bool,
    is_test: bool,
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
    identity_claim: SymbolIdentityClaim,
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
    unsupported_semantic_identity: bool,
}

#[derive(Default)]
struct ScopeIdentityCaptures<'a> {
    trait_type: Option<&'a SyntaxFact>,
    self_type: Option<&'a SyntaxFact>,
    invalid: bool,
}

impl<'a> ScopeIdentityCaptures<'a> {
    fn insert_trait(&mut self, fact: &'a SyntaxFact) {
        self.invalid |= self.trait_type.replace(fact).is_some();
    }

    fn insert_type(&mut self, fact: &'a SyntaxFact) {
        self.invalid |= self.self_type.replace(fact).is_some();
    }
}

struct StableScopeHeader {
    digest: [u8; 32],
    qualified_prefix: String,
    kind: StableScopeKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StableScopeKind {
    RustImpl,
    SwiftExtension,
    CssContext,
}

fn materialize_entity(
    draft: &EntityDraft,
    full_source: &SourceRef,
    build_context: rootlight_ir::BuildContextIdentity,
    provenance: FactId,
    tier: AnalysisTier,
    maximum_string_bytes: usize,
    materialized: &HashMap<u64, MaterializedEntity>,
) -> Result<MaterializedEntity, AdapterError> {
    let (container, mut container_identity, qualified_name) =
        if let Some(address) = draft.data_identity {
            // Data addresses are independent of whether an implicit parent table
            // has a written header. The Contains edge still uses the explicit owner.
            let file = full_source.span().file();
            let container = draft
                .parent_entity
                .and_then(|parent| materialized.get(&parent))
                .map_or(ContainerRef::File(file), |parent| {
                    ContainerRef::Entity(parent.record.id)
                });
            let mut identity = Vec::with_capacity(1 + file.as_bytes().len() + address.len());
            identity.push(4);
            identity.extend_from_slice(file.as_bytes());
            identity.extend_from_slice(&address);
            (
                container,
                identity,
                draft
                    .data_qualified_name
                    .clone()
                    .ok_or_else(|| provider_failure("treesitter-data-address-missing"))?,
            )
        } else {
            match draft
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
            }
        };
    if let Some(scope_identity) = draft.scope_identity {
        container_identity.push(3);
        container_identity.extend_from_slice(&scope_identity);
    }
    debug_assert_eq!(qualified_name.len(), draft.qualified_length);
    let semantic_kind = entity_kind_identity_label(draft.kind);
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
    let mut flags = BTreeSet::new();
    if draft.synthetic {
        flags.insert(EntityFlag::Synthetic);
    }
    if draft.is_test {
        flags.insert(EntityFlag::Test);
    }
    let record = EntityRecord {
        id,
        repository: full_source.repository(),
        generation: full_source.generation(),
        kind: draft.kind,
        language: draft.language.clone(),
        tier,
        canonical_name: draft.name.clone(),
        display_name: if draft.kind == EntityKind::Property || draft.language == "toml" {
            rootlight_adapter_sdk::structural_display_name_for_language(
                &draft.language,
                &draft.name,
            )
            .into_owned()
        } else {
            draft.name.clone()
        },
        qualified_name,
        container: Some(container),
        visibility: EntityVisibility::Unknown,
        flags: flags.into_iter().collect(),
        provenance,
        evidence: direct_evidence(source),
    };
    let identity_claim = SymbolIdentityClaim {
        symbol: record.id,
        repository: record.repository,
        language: draft.language.clone(),
        kind: draft.kind,
        container: record.container,
        container_identity,
        declared_identity: draft.name.clone(),
        signature_discriminator: draft.signature.as_bytes().to_vec(),
        build_context_discriminator: build_context.digest().as_bytes().to_vec(),
    };
    let entity = MaterializedEntity {
        direct_parent: container,
        identity_claim,
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

fn exclude_ambiguous_entities(
    materialized: &mut HashMap<u64, MaterializedEntity>,
) -> BTreeSet<u64> {
    let mut guards = BTreeMap::<SymbolId, [u8; 32]>::new();
    let mut excluded_symbols = BTreeSet::new();
    for entity in materialized.values() {
        if record_symbol_identity_guard(&mut guards, entity.record.id, entity.identity_guard) {
            excluded_symbols.insert(entity.record.id);
        }
    }

    // Anonymous scopes deliberately omit source positions from durable symbol
    // identity. If two such scopes produce the same semantic identity, omit
    // both ambiguous subtrees and report bounded coverage instead of binding
    // either declaration to the other's stable ID.
    loop {
        let previous_count = excluded_symbols.len();
        for entity in materialized.values() {
            if matches!(
                entity.record.container,
                Some(ContainerRef::Entity(parent)) if excluded_symbols.contains(&parent)
            ) {
                excluded_symbols.insert(entity.record.id);
            }
        }
        if excluded_symbols.len() == previous_count {
            break;
        }
    }

    let mut excluded_local_ids = BTreeSet::new();
    materialized.retain(|local_id, entity| {
        let retained = !excluded_symbols.contains(&entity.record.id);
        if !retained {
            excluded_local_ids.insert(*local_id);
        }
        retained
    });
    excluded_local_ids
}

fn record_symbol_identity_guard(
    guards: &mut BTreeMap<SymbolId, [u8; 32]>,
    symbol: SymbolId,
    guard: [u8; 32],
) -> bool {
    match guards.get(&symbol) {
        Some(existing) => existing != &guard,
        None => {
            guards.insert(symbol, guard);
            false
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

fn source_coverage_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match fact.syntax_kind().as_str() {
        "scala.unbraced_package_unavailable.scope" => Some((
            FactDomain::Relations,
            "scala-nonleading-unbraced-package-scope-unavailable",
        )),
        "powershell.file.module" => Some((
            FactDomain::Relations,
            "powershell-runtime-command-module-and-dispatch-resolution-unavailable",
        )),
        "powershell.script_block.scope" => Some((
            FactDomain::Entities,
            "powershell-runtime-script-block-identity-unavailable",
        )),
        "powershell.hashtable.scope" | "powershell.data.scope" => Some((
            FactDomain::Entities,
            "powershell-data-member-analysis-unavailable",
        )),
        "dart.file.module" => Some((
            FactDomain::Relations,
            "dart-import-inheritance-extension-dispatch-resolution-unavailable",
        )),
        "dart.lambda.scope" => Some((
            FactDomain::Entities,
            "dart-anonymous-runtime-declaration-identity-unavailable",
        )),
        "scala.file.module" => Some((
            FactDomain::Relations,
            "scala-import-inheritance-implicit-dispatch-resolution-unavailable",
        )),
        "scala.anonymous_given.scope" | "scala.lambda.scope" => Some((
            FactDomain::Entities,
            "scala-anonymous-runtime-declaration-identity-unavailable",
        )),
        "solidity.file.module" => Some((
            FactDomain::Relations,
            "solidity-import-inheritance-dispatch-resolution-unavailable",
        )),
        "solidity.assembly.scope" => Some((
            FactDomain::Entities,
            "solidity-inline-assembly-analysis-unavailable",
        )),
        "r.file.root" => Some((
            FactDomain::Entities,
            "r-runtime-generated-definitions-unavailable",
        )),
        "r.file.module" => Some((
            FactDomain::Relations,
            "r-environment-dispatch-resolution-unavailable",
        )),
        "r.nonlocal_function.declaration" | "r.nonlocal_variable.declaration" => Some((
            FactDomain::Relations,
            "r-nonlocal-binding-target-unavailable",
        )),
        "r.namespace_name.reference" | "r.namespace_call.call" => Some((
            FactDomain::Relations,
            "r-package-namespace-target-unavailable",
        )),
        "r.member_name.reference" | "r.member_call.call" => {
            Some((FactDomain::Relations, "r-object-member-target-unavailable"))
        }
        "r.computed_call.call" => {
            Some((FactDomain::Relations, "r-computed-call-target-unavailable"))
        }
        "sql.file.root" => Some((
            FactDomain::Entities,
            "sql-dialect-statement-coverage-incomplete",
        )),
        "sql.file.module" => Some((FactDomain::Relations, "sql-catalog-resolution-unavailable")),
        "sql.body.signature" => Some((
            FactDomain::Entities,
            "sql-function-body-semantics-unavailable",
        )),
        "html.file.module" => Some((FactDomain::Relations, "html-dom-semantics-unavailable")),
        "html.embedded_text.signature" => {
            Some((FactDomain::Entities, "html-embedded-analysis-unavailable"))
        }
        "html.unmatched_end_tag.signature" => {
            Some((FactDomain::Relations, "html-unmatched-end-tag"))
        }
        "html.foreign_context.signature" => {
            Some((FactDomain::Entities, "html-foreign-context-unavailable"))
        }
        "html.scripting_context.signature" => {
            Some((FactDomain::Entities, "html-scripting-mode-unavailable"))
        }
        _ => None,
    }
}

fn json_data_identity(parent: Option<[u8; 32]>, kind: &str, position: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("rootlight.json-data-position/1");
    match parent {
        Some(parent) => {
            hasher.update(&[1]);
            hasher.update(&parent);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&position.to_be_bytes());
    hasher.update(kind.as_bytes());
    *hasher.finalize().as_bytes()
}

fn written_source_identity(
    language: &str,
    parent: Option<[u8; 32]>,
    kind: &str,
    position: u64,
) -> [u8; 32] {
    let context = if language == "r" {
        "rootlight.r-source-occurrence/1"
    } else {
        "rootlight.powershell-source-occurrence/1"
    };
    source_occurrence_identity(context, parent, kind, position)
}

fn source_occurrence_identity(
    context: &'static str,
    parent: Option<[u8; 32]>,
    kind: &str,
    position: u64,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    match parent {
        Some(parent) => {
            hasher.update(&[1]);
            hasher.update(&parent);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&position.to_be_bytes());
    hasher.update(kind.as_bytes());
    *hasher.finalize().as_bytes()
}

fn scope_identity(
    parent: Option<[u8; 32]>,
    syntax_kind: &str,
    header: [u8; 32],
) -> Result<[u8; 32], AdapterError> {
    if matches!(
        syntax_kind,
        "lua.file.scope"
            | "lua.block.scope"
            | "lua.function.scope"
            | "lua.for.scope"
            | "lua.repeat.scope"
            | "swift.extension.scope"
            | "css.media.scope"
            | "css.supports.scope"
            | "css.scope.scope"
            | "css.at_rule.scope"
            | "css.keyframe_step.scope"
    ) {
        let context = if syntax_kind == "swift.extension.scope" {
            "rootlight.swift-extension-scope-identity/1"
        } else if syntax_kind.starts_with("css.") {
            "rootlight.css-context-scope-identity/1"
        } else {
            "rootlight.lua-lexical-scope-identity/1"
        };
        let mut hasher = blake3::Hasher::new_derive_key(context);
        if let Some(parent) = parent {
            hasher.update(&[1]);
            hasher.update(&parent);
        } else {
            hasher.update(&[0]);
        }
        hasher.update(syntax_kind.as_bytes());
        hasher.update(&header);
        return Ok(*hasher.finalize().as_bytes());
    }
    if syntax_kind != "rust.impl.scope" {
        return Err(provider_failure("treesitter-lowering-scope"));
    }
    derive_rust_impl_scope_identity(parent, header)
        .map_err(|_| provider_failure("treesitter-lowering-scope"))
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
    spelling_hash: ContentHash,
) -> Result<OccurrenceRecord, AdapterError> {
    let mut record = OccurrenceRecord {
        id: FactId::from_bytes([0; 20]),
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
        syntactic_text_hash: spelling_hash,
        syntax_kind: fact.syntax_kind().as_str().to_owned(),
        provenance,
        confidence,
        // Keep each full declaration as context, including repeated definitions
        // whose shared entity retains only one representative declaration.
        evidence: entity.record.evidence.clone(),
    };
    record.id = derive_occurrence_record_id(&record)
        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
    Ok(record)
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
    let mut record = OccurrenceRecord {
        id: FactId::from_bytes([0; 20]),
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
    };
    record.id = derive_occurrence_record_id(&record)
        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
    Ok(record)
}

fn structural_resolution_text<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    terminal_call_name: Option<&'a str>,
    maximum_name_bytes: usize,
) -> Option<std::borrow::Cow<'a, str>> {
    if language == "r"
        && matches!(
            fact.syntax_kind().as_str(),
            "r.identifier.reference" | "r.call.call"
        )
    {
        let name = if fact.syntax_kind().as_str() == "r.call.call" {
            terminal_call_name?
        } else {
            text
        };
        return rootlight_adapter_sdk::structural_captured_name_for_language(
            "r",
            name,
            maximum_name_bytes,
        );
    }
    if let Some(terminal_call_name) = terminal_call_name {
        return Some(std::borrow::Cow::Borrowed(terminal_call_name));
    }
    // Structural resolution matches entity-name hashes. The full scoped Rust
    // spelling remains available through the occurrence's source span.
    let text = if language == "rust" && fact.syntax_kind().as_str().ends_with(".scoped_call") {
        text.rsplit("::")
            .next()
            .map(str::trim)
            .filter(|terminal| !terminal.is_empty())
            .unwrap_or(text)
    } else {
        text
    };
    Some(std::borrow::Cow::Borrowed(text))
}

fn terminal_call_names(
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<HashMap<u64, u64>, AdapterError> {
    let mut captures = facts
        .iter()
        .filter(|fact| is_call_capture(fact) || is_call_name_capture(fact))
        .collect::<Vec<_>>();
    captures.sort_unstable_by(|left, right| {
        left.span()
            .start_byte()
            .cmp(&right.span().start_byte())
            .then_with(|| right.span().end_byte().cmp(&left.span().end_byte()))
            .then_with(|| is_call_name_capture(left).cmp(&is_call_name_capture(right)))
            .then_with(|| left.local_id().cmp(&right.local_id()))
    });

    let mut active_calls = Vec::<&SyntaxFact>::new();
    let mut names_by_call = HashMap::<u64, Option<u64>>::new();
    for (index, capture) in captures.into_iter().enumerate() {
        check_periodically(index, cancellation)?;
        while active_calls
            .last()
            .is_some_and(|call| !span_contains(call.span(), capture.span()))
        {
            active_calls.pop();
        }
        if is_call_name_capture(capture) {
            if let Some(call) = active_calls.last() {
                names_by_call
                    .entry(call.local_id())
                    .and_modify(|name| *name = None)
                    .or_insert(Some(capture.local_id()));
            }
        } else {
            active_calls.push(capture);
        }
    }

    Ok(names_by_call
        .into_iter()
        .filter_map(|(call, name)| name.map(|name| (call, name)))
        .collect())
}

fn lexical_reference_relation(
    occurrence: &OccurrenceRecord,
    symbol: SymbolId,
) -> Result<RelationRecord, AdapterError> {
    let mut record = RelationRecord {
        id: FactId::from_bytes([0; 20]),
        repository: occurrence.source.repository(),
        generation: occurrence.source.generation(),
        subject: RelationEndpoint::Occurrence(occurrence.id),
        predicate: RelationPredicate::RefersTo,
        object: RelationEndpoint::Entity(symbol),
        confidence: occurrence.confidence,
        evidence_kind: EvidenceKind::Derived,
        provenance: occurrence.provenance,
        evidence: direct_evidence(occurrence.source.clone()),
    };
    record.id = derive_relation_record_id(&record)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    Ok(record)
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
    let mut record = RelationRecord {
        id: FactId::from_bytes([0; 20]),
        repository: source.repository(),
        generation: source.generation(),
        subject,
        predicate: RelationPredicate::Contains,
        object,
        confidence,
        evidence_kind: EvidenceKind::Syntax,
        provenance,
        evidence: direct_evidence(source.clone()),
    };
    record.id = derive_relation_record_id(&record)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    Ok(record)
}

fn parser_provenance(
    analyzer: &TreeSitterAnalyzer,
    request: &AnalysisRequest<'_>,
    source: &SourceRef,
) -> Result<ProvenanceRecord, AdapterError> {
    let mut record = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository: source.repository(),
        generation: source.generation(),
        producer_kind: ProducerKind::Parser,
        producer: analyzer.descriptor.identity().clone(),
        binary_digest: analyzer.binary_digest,
        frontend_version: Some(analyzer.frontend_version.clone()),
        language: request.language().as_str().to_owned(),
        tier: analyzer.descriptor.tier(),
        build_context: request.build_context(),
        input_sources: vec![source.clone()],
        evidence_sources: vec![source.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    };
    record.id = derive_provenance_record_id(&record)
        .map_err(|_| provider_failure("treesitter-provenance-identity"))?;
    Ok(record)
}

fn parser_file(
    request: &AnalysisRequest<'_>,
    source: &SourceRef,
    provenance: FactId,
) -> Result<FileRecord, AdapterError> {
    let generated = request
        .generated_status()
        .ok_or(RequestError::GeneratedStatusRequired)?;
    Ok(FileRecord {
        id: source.span().file(),
        repository: source.repository(),
        generation: source.generation(),
        path: request.source().path().as_str().to_owned(),
        path_locator: Some(request.source().path().to_locator()),
        content_hash: source.content_hash(),
        byte_length: source.span().end_byte(),
        language: request.language().as_str().to_owned(),
        encoding: request.encoding().as_str().to_owned(),
        generated,
        provenance,
        evidence: direct_evidence(source.clone()),
    })
}

fn diagnostic_record(
    full_source: &SourceRef,
    diagnostic: &AdapterDiagnostic,
    provenance: FactId,
) -> Result<DiagnosticRecord, AdapterError> {
    let source = diagnostic
        .source()
        .map(|diagnostic_source| source_for_span(full_source, diagnostic_source.span()));
    let mut record = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
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
    };
    record.id = derive_diagnostic_record_id(&record)
        .map_err(|_| provider_failure("treesitter-diagnostic-identity"))?;
    Ok(record)
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
    let mut record = SkippedRegion {
        id: FactId::from_bytes([0; 20]),
        repository: full_source.repository(),
        generation: full_source.generation(),
        source: source.clone(),
        domain,
        reason,
        detail: detail.to_owned(),
        provenance,
        evidence: direct_evidence(source),
    };
    record.id = derive_skipped_region_id(&record)
        .map_err(|_| provider_failure("treesitter-skipped-region-identity"))?;
    Ok(record)
}

fn coverage_records(
    source: &SourceRef,
    provenance: FactId,
    tier: AnalysisTier,
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
            let mut record = CoverageRecord {
                id: FactId::from_bytes([0; 20]),
                repository: source.repository(),
                generation: source.generation(),
                scope: CoverageScope::File(source.span().file()),
                domain: domain.domain(),
                tier,
                status: domain.status(),
                discovered,
                indexed,
                skipped,
                provenance,
                evidence: direct_evidence(source.clone()),
            };
            record.id = derive_coverage_record_id(&record)
                .map_err(|_| provider_failure("treesitter-coverage-identity"))?;
            Ok(record)
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

fn rust_test_declarations(facts: &[&SyntaxFact]) -> BTreeSet<u64> {
    let mut source_order = facts.to_vec();
    source_order.sort_unstable_by(|left, right| {
        (
            left.span().start_byte(),
            left.span().end_byte(),
            left.depth(),
            left.local_id(),
        )
            .cmp(&(
                right.span().start_byte(),
                right.span().end_byte(),
                right.depth(),
                right.local_id(),
            ))
    });
    let mut pending_parents = BTreeSet::new();
    let mut tests = BTreeSet::new();
    for fact in source_order {
        if fact.syntax_kind().as_str() == "rust.test_attribute.test_attribute" {
            pending_parents.insert(fact.parent());
            continue;
        }
        if structural_entity_kind(fact).is_some()
            && pending_parents.remove(&fact.parent())
            && fact.syntax_kind().as_str() == "rust.function.declaration"
        {
            tests.insert(fact.local_id());
        }
    }
    tests
}

fn test_source_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let mut components = normalized
        .split('/')
        .filter(|component| !component.is_empty());
    let file_name = components.next_back().unwrap_or_default();
    if components.any(|component| {
        matches!(component, "test" | "tests" | "__tests__" | "spec" | "specs")
            || component.ends_with(".tests")
    }) {
        return true;
    }
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _extension)| stem);
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
        || matches!(stem, "test" | "tests")
}

fn is_explicit_file_module(fact: &SyntaxFact, language: &str) -> bool {
    fact.kind() == SyntaxFactKind::Module
        && matches!(
            fact.syntax_kind().as_str(),
            "python.file.module"
                | "javascript.file.module"
                | "typescript.file.module"
                | "lua.file.module"
                | "ruby.file.module"
                | "swift.file.module"
                | "css.file.module"
                | "bash.file.module"
                | "json.file.module"
                | "toml.file.module"
                | "yaml.file.module"
                | "html.file.module"
                | "sql.file.module"
                | "r.file.module"
                | "solidity.file.module"
                | "scala.file.module"
                | "dart.file.module"
                | "powershell.file.module"
        )
        && matches!(
            language,
            "python"
                | "javascript"
                | "typescript"
                | "lua"
                | "ruby"
                | "swift"
                | "css"
                | "bash"
                | "json"
                | "toml"
                | "yaml"
                | "html"
                | "sql"
                | "r"
                | "solidity"
                | "scala"
                | "dart"
                | "powershell"
        )
}

fn is_definition_capture(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str().ends_with(".definition")
}

fn is_call_capture(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str().ends_with(".call")
}

fn is_call_name_capture(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str().ends_with(".call_name")
}

fn span_contains(container: SourceSpan, child: SourceSpan) -> bool {
    container.file() == child.file()
        && container.start_byte() <= child.start_byte()
        && container.end_byte() >= child.end_byte()
}

fn is_signature_capture(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str().ends_with(".signature")
        && fact.syntax_kind().as_str() != "sql.body.signature"
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
            | EntityKind::Event
            | EntityKind::ErrorDeclaration
            | EntityKind::Modifier
    )
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
        SyntaxFactKind::Occurrence if is_call_name_capture(fact) => None,
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
    current_count: usize,
    total: &mut usize,
    limits: &IrLimits,
) -> Result<(), AdapterError> {
    let observed_count = checked_add(current_count, 1)?;
    require_resource_limit(ResourceKind::Records, observed_count, limits.max_extensions)?;
    require_resource_limit(
        ResourceKind::ExtensionBytes,
        envelope.payload.len(),
        limits.max_extension_payload_bytes,
    )?;
    let observed_bytes = checked_add(*total, envelope.payload.len())?;
    require_resource_limit(
        ResourceKind::ExtensionBytes,
        observed_bytes,
        limits.max_total_extension_bytes,
    )?;
    *total = observed_bytes;
    Ok(())
}

fn insert_optional_extension(
    envelope: ExtensionEnvelope,
    extensions: &mut BTreeMap<FactId, ExtensionEnvelope>,
    total: &mut usize,
    limits: &IrLimits,
) -> Result<bool, AdapterError> {
    if let std::collections::btree_map::Entry::Occupied(mut existing) =
        extensions.entry(envelope.id)
    {
        existing.insert(envelope);
        return Ok(true);
    }
    let fits = envelope.payload.len() <= limits.max_extension_payload_bytes
        && extensions
            .len()
            .checked_add(1)
            .is_some_and(|count| count <= limits.max_extensions)
        && total
            .checked_add(envelope.payload.len())
            .is_some_and(|bytes| bytes <= limits.max_total_extension_bytes);
    if !fits {
        return Ok(false);
    }
    ensure_extension_budget(&envelope, extensions.len(), total, limits)?;
    extensions.insert(envelope.id, envelope);
    Ok(true)
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

fn provider_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("hard-coded lowering diagnostic code is valid"),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, SyntaxKindLabel};
    use rootlight_ids::{FileId, GenerationId, RepositoryId, derive_fact};
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
            ("java.field.declaration", EntityKind::Field),
            ("java.local_variable.declaration", EntityKind::Variable),
            ("java.record.declaration", EntityKind::Struct),
            ("java.annotation.declaration", EntityKind::Interface),
            ("java.annotation_element.declaration", EntityKind::Method),
            ("rust.type.declaration", EntityKind::TypeAlias),
            ("rust.const.declaration", EntityKind::Constant),
            ("rust.static.declaration", EntityKind::Variable),
            ("go.type.declaration", EntityKind::TypeAlias),
            ("go.constant.declaration", EntityKind::Constant),
            ("typescript.interface.declaration", EntityKind::Interface),
            ("typescript.type_alias.declaration", EntityKind::TypeAlias),
            ("typescript.variable.declaration", EntityKind::Variable),
            ("javascript.variable.declaration", EntityKind::Variable),
        ] {
            assert_eq!(structural_entity_kind(&declaration(label)), Some(expected));
        }
        let file = FileId::from_bytes([3; 20]);
        let module = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Module,
            SourceSpan::new(file, 0, 1).expect("test span is ordered"),
            0,
            SyntaxKindLabel::new("java.package.module").expect("test label is valid"),
        );
        assert_eq!(structural_entity_kind(&module), Some(EntityKind::Module));
        let go_type = declaration("go.type.declaration");
        assert_eq!(
            structural_entity_kind_from_source(&go_type, "type Worker struct{}"),
            Some(EntityKind::Struct)
        );
        assert_eq!(
            structural_entity_kind_from_source(&go_type, "type Worker interface{}"),
            Some(EntityKind::Interface)
        );
        assert_eq!(
            structural_entity_kind_from_source(&go_type, "type Worker string"),
            Some(EntityKind::TypeAlias)
        );
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
        assert!(is_explicit_file_module(
            &module("typescript.file.module"),
            "typescript"
        ));
        assert!(is_explicit_file_module(&module("ruby.file.module"), "ruby"));
        assert!(!is_explicit_file_module(&module("python.module"), "python"));
        assert!(!is_explicit_file_module(
            &module("java.file.module"),
            "java"
        ));
    }

    #[test]
    fn signature_discriminator_ignores_formatting_but_keeps_overloads_distinct() {
        let compact =
            canonical_symbol_signature("(x:i32)", 128).expect("compact signature is usable");
        let spaced =
            canonical_symbol_signature("( x : i32 )", 128).expect("spaced signature is usable");
        let overload =
            canonical_symbol_signature("(x:u64)", 128).expect("overload signature is usable");

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
    fn test_source_paths_cover_supported_language_conventions() {
        for path in [
            "packages/runtime-core/__tests__/apiAsyncComponent.spec.ts",
            "tests/test_newton_raphson.py",
            "server/routes_test.go",
            "cli/tests/unit/flags_test.ts",
            "spec/models/user_spec.rb",
            "src/Parser.Tests/parser.cs",
        ] {
            assert!(test_source_path(path), "{path} should be a test source");
        }
        for path in [
            "src/contest_score.go",
            "src/latest_release.ts",
            "src/specification.rs",
            "examples/testdata.py",
        ] {
            assert!(
                !test_source_path(path),
                "{path} should be production source"
            );
        }
    }

    #[test]
    fn symbol_identity_guard_rejects_distinct_inputs_for_one_id() {
        let symbol = SymbolId::from_bytes([7; 20]);
        let mut guards = BTreeMap::new();
        assert!(!record_symbol_identity_guard(&mut guards, symbol, [1; 32]));
        assert!(!record_symbol_identity_guard(&mut guards, symbol, [1; 32]));
        assert!(record_symbol_identity_guard(&mut guards, symbol, [2; 32]));
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
