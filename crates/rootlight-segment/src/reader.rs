//! Backend-neutral indexed reads over one verified owned-byte segment.
//!
//! In-memory indexes contain generation-local ordinals, while every public
//! page is ordered and resumed exclusively through stable fact identifiers.

use std::{collections::BTreeSet, fmt, sync::Arc};

use rootlight_ids::{FactId, FileId, SymbolId};
use rootlight_ir::{
    CoverageRecord, EntityRecord, FactEvidence, FileRecord, NormalizedIrDocument, OccurrenceRecord,
    OccurrenceTarget, ProvenanceRecord, RelationRecord, SourceRef,
};
use rootlight_storage::{
    CoverageReadRequest, GenerationContext, GenerationMetadata, GenerationReader,
    GenerationResource, GenerationStats, IdentityVerificationError, IdentityVerifiedGeneration,
    OccurrenceReadRequest, ReadPage, RelationReadDirection, RelationReadRequest,
};

use crate::{
    SegmentError,
    format::{self, SegmentIndexes},
};

/// Read-only owner of one defensively decoded in-memory segment.
pub struct SegmentReader {
    bytes: Arc<[u8]>,
    snapshot: rootlight_storage::GenerationSnapshot,
    stats: GenerationStats,
    indexes: SegmentIndexes,
}

impl SegmentReader {
    /// Opens, checksums, decodes, canonicalizes, and identity-verifies bytes.
    ///
    /// The reader takes shared ownership of the complete immutable byte slice.
    /// It does not resolve paths, map files, publish generations, or interpret
    /// completion markers.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentError`] for cancellation, resource exhaustion,
    /// unsupported versions, malformed bounds, checksum failures, noncanonical
    /// sections, divergent indexes, invalid IR, or invalid stable identities.
    pub fn open(
        bytes: Arc<[u8]>,
        limits: &rootlight_ir::IrLimits,
        extensions: &rootlight_ir::ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, SegmentError> {
        let decoded = format::decode(bytes, limits, extensions, context)?;
        Ok(Self {
            bytes: decoded.bytes,
            snapshot: decoded.snapshot,
            stats: decoded.stats,
            indexes: decoded.indexes,
        })
    }

    /// Returns the complete encoded byte length retained by this reader.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns another shared owner of the exact validated bytes.
    #[must_use]
    pub fn encoded_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }

    fn document(&self) -> &NormalizedIrDocument {
        self.snapshot.document()
    }
}

