//! Checked coverage, resource, and explicit stream-completion reports.
//!
//! A successful report is the transaction commit boundary: staged facts remain
//! invisible until its usage and end marker match the sink exactly.

use rootlight_ir::{AnalysisTier, CoverageStatus, FactDomain};

use crate::{
    error::{ReportError, ResourceKind},
    limits::{AnalysisLimits, StreamUsage},
};

// `FactDomain` is a closed eight-variant contract, so a longer input can only
// contain duplicates and is rejected before sorting.
const MAX_COVERAGE_DOMAINS: usize = 8;

/// Coverage counts for one normalized fact domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainCoverage {
    domain: FactDomain,
    status: CoverageStatus,
    discovered: usize,
    indexed: usize,
    skipped: usize,
}

impl DomainCoverage {
    /// Creates checked fact-domain coverage.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError`] when indexed and skipped counts exceed discovered
    /// work or when complete status contradicts the counts.
    pub fn new(
        domain: FactDomain,
        status: CoverageStatus,
        discovered: usize,
        indexed: usize,
        skipped: usize,
    ) -> Result<Self, ReportError> {
        let accounted = indexed
            .checked_add(skipped)
            .ok_or(ReportError::InvalidDomainCoverage)?;
        if accounted > discovered {
            return Err(ReportError::InvalidDomainCoverage);
        }
        if status == CoverageStatus::Complete && (indexed != discovered || skipped != 0) {
            return Err(ReportError::InvalidDomainCoverage);
        }
        Ok(Self {
            domain,
            status,
            discovered,
            indexed,
            skipped,
        })
    }

    /// Returns the covered fact domain.
    #[must_use]
    pub const fn domain(&self) -> FactDomain {
        self.domain
    }

    /// Returns declared domain completeness.
    #[must_use]
    pub const fn status(&self) -> CoverageStatus {
        self.status
    }

    /// Returns discovered work units.
    #[must_use]
    pub const fn discovered(&self) -> usize {
        self.discovered
    }

    /// Returns indexed work units.
    #[must_use]
    pub const fn indexed(&self) -> usize {
        self.indexed
    }

    /// Returns explicitly skipped work units.
    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.skipped
    }
}

/// Per-file byte and fact-domain coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    tier: AnalysisTier,
    status: CoverageStatus,
    total_source_bytes: usize,
    covered_source_bytes: usize,
    skipped_regions: usize,
    domains: Vec<DomainCoverage>,
}

impl CoverageReport {
    /// Creates checked per-file coverage.
    ///
    /// Domain entries are sorted by domain so equivalent reports are independent
    /// of adapter emission order.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError`] for out-of-bounds coverage, duplicate domain
    /// entries, or complete status with uncovered or skipped work.
    pub fn new(
        tier: AnalysisTier,
        status: CoverageStatus,
        total_source_bytes: usize,
        covered_source_bytes: usize,
        skipped_regions: usize,
        mut domains: Vec<DomainCoverage>,
    ) -> Result<Self, ReportError> {
        if covered_source_bytes > total_source_bytes {
            return Err(ReportError::CoverageOutOfBounds {
                covered: covered_source_bytes,
                total: total_source_bytes,
            });
        }
        if status == CoverageStatus::Complete
            && (covered_source_bytes != total_source_bytes || skipped_regions != 0)
        {
            return Err(ReportError::InvalidCompleteCoverage);
        }
        if domains.len() > MAX_COVERAGE_DOMAINS {
            return Err(ReportError::InvalidDomainCoverage);
        }
        domains.sort_by_key(|coverage| coverage.domain);
        if domains
            .windows(2)
            .any(|pair| pair[0].domain == pair[1].domain)
        {
            return Err(ReportError::InvalidDomainCoverage);
        }
        Ok(Self {
            tier,
            status,
            total_source_bytes,
            covered_source_bytes,
            skipped_regions,
            domains,
        })
    }

    /// Returns the analysis tier supporting this coverage.
    #[must_use]
    pub const fn tier(&self) -> AnalysisTier {
        self.tier
    }

    /// Returns overall coverage completeness.
    #[must_use]
    pub const fn status(&self) -> CoverageStatus {
        self.status
    }

    /// Returns the authoritative full-file byte length.
    #[must_use]
    pub const fn total_source_bytes(&self) -> usize {
        self.total_source_bytes
    }

    /// Returns source bytes included in analysis.
    #[must_use]
    pub const fn covered_source_bytes(&self) -> usize {
        self.covered_source_bytes
    }

    /// Returns the number of explicitly skipped regions.
    #[must_use]
    pub const fn skipped_regions(&self) -> usize {
        self.skipped_regions
    }

    /// Returns sorted per-domain coverage.
    #[must_use]
    pub fn domains(&self) -> &[DomainCoverage] {
        &self.domains
    }
}

/// Deterministic resource use reported for one adapter invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceUsage {
    source_bytes: usize,
    output_records: usize,
    syntax_nodes: usize,
    max_syntax_depth: usize,
    reported_memory_bytes: Option<usize>,
    stream: StreamUsage,
}

