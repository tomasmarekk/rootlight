//! Table-driven language adapters and validated deep semantic context imports.
//!
//! Contexts contain canonical normalized facts captured without repository
//! execution; the analyzer revalidates request identity before bounded emission.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisReport, AnalysisRequest, CoverageReport, DiagnosticCode, DomainCoverage,
    IrBatch, IrBatchSink, IrRecord, LanguageAnalyzer, LanguageId, MemoryEnforcement,
    ProducerDescriptor, RemainingBudget, ResourceUsage, SinkError, StreamEnd, StreamUsage,
};
use rootlight_cancel::Cancellation;
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, ExtensionSupport, FactDomain, IrLimits,
    IrValidationError, NormalizedIrDocument, ProducerIdentity, ProducerKind,
    canonicalize_ir_document,
};

const CANCELLATION_CHECK_INTERVAL: usize = 64;
/// Hard ceiling for one process-local language profile registry.
pub const MAX_LANGUAGE_PROFILES: usize = 256;

/// Broad semantic behavior used by calibration and promotion policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LanguageSemantics {
    /// Calls and names normally bind through declarative static context.
    Static,
    /// Runtime mutation can invalidate otherwise plausible static bindings.
    Dynamic,
}

/// Immutable table-driven policy for one language identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageProfile {
    language: LanguageId,
    maximum_tier: AnalysisTier,
    semantics: LanguageSemantics,
}

impl LanguageProfile {
    /// Creates a checked language profile.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageProfileError`] when the language label is invalid.
    pub fn new(
        language: &str,
        maximum_tier: AnalysisTier,
        semantics: LanguageSemantics,
    ) -> Result<Self, LanguageProfileError> {
        Ok(Self {
            language: LanguageId::new(language)
                .map_err(|_| LanguageProfileError::InvalidLanguage)?,
            maximum_tier,
            semantics,
        })
    }

    /// Returns the canonical language identity.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }

    /// Returns the strongest evidence tier this profile may advertise.
    #[must_use]
    pub const fn maximum_tier(&self) -> AnalysisTier {
        self.maximum_tier
    }

    /// Returns the broad calibration behavior.
    #[must_use]
    pub const fn semantics(&self) -> LanguageSemantics {
        self.semantics
    }
}

/// Invalid language profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LanguageProfileError {
    /// The language was not a bounded SDK label.
    #[error("language profile identity is invalid")]
    InvalidLanguage,
}

/// Bounded deterministic registry shared by all first-party language profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageAdapterRegistry {
    profiles: BTreeMap<LanguageId, LanguageProfile>,
}

impl LanguageAdapterRegistry {
    /// Builds a bounded registry and rejects duplicate language identities.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageRegistryError`] for an empty, oversized, or duplicate
    /// profile collection.
    pub fn new(profiles: Vec<LanguageProfile>) -> Result<Self, LanguageRegistryError> {
        if profiles.is_empty() {
            return Err(LanguageRegistryError::Empty);
        }
        if profiles.len() > MAX_LANGUAGE_PROFILES {
            return Err(LanguageRegistryError::TooMany {
                observed: profiles.len(),
                limit: MAX_LANGUAGE_PROFILES,
            });
        }
        let observed = profiles.len();
        let profiles = profiles
            .into_iter()
            .map(|profile| (profile.language.clone(), profile))
            .collect::<BTreeMap<_, _>>();
        if profiles.len() != observed {
            return Err(LanguageRegistryError::DuplicateLanguage);
        }
        Ok(Self { profiles })
    }

    /// Returns the profile for one exact language identity.
    #[must_use]
    pub fn get(&self, language: &LanguageId) -> Option<&LanguageProfile> {
        self.profiles.get(language)
    }

    /// Returns profiles in canonical language order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &LanguageProfile> {
        self.profiles.values()
    }
}

