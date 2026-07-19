//! Deterministic portable encoding and defensive decoding for research segments.
//!
//! All offsets are little-endian, sections are contiguous and checksummed, and
//! decoded indexes must equal indexes rebuilt from the verified canonical IR.

use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::Arc,
};

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{
    CoverageScope, ExtensionSupport, IrDocument, IrLimits, NormalizedIrDocument, RelationEndpoint,
    decode_ir_document,
};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationContext, GenerationContractVersion, GenerationMetadata,
    GenerationResource, GenerationSnapshot, GenerationStats, HARD_MAX_GENERATION_ROWS,
    IdentityVerificationError, IdentityVerifiedGeneration,
};

use crate::{MAX_SEGMENT_BYTES, SEGMENT_FORMAT_MAJOR, SEGMENT_FORMAT_MINOR, SegmentError};

const MAGIC: [u8; 8] = *b"RLSEG001";
const SECTION_COUNT: usize = 8;
const HEADER_PREFIX_BYTES: usize = 64;
const SECTION_DESCRIPTOR_BYTES: usize = 64;
const HEADER_BYTES: usize = HEADER_PREFIX_BYTES + SECTION_COUNT * SECTION_DESCRIPTOR_BYTES;
const HEADER_CHECKSUM_START: usize = 32;
const HEADER_CHECKSUM_END: usize = 64;
const MANIFEST_BYTES: usize = 261;
const CHECKPOINT_BYTES: usize = 64 * 1024;
const MAX_INDEX_ENTRIES: u64 = HARD_MAX_GENERATION_ROWS * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Manifest,
    Document,
    Files,
    Entities,
    Relations,
    Occurrences,
    Provenance,
    Coverage,
}

impl SectionKind {
    const fn code(self) -> u16 {
        match self {
            Self::Manifest => 1,
            Self::Document => 2,
            Self::Files => 3,
            Self::Entities => 4,
            Self::Relations => 5,
            Self::Occurrences => 6,
            Self::Provenance => 7,
            Self::Coverage => 8,
        }
    }

    fn parse(code: u16) -> Result<Self, SegmentError> {
        match code {
            1 => Ok(Self::Manifest),
            2 => Ok(Self::Document),
            3 => Ok(Self::Files),
            4 => Ok(Self::Entities),
            5 => Ok(Self::Relations),
            6 => Ok(Self::Occurrences),
            7 => Ok(Self::Provenance),
            8 => Ok(Self::Coverage),
            _ => Err(SegmentError::Corrupt),
        }
    }
}

const SECTION_KINDS: [SectionKind; SECTION_COUNT] = [
    SectionKind::Manifest,
    SectionKind::Document,
    SectionKind::Files,
    SectionKind::Entities,
    SectionKind::Relations,
    SectionKind::Occurrences,
    SectionKind::Provenance,
    SectionKind::Coverage,
];

#[derive(Debug)]
struct Section {
    kind: SectionKind,
    bytes: Vec<u8>,
    count: u64,
}

#[derive(Debug, Clone, Copy)]
struct SectionDescriptor {
    kind: SectionKind,
    offset: u64,
    length: u64,
    count: u64,
    checksum: [u8; 32],
}

pub(crate) struct DecodedSegment {
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) snapshot: GenerationSnapshot,
    pub(crate) stats: GenerationStats,
    pub(crate) indexes: SegmentIndexes,
}

pub(crate) struct SegmentIndexes {
    pub(crate) files: BTreeMap<FileId, usize>,
    pub(crate) entities: BTreeMap<SymbolId, usize>,
    pub(crate) outgoing_relations: BTreeMap<RelationEndpoint, Vec<usize>>,
    pub(crate) incoming_relations: BTreeMap<RelationEndpoint, Vec<usize>>,
    pub(crate) occurrences: BTreeMap<FileId, Vec<usize>>,
    pub(crate) provenance: BTreeMap<FactId, usize>,
    pub(crate) coverage: BTreeMap<CoverageScope, Vec<usize>>,
}

