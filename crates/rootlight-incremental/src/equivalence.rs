//! Clean-index equivalence fingerprints and deterministic mismatch reports.
//!
//! Callers supply canonical logical projections with generation-local physical
//! details removed; this module hashes them cooperatively under a byte ceiling.

use std::collections::BTreeMap;
use std::io::Write as _;

use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use serde::Serialize;

use crate::{IncrementalError, ResourceKind};

/// Hard ceiling for one canonical logical component.
pub(crate) const HARD_MAX_LOGICAL_COMPONENT_BYTES: usize = 256 * 1024 * 1024;
const HARD_MAX_STREAMED_LOGICAL_COMPONENT_BYTES: usize = 16 * 1024 * 1024 * 1024;
const HASH_CHECKPOINT_BYTES: usize = 64 * 1024;
const LOGICAL_HASH_CONTEXT: &[u8] = b"rootlight.incremental.logical/1";
const LOGICAL_SNAPSHOT_HASH_CONTEXT: &[u8] = b"rootlight.incremental.snapshot/1";

/// Version of the complete generation-neutral logical snapshot projection.
pub const LOGICAL_SNAPSHOT_SCHEMA_VERSION: &str = "1.0";

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

    /// Streams one canonical logical projection into its domain-separated digest.
    ///
    /// # Errors
    ///
    /// Returns a byte-limit, serialization, or cancellation error.
    pub fn from_canonical_value(
        domain: LogicalDomain,
        value: &impl Serialize,
        records: u64,
        max_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        validate_streamed_limit(max_bytes)?;
        let mut writer = SingleLogicalHashWriter::new(domain, max_bytes, cancellation);
        if let Err(error) = serde_json::to_writer(&mut writer, value) {
            if let Some(failure) = writer.failure.take() {
                return Err(failure);
            }
            return Err(IncrementalError::SerializeTrace(error));
        }
        writer.finish(records)
    }

    /// Streams one canonical logical projection into two domain-separated digests.
    ///
    /// This path avoids materializing canonical JSON and permits a complete
    /// already-admitted generation to exceed the in-memory byte-slice ceiling.
    /// The second domain allows callers to bind one canonical projection to two
    /// logical contracts without serializing it twice.
    ///
    /// # Errors
    ///
    /// Returns a byte-limit, serialization, or cancellation error.
    pub fn pair_from_canonical_value(
        first_domain: LogicalDomain,
        second_domain: LogicalDomain,
        value: &impl Serialize,
        records: u64,
        max_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<(Self, Self), IncrementalError> {
        validate_streamed_limit(max_bytes)?;
        if first_domain == second_domain {
            return Err(IncrementalError::DuplicateLogicalDomain {
                domain: first_domain,
            });
        }
        let mut writer =
            LogicalHashWriter::new(first_domain, second_domain, max_bytes, cancellation);
        if let Err(error) = serde_json::to_writer(&mut writer, value) {
            if let Some(failure) = writer.failure.take() {
                return Err(failure);
            }
            return Err(IncrementalError::SerializeTrace(error));
        }
        writer.finish(records)
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

/// Incrementally hashes one canonical JSON sequence without retaining its items.
///
/// Pushing the same values in the same order produces the exact component
/// digest as [`LogicalComponent::from_canonical_value`] over their slice.
pub struct LogicalSequenceBuilder<'cancellation> {
    writer: SingleLogicalHashWriter<'cancellation>,
    records: u64,
}

impl<'cancellation> LogicalSequenceBuilder<'cancellation> {
    /// Starts an empty canonical sequence under the streamed logical byte cap.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError`] for an invalid byte limit, cancellation, or
    /// an unavailable output boundary.
    pub fn new(
        domain: LogicalDomain,
        max_bytes: usize,
        cancellation: &'cancellation Cancellation,
    ) -> Result<Self, IncrementalError> {
        validate_streamed_limit(max_bytes)?;
        cancellation.check()?;
        let mut builder = Self {
            writer: SingleLogicalHashWriter::new(domain, max_bytes, cancellation),
            records: 0,
        };
        builder.write_delimiter(b"[")?;
        Ok(builder)
    }

    /// Appends one canonical sequence item.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError`] when serialization, cancellation, record
    /// accounting, or the logical byte cap prevents a complete item.
    pub fn push(&mut self, value: &impl Serialize) -> Result<(), IncrementalError> {
        self.writer.cancellation.check()?;
        if self.records != 0 {
            self.write_delimiter(b",")?;
        }
        if let Err(error) = serde_json::to_writer(&mut self.writer, value) {
            return Err(self
                .writer
                .failure
                .take()
                .unwrap_or(IncrementalError::SerializeTrace(error)));
        }
        self.records = self
            .records
            .checked_add(1)
            .ok_or(IncrementalError::ResourceLimit {
                resource: ResourceKind::LogicalBytes,
                observed: usize::MAX,
                limit: self.writer.max_bytes,
            })?;
        Ok(())
    }

    /// Closes the canonical sequence and returns its exact component digest.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError`] for cancellation or a byte-limit failure.
    pub fn finish(mut self) -> Result<LogicalComponent, IncrementalError> {
        self.write_delimiter(b"]")?;
        self.writer.finish(self.records)
    }

    fn write_delimiter(&mut self, delimiter: &[u8]) -> Result<(), IncrementalError> {
        if let Err(error) = self.writer.write_all(delimiter) {
            return Err(self.writer.failure.take().unwrap_or_else(|| {
                IncrementalError::SerializeTrace(serde_json::Error::io(error))
            }));
        }
        Ok(())
    }
}

fn validate_streamed_limit(max_bytes: usize) -> Result<(), IncrementalError> {
    if max_bytes == 0 || max_bytes > HARD_MAX_STREAMED_LOGICAL_COMPONENT_BYTES {
        return Err(IncrementalError::InvalidLimit {
            resource: ResourceKind::LogicalBytes,
            value: max_bytes,
            hard_maximum: HARD_MAX_STREAMED_LOGICAL_COMPONENT_BYTES,
        });
    }
    Ok(())
}

struct SingleLogicalHashWriter<'cancellation> {
    domain: LogicalDomain,
    hasher: blake3::Hasher,
    bytes: usize,
    bytes_since_checkpoint: usize,
    max_bytes: usize,
    cancellation: &'cancellation Cancellation,
    failure: Option<IncrementalError>,
}