impl fmt::Debug for SegmentReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SegmentReader")
            .field("metadata", &self.snapshot.metadata())
            .field("stats", &self.stats)
            .field("encoded_len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl GenerationReader for SegmentReader {
    type Error = SegmentError;

    fn metadata(&self) -> GenerationMetadata {
        self.snapshot.metadata()
    }

    fn stats(&self) -> GenerationStats {
        self.stats
    }

    fn file(
        &self,
        id: FileId,
        context: &GenerationContext<'_>,
    ) -> Result<Option<FileRecord>, Self::Error> {
        context.check()?;
        let Some(ordinal) = self.indexes.files.get(&id).copied() else {
            return Ok(None);
        };
        let record = self
            .document()
            .files
            .get(ordinal)
            .ok_or(SegmentError::Corrupt)?;
        let mut meter = ReadMeter::new(context)?;
        meter.file(record)?;
        Ok(Some(record.clone()))
    }

    fn entity(
        &self,
        id: SymbolId,
        context: &GenerationContext<'_>,
    ) -> Result<Option<EntityRecord>, Self::Error> {
        context.check()?;
        let Some(ordinal) = self.indexes.entities.get(&id).copied() else {
            return Ok(None);
        };
        let record = self
            .document()
            .entities
            .get(ordinal)
            .ok_or(SegmentError::Corrupt)?;
        let mut meter = ReadMeter::new(context)?;
        meter.entity(record)?;
        Ok(Some(record.clone()))
    }

    fn relations(
        &self,
        request: &RelationReadRequest,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<RelationRecord>, Self::Error> {
        context.check()?;
        let ordinals = match request.direction() {
            RelationReadDirection::Outgoing => {
                self.indexes.outgoing_relations.get(&request.anchor())
            }
            RelationReadDirection::Incoming => {
                self.indexes.incoming_relations.get(&request.anchor())
            }
        };
        let mut meter = ReadMeter::new(context)?;
        let mut total = 0_u64;
        let mut records = probe_buffer(request.limit().get())?;
        if let Some(ordinals) = ordinals {
            for ordinal in ordinals {
                context.check()?;
                let record = self
                    .document()
                    .relations
                    .get(*ordinal)
                    .ok_or(SegmentError::Corrupt)?;
                if !request.predicates().is_empty()
                    && request
                        .predicates()
                        .binary_search(&record.predicate)
                        .is_err()
                {
                    continue;
                }
                total = total.checked_add(1).ok_or(SegmentError::Corrupt)?;
                if request.after().is_none_or(|after| record.id > after)
                    && records.len() <= usize::from(request.limit().get())
                {
                    meter.relation(record)?;
                    records.push(record.clone());
                }
            }
        }
        ReadPage::from_limit_plus_one(records, total, request.limit(), |record| record.id)
            .map_err(SegmentError::from)
    }

    fn occurrences(
        &self,
        request: &OccurrenceReadRequest,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<OccurrenceRecord>, Self::Error> {
        context.check()?;
        let mut meter = ReadMeter::new(context)?;
        let mut total = 0_u64;
        let mut records = probe_buffer(request.limit().get())?;
        if let Some(ordinals) = self.indexes.occurrences.get(&request.file()) {
            for ordinal in ordinals {
                context.check()?;
                let record = self
                    .document()
                    .occurrences
                    .get(*ordinal)
                    .ok_or(SegmentError::Corrupt)?;
                total = total.checked_add(1).ok_or(SegmentError::Corrupt)?;
                if request.after().is_none_or(|after| record.id > after)
                    && records.len() <= usize::from(request.limit().get())
                {
                    meter.occurrence(record)?;
                    records.push(record.clone());
                }
            }
        }
        ReadPage::from_limit_plus_one(records, total, request.limit(), |record| record.id)
            .map_err(SegmentError::from)
    }

    fn provenance(
        &self,
        id: FactId,
        context: &GenerationContext<'_>,
    ) -> Result<Option<ProvenanceRecord>, Self::Error> {
        context.check()?;
        let Some(ordinal) = self.indexes.provenance.get(&id).copied() else {
            return Ok(None);
        };
        let record = self
            .document()
            .provenance
            .get(ordinal)
            .ok_or(SegmentError::Corrupt)?;
        let mut meter = ReadMeter::new(context)?;
        meter.provenance(record)?;
        Ok(Some(record.clone()))
    }

    fn coverage(
        &self,
        request: &CoverageReadRequest,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<CoverageRecord>, Self::Error> {
        context.check()?;
        let mut meter = ReadMeter::new(context)?;
        let mut total = 0_u64;
        let mut records = probe_buffer(request.limit().get())?;
        if let Some(ordinals) = self.indexes.coverage.get(&request.scope()) {
            for ordinal in ordinals {
                context.check()?;
                let record = self
                    .document()
                    .coverage_records
                    .get(*ordinal)
                    .ok_or(SegmentError::Corrupt)?;
                total = total.checked_add(1).ok_or(SegmentError::Corrupt)?;
                if request.after().is_none_or(|after| record.id > after)
                    && records.len() <= usize::from(request.limit().get())
                {
                    meter.coverage(record)?;
                    records.push(record.clone());
                }
            }
        }
        ReadPage::from_limit_plus_one(records, total, request.limit(), |record| record.id)
            .map_err(SegmentError::from)
    }

    fn read_generation(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<IdentityVerifiedGeneration, Self::Error> {
        IdentityVerifiedGeneration::verify_snapshot(self.snapshot.clone(), context).map_err(
            |error| match error {
                IdentityVerificationError::Control(error) => SegmentError::Control(error),
                _ => SegmentError::Corrupt,
            },
        )
    }
}

fn probe_buffer<T>(limit: u16) -> Result<Vec<T>, SegmentError> {
    let capacity = usize::from(limit)
        .checked_add(1)
        .ok_or(SegmentError::Corrupt)?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(capacity)
        .map_err(|_| SegmentError::Allocation)?;
    Ok(records)
}

struct ReadMeter<'a, 'b> {
    context: &'a GenerationContext<'b>,
    rows: u64,
    text_bytes: u64,
    sources: BTreeSet<SourceRef>,
}

impl<'a, 'b> ReadMeter<'a, 'b> {
    fn new(context: &'a GenerationContext<'b>) -> Result<Self, SegmentError> {
        context.check()?;
        Ok(Self {
            context,
            rows: 0,
            text_bytes: 0,
            sources: BTreeSet::new(),
        })
    }

    fn rows(&mut self, count: usize) -> Result<(), SegmentError> {
        let count = u64::try_from(count).map_err(|_| SegmentError::Corrupt)?;
        self.rows = self.rows.checked_add(count).ok_or(SegmentError::Corrupt)?;
        self.context
            .require(GenerationResource::Rows, self.rows)
            .map_err(SegmentError::from)
    }

    fn text(&mut self, value: &str) -> Result<(), SegmentError> {
        let length = u64::try_from(value.len()).map_err(|_| SegmentError::Corrupt)?;
        self.text_bytes = self
            .text_bytes
            .checked_add(length)
            .ok_or(SegmentError::Corrupt)?;
        self.context
            .require(GenerationResource::TextBytes, self.text_bytes)
            .map_err(SegmentError::from)
    }

    fn optional_text(&mut self, value: Option<&str>) -> Result<(), SegmentError> {
        if let Some(value) = value {
            self.text(value)?;
        }
        Ok(())
    }

    fn source(&mut self, source: &SourceRef) -> Result<(), SegmentError> {
        if self.sources.insert(source.clone()) {
            self.rows(1)?;
            let observed = u64::try_from(self.sources.len()).map_err(|_| SegmentError::Corrupt)?;
            self.context
                .require(GenerationResource::SourceReferences, observed)?;
        }
        Ok(())
    }

    fn evidence(&mut self, evidence: &FactEvidence) -> Result<(), SegmentError> {
        if let Some(source) = &evidence.source {
            self.source(source)?;
        }
        self.rows(evidence.derivation.len())
    }

    fn file(&mut self, record: &FileRecord) -> Result<(), SegmentError> {
        self.rows(1)?;
        self.text(&record.path)?;
        if let Some(locator) = &record.path_locator {
            self.text(locator.encoding().as_str())?;
            self.rows(locator.components().len())?;
            for component in locator.components() {
                self.text(component)?;
            }
        }
        self.text(&record.language)?;
        self.text(&record.encoding)?;
        self.evidence(&record.evidence)
    }

    fn entity(&mut self, record: &EntityRecord) -> Result<(), SegmentError> {
        self.rows(1)?;
        self.rows(record.flags.len())?;
        self.text(&record.language)?;
        self.text(&record.canonical_name)?;
        self.text(&record.display_name)?;
        self.text(&record.qualified_name)?;
        self.evidence(&record.evidence)
    }

    fn occurrence(&mut self, record: &OccurrenceRecord) -> Result<(), SegmentError> {
        self.rows(1)?;
        self.source(&record.source)?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &record.target {
            self.rows(symbols.len())?;
        }
        self.text(&record.syntax_kind)?;
        self.evidence(&record.evidence)
    }

    fn relation(&mut self, record: &RelationRecord) -> Result<(), SegmentError> {
        self.rows(1)?;
        self.evidence(&record.evidence)
    }

    fn provenance(&mut self, record: &ProvenanceRecord) -> Result<(), SegmentError> {
        self.rows(1)?;
        for source in record.input_sources.iter().chain(&record.evidence_sources) {
            self.source(source)?;
        }
        self.rows(record.derivation_parents.len())?;
        self.text(record.producer.name())?;
        self.text(record.producer.version())?;
        self.optional_text(record.frontend_version.as_deref())?;
        self.text(&record.language)?;
        self.optional_text(record.rule.as_deref())
    }

    fn coverage(&mut self, record: &CoverageRecord) -> Result<(), SegmentError> {
        self.rows(1)?;
        self.evidence(&record.evidence)
    }
}