impl SegmentIndexes {
    fn build(
        document: &NormalizedIrDocument,
        context: &GenerationContext<'_>,
    ) -> Result<Self, SegmentError> {
        let mut files = BTreeMap::new();
        for (ordinal, record) in document.files.iter().enumerate() {
            context.check()?;
            if files.insert(record.id, ordinal).is_some() {
                return Err(SegmentError::Corrupt);
            }
        }

        let mut entities = BTreeMap::new();
        for (ordinal, record) in document.entities.iter().enumerate() {
            context.check()?;
            if entities.insert(record.id, ordinal).is_some() {
                return Err(SegmentError::Corrupt);
            }
        }

        let mut outgoing_relations = BTreeMap::<RelationEndpoint, Vec<usize>>::new();
        let mut incoming_relations = BTreeMap::<RelationEndpoint, Vec<usize>>::new();
        for (ordinal, record) in document.relations.iter().enumerate() {
            context.check()?;
            outgoing_relations
                .entry(record.subject)
                .or_default()
                .push(ordinal);
            incoming_relations
                .entry(record.object)
                .or_default()
                .push(ordinal);
        }
        sort_relation_ordinals(&mut outgoing_relations, document);
        sort_relation_ordinals(&mut incoming_relations, document);

        let mut occurrences = BTreeMap::<FileId, Vec<usize>>::new();
        for (ordinal, record) in document.occurrences.iter().enumerate() {
            context.check()?;
            occurrences.entry(record.file).or_default().push(ordinal);
        }
        for ordinals in occurrences.values_mut() {
            ordinals.sort_unstable_by_key(|ordinal| document.occurrences[*ordinal].id);
        }

        let mut provenance = BTreeMap::new();
        for (ordinal, record) in document.provenance.iter().enumerate() {
            context.check()?;
            if provenance.insert(record.id, ordinal).is_some() {
                return Err(SegmentError::Corrupt);
            }
        }

        let mut coverage = BTreeMap::<CoverageScope, Vec<usize>>::new();
        for (ordinal, record) in document.coverage_records.iter().enumerate() {
            context.check()?;
            coverage.entry(record.scope).or_default().push(ordinal);
        }
        for ordinals in coverage.values_mut() {
            ordinals.sort_unstable_by_key(|ordinal| document.coverage_records[*ordinal].id);
        }

        context.check()?;
        Ok(Self {
            files,
            entities,
            outgoing_relations,
            incoming_relations,
            occurrences,
            provenance,
            coverage,
        })
    }
}

fn sort_relation_ordinals(
    index: &mut BTreeMap<RelationEndpoint, Vec<usize>>,
    document: &NormalizedIrDocument,
) {
    for ordinals in index.values_mut() {
        ordinals.sort_unstable_by_key(|ordinal| document.relations[*ordinal].id);
    }
}

