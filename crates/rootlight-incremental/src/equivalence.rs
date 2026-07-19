//! Clean-index equivalence fingerprints and deterministic mismatch reports.
//!
//! Callers supply canonical logical projections with generation-local physical
//! details removed; this module hashes them cooperatively under a byte ceiling.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use serde::Serialize;

use crate::{IncrementalError, ResourceKind};

/// Hard ceiling for one canonical logical component.
pub(crate) const HARD_MAX_LOGICAL_COMPONENT_BYTES: usize = 256 * 1024 * 1024;
const HASH_CHECKPOINT_BYTES: usize = 64 * 1024;
const LOGICAL_HASH_CONTEXT: &[u8] = b"rootlight.incremental.logical/1";

/// Mandatory logical projections compared after every incremental publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalDomain {
    /// Canonical normalized discovery output.
    Discovery,
    /// Canonical normalized IR with physical generation identity removed.
    NormalizedIr,
    /// Backend-neutral logical store contents.
    LogicalStore,
    /// Canonically ordered mandatory query-corpus outputs.
    QueryOutputs,
    /// Coverage and freshness semantics.
    Coverage,
    /// Fact and derivation provenance.
    Provenance,
    /// Current stable semantic identities and explicit lineage.
    StableIds,
}

impl LogicalDomain {
    const ALL: [Self; 7] = [
        Self::Discovery,
        Self::NormalizedIr,
        Self::LogicalStore,
        Self::QueryOutputs,
        Self::Coverage,
        Self::Provenance,
        Self::StableIds,
    ];

    const fn discriminator(self) -> u8 {
        match self {
            Self::Discovery => 1,
            Self::NormalizedIr => 2,
            Self::LogicalStore => 3,
            Self::QueryOutputs => 4,
            Self::Coverage => 5,
            Self::Provenance => 6,
            Self::StableIds => 7,
        }
    }
}

/// Digest and record count for one canonical logical projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalComponent {
    domain: LogicalDomain,
    digest: ContentHash,
    records: u64,
}

impl LogicalComponent {
    /// Hashes one canonical logical projection with domain separation.
    ///
    /// The byte representation must exclude physical row IDs, segment ordinals,
    /// active-generation IDs, timestamps, and other values that may legitimately
    /// differ between clean and incremental construction.
    ///
    /// # Errors
    ///
    /// Returns a byte-limit or cancellation error.
    pub fn from_canonical_bytes(
        domain: LogicalDomain,
        canonical_bytes: &[u8],
        records: u64,
        max_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        if max_bytes == 0 || max_bytes > HARD_MAX_LOGICAL_COMPONENT_BYTES {
            return Err(IncrementalError::InvalidLimit {
                resource: ResourceKind::LogicalBytes,
                value: max_bytes,
                hard_maximum: HARD_MAX_LOGICAL_COMPONENT_BYTES,
            });
        }
        if canonical_bytes.len() > max_bytes {
            return Err(IncrementalError::ResourceLimit {
                resource: ResourceKind::LogicalBytes,
                observed: canonical_bytes.len(),
                limit: max_bytes,
            });
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(LOGICAL_HASH_CONTEXT);
        hasher.update(&[domain.discriminator()]);
        for chunk in canonical_bytes.chunks(HASH_CHECKPOINT_BYTES) {
            cancellation.check()?;
            hasher.update(chunk);
        }
        cancellation.check()?;
        Ok(Self {
            domain,
            digest: ContentHash::from_bytes(*hasher.finalize().as_bytes()),
            records,
        })
    }

    /// Returns the logical projection domain.
    #[must_use]
    pub const fn domain(self) -> LogicalDomain {
        self.domain
    }

    /// Returns the domain-separated canonical digest.
    #[must_use]
    pub const fn digest(self) -> ContentHash {
        self.digest
    }

    /// Returns the canonical logical record count.
    #[must_use]
    pub const fn records(self) -> u64 {
        self.records
    }
}

/// Complete mandatory clean-equivalence snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquivalenceSnapshot {
    components: BTreeMap<LogicalDomain, LogicalComponent>,
}