/// Invalid bounded language-adapter registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LanguageRegistryError {
    /// At least one profile is required.
    #[error("language adapter registry must not be empty")]
    Empty,
    /// The configured profile count exceeded the hard registry ceiling.
    #[error("language adapter registry contains {observed} profiles, limit is {limit}")]
    TooMany {
        /// Configured profile count.
        observed: usize,
        /// Hard profile ceiling.
        limit: usize,
    },
    /// Two profiles used the same canonical language identity.
    #[error("language adapter registry contains a duplicate language")]
    DuplicateLanguage,
}

/// Returns the initial M09 semantic language registry.
///
/// # Errors
///
/// Returns [`InitialRegistryError`] if a built-in profile violates the same
/// bounded contracts applied to configured profiles.
pub fn initial_semantic_registry() -> Result<LanguageAdapterRegistry, InitialRegistryError> {
    let profiles = [
        ("go", AnalysisTier::TierA, LanguageSemantics::Static),
        (
            "javascript",
            AnalysisTier::TierB,
            LanguageSemantics::Dynamic,
        ),
        ("python", AnalysisTier::TierB, LanguageSemantics::Dynamic),
        ("rust", AnalysisTier::TierA, LanguageSemantics::Static),
        ("typescript", AnalysisTier::TierA, LanguageSemantics::Static),
    ]
    .into_iter()
    .map(|(language, tier, semantics)| LanguageProfile::new(language, tier, semantics))
    .collect::<Result<Vec<_>, _>>()?;
    LanguageAdapterRegistry::new(profiles).map_err(InitialRegistryError::Registry)
}