pub(crate) fn encode(
    generation: IdentityVerifiedGeneration,
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<Arc<[u8]>, SegmentError> {
    context.check()?;
    let snapshot = generation.into_snapshot();
    validate_stats(snapshot.document(), stats, SegmentError::InvalidStatistics)?;
    require_stats(stats, context)?;

    let indexes = SegmentIndexes::build(snapshot.document(), context)?;
    let document = encode_json(snapshot.document(), context)?;
    let sections = sections_for(
        snapshot.metadata(),
        stats,
        document,
        snapshot.document(),
        &indexes,
        context,
    )?;
    let encoded = encode_sections(&sections, context)?;
    context.check()?;
    Ok(Arc::from(encoded.into_boxed_slice()))
}

pub(crate) fn decode(
    bytes: Arc<[u8]>,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
    context: &GenerationContext<'_>,
) -> Result<DecodedSegment, SegmentError> {
    context.check()?;
    if bytes.len() > MAX_SEGMENT_BYTES {
        return Err(SegmentError::TooLarge {
            maximum: MAX_SEGMENT_BYTES,
        });
    }
    let descriptors = decode_header(&bytes, context)?;
    let manifest_bytes = section_bytes(&bytes, descriptors[0])?;
    if manifest_bytes.len() != MANIFEST_BYTES {
        return Err(SegmentError::Corrupt);
    }
    let (metadata, stats) = decode_manifest(manifest_bytes)?;
    require_stats(stats, context)?;

    let document_bytes = section_bytes(&bytes, descriptors[1])?;
    if document_bytes.len() > limits.max_document_bytes {
        return Err(SegmentError::Corrupt);
    }
    let IrDocument::NormalizedV1_1(document) =
        decode_ir_document(document_bytes, limits, extensions)
            .map_err(|_| SegmentError::Corrupt)?
    else {
        return Err(SegmentError::Corrupt);
    };
    let canonical_document = encode_json(&document, context)?;
    if canonical_document.as_slice() != document_bytes {
        return Err(SegmentError::Corrupt);
    }
    validate_stats(&document, stats, SegmentError::Corrupt)?;

    let verified =
        IdentityVerifiedGeneration::verify(metadata, document, limits, extensions, context)
            .map_err(map_identity_error)?;
    let snapshot = verified.into_snapshot();
    let indexes = SegmentIndexes::build(snapshot.document(), context)?;
    let expected = sections_for(
        metadata,
        stats,
        canonical_document,
        snapshot.document(),
        &indexes,
        context,
    )?;
    for (descriptor, section) in descriptors.iter().zip(&expected) {
        context.check()?;
        if descriptor.kind != section.kind
            || descriptor.count != section.count
            || section_bytes(&bytes, *descriptor)? != section.bytes
        {
            return Err(SegmentError::Corrupt);
        }
    }
    context.check()?;
    Ok(DecodedSegment {
        bytes,
        snapshot,
        stats,
        indexes,
    })
}

fn map_identity_error(error: IdentityVerificationError) -> SegmentError {
    match error {
        IdentityVerificationError::Control(error) => SegmentError::Control(error),
        _ => SegmentError::Corrupt,
    }
}

fn require_stats(
    stats: GenerationStats,
    context: &GenerationContext<'_>,
) -> Result<(), SegmentError> {
    context.require(GenerationResource::Rows, stats.stored_rows())?;
    context.require(GenerationResource::SourceReferences, stats.source_refs())?;
    context.require(GenerationResource::TextBytes, stats.text_bytes())?;
    Ok(())
}

fn validate_stats(
    document: &NormalizedIrDocument,
    stats: GenerationStats,
    mismatch: SegmentError,
) -> Result<(), SegmentError> {
    let observed = [
        usize_to_u64(document.files.len())?,
        usize_to_u64(document.entities.len())?,
        usize_to_u64(document.occurrences.len())?,
        usize_to_u64(document.relations.len())?,
        usize_to_u64(document.provenance.len())?,
        usize_to_u64(document.source_mappings.len())?,
        usize_to_u64(document.coverage_records.len())?,
        usize_to_u64(document.skipped_regions.len())?,
        usize_to_u64(document.diagnostics.len())?,
        usize_to_u64(document.extensions.len())?,
    ];
    let expected = [
        stats.files(),
        stats.entities(),
        stats.occurrences(),
        stats.relations(),
        stats.provenance(),
        stats.source_mappings(),
        stats.coverage(),
        stats.skipped_regions(),
        stats.diagnostics(),
        stats.extensions(),
    ];
    if observed == expected {
        Ok(())
    } else {
        Err(mismatch)
    }
}

fn sections_for(
    metadata: GenerationMetadata,
    stats: GenerationStats,
    document_bytes: Vec<u8>,
    document: &NormalizedIrDocument,
    indexes: &SegmentIndexes,
    context: &GenerationContext<'_>,
) -> Result<Vec<Section>, SegmentError> {
    let relation_count = usize_to_u64(document.relations.len())?
        .checked_mul(2)
        .ok_or(SegmentError::Encoding)?;
    let sections = vec![
        Section {
            kind: SectionKind::Manifest,
            bytes: encode_manifest(metadata, stats)?,
            count: 1,
        },
        Section {
            kind: SectionKind::Document,
            bytes: document_bytes,
            count: 1,
        },
        Section {
            kind: SectionKind::Files,
            bytes: encode_file_index(indexes)?,
            count: usize_to_u64(document.files.len())?,
        },
        Section {
            kind: SectionKind::Entities,
            bytes: encode_entity_index(indexes)?,
            count: usize_to_u64(document.entities.len())?,
        },
        Section {
            kind: SectionKind::Relations,
            bytes: encode_relation_index(indexes, document, context)?,
            count: relation_count,
        },
        Section {
            kind: SectionKind::Occurrences,
            bytes: encode_occurrence_index(indexes, document, context)?,
            count: usize_to_u64(document.occurrences.len())?,
        },
        Section {
            kind: SectionKind::Provenance,
            bytes: encode_provenance_index(indexes)?,
            count: usize_to_u64(document.provenance.len())?,
        },
        Section {
            kind: SectionKind::Coverage,
            bytes: encode_coverage_index(indexes, document, context)?,
            count: usize_to_u64(document.coverage_records.len())?,
        },
    ];
    Ok(sections)
}

fn encode_sections(
    sections: &[Section],
    context: &GenerationContext<'_>,
) -> Result<Vec<u8>, SegmentError> {
    if sections.len() != SECTION_COUNT {
        return Err(SegmentError::Encoding);
    }
    let mut offset = usize_to_u64(HEADER_BYTES)?;
    let mut descriptors = Vec::with_capacity(SECTION_COUNT);
    for (expected_kind, section) in SECTION_KINDS.into_iter().zip(sections) {
        context.check()?;
        if section.kind != expected_kind || section.count > MAX_INDEX_ENTRIES {
            return Err(SegmentError::Encoding);
        }
        let length = usize_to_u64(section.bytes.len())?;
        let checksum = checked_hash(&section.bytes, context)?;
        descriptors.push(SectionDescriptor {
            kind: section.kind,
            offset,
            length,
            count: section.count,
            checksum,
        });
        offset = offset.checked_add(length).ok_or(SegmentError::TooLarge {
            maximum: MAX_SEGMENT_BYTES,
        })?;
    }
    let total = usize::try_from(offset).map_err(|_| SegmentError::TooLarge {
        maximum: MAX_SEGMENT_BYTES,
    })?;
    if total > MAX_SEGMENT_BYTES {
        return Err(SegmentError::TooLarge {
            maximum: MAX_SEGMENT_BYTES,
        });
    }

    let header = encode_header(&descriptors, offset, context)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(total)
        .map_err(|_| SegmentError::Allocation)?;
    output.extend_from_slice(&header);
    for section in sections {
        context.check()?;
        output.extend_from_slice(&section.bytes);
    }
    if output.len() != total {
        return Err(SegmentError::Encoding);
    }
    Ok(output)
}

fn encode_header(
    descriptors: &[SectionDescriptor],
    total_length: u64,
    context: &GenerationContext<'_>,
) -> Result<Vec<u8>, SegmentError> {
    if descriptors.len() != SECTION_COUNT {
        return Err(SegmentError::Encoding);
    }
    let mut encoder = Encoder::with_capacity(HEADER_BYTES)?;
    encoder.bytes(&MAGIC)?;
    encoder.u16(SEGMENT_FORMAT_MAJOR)?;
    encoder.u16(SEGMENT_FORMAT_MINOR)?;
    encoder.u32(0)?;
    encoder.u16(u16::try_from(SECTION_COUNT).map_err(|_| SegmentError::Encoding)?)?;
    encoder.u16(0)?;
    encoder.u32(u32::try_from(HEADER_BYTES).map_err(|_| SegmentError::Encoding)?)?;
    encoder.u64(total_length)?;
    encoder.bytes(&[0; 32])?;
    for descriptor in descriptors {
        encoder.u16(descriptor.kind.code())?;
        encoder.u16(0)?;
        encoder.u32(0)?;
        encoder.u64(descriptor.offset)?;
        encoder.u64(descriptor.length)?;
        encoder.u64(descriptor.count)?;
        encoder.bytes(&descriptor.checksum)?;
    }
    let mut header = encoder.finish();
    if header.len() != HEADER_BYTES {
        return Err(SegmentError::Encoding);
    }
    let checksum = checked_header_hash(&header, context)?;
    let destination = header
        .get_mut(HEADER_CHECKSUM_START..HEADER_CHECKSUM_END)
        .ok_or(SegmentError::Encoding)?;
    destination.copy_from_slice(&checksum);
    Ok(header)
}

fn decode_header(
    bytes: &[u8],
    context: &GenerationContext<'_>,
) -> Result<Vec<SectionDescriptor>, SegmentError> {
    let header = bytes.get(..HEADER_BYTES).ok_or(SegmentError::Corrupt)?;
    let mut decoder = Decoder::new(header);
    if decoder.array::<8>()? != MAGIC {
        return Err(SegmentError::Corrupt);
    }
    let major = decoder.u16()?;
    let minor = decoder.u16()?;
    if major != SEGMENT_FORMAT_MAJOR || minor != SEGMENT_FORMAT_MINOR {
        return Err(SegmentError::UnsupportedVersion { major, minor });
    }
    if decoder.u32()? != 0 {
        return Err(SegmentError::Corrupt);
    }
    if usize::from(decoder.u16()?) != SECTION_COUNT || decoder.u16()? != 0 {
        return Err(SegmentError::Corrupt);
    }
    if usize::try_from(decoder.u32()?).map_err(|_| SegmentError::Corrupt)? != HEADER_BYTES {
        return Err(SegmentError::Corrupt);
    }
    let total_length = usize::try_from(decoder.u64()?).map_err(|_| SegmentError::Corrupt)?;
    if total_length != bytes.len() || total_length > MAX_SEGMENT_BYTES {
        return Err(SegmentError::Corrupt);
    }
    let declared_header_checksum = decoder.array::<32>()?;

    let mut descriptors = Vec::with_capacity(SECTION_COUNT);
    for expected_kind in SECTION_KINDS {
        context.check()?;
        let kind = SectionKind::parse(decoder.u16()?)?;
        if kind != expected_kind || decoder.u16()? != 0 || decoder.u32()? != 0 {
            return Err(SegmentError::Corrupt);
        }
        let descriptor = SectionDescriptor {
            kind,
            offset: decoder.u64()?,
            length: decoder.u64()?,
            count: decoder.u64()?,
            checksum: decoder.array::<32>()?,
        };
        if descriptor.count > MAX_INDEX_ENTRIES {
            return Err(SegmentError::Corrupt);
        }
        descriptors.push(descriptor);
    }
    decoder.finish()?;
    if checked_header_hash(header, context)? != declared_header_checksum {
        return Err(SegmentError::Corrupt);
    }

    let mut expected_offset = HEADER_BYTES;
    for descriptor in &descriptors {
        context.check()?;
        let offset = usize::try_from(descriptor.offset).map_err(|_| SegmentError::Corrupt)?;
        let length = usize::try_from(descriptor.length).map_err(|_| SegmentError::Corrupt)?;
        if offset != expected_offset {
            return Err(SegmentError::Corrupt);
        }
        expected_offset = offset.checked_add(length).ok_or(SegmentError::Corrupt)?;
        let section = bytes
            .get(offset..expected_offset)
            .ok_or(SegmentError::Corrupt)?;
        if checked_hash(section, context)? != descriptor.checksum {
            return Err(SegmentError::Corrupt);
        }
    }
    if expected_offset != bytes.len() || descriptors[0].count != 1 || descriptors[1].count != 1 {
        return Err(SegmentError::Corrupt);
    }
    Ok(descriptors)
}

fn section_bytes(bytes: &[u8], descriptor: SectionDescriptor) -> Result<&[u8], SegmentError> {
    let start = usize::try_from(descriptor.offset).map_err(|_| SegmentError::Corrupt)?;
    let length = usize::try_from(descriptor.length).map_err(|_| SegmentError::Corrupt)?;
    let end = start.checked_add(length).ok_or(SegmentError::Corrupt)?;
    bytes.get(start..end).ok_or(SegmentError::Corrupt)
}

fn checked_hash(bytes: &[u8], context: &GenerationContext<'_>) -> Result<[u8; 32], SegmentError> {
    let mut hasher = blake3::Hasher::new();
    for chunk in bytes.chunks(CHECKPOINT_BYTES) {
        context.check()?;
        hasher.update(chunk);
    }
    context.check()?;
    Ok(*hasher.finalize().as_bytes())
}

fn checked_header_hash(
    header: &[u8],
    context: &GenerationContext<'_>,
) -> Result<[u8; 32], SegmentError> {
    let before = header
        .get(..HEADER_CHECKSUM_START)
        .ok_or(SegmentError::Corrupt)?;
    let after = header
        .get(HEADER_CHECKSUM_END..HEADER_BYTES)
        .ok_or(SegmentError::Corrupt)?;
    context.check()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(before);
    hasher.update(&[0; 32]);
    hasher.update(after);
    context.check()?;
    Ok(*hasher.finalize().as_bytes())
}

fn encode_manifest(
    metadata: GenerationMetadata,
    stats: GenerationStats,
) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::with_capacity(MANIFEST_BYTES)?;
    let contract = metadata.contract_version();
    encoder.u16(contract.major())?;
    encoder.u16(contract.minor())?;
    encoder.bytes(metadata.repository().as_bytes())?;
    encoder.bytes(metadata.generation().as_bytes())?;
    match metadata.parent() {
        Some(parent) => {
            encoder.u8(1)?;
            encoder.bytes(parent.as_bytes())?;
        }
        None => {
            encoder.u8(0)?;
            encoder.bytes(&[0; 20])?;
        }
    }
    encoder.bytes(metadata.manifest_hash().as_bytes())?;
    encoder.bytes(metadata.configuration_hash().as_bytes())?;
    encoder.bytes(metadata.provider_set_hash().as_bytes())?;
    for value in stats_values(stats) {
        encoder.u64(value)?;
    }
    let bytes = encoder.finish();
    if bytes.len() == MANIFEST_BYTES {
        Ok(bytes)
    } else {
        Err(SegmentError::Encoding)
    }
}

