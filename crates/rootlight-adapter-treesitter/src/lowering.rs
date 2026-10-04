//! Parser-independent syntax-fact lowering into normalized IR.
//!
//! The analyzer injects a `ParseProvider` and never observes native Tree-sitter
//! types, so extraction can evolve independently from stable IR construction.

mod embedded;
mod matlab;
mod nix_paths;
mod perl;
mod toml;
mod yaml;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::Arc,
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisOutput, AnalysisReport,
    AnalysisRequest, CoverageReport, DiagnosticCode, DomainCoverage, IrBatch, IrBatchSink,
    IrRecord, LanguageAnalyzer, MemoryAdmissionPolicy, MemoryEnforcement, ParseOutput,
    ParseProvider, ProducerDescriptor, RequestError, ResourceKind, ResourceUsage, SinkError,
    StreamEnd, StreamUsage, SyntaxFact, SyntaxFactKind,
    derive_structural_occurrence_identity as source_occurrence_identity, execute_analysis,
    execute_parse, structural_entity_classification_gap, structural_entity_kind,
    structural_entity_kind_from_source, structural_syntax_fact_order, structural_type_member_kind,
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
    SkippedRegionReason, SourceRef, SourceSpan, SymbolIdentityClaim, VBA_COMPILATION_NAMESPACE,
    VBA_COMPILATION_VERSION, canonical_rust_impl_scope, canonical_symbol_signature,
    derive_coverage_record_id, derive_diagnostic_record_id, derive_occurrence_record_id,
    derive_provenance_record_id, derive_relation_record_id, derive_rust_impl_scope_identity,
    derive_skipped_region_id, entity_kind_identity_label, new_file_identity_claim_envelope,
    new_lexical_evidence_envelope, new_symbol_identity_claim_envelope,
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
const VBA_CONDITIONAL_EVALUATION_UNAVAILABLE: &str =
    "vba-conditional-compilation-evaluation-unavailable";
const HASKELL_BINDING_UNAVAILABLE: &str = "haskell-binding-target-unavailable";
const HASKELL_ANNOTATION_UNAVAILABLE: &str = "haskell-annotation-owner-unavailable";
type ApplicationProofKey = (u64, u64, SymbolId, u64, u64);

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
    /// Returns [`AdapterError`] for cancellation, accounting overflow, or
    /// bounded allocation failure.
    pub fn required_syntax_fact_count(
        &self,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        required_syntax_fact_count_from_output(&self.parse_output, cancellation)
    }

    /// Returns whether this artifact can be replayed under the supplied limits.
    ///
    /// An exact limit match is always compatible. A changed syntax-record
    /// partition is also compatible when no query or sink clipping occurred,
    /// the retained facts still fit, and every other limit is unchanged.
    /// Context-limited embedded analysis keeps its scoped incomplete coverage;
    /// it is not made complete by replaying the same immutable evidence.
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
    /// Repository, file, content, path, parse context, producer, frontend, and
    /// binary identity must match. The generation may differ; limits must
    /// satisfy [`TreeSitterStructuralArtifact::is_compatible_with_limits`].
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
    let mut declarations = Vec::new();
    declarations
        .try_reserve_exact(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for (index, fact) in facts.iter().enumerate() {
        check_periodically(index, cancellation)?;
        if fact.kind() == SyntaxFactKind::Declaration {
            declarations.push((fact.span().start_byte(), fact.span().end_byte()));
        }
    }
    crate::runtime::sort_cancellable_by(&mut declarations, cancellation, Ord::cmp)?;
    // Required closure uses source containment, not the selected parent edge:
    // a CSS scope and declaration can have identical spans. The suffix minimum
    // matches runtime preflight without depending on which tied parent won.
    let mut minimum_end = u64::MAX;
    for (visit, (_, end)) in declarations.iter_mut().rev().enumerate() {
        check_periodically(visit, cancellation)?;
        minimum_end = minimum_end.min(*end);
        *end = minimum_end;
    }

    let mut required = 0usize;
    for (index, fact) in facts.iter().enumerate() {
        check_periodically(index, cancellation)?;
        let first = declarations.partition_point(|(start, _)| *start < fact.span().start_byte());
        let contains_declaration = declarations
            .get(first)
            .is_some_and(|(_, end)| *end <= fact.span().end_byte());
        let identity_fact = matches!(
            fact.kind(),
            SyntaxFactKind::Root | SyntaxFactKind::Module | SyntaxFactKind::Declaration
        ) || (fact.kind() == SyntaxFactKind::Signature
            // Expressions share the wire fact kind but not the mandatory
            // identity closure; count the retained structural role instead.
            && !fact.syntax_kind().as_str().ends_with(".expression"))
            || (fact.kind() == SyntaxFactKind::Scope
                && (contains_declaration
                    || fact.syntax_kind().as_str().starts_with("json.")
                    || fact.syntax_kind().as_str().starts_with("toml.")
                    || fact.syntax_kind().as_str().starts_with("yaml.")))
            || (fact.kind() == SyntaxFactKind::Occurrence
                && (fact.syntax_kind().as_str().ends_with(".definition")
                    || fact.syntax_kind().as_str().ends_with(".definition_part")));
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
    hasher.update(&[match request.native_vba_module() {
        None => 0,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Procedural) => 1,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object) => 2,
    }]);
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

struct LoweringBudget {
    string_bytes: usize,
    records: usize,
    nested_items: usize,
}