/// Failure constructing the built-in semantic registry.
#[derive(Debug, thiserror::Error)]
pub enum InitialRegistryError {
    /// A built-in language label violated the common profile contract.
    #[error(transparent)]
    Profile(#[from] LanguageProfileError),
    /// Built-in profiles violated bounded registry invariants.
    #[error(transparent)]
    Registry(#[from] LanguageRegistryError),
}

/// Canonical semantic facts imported from an explicitly captured build context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSemanticContext {
    language: LanguageId,
    tier: AnalysisTier,
    build_context: BuildContextIdentity,
    document: NormalizedIrDocument,
    coverage_status: CoverageStatus,
    covered_source_bytes: usize,
    domain_coverage: Vec<DomainCoverage>,
}

impl ImportedSemanticContext {
    /// Validates one full-file Tier A or Tier B semantic context.
    ///
    /// The document must contain exactly one file, use one language and build
    /// identity throughout, and remain valid under the common IR contract.
    ///
    /// # Errors
    ///
    /// Returns [`ImportedContextError`] when identity, tier, language, coverage,
    /// or normalized-IR invariants are inconsistent.
    pub fn new(
        language: &str,
        tier: AnalysisTier,
        build_context: BuildContextIdentity,
        document: NormalizedIrDocument,
        limits: &IrLimits,
    ) -> Result<Self, ImportedContextError> {
        if !matches!(tier, AnalysisTier::TierA | AnalysisTier::TierB) {
            return Err(ImportedContextError::UnsupportedTier);
        }
        let language =
            LanguageId::new(language).map_err(|_| ImportedContextError::InvalidLanguage)?;
        let document = canonicalize_ir_document(document, limits, &ExtensionSupport::default())
            .map_err(ImportedContextError::InvalidDocument)?;
        let [file] = document.files.as_slice() else {
            return Err(ImportedContextError::SingleFileRequired);
        };
        if file.language != language.as_str()
            || document
                .entities
                .iter()
                .any(|entity| entity.language != language.as_str())
            || document
                .provenance
                .iter()
                .any(|record| record.language != language.as_str())
        {
            return Err(ImportedContextError::LanguageMismatch);
        }
        if document
            .provenance
            .iter()
            .any(|record| record.build_context != build_context)
        {
            return Err(ImportedContextError::BuildContextMismatch);
        }
        if document
            .entities
            .iter()
            .any(|entity| tier_rank(entity.tier) > tier_rank(tier))
            || document
                .provenance
                .iter()
                .any(|record| tier_rank(record.tier) > tier_rank(tier))
            || document
                .coverage_records
                .iter()
                .any(|record| tier_rank(record.tier) > tier_rank(tier))
        {
            return Err(ImportedContextError::TierMismatch);
        }
        let covered_domains = document
            .coverage_records
            .iter()
            .filter_map(|record| {
                if record.scope == rootlight_ir::CoverageScope::File(file.id) {
                    Some(record.domain)
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        if covered_domains
            != [
                FactDomain::Files,
                FactDomain::Entities,
                FactDomain::Occurrences,
                FactDomain::Relations,
                FactDomain::Provenance,
                FactDomain::SourceMappings,
                FactDomain::Diagnostics,
                FactDomain::Extensions,
            ]
            .into_iter()
            .collect()
        {
            return Err(ImportedContextError::CoverageContractIncomplete);
        }

        let byte_length = usize::try_from(file.byte_length)
            .map_err(|_| ImportedContextError::SourceLengthOverflow)?;
        let coverage_status = aggregate_status(&document);
        let covered_source_bytes = if document.skipped_regions.is_empty() {
            byte_length
        } else {
            0
        };
        let domain_coverage = aggregate_domain_coverage(&document)?;
        CoverageReport::new(
            tier,
            coverage_status,
            byte_length,
            covered_source_bytes,
            document.skipped_regions.len(),
            domain_coverage.clone(),
        )
        .map_err(|_| ImportedContextError::InvalidCoverage)?;

        Ok(Self {
            language,
            tier,
            build_context,
            document,
            coverage_status,
            covered_source_bytes,
            domain_coverage,
        })
    }

    /// Returns the imported language identity.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }

    /// Returns the evidence tier supplied by this context.
    #[must_use]
    pub const fn tier(&self) -> AnalysisTier {
        self.tier
    }

    /// Returns the immutable declarative build identity.
    #[must_use]
    pub const fn build_context(&self) -> BuildContextIdentity {
        self.build_context
    }
}

/// Invalid imported semantic context.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportedContextError {
    /// Only compiler-assisted Tier A or evidence-backed Tier B is accepted.
    #[error("imported semantic context must declare Tier A or Tier B")]
    UnsupportedTier,
    /// The language identity was not a bounded SDK label.
    #[error("imported semantic context language is invalid")]
    InvalidLanguage,
    /// A context import is bound to exactly one analyzed file.
    #[error("imported semantic context must contain exactly one file")]
    SingleFileRequired,
    /// File, entity, or provenance language identity differed.
    #[error("imported semantic context language identities differ")]
    LanguageMismatch,
    /// Provenance named a different build-context identity.
    #[error("imported semantic context build identities differ")]
    BuildContextMismatch,
    /// A fact claimed evidence stronger than the context descriptor.
    #[error("imported semantic fact exceeds the context tier")]
    TierMismatch,
    /// The authoritative source length was not representable.
    #[error("imported semantic source length is not representable")]
    SourceLengthOverflow,
    /// Coverage records could not be represented or contradicted the report.
    #[error("imported semantic coverage is invalid")]
    InvalidCoverage,
    /// The file omitted one or more normalized fact-domain coverage records.
    #[error("imported semantic context must cover every normalized fact domain")]
    CoverageContractIncomplete,
    /// The normalized document failed common ownership or quota validation.
    #[error("imported semantic document is invalid")]
    InvalidDocument(#[source] rootlight_ir::IrDocumentValidationError),
}

/// Bounded analyzer that emits one validated compiler or declarative context.
#[derive(Debug, Clone)]
pub struct ContextAnalyzer {
    context: ImportedSemanticContext,
    profile: LanguageProfile,
    descriptor: ProducerDescriptor,
}

impl ContextAnalyzer {
    /// Creates a language-specific semantic context analyzer.
    ///
    /// # Errors
    ///
    /// Returns [`ContextAnalyzerConfigError`] when producer metadata is invalid.
    pub fn new(
        context: ImportedSemanticContext,
        profile: LanguageProfile,
        producer_kind: ProducerKind,
    ) -> Result<Self, ContextAnalyzerConfigError> {
        if context.language != profile.language {
            return Err(ContextAnalyzerConfigError::LanguageMismatch);
        }
        if tier_rank(context.tier) > tier_rank(profile.maximum_tier) {
            return Err(ContextAnalyzerConfigError::TierMismatch);
        }
        if !matches!(producer_kind, ProducerKind::Compiler | ProducerKind::Scip) {
            return Err(ContextAnalyzerConfigError::UnsupportedProducerKind);
        }
        let identity = ProducerIdentity::new(
            "rootlight-adapters",
            env!("CARGO_PKG_VERSION"),
            context.build_context.digest(),
        )
        .map_err(ContextAnalyzerConfigError::InvalidProducer)?;
        let descriptor = ProducerDescriptor::new(
            identity,
            producer_kind,
            context.language.clone(),
            context.tier,
            MemoryEnforcement::Unavailable,
            true,
        );
        Ok(Self {
            context,
            profile,
            descriptor,
        })
    }

    /// Returns the immutable language policy selected for this analyzer.
    #[must_use]
    pub const fn profile(&self) -> &LanguageProfile {
        &self.profile
    }

    fn validate_request(&self, request: &AnalysisRequest<'_>) -> Result<(), AdapterError> {
        let file = &self.context.document.files[0];
        let source = request.source().source_ref();
        let generated = request
            .generated_status()
            .ok_or(rootlight_adapter_sdk::RequestError::GeneratedStatusRequired)?;
        let source_length = u64::try_from(request.source().bytes().len())
            .map_err(|_| context_failure("context-source-length"))?;
        if request.build_context() != self.context.build_context
            || request.language() != &self.context.language
            || request.encoding().as_str() != file.encoding
            || generated != file.generated
            || source.repository() != self.context.document.repository
            || source.generation() != self.context.document.generation
            || source.span().file() != file.id
            || source.content_hash() != file.content_hash
            || source_length != file.byte_length
            || !request.included_ranges().is_empty()
        {
            return Err(context_failure("semantic-context-identity"));
        }
        Ok(())
    }
}

impl LanguageAnalyzer for ContextAnalyzer {
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
        self.validate_request(request)?;
        emit_records(
            document_records(&self.context.document),
            request,
            sink,
            cancellation,
        )?;
        cancellation.check()?;

        let usage = sink.staged_usage();
        let source_bytes = request.source().bytes().len();
        let coverage = CoverageReport::new(
            self.context.tier,
            self.context.coverage_status,
            source_bytes,
            self.context.covered_source_bytes,
            self.context.document.skipped_regions.len(),
            self.context.domain_coverage.clone(),
        )?;
        let resources = ResourceUsage::new(source_bytes, usage.records(), 0, 0, None, usage);
        rootlight_adapter_sdk::WorkReport::new(
            coverage,
            resources,
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}

/// Invalid configuration for [`ContextAnalyzer`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContextAnalyzerConfigError {
    /// The selected profile received another language's context.
    #[error("semantic context does not match the adapter language")]
    LanguageMismatch,
    /// Imported evidence exceeded the profile's promotion ceiling.
    #[error("semantic context exceeds the language profile tier")]
    TierMismatch,
    /// Context importers may represent compiler or SCIP evidence only.
    #[error("semantic context producer kind must be compiler or SCIP")]
    UnsupportedProducerKind,
    /// Producer labels did not satisfy normalized-IR constraints.
    #[error("semantic context producer identity is invalid")]
    InvalidProducer(#[source] IrValidationError),
}

fn document_records(document: &NormalizedIrDocument) -> Vec<IrRecord> {
    let mut records = Vec::new();
    records.extend(document.files.iter().cloned().map(IrRecord::File));
    records.extend(
        document
            .provenance
            .iter()
            .cloned()
            .map(IrRecord::Provenance),
    );
    records.extend(document.entities.iter().cloned().map(IrRecord::Entity));
    records.extend(
        document
            .occurrences
            .iter()
            .cloned()
            .map(IrRecord::Occurrence),
    );
    records.extend(document.relations.iter().cloned().map(IrRecord::Relation));
    records.extend(
        document
            .source_mappings
            .iter()
            .cloned()
            .map(IrRecord::SourceMapping),
    );
    records.extend(
        document
            .coverage_records
            .iter()
            .cloned()
            .map(IrRecord::Coverage),
    );
    records.extend(
        document
            .skipped_regions
            .iter()
            .cloned()
            .map(IrRecord::SkippedRegion),
    );
    records.extend(
        document
            .diagnostics
            .iter()
            .cloned()
            .map(IrRecord::Diagnostic),
    );
    records.extend(document.extensions.iter().cloned().map(IrRecord::Extension));
    records
}

fn aggregate_status(document: &NormalizedIrDocument) -> CoverageStatus {
    if !document.skipped_regions.is_empty() {
        return CoverageStatus::Bounded;
    }
    document
        .coverage_records
        .iter()
        .fold(CoverageStatus::Complete, |status, record| {
            merge_status(status, record.status)
        })
}

fn aggregate_domain_coverage(
    document: &NormalizedIrDocument,
) -> Result<Vec<DomainCoverage>, ImportedContextError> {
    let mut totals = BTreeMap::<FactDomain, (CoverageStatus, u64, u64, u64)>::new();
    for record in &document.coverage_records {
        let entry = totals
            .entry(record.domain)
            .or_insert((CoverageStatus::Complete, 0, 0, 0));
        entry.0 = merge_status(entry.0, record.status);
        entry.1 = entry
            .1
            .checked_add(record.discovered)
            .ok_or(ImportedContextError::InvalidCoverage)?;
        entry.2 = entry
            .2
            .checked_add(record.indexed)
            .ok_or(ImportedContextError::InvalidCoverage)?;
        entry.3 = entry
            .3
            .checked_add(record.skipped)
            .ok_or(ImportedContextError::InvalidCoverage)?;
    }
    totals
        .into_iter()
        .map(|(domain, (status, discovered, indexed, skipped))| {
            DomainCoverage::new(
                domain,
                status,
                usize::try_from(discovered).map_err(|_| ImportedContextError::InvalidCoverage)?,
                usize::try_from(indexed).map_err(|_| ImportedContextError::InvalidCoverage)?,
                usize::try_from(skipped).map_err(|_| ImportedContextError::InvalidCoverage)?,
            )
            .map_err(|_| ImportedContextError::InvalidCoverage)
        })
        .collect()
}

fn merge_status(left: CoverageStatus, right: CoverageStatus) -> CoverageStatus {
    if left == CoverageStatus::Unknown || right == CoverageStatus::Unknown {
        CoverageStatus::Unknown
    } else if left == CoverageStatus::Sampled || right == CoverageStatus::Sampled {
        CoverageStatus::Sampled
    } else if left == CoverageStatus::Bounded || right == CoverageStatus::Bounded {
        CoverageStatus::Bounded
    } else {
        CoverageStatus::Complete
    }
}

const fn tier_rank(tier: AnalysisTier) -> u8 {
    match tier {
        AnalysisTier::TierA => 4,
        AnalysisTier::TierB => 3,
        AnalysisTier::TierC => 2,
        AnalysisTier::TierD => 1,
        _ => 0,
    }
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
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
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

fn usage_fits(usage: StreamUsage, budget: RemainingBudget) -> bool {
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

fn context_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("built-in context failure code is valid"),
    }
}