fn decode_manifest(bytes: &[u8]) -> Result<(GenerationMetadata, GenerationStats), SegmentError> {
    let mut decoder = Decoder::new(bytes);
    let contract = GenerationContractVersion::new(decoder.u16()?, decoder.u16()?);
    if contract != GENERATION_CONTRACT_VERSION {
        return Err(SegmentError::Corrupt);
    }
    let repository = RepositoryId::from_bytes(decoder.array::<16>()?);
    let generation = GenerationId::from_bytes(decoder.array::<20>()?);
    let parent_present = decoder.u8()?;
    let parent_bytes = decoder.array::<20>()?;
    let parent = match parent_present {
        0 if parent_bytes == [0; 20] => None,
        1 => Some(GenerationId::from_bytes(parent_bytes)),
        _ => return Err(SegmentError::Corrupt),
    };
    let manifest_hash = ContentHash::from_bytes(decoder.array::<32>()?);
    let configuration_hash = ContentHash::from_bytes(decoder.array::<32>()?);
    let provider_set_hash = ContentHash::from_bytes(decoder.array::<32>()?);
    let mut values = [0_u64; 13];
    for value in &mut values {
        *value = decoder.u64()?;
    }
    decoder.finish()?;
    let metadata = GenerationMetadata::new_for_contract(
        contract,
        repository,
        generation,
        parent,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .map_err(|_| SegmentError::Corrupt)?;
    let stats = GenerationStats::new(
        values[0], values[1], values[2], values[3], values[4], values[5], values[6], values[7],
        values[8], values[9], values[10], values[11], values[12],
    )
    .map_err(|_| SegmentError::Corrupt)?;
    Ok((metadata, stats))
}

fn stats_values(stats: GenerationStats) -> [u64; 13] {
    [
        stats.files(),
        stats.entities(),
        stats.occurrences(),
        stats.relations(),
        stats.provenance(),
        stats.source_mappings(),
        stats.coverage(),
        stats.skipped_regions(),
        stats.diagnostics(),
        stats.extensions(),
        stats.source_refs(),
        stats.stored_rows(),
        stats.text_bytes(),
    ]
}

fn encode_file_index(indexes: &SegmentIndexes) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::new();
    for (id, ordinal) in &indexes.files {
        encoder.bytes(id.as_bytes())?;
        encoder.u64(usize_to_u64(*ordinal)?)?;
    }
    Ok(encoder.finish())
}