impl EquivalenceSnapshot {
    /// Creates a complete canonical snapshot containing every mandatory domain.
    ///
    /// # Errors
    ///
    /// Returns a duplicate, missing-domain, or cancellation error.
    pub fn new(
        components: impl IntoIterator<Item = LogicalComponent>,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical = BTreeMap::new();
        for component in components {
            cancellation.check()?;
            if canonical.insert(component.domain(), component).is_some() {
                return Err(IncrementalError::DuplicateLogicalDomain {
                    domain: component.domain(),
                });
            }
        }
        for domain in LogicalDomain::ALL {
            if !canonical.contains_key(&domain) {
                return Err(IncrementalError::MissingLogicalDomain { domain });
            }
        }
        Ok(Self {
            components: canonical,
        })
    }

    /// Compares incremental output with a clean build in canonical domain order.
    ///
    /// # Errors
    ///
    /// Returns cancellation when comparison is interrupted.
    pub fn compare_clean(
        &self,
        clean: &Self,
        cancellation: &Cancellation,
    ) -> Result<EquivalenceReport, IncrementalError> {
        let mut mismatches = Vec::new();
        for domain in LogicalDomain::ALL {
            cancellation.check()?;
            let incremental = self
                .components
                .get(&domain)
                .copied()
                .ok_or(IncrementalError::MissingLogicalDomain { domain })?;
            let clean = clean
                .components
                .get(&domain)
                .copied()
                .ok_or(IncrementalError::MissingLogicalDomain { domain })?;
            if incremental != clean {
                mismatches.push(EquivalenceMismatch {
                    domain,
                    incremental_digest: incremental.digest(),
                    clean_digest: clean.digest(),
                    incremental_records: incremental.records(),
                    clean_records: clean.records(),
                });
            }
        }
        Ok(EquivalenceReport { mismatches })
    }
}

/// One deterministic logical inequality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceMismatch {
    domain: LogicalDomain,
    incremental_digest: ContentHash,
    clean_digest: ContentHash,
    incremental_records: u64,
    clean_records: u64,
}

impl EquivalenceMismatch {
    /// Returns the mismatched logical domain.
    #[must_use]
    pub const fn domain(self) -> LogicalDomain {
        self.domain
    }

    /// Returns the incremental projection digest.
    #[must_use]
    pub const fn incremental_digest(self) -> ContentHash {
        self.incremental_digest
    }

    /// Returns the clean-build projection digest.
    #[must_use]
    pub const fn clean_digest(self) -> ContentHash {
        self.clean_digest
    }

    /// Returns the incremental logical record count.
    #[must_use]
    pub const fn incremental_records(self) -> u64 {
        self.incremental_records
    }

    /// Returns the clean-build logical record count.
    #[must_use]
    pub const fn clean_records(self) -> u64 {
        self.clean_records
    }
}

/// Result of comparing an incremental snapshot with a clean snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquivalenceReport {
    mismatches: Vec<EquivalenceMismatch>,
}

impl EquivalenceReport {
    /// Reports whether all mandatory logical projections are exactly equal.
    #[must_use]
    pub fn is_equivalent(&self) -> bool {
        self.mismatches.is_empty()
    }

    /// Returns mismatches in canonical domain order.
    #[must_use]
    pub fn mismatches(&self) -> &[EquivalenceMismatch] {
        &self.mismatches
    }

    /// Converts any inequality into the contract's hard-stop error.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::LogicalInequality`] when any domain differs.
    pub fn require_equivalent(&self) -> Result<(), IncrementalError> {
        if self.is_equivalent() {
            Ok(())
        } else {
            Err(IncrementalError::LogicalInequality)
        }
    }
}