fn preflight_lowering_limits(
    request: &AnalysisRequest<'_>,
    parse_output: &ParseOutput,
    producer: &ProducerIdentity,
    frontend_version: &str,
    profile_fences: &[ProfileFence],
    cancellation: &Cancellation,
) -> Result<LoweringBudget, AdapterError> {
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
    let mut vba_compilation_candidates = 0_usize;
    let mut skipped_candidates = included_range_gap_count(
        request.source().source_ref().span(),
        request.included_ranges(),
    )?;
    let mut string_bytes = 0_usize;
    for _ in classic_dialect_spans(request) {
        cancellation.check()?;
        skipped_candidates = checked_add(skipped_candidates, 1)?;
        account_string(
            &mut string_bytes,
            "visual-basic-dialect-unproven".len(),
            limits,
        )?;
    }

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
        if parse_output.report().coverage().status() == CoverageStatus::Complete
            && fact.syntax_kind().as_str() == "vba.file.scope"
            && fact.span().start_byte() < fact.span().end_byte()
        {
            vba_compilation_candidates = checked_add(vba_compilation_candidates, 1)?;
            account_string(&mut string_bytes, VBA_COMPILATION_NAMESPACE.len(), limits)?;
            account_string(&mut string_bytes, VBA_COMPILATION_VERSION.len(), limits)?;
        }
        if request.language().as_str() == "markdown" && is_classic_fence(fact) {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "visual-basic-dialect-unproven".len(),
                limits,
            )?;
        }
        if let Some((_, detail)) = source_coverage_gap(fact) {
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(&mut string_bytes, detail.len(), limits)?;
        }
        if language_for_fact_with_profile_fences(request, fact, profile_fences) == "yaml"
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
        if language_for_fact_with_profile_fences(request, fact, profile_fences) == "yaml"
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
        if nix_paths::is_segment(fact) {
            // Each native path component may introduce one implicit owner with
            // its own definition, containment edge and identity extension.
            entity_candidates = checked_add(entity_candidates, 1)?;
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                nix_paths::OWNER_UNAVAILABLE.len(),
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
            if supports_signature(
                kind,
                language_for_fact_with_profile_fences(request, fact, profile_fences),
            ) {
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
                structural_entity_classification_gap(fact)
                    .map_or("declaration-name-unavailable".len(), |gap| {
                        gap.detail().len()
                    }),
                limits,
            )?;
        }
        if is_haskell_annotation_name(fact) {
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 2)?;
            for detail in [
                HASKELL_ANNOTATION_UNAVAILABLE,
                "lexical-evidence-resource-limit",
            ] {
                account_string(&mut string_bytes, detail.len(), limits)?;
            }
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
        }
        if is_haskell_signature_member_name(fact) {
            // A missing direct class member can publish a definition as well as
            // the written annotation, even though generic labels infer no callable.
            entity_candidates = checked_add(entity_candidates, 1)?;
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "lexical-evidence-resource-limit".len(),
                limits,
            )?;
            account_string(
                &mut string_bytes,
                "haskell.binding_name.definition".len(),
                limits,
            )?;
        }
        if is_haskell_type_annotation(fact) {
            account_string(
                &mut string_bytes,
                HASKELL_ANNOTATION_UNAVAILABLE.len(),
                limits,
            )?;
        }
        if occurrence_role(fact).is_some() {
            occurrence_candidates = checked_add(occurrence_candidates, 1)?;
            account_string(&mut string_bytes, fact.syntax_kind().as_str().len(), limits)?;
            if language_for_fact_with_profile_fences(request, fact, profile_fences) == "lua"
                && parse_output.report().coverage().status() == CoverageStatus::Complete
                && fact.syntax_kind().as_str() == "lua.identifier.reference"
            {
                lexical_relation_candidates = checked_add(lexical_relation_candidates, 1)?;
            }
            if language_for_fact_with_profile_fences(request, fact, profile_fences) == "groovy"
                && parse_output.report().coverage().status() == CoverageStatus::Complete
                && matches!(
                    fact.syntax_kind().as_str(),
                    "groovy.qualified_type.reference"
                        | "groovy.extends_type.reference"
                        | "groovy.implements_type.reference"
                )
            {
                lexical_relation_candidates = checked_add(lexical_relation_candidates, 1)?;
            }
            if language_for_fact_with_profile_fences(request, fact, profile_fences) == "yaml"
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
            if let Some(detail) = source_reference_gap(fact) {
                if parse_output.report().coverage().status() == CoverageStatus::Complete {
                    lexical_relation_candidates = checked_add(lexical_relation_candidates, 1)?;
                }
                skipped_candidates = checked_add(skipped_candidates, 1)?;
                account_string(&mut string_bytes, detail.len(), limits)?;
            }
            if let Some(detail) = automatic_instantiation_gap(fact) {
                skipped_candidates = checked_add(skipped_candidates, 1)?;
                account_string(&mut string_bytes, detail.len(), limits)?;
            }
            if request.language().as_str() == "markdown"
                && crate::markdown_bindings::is_reference(fact)
            {
                lexical_relation_candidates = checked_add(lexical_relation_candidates, 1)?;
                skipped_candidates = checked_add(skipped_candidates, 1)?;
                account_string(
                    &mut string_bytes,
                    crate::markdown_bindings::TARGET_UNAVAILABLE.len(),
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
        if perl::project_candidate(fact) {
            extension_candidates = checked_add(extension_candidates, 1)?;
            skipped_candidates = checked_add(skipped_candidates, 1)?;
            account_string(
                &mut string_bytes,
                "perl-project-evidence-resource-limit".len(),
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
        if fact.syntax_kind().as_str() == "haskell.field.declaration" {
            // One written field signature may name several independently owned fields.
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
            account_string(&mut string_bytes, import_target_gap(fact).len(), limits)?;
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
    let required_extension_candidates = checked_add(
        checked_add(1, entity_candidates)?,
        vba_compilation_candidates,
    )?;
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
    let nested_items = checked_add(
        checked_add(2, diagnostic_derivations)?,
        vba_compilation_candidates,
    )?;
    require_resource_limit(
        ResourceKind::NestedItems,
        nested_items,
        limits.max_total_nested_items,
    )?;
    Ok(LoweringBudget {
        string_bytes,
        records: total_records,
        nested_items,
    })
}

struct Lowering<'context, 'source> {
    analyzer: &'context TreeSitterAnalyzer,
    request: &'context AnalysisRequest<'source>,
    parse_output: &'context ParseOutput,
    source_text: &'context str,
    full_source: &'context SourceRef,
    classic_fences: Vec<SourceSpan>,
    profile_fences: Vec<ProfileFence>,
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
            classic_fences: Vec::new(),
            profile_fences: Vec::new(),
        })
    }

    fn language_for_fact<'a>(&'a self, fact: &'a SyntaxFact) -> &'a str {
        // The native capture vocabulary describes grammar syntax, not the
        // declared dialect. Fence intervals retain provenance without rescanning bytes.
        if !self.classic_fences.is_empty() && fact.syntax_kind().as_str().starts_with("vba.") {
            let position = self
                .classic_fences
                .partition_point(|span| span.start_byte() <= fact.span().start_byte());
            if let Some(index) = position.checked_sub(1)
                && let Some(span) = self.classic_fences.get(index)
                && fact.span().end_byte() <= span.end_byte()
            {
                return "visual-basic-classic";
            }
        }
        language_for_fact_with_profile_fences(self.request, fact, &self.profile_fences)
    }

    fn lower(mut self, cancellation: &Cancellation) -> Result<LoweredOutput, AdapterError> {
        if self.request.language().as_str() == "markdown" {
            self.profile_fences = profile_fence_spans(self.parse_output.facts(), cancellation)?;
        }
        let mut budget = preflight_lowering_limits(
            self.request,
            self.parse_output,
            self.analyzer.descriptor.identity(),
            &self.analyzer.frontend_version,
            &self.profile_fences,
            cancellation,
        )?;
        let mut total_string_bytes = budget.string_bytes;
        validate_fact_graph(
            self.parse_output.facts(),
            self.request.included_ranges(),
            self.full_source,
            cancellation,
        )?;
        if self.request.language().as_str() == "markdown" {
            self.classic_fences = classic_fence_spans(self.parse_output.facts(), cancellation)?;
        }
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

        let mut awk_functions = HashMap::<String, Option<SymbolId>>::new();
        if self.request.language().as_str() == "awk" {
            let mut definition_counts = HashMap::<String, usize>::new();
            // Entity identity can coalesce same-named declarations, so count
            // authored definition captures before accepting a call target.
            for fact in self
                .parse_output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "awk.function.definition")
            {
                cancellation.check()?;
                let name = self.text_for_span(fact.span())?;
                let count = definition_counts.entry(name.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
            for entity in entities
                .values()
                .filter(|entity| entity.kind == EntityKind::Function)
            {
                if definition_counts.get(&entity.canonical_name) == Some(&1) {
                    awk_functions
                        .entry(entity.canonical_name.clone())
                        .and_modify(|symbol| *symbol = None)
                        .or_insert(Some(entity.id));
                }
            }
        }
        let mut erlang_functions = HashMap::<String, Option<SymbolId>>::new();
        if self.request.language().as_str() == "erlang" {
            let mut definition_counts = HashMap::<String, usize>::new();
            for fact in self
                .parse_output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "erlang.function.definition")
            {
                cancellation.check()?;
                let head = self.text_for_span(fact.span())?;
                if let Some(indicator) = rootlight_adapter_sdk::canonical_erlang_function_head(
                    head,
                    self.request.limits().ir().max_string_bytes,
                ) {
                    let count = definition_counts.entry(indicator).or_default();
                    *count = count.saturating_add(1);
                }
            }
            for entity in entities
                .values()
                .filter(|entity| entity.kind == EntityKind::Function)
            {
                if definition_counts.get(&entity.canonical_name) == Some(&1) {
                    erlang_functions
                        .entry(entity.canonical_name.clone())
                        .and_modify(|symbol| *symbol = None)
                        .or_insert(Some(entity.id));
                }
            }
        }
        let mut make_targets = HashMap::<String, Option<SymbolId>>::new();
        if self.request.language().as_str() == "make" {
            for entity in entities
                .values()
                .filter(|entity| entity.kind == EntityKind::BuildTarget)
            {
                make_targets
                    .entry(entity.canonical_name.clone())
                    .and_modify(|symbol| *symbol = None)
                    .or_insert(Some(entity.id));
            }
        }

        let syntax_confidence = confidence(SYNTAX_CONFIDENCE)?;
        let containment_confidence = confidence(CONTAINMENT_CONFIDENCE)?;
        let mut occurrences = BTreeMap::<FactId, OccurrenceRecord>::new();
        let mut application_proofs = Vec::<ProvenanceRecord>::new();
        let mut application_proof_ids = HashMap::<ApplicationProofKey, FactId>::new();
        let mut relations = BTreeMap::<FactId, RelationRecord>::new();
        let mut extensions = BTreeMap::<FactId, ExtensionEnvelope>::new();
        let mut extension_bytes = 0_usize;
        let mut skipped = BTreeMap::<FactId, SkippedRegion>::new();
        let mut diagnostics = BTreeMap::<FactId, DiagnosticRecord>::new();
        for span in classic_dialect_spans(self.request).chain(self.classic_fences.iter().copied()) {
            cancellation.check()?;
            let region = skipped_region(
                self.full_source,
                span,
                FactDomain::Relations,
                SkippedRegionReason::UnsupportedConstruct,
                "visual-basic-dialect-unproven",
                provenance_id,
            )?;
            skipped.insert(region.id, region);
        }
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
        let lua_bindings = if self.parse_output.report().coverage().status()
            == CoverageStatus::Complete
            && (self.request.language().as_str() == "lua"
                || (self.request.language().as_str() == "markdown"
                    && self
                        .parse_output
                        .facts()
                        .iter()
                        .any(|fact| fact.syntax_kind().as_str() == "lua.identifier.reference")))
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

        let haskell_bindings = if self.request.language().as_str() == "haskell"
            && self.parse_output.report().coverage().status() == CoverageStatus::Complete
        {
            let mut symbols = HashMap::new();
            symbols
                .try_reserve(materialized.len())
                .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
            for entity in materialized.values() {
                cancellation.check()?;
                if entity.record.kind == EntityKind::Parameter
                    && let Some(definition) = entity.definition_local_id
                    && symbols.insert(definition, entity.record.id).is_some()
                {
                    return Err(provider_failure("haskell-formal-identity-conflict"));
                }
            }
            Some(crate::haskell_bindings::HaskellBindings::new(
                self.parse_output.facts(),
                self.request.source().bytes(),
                &symbols,
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?)
        } else {
            None
        };

        let vba_bindings = if self.parse_output.report().coverage().status()
            == CoverageStatus::Complete
            && self
                .parse_output
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "vba.file.scope")
        {
            let symbols = materialized
                .values()
                .filter_map(|entity| entity.definition_local_id.map(|id| (id, entity.record.id)))
                .chain(materialized.iter().filter_map(|(local_id, entity)| {
                    (entity.record.kind == EntityKind::Class
                        && matches!(
                            entity.record.language.as_str(),
                            "vba" | "visual-basic-classic"
                        ))
                    .then_some((*local_id, entity.record.id))
                }))
                .collect();
            Some(crate::vba_bindings::VbaBindings::new(
                self.parse_output.facts(),
                self.request.source().bytes(),
                &symbols,
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?)
        } else {
            None
        };

        if let Some(bindings) = &vba_bindings {
            for (owner, visibility) in bindings.visibilities() {
                cancellation.check()?;
                if let Some(entity) = materialized.get_mut(&owner) {
                    entity.record.visibility = visibility;
                    // Unknown is real missing evidence, not an uninitialized vote.
                    // Seed only from the selected source-witnessed representative.
                    if let Some(record) = entities.get_mut(&entity.record.id)
                        && entity
                            .record
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| record.evidence.source.as_ref() == Some(source))
                    {
                        record.visibility = visibility;
                    }
                }
            }
            // Include declarations without optional visibility facts. A conflict
            // stays Unknown regardless of the materialized hash map's order.
            for entity in materialized.values() {
                cancellation.check()?;
                if let Some(record) = entities.get_mut(&entity.record.id)
                    && record.visibility != entity.record.visibility
                {
                    record.visibility = EntityVisibility::Unknown;
                }
            }
        }

        let groovy_bindings = if self.parse_output.report().coverage().status()
            == CoverageStatus::Complete
            && self
                .parse_output
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "groovy.file.module")
        {
            let symbols = materialized
                .values()
                .filter_map(|entity| {
                    entity
                        .definition_local_id
                        .map(|definition| (definition, entity.record.id))
                })
                .collect();
            Some(crate::groovy_bindings::GroovyBindings::new(
                self.parse_output.facts(),
                self.request.source().bytes(),
                &symbols,
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?)
        } else {
            None
        };

        let groovy_callables = if groovy_bindings.is_some() {
            crate::groovy_callables::collect(self.parse_output.facts(), &facts_by_id, cancellation)?
        } else {
            BTreeMap::new()
        };
        let groovy_members = if let Some(bindings) = &groovy_bindings {
            crate::groovy_members::collect(
                self.parse_output.facts(),
                &facts_by_id,
                bindings,
                cancellation,
            )?
        } else {
            BTreeMap::new()
        };
        let groovy_calls = if let Some(bindings) = &groovy_bindings {
            crate::groovy_calls::collect(
                self.parse_output.facts(),
                &facts_by_id,
                bindings,
                self.request.source().bytes(),
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?
        } else {
            crate::groovy_calls::Plan::default()
        };

        if let Some(bindings) = &groovy_bindings {
            let callable_visibilities = groovy_callables.iter().map(|(owner, callable)| {
                use rootlight_ir::GroovyCallableVisibility;
                let visibility = match callable.visibility {
                    GroovyCallableVisibility::Public => rootlight_ir::EntityVisibility::Public,
                    GroovyCallableVisibility::Protected => {
                        rootlight_ir::EntityVisibility::Restricted
                    }
                    GroovyCallableVisibility::Private => rootlight_ir::EntityVisibility::Private,
                    GroovyCallableVisibility::Unknown => rootlight_ir::EntityVisibility::Unknown,
                };
                (*owner, visibility)
            });
            let member_visibilities = groovy_members
                .iter()
                .map(|(owner, (_, member))| (*owner, member.visibility));
            for (owner, visibility) in bindings
                .type_visibilities()
                .chain(callable_visibilities)
                .chain(member_visibilities)
            {
                cancellation.check()?;
                if let Some(entity) = materialized.get_mut(&owner) {
                    entity.record.visibility = visibility;
                    if let Some(record) = entities.get_mut(&entity.record.id) {
                        record.visibility = visibility;
                    }
                }
            }
        }

        let nix_bindings = if self.parse_output.report().coverage().status()
            == CoverageStatus::Complete
            && self
                .parse_output
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "nix.file.module")
        {
            let symbols = materialized
                .values()
                .filter_map(|entity| {
                    entity
                        .definition_local_id
                        .map(|local| (local, entity.record.id))
                })
                .collect();
            Some(crate::nix_bindings::NixBindings::new(
                self.parse_output.facts(),
                self.request.source().bytes(),
                &symbols,
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?)
        } else {
            None
        };

        let perl_bindings = if self.parse_output.report().coverage().status()
            == CoverageStatus::Complete
            && self
                .parse_output
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "perl.file.module")
        {
            let symbols = materialized
                .values()
                .filter_map(|entity| {
                    entity
                        .definition_local_id
                        .map(|local| (local, entity.record.id))
                })
                .collect();
            Some(crate::perl_bindings::PerlBindings::new(
                self.parse_output.facts(),
                self.request.source().bytes(),
                &symbols,
                &entity_plan
                    .perl
                    .aliases
                    .iter()
                    .filter_map(|(&definition, declaration)| {
                        materialized
                            .get(declaration)
                            .map(|entity| (definition, entity.record.id))
                    })
                    .collect(),
                self.request.limits().ir().max_string_bytes,
                cancellation,
            )?)
        } else {
            None
        };

        let perl_code_values = if let Some(bindings) = &perl_bindings {
            crate::perl_code_values::resolve(
                self.parse_output.facts(),
                self.request.source().bytes(),
                bindings,
                &materialized
                    .values()
                    .filter_map(|entity| {
                        entity.definition_local_id.map(|id| (id, entity.record.id))
                    })
                    .collect(),
                &entity_plan
                    .perl
                    .references
                    .iter()
                    .filter_map(|(&id, target)| {
                        materialized
                            .get(target)
                            .map(|entity| (id, entity.record.id))
                    })
                    .collect(),
                cancellation,
            )?
        } else {
            BTreeMap::new()
        };

        let markdown_bindings = if self.request.language().as_str() == "markdown"
            && self.parse_output.report().coverage().status() == CoverageStatus::Complete
        {
            let symbols = materialized
                .values()
                .filter_map(|entity| {
                    entity
                        .definition_local_id
                        .map(|local| (local, entity.record.id))
                })
                .collect();
            Some(crate::markdown_bindings::MarkdownBindings::new(
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

        if let Some(bindings) = &vba_bindings {
            // An explicit zero-branch inventory distinguishes unguarded source
            // from a missing or truncated plan at downstream resolver boundaries.
            for compilation in bindings.compilations() {
                cancellation.check()?;
                let envelope = rootlight_ir::new_vba_compilation_envelope(
                    self.full_source.repository(),
                    self.full_source.generation(),
                    provenance_id,
                    source_for_span(self.full_source, compilation.scope),
                    compilation.clone(),
                )
                .map_err(|_| provider_failure("vba-compilation-evidence"))?;
                ensure_extension_budget(
                    &envelope,
                    extensions.len(),
                    &mut extension_bytes,
                    self.request.limits().ir(),
                )?;
                extensions.insert(envelope.id, envelope);
            }
        }

        for (index, fact) in self.parse_output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            let source = source_for_span(self.full_source, fact.span());
            if entity_plan
                .haskell_orphan_signatures
                .contains(&fact.local_id())
            {
                let gap = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Entities,
                    SkippedRegionReason::UnsupportedConstruct,
                    HASKELL_ANNOTATION_UNAVAILABLE,
                    provenance_id,
                )?;
                skipped.insert(gap.id, gap);
            }
            if let Some(annotation) = entity_plan.haskell_annotations.get(&fact.local_id()) {
                let Some(definition) = annotation.definition.and_then(|id| facts_by_id.get(&id))
                else {
                    let gap = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Entities,
                        SkippedRegionReason::UnsupportedConstruct,
                        HASKELL_ANNOTATION_UNAVAILABLE,
                        provenance_id,
                    )?;
                    skipped.insert(gap.id, gap);
                    continue;
                };
                let target = annotation.target.and_then(|id| materialized.get(&id));
                let enclosing = annotation
                    .enclosing
                    .and_then(|id| materialized.get(&id))
                    .map(|entity| entity.record.id);
                let mut occurrence = unresolved_occurrence(
                    definition,
                    OccurrenceRole::Declaration,
                    enclosing,
                    provenance_id,
                    syntax_confidence,
                    source_for_span(self.full_source, definition.span()),
                    self.text_for_span(definition.span())?,
                )?;
                if let Some(target) = target {
                    occurrence.target = OccurrenceTarget::Resolved {
                        symbol: target.record.id,
                    };
                } else {
                    let gap = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Entities,
                        SkippedRegionReason::UnsupportedConstruct,
                        HASKELL_ANNOTATION_UNAVAILABLE,
                        provenance_id,
                    )?;
                    skipped.insert(gap.id, gap);
                }
                let signature_source = annotation
                    .signature
                    .map(|span| source_for_span(self.full_source, span));
                if let Some(signature_source) = &signature_source {
                    occurrence.evidence = direct_evidence(signature_source.clone());
                }
                occurrence.id = derive_occurrence_record_id(&occurrence)
                    .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                if let Some(signature_source) = signature_source {
                    let span = signature_source.span();
                    let lexical = LexicalEvidenceV1::from_complete_text(
                        LexicalEvidenceKind::Signature,
                        FactRef::Fact(occurrence.id),
                        LexicalEvidenceFormat::SourceText,
                        self.text_for_span(span)?,
                    )
                    .ok();
                    let envelope = lexical.and_then(|lexical| {
                        new_lexical_evidence_envelope(
                            self.full_source.repository(),
                            self.full_source.generation(),
                            provenance_id,
                            signature_source,
                            &lexical,
                        )
                        .ok()
                    });
                    let retained = match envelope {
                        Some(envelope) => insert_optional_extension(
                            envelope,
                            &mut extensions,
                            &mut extension_bytes,
                            self.request.limits().ir(),
                        )?,
                        None => false,
                    };
                    if !retained {
                        let gap = skipped_region(
                            self.full_source,
                            span,
                            FactDomain::Extensions,
                            SkippedRegionReason::ResourceLimit,
                            "lexical-evidence-resource-limit",
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                } else {
                    let gap = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Extensions,
                        SkippedRegionReason::UnsupportedConstruct,
                        HASKELL_ANNOTATION_UNAVAILABLE,
                        provenance_id,
                    )?;
                    skipped.insert(gap.id, gap);
                }
                occurrences.insert(occurrence.id, occurrence);
                continue;
            }
            if entity_plan.groovy_name_gaps.contains(&fact.local_id()) {
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Entities,
                    SkippedRegionReason::UnsupportedConstruct,
                    if fact.syntax_kind().as_str() == "groovy.qualified_identifier.definition" {
                        "groovy-package-name-unavailable"
                    } else {
                        "groovy-quoted-name-unavailable"
                    },
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            }
            if nix_paths::is_segment(fact)
                && (entity_plan.nix_path_gaps.contains(&fact.local_id())
                    || entity_plan
                        .unsupported_scope_entities
                        .contains(&fact.local_id()))
            {
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    FactDomain::Entities,
                    SkippedRegionReason::UnsupportedConstruct,
                    nix_paths::OWNER_UNAVAILABLE,
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            }
            if let Some((domain, detail)) = source_coverage_gap(fact)
                && !(entity_plan.perl.owned_functions.contains(&fact.local_id())
                    && materialized.contains_key(&fact.local_id()))
                && !(detail == VBA_CONDITIONAL_EVALUATION_UNAVAILABLE
                    && match &vba_bindings {
                        Some(bindings) => {
                            bindings.compilation_evaluation_proven(fact, cancellation)?
                        }
                        None => false,
                    })
            {
                let reason = match fact.syntax_kind().as_str() {
                    "html.embedded_limit.signature"
                    | "astro.embedded_limit.signature"
                    | "markdown.code_limit.signature"
                    | "markdown.inline_limit.signature" => SkippedRegionReason::ResourceLimit,
                    "html.embedded_parse_error.signature"
                    | "astro.embedded_parse_error.signature"
                    | "markdown.code_error.signature"
                    | "markdown.classic_code_error.signature"
                    | "markdown.cobol_fixed_code_error.signature"
                    | "markdown.cobol_free_code_error.signature"
                    | "markdown.inline_error.signature" => SkippedRegionReason::ParseError,
                    _ => SkippedRegionReason::UnsupportedConstruct,
                };
                let region = skipped_region(
                    self.full_source,
                    fact.span(),
                    domain,
                    reason,
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
                    // its definition occurrence retains the original name extent,
                    // including intervening source in a multipart selector.
                    let definition_span = entity_plan
                        .yaml_key_sources
                        .get(&definition_local_id)
                        .copied()
                        .unwrap_or(definition_fact.span());
                    let mut occurrence = declaration_occurrence(
                        definition_fact,
                        entity,
                        provenance_id,
                        syntax_confidence,
                        &source_for_span(self.full_source, definition_span),
                        content_hash(self.text_for_span(definition_span)?.as_bytes()),
                    )?;
                    if let Some(owner) = entity_plan.matlab.writes.get(&fact.local_id()) {
                        occurrence.role = OccurrenceRole::Write;
                        occurrence.enclosing = owner
                            .and_then(|local| materialized.get(&local))
                            .map(|entity| entity.record.id);
                        occurrence.id = derive_occurrence_record_id(&occurrence)
                            .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    }
                    occurrences.insert(occurrence.id, occurrence);
                }
                let relation = containment_relation(
                    entity,
                    provenance_id,
                    containment_confidence,
                    &entity_source,
                )?;
                relations.insert(relation.id, relation);
                if let Some((statement, member)) = groovy_members.get(&fact.local_id()) {
                    let envelope = rootlight_ir::new_groovy_member_envelope(
                        self.full_source.repository(),
                        self.full_source.generation(),
                        provenance_id,
                        source_for_span(self.full_source, *statement),
                        *member,
                    )
                    .map_err(|_| provider_failure("groovy-member-evidence"))?;
                    if !insert_optional_extension(
                        envelope,
                        &mut extensions,
                        &mut extension_bytes,
                        self.request.limits().ir(),
                    )? {
                        let gap = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Extensions,
                            SkippedRegionReason::ResourceLimit,
                            "groovy-member-evidence-resource-limit",
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                } else if groovy_bindings.is_some()
                    && matches!(
                        fact.syntax_kind().as_str(),
                        "groovy.field.declaration" | "groovy.property.declaration"
                    )
                {
                    let gap = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Extensions,
                        SkippedRegionReason::UnsupportedConstruct,
                        "groovy-member-evidence-unavailable",
                        provenance_id,
                    )?;
                    skipped.insert(gap.id, gap);
                }
                if let Some(callable) = groovy_callables.get(&fact.local_id()) {
                    let mut callable = *callable;
                    callable.name_identity = Some(rootlight_ir::GroovyCallableName {
                        spelling_hash: content_hash(self.text_for_span(callable.name)?.as_bytes()),
                        canonical_hash: content_hash(entity.record.canonical_name.as_bytes()),
                    });
                    let envelope = rootlight_ir::new_groovy_callable_envelope(
                        self.full_source.repository(),
                        self.full_source.generation(),
                        provenance_id,
                        entity_source.clone(),
                        callable,
                    )
                    .map_err(|_| provider_failure("groovy-callable-evidence"))?;
                    if !insert_optional_extension(
                        envelope,
                        &mut extensions,
                        &mut extension_bytes,
                        self.request.limits().ir(),
                    )? {
                        let gap = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Extensions,
                            SkippedRegionReason::ResourceLimit,
                            "groovy-callable-evidence-resource-limit",
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                }
                if groovy_bindings.is_some()
                    && fact.syntax_kind().as_str() == "groovy.function.declaration"
                    && !groovy_callables.contains_key(&fact.local_id())
                {
                    let gap = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Extensions,
                        SkippedRegionReason::UnsupportedConstruct,
                        "groovy-callable-evidence-unavailable",
                        provenance_id,
                    )?;
                    skipped.insert(gap.id, gap);
                }
                if let Some(signature) = entity.signature_evidence.as_ref() {
                    let signature_span = entity
                        .signature_span
                        .ok_or_else(|| provider_failure("treesitter-lowering-signature"))?;
                    let signature_source = source_for_span(self.full_source, signature_span);
                    let lexical = signature.lexical(FactRef::Entity(entity.record.id));
                    if let Some(envelope) = lexical.and_then(|lexical| {
                        new_lexical_evidence_envelope(
                            self.full_source.repository(),
                            self.full_source.generation(),
                            provenance_id,
                            signature_source,
                            &lexical,
                        )
                        .ok()
                    }) {
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
                } else if supports_declaration_signature(
                    entity.record.kind,
                    &entity.record.language,
                    fact,
                    entity
                        .definition_local_id
                        .and_then(|id| facts_by_id.get(&id).copied()),
                ) {
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
                let call_context = if fact.syntax_kind().as_str() == "groovy.implicit_call.call" {
                    groovy_calls.calls.get(&fact.span()).cloned().map(|lookup| {
                        rootlight_ir::new_groovy_call_lookup_envelope(
                            self.full_source.repository(),
                            self.full_source.generation(),
                            provenance_id,
                            source.clone(),
                            lookup,
                        )
                    })
                } else if fact.syntax_kind().as_str() == "groovy.identifier.reference" {
                    groovy_calls
                        .values
                        .get(&fact.span())
                        .cloned()
                        .map(|lookup| {
                            rootlight_ir::new_groovy_value_lookup_envelope(
                                self.full_source.repository(),
                                self.full_source.generation(),
                                provenance_id,
                                source.clone(),
                                lookup,
                            )
                        })
                } else if fact.kind() == SyntaxFactKind::Import {
                    groovy_calls
                        .imports
                        .get(&fact.span())
                        .cloned()
                        .flatten()
                        .map(|import| {
                            rootlight_ir::new_groovy_static_import_envelope(
                                self.full_source.repository(),
                                self.full_source.generation(),
                                provenance_id,
                                source.clone(),
                                import,
                            )
                        })
                } else {
                    None
                };
                if let Some(envelope) = call_context {
                    let retained = match envelope {
                        Ok(envelope) => insert_optional_extension(
                            envelope,
                            &mut extensions,
                            &mut extension_bytes,
                            self.request.limits().ir(),
                        )?,
                        Err(_) => false,
                    };
                    if !retained {
                        let gap = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Extensions,
                            SkippedRegionReason::ResourceLimit,
                            if fact.syntax_kind().as_str() == "groovy.identifier.reference" {
                                "groovy-value-lookup-evidence-resource-limit"
                            } else {
                                "groovy-call-lookup-evidence-resource-limit"
                            },
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                } else if groovy_bindings.is_some()
                    && (matches!(
                        fact.syntax_kind().as_str(),
                        "groovy.implicit_call.call" | "groovy.identifier.reference"
                    ) || (fact.kind() == SyntaxFactKind::Import
                        && groovy_calls.imports.contains_key(&fact.span())))
                {
                    let gap = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Extensions,
                        SkippedRegionReason::UnsupportedConstruct,
                        if fact.syntax_kind().as_str() == "groovy.identifier.reference" {
                            "groovy-value-lookup-evidence-unavailable"
                        } else {
                            "groovy-call-lookup-evidence-unavailable"
                        },
                        provenance_id,
                    )?;
                    skipped.insert(gap.id, gap);
                }
                let role = if entity_plan.perl.calls.contains(&fact.local_id()) {
                    OccurrenceRole::CallSite
                } else {
                    role
                };
                let text = self.text_for_span(fact.span())?;
                let terminal_call_name = terminal_call_names
                    .get(&fact.local_id())
                    .and_then(|local_id| facts_by_id.get(local_id))
                    .map(|terminal| self.text_for_span(terminal.span()))
                    .transpose()?;
                let resolution_text = structural_resolution_text(
                    self.language_for_fact(fact),
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
                    occurrence.syntax_kind = format!(
                        "{}.unavailable_name.{}",
                        self.language_for_fact(fact),
                        if role == OccurrenceRole::CallSite {
                            "call"
                        } else {
                            "reference"
                        }
                    );
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let region = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Occurrences,
                        SkippedRegionReason::UnsupportedConstruct,
                        &format!(
                            "{}-reference-name-unavailable",
                            self.language_for_fact(fact)
                        ),
                        provenance_id,
                    )?;
                    skipped.insert(region.id, region);
                }
                if fact.syntax_kind().as_str() == "make.target.reference"
                    && let Some(Some(symbol)) = make_targets.get(text)
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol: *symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let relation = lexical_reference_relation(&occurrence, *symbol)?;
                    relations.insert(relation.id, relation);
                }
                if fact.syntax_kind().as_str() == "awk.function.call"
                    && let Some(Some(symbol)) = awk_functions.get(text)
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol: *symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, *symbol)?;
                    relation.predicate = RelationPredicate::Calls;
                    relation.id = derive_relation_record_id(&relation)
                        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    relations.insert(relation.id, relation);
                }
                if fact.syntax_kind().as_str() == "erlang.function.call"
                    && let Some(Some(symbol)) = resolution_text
                        .as_deref()
                        .and_then(|indicator| erlang_functions.get(indicator))
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol: *symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, *symbol)?;
                    relation.predicate = RelationPredicate::Calls;
                    relation.id = derive_relation_record_id(&relation)
                        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    relations.insert(relation.id, relation);
                }
                let mut vba_array_invocation_proven = false;
                if let Some(bindings) = &vba_bindings
                    && let Some((binding, application_evidence)) =
                        bindings.resolve_with_application_evidence(fact, text, cancellation)?
                {
                    if fact.syntax_kind().as_str() == "vba.event_identifier.reference" {
                        // A declared event is a static reference target. Its
                        // connected runtime handlers are not implied by that edge.
                        let gap = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Relations,
                            SkippedRegionReason::UnsupportedConstruct,
                            "vba-event-handlers-unavailable",
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                    occurrence.target = OccurrenceTarget::Resolved {
                        symbol: binding.symbol,
                    };
                    if let Some(evidence) = application_evidence {
                        if binding.is_call && evidence.implicit_statement.is_some() {
                            // The full grouped comparison argument and materialized
                            // ordinary formals, not GLR's assignment hint, prove dispatch.
                            occurrence.syntax_kind.clear();
                            occurrence
                                .syntax_kind
                                .push_str("vba.invoked_identifier.reference");
                        }
                        occurrence.provenance = self.application_provenance(
                            fact,
                            binding.symbol,
                            evidence,
                            &entities,
                            &provenance,
                            &mut application_proofs,
                            &mut application_proof_ids,
                            &mut budget,
                            &mut total_string_bytes,
                            cancellation,
                        )?;
                    }
                    if binding.is_call {
                        occurrence.role = OccurrenceRole::CallSite;
                    } else {
                        vba_array_invocation_proven = bindings.array_invocation_target_is_proven(
                            fact,
                            binding.symbol,
                            cancellation,
                        )?;
                    }
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, binding.symbol)?;
                    if binding.is_call {
                        relation.predicate = RelationPredicate::Calls;
                        relation.id = derive_relation_record_id(&relation)
                            .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    } else if occurrence.role == OccurrenceRole::TypeUse {
                        relation.predicate = RelationPredicate::UsesType;
                        relation.id = derive_relation_record_id(&relation)
                            .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    }
                    relations.insert(relation.id, relation);
                }
                if let Some(bindings) = &haskell_bindings
                    && let Some(symbol) = bindings.resolve(fact, text, cancellation)?
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let relation = lexical_reference_relation(&occurrence, symbol)?;
                    relations.insert(relation.id, relation);
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
                if let Some(bindings) = &groovy_bindings
                    && let Some(symbol) = bindings.resolve(fact, text, cancellation)?
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol };
                    if fact.syntax_kind().as_str() == "groovy.qualified_type.reference" {
                        occurrence.role = OccurrenceRole::TypeUse;
                    }
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, symbol)?;
                    if occurrence.role == OccurrenceRole::TypeUse {
                        relation.predicate = RelationPredicate::UsesType;
                    } else if matches!(
                        occurrence.role,
                        OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
                    ) && let Some(owner) = occurrence.enclosing
                    {
                        relation.predicate = if occurrence.role == OccurrenceRole::InheritanceUse {
                            RelationPredicate::Extends
                        } else {
                            RelationPredicate::Implements
                        };
                        relation.subject = RelationEndpoint::Entity(owner);
                        relation.evidence.derivation.push(FactRef::Entity(owner));
                    }
                    if relation.predicate != RelationPredicate::RefersTo {
                        relation.id = derive_relation_record_id(&relation)
                            .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    }
                    relations.insert(relation.id, relation);
                }
                if let Some(bindings) = &groovy_bindings
                    && let Some(binding) = bindings.imported_type(fact, text, cancellation)?
                {
                    let envelope = rootlight_ir::new_groovy_binding_envelope(
                        self.full_source.repository(),
                        self.full_source.generation(),
                        provenance_id,
                        occurrence.source.clone(),
                        binding,
                    )
                    .map_err(|_| provider_failure("groovy-import-use-evidence"))?;
                    if !matches!(
                        occurrence.role,
                        OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
                    ) {
                        occurrence.role = OccurrenceRole::TypeUse;
                    }
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    if !insert_optional_extension(
                        envelope,
                        &mut extensions,
                        &mut extension_bytes,
                        self.request.limits().ir(),
                    )? {
                        let gap = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Extensions,
                            SkippedRegionReason::ResourceLimit,
                            "groovy-import-use-evidence-resource-limit",
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                }
                if let Some(bindings) = &groovy_bindings
                    && (matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
                        || matches!(
                            occurrence.role,
                            OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
                        ))
                    && let Some(lookup) = bindings.type_lookup(fact, text, cancellation)?
                {
                    occurrence.target = OccurrenceTarget::Unresolved {
                        text_hash: occurrence.syntactic_text_hash,
                    };
                    let envelope = rootlight_ir::new_groovy_type_lookup_envelope(
                        self.full_source.repository(),
                        self.full_source.generation(),
                        provenance_id,
                        occurrence.source.clone(),
                        lookup,
                    )
                    .map_err(|_| provider_failure("groovy-type-lookup-evidence"))?;
                    if !matches!(
                        occurrence.role,
                        OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
                    ) {
                        occurrence.role = OccurrenceRole::TypeUse;
                    }
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    if !insert_optional_extension(
                        envelope,
                        &mut extensions,
                        &mut extension_bytes,
                        self.request.limits().ir(),
                    )? {
                        let gap = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Extensions,
                            SkippedRegionReason::ResourceLimit,
                            "groovy-type-lookup-evidence-resource-limit",
                            provenance_id,
                        )?;
                        skipped.insert(gap.id, gap);
                    }
                }
                if let Some(bindings) = &nix_bindings
                    && let Some(symbol) = bindings.resolve(fact, text, cancellation)?
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let relation = lexical_reference_relation(&occurrence, symbol)?;
                    relations.insert(relation.id, relation);
                }
                if let Some(bindings) = &perl_bindings
                    && let Some(symbol) = bindings.resolve(fact, text, cancellation)?
                {
                    occurrence.target = OccurrenceTarget::Resolved { symbol };
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let relation = lexical_reference_relation(&occurrence, symbol)?;
                    relations.insert(relation.id, relation);
                }
                if let Some(target) = entity_plan
                    .perl
                    .references
                    .get(&fact.local_id())
                    .and_then(|local| materialized.get(local))
                {
                    occurrence.target = OccurrenceTarget::Resolved {
                        symbol: target.record.id,
                    };
                    let is_call = entity_plan.perl.calls.contains(&fact.local_id());
                    if is_call {
                        occurrence.role = OccurrenceRole::CallSite;
                    }
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, target.record.id)?;
                    if is_call {
                        relation.predicate = RelationPredicate::Calls;
                        relation.id = derive_relation_record_id(&relation)
                            .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    }
                    relations.insert(relation.id, relation);
                }
                if let Some(&symbol) = perl_code_values.get(&fact.local_id()) {
                    occurrence.target = OccurrenceTarget::Resolved { symbol };
                    occurrence.role = OccurrenceRole::CallSite;
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, symbol)?;
                    relation.predicate = RelationPredicate::Calls;
                    relation.id = derive_relation_record_id(&relation)
                        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    relations.insert(relation.id, relation);
                }
                if let Some(target) = entity_plan
                    .matlab
                    .references
                    .get(&fact.local_id())
                    .and_then(|local| materialized.get(local))
                {
                    occurrence.target = OccurrenceTarget::Resolved {
                        symbol: target.record.id,
                    };
                    let is_call = entity_plan.matlab.calls.contains(&fact.local_id());
                    if is_call {
                        occurrence.role = OccurrenceRole::CallSite;
                    }
                    occurrence.id = derive_occurrence_record_id(&occurrence)
                        .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                    let mut relation = lexical_reference_relation(&occurrence, target.record.id)?;
                    if is_call {
                        relation.predicate = RelationPredicate::Calls;
                        relation.id = derive_relation_record_id(&relation)
                            .map_err(|_| provider_failure("treesitter-relation-identity"))?;
                    }
                    relations.insert(relation.id, relation);
                }
                if let Some(detail) = source_reference_gap(fact)
                    && (matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
                        || (matches!(
                            fact.syntax_kind().as_str(),
                            "vba.invoked_identifier.reference"
                                | "vba.invoked_instance.reference"
                                | "vba.invoked_get_identifier.reference"
                                | "vba.invoked_let_identifier.reference"
                                | "vba.invoked_set_identifier.reference"
                                | "vba.self_invoked_member.reference"
                                | "vba.self_invoked_get_member.reference"
                                | "vba.self_invoked_let_member.reference"
                                | "vba.self_invoked_set_member.reference"
                        ) && occurrence.role != OccurrenceRole::CallSite
                            && !vba_array_invocation_proven))
                {
                    let region = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Relations,
                        SkippedRegionReason::UnsupportedConstruct,
                        detail,
                        provenance_id,
                    )?;
                    skipped.insert(region.id, region);
                }
                if let Some(detail) = automatic_instantiation_gap(fact) {
                    let region = skipped_region(
                        self.full_source,
                        fact.span(),
                        FactDomain::Relations,
                        SkippedRegionReason::UnsupportedConstruct,
                        detail,
                        provenance_id,
                    )?;
                    skipped.insert(region.id, region);
                }
                if self.language_for_fact(fact) == "yaml"
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
                if self.request.language().as_str() == "markdown"
                    && crate::markdown_bindings::is_reference(fact)
                {
                    let target = markdown_bindings
                        .as_ref()
                        .map(|bindings| {
                            bindings.resolve(fact, self.request.source().bytes(), cancellation)
                        })
                        .transpose()?
                        .flatten();
                    if let Some(symbol) = target {
                        occurrence.target = OccurrenceTarget::Resolved { symbol };
                        occurrence.id = derive_occurrence_record_id(&occurrence)
                            .map_err(|_| provider_failure("treesitter-occurrence-identity"))?;
                        let relation = lexical_reference_relation(&occurrence, symbol)?;
                        relations.insert(relation.id, relation);
                    } else {
                        let region = skipped_region(
                            self.full_source,
                            fact.span(),
                            FactDomain::Relations,
                            SkippedRegionReason::UnsupportedConstruct,
                            crate::markdown_bindings::TARGET_UNAVAILABLE,
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
                    import_target_gap(fact),
                    provenance_id,
                )?;
                skipped.insert(region.id, region);
            } else if matches!(
                fact.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Module
            ) {
                if let Some(gap) = structural_entity_classification_gap(fact) {
                    let region = skipped_region(
                        self.full_source,
                        gap.span(),
                        gap.domain(),
                        gap.reason(),
                        gap.detail(),
                        provenance_id,
                    )?;
                    skipped.insert(region.id, region);
                    continue;
                }
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

        // Publish a native-context marker only after every binding claim for that
        // root has been admitted. A dropped write must never look like no write.
        let mut project_complete = BTreeMap::new();
        for item in &entity_plan.perl.project {
            cancellation.check()?;
            project_complete.entry(item.module).or_insert(true);
            let binding = match item.binding {
                perl::ProjectBinding::Definition(local, storage) => {
                    rootlight_ir::PerlBinding::Definition {
                        storage,
                        symbol: materialized
                            .get(&local)
                            .ok_or_else(|| provider_failure("perl-project-definition"))?
                            .record
                            .id,
                    }
                }
                perl::ProjectBinding::Claim(rootlight_ir::PerlBinding::ModuleContext) => continue,
                perl::ProjectBinding::Claim(binding) => binding,
            };
            let envelope = rootlight_ir::new_perl_binding_envelope(
                self.full_source.repository(),
                self.full_source.generation(),
                provenance_id,
                source_for_span(self.full_source, item.source),
                rootlight_ir::PerlBindingEvidence::new(item.module, binding),
            )
            .map_err(|_| provider_failure("perl-project-evidence"))?;
            if !insert_optional_extension(
                envelope,
                &mut extensions,
                &mut extension_bytes,
                self.request.limits().ir(),
            )? {
                project_complete.insert(item.module, false);
                let gap = skipped_region(
                    self.full_source,
                    item.source,
                    FactDomain::Extensions,
                    SkippedRegionReason::ResourceLimit,
                    "perl-project-evidence-resource-limit",
                    provenance_id,
                )?;
                skipped.insert(gap.id, gap);
            }
        }
        for (module, complete) in project_complete {
            cancellation.check()?;
            if !complete {
                continue;
            }
            let envelope = rootlight_ir::new_perl_binding_envelope(
                self.full_source.repository(),
                self.full_source.generation(),
                provenance_id,
                source_for_span(self.full_source, module),
                rootlight_ir::PerlBindingEvidence::new(
                    module,
                    rootlight_ir::PerlBinding::ModuleContext,
                ),
            )
            .map_err(|_| provider_failure("perl-project-context"))?;
            if !insert_optional_extension(
                envelope,
                &mut extensions,
                &mut extension_bytes,
                self.request.limits().ir(),
            )? {
                let gap = skipped_region(
                    self.full_source,
                    module,
                    FactDomain::Extensions,
                    SkippedRegionReason::ResourceLimit,
                    "perl-project-evidence-resource-limit",
                    provenance_id,
                )?;
                skipped.insert(gap.id, gap);
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
                let region = skipped_region(
                    self.full_source,
                    source.span(),
                    FactDomain::Diagnostics,
                    diagnostic_skip_reason(diagnostic.code().as_str()),
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
        sort_owned_cancellable_by(&mut application_proofs, cancellation, |left, right| {
            left.id.cmp(&right.id)
        })?;
        for adjacent in application_proofs.windows(2) {
            cancellation.check()?;
            if let [left, right] = adjacent
                && left.id == right.id
                && left != right
            {
                return Err(provider_failure("vba-application-proof-collision"));
            }
        }
        application_proofs.dedup_by(|left, right| left.id == right.id);
        let stats = DomainStats::new(
            entities.len(),
            occurrences.len(),
            relations.len(),
            checked_add(1, application_proofs.len())?,
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
        let record_count = [
            2,
            application_proofs.len(),
            entities.len(),
            occurrences.len(),
            relations.len(),
            coverage_records.len(),
            skipped.len(),
            diagnostics.len(),
            extensions.len(),
        ]
        .into_iter()
        .try_fold(0, checked_add)?;
        require_resource_limit(
            ResourceKind::Records,
            record_count,
            self.request.limits().ir().max_total_records,
        )?;
        records
            .try_reserve_exact(record_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        records.push(IrRecord::File(file));
        records.push(IrRecord::Provenance(provenance));
        records.extend(application_proofs.into_iter().map(IrRecord::Provenance));
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

    #[allow(clippy::too_many_arguments)]
    fn application_provenance(
        &self,
        application: &SyntaxFact,
        target: SymbolId,
        evidence: crate::vba_bindings::ProcedureApplicationEvidence<'_>,
        entities: &BTreeMap<SymbolId, EntityRecord>,
        parser: &ProvenanceRecord,
        proofs: &mut Vec<ProvenanceRecord>,
        proof_ids: &mut HashMap<ApplicationProofKey, FactId>,
        budget: &mut LoweringBudget,
        total_string_bytes: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<FactId, AdapterError> {
        const RULE: &str = "vba-procedure-application-v1.source";
        cancellation.check()?;
        let key = (
            application.span().start_byte(),
            application.span().end_byte(),
            target,
            evidence.scope.span().start_byte(),
            evidence.scope.span().end_byte(),
        );
        if let Some(id) = proof_ids.get(&key) {
            return Ok(*id);
        }
        let same_source = |source: &SourceRef| {
            source.repository() == self.full_source.repository()
                && source.generation() == self.full_source.generation()
                && source.content_hash() == self.full_source.content_hash()
                && span_contains(self.full_source.span(), source.span())
        };
        let target_entity = entities
            .get(&target)
            .ok_or_else(|| provider_failure("vba-application-proof-target"))?;
        let target_source = target_entity
            .evidence
            .source
            .as_ref()
            .filter(|source| same_source(source))
            .ok_or_else(|| provider_failure("vba-application-proof-target"))?;
        if !span_contains(self.full_source.span(), application.span())
            || !span_contains(self.full_source.span(), evidence.scope.span())
            || evidence.implicit_statement.is_some_and(|statement| {
                !span_contains(self.full_source.span(), statement.span())
                    || !span_contains(statement.span(), application.span())
            })
            || parser.id != target_entity.provenance
        {
            return Err(provider_failure("vba-application-proof-source"));
        }
        for (declaration, definition, symbol) in evidence.parameters {
            cancellation.check()?;
            let parameter = entities
                .get(symbol)
                .ok_or_else(|| provider_failure("vba-application-proof-parameter"))?;
            let source = parameter
                .evidence
                .source
                .as_ref()
                .ok_or_else(|| provider_failure("vba-application-proof-parameter"))?;
            if parameter.kind != EntityKind::Parameter
                || parameter.container != Some(ContainerRef::Entity(target))
                || parameter.provenance != parser.id
                || !same_source(source)
                || !span_contains(evidence.scope.span(), declaration.span())
                || !span_contains(declaration.span(), definition.span())
                || !span_contains(declaration.span(), source.span())
            {
                return Err(provider_failure("vba-application-proof-parameter"));
            }
        }

        let limits = self.request.limits().ir();
        let scope_source = source_for_span(self.full_source, evidence.scope.span());
        let distinct_target_source = *target_source != scope_source;
        let evidence_count = checked_add(
            checked_add(
                checked_add(2, usize::from(distinct_target_source))?,
                usize::from(evidence.implicit_statement.is_some()),
            )?,
            evidence
                .parameters
                .len()
                .checked_mul(2)
                .ok_or(SinkError::AccountingOverflow)?,
        )?;
        let parent_count = checked_add(2, evidence.parameters.len())?;
        let input_count = checked_add(1, evidence.parameters.len())?;
        let nested_count = checked_add(input_count, checked_add(evidence_count, parent_count)?)?;
        for count in [input_count, evidence_count, parent_count] {
            require_resource_limit(
                ResourceKind::NestedItems,
                count,
                limits.max_nested_items_per_record,
            )?;
        }
        require_resource_limit(
            ResourceKind::Records,
            checked_add(2, proofs.len())?,
            limits.max_provenance_records,
        )?;
        let records = checked_add(budget.records, 1)?;
        let nested_items = checked_add(budget.nested_items, nested_count)?;
        require_resource_limit(ResourceKind::Records, records, limits.max_total_records)?;
        require_resource_limit(
            ResourceKind::NestedItems,
            nested_items,
            limits.max_total_nested_items,
        )?;
        for value in [
            parser.producer.name(),
            parser.producer.version(),
            parser.language.as_str(),
            RULE,
        ] {
            account_string(total_string_bytes, value.len(), limits)?;
        }
        if let Some(version) = &parser.frontend_version {
            account_string(total_string_bytes, version.len(), limits)?;
        }

        // Charge the complete source/parent closure before reserving any per-call
        // vectors: many calls to a large signature must not bypass IR admission.
        cancellation.check()?;
        let mut evidence_sources = Vec::new();
        evidence_sources
            .try_reserve_exact(evidence_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        evidence_sources.push(source_for_span(self.full_source, application.span()));
        if let Some(statement) = evidence.implicit_statement {
            evidence_sources.push(source_for_span(self.full_source, statement.span()));
        }
        evidence_sources.push(scope_source);
        if distinct_target_source {
            evidence_sources.push(target_source.clone());
        }
        let mut derivation_parents = Vec::new();
        derivation_parents
            .try_reserve_exact(parent_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        derivation_parents.extend([FactRef::Fact(parser.id), FactRef::Entity(target)]);
        let mut input_sources = Vec::new();
        input_sources
            .try_reserve_exact(input_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        input_sources.push(self.full_source.clone());
        for (declaration, definition, symbol) in evidence.parameters {
            cancellation.check()?;
            input_sources.push(source_for_span(self.full_source, declaration.span()));
            evidence_sources.push(source_for_span(self.full_source, declaration.span()));
            evidence_sources.push(source_for_span(self.full_source, definition.span()));
            derivation_parents.push(FactRef::Entity(*symbol));
        }
        sort_owned_cancellable_by(&mut input_sources, cancellation, Ord::cmp)?;
        input_sources.dedup();
        sort_owned_cancellable_by(&mut evidence_sources, cancellation, Ord::cmp)?;
        evidence_sources.dedup();
        crate::runtime::sort_cancellable_by(&mut derivation_parents, cancellation, Ord::cmp)?;
        derivation_parents.dedup();
        let mut proof = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository: parser.repository,
            generation: parser.generation,
            producer_kind: parser.producer_kind,
            producer: parser.producer.clone(),
            binary_digest: parser.binary_digest,
            frontend_version: parser.frontend_version.clone(),
            language: parser.language.clone(),
            tier: parser.tier,
            build_context: parser.build_context,
            input_sources,
            evidence_sources,
            derivation_parents,
            rule: Some(RULE.to_owned()),
        };
        proof.id = derive_provenance_record_id(&proof)
            .map_err(|_| provider_failure("treesitter-provenance-identity"))?;
        cancellation.check()?;
        proofs
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        proof_ids
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        let id = proof.id;
        proofs.push(proof);
        proof_ids.insert(key, id);
        budget.records = records;
        budget.nested_items = nested_items;
        Ok(id)
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
        let mut yaml_names = if self.request.language().as_str() == "yaml" {
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
        let mut embedded_yaml = Vec::new();
        if self.request.language().as_str() == "markdown" {
            for facts in embedded::partitions(self.parse_output.facts(), "yaml", cancellation)? {
                let names = yaml::names::Names::new(
                    &facts,
                    self.source_text,
                    self.request.limits().ir().max_string_bytes,
                    self.parse_output.report().coverage().status() == CoverageStatus::Complete,
                    cancellation,
                )?;
                yaml_names.keys.extend(names.keys);
                yaml_names.owners.extend(names.owners);
                yaml_names.warnings.extend(names.warnings);
                embedded_yaml.push((facts, names.bindings));
            }
        }
        let rust_test_declarations = rust_test_declarations(&ordered_facts);
        let infer_bindings = rootlight_adapter_sdk::typescript_infer_bindings(
            self.parse_output.facts(),
            cancellation,
        )?;
        let file_is_test = test_source_path(self.request.source().path().as_str());
        let mut nearest_declaration = HashMap::new();
        let mut captures = HashMap::<u64, AssociatedCaptures>::new();
        let mut scope_identity_captures = HashMap::<u64, ScopeIdentityCaptures>::new();
        let mut signature_comments = Vec::new();
        let mut haskell_declarations = HashMap::new();
        let mut haskell_scopes = HashMap::new();
        let mut haskell_field_signatures = Vec::new();
        for (index, fact) in ordered_facts.iter().enumerate() {
            check_periodically(index, cancellation)?;
            if self.language_for_fact(fact) == "haskell" {
                let scope = if fact.kind() == SyntaxFactKind::Scope {
                    Some(fact.local_id())
                } else {
                    fact.parent()
                        .and_then(|parent| haskell_scopes.get(&parent).copied().flatten())
                };
                cancellation.check()?;
                haskell_scopes
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                cancellation.check()?;
                haskell_scopes.insert(fact.local_id(), scope);
            }
            if self.language_for_fact(fact) == "powershell"
                && fact.kind() == SyntaxFactKind::Comment
            {
                signature_comments.push(fact.span());
            }
            let parent_declaration = fact
                .parent()
                .and_then(|parent| nearest_declaration.get(&parent).copied().flatten());
            if structural_entity_kind(fact).is_some()
                || is_haskell_annotation_name(fact)
                || structural_entity_classification_gap(fact).is_some()
            {
                if self.language_for_fact(fact) == "haskell" {
                    cancellation.check()?;
                    haskell_declarations
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    cancellation.check()?;
                    haskell_declarations.insert(fact.local_id(), *fact);
                }
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
                    } else if fact.syntax_kind().as_str() == "objective_c.selector.definition_part"
                    {
                        captures
                            .entry(owner)
                            .or_default()
                            .definition_parts
                            .push(*fact);
                    } else if is_signature_capture(fact)
                        && !is_haskell_type_annotation(fact)
                        && fact.syntax_kind().as_str() != "haskell.simple_equation_match.signature"
                    {
                        if fact.syntax_kind().as_str() == "haskell.field_signature.signature" {
                            cancellation.check()?;
                            haskell_field_signatures
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            cancellation.check()?;
                            haskell_field_signatures.push(*fact);
                        } else if haskell_signature_owner_matches(
                            fact,
                            haskell_declarations.get(&owner).copied(),
                        ) {
                            captures.entry(owner).or_default().signatures.push(*fact);
                        }
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
                    "objective_c.owner.scope_trait" => identity.insert_trait(fact),
                    "objective_c.owner_header.scope_type" => identity.insert_type(fact),
                    "css.context_header.scope_type" => identity.insert_type(fact),
                    _ => {}
                }
            }
        }
        Self::associate_haskell_field_signatures(
            &ordered_facts,
            &nearest_declaration,
            &haskell_scopes,
            haskell_field_signatures,
            &mut captures,
            cancellation,
        )?;
        crate::runtime::sort_cancellable_by(
            &mut signature_comments,
            cancellation,
            |left, right| {
                (left.start_byte(), left.end_byte()).cmp(&(right.start_byte(), right.end_byte()))
            },
        )?;
        signature_comments.dedup();
        let mut drafts = HashMap::<u64, EntityDraft>::new();
        let mut nix_contexts = HashMap::new();
        let mut nearest_entity_ancestor = HashMap::new();
        let mut nearest_scope_ancestor = HashMap::<u64, Option<ScopeContext>>::new();
        let mut unsupported_scope_entities = BTreeSet::new();
        let mut groovy_name_gaps = BTreeSet::new();
        let mut json_positions = HashMap::<Option<u64>, u64>::new();
        let mut json_members = HashMap::<(Option<u64>, String), u64>::new();
        let mut markup_members = HashMap::<(Option<u64>, EntityKind, String), u64>::new();
        let mut sql_declarations = HashMap::<(Option<u64>, String, String), u64>::new();
        let mut ecmascript_declarations = HashMap::<(Option<u64>, String, String), u64>::new();
        let mut written_declarations = HashMap::<(Option<u64>, String, String), u64>::new();
        let mut written_scopes = HashMap::<Option<u64>, u64>::new();
        let mut haskell_clause_positions = HashMap::<HaskellClauseKey, u64>::new();
        let matlab_scope_names = matlab::named_scopes(
            self.parse_output.facts(),
            &captures,
            self.source_text,
            self.request.limits().ir().max_string_bytes,
            cancellation,
        )?;
        let mut matlab_named_scopes = HashMap::<(Option<u64>, &str, &str), u64>::new();
        let mut anonymous_scopes = HashMap::<Option<u64>, u64>::new();
        let mut haskell_annotation_drafts = Vec::new();
        for (index, fact) in ordered_facts.into_iter().enumerate() {
            check_periodically(index, cancellation)?;
            let mut parent_entity = fact.parent().and_then(|parent| {
                drafts
                    .contains_key(&parent)
                    .then_some(parent)
                    .or_else(|| nearest_entity_ancestor.get(&parent).copied().flatten())
            });
            let mut parent_scope = fact
                .parent()
                .and_then(|parent| nearest_scope_ancestor.get(&parent).cloned().flatten());
            if self.language_for_fact(fact) == "haskell"
                && fact.kind() == SyntaxFactKind::Scope
                && parent_scope.is_none()
                && let Some(parent) = fact.parent().and_then(|parent| drafts.get(&parent))
            {
                parent_scope = Some(ScopeContext {
                    stable_identity: parent.scope_identity,
                    collision_guard: parent.scope_collision_guard,
                    ..ScopeContext::default()
                });
            }
            if fact.syntax_kind().as_str() == "vba.compilation_constant.declaration" {
                // Compiler constants belong to their source module even when
                // written inside a procedure or an excluded conditional branch.
                while let Some(parent) = parent_entity.and_then(|id| drafts.get(&id)) {
                    cancellation.check()?;
                    if matches!(parent.kind, EntityKind::Module | EntityKind::Class) {
                        break;
                    }
                    parent_entity = parent.parent_entity;
                }
                parent_scope = None;
            }
            if let Some(binding) = infer_bindings.get(&fact.local_id()) {
                // Nested signatures contribute to the conditional's binding, not
                // an independent parameter in the signature where infer is written.
                parent_entity = nearest_entity_ancestor
                    .get(&binding.scope())
                    .copied()
                    .flatten();
                parent_scope = nearest_scope_ancestor
                    .get(&binding.scope())
                    .cloned()
                    .flatten();
            }
            if matches!(
                parent_scope.as_ref().and_then(|scope| scope.kind),
                Some(StableScopeKind::SwiftExtension | StableScopeKind::ObjectiveCOwner)
            ) && parent_entity
                .and_then(|parent| drafts.get(&parent))
                .is_some_and(|parent| parent.kind == EntityKind::Module)
            {
                // An extension or implementation names a type without defining it. Keep its
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
                            | "objective_c.implementation.scope"
                            | "objective_c.category.scope"
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
                            (self.language_for_fact(fact) == "lua"
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
                // These dynamic languages record source occurrences, not evaluated environments. Anonymous
                // sibling functions need distinct parameter owners, including when their
                // headers match. Offsets and function bodies must not affect identity.
                // Alternate VBA branches need separate source identities without selecting one.
                let written_scope_identity = if matches!(
                    self.language_for_fact(fact),
                    "r" | "powershell" | "nix" | "matlab" | "perl" | "groovy" | "haskell"
                ) || fact.syntax_kind().as_str()
                    == "vba.preprocessor_branch.scope"
                {
                    // Named MATLAB workspaces survive unrelated sibling insertion;
                    // only indistinguishable same-name siblings need an ordinal.
                    let name = matlab_scope_names.get(&fact.span()).copied().flatten();
                    let next = if self.language_for_fact(fact) == "haskell"
                        && let Some(owner) = fact.parent().and_then(|parent| drafts.get(&parent))
                    {
                        // Equations share their callable entity, not their formal
                        // parameter or local-binding environments. Count clauses
                        // under the written binding rather than parser-local IDs.
                        let key = (
                            owner.parent_entity,
                            owner.scope_identity,
                            haskell_owned_text(fact.syntax_kind().as_str(), cancellation)?,
                            haskell_owned_text(&owner.name, cancellation)?,
                        );
                        if !haskell_clause_positions.contains_key(&key) {
                            cancellation.check()?;
                            haskell_clause_positions
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            account_string(
                                total_string_bytes,
                                key.2.len(),
                                self.request.limits().ir(),
                            )?;
                            account_string(
                                total_string_bytes,
                                key.3.len(),
                                self.request.limits().ir(),
                            )?;
                        }
                        haskell_clause_positions.entry(key).or_default()
                    } else if let Some(name) = name {
                        matlab_named_scopes
                            .entry((fact.parent(), fact.syntax_kind().as_str(), name))
                            .or_default()
                    } else {
                        written_scopes.entry(fact.parent()).or_default()
                    };
                    let position = *next;
                    *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    let identity = written_source_identity(
                        self.language_for_fact(fact),
                        parent_scope
                            .as_ref()
                            .and_then(|scope| scope.stable_identity),
                        fact.syntax_kind().as_str(),
                        position,
                    );
                    Some(if let Some(name) = name {
                        let mut hash =
                            blake3::Hasher::new_derive_key("rootlight.matlab-named-workspace/1");
                        hash.update(&identity);
                        hash.update(name.as_bytes());
                        *hash.finalize().as_bytes()
                    } else {
                        identity
                    })
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
                        | "javascript.lambda.scope"
                        | "typescript.lambda.scope"
                        | "typescript.mapped_type.scope"
                        | "typescript.conditional.scope"
                        | "scss.block.scope"
                ) {
                    let next = anonymous_scopes.entry(fact.parent()).or_default();
                    let position = *next;
                    *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    Some(source_occurrence_identity(
                        if self.language_for_fact(fact) == "dart" {
                            "rootlight.dart-lexical-scope/1"
                        } else if self.language_for_fact(fact) == "scala" {
                            "rootlight.scala-lexical-scope/1"
                        } else if matches!(
                            self.language_for_fact(fact),
                            "javascript" | "typescript"
                        ) {
                            "rootlight.ecmascript-lexical-scope/1"
                        } else if self.language_for_fact(fact) == "scss" {
                            "rootlight.scss-lexical-scope/1"
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
                    || self.language_for_fact(fact) == "json"
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
                    lexical_module: stable_header
                        .as_ref()
                        .filter(|header| {
                            matches!(
                                header.kind,
                                StableScopeKind::SwiftExtension | StableScopeKind::ObjectiveCOwner
                            )
                        })
                        .and(parent_entity)
                        .filter(|parent| {
                            drafts.get(parent).is_some_and(|module| {
                                module.kind == EntityKind::Module && module.parent_entity.is_some()
                            })
                        })
                        .or_else(|| parent_scope.as_ref().and_then(|scope| scope.lexical_module)),
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
            if is_haskell_annotation_name(fact) {
                cancellation.check()?;
                haskell_annotation_drafts
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                haskell_annotation_drafts.push(HaskellAnnotationDraft {
                    local_id: fact.local_id(),
                    definition: captures
                        .get(&fact.local_id())
                        .and_then(|capture| select_unique_capture(&capture.definitions))
                        .map(SyntaxFact::local_id),
                    parent_entity,
                    scope_identity: parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    span: fact.span(),
                    direct_class_member: is_haskell_signature_member_name(fact),
                });
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            }
            let declaration_text = self.text_for_span(fact.span())?;
            if structural_entity_classification_gap(fact).is_some() {
                // Unknown owners must not turn nested declarations into apparent
                // file-owned methods or leak their definitions into a named parent.
                if !matches!(
                    fact.syntax_kind().as_str(),
                    "fsharp.unsupported_parameters.declaration"
                        | "fsharp.unsupported_anonymous_record_field.declaration"
                        | "fsharp.unsupported_union_field.declaration"
                ) {
                    parent_scope
                        .get_or_insert_with(ScopeContext::default)
                        .unsupported_semantic_identity = true;
                }
                nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                continue;
            }
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
            if !matches!(
                fact.syntax_kind().as_str(),
                "groovy.anonymous_function.declaration" | "fsharp.function.declaration"
            ) && parent_entity
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
                kind = structural_type_member_kind(fact, kind);
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
                } else if definition.syntax_kind().as_str() == "objective_c.method.definition" {
                    rootlight_adapter_sdk::canonical_objective_c_selector(
                        definition,
                        &capture.definition_parts,
                        self.source_text,
                        self.request.limits().ir().max_string_bytes,
                    )
                    .map(std::borrow::Cow::Owned)
                } else {
                    rootlight_adapter_sdk::structural_captured_name_for_fact(
                        self.language_for_fact(definition),
                        definition,
                        text,
                        self.request.limits().ir().max_string_bytes,
                    )
                };
                let Some(name) = name else {
                    if fact.syntax_kind().as_str() == "cobol.program.declaration"
                        || (matches!(self.language_for_fact(fact), "fsharp" | "fsharp-signature")
                            && fact.syntax_kind().as_str().starts_with("fsharp."))
                    {
                        // Unsupported owner identity cannot turn its children
                        // into independent file-owned declarations.
                        parent_scope
                            .get_or_insert_with(ScopeContext::default)
                            .unsupported_semantic_identity = true;
                    }
                    if matches!(
                        definition.syntax_kind().as_str(),
                        "groovy.quoted_identifier.definition"
                            | "groovy.qualified_identifier.definition"
                    ) {
                        groovy_name_gaps.insert(definition.local_id());
                        parent_scope
                            .get_or_insert_with(ScopeContext::default)
                            .unsupported_semantic_identity = true;
                    }
                    nearest_entity_ancestor.insert(fact.local_id(), parent_entity);
                    nearest_scope_ancestor.insert(fact.local_id(), parent_scope);
                    continue;
                };
                (name, Some(definition.local_id()))
            } else if is_explicit_file_module(fact, self.request.language().as_str()) {
                let name = self.request.source().path().as_str();
                (std::borrow::Cow::Borrowed(name), None)
            } else if matches!(
                fact.syntax_kind().as_str(),
                "r.anonymous_function.declaration"
                    | "powershell.anonymous_function.declaration"
                    | "nix.anonymous_function.declaration"
                    | "groovy.anonymous_function.declaration"
            ) {
                // This label is synthetic, not a written binding. The native
                // callable span and scope identity retain its source ownership.
                (std::borrow::Cow::Borrowed("<anonymous>"), None)
            } else if matches!(
                fact.syntax_kind().as_str(),
                "nix.dynamic_variable.declaration"
                    | "nix.dynamic_function.declaration"
                    | "nix.dynamic_path_variable.declaration"
                    | "nix.dynamic_path_function.declaration"
            ) {
                // A computed key owns its source expression without inventing an evaluated name.
                (std::borrow::Cow::Borrowed("<computed-key>"), None)
            } else if fact.syntax_kind().as_str() == "powershell.hashtable.declaration" {
                // Source containers own entries without asserting a runtime variable binding.
                (std::borrow::Cow::Borrowed("<hashtable>"), None)
            } else if fact.syntax_kind().as_str() == "powershell.dynamic_property.declaration" {
                // Keep nested values under their written entry even when its key is evaluated.
                (std::borrow::Cow::Borrowed("<computed-key>"), None)
            } else if fact.syntax_kind().as_str() == "markdown.section.declaration" {
                // Empty written headings still own a section; this is not a fragment ID.
                (std::borrow::Cow::Borrowed("<untitled>"), None)
            } else if fact.syntax_kind().as_str() == "markdown.code_block.module" {
                // Separate examples own separate declarations, not one shared program.
                (std::borrow::Cow::Borrowed("<code-block>"), None)
            } else {
                if fact.syntax_kind().as_str() == "cobol.program.declaration"
                    || (matches!(self.language_for_fact(fact), "fsharp" | "fsharp-signature")
                        && fact.syntax_kind().as_str().starts_with("fsharp."))
                {
                    parent_scope
                        .get_or_insert_with(ScopeContext::default)
                        .unsupported_semantic_identity = true;
                }
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
            } else if fact.syntax_kind().as_str() == "vba.compilation_constant.declaration" {
                // Keep compiler constants distinct from same-named runtime constants.
                let label = fact.syntax_kind().as_str();
                let position = written_declarations
                    .entry((parent_entity, label.to_owned(), name.to_string()))
                    .or_default();
                let ordinal = *position;
                *position = position
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
                Some(written_source_identity("vba", None, label, ordinal))
            } else if self.language_for_fact(fact) == "sql" && kind != EntityKind::Module {
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
            } else if matches!(self.language_for_fact(fact), "javascript" | "typescript")
                && kind == EntityKind::Variable
            {
                // Written variables can share a name in disjoint unnamed blocks.
                // Count only same-name declarations under their named owner so an
                // unrelated inserted block cannot change an existing symbol ID.
                let label = fact.syntax_kind().as_str();
                let next = ecmascript_declarations
                    .entry((parent_entity, label.to_owned(), name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(source_occurrence_identity(
                    "rootlight.ecmascript-source-declaration/1",
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    label,
                    position,
                ))
            } else if matches!(
                fact.syntax_kind().as_str(),
                "objective_c.forward_class.declaration"
                    | "objective_c.forward_protocol.declaration"
            ) {
                // Forward declarations are written occurrences, not evidence that
                // separate declarations or a later body share resolved identity.
                let label = fact.syntax_kind().as_str();
                let next = written_declarations
                    .entry((fact.parent(), label.to_owned(), name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(source_occurrence_identity(
                    "rootlight.objective-c-forward-declaration/1",
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    label,
                    position,
                ))
            } else if matches!(
                self.language_for_fact(fact),
                "r" | "powershell" | "nix" | "matlab" | "perl" | "groovy" | "tcl"
            ) && kind != EntityKind::Module
            {
                // Separate written occurrences and their children without claiming
                // runtime binding identity or merging disjoint source environments.
                let label = fact.syntax_kind().as_str();
                let next = written_declarations
                    .entry((fact.parent(), label.to_owned(), name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(written_source_identity(
                    self.language_for_fact(fact),
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                    label,
                    position,
                ))
            } else if fact.syntax_kind().as_str() == "markdown.code_block.module"
                || matches!(
                    kind,
                    EntityKind::MarkupElement
                        | EntityKind::MarkupAttribute
                        | EntityKind::DocumentSection
                        | EntityKind::LinkDefinition
                )
            {
                // Repeated markup and document declarations are distinct
                // occurrences. Only same-name sibling order affects identity;
                // text bodies, attribute values and byte offsets do not.
                let next = markup_members
                    .entry((parent_entity, kind, name.to_string()))
                    .or_default();
                let position = *next;
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                Some(blake3::derive_key(
                    if fact.syntax_kind().as_str() == "markdown.code_block.module"
                        || matches!(
                            kind,
                            EntityKind::DocumentSection | EntityKind::LinkDefinition
                        )
                    {
                        "rootlight.document-source-occurrence/1"
                    } else {
                        "rootlight.html-source-occurrence/1"
                    },
                    &position.to_be_bytes(),
                ))
            } else {
                parent_scope
                    .as_ref()
                    .and_then(|scope| scope.stable_identity)
            };
            let (signature, signature_evidence, signature_span) = if supports_declaration_signature(
                kind,
                self.language_for_fact(fact),
                fact,
                definition,
            ) && let Some(signature) =
                signature_capture
            {
                let text = self.text_for_span(signature.span())?;
                let maximum = self.request.limits().ir().max_string_bytes;
                let first_comment = signature_comments.partition_point(|comment| {
                    comment.start_byte() < signature.span().start_byte()
                });
                let comments = &signature_comments[first_comment..];
                let needs_projection = comments
                    .first()
                    .is_some_and(|comment| comment.start_byte() < signature.span().end_byte());
                let projection = if needs_projection {
                    project_commented_signature(text, signature.span(), comments, cancellation)?
                } else {
                    None
                };
                let signature_text = projection
                    .as_ref()
                    .map_or(text, rootlight_ir::SourceTextProjection::text);
                let canonical = if needs_projection && projection.is_none() {
                    None
                } else if kind == EntityKind::Constructor
                    && self.language_for_fact(fact) == "dart"
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
                    canonical_symbol_signature(signature_text, maximum)
                };
                match canonical {
                    Some(canonical) => {
                        if fact.syntax_kind().as_str() == "haskell.field.declaration" {
                            account_string(
                                total_string_bytes,
                                text.len(),
                                self.request.limits().ir(),
                            )?;
                        }
                        (
                            canonical,
                            Some(match projection {
                                Some(projection) => SignatureEvidence::Projected(projection),
                                None => SignatureEvidence::Contiguous(text.to_owned()),
                            }),
                            Some(signature.span()),
                        )
                    }
                    None => (String::new(), None, None),
                }
            } else {
                (String::new(), None, None)
            };
            let language = self.language_for_fact(fact).to_owned();
            let qualified_prefix = parent_scope
                .as_ref()
                .and_then(|scope| scope.qualified_prefix.as_deref());
            let qualified_length =
                if matches!(language.as_str(), "toml" | "yaml") && kind != EntityKind::Module {
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
            if language == "nix" {
                nix_contexts.insert(
                    fact.local_id(),
                    parent_scope
                        .as_ref()
                        .and_then(|scope| scope.stable_identity),
                );
            }
            drafts.insert(
                fact.local_id(),
                EntityDraft {
                    local_id: fact.local_id(),
                    parent_entity,
                    lexical_module: parent_entity
                        .is_none()
                        .then(|| parent_scope.as_ref().and_then(|scope| scope.lexical_module))
                        .flatten(),
                    scope_identity: member_scope_identity,
                    data_identity: None,
                    data_root_entity: None,
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
        if self.request.language().as_str() == "markdown" {
            for facts in embedded::partitions(self.parse_output.facts(), "toml", cancellation)? {
                let base = facts
                    .iter()
                    .find(|fact| fact.parent().is_none())
                    .and_then(|fact| drafts.get(&fact.local_id()))
                    .map_or(0, |draft| draft.depth);
                toml::resolve(
                    &facts,
                    &mut drafts,
                    &mut unsupported_scope_entities,
                    total_string_bytes,
                    self.request.limits().ir(),
                    cancellation,
                )?;
                embedded::restore_host_depths(&facts, &mut drafts, base, cancellation)?;
            }
        }
        let (mut duplicate_data_keys, mut yaml_aliases) =
            if self.request.language().as_str() == "yaml" {
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
        for (facts, bindings) in embedded_yaml {
            let base = facts
                .iter()
                .find(|fact| fact.parent().is_none())
                .and_then(|fact| drafts.get(&fact.local_id()))
                .map_or(0, |draft| draft.depth);
            let plan = yaml::resolve(
                &facts,
                bindings,
                &mut drafts,
                &mut unsupported_scope_entities,
                total_string_bytes,
                self.request.limits().ir(),
                cancellation,
            )?;
            duplicate_data_keys.extend(plan.duplicates);
            yaml_aliases.extend(plan.aliases);
            embedded::restore_host_depths(&facts, &mut drafts, base, cancellation)?;
        }
        let nix_path_gaps = nix_paths::resolve(
            self.parse_output.facts(),
            self.source_text,
            &nix_contexts,
            &mut drafts,
            total_string_bytes,
            self.request.limits().ir(),
            cancellation,
        )?;
        let matlab = matlab::resolve(
            self.parse_output.facts(),
            self.source_text,
            &mut drafts,
            self.parse_output.report().coverage().status() == CoverageStatus::Complete,
            self.request.limits().ir().max_string_bytes,
            &matlab_scope_names,
            cancellation,
        )?;
        let perl = perl::resolve(
            self.parse_output.facts(),
            self.source_text,
            &mut drafts,
            self.parse_output.report().coverage().status() == CoverageStatus::Complete,
            total_string_bytes,
            self.request.limits().ir(),
            cancellation,
        )?;
        let HaskellAnnotationPlan {
            annotations: haskell_annotations,
            orphan_signatures: haskell_orphan_signatures,
            members: haskell_members,
        } = self.haskell_annotations(
            &drafts,
            haskell_annotation_drafts,
            &nearest_entity_ancestor,
            &nearest_scope_ancestor,
            total_string_bytes,
            cancellation,
        )?;
        drafts
            .try_reserve(haskell_members.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for member in haskell_members {
            cancellation.check()?;
            if drafts.insert(member.local_id, member).is_some() {
                return Err(provider_failure("haskell-member-definition-collision"));
            }
        }
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
            haskell_annotations,
            haskell_orphan_signatures,
            perl,
            matlab,
            drafts,
            nix_path_gaps,
            groovy_name_gaps,
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

    fn associate_haskell_field_signatures<'fact>(
        facts: &[&'fact SyntaxFact],
        owners: &HashMap<u64, Option<u64>>,
        scopes: &HashMap<u64, Option<u64>>,
        mut signatures: Vec<&'fact SyntaxFact>,
        captures: &mut HashMap<u64, AssociatedCaptures<'fact>>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        if signatures.is_empty() {
            return Ok(());
        }
        crate::runtime::sort_cancellable_by(&mut signatures, cancellation, |left, right| {
            (left.span().start_byte(), left.span().end_byte())
                .cmp(&(right.span().start_byte(), right.span().end_byte()))
        })?;
        let mut invalid = HashMap::new();
        invalid
            .try_reserve(signatures.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        cancellation.check()?;
        let mut active: Option<&SyntaxFact> = None;
        for signature in &signatures {
            cancellation.check()?;
            if let Some(previous) = active
                && previous.span() != signature.span()
                && previous.span().end_byte() > signature.span().start_byte()
            {
                invalid.insert(previous.local_id(), ());
                invalid.insert(signature.local_id(), ());
            }
            if active
                .is_none_or(|previous| previous.span().end_byte() < signature.span().end_byte())
            {
                active = Some(signature);
            }
        }
        for field in facts {
            cancellation.check()?;
            if field.syntax_kind().as_str() != "haskell.field.declaration" {
                continue;
            }
            let end = signatures.partition_point(|signature| {
                signature.span().start_byte() <= field.span().start_byte()
            });
            let signature = end
                .checked_sub(1)
                .and_then(|index| signatures.get(index))
                .copied();
            let field_owner = field
                .parent()
                .and_then(|parent| owners.get(&parent).copied().flatten());
            let Some(signature) = signature.filter(|signature| {
                !invalid.contains_key(&signature.local_id())
                    && span_contains(signature.span(), field.span())
                    && owners.get(&signature.local_id()).copied().flatten() == field_owner
                    && scopes.get(&signature.local_id()) == scopes.get(&field.local_id())
                    && field_owner.is_some()
            }) else {
                continue;
            };
            let associated = captures.entry(field.local_id()).or_default();
            associated
                .signatures
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            cancellation.check()?;
            associated.signatures.push(signature);
        }
        Ok(())
    }

    fn haskell_annotations(
        &self,
        drafts: &HashMap<u64, EntityDraft>,
        annotations: Vec<HaskellAnnotationDraft>,
        owners: &HashMap<u64, Option<u64>>,
        scopes: &HashMap<u64, Option<ScopeContext>>,
        total_string_bytes: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<HaskellAnnotationPlan, AdapterError> {
        if self.request.language().as_str() != "haskell" {
            return Ok(HaskellAnnotationPlan::default());
        }
        let mut signatures = Vec::new();
        for fact in self.parse_output.facts() {
            cancellation.check()?;
            if is_haskell_type_annotation(fact) {
                signatures
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                signatures.push(fact);
            }
        }
        crate::runtime::sort_cancellable_by(&mut signatures, cancellation, |left, right| {
            (left.span().start_byte(), left.span().end_byte())
                .cmp(&(right.span().start_byte(), right.span().end_byte()))
        })?;
        let mut invalid_signatures = HashMap::new();
        invalid_signatures
            .try_reserve(signatures.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut active: Option<&SyntaxFact> = None;
        for signature in &signatures {
            cancellation.check()?;
            if let Some(previous) = active
                && previous.span() != signature.span()
                && previous.span().end_byte() > signature.span().start_byte()
            {
                invalid_signatures.insert(previous.local_id(), ());
                invalid_signatures.insert(signature.local_id(), ());
            }
            if active
                .is_none_or(|previous| previous.span().end_byte() < signature.span().end_byte())
            {
                active = Some(signature);
            }
        }
        let mut facts = HashMap::new();
        facts
            .try_reserve(self.parse_output.facts().len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in self.parse_output.facts() {
            cancellation.check()?;
            facts.insert(fact.local_id(), fact);
        }
        let mut proposals = Vec::new();
        proposals
            .try_reserve_exact(annotations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut represented_signatures = BTreeSet::new();
        for annotation in annotations {
            cancellation.check()?;
            let definition = annotation.definition.and_then(|id| facts.get(&id)).copied();
            let name = definition
                .map(|fact| {
                    let text = self.text_for_span(fact.span())?;
                    Ok::<_, AdapterError>(
                        rootlight_adapter_sdk::structural_captured_name_for_fact(
                            "haskell",
                            fact,
                            text,
                            self.request.limits().ir().max_string_bytes,
                        )
                        .map(|_| text),
                    )
                })
                .transpose()?
                .flatten();
            let end = signatures
                .partition_point(|fact| fact.span().start_byte() <= annotation.span.start_byte());
            let signature_fact = end
                .checked_sub(1)
                .and_then(|index| signatures.get(index))
                .filter(|fact| !invalid_signatures.contains_key(&fact.local_id()))
                .filter(|fact| span_contains(fact.span(), annotation.span))
                .filter(|fact| {
                    owners.get(&fact.local_id()).copied().flatten() == annotation.parent_entity
                        && scopes
                            .get(&fact.local_id())
                            .and_then(Option::as_ref)
                            .and_then(|scope| scope.stable_identity)
                            == annotation.scope_identity
                });
            let signature = match signature_fact.map(|fact| fact.span()) {
                Some(span)
                    if self.text_for_span(span)?.len()
                        <= self.request.limits().ir().max_string_bytes =>
                {
                    account_string(
                        total_string_bytes,
                        self.text_for_span(span)?.len(),
                        self.request.limits().ir(),
                    )?;
                    if name.is_some()
                        && let Some(fact) = signature_fact
                    {
                        represented_signatures.insert(fact.local_id());
                    }
                    Some(span)
                }
                _ => None,
            };
            proposals.push(HaskellAnnotationProposal {
                definition: definition
                    .filter(|_| name.is_some())
                    .map(SyntaxFact::local_id),
                name,
                signature,
                annotation,
            });
        }
        let mut bindings = HashMap::new();
        cancellation.check()?;
        bindings
            .try_reserve(checked_add(drafts.len(), proposals.len())?)
            .map_err(|_| SinkError::AllocationFailed)?;
        for draft in drafts.values() {
            cancellation.check()?;
            if draft.language != "haskell"
                || !matches!(
                    draft.kind,
                    EntityKind::Function | EntityKind::Method | EntityKind::Variable
                )
            {
                continue;
            }
            let key = (
                draft.parent_entity,
                draft.scope_identity,
                draft.name.as_str(),
            );
            bindings
                .entry(key)
                .and_modify(|target: &mut HaskellBinding| {
                    if let HaskellBinding::Existing(local, kind) = target {
                        if *kind != draft.kind {
                            *target = HaskellBinding::Ambiguous;
                        } else if drafts.get(local).is_some_and(|existing| {
                            (draft.span.start_byte(), draft.span.end_byte())
                                < (existing.span.start_byte(), existing.span.end_byte())
                        }) {
                            *local = draft.local_id;
                        }
                    }
                })
                .or_insert(HaskellBinding::Existing(draft.local_id, draft.kind));
        }
        let complete = self.parse_output.report().coverage().status() == CoverageStatus::Complete;
        for (index, proposal) in proposals.iter().enumerate() {
            cancellation.check()?;
            let annotation = &proposal.annotation;
            if !complete
                || !annotation.direct_class_member
                || proposal.signature.is_none()
                || proposal.definition.is_none()
                || !annotation
                    .parent_entity
                    .and_then(|id| drafts.get(&id))
                    .is_some_and(|owner| {
                        owner.kind == EntityKind::Trait && owner.language == "haskell"
                    })
                || scopes
                    .get(&annotation.local_id)
                    .and_then(Option::as_ref)
                    .is_some_and(|scope| scope.unsupported_semantic_identity)
                || !proposal
                    .definition
                    .and_then(|id| facts.get(&id))
                    .is_some_and(|definition| {
                        definition.syntax_kind().as_str() == "haskell.binding_name.definition"
                            && span_contains(annotation.span, definition.span())
                            && definition.span().start_byte() < definition.span().end_byte()
                    })
            {
                continue;
            }
            let Some(name) = proposal.name else {
                continue;
            };
            // An occupied-but-ambiguous binding key is not an absent declaration.
            // A second signature-only candidate is ambiguous as well.
            bindings
                .entry((annotation.parent_entity, annotation.scope_identity, name))
                .and_modify(|target| {
                    if matches!(target, HaskellBinding::Member(_)) {
                        *target = HaskellBinding::Ambiguous;
                    }
                })
                .or_insert(HaskellBinding::Member(index));
        }
        let mut result = HashMap::new();
        cancellation.check()?;
        result
            .try_reserve(proposals.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut members = Vec::new();
        cancellation.check()?;
        members
            .try_reserve_exact(proposals.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, proposal) in proposals.iter().enumerate() {
            cancellation.check()?;
            let annotation = &proposal.annotation;
            let binding = proposal
                .name
                .and_then(|name| {
                    bindings.get(&(annotation.parent_entity, annotation.scope_identity, name))
                })
                .copied();
            let target = match binding {
                Some(HaskellBinding::Existing(local, _)) => Some(local),
                Some(HaskellBinding::Member(candidate)) => proposals[candidate].definition,
                Some(HaskellBinding::Ambiguous) | None => None,
            }
            .filter(|_| complete && proposal.signature.is_some());
            if matches!(binding, Some(HaskellBinding::Member(candidate)) if candidate == index) {
                let name = proposal
                    .name
                    .ok_or_else(|| provider_failure("haskell-member-name"))?;
                let definition = proposal
                    .definition
                    .ok_or_else(|| provider_failure("haskell-member-definition"))?;
                let signature = proposal
                    .signature
                    .ok_or_else(|| provider_failure("haskell-member-signature"))?;
                let owner = annotation
                    .parent_entity
                    .and_then(|id| drafts.get(&id))
                    .ok_or_else(|| provider_failure("haskell-member-owner"))?;
                let qualified_length =
                    checked_add(checked_add(owner.qualified_length, 2)?, name.len())?;
                require_resource_limit(
                    ResourceKind::StringBytes,
                    qualified_length,
                    self.request.limits().ir().max_string_bytes,
                )?;
                let written = self.text_for_span(signature)?;
                for length in [
                    "haskell".len(),
                    name.len(),
                    name.len(),
                    qualified_length,
                    written.len(),
                ] {
                    account_string(total_string_bytes, length, self.request.limits().ir())?;
                }
                let definition_fact = facts
                    .get(&definition)
                    .ok_or_else(|| provider_failure("haskell-member-definition"))?;
                // Key the staged draft by the associated definition, so normal
                // publication emits its exact witness and containment. The
                // annotation remains a separate declaration occurrence.
                members.push(EntityDraft {
                    local_id: definition,
                    parent_entity: annotation.parent_entity,
                    lexical_module: None,
                    scope_identity: annotation.scope_identity,
                    data_identity: None,
                    data_root_entity: None,
                    data_qualified_name: None,
                    scope_collision_guard: scopes
                        .get(&annotation.local_id)
                        .and_then(Option::as_ref)
                        .and_then(|scope| scope.collision_guard),
                    qualified_prefix: None,
                    synthetic: false,
                    is_test: owner.is_test,
                    definition_local_id: Some(definition),
                    span: signature,
                    depth: definition_fact.depth(),
                    kind: EntityKind::Method,
                    name: haskell_owned_text(name, cancellation)?,
                    signature: String::new(),
                    signature_evidence: Some(SignatureEvidence::Contiguous(haskell_owned_text(
                        written,
                        cancellation,
                    )?)),
                    signature_span: Some(signature),
                    language: haskell_owned_text("haskell", cancellation)?,
                    qualified_length,
                });
            }
            result.insert(
                annotation.local_id,
                HaskellAnnotation {
                    definition: proposal.definition,
                    signature: proposal.signature,
                    target,
                    enclosing: annotation.parent_entity,
                },
            );
        }
        let mut orphan_signatures = BTreeSet::new();
        for fact in signatures {
            cancellation.check()?;
            if !represented_signatures.contains(&fact.local_id()) {
                orphan_signatures.insert(fact.local_id());
            }
        }
        Ok(HaskellAnnotationPlan {
            annotations: result,
            orphan_signatures,
            members,
        })
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
            "rust.impl.scope"
                | "swift.extension.scope"
                | "objective_c.implementation.scope"
                | "objective_c.category.scope"
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
        if matches!(
            scope.syntax_kind().as_str(),
            "swift.extension.scope"
                | "objective_c.implementation.scope"
                | "objective_c.category.scope"
        ) {
            let maximum = self.request.limits().ir().max_string_bytes;
            let Some(target) = trait_type
                .and_then(|text| rootlight_adapter_sdk::structural_captured_name(text, maximum))
            else {
                return Ok(None);
            };
            if self_type.len() > maximum {
                return Ok(None);
            }
            // Authored extension/category headers distinguish owners without
            // creating another type entity; body edits leave identity stable.
            let header = self_type.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut digest = blake3::Hasher::new();
            let swift = scope.syntax_kind().as_str() == "swift.extension.scope";
            digest.update(if swift {
                b"rootlight.swift-extension-scope/1\0"
            } else {
                b"rootlight.objective-c-owner-scope/1\0"
            });
            digest.update(header.as_bytes());
            return Ok(Some(StableScopeHeader {
                digest: *digest.finalize().as_bytes(),
                qualified_prefix: target.to_owned(),
                kind: if swift {
                    StableScopeKind::SwiftExtension
                } else {
                    StableScopeKind::ObjectiveCOwner
                },
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
    definition_parts: Vec<&'a SyntaxFact>,
    signatures: Vec<&'a SyntaxFact>,
}

struct EntityPlan {
    haskell_annotations: HashMap<u64, HaskellAnnotation>,
    haskell_orphan_signatures: BTreeSet<u64>,
    perl: perl::Plan,
    matlab: matlab::Plan,
    nix_path_gaps: BTreeSet<u64>,
    groovy_name_gaps: BTreeSet<u64>,
    drafts: Vec<EntityDraft>,
    nearest_entity_ancestor: HashMap<u64, Option<u64>>,
    unsupported_scope_entities: BTreeSet<u64>,
    duplicate_data_keys: BTreeSet<u64>,
    yaml_aliases: HashMap<u64, u64>,
    yaml_key_sources: HashMap<u64, SourceSpan>,
    yaml_warnings: Vec<(SourceSpan, &'static str)>,
}

#[derive(Clone)]
enum SignatureEvidence {
    Contiguous(String),
    Projected(rootlight_ir::SourceTextProjection),
}

impl SignatureEvidence {
    fn lexical(&self, subject: FactRef) -> Option<LexicalEvidenceV1> {
        match self {
            Self::Contiguous(text) => LexicalEvidenceV1::from_complete_text(
                LexicalEvidenceKind::Signature,
                subject,
                LexicalEvidenceFormat::SourceText,
                text,
            )
            .ok(),
            Self::Projected(projection) => Some(LexicalEvidenceV1::from_source_projection(
                subject,
                projection.clone(),
            )),
        }
    }
}

fn project_commented_signature(
    text: &str,
    span: SourceSpan,
    comments: &[SourceSpan],
    cancellation: &Cancellation,
) -> Result<Option<rootlight_ir::SourceTextProjection>, AdapterError> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    // Native comment facts identify trivia; quote contents are never scanned
    // for comment markers. Every retained fragment keeps its original bytes.
    for comment in comments
        .iter()
        .take_while(|comment| comment.start_byte() < span.end_byte())
    {
        cancellation.check()?;
        let Some(start) = comment
            .start_byte()
            .checked_sub(span.start_byte())
            .and_then(|value| usize::try_from(value).ok())
        else {
            return Ok(None);
        };
        let Some(end) = comment
            .end_byte()
            .checked_sub(span.start_byte())
            .and_then(|value| usize::try_from(value).ok())
        else {
            return Ok(None);
        };
        if start < cursor || end > text.len() {
            return Ok(None);
        }
        if !push_signature_fragment(text, cursor..start, &mut ranges) {
            return Ok(None);
        }
        cursor = end;
    }
    if !push_signature_fragment(text, cursor..text.len(), &mut ranges) {
        return Ok(None);
    }
    Ok(rootlight_ir::SourceTextProjection::from_source(text, &ranges).ok())
}

fn push_signature_fragment(
    text: &str,
    range: std::ops::Range<usize>,
    ranges: &mut Vec<std::ops::Range<usize>>,
) -> bool {
    let Some(fragment) = text.get(range.clone()) else {
        return false;
    };
    let trimmed = fragment.trim_ascii();
    if trimmed.is_empty() {
        return true;
    }
    if ranges.len() == rootlight_ir::MAX_SIGNATURE_SOURCE_PARTS {
        return false;
    }
    let start = range.start + fragment.len() - fragment.trim_ascii_start().len();
    ranges.push(start..start + trimmed.len());
    true
}

#[derive(Clone)]
struct EntityDraft {
    local_id: u64,
    parent_entity: Option<u64>,
    lexical_module: Option<u64>,
    scope_identity: Option<[u8; 32]>,
    data_identity: Option<[u8; 32]>,
    data_root_entity: Option<u64>,
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
    signature_evidence: Option<SignatureEvidence>,
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
    signature_evidence: Option<SignatureEvidence>,
    signature_span: Option<SourceSpan>,
}

struct HaskellAnnotationDraft {
    local_id: u64,
    definition: Option<u64>,
    parent_entity: Option<u64>,
    scope_identity: Option<[u8; 32]>,
    span: SourceSpan,
    direct_class_member: bool,
}

struct HaskellAnnotationProposal<'a> {
    annotation: HaskellAnnotationDraft,
    definition: Option<u64>,
    name: Option<&'a str>,
    signature: Option<SourceSpan>,
}

#[derive(Clone, Copy)]
enum HaskellBinding {
    Existing(u64, EntityKind),
    Member(usize),
    Ambiguous,
}

#[derive(Default)]
struct HaskellAnnotationPlan {
    annotations: HashMap<u64, HaskellAnnotation>,
    orphan_signatures: BTreeSet<u64>,
    members: Vec<EntityDraft>,
}

type HaskellClauseKey = (Option<u64>, Option<[u8; 32]>, String, String);

fn haskell_owned_text(text: &str, cancellation: &Cancellation) -> Result<String, AdapterError> {
    cancellation.check()?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    cancellation.check()?;
    owned.push_str(text);
    Ok(owned)
}

struct HaskellAnnotation {
    definition: Option<u64>,
    signature: Option<SourceSpan>,
    target: Option<u64>,
    enclosing: Option<u64>,
}

#[derive(Clone, Default)]
struct ScopeContext {
    stable_identity: Option<[u8; 32]>,
    collision_guard: Option<[u8; 32]>,
    lexical_module: Option<u64>,
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
    ObjectiveCOwner,
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
            if let Some(root) = draft.data_root_entity {
                // A data address is file-local for standalone sources, but each
                // embedded example owns an independent address space in its host.
                let root = materialized
                    .get(&root)
                    .ok_or_else(|| provider_failure("embedded-data-root-missing"))?;
                identity.push(5);
                identity.extend_from_slice(root.record.id.as_bytes());
            }
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
    if let Some(module) = draft.lexical_module {
        // Extensions/categories name an external type without owning it, but
        // identical embedded examples must retain separate source identities.
        let module = materialized
            .get(&module)
            .ok_or_else(|| provider_failure("embedded-source-owner-missing"))?;
        container_identity.push(6);
        container_identity.extend_from_slice(module.record.id.as_bytes());
    }
    debug_assert_eq!(qualified_name.len(), draft.qualified_length);
    let semantic_kind = entity_kind_identity_label(draft.kind);
    // Repeated Haskell equations are one written lexical binding. Pattern heads
    // and type annotations remain evidence, not overload or arity identities.
    let signature_discriminator = if draft.language == "haskell"
        && matches!(
            draft.kind,
            EntityKind::Function | EntityKind::Method | EntityKind::Variable
        ) {
        &[][..]
    } else {
        draft.signature.as_bytes()
    };
    let identity_guard = entity_identity_guard(
        full_source.repository(),
        &draft.language,
        semantic_kind,
        &container_identity,
        &draft.name,
        signature_discriminator,
        build_context.digest().as_bytes(),
        draft.scope_collision_guard.as_ref(),
    )?;
    let id = derive_symbol(SymbolIdentity {
        repository: full_source.repository(),
        language: &draft.language,
        semantic_kind,
        container_identity: &container_identity,
        declared_identity: &draft.name,
        signature_discriminator,
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
        display_name: if draft.kind == EntityKind::Property
            || matches!(draft.language.as_str(), "toml" | "nix")
        {
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
        signature_discriminator: signature_discriminator.to_vec(),
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
        "vba.compilation_constant.declaration"
        | "vba.preprocessor.scope"
        | "vba.conditional_sub_function.declaration"
        | "vba.conditional_function.declaration"
        | "vba.conditional_property.declaration" => Some((
            FactDomain::Relations,
            VBA_CONDITIONAL_EVALUATION_UNAVAILABLE,
        )),
        "groovy.invalid_callable.scope" => {
            Some((FactDomain::Entities, "groovy-callable-identity-unavailable"))
        }
        "groovy.import.scope" => Some((FactDomain::Relations, "unresolved-import-target")),
        "groovy.file.module" => Some((
            FactDomain::Relations,
            "groovy-import-inheritance-and-runtime-dispatch-resolution-unavailable",
        )),
        "perl.function.declaration" => {
            Some((FactDomain::Entities, "perl-function-ownership-unavailable"))
        }
        "nix.file.module" => Some((
            FactDomain::Relations,
            "nix-attribute-import-and-runtime-binding-resolution-unavailable",
        )),
        "nix.dynamic_variable.declaration"
        | "nix.dynamic_function.declaration"
        | "nix.dynamic_path_variable.declaration"
        | "nix.dynamic_path_function.declaration" => Some((
            FactDomain::Entities,
            "nix-computed-attribute-name-unavailable",
        )),
        "objective_c.file.root" => Some((
            FactDomain::Entities,
            "objective-c-preprocessed-and-inherited-c-declarations-unavailable",
        )),
        "objective_c.file.module" => Some((
            FactDomain::Relations,
            "objective-c-inheritance-message-dispatch-and-cross-file-binding-unavailable",
        )),
        "scala.unbraced_package_unavailable.scope" => Some((
            FactDomain::Relations,
            "scala-nonleading-unbraced-package-scope-unavailable",
        )),
        "groovy.package_ownership_unavailable.scope" => Some((
            FactDomain::Relations,
            "groovy-invalid-package-ownership-unavailable",
        )),
        "powershell.file.module" => Some((
            FactDomain::Relations,
            "powershell-runtime-command-module-and-dispatch-resolution-unavailable",
        )),
        "powershell.hashtable.scope" => Some((
            FactDomain::Relations,
            "powershell-hashtable-runtime-key-comparison-unavailable",
        )),
        "powershell.dynamic_key.scope" => Some((
            FactDomain::Entities,
            "powershell-computed-key-value-unavailable",
        )),
        "powershell.data.scope" => Some((
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
        "astro.file.module" => Some((
            FactDomain::Relations,
            "astro-template-semantics-unavailable",
        )),
        "astro.expression.signature" => Some((
            FactDomain::Entities,
            "astro-expression-analysis-unavailable",
        )),
        "astro.frontmatter.signature" | "astro.embedded_text.signature" => {
            Some((FactDomain::Entities, "astro-embedded-analysis-unavailable"))
        }
        "astro.embedded_limit.signature" => {
            Some((FactDomain::Entities, "astro-embedded-analysis-limit"))
        }
        "astro.embedded_parse_error.signature" => {
            Some((FactDomain::Entities, "astro-embedded-parse-error"))
        }
        "astro.unmatched_end_tag.signature" => {
            Some((FactDomain::Entities, "astro-unmatched-end-tag"))
        }
        "markdown.file.module" => Some((
            FactDomain::Relations,
            "markdown-link-resolution-unavailable",
        )),
        "markdown.inline.signature" => Some((
            FactDomain::Occurrences,
            "markdown-inline-analysis-unavailable",
        )),
        "markdown.inline_limit.signature" => Some((
            FactDomain::Occurrences,
            "markdown-inline-budget-unavailable",
        )),
        "markdown.inline_error.signature" => {
            Some((FactDomain::Occurrences, "markdown-inline-parse-unavailable"))
        }
        "markdown.inline_embedded.signature" => {
            Some((FactDomain::Entities, "markdown-inline-embedded-unavailable"))
        }
        "markdown.embedded_text.signature" | "markdown.code_block.signature" => Some((
            FactDomain::Entities,
            "markdown-embedded-analysis-unavailable",
        )),
        "markdown.code_limit.signature" => {
            Some((FactDomain::Entities, "markdown-code-budget-unavailable"))
        }
        "markdown.code_error.signature"
        | "markdown.classic_code_error.signature"
        | "markdown.fsharp_code_error.signature"
        | "markdown.fsharp_signature_code_error.signature"
        | "markdown.cobol_fixed_code_error.signature"
        | "markdown.cobol_free_code_error.signature" => {
            Some((FactDomain::Entities, "markdown-code-parse-unavailable"))
        }
        "markdown.code_parsed.signature"
        | "markdown.classic_code_parsed.signature"
        | "markdown.fsharp_code_parsed.signature"
        | "markdown.fsharp_signature_code_parsed.signature"
        | "markdown.cobol_fixed_code_parsed.signature"
        | "markdown.cobol_free_code_parsed.signature" => {
            Some((FactDomain::Relations, "markdown-code-semantics-unavailable"))
        }
        "html.embedded_text.signature" => {
            Some((FactDomain::Entities, "html-embedded-analysis-unavailable"))
        }
        "html.embedded_limit.signature" => {
            Some((FactDomain::Entities, "html-embedded-resource-limit"))
        }
        "html.embedded_parse_error.signature" => {
            Some((FactDomain::Entities, "html-embedded-parse-error"))
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
    } else if language == "nix" {
        "rootlight.nix-source-occurrence/1"
    } else if language == "matlab" {
        "rootlight.matlab-source-occurrence/1"
    } else if language == "perl" {
        "rootlight.perl-source-occurrence/1"
    } else if language == "groovy" {
        "rootlight.groovy-source-occurrence/1"
    } else if language == "vba" {
        "rootlight.vba-preprocessor-branch/1"
    } else if language == "haskell" {
        "rootlight.haskell-source-scope/1"
    } else {
        "rootlight.powershell-source-occurrence/1"
    };
    source_occurrence_identity(context, parent, kind, position)
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
            | "objective_c.implementation.scope"
            | "objective_c.category.scope"
            | "css.media.scope"
            | "css.supports.scope"
            | "css.scope.scope"
            | "css.at_rule.scope"
            | "css.keyframe_step.scope"
    ) {
        let context = if syntax_kind == "swift.extension.scope" {
            "rootlight.swift-extension-scope-identity/1"
        } else if syntax_kind.starts_with("objective_c.") {
            "rootlight.objective-c-owner-scope-identity/1"
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
        role: if matches!(
            fact.syntax_kind().as_str(),
            "objective_c.forward_class.definition" | "objective_c.forward_protocol.definition"
        ) {
            OccurrenceRole::Declaration
        } else {
            OccurrenceRole::Definition
        },
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
    if language == "erlang" && fact.syntax_kind().as_str() == "erlang.function.call" {
        return rootlight_adapter_sdk::canonical_erlang_function_head(text, maximum_name_bytes)
            .map(std::borrow::Cow::Owned);
    }
    if language == "groovy"
        && matches!(
            fact.syntax_kind().as_str(),
            "groovy.type_import.reference"
                | "groovy.qualified_type.reference"
                | "groovy.extends_type.reference"
                | "groovy.implements_type.reference"
                | "groovy.package_star_import.reference"
                | "groovy.static_import.reference"
                | "groovy.static_star_import.reference"
        )
    {
        return rootlight_adapter_sdk::structural_captured_name_for_fact(
            language,
            fact,
            text,
            maximum_name_bytes,
        );
    }
    // Strings, documentation and full member-path metadata are not identifiers.
    // Only native identifier fields may acquire identifier decoding failures.
    let identifier_reference =
        fact.syntax_kind()
            .as_str()
            .split_once('.')
            .is_some_and(|(_, role)| {
                matches!(
                    role,
                    "identifier.reference"
                        | "type_identifier.reference"
                        | "property_identifier.reference"
                        | "member_name.reference"
                        | "type_member_name.reference"
                        | "type_namespace_member.reference"
                        | "type_query_member_name.reference"
                        | "type_namespace_root.reference"
                        | "type_query_value.reference"
                        | "type_export_local.reference"
                )
            });
    if matches!(language, "javascript" | "typescript")
        && (terminal_call_name.is_some() || identifier_reference)
    {
        return rootlight_adapter_sdk::canonical_ecmascript_identifier(
            terminal_call_name.unwrap_or(text),
            maximum_name_bytes,
        );
    }
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

fn diagnostic_skip_reason(code: &str) -> SkippedRegionReason {
    // Unanalyzed constructs and dialect uncertainty are not rejected syntax.
    if code.ends_with("-semantics-partial")
        || code.ends_with("-not-analyzed")
        || code.ends_with("-dialect-unverified")
    {
        SkippedRegionReason::UnsupportedConstruct
    } else if code.contains("limit") {
        SkippedRegionReason::ResourceLimit
    } else {
        SkippedRegionReason::ParseError
    }
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
    provenance_indexed: usize,
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
        provenance_indexed: usize,
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
            provenance_indexed,
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
        let provenance_discovered = self
            .provenance_indexed
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
                self.provenance_indexed,
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

#[derive(Clone, Copy)]
struct ProfileFence {
    span: SourceSpan,
    language: &'static str,
}

fn profile_fence_spans(
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<Vec<ProfileFence>, AdapterError> {
    let mut fences = Vec::new();
    for fact in facts {
        cancellation.check()?;
        let language = match fact.syntax_kind().as_str() {
            "markdown.fsharp_code_parsed.signature" | "markdown.fsharp_code_error.signature" => {
                "fsharp"
            }
            "markdown.fsharp_signature_code_parsed.signature"
            | "markdown.fsharp_signature_code_error.signature" => "fsharp-signature",
            "markdown.cobol_fixed_code_parsed.signature"
            | "markdown.cobol_fixed_code_error.signature" => "cobol-fixed",
            "markdown.cobol_free_code_parsed.signature"
            | "markdown.cobol_free_code_error.signature" => "cobol-free",
            _ => continue,
        };
        fences
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        fences.push(ProfileFence {
            span: fact.span(),
            language,
        });
    }
    crate::runtime::sort_cancellable_by(&mut fences, cancellation, |left, right| {
        left.span.start_byte().cmp(&right.span.start_byte())
    })?;
    for adjacent in fences.windows(2) {
        cancellation.check()?;
        if let [left, right] = adjacent
            && left.span.end_byte() > right.span.start_byte()
        {
            return Err(provider_failure("markdown-language-fence-overlap"));
        }
    }
    Ok(fences)
}

fn language_for_fact_with_profile_fences<'a>(
    request: &'a AnalysisRequest<'_>,
    fact: &'a SyntaxFact,
    fences: &[ProfileFence],
) -> &'a str {
    if request.language().as_str() == "markdown"
        && matches!(
            fact.syntax_kind()
                .as_str()
                .split_once('.')
                .map(|(prefix, _)| prefix),
            Some("cobol" | "fsharp")
        )
    {
        let position =
            fences.partition_point(|fence| fence.span.start_byte() <= fact.span().start_byte());
        if let Some(index) = position.checked_sub(1)
            && let Some(fence) = fences.get(index)
            && fact.span().file() == fence.span.file()
            && fact.span().end_byte() <= fence.span.end_byte()
            && matches!(
                (
                    fact.syntax_kind()
                        .as_str()
                        .split_once('.')
                        .map(|(prefix, _)| prefix),
                    fence.language
                ),
                (Some("cobol"), "cobol-fixed" | "cobol-free")
                    | (Some("fsharp"), "fsharp" | "fsharp-signature")
            )
        {
            return fence.language;
        }
    }
    language_for_fact(request, fact)
}

fn classic_fence_spans(
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<Vec<SourceSpan>, AdapterError> {
    let mut spans = Vec::new();
    for fact in facts {
        cancellation.check()?;
        if is_classic_fence(fact) {
            spans
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            spans.push(fact.span());
        }
    }
    crate::runtime::sort_cancellable_by(&mut spans, cancellation, |left, right| {
        left.start_byte().cmp(&right.start_byte())
    })?;
    for adjacent in spans.windows(2) {
        cancellation.check()?;
        if let [left, right] = adjacent
            && left.end_byte() > right.start_byte()
        {
            return Err(provider_failure("markdown-language-fence-overlap"));
        }
    }
    Ok(spans)
}

fn is_classic_fence(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "markdown.classic_code_parsed.signature" | "markdown.classic_code_error.signature"
    )
}

fn classic_dialect_spans<'a>(
    request: &'a AnalysisRequest<'_>,
) -> impl Iterator<Item = SourceSpan> + 'a {
    std::iter::once(request.source().source_ref().span())
        .filter(|_| {
            request.language().as_str() == "visual-basic-classic"
                && request.included_ranges().is_empty()
        })
        .chain(
            request
                .included_ranges()
                .iter()
                .filter(|range| range.language().as_str() == "visual-basic-classic")
                .map(rootlight_adapter_sdk::IncludedRange::span),
        )
}

fn language_for_fact<'a>(request: &'a AnalysisRequest<'_>, fact: &'a SyntaxFact) -> &'a str {
    if request.language().as_str() == "markdown"
        && fact.syntax_kind().as_str().starts_with("objective_c.")
    {
        return "objective-c";
    }
    if request.language().as_str() == "markdown"
        && let Some((language, _)) = fact.syntax_kind().as_str().split_once('.')
        && matches!(
            language,
            "rust"
                | "javascript"
                | "typescript"
                | "python"
                | "c"
                | "cpp"
                | "csharp"
                | "go"
                | "java"
                | "groovy"
                | "haskell"
                | "kotlin"
                | "lua"
                | "php"
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
                | "nix"
                | "matlab"
                | "perl"
                | "solidity"
                | "scala"
                | "dart"
                | "powershell"
                | "astro"
                | "vba"
        )
    {
        return language;
    }
    if matches!(request.language().as_str(), "html" | "astro") {
        if fact.syntax_kind().as_str().starts_with("typescript.") {
            return "typescript";
        }
        if fact.syntax_kind().as_str().starts_with("javascript.") {
            return "javascript";
        }
        if fact.syntax_kind().as_str().starts_with("css.") {
            return "css";
        }
    }
    containing_range(request.included_ranges(), fact.span())
        .map_or(
            request.language(),
            rootlight_adapter_sdk::IncludedRange::language,
        )
        .as_str()
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
                | "nix.file.module"
                | "matlab.file.module"
                | "perl.file.module"
                | "groovy.file.module"
                | "haskell.file.module"
                | "fsharp.file.module"
                | "vba.file.module"
                | "vba.class.module"
                | "solidity.file.module"
                | "scala.file.module"
                | "dart.file.module"
                | "powershell.file.module"
                | "markdown.file.module"
                | "astro.file.module"
                | "objective_c.file.module"
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
                | "nix"
                | "matlab"
                | "perl"
                | "solidity"
                | "scala"
                | "dart"
                | "powershell"
                | "markdown"
                | "groovy"
                | "haskell"
                | "fsharp"
                | "fsharp-signature"
                | "vba"
                | "visual-basic-classic"
                | "astro"
                | "objective-c"
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

fn is_haskell_annotation_name(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Declaration
        && matches!(
            fact.syntax_kind().as_str(),
            "haskell.signature_name.declaration" | "haskell.signature_member_name.declaration"
        )
}

fn is_haskell_signature_member_name(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Declaration
        && fact.syntax_kind().as_str() == "haskell.signature_member_name.declaration"
}

fn is_haskell_type_annotation(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "haskell.written_signature.signature"
}

fn is_signature_capture(fact: &SyntaxFact) -> bool {
    // Host context spans can equal a whole embedded function, but are not its
    // callable header. Associating them would make the real signature ambiguous.
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str().ends_with(".signature")
        && !fact.syntax_kind().as_str().starts_with("vba.compilation_")
        && !fact.syntax_kind().as_str().contains(".default_export_")
        && !fact.syntax_kind().as_str().contains(".export_")
        && !fact.syntax_kind().as_str().contains(".hoisted_binding.")
        && !matches!(
            fact.syntax_kind().as_str(),
            "typescript.conditional_right.signature"
                | "typescript.conditional_consequence.signature"
        )
        && fact.syntax_kind().as_str() != "sql.body.signature"
        && !fact.syntax_kind().as_str().starts_with("html.embedded_")
        && !fact.syntax_kind().as_str().starts_with("astro.")
        && !fact.syntax_kind().as_str().starts_with("markdown.")
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

fn haskell_signature_owner_matches(signature: &SyntaxFact, owner: Option<&SyntaxFact>) -> bool {
    match signature.syntax_kind().as_str() {
        // These headers belong to lexical scopes, not the enclosing named declaration.
        "haskell.lambda_header.signature" | "haskell.instance_header.signature" => false,
        "haskell.function_header.signature" => owner
            .is_some_and(|owner| owner.syntax_kind().as_str() == "haskell.function.declaration"),
        "haskell.binding_header.signature" => owner
            .is_some_and(|owner| owner.syntax_kind().as_str() == "haskell.variable.declaration"),
        "haskell.declaration_header.signature" => owner.is_some_and(|owner| {
            matches!(
                owner.syntax_kind().as_str(),
                "haskell.module.declaration"
                    | "haskell.data_type.declaration"
                    | "haskell.newtype.declaration"
                    | "haskell.type_synonym.declaration"
                    | "haskell.class.declaration"
                    | "haskell.constructor.declaration"
            )
        }),
        _ => true,
    }
}

fn supports_declaration_signature(
    kind: EntityKind,
    language: &str,
    declaration: &SyntaxFact,
    definition: Option<&SyntaxFact>,
) -> bool {
    if matches!(language, "fsharp" | "fsharp-signature")
        && declaration.syntax_kind().as_str() == "fsharp.file.module"
    {
        return false;
    }
    if language == "haskell" {
        if kind == EntityKind::Variable {
            // Pattern leaves have the exact name span; equations also contain their match.
            return definition.is_some_and(|definition| definition.span() != declaration.span());
        }
        if kind == EntityKind::Field {
            return declaration.syntax_kind().as_str() == "haskell.field.declaration";
        }
    }
    supports_signature(kind, language)
}

fn supports_signature(kind: EntityKind, language: &str) -> bool {
    // Classic BASIC accessors share a name but have distinct Get/Let/Set headers.
    // The same rule governs identity, evidence emission and admission accounting.
    matches!(language, "fsharp" | "fsharp-signature")
        || (kind == EntityKind::Variable && language == "haskell")
        || (kind == EntityKind::Property && matches!(language, "vba" | "visual-basic-classic"))
        || (kind == EntityKind::Module && matches!(language, "cobol-fixed" | "cobol-free"))
        || matches!(
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

fn automatic_instantiation_gap(fact: &SyntaxFact) -> Option<&'static str> {
    // As New establishes a declared type, not a declaration-time call. Its
    // lazy creation on a Nothing-valued read needs separate use-site evidence
    // (MS-VBAL 5.3.1.10); resolving the type must not erase that missing analysis.
    matches!(
        fact.syntax_kind().as_str(),
        "vba.automatic_type_identifier.reference"
            | "vba.unsupported_automatic_type_identifier.reference"
    )
    .then_some("vba-automatic-instantiation-semantics-unavailable")
}

fn source_reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    if fact.syntax_kind().as_str().starts_with("cobol.")
        && fact.kind() == SyntaxFactKind::Occurrence
        && !is_definition_capture(fact)
    {
        return Some("cobol-binding-target-unavailable");
    }
    if fact.syntax_kind().as_str().starts_with("haskell.")
        && fact.kind() == SyntaxFactKind::Occurrence
        && !is_definition_capture(fact)
        && !is_call_name_capture(fact)
    {
        return Some(HASKELL_BINDING_UNAVAILABLE);
    }
    match fact.syntax_kind().as_str() {
        "vba.procedure_pointer.reference"
        | "vba.qualified_procedure_pointer.reference"
        | "vba.unsupported_procedure_pointer.reference" => {
            Some("vba-procedure-pointer-target-unavailable")
        }
        "vba.event_identifier.reference" => Some("vba-event-target-unavailable"),
        "vba.statement_label.reference"
        | "vba.error_label.reference"
        | "vba.resume_label.reference" => Some("vba-statement-label-target-unavailable"),
        "vba.direct_type_identifier.reference"
        | "vba.implemented_type.reference"
        | "vba.unsupported_implemented_type.reference"
        | "vba.constructed_type_identifier.reference"
        | "vba.tested_type_identifier.reference"
        | "vba.unsupported_constructed_type_identifier.reference"
        | "vba.automatic_type_identifier.reference"
        | "vba.unsupported_automatic_type_identifier.reference"
        | "vba.unsupported_type_identifier.reference"
        | "vba.type_identifier.reference" => Some("vba-type-target-unavailable"),
        "vba.instance.reference" => Some("vba-instance-owner-unavailable"),
        "vba.compilation_identifier.reference" => {
            Some("vba-compilation-constant-target-unavailable")
        }
        "vba.construction.reference" => Some("vba-construction-semantics-unavailable"),
        "vba.module_name.reference" => Some("vba-module-name-unavailable"),
        "vba.identifier.reference"
        | "vba.value_identifier.reference"
        | "vba.let_identifier.reference"
        | "vba.set_identifier.reference" => Some("vba-binding-target-unavailable"),
        "vba.invoked_identifier.reference"
        | "vba.invoked_instance.reference"
        | "vba.invoked_get_identifier.reference"
        | "vba.invoked_let_identifier.reference"
        | "vba.invoked_set_identifier.reference" => Some("vba-call-or-index-target-unavailable"),
        "vba.default_constant_identifier.reference" => {
            Some("vba-default-constant-target-unavailable")
        }
        // A bare application and its callee can share a span. Keep target and
        // application gaps distinct so resolving a member never proves its arguments.
        "vba.member.reference"
        | "vba.invoked_member.reference"
        | "vba.get_member.reference"
        | "vba.let_member.reference"
        | "vba.set_member.reference"
        | "vba.invoked_get_member.reference"
        | "vba.invoked_let_member.reference"
        | "vba.invoked_set_member.reference" => Some("vba-member-target-unavailable"),
        "vba.self_member.reference"
        | "vba.self_get_member.reference"
        | "vba.self_let_member.reference"
        | "vba.self_set_member.reference" => Some("vba-member-target-unavailable"),
        "vba.self_invoked_member.reference"
        | "vba.self_invoked_get_member.reference"
        | "vba.self_invoked_let_member.reference"
        | "vba.self_invoked_set_member.reference" => Some("vba-call-or-index-target-unavailable"),
        "vba.application.reference" | "vba.incomplete_application.reference" => {
            Some("vba-call-or-index-target-unavailable")
        }
        "vba.nonvalue_identifier.reference" => Some("vba-nonvalue-target-unavailable"),
        "perl.variable_name.reference"
        | "perl.array_container.reference"
        | "perl.hash_container.reference"
        | "perl.array_length.reference"
        | "perl.dynamic_container.reference" => Some("perl-binding-target-unavailable"),
        "perl.function_name.reference"
        | "perl.static_function_name.reference"
        | "perl.bare_function_name.reference"
        | "perl.builtin_function_name.reference"
        | "perl.importable_function_name.reference"
        | "perl.amper_function_name.reference"
        | "perl.code_function_name.reference"
        | "perl.direct_coderef_function_name.reference"
        | "perl.coderef_application.reference" => Some("perl-function-target-unavailable"),
        "perl.method_application.reference" => Some("perl-method-target-unavailable"),
        "perl.identifier.reference"
        | "perl.module_name.reference"
        | "perl.use_module_name.reference" => Some("perl-import-target-unavailable"),
        "perl.subs_import.reference" | "perl.subs_word_list.reference" => {
            Some("perl-import-target-unavailable")
        }
        "perl.package_context.reference" => Some("perl-package-ownership-unavailable"),
        "matlab.member_name.reference" => Some("matlab-member-target-unavailable"),
        "matlab.function_handle_name.reference"
        | "matlab.unqualified_function_handle_name.reference" => {
            Some("matlab-function-handle-target-unavailable")
        }
        "matlab.type_or_attribute_name.reference" => {
            Some("matlab-type-or-attribute-target-unavailable")
        }
        "matlab.identifier.reference" | "matlab.indexed_value_name.reference" => {
            Some("matlab-binding-target-unavailable")
        }
        "matlab.application.reference" | "matlab.named_application.reference" => {
            Some("matlab-call-or-index-target-unavailable")
        }
        "matlab.import.reference" => Some("matlab-import-target-unavailable"),
        "matlab.command.reference" => Some("matlab-command-target-unavailable"),
        "scss.mixin.call" => Some("scss-mixin-target-unavailable"),
        "scss.extend.reference" => Some("scss-selector-target-unavailable"),
        "nix.identifier.reference" | "nix.inherited_name.reference" => {
            Some("nix-lexical-binding-target-unavailable")
        }
        "nix.selected_attribute.reference" => Some("nix-attribute-selection-target-unavailable"),
        "nix.inherited_attribute.reference" => Some("nix-inherited-attribute-target-unavailable"),
        _ => None,
    }
}

fn import_target_gap(fact: &SyntaxFact) -> &'static str {
    if fact.syntax_kind().as_str() == "cobol.copy.import" {
        "cobol-copy-target-unavailable"
    } else {
        "unresolved-import-target"
    }
}

fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    if nix_paths::is_segment(fact) {
        return None;
    }
    match fact.kind() {
        SyntaxFactKind::Import => Some(OccurrenceRole::ImportUse),
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str() == "cobol.perform_target.reference" =>
        {
            Some(OccurrenceRole::CallSite)
        }
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str() == "groovy.extends_type.reference" =>
        {
            Some(OccurrenceRole::InheritanceUse)
        }
        SyntaxFactKind::Occurrence
            if matches!(
                fact.syntax_kind().as_str(),
                "groovy.implements_type.reference"
                    | "vba.implemented_type.reference"
                    | "vba.unsupported_implemented_type.reference"
            ) =>
        {
            Some(OccurrenceRole::ImplementationUse)
        }
        SyntaxFactKind::Occurrence
            if matches!(
                fact.syntax_kind().as_str(),
                "perl.use_module_name.reference"
                    | "groovy.type_import.reference"
                    | "groovy.package_star_import.reference"
                    | "groovy.static_import.reference"
                    | "groovy.static_star_import.reference"
            ) =>
        {
            Some(OccurrenceRole::ImportUse)
        }
        SyntaxFactKind::Occurrence if is_definition_capture(fact) => None,
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str() == "objective_c.selector.definition_part" =>
        {
            None
        }
        SyntaxFactKind::Occurrence if is_call_name_capture(fact) => None,
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str().starts_with("haskell.")
                && (fact.syntax_kind().as_str().contains("type")
                    || fact.syntax_kind().as_str() == "haskell.instance_class_name.reference") =>
        {
            Some(OccurrenceRole::TypeUse)
        }
        SyntaxFactKind::Occurrence
            if matches!(
                fact.syntax_kind().as_str(),
                "javascript.member_path.reference" | "typescript.member_path.reference"
            ) =>
        {
            None
        }
        SyntaxFactKind::Occurrence
            if matches!(
                fact.syntax_kind().as_str(),
                "elm.type.reference"
                    | "elm.ambiguous_type.reference"
                    | "typescript.type_identifier.reference"
                    | "vba.direct_type_identifier.reference"
                    | "vba.constructed_type_identifier.reference"
                    | "vba.tested_type_identifier.reference"
                    | "vba.unsupported_constructed_type_identifier.reference"
                    | "vba.automatic_type_identifier.reference"
                    | "vba.unsupported_automatic_type_identifier.reference"
                    | "vba.unsupported_type_identifier.reference"
                    | "vba.type_identifier.reference"
                    | "typescript.type_member_name.reference"
                    | "typescript.type_export_local.reference"
                    | "javascript.type_export_local.reference"
            ) =>
        {
            Some(OccurrenceRole::TypeUse)
        }
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

fn sort_owned_cancellable_by<T>(
    values: &mut [T],
    cancellation: &Cancellation,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering + Copy,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(values.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for index in 0..values.len() {
        check_periodically(index, cancellation)?;
        ordered.push(index);
    }
    crate::runtime::sort_cancellable_by(&mut ordered, cancellation, |left, right| {
        // These indices are the complete original position set; sorting only
        // permutes that set while the owned values remain immutably borrowed.
        compare(&values[*left], &values[*right]).then_with(|| left.cmp(right))
    })?;
    cancellation.check()?;
    let mut destination = Vec::new();
    destination
        .try_reserve_exact(values.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for index in 0..values.len() {
        check_periodically(index, cancellation)?;
        destination.push(0);
    }
    for (new_position, old_position) in ordered.into_iter().enumerate() {
        check_periodically(new_position, cancellation)?;
        destination[old_position] = new_position;
    }
    // Invert new→old ordering before swapping: using it directly would apply
    // the inverse sort for cycles longer than two elements.
    for position in 0..values.len() {
        check_periodically(position, cancellation)?;
        while destination[position] != position {
            cancellation.check()?;
            let target = destination[position];
            values.swap(position, target);
            destination.swap(position, target);
        }
    }
    cancellation.check()?;
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
    fn haskell_scope_headers_cannot_replace_a_named_declaration_header() {
        let fact = |id, span, label| {
            SyntaxFact::new(
                id,
                None,
                SyntaxFactKind::Signature,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
        let owner = declaration("haskell.function.declaration");
        let header = fact(2, span, "haskell.function_header.signature");
        let lambda = fact(3, span, "haskell.lambda_header.signature");
        let instance = fact(4, span, "haskell.instance_header.signature");
        assert!(haskell_signature_owner_matches(&header, Some(&owner)));
        assert!(!haskell_signature_owner_matches(&header, None));
        assert!(!haskell_signature_owner_matches(&lambda, Some(&owner)));
        assert!(!haskell_signature_owner_matches(&instance, Some(&owner)));
        let foreign = fact(5, span, "rust.function.signature");
        assert!(haskell_signature_owner_matches(&foreign, None));
        let conflicting = fact(
            6,
            SourceSpan::new(span.file(), 0, 2).unwrap(),
            "haskell.function_header.signature",
        );
        assert!(select_unique_capture(&[&header, &header]).is_some());
        assert!(select_unique_capture(&[&header, &conflicting]).is_none());
    }

    #[test]
    fn haskell_signature_requiredness_distinguishes_name_leaves_from_equations() {
        let leaf = declaration("haskell.variable.declaration");
        let definition = SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Occurrence,
            leaf.span(),
            1,
            SyntaxKindLabel::new("haskell.binding_name.definition").unwrap(),
        );
        let equation = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(leaf.span().file(), 0, 8).unwrap(),
            0,
            SyntaxKindLabel::new("haskell.variable.declaration").unwrap(),
        );
        assert!(!supports_declaration_signature(
            EntityKind::Variable,
            "haskell",
            &leaf,
            Some(&definition)
        ));
        assert!(supports_declaration_signature(
            EntityKind::Variable,
            "haskell",
            &equation,
            Some(&definition)
        ));
        assert!(!supports_declaration_signature(
            EntityKind::Variable,
            "haskell",
            &equation,
            None
        ));
        assert!(!supports_declaration_signature(
            EntityKind::Variable,
            "javascript",
            &equation,
            Some(&definition)
        ));
        assert!(supports_declaration_signature(
            EntityKind::Field,
            "haskell",
            &declaration("haskell.field.declaration"),
            None
        ));
        assert!(!supports_declaration_signature(
            EntityKind::Field,
            "rust",
            &declaration("rust.field.declaration"),
            None
        ));
    }

    #[test]
    fn owned_lowering_sort_preserves_non_copy_payloads_and_stable_ties() {
        struct Owned {
            key: u64,
            payload: String,
        }
        let mut values: Vec<_> = [(2, "two"), (0, "zero"), (1, "one-first"), (1, "one-second")]
            .into_iter()
            .map(|(key, payload)| Owned {
                key,
                payload: payload.to_owned(),
            })
            .collect();
        sort_owned_cancellable_by(&mut values, &Cancellation::new(), |left, right| {
            left.key.cmp(&right.key)
        })
        .unwrap();
        assert_eq!(
            values
                .iter()
                .map(|value| value.payload.as_str())
                .collect::<Vec<_>>(),
            ["zero", "one-first", "one-second", "two"]
        );
        let mut empty: Vec<Owned> = Vec::new();
        sort_owned_cancellable_by(&mut empty, &Cancellation::new(), |left, right| {
            left.key.cmp(&right.key)
        })
        .unwrap();
        let mut singleton = vec![Owned {
            key: 1,
            payload: "only".to_owned(),
        }];
        let pointer = singleton[0].payload.as_ptr();
        sort_owned_cancellable_by(&mut singleton, &Cancellation::new(), |left, right| {
            left.key.cmp(&right.key)
        })
        .unwrap();
        assert_eq!(singleton[0].payload.as_ptr(), pointer);
    }

    #[test]
    fn owned_lowering_sort_moves_large_vectors_without_cloning_payloads() {
        struct Owned {
            key: usize,
            payload: Box<usize>,
        }
        let count = 50_000;
        let mut values: Vec<_> = (0..count)
            .rev()
            .map(|key| Owned {
                key,
                payload: Box::new(key),
            })
            .collect();
        let pointers: BTreeMap<_, _> = values
            .iter()
            .map(|value| (value.key, std::ptr::from_ref(value.payload.as_ref())))
            .collect();
        let comparisons = std::cell::Cell::new(0_usize);
        sort_owned_cancellable_by(&mut values, &Cancellation::new(), |left, right| {
            comparisons.set(comparisons.get() + 1);
            left.key.cmp(&right.key)
        })
        .unwrap();
        for (position, value) in values.iter().enumerate() {
            assert_eq!(value.key, position);
            assert_eq!(*value.payload, position);
            assert_eq!(
                std::ptr::from_ref(value.payload.as_ref()),
                pointers[&position]
            );
        }
        assert!(comparisons.get() < count * 32);
    }

    #[test]
    fn owned_lowering_sort_propagates_cancellation_without_losing_owned_values() {
        use rootlight_cancel::CancellationReason;
        let mut values: Vec<_> = (0..1024).rev().map(|key| format!("{key:04}")).collect();
        let original = values.clone();
        let cancellation = Cancellation::new();
        let comparisons = std::cell::Cell::new(0);
        assert!(matches!(
            sort_owned_cancellable_by(&mut values, &cancellation, |left, right| {
                comparisons.set(comparisons.get() + 1);
                if comparisons.get() == 100 {
                    cancellation.cancel(CancellationReason::ClientRequest);
                }
                left.cmp(right)
            }),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(values, original);
        assert!(comparisons.get() >= 100);
        assert!(comparisons.get() < 1024);
        assert!(matches!(
            sort_owned_cancellable_by(&mut values, &cancellation, Ord::cmp),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(values, original);
    }

    #[test]
    fn unsupported_constructs_do_not_claim_parse_errors() {
        for code in [
            "agda-layout-and-expression-semantics-partial",
            "awk-expression-semantics-partial",
            "erlang-macro-and-expression-semantics-partial",
            "make-recipe-not-analyzed",
            "ini-value-semantics-not-analyzed",
            "ini-indented-key-dialect-unverified",
        ] {
            assert_eq!(
                diagnostic_skip_reason(code),
                SkippedRegionReason::UnsupportedConstruct
            );
        }
        assert_eq!(
            diagnostic_skip_reason("parser-node-limit"),
            SkippedRegionReason::ResourceLimit
        );
        assert_eq!(
            diagnostic_skip_reason("unexpected-token"),
            SkippedRegionReason::ParseError
        );
        assert_eq!(
            diagnostic_skip_reason("ini-section-header-parse-error"),
            SkippedRegionReason::ParseError
        );
    }

    #[test]
    fn groovy_import_names_reject_incomplete_and_oversized_evidence() {
        for label in [
            "groovy.type_import.reference",
            "groovy.package_star_import.reference",
            "groovy.static_import.reference",
            "groovy.static_star_import.reference",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(occurrence_role(&fact), Some(OccurrenceRole::ImportUse));
            for invalid in [
                "",
                "sample.",
                "sample..Type",
                "sample /* unfinished",
                "sample.*",
                "sample.Type as Alias",
                "sample.\nType",
            ] {
                assert!(
                    structural_resolution_text("groovy", &fact, invalid, None, 128).is_none(),
                    "{label}: {invalid}"
                );
            }
            let text = "sample /* trivia */ . Type";
            assert_eq!(
                structural_resolution_text("groovy", &fact, text, None, text.len()).as_deref(),
                Some("sample.Type")
            );
            assert!(
                structural_resolution_text("groovy", &fact, text, None, text.len() - 1).is_none()
            );
        }
    }

    #[test]
    fn language_specific_declarations_map_conservatively() {
        for (label, expected) in [
            ("javascript.import_binding.declaration", EntityKind::Import),
            ("typescript.import_binding.declaration", EntityKind::Import),
            ("groovy.import_alias.declaration", EntityKind::Import),
            (
                "javascript.type_import_binding.declaration",
                EntityKind::Import,
            ),
            (
                "typescript.type_import_binding.declaration",
                EntityKind::Import,
            ),
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
            (
                "objective_c.type_parameter.declaration",
                EntityKind::TypeParameter,
            ),
            ("typescript.type_alias.declaration", EntityKind::TypeAlias),
            (
                "typescript.type_parameter.declaration",
                EntityKind::TypeParameter,
            ),
            (
                "typescript.mapped_type_parameter.declaration",
                EntityKind::TypeParameter,
            ),
            (
                "typescript.infer_parameter.declaration",
                EntityKind::TypeParameter,
            ),
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