fn encode_entity_index(indexes: &SegmentIndexes) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::new();
    for (id, ordinal) in &indexes.entities {
        encoder.bytes(id.as_bytes())?;
        encoder.u64(usize_to_u64(*ordinal)?)?;
    }
    Ok(encoder.finish())
}

fn encode_relation_index(
    indexes: &SegmentIndexes,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::new();
    encode_relation_direction(
        0,
        &indexes.outgoing_relations,
        document,
        context,
        &mut encoder,
    )?;
    encode_relation_direction(
        1,
        &indexes.incoming_relations,
        document,
        context,
        &mut encoder,
    )?;
    Ok(encoder.finish())
}

fn encode_relation_direction(
    direction: u8,
    index: &BTreeMap<RelationEndpoint, Vec<usize>>,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
    encoder: &mut Encoder,
) -> Result<(), SegmentError> {
    for (endpoint, ordinals) in index {
        for ordinal in ordinals {
            context.check()?;
            let record = document
                .relations
                .get(*ordinal)
                .ok_or(SegmentError::Encoding)?;
            encoder.u8(direction)?;
            encode_endpoint(*endpoint, encoder)?;
            encoder.bytes(record.id.as_bytes())?;
            encoder.u64(usize_to_u64(*ordinal)?)?;
        }
    }
    Ok(())
}

