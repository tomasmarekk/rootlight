//! Count logical JSON record contributions in the final projection's one pass.
//! Only complete numeric observations escape; payloads and temporary allocation
//! lifetimes are neither retained nor inferred by this opt-in diagnostic.

use std::{cell::Cell, io::Write};

use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{
    CoverageRecord, DiagnosticRecord, EntityRecord, ExtensionEnvelope, FileRecord,
    NormalizedIrDocument, NormalizedIrVersion, OccurrenceRecord, ProvenanceRecord, RelationRecord,
    SkippedRegion, SourceMappingRecord,
};
use serde::{Serialize, ser::SerializeSeq};

const CATEGORIES: [&str; 10] = [
    "files",
    "entities",
    "occurrences",
    "relations",
    "provenance",
    "source_mappings",
    "coverage_records",
    "skipped_regions",
    "diagnostics",
    "extensions",
];

/// Complete source-free logical wire measurements, never a heap or RSS claim.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Observation {
    scope: &'static str,
    stage: &'static str,
    document_bytes: u64,
    records: u64,
    serialized_record_bytes: u64,
    maximum_serialized_record_bytes: u64,
    envelope_bytes: u64,
    categories: [Category; CATEGORIES.len()],
}

#[derive(Clone, Copy, Debug, Serialize)]
struct Category {
    category: &'static str,
    records: u64,
    serialized_record_bytes: u64,
    maximum_serialized_record_bytes: u64,
}

#[derive(Clone, Copy, Default)]
struct RecordSizes {
    records: u64,
    bytes: u64,
    maximum: u64,
}

impl RecordSizes {
    fn record(self, bytes: u64) -> Option<Self> {
        Some(Self {
            records: self.records.checked_add(1)?,
            bytes: self.bytes.checked_add(bytes)?,
            maximum: self.maximum.max(bytes),
        })
    }
}

type Position = Cell<Option<u64>>;
type Sizes = [Cell<Option<RecordSizes>>; CATEGORIES.len()];

struct ObservedWriter<'a, W> {
    inner: W,
    position: &'a Position,
}

impl<W: Write> Write for ObservedWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let accepted = self.inner.write(buffer)?;
        self.position.set(
            self.position
                .get()
                .and_then(|position| position.checked_add(u64::try_from(accepted).ok()?)),
        );
        Ok(accepted)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct Record<'a, T> {
    value: &'a T,
    position: &'a Position,
    sizes: &'a Cell<Option<RecordSizes>>,
}

impl<T: Serialize> Serialize for Record<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // JSON sequence separators have already been written when this starts.
        let start = self.position.get();
        let result = self.value.serialize(serializer);
        let bytes = start.and_then(|start| self.position.get()?.checked_sub(start));
        self.sizes.set(if result.is_ok() {
            self.sizes.get().and_then(|sizes| sizes.record(bytes?))
        } else {
            None
        });
        result
    }
}

struct Records<'a, T> {
    values: &'a [T],
    position: &'a Position,
    sizes: &'a Cell<Option<RecordSizes>>,
}

impl<T: Serialize> Serialize for Records<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.values.len()))?;
        for value in self.values {
            sequence.serialize_element(&Record {
                value,
                position: self.position,
                sizes: self.sizes,
            })?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct Document<'a> {
    version: &'a NormalizedIrVersion,
    repository: &'a RepositoryId,
    generation: &'a GenerationId,
    files: Records<'a, FileRecord>,
    entities: Records<'a, EntityRecord>,
    occurrences: Records<'a, OccurrenceRecord>,
    relations: Records<'a, RelationRecord>,
    provenance: Records<'a, ProvenanceRecord>,
    source_mappings: Records<'a, SourceMappingRecord>,
    coverage_records: Records<'a, CoverageRecord>,
    skipped_regions: Records<'a, SkippedRegion>,
    diagnostics: Records<'a, DiagnosticRecord>,
    extensions: Records<'a, ExtensionEnvelope>,
}