impl<'cancellation> SingleLogicalHashWriter<'cancellation> {
    fn new(
        domain: LogicalDomain,
        max_bytes: usize,
        cancellation: &'cancellation Cancellation,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(LOGICAL_HASH_CONTEXT);
        hasher.update(&[domain.discriminator()]);
        Self {
            domain,
            hasher,
            bytes: 0,
            bytes_since_checkpoint: 0,
            max_bytes,
            cancellation,
            failure: None,
        }
    }

    fn finish(self, records: u64) -> Result<LogicalComponent, IncrementalError> {
        self.cancellation.check()?;
        Ok(LogicalComponent {
            domain: self.domain,
            digest: ContentHash::from_bytes(*self.hasher.finalize().as_bytes()),
            records,
        })
    }
}

impl std::io::Write for SingleLogicalHashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let observed = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("logical component byte accounting overflowed"))?;
        if observed > self.max_bytes {
            self.failure = Some(IncrementalError::ResourceLimit {
                resource: ResourceKind::LogicalBytes,
                observed,
                limit: self.max_bytes,
            });
            return Err(std::io::Error::other(
                "logical component byte limit exceeded",
            ));
        }
        self.hasher.update(bytes);
        self.bytes = observed;
        self.bytes_since_checkpoint = self
            .bytes_since_checkpoint
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("logical checkpoint accounting overflowed"))?;
        if self.bytes_since_checkpoint >= HASH_CHECKPOINT_BYTES {
            if let Err(cancelled) = self.cancellation.check() {
                self.failure = Some(IncrementalError::Cancelled(cancelled));
                return Err(std::io::Error::other(
                    "logical component hashing was cancelled",
                ));
            }
            self.bytes_since_checkpoint %= HASH_CHECKPOINT_BYTES;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct LogicalHashWriter<'cancellation> {
    first_domain: LogicalDomain,
    second_domain: LogicalDomain,
    first: blake3::Hasher,
    second: blake3::Hasher,
    bytes: usize,
    bytes_since_checkpoint: usize,
    max_bytes: usize,
    cancellation: &'cancellation Cancellation,
    failure: Option<IncrementalError>,
}

