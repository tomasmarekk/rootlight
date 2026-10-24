//! Preparation-local undo ownership for ordinary bounded resolution.
//! Mutable collections have fallible owned backups; unchanged and append-only
//! collections must retain their exact original records before capacity rollback.

use std::{io, mem::size_of};

use rootlight_ir::OccurrenceTarget;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    File,
    Entity,
    Provenance,
    Mapping,
    Extension,
}

struct Fingerprint {
    kind: Kind,
    id: [u8; 20],
    value: blake3::Hash,
}

pub(super) struct Undo {
    repository: RepositoryId,
    generation: GenerationId,
    version: NormalizedIrVersion,
    occurrences: Vec<OccurrenceRecord>,
    relations: Vec<RelationRecord>,
    coverage: Vec<CoverageRecord>,
    gaps: Vec<SkippedRegion>,
    diagnostics: Vec<DiagnosticRecord>,
    unchanged: Vec<Fingerprint>,
}

fn unavailable() -> FirstSliceError {
    FirstSliceError::ResourceUnavailable {
        resource: FirstSliceResource::MemoryBytes,
    }
}

fn amount<T>(count: usize) -> Result<u64, FirstSliceError> {
    count
        .checked_mul(size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(FirstSliceError::Limits)
}

fn add(total: &mut u64, bytes: u64) -> Result<(), FirstSliceError> {
    *total = total.checked_add(bytes).ok_or(FirstSliceError::Limits)?;
    Ok(())
}

fn reserve<T>(count: usize) -> Result<Vec<T>, FirstSliceError> {
    #[cfg(test)]
    if tests::FAIL_NEXT_ALLOCATION.with(|slot| slot.replace(false)) {
        return Err(unavailable());
    }
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| unavailable())?;
    Ok(values)
}

fn copy<T: Clone>(values: &[T]) -> Result<Vec<T>, FirstSliceError> {
    let mut result = reserve(values.len())?;
    result.extend_from_slice(values);
    Ok(result)
}

fn text(value: &str) -> Result<String, FirstSliceError> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| unavailable())?;
    result.push_str(value);
    Ok(result)
}

fn evidence(value: &FactEvidence) -> Result<FactEvidence, FirstSliceError> {
    Ok(FactEvidence {
        source: value.source.clone(),
        derivation: copy(&value.derivation)?,
    })
}

fn target(value: &OccurrenceTarget) -> Result<OccurrenceTarget, FirstSliceError> {
    Ok(match value {
        OccurrenceTarget::Resolved { symbol } => OccurrenceTarget::Resolved { symbol: *symbol },
        OccurrenceTarget::Unresolved { text_hash } => OccurrenceTarget::Unresolved {
            text_hash: *text_hash,
        },
        OccurrenceTarget::Candidates {
            symbols,
            total_count,
            completeness,
        } => OccurrenceTarget::Candidates {
            symbols: copy(symbols)?,
            total_count: *total_count,
            completeness: *completeness,
        },
    })
}

fn fingerprint(
    kind: Kind,
    id: [u8; 20],
    value: &impl serde::Serialize,
    cancellation: &Cancellation,
) -> Result<Fingerprint, FirstSliceError> {
    struct Writer<'a> {
        state: blake3::Hasher,
        cancellation: &'a Cancellation,
    }
    impl io::Write for Writer<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.cancellation
                .check()
                .map_err(|_| io::Error::other("resolution undo fingerprint cancelled"))?;
            self.state.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    check_cancellation(cancellation)?;
    let mut writer = Writer {
        state: blake3::Hasher::new(),
        cancellation,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(current_cancellation(cancellation).unwrap_or(FirstSliceError::Identity));
    }
    check_cancellation(cancellation)?;
    Ok(Fingerprint {
        kind,
        id,
        value: writer.state.finalize(),
    })
}