fn view<'a>(
    document: &'a NormalizedIrDocument,
    position: &'a Position,
    sizes: &'a Sizes,
) -> Document<'a> {
    // Exhaustive destructuring makes a future wire-field addition visible here.
    let NormalizedIrDocument {
        version,
        repository,
        generation,
        files,
        entities,
        occurrences,
        relations,
        provenance,
        source_mappings,
        coverage_records,
        skipped_regions,
        diagnostics,
        extensions,
    } = document;
    Document {
        version,
        repository,
        generation,
        files: Records {
            values: files,
            position,
            sizes: &sizes[0],
        },
        entities: Records {
            values: entities,
            position,
            sizes: &sizes[1],
        },
        occurrences: Records {
            values: occurrences,
            position,
            sizes: &sizes[2],
        },
        relations: Records {
            values: relations,
            position,
            sizes: &sizes[3],
        },
        provenance: Records {
            values: provenance,
            position,
            sizes: &sizes[4],
        },
        source_mappings: Records {
            values: source_mappings,
            position,
            sizes: &sizes[5],
        },
        coverage_records: Records {
            values: coverage_records,
            position,
            sizes: &sizes[6],
        },
        skipped_regions: Records {
            values: skipped_regions,
            position,
            sizes: &sizes[7],
        },
        diagnostics: Records {
            values: diagnostics,
            position,
            sizes: &sizes[8],
        },
        extensions: Records {
            values: extensions,
            position,
            sizes: &sizes[9],
        },
    }
}

fn finish(position: &Position, sizes: &Sizes) -> Option<Observation> {
    let document_bytes = position.get()?;
    let mut records = 0_u64;
    let mut serialized_record_bytes = 0_u64;
    let mut maximum_serialized_record_bytes = 0_u64;
    let mut categories = [Category {
        category: "",
        records: 0,
        serialized_record_bytes: 0,
        maximum_serialized_record_bytes: 0,
    }; CATEGORIES.len()];
    for ((category, sizes), name) in categories.iter_mut().zip(sizes).zip(CATEGORIES) {
        let sizes = sizes.get()?;
        records = records.checked_add(sizes.records)?;
        serialized_record_bytes = serialized_record_bytes.checked_add(sizes.bytes)?;
        maximum_serialized_record_bytes = maximum_serialized_record_bytes.max(sizes.maximum);
        *category = Category {
            category: name,
            records: sizes.records,
            serialized_record_bytes: sizes.bytes,
            maximum_serialized_record_bytes: sizes.maximum,
        };
    }
    Some(Observation {
        scope: "complete_serialized_logical_json_bytes_not_rss_heap_or_temporary_lifetimes",
        stage: "fresh_final_projection_serialization_before_admission",
        document_bytes,
        records,
        serialized_record_bytes,
        maximum_serialized_record_bytes,
        envelope_bytes: document_bytes.checked_sub(serialized_record_bytes)?,
        categories,
    })
}

fn serialize_profiled<W: Write>(
    writer: W,
    document: &NormalizedIrDocument,
) -> serde_json::Result<()> {
    crate::with_analysis_profile(|profile| profile.last_fresh_projection_records = None);
    let position = Position::new(Some(0));
    let sizes: Sizes = std::array::from_fn(|_| Cell::new(Some(RecordSizes::default())));
    serde_json::to_writer(
        ObservedWriter {
            inner: writer,
            position: &position,
        },
        &view(document, &position, &sizes),
    )?;
    let observation = finish(&position, &sizes);
    crate::with_analysis_profile(|profile| profile.last_fresh_projection_records = observation);
    Ok(())
}

/// Preserve normal admission counting, observing only an explicitly profiled run.
///
/// # Errors
///
/// Serialization errors retain the existing `Limits` result. Numeric telemetry
/// loss leaves an unknown observation and never becomes an admission failure.
pub(crate) fn document_serialized_bytes(
    document: &NormalizedIrDocument,
) -> Result<u64, crate::FirstSliceError> {
    if !crate::analysis_profile_enabled() {
        return crate::normalized_document_serialized_bytes(document);
    }
    let mut counter = crate::SerializedSizeCounter::default();
    serialize_profiled(&mut counter, document).map_err(|_| crate::FirstSliceError::Limits)?;
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests;