impl<'cancellation> LogicalHashWriter<'cancellation> {
    fn new(
        first_domain: LogicalDomain,
        second_domain: LogicalDomain,
        max_bytes: usize,
        cancellation: &'cancellation Cancellation,
    ) -> Self {
        let mut first = blake3::Hasher::new();
        first.update(LOGICAL_HASH_CONTEXT);
        first.update(&[first_domain.discriminator()]);
        let mut second = blake3::Hasher::new();
        second.update(LOGICAL_HASH_CONTEXT);
        second.update(&[second_domain.discriminator()]);
        Self {
            first_domain,
            second_domain,
            first,
            second,
            bytes: 0,
            bytes_since_checkpoint: 0,
            max_bytes,
            cancellation,
            failure: None,
        }
    }

    fn finish(
        self,
        records: u64,
    ) -> Result<(LogicalComponent, LogicalComponent), IncrementalError> {
        self.cancellation.check()?;
        Ok((
            LogicalComponent {
                domain: self.first_domain,
                digest: ContentHash::from_bytes(*self.first.finalize().as_bytes()),
                records,
            },
            LogicalComponent {
                domain: self.second_domain,
                digest: ContentHash::from_bytes(*self.second.finalize().as_bytes()),
                records,
            },
        ))
    }
}

impl std::io::Write for LogicalHashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let observed = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("logical component byte accounting overflowed"))?;
        if observed > self.max_bytes {
            self.failure = Some(IncrementalError::ResourceLimit {
                resource: ResourceKind::LogicalBytes,
                observed,
                limit: self.max_bytes,
            });
            return Err(std::io::Error::other(
                "logical component byte limit exceeded",
            ));
        }
        self.first.update(bytes);
        self.second.update(bytes);
        self.bytes = observed;
        self.bytes_since_checkpoint = self
            .bytes_since_checkpoint
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("logical checkpoint accounting overflowed"))?;
        if self.bytes_since_checkpoint >= HASH_CHECKPOINT_BYTES {
            if let Err(cancelled) = self.cancellation.check() {
                self.failure = Some(IncrementalError::Cancelled(cancelled));
                return Err(std::io::Error::other(
                    "logical component hashing was cancelled",
                ));
            }
            self.bytes_since_checkpoint %= HASH_CHECKPOINT_BYTES;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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

    /// Returns one digest that binds every mandatory logical component.
    ///
    /// The aggregate preserves the canonical domain order and binds both each
    /// component digest and record count. Callers must compare hashes only
    /// when their [`LOGICAL_SNAPSHOT_SCHEMA_VERSION`] values are equal.
    ///
    /// # Panics
    ///
    /// Panics only if this snapshot's construction invariant is violated and
    /// one of the seven mandatory logical domains is missing.
    #[must_use]
    pub fn logical_snapshot_hash(&self) -> ContentHash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(LOGICAL_SNAPSHOT_HASH_CONTEXT);
        for domain in LogicalDomain::ALL {
            let component = self
                .components
                .get(&domain)
                .expect("complete snapshots retain every mandatory domain");
            hasher.update(&[domain.discriminator()]);
            hasher.update(component.digest().as_bytes());
            hasher.update(&component.records().to_be_bytes());
        }
        ContentHash::from_bytes(*hasher.finalize().as_bytes())
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