fn vector_bytes(document: &NormalizedIrDocument, capacity: bool) -> Result<u64, FirstSliceError> {
    macro_rules! count {
        ($values:expr) => {
            if capacity {
                $values.capacity()
            } else {
                $values.len()
            }
        };
    }
    let mut bytes = 0;
    add(
        &mut bytes,
        amount::<OccurrenceRecord>(count!(document.occurrences))?,
    )?;
    add(
        &mut bytes,
        amount::<RelationRecord>(count!(document.relations))?,
    )?;
    add(
        &mut bytes,
        amount::<CoverageRecord>(count!(document.coverage_records))?,
    )?;
    add(
        &mut bytes,
        amount::<SkippedRegion>(count!(document.skipped_regions))?,
    )?;
    add(
        &mut bytes,
        amount::<DiagnosticRecord>(count!(document.diagnostics))?,
    )?;
    for occurrence in &document.occurrences {
        add(&mut bytes, amount::<u8>(count!(occurrence.syntax_kind))?)?;
        add(
            &mut bytes,
            amount::<FactRef>(count!(occurrence.evidence.derivation))?,
        )?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &occurrence.target {
            add(&mut bytes, amount::<SymbolId>(count!(symbols))?)?;
        }
    }
    for relation in &document.relations {
        add(
            &mut bytes,
            amount::<FactRef>(count!(relation.evidence.derivation))?,
        )?;
    }
    for coverage in &document.coverage_records {
        add(
            &mut bytes,
            amount::<FactRef>(count!(coverage.evidence.derivation))?,
        )?;
    }
    for gap in &document.skipped_regions {
        add(&mut bytes, amount::<u8>(count!(gap.detail))?)?;
        add(
            &mut bytes,
            amount::<FactRef>(count!(gap.evidence.derivation))?,
        )?;
    }
    for diagnostic in &document.diagnostics {
        add(&mut bytes, amount::<u8>(count!(diagnostic.code))?)?;
        add(&mut bytes, amount::<u8>(count!(diagnostic.message))?)?;
        add(
            &mut bytes,
            amount::<FactRef>(count!(diagnostic.evidence.derivation))?,
        )?;
    }
    Ok(bytes)
}