fn encode_occurrence_index(
    indexes: &SegmentIndexes,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::new();
    for (file, ordinals) in &indexes.occurrences {
        for ordinal in ordinals {
            context.check()?;
            let record = document
                .occurrences
                .get(*ordinal)
                .ok_or(SegmentError::Encoding)?;
            encoder.bytes(file.as_bytes())?;
            encoder.bytes(record.id.as_bytes())?;
            encoder.u64(usize_to_u64(*ordinal)?)?;
        }
    }
    Ok(encoder.finish())
}

fn encode_provenance_index(indexes: &SegmentIndexes) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::new();
    for (id, ordinal) in &indexes.provenance {
        encoder.bytes(id.as_bytes())?;
        encoder.u64(usize_to_u64(*ordinal)?)?;
    }
    Ok(encoder.finish())
}

fn encode_coverage_index(
    indexes: &SegmentIndexes,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<Vec<u8>, SegmentError> {
    let mut encoder = Encoder::new();
    for (scope, ordinals) in &indexes.coverage {
        for ordinal in ordinals {
            context.check()?;
            let record = document
                .coverage_records
                .get(*ordinal)
                .ok_or(SegmentError::Encoding)?;
            encode_scope(*scope, &mut encoder)?;
            encoder.bytes(record.id.as_bytes())?;
            encoder.u64(usize_to_u64(*ordinal)?)?;
        }
    }
    Ok(encoder.finish())
}

fn encode_endpoint(endpoint: RelationEndpoint, encoder: &mut Encoder) -> Result<(), SegmentError> {
    match endpoint {
        RelationEndpoint::Repository(id) => {
            encoder.u8(0)?;
            encoder.bytes(id.as_bytes())?;
            encoder.bytes(&[0; 4])
        }
        RelationEndpoint::File(id) => {
            encoder.u8(1)?;
            encoder.bytes(id.as_bytes())
        }
        RelationEndpoint::Entity(id) => {
            encoder.u8(2)?;
            encoder.bytes(id.as_bytes())
        }
        RelationEndpoint::Occurrence(id) => {
            encoder.u8(3)?;
            encoder.bytes(id.as_bytes())
        }
    }
}

fn encode_scope(scope: CoverageScope, encoder: &mut Encoder) -> Result<(), SegmentError> {
    match scope {
        CoverageScope::Repository(id) => {
            encoder.u8(0)?;
            encoder.bytes(id.as_bytes())?;
            encoder.bytes(&[0; 4])
        }
        CoverageScope::File(id) => {
            encoder.u8(1)?;
            encoder.bytes(id.as_bytes())
        }
        CoverageScope::Entity(id) => {
            encoder.u8(2)?;
            encoder.bytes(id.as_bytes())
        }
    }
}

fn encode_json(
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<Vec<u8>, SegmentError> {
    let mut writer = CheckedWriter::new(context);
    let result = serde_json::to_writer(&mut writer, document);
    match (result, writer.failure.take()) {
        (Ok(()), None) => {
            context.check()?;
            Ok(writer.bytes)
        }
        (_, Some(WriterFailure::Control(error))) => Err(SegmentError::Control(error)),
        (_, Some(WriterFailure::TooLarge)) => Err(SegmentError::TooLarge {
            maximum: MAX_SEGMENT_BYTES,
        }),
        (_, Some(WriterFailure::Allocation)) => Err(SegmentError::Allocation),
        (Err(_), None) => Err(SegmentError::Encoding),
    }
}

enum WriterFailure {
    Control(rootlight_storage::GenerationControlError),
    TooLarge,
    Allocation,
}

struct CheckedWriter<'a, 'b> {
    bytes: Vec<u8>,
    context: &'a GenerationContext<'b>,
    failure: Option<WriterFailure>,
}

impl<'a, 'b> CheckedWriter<'a, 'b> {
    fn new(context: &'a GenerationContext<'b>) -> Self {
        Self {
            bytes: Vec::new(),
            context,
            failure: None,
        }
    }

    fn fail(&mut self, failure: WriterFailure) -> io::Error {
        self.failure = Some(failure);
        io::Error::other("bounded segment writer stopped")
    }
}

impl Write for CheckedWriter<'_, '_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("bounded segment writer already stopped"));
        }
        for chunk in buffer.chunks(CHECKPOINT_BYTES) {
            if let Err(error) = self.context.check() {
                return Err(self.fail(WriterFailure::Control(error)));
            }
            let Some(next_length) = self.bytes.len().checked_add(chunk.len()) else {
                return Err(self.fail(WriterFailure::TooLarge));
            };
            if next_length > MAX_SEGMENT_BYTES {
                return Err(self.fail(WriterFailure::TooLarge));
            }
            if self.bytes.try_reserve(chunk.len()).is_err() {
                return Err(self.fail(WriterFailure::Allocation));
            }
            self.bytes.extend_from_slice(chunk);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.context
            .check()
            .map_err(|error| self.fail(WriterFailure::Control(error)))
    }
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn with_capacity(capacity: usize) -> Result<Self, SegmentError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| SegmentError::Allocation)?;
        Ok(Self { bytes })
    }

    fn u8(&mut self, value: u8) -> Result<(), SegmentError> {
        self.bytes(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), SegmentError> {
        self.bytes(&value.to_le_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), SegmentError> {
        self.bytes(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), SegmentError> {
        self.bytes(&value.to_le_bytes())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), SegmentError> {
        let next_length =
            self.bytes
                .len()
                .checked_add(value.len())
                .ok_or(SegmentError::TooLarge {
                    maximum: MAX_SEGMENT_BYTES,
                })?;
        if next_length > MAX_SEGMENT_BYTES {
            return Err(SegmentError::TooLarge {
                maximum: MAX_SEGMENT_BYTES,
            });
        }
        self.bytes
            .try_reserve(value.len())
            .map_err(|_| SegmentError::Allocation)?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn u8(&mut self) -> Result<u8, SegmentError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, SegmentError> {
        Ok(u16::from_le_bytes(self.array::<2>()?))
    }

    fn u32(&mut self) -> Result<u32, SegmentError> {
        Ok(u32::from_le_bytes(self.array::<4>()?))
    }

    fn u64(&mut self) -> Result<u64, SegmentError> {
        Ok(u64::from_le_bytes(self.array::<8>()?))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], SegmentError> {
        let end = self.position.checked_add(N).ok_or(SegmentError::Corrupt)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SegmentError::Corrupt)?;
        self.position = end;
        value.try_into().map_err(|_| SegmentError::Corrupt)
    }

    fn finish(self) -> Result<(), SegmentError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SegmentError::Corrupt)
        }
    }
}

fn usize_to_u64(value: usize) -> Result<u64, SegmentError> {
    u64::try_from(value).map_err(|_| SegmentError::Encoding)
}