impl ResourceUsage {
    /// Creates an exact invocation resource report.
    #[must_use]
    pub const fn new(
        source_bytes: usize,
        output_records: usize,
        syntax_nodes: usize,
        max_syntax_depth: usize,
        reported_memory_bytes: Option<usize>,
        stream: StreamUsage,
    ) -> Self {
        Self {
            source_bytes,
            output_records,
            syntax_nodes,
            max_syntax_depth,
            reported_memory_bytes,
            stream,
        }
    }

    /// Returns source bytes observed by the adapter.
    #[must_use]
    pub const fn source_bytes(self) -> usize {
        self.source_bytes
    }

    /// Returns output facts before canonical deduplication.
    #[must_use]
    pub const fn output_records(self) -> usize {
        self.output_records
    }

    /// Returns concrete-syntax nodes observed independently of emitted facts.
    #[must_use]
    pub const fn syntax_nodes(self) -> usize {
        self.syntax_nodes
    }

    /// Returns the deepest syntax nesting level observed.
    #[must_use]
    pub const fn max_syntax_depth(self) -> usize {
        self.max_syntax_depth
    }

    /// Returns the cooperative provider's reported working-memory counter.
    ///
    /// The adapter authors this post-hoc counter, so it is not proof against a
    /// malicious or noncooperative provider.
    #[must_use]
    pub const fn reported_memory_bytes(self) -> Option<usize> {
        self.reported_memory_bytes
    }

    /// Returns exact output stream usage.
    #[must_use]
    pub const fn stream(self) -> StreamUsage {
        self.stream
    }
}

/// Explicit successful end-of-stream marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamEnd {
    next_sequence: u64,
    usage: StreamUsage,
}

impl StreamEnd {
    /// Marks successful completion after the supplied contiguous sequences.
    #[must_use]
    pub const fn new(next_sequence: u64, usage: StreamUsage) -> Self {
        Self {
            next_sequence,
            usage,
        }
    }

    /// Returns the sequence that would follow the final batch.
    #[must_use]
    pub const fn next_sequence(self) -> u64 {
        self.next_sequence
    }

    /// Returns usage covered by the end marker.
    #[must_use]
    pub const fn usage(self) -> StreamUsage {
        self.usage
    }
}

/// Successful adapter report used as the transactional commit boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkReport {
    coverage: CoverageReport,
    resources: ResourceUsage,
    end: StreamEnd,
}

impl WorkReport {
    /// Creates a self-consistent successful report.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError::StreamUsageMismatch`] when the resource report and
    /// explicit end marker describe different streams.
    pub fn new(
        coverage: CoverageReport,
        resources: ResourceUsage,
        end: StreamEnd,
    ) -> Result<Self, ReportError> {
        if resources.stream != end.usage {
            return Err(ReportError::StreamUsageMismatch);
        }
        Ok(Self {
            coverage,
            resources,
            end,
        })
    }

    /// Returns coverage declared by the provider.
    #[must_use]
    pub const fn coverage(&self) -> &CoverageReport {
        &self.coverage
    }

    /// Returns exact resource usage.
    #[must_use]
    pub const fn resources(&self) -> ResourceUsage {
        self.resources
    }

    /// Returns the explicit successful end marker.
    #[must_use]
    pub const fn end(&self) -> StreamEnd {
        self.end
    }

    pub(crate) fn validate_commit(
        &self,
        source_bytes: usize,
        limits: &AnalysisLimits,
        staged_usage: StreamUsage,
        next_sequence: u64,
    ) -> Result<(), ReportError> {
        if self.coverage.total_source_bytes != source_bytes {
            return Err(ReportError::SourceLengthMismatch {
                expected: source_bytes,
                observed: self.coverage.total_source_bytes,
            });
        }
        if self.resources.source_bytes != source_bytes {
            return Err(ReportError::SourceLengthMismatch {
                expected: source_bytes,
                observed: self.resources.source_bytes,
            });
        }
        if self.resources.output_records != staged_usage.records()
            || self.resources.stream != staged_usage
            || self.end.usage != staged_usage
        {
            return Err(ReportError::StreamUsageMismatch);
        }
        if self.end.next_sequence != next_sequence {
            return Err(ReportError::EndSequenceMismatch {
                expected: next_sequence,
                observed: self.end.next_sequence,
            });
        }
        require_at_most(
            ResourceKind::SourceBytes,
            self.resources.source_bytes,
            limits.max_source_bytes(),
        )?;
        require_at_most(
            ResourceKind::SyntaxNodes,
            self.resources.syntax_nodes,
            limits.max_syntax_nodes(),
        )?;
        require_at_most(
            ResourceKind::SyntaxDepth,
            self.resources.max_syntax_depth,
            limits.max_syntax_depth(),
        )?;
        if let Some(observed) = self.resources.reported_memory_bytes {
            require_at_most(
                ResourceKind::ReportedMemoryBytes,
                observed,
                limits.max_reported_memory_bytes(),
            )?;
        }
        Ok(())
    }
}

/// Successful parser report whose validation commits staged syntax facts.
pub type ParseReport = WorkReport;

/// Successful analyzer report whose validation commits staged normalized IR.
pub type AnalysisReport = WorkReport;

fn require_at_most(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<(), ReportError> {
    if observed > limit {
        Err(ReportError::ResourceLimit {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}