impl Undo {
    /// Owns the exact collections ordinary resolution may replace or remove.
    ///
    /// The caller supplies canonical input without native project materialization
    /// and charges this additional resident ownership before invoking the engine.
    /// Serialization fingerprints authorize rollback only; they are never used
    /// as a proxy for Oracle physical rows or restore-capacity measurement.
    pub(super) fn capture(
        document: &NormalizedIrDocument,
        cancellation: &Cancellation,
        admit: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        let unchanged_count = document
            .files
            .len()
            .checked_add(document.entities.len())
            .and_then(|count| count.checked_add(document.provenance.len()))
            .and_then(|count| count.checked_add(document.source_mappings.len()))
            .and_then(|count| count.checked_add(document.extensions.len()))
            .ok_or(FirstSliceError::Limits)?;
        let predicted = vector_bytes(document, false)?
            .checked_add(amount::<Fingerprint>(unchanged_count)?)
            .ok_or(FirstSliceError::Limits)?;
        admit(predicted)?;
        let mut undo = Self {
            repository: document.repository,
            generation: document.generation,
            version: document.version,
            occurrences: reserve(document.occurrences.len())?,
            relations: reserve(document.relations.len())?,
            coverage: reserve(document.coverage_records.len())?,
            gaps: reserve(document.skipped_regions.len())?,
            diagnostics: reserve(document.diagnostics.len())?,
            unchanged: reserve(unchanged_count)?,
        };
        for value in &document.occurrences {
            check_cancellation(cancellation)?;
            undo.occurrences.push(OccurrenceRecord {
                id: value.id,
                repository: value.repository,
                generation: value.generation,
                file: value.file,
                source: value.source.clone(),
                role: value.role,
                enclosing: value.enclosing,
                target: target(&value.target)?,
                syntactic_text_hash: value.syntactic_text_hash,
                syntax_kind: text(&value.syntax_kind)?,
                provenance: value.provenance,
                confidence: value.confidence,
                evidence: evidence(&value.evidence)?,
            });
        }
        for value in &document.relations {
            check_cancellation(cancellation)?;
            undo.relations.push(RelationRecord {
                id: value.id,
                repository: value.repository,
                generation: value.generation,
                subject: value.subject,
                predicate: value.predicate,
                object: value.object,
                confidence: value.confidence,
                evidence_kind: value.evidence_kind,
                provenance: value.provenance,
                evidence: evidence(&value.evidence)?,
            });
        }
        for value in &document.coverage_records {
            check_cancellation(cancellation)?;
            undo.coverage.push(CoverageRecord {
                id: value.id,
                repository: value.repository,
                generation: value.generation,
                scope: value.scope,
                domain: value.domain,
                tier: value.tier,
                status: value.status,
                discovered: value.discovered,
                indexed: value.indexed,
                skipped: value.skipped,
                provenance: value.provenance,
                evidence: evidence(&value.evidence)?,
            });
        }
        for value in &document.skipped_regions {
            check_cancellation(cancellation)?;
            undo.gaps.push(SkippedRegion {
                id: value.id,
                repository: value.repository,
                generation: value.generation,
                source: value.source.clone(),
                domain: value.domain,
                reason: value.reason,
                detail: text(&value.detail)?,
                provenance: value.provenance,
                evidence: evidence(&value.evidence)?,
            });
        }
        for value in &document.diagnostics {
            check_cancellation(cancellation)?;
            undo.diagnostics.push(DiagnosticRecord {
                id: value.id,
                repository: value.repository,
                generation: value.generation,
                code: text(&value.code)?,
                message: text(&value.message)?,
                severity: value.severity,
                source: value.source.clone(),
                coverage_effect: value.coverage_effect,
                provenance: value.provenance,
                evidence: evidence(&value.evidence)?,
            });
        }
        macro_rules! fingerprint_rows {
            ($kind:ident, $rows:expr) => {
                for record in $rows {
                    undo.unchanged.push(fingerprint(
                        Kind::$kind,
                        *record.id.as_bytes(),
                        record,
                        cancellation,
                    )?);
                }
            };
        }
        fingerprint_rows!(File, &document.files);
        fingerprint_rows!(Entity, &document.entities);
        fingerprint_rows!(Provenance, &document.provenance);
        fingerprint_rows!(Mapping, &document.source_mappings);
        fingerprint_rows!(Extension, &document.extensions);
        undo.unchanged
            .sort_unstable_by_key(|value| (value.kind, value.id));
        check_cancellation(cancellation)?;
        let actual = undo.retained_bytes()?;
        if actual > predicted {
            admit(actual - predicted)?;
        }
        #[cfg(test)]
        tests::observe_cost(predicted, actual, undo.unchanged.len());
        Ok(undo)
    }

    fn retained_bytes(&self) -> Result<u64, FirstSliceError> {
        let mut bytes = amount::<OccurrenceRecord>(self.occurrences.capacity())?
            .checked_add(amount::<RelationRecord>(self.relations.capacity())?)
            .and_then(|bytes| {
                bytes.checked_add(amount::<CoverageRecord>(self.coverage.capacity()).ok()?)
            })
            .and_then(|bytes| {
                bytes.checked_add(amount::<SkippedRegion>(self.gaps.capacity()).ok()?)
            })
            .and_then(|bytes| {
                bytes.checked_add(amount::<DiagnosticRecord>(self.diagnostics.capacity()).ok()?)
            })
            .and_then(|bytes| {
                bytes.checked_add(amount::<Fingerprint>(self.unchanged.capacity()).ok()?)
            })
            .ok_or(FirstSliceError::Limits)?;
        for value in &self.occurrences {
            add(&mut bytes, amount::<u8>(value.syntax_kind.capacity())?)?;
            add(
                &mut bytes,
                amount::<FactRef>(value.evidence.derivation.capacity())?,
            )?;
            if let OccurrenceTarget::Candidates { symbols, .. } = &value.target {
                add(&mut bytes, amount::<SymbolId>(symbols.capacity())?)?;
            }
        }
        for value in &self.relations {
            add(
                &mut bytes,
                amount::<FactRef>(value.evidence.derivation.capacity())?,
            )?;
        }
        for value in &self.coverage {
            add(
                &mut bytes,
                amount::<FactRef>(value.evidence.derivation.capacity())?,
            )?;
        }
        for value in &self.gaps {
            add(&mut bytes, amount::<u8>(value.detail.capacity())?)?;
            add(
                &mut bytes,
                amount::<FactRef>(value.evidence.derivation.capacity())?,
            )?;
        }
        for value in &self.diagnostics {
            add(&mut bytes, amount::<u8>(value.code.capacity())?)?;
            add(&mut bytes, amount::<u8>(value.message.capacity())?)?;
            add(
                &mut bytes,
                amount::<FactRef>(value.evidence.derivation.capacity())?,
            )?;
        }
        Ok(bytes)
    }

    fn has_original(&self, kind: Kind, id: [u8; 20]) -> bool {
        self.unchanged
            .binary_search_by_key(&(kind, id), |value| (value.kind, value.id))
            .is_ok()
    }

    /// Restores accepted pre-resolution facts after a separately typed capacity refusal.
    ///
    /// The caller can invoke this only for its exact measured restore cause.
    /// Identity, provider, allocation, cancellation and engine failures propagate
    /// directly and never enter rollback as capacity evidence. Any change outside
    /// this checkpoint's ownership rejects rollback rather than guessing lost data.
    pub(super) fn restore(
        self,
        mut candidate: NormalizedIrDocument,
        cancellation: &Cancellation,
    ) -> Result<NormalizedIrDocument, FirstSliceError> {
        check_cancellation(cancellation)?;
        if candidate.repository != self.repository
            || candidate.generation != self.generation
            || candidate.version != self.version
        {
            return Err(FirstSliceError::Identity);
        }
        rootlight_ir::validate_ir_document(
            &candidate,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .map_err(|_| FirstSliceError::Identity)?;
        let mut matched = 0usize;
        let mut check =
            |kind: Kind, id: [u8; 20], value: Fingerprint| -> Result<(), FirstSliceError> {
                let original = self
                    .unchanged
                    .binary_search_by_key(&(kind, id), |value| (value.kind, value.id));
                match original {
                    Ok(index) if self.unchanged[index].value == value.value => {
                        matched = matched.checked_add(1).ok_or(FirstSliceError::Limits)?;
                        Ok(())
                    }
                    Err(_) if kind == Kind::Provenance => Ok(()),
                    _ => Err(FirstSliceError::Identity),
                }
            };
        macro_rules! check_rows {
            ($kind:ident, $rows:expr) => {
                for row in $rows {
                    check(
                        Kind::$kind,
                        *row.id.as_bytes(),
                        fingerprint(Kind::$kind, *row.id.as_bytes(), row, cancellation)?,
                    )?;
                }
            };
        }
        check_rows!(File, &candidate.files);
        check_rows!(Entity, &candidate.entities);
        check_rows!(Provenance, &candidate.provenance);
        check_rows!(Mapping, &candidate.source_mappings);
        check_rows!(Extension, &candidate.extensions);
        if matched != self.unchanged.len() {
            return Err(FirstSliceError::Identity);
        }
        candidate
            .provenance
            .retain(|record| self.has_original(Kind::Provenance, *record.id.as_bytes()));
        candidate.occurrences = self.occurrences;
        candidate.relations = self.relations;
        candidate.coverage_records = self.coverage;
        candidate.skipped_regions = self.gaps;
        candidate.diagnostics = self.diagnostics;
        check_cancellation(cancellation)?;
        Ok(candidate)
    }
}

#[cfg(test)]
#[path = "resolver_restore_undo/tests.rs"]
mod tests;
