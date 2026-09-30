//! Source-bound conditional membership for returned query records.
//! Definitions and expanded source chunks have different anchors; projection
//! remains fail-closed until bounded inventory and definition scans finish.

use std::{collections::BTreeMap, mem};

use rootlight_ids::{ContentHash, FactId, FileId, SymbolId};
use rootlight_ir::{
    ExtensionEnvelope, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerKind,
    SourceRef, SourceSpan, VBA_COMPILATION_NAMESPACE, VbaCompilation, VbaCompilationActivity,
    decode_vba_compilation_envelope,
};
use rootlight_storage::GenerationSnapshot;
use serde::Serialize;

use crate::{
    model::{LocateHit, QueryError, QueryResource, SourceChunkResult, checked_usize_to_u64},
    service::{QueryControl, UsageTracker, record_limit},
};

/// Membership in a stored, admitted conditional compilation inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalActivityState {
    /// The exact assessed range is unguarded or belongs to a selected alternative.
    Active,
    /// An excluded alternative contains the entire assessed range.
    Inactive,
    /// Evidence cannot establish one active or inactive membership.
    Unproven,
}

/// Exact source surface to which a membership assessment applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalActivityBasis {
    /// The unique source-written definition token, not its containing body.
    DefinitionToken,
    /// A file-only locate source or an actual expanded returned source chunk.
    ReturnedRange,
}

/// Why the returned source's conditional membership remains unproven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalActivityReason {
    /// No retained inventory owns the assessed range.
    NoInventory,
    /// The retained scope does not have a complete branch inventory.
    IncompleteInventory,
    /// No exact definition token resolving to the returned symbol is retained.
    MissingDefinition,
    /// More than one definition token resolves to the returned symbol.
    AmbiguousDefinition,
    /// The range crosses the bounds of separate source-written scopes.
    CrossScope,
    /// Empty ranges cannot establish source-written membership.
    EmptyRange,
    /// A complete inventory retains unknown conditions or the range crosses guards.
    MembershipUnproven,
    /// A resource ceiling stopped the correctness-bearing projection scan.
    ScanIncomplete,
    /// The serving transport does not support conditional membership metadata.
    TransportUnavailable,
}

/// Nonempty inventory bounds inheriting the assessed source's exact file identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConditionalActivityScope {
    /// Inclusive scope start in the assessed source's indexed file.
    pub start_byte: u64,
    /// Exclusive scope end in the assessed source's indexed file.
    pub end_byte: u64,
}

/// Exact persisted inventory and parser attribution for one owning source scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConditionalActivityInventory {
    /// Fact identity of the retained compilation extension envelope.
    pub fact_id: FactId,
    /// Exact parser provenance referenced by that envelope.
    pub parser_provenance_id: FactId,
    /// Owning byte range with the assessed source's repository/generation/file/hash.
    pub scope: ConditionalActivityScope,
    /// Whether every conditional branch is inventoried, not whole-program validity.
    pub complete: bool,
}

/// A source-attributed conditional assessment of one returned item ordinal.
///
/// This projects admitted stored evidence, not producer authenticity, host
/// configuration, runtime execution or semantic confidence. Null attribution
/// never proves activity; active/inactive always carry a nonempty exact source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConditionalActivityAssessment {
    /// Zero-based ordinal into the returned hits or chunks, not original selectors.
    pub result_index: u32,
    /// The source surface assessed independently of whole-entity membership.
    pub basis: ConditionalActivityBasis,
    /// Activity established by the owning stored inventory, or explicit uncertainty.
    pub activity: ConditionalActivityState,
    /// Exact assessed token/range, absent when no unique definition is established.
    pub assessed_source_ref: Option<SourceRef>,
    /// Unique owning inventory, absent for unavailable or incomplete scan evidence.
    pub inventory: Option<ConditionalActivityInventory>,
    /// Required for unproven assessments and absent for active/inactive membership.
    pub reason: Option<ConditionalActivityReason>,
}

pub(super) struct ActivityProjection {
    pub assessments: Vec<ConditionalActivityAssessment>,
    pub incomplete: bool,
}

struct ActivityRequest {
    result_index: u32,
    source: SourceRef,
    symbol: Option<SymbolId>,
}

#[derive(Default)]
struct DefinitionAnchor {
    source: Option<SourceRef>,
    ambiguous: bool,
}

struct Inventory {
    model: VbaCompilation,
    fact_id: FactId,
    parser_provenance_id: FactId,
}

#[derive(Clone, Copy)]
struct SelectedFile {
    content_hash: ContentHash,
    byte_length: u64,
}

struct ProjectionBudget<'tracker, 'control, 'cancellation> {
    tracker: &'tracker mut UsageTracker,
    control: &'control QueryControl<'cancellation>,
    limiting_resources: &'tracker mut Vec<QueryResource>,
}

impl ProjectionBudget<'_, '_, '_> {
    fn rows(&mut self, amount: u64) -> Result<bool, QueryError> {
        self.control.check()?;
        if !self.tracker.can_add(QueryResource::Rows, amount) {
            record_limit(self.limiting_resources, QueryResource::Rows)?;
            return Ok(false);
        }
        self.tracker.add_rows(amount)?;
        Ok(true)
    }

    fn memory(&mut self, amount: u64) -> Result<bool, QueryError> {
        self.control.check()?;
        if !self.tracker.can_add(QueryResource::MemoryBytes, amount) {
            record_limit(self.limiting_resources, QueryResource::MemoryBytes)?;
            return Ok(false);
        }
        self.tracker.add_memory(amount)?;
        Ok(true)
    }
}

pub(super) fn applicable(language: &str) -> bool {
    matches!(language, "vba" | "visual-basic-classic")
}

pub(super) fn mandatory_memory(count: usize) -> Result<u64, QueryError> {
    // Charge complete singleton tree nodes, not amortized occupancy. Sixteen
    // key/value/link slots exceed the standard tree's eleven-key node capacity;
    // this also covers a selected symbol's independently cloned definition ref.
    let tree_slots = mem::size_of::<(FileId, SelectedFile)>()
        .checked_add(mem::size_of::<(SymbolId, (&SourceRef, DefinitionAnchor))>())
        .and_then(|bytes| bytes.checked_add(4 * mem::size_of::<usize>()))
        .and_then(|bytes| bytes.checked_mul(16))
        .ok_or(QueryError::MemoryUnavailable)?;
    let per_item = mem::size_of::<ActivityRequest>()
        .checked_add(mem::size_of::<ConditionalActivityAssessment>())
        .and_then(|bytes| bytes.checked_add(tree_slots))
        .ok_or(QueryError::MemoryUnavailable)?;
    checked_usize_to_u64(
        count
            .checked_mul(per_item)
            .ok_or(QueryError::MemoryUnavailable)?,
    )
}

pub(super) fn source_reservation(
    generation: &GenerationSnapshot,
    references: &[SourceRef],
) -> Result<u64, QueryError> {
    mandatory_memory(
        references
            .iter()
            .filter(|source| {
                generation
                    .find_file(source.span().file())
                    .is_some_and(|file| applicable(&file.language))
            })
            .count(),
    )
}

pub(super) fn project_locate(
    generation: &GenerationSnapshot,
    hits: &[LocateHit],
    tracker: &mut UsageTracker,
    control: &QueryControl<'_>,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<ActivityProjection, QueryError> {
    let count = hits.iter().filter(|hit| applicable(&hit.language)).count();
    tracker.add_memory(mandatory_memory(count)?)?;
    let mut requests = Vec::new();
    requests
        .try_reserve_exact(count)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    for (index, hit) in hits.iter().enumerate() {
        control.check()?;
        if applicable(&hit.language) {
            let source = hit.source.as_ref().ok_or(QueryError::IndexDrift)?;
            validate_request(generation, source)?;
            requests.push(ActivityRequest {
                result_index: u32::try_from(index).map_err(|_| QueryError::MemoryUnavailable)?,
                source: source.clone(),
                symbol: hit.symbol,
            });
        }
    }
    project_document(generation, &requests, tracker, control, limiting_resources)
}

pub(super) fn project_source(
    generation: &GenerationSnapshot,
    chunks: &[SourceChunkResult],
    tracker: &mut UsageTracker,
    control: &QueryControl<'_>,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<ActivityProjection, QueryError> {
    let count = chunks
        .iter()
        .filter(|chunk| applicable(&chunk.language))
        .count();
    tracker.add_memory(mandatory_memory(count)?)?;
    let mut requests = Vec::new();
    requests
        .try_reserve_exact(count)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    for (index, chunk) in chunks.iter().enumerate() {
        control.check()?;
        if applicable(&chunk.language) {
            let source = SourceRef::new(
                chunk.reference.repository(),
                chunk.reference.generation(),
                SourceSpan::new(
                    chunk.reference.span().file(),
                    chunk.start_byte,
                    chunk.end_byte,
                )
                .map_err(|_| QueryError::IndexDrift)?,
                chunk.content_hash,
                None,
            );
            validate_request(generation, &source)?;
            requests.push(ActivityRequest {
                result_index: u32::try_from(index).map_err(|_| QueryError::MemoryUnavailable)?,
                source,
                symbol: None,
            });
        }
    }
    project_document(generation, &requests, tracker, control, limiting_resources)
}

fn validate_request(generation: &GenerationSnapshot, source: &SourceRef) -> Result<(), QueryError> {
    let file = generation
        .find_file(source.span().file())
        .ok_or(QueryError::IndexDrift)?;
    if source.repository() != generation.metadata().repository()
        || source.generation() != generation.metadata().generation()
        || source.content_hash() != file.content_hash
        || source.span().end_byte() > file.byte_length
    {
        return Err(QueryError::IndexDrift);
    }
    Ok(())
}

fn unavailable(
    request: &ActivityRequest,
    reason: ConditionalActivityReason,
) -> ConditionalActivityAssessment {
    ConditionalActivityAssessment {
        result_index: request.result_index,
        basis: if request.symbol.is_some() {
            ConditionalActivityBasis::DefinitionToken
        } else {
            ConditionalActivityBasis::ReturnedRange
        },
        activity: ConditionalActivityState::Unproven,
        assessed_source_ref: request.symbol.is_none().then(|| request.source.clone()),
        inventory: None,
        reason: Some(reason),
    }
}

fn incomplete(
    requests: &[ActivityRequest],
    mut assessments: Vec<ConditionalActivityAssessment>,
) -> ActivityProjection {
    for (request, assessment) in requests.iter().zip(&mut assessments) {
        *assessment = unavailable(request, ConditionalActivityReason::ScanIncomplete);
    }
    ActivityProjection {
        assessments,
        incomplete: true,
    }
}

fn project_document(
    generation: &GenerationSnapshot,
    requests: &[ActivityRequest],
    tracker: &mut UsageTracker,
    control: &QueryControl<'_>,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<ActivityProjection, QueryError> {
    let document = generation.document();
    let mut assessments = Vec::new();
    assessments
        .try_reserve_exact(requests.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    if requests.is_empty() {
        return Ok(ActivityProjection {
            assessments,
            incomplete: false,
        });
    }
    let mut files = BTreeMap::<FileId, SelectedFile>::new();
    let mut definitions = BTreeMap::<SymbolId, (&SourceRef, DefinitionAnchor)>::new();
    for request in requests {
        control.check()?;
        let file = generation
            .find_file(request.source.span().file())
            .ok_or(QueryError::IndexDrift)?;
        if files
            .insert(
                request.source.span().file(),
                SelectedFile {
                    content_hash: request.source.content_hash(),
                    byte_length: file.byte_length,
                },
            )
            .is_some_and(|previous| previous.content_hash != request.source.content_hash())
        {
            return Err(QueryError::IndexDrift);
        }
        if let Some(symbol) = request.symbol {
            definitions
                .entry(symbol)
                .or_insert((&request.source, DefinitionAnchor::default()));
        }
        assessments.push(unavailable(
            request,
            ConditionalActivityReason::ScanIncomplete,
        ));
    }
    let mut budget = ProjectionBudget {
        tracker,
        control,
        limiting_resources,
    };
    let mut inventories = BTreeMap::<(FileId, u64, u64), Inventory>::new();
    let mut decoder_high_water = 0;
    for envelope in &document.extensions {
        if !budget.rows(1)? {
            return Ok(incomplete(requests, assessments));
        }
        if envelope.namespace != VBA_COMPILATION_NAMESPACE {
            continue;
        }
        let source = envelope
            .evidence
            .source
            .as_ref()
            .ok_or(QueryError::IndexDrift)?;
        if !files.contains_key(&source.span().file()) {
            continue;
        }
        let decode_memory = decoder_memory(envelope.payload.len())?;
        if !budget.memory(decode_memory.saturating_sub(decoder_high_water))?
            || !budget
                .rows(checked_usize_to_u64(rootlight_ir::MAX_VBA_COMPILATION_BRANCHES)? + 1)?
        {
            return Ok(incomplete(requests, assessments));
        }
        decoder_high_water = decoder_high_water.max(decode_memory);
        let model =
            decode_vba_compilation_envelope(envelope).map_err(|_| QueryError::IndexDrift)?;
        control.check()?;
        if !validate_inventory(document, &files, envelope, &model, &mut budget)? {
            return Ok(incomplete(requests, assessments));
        }
        let scope = model.scope;
        let key = (scope.file(), scope.start_byte(), scope.end_byte());
        validate_scope(&inventories, key)?;
        let retained_memory = mem::size_of::<((FileId, u64, u64), Inventory)>()
            .checked_add(2 * mem::size_of::<usize>())
            .and_then(|bytes| bytes.checked_mul(16))
            .and_then(|bytes| {
                bytes.checked_add(
                    model
                        .branches
                        .capacity()
                        .checked_mul(mem::size_of::<rootlight_ir::VbaCompilationBranch>())?,
                )
            })
            .ok_or(QueryError::MemoryUnavailable)?;
        if !budget.memory(checked_usize_to_u64(retained_memory)?)? {
            return Ok(incomplete(requests, assessments));
        }
        inventories.insert(
            key,
            Inventory {
                model,
                fact_id: envelope.id,
                parser_provenance_id: envelope.provenance,
            },
        );
    }
    if !definitions.is_empty() {
        for occurrence in &document.occurrences {
            if !budget.rows(1)? {
                return Ok(incomplete(requests, assessments));
            }
            if occurrence.role != OccurrenceRole::Definition {
                continue;
            }
            let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                continue;
            };
            let Some((entity_source, anchor)) = definitions.get_mut(&symbol) else {
                continue;
            };
            let source = &occurrence.source;
            if occurrence.repository != document.repository
                || occurrence.generation != document.generation
                || occurrence.file != source.span().file()
                || !same_identity(entity_source, source)
                || !contains(entity_source.span(), source.span())
                || source.span().start_byte() == source.span().end_byte()
            {
                return Err(QueryError::IndexDrift);
            }
            if anchor.source.is_some() {
                anchor.ambiguous = true;
            } else {
                anchor.source = Some(source.clone());
            }
        }
    }
    for (request, assessment) in requests.iter().zip(&mut assessments) {
        if !budget.rows(1)? {
            return Ok(incomplete(requests, assessments));
        }
        let assessed = if let Some(symbol) = request.symbol {
            let (_, anchor) = definitions.get(&symbol).ok_or(QueryError::IndexDrift)?;
            if anchor.ambiguous {
                *assessment = unavailable(request, ConditionalActivityReason::AmbiguousDefinition);
                continue;
            }
            let Some(source) = &anchor.source else {
                *assessment = unavailable(request, ConditionalActivityReason::MissingDefinition);
                continue;
            };
            source
        } else {
            &request.source
        };
        assessment.assessed_source_ref = Some(assessed.clone());
        let span = assessed.span();
        if span.start_byte() == span.end_byte() {
            assessment.reason = Some(ConditionalActivityReason::EmptyRange);
            continue;
        }
        let mut selected = None;
        let mut overlaps = false;
        for (_, inventory) in
            inventories.range((span.file(), 0, 0)..=(span.file(), u64::MAX, u64::MAX))
        {
            if !budget.rows(1)? {
                return Ok(incomplete(requests, assessments));
            }
            let scope = inventory.model.scope;
            overlaps |=
                scope.start_byte() < span.end_byte() && span.start_byte() < scope.end_byte();
            if contains(scope, span) {
                if selected.is_some() {
                    return Err(QueryError::IndexDrift);
                }
                selected = Some(inventory);
            }
        }
        let Some(inventory) = selected else {
            assessment.reason = Some(if overlaps {
                ConditionalActivityReason::CrossScope
            } else {
                ConditionalActivityReason::NoInventory
            });
            continue;
        };
        // One containment pass, group partition probes and per-group membership
        // probes inspect at most four branch records plus one guard per branch.
        let activity_rows = checked_usize_to_u64(inventory.model.branches.len())?
            .checked_mul(4)
            .and_then(|rows| rows.checked_add(1))
            .ok_or(QueryError::MemoryUnavailable)?;
        if !budget.rows(activity_rows)? {
            return Ok(incomplete(requests, assessments));
        }
        assessment.inventory = Some(ConditionalActivityInventory {
            fact_id: inventory.fact_id,
            parser_provenance_id: inventory.parser_provenance_id,
            scope: ConditionalActivityScope {
                start_byte: inventory.model.scope.start_byte(),
                end_byte: inventory.model.scope.end_byte(),
            },
            complete: inventory.model.complete,
        });
        assessment.activity = match inventory.model.activity_at(span) {
            VbaCompilationActivity::Active => ConditionalActivityState::Active,
            VbaCompilationActivity::Inactive => ConditionalActivityState::Inactive,
            VbaCompilationActivity::Unproven => ConditionalActivityState::Unproven,
        };
        assessment.reason = (assessment.activity == ConditionalActivityState::Unproven).then_some(
            if inventory.model.complete {
                ConditionalActivityReason::MembershipUnproven
            } else {
                ConditionalActivityReason::IncompleteInventory
            },
        );
        control.check()?;
    }
    Ok(ActivityProjection {
        assessments,
        incomplete: false,
    })
}

fn validate_inventory(
    document: &NormalizedIrDocument,
    files: &BTreeMap<FileId, SelectedFile>,
    envelope: &ExtensionEnvelope,
    model: &VbaCompilation,
    budget: &mut ProjectionBudget<'_, '_, '_>,
) -> Result<bool, QueryError> {
    let source = envelope
        .evidence
        .source
        .as_ref()
        .ok_or(QueryError::IndexDrift)?;
    let file = files
        .get(&model.scope.file())
        .ok_or(QueryError::IndexDrift)?;
    if envelope.repository != document.repository
        || envelope.generation != document.generation
        || source.repository() != document.repository
        || source.generation() != document.generation
        || file.content_hash != source.content_hash()
        || model.scope.end_byte() > file.byte_length
        || source.span() != model.scope
    {
        return Err(QueryError::IndexDrift);
    }
    if !budget.rows(1)? {
        return Ok(false);
    }
    let parser = document
        .provenance
        .binary_search_by_key(&envelope.provenance, |record| record.id)
        .ok()
        .and_then(|index| document.provenance.get(index))
        .ok_or(QueryError::IndexDrift)?;
    if parser.repository != document.repository
        || parser.generation != document.generation
        || parser.producer_kind != ProducerKind::Parser
    {
        return Err(QueryError::IndexDrift);
    }
    let mut matching_input = false;
    for input in &parser.input_sources {
        if !budget.rows(1)? {
            return Ok(false);
        }
        if same_identity(input, source) {
            if input.span().end_byte() > file.byte_length {
                return Err(QueryError::IndexDrift);
            }
            matching_input |= contains(input.span(), model.scope);
        }
    }
    if !matching_input {
        return Err(QueryError::IndexDrift);
    }
    Ok(true)
}

fn validate_scope(
    inventories: &BTreeMap<(FileId, u64, u64), Inventory>,
    key: (FileId, u64, u64),
) -> Result<(), QueryError> {
    if inventories
        .range(..key)
        .next_back()
        .is_some_and(|(previous, _)| previous.0 == key.0 && previous.2 > key.1)
        || inventories
            .range(key..)
            .next()
            .is_some_and(|(next, _)| next.0 == key.0 && key.2 > next.1)
    {
        return Err(QueryError::IndexDrift);
    }
    Ok(())
}

fn decoder_memory(payload_bytes: usize) -> Result<u64, QueryError> {
    // The decoder simultaneously retains parsed and cloned branch vectors,
    // two validation trees, group vectors and ordered/ancestor vectors. Charge
    // sixteen full slots per possible singleton tree node and both model
    // vectors; sixteen reference-sized slots cover the group/ordered/ancestor
    // vectors. Four payload copies cover canonical payload/identity strings
    // and transient text decoding. This deliberately overcounts tree occupancy,
    // but reserves reusable scratch once rather than once per retained model.
    let per_branch = 2 * mem::size_of::<rootlight_ir::VbaCompilationBranch>()
        + 16 * (2 * mem::size_of::<SourceSpan>()
            + mem::size_of::<Vec<&rootlight_ir::VbaCompilationBranch>>()
            + 4 * mem::size_of::<usize>())
        + 16 * mem::size_of::<usize>();
    let branch_scratch = rootlight_ir::MAX_VBA_COMPILATION_BRANCHES
        .checked_mul(per_branch)
        .ok_or(QueryError::MemoryUnavailable)?;
    checked_usize_to_u64(payload_bytes)?
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(u64::try_from(branch_scratch).ok()?))
        .and_then(|bytes| bytes.checked_add(4096))
        .ok_or(QueryError::MemoryUnavailable)
}

fn same_identity(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && inner.end_byte() <= outer.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::{GenerationIdentity, content_hash, derive_generation, derive_repository};
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, ExtensionSupport, FactEvidence, FileRecord, IrLimits,
        ProducerIdentity, ProvenanceRecord, VbaCompilationBranch, new_vba_compilation_envelope,
    };
    use rootlight_storage::{GENERATION_CONTRACT_VERSION, GenerationMetadata};
    use std::time::Duration;

    use crate::{QueryBudget, RepositoryDataTrust, SourceChunkEncoding};

    fn fixture() -> (GenerationSnapshot, SourceRef) {
        let repository = derive_repository(b"conditional-query-unit").id();
        let hash = content_hash(b"conditional-query-inputs");
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: hash,
            config_hash: hash,
            provider_set_hash: hash,
            format_version: u32::from(GENERATION_CONTRACT_VERSION.major()) << 16
                | u32::from(GENERATION_CONTRACT_VERSION.minor()),
        })
        .id();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 100).unwrap(),
            hash,
            None,
        );
        let parser = FactId::from_bytes([4; 20]);
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.files.push(FileRecord {
            id: source.span().file(),
            repository,
            generation,
            path: "Module.bas".to_owned(),
            path_locator: None,
            content_hash: hash,
            byte_length: 100,
            language: "vba".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: parser,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
        document.provenance.push(ProvenanceRecord {
            id: parser,
            repository,
            generation,
            producer_kind: ProducerKind::Parser,
            producer: ProducerIdentity::new("rootlight-query-test", "1.0.0", hash).unwrap(),
            binary_digest: hash,
            frontend_version: None,
            language: "vba".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(hash),
            input_sources: vec![source.clone()],
            evidence_sources: vec![source.clone()],
            derivation_parents: Vec::new(),
            rule: None,
        });
        document.extensions.push(
            new_vba_compilation_envelope(
                repository,
                generation,
                parser,
                source.clone(),
                VbaCompilation {
                    scope: source.span(),
                    complete: true,
                    branches: Vec::new(),
                },
            )
            .unwrap(),
        );
        let metadata =
            GenerationMetadata::new(repository, generation, None, hash, hash, hash).unwrap();
        (
            GenerationSnapshot::new(
                metadata,
                document,
                &IrLimits::default(),
                &ExtensionSupport::default(),
            )
            .unwrap(),
            source,
        )
    }

    fn chunk(source: SourceRef, language: &str) -> SourceChunkResult {
        SourceChunkResult {
            start_byte: source.span().start_byte(),
            end_byte: source.span().end_byte(),
            start_line: Some(1),
            end_line: Some(1),
            bytes: Vec::new(),
            encoding: SourceChunkEncoding::Utf8,
            content_hash: source.content_hash(),
            reference: source,
            path: "Module.bas".to_owned(),
            language: language.to_owned(),
            generated: false,
            extracted_from: None,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        }
    }

    #[test]
    fn conditional_activity_memory_reservations_have_exact_and_one_short_boundaries() {
        let (snapshot, source) = fixture();
        let chunks = [chunk(source, "vba")];
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(2));
        let mandatory = mandatory_memory(1).unwrap();
        let scratch = decoder_memory(snapshot.document().extensions[0].payload.len()).unwrap();
        for (limit, expected_error, expected_partial) in [
            (mandatory - 1, true, false),
            (mandatory, false, true),
            (mandatory + scratch - 1, false, true),
            (mandatory + scratch, false, true),
        ] {
            let mut tracker = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(limit));
            let mut resources = Vec::new();
            let result = project_source(&snapshot, &chunks, &mut tracker, &control, &mut resources);
            if expected_error {
                assert!(
                    matches!(result, Err(QueryError::BudgetExceeded { resource: QueryResource::MemoryBytes, limit: actual }) if actual == limit)
                );
            } else {
                let projection = result.unwrap();
                assert_eq!(projection.incomplete, expected_partial);
                assert_eq!(resources, vec![QueryResource::MemoryBytes]);
                assert_eq!(
                    projection.assessments[0].reason,
                    Some(ConditionalActivityReason::ScanIncomplete)
                );
                assert!(projection.assessments[0].inventory.is_none());
            }
        }
        let mut full_tracker = UsageTracker::new(QueryBudget::new());
        let full = project_source(
            &snapshot,
            &chunks,
            &mut full_tracker,
            &control,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(!full.incomplete);
        assert_eq!(
            full.assessments[0].activity,
            ConditionalActivityState::Active
        );
        assert!(full_tracker.can_add(QueryResource::MemoryBytes, 1));
        let mut tracker = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(scratch));
        let mut resources = Vec::new();
        let mut budget = ProjectionBudget {
            tracker: &mut tracker,
            control: &control,
            limiting_resources: &mut resources,
        };
        assert!(budget.memory(scratch).unwrap());
        assert!(!budget.memory(1).unwrap());
        assert_eq!(resources, vec![QueryResource::MemoryBytes]);
    }

    #[test]
    fn conditional_activity_inventory_admission_rechecks_file_bounds_and_parser_ownership() {
        let (snapshot, source) = fixture();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(2));
        let files = BTreeMap::from([(
            source.span().file(),
            SelectedFile {
                content_hash: source.content_hash(),
                byte_length: 100,
            },
        )]);
        for mode in [
            "oversized_scope",
            "foreign_input",
            "non_parser",
            "foreign_owner",
            "stale_hash",
        ] {
            let mut document = snapshot.document().clone();
            let mut envelope = document.extensions[0].clone();
            if mode == "oversized_scope" {
                let oversized = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    SourceSpan::new(source.span().file(), 0, 101).unwrap(),
                    source.content_hash(),
                    None,
                );
                document.provenance[0].input_sources = vec![oversized.clone()];
                envelope = new_vba_compilation_envelope(
                    document.repository,
                    document.generation,
                    envelope.provenance,
                    oversized.clone(),
                    VbaCompilation {
                        scope: oversized.span(),
                        complete: true,
                        branches: Vec::new(),
                    },
                )
                .unwrap();
            }
            if mode == "foreign_input" {
                document.provenance[0].input_sources.clear();
            }
            if mode == "non_parser" {
                document.provenance[0].producer_kind = ProducerKind::Rule;
            }
            if mode == "foreign_owner" {
                document.provenance[0].repository = derive_repository(b"foreign").id();
            }
            if mode == "stale_hash" {
                let stale = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    source.span(),
                    content_hash(b"stale"),
                    None,
                );
                envelope = new_vba_compilation_envelope(
                    document.repository,
                    document.generation,
                    envelope.provenance,
                    stale,
                    VbaCompilation {
                        scope: source.span(),
                        complete: true,
                        branches: Vec::new(),
                    },
                )
                .unwrap();
            }
            let model = decode_vba_compilation_envelope(&envelope).unwrap();
            let mut tracker = UsageTracker::new(QueryBudget::new());
            let mut resources = Vec::new();
            let mut budget = ProjectionBudget {
                tracker: &mut tracker,
                control: &control,
                limiting_resources: &mut resources,
            };
            assert!(
                matches!(
                    validate_inventory(&document, &files, &envelope, &model, &mut budget),
                    Err(QueryError::IndexDrift)
                ),
                "{mode}"
            );
        }
    }

    #[test]
    fn conditional_activity_projection_preserves_ordinals_empty_ranges_and_cancellation() {
        let (snapshot, source) = fixture();
        let empty = SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(source.span().file(), 10, 10).unwrap(),
            source.content_hash(),
            None,
        );
        let chunks = [
            chunk(source.clone(), "rust"),
            chunk(empty.clone(), "vba"),
            chunk(source, "visual-basic-classic"),
        ];
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(2));
        let projection = project_source(
            &snapshot,
            &chunks,
            &mut UsageTracker::new(QueryBudget::new()),
            &control,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(projection.assessments.len(), 2);
        assert_eq!(projection.assessments[0].result_index, 1);
        assert_eq!(
            projection.assessments[0].reason,
            Some(ConditionalActivityReason::EmptyRange)
        );
        assert_eq!(projection.assessments[0].assessed_source_ref, Some(empty));
        assert!(projection.assessments[0].inventory.is_none());
        assert_eq!(projection.assessments[1].result_index, 2);
        assert_eq!(
            projection.assessments[1].activity,
            ConditionalActivityState::Active
        );
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            project_source(
                &snapshot,
                &chunks,
                &mut UsageTracker::new(QueryBudget::new()),
                &control,
                &mut Vec::new()
            ),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        let fresh = Cancellation::new();
        let expired = QueryControl::new(&fresh, Duration::ZERO);
        assert!(matches!(
            project_source(
                &snapshot,
                &chunks,
                &mut UsageTracker::new(QueryBudget::new()),
                &expired,
                &mut Vec::new()
            ),
            Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded))
        ));
    }

    #[test]
    fn conditional_activity_late_row_stop_discards_already_classified_membership() {
        let (snapshot, source) = fixture();
        let chunks = [chunk(source.clone(), "vba"), chunk(source, "vba")];
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(2));
        // Envelope + bounded decoder + parser lookup/input + one complete
        // classification. The second classification cannot begin; the first
        // must not escape this incomplete transaction as Active.
        let exact_first =
            1 + u64::try_from(rootlight_ir::MAX_VBA_COMPILATION_BRANCHES).unwrap() + 1 + 1 + 1 + 3;
        let mut resources = Vec::new();
        let projection = project_source(
            &snapshot,
            &chunks,
            &mut UsageTracker::new(QueryBudget::new().with_max_rows(exact_first)),
            &control,
            &mut resources,
        )
        .unwrap();
        assert!(projection.incomplete);
        assert_eq!(resources, vec![QueryResource::Rows]);
        assert_eq!(projection.assessments.len(), 2);
        for assessment in projection.assessments {
            assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
            assert_eq!(
                assessment.reason,
                Some(ConditionalActivityReason::ScanIncomplete)
            );
            assert!(assessment.assessed_source_ref.is_some());
            assert!(assessment.inventory.is_none());
        }
    }

    #[test]
    fn conditional_activity_conflicting_scopes_fail_closed_and_inactive_ancestors_win() {
        let (snapshot, source) = fixture();
        let mut document = snapshot.document().clone();
        document.extensions.push(
            new_vba_compilation_envelope(
                document.repository,
                document.generation,
                document.provenance[0].id,
                source.clone(),
                VbaCompilation {
                    scope: source.span(),
                    complete: false,
                    branches: Vec::new(),
                },
            )
            .unwrap(),
        );
        let conflicting = GenerationSnapshot::new(
            snapshot.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        );
        assert!(matches!(
            conflicting,
            Err(rootlight_storage::GenerationValidationError::InvalidIr(
                rootlight_ir::IrDocumentValidationError::InvalidVbaCompilation(_)
            ))
        ));
        let envelope = &snapshot.document().extensions[0];
        let model = decode_vba_compilation_envelope(envelope).unwrap();
        let scope = model.scope;
        let inventories = BTreeMap::from([(
            (scope.file(), scope.start_byte(), scope.end_byte()),
            Inventory {
                model,
                fact_id: envelope.id,
                parser_provenance_id: envelope.provenance,
            },
        )]);
        assert!(matches!(
            validate_scope(
                &inventories,
                (scope.file(), scope.start_byte(), scope.end_byte())
            ),
            Err(QueryError::IndexDrift)
        ));
        assert!(matches!(
            validate_scope(&inventories, (scope.file(), 25, 75)),
            Err(QueryError::IndexDrift)
        ));
        assert!(validate_scope(&inventories, (scope.file(), 100, 110)).is_ok());
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(2));
        let span = |start, end| SourceSpan::new(source.span().file(), start, end).unwrap();
        let mut document = snapshot.document().clone();
        document.extensions[0] = new_vba_compilation_envelope(
            document.repository,
            document.generation,
            document.provenance[0].id,
            source.clone(),
            VbaCompilation {
                scope: source.span(),
                complete: true,
                branches: vec![
                    VbaCompilationBranch {
                        group: span(10, 80),
                        region: span(15, 75),
                        activity: VbaCompilationActivity::Inactive,
                    },
                    VbaCompilationBranch {
                        group: span(20, 40),
                        region: span(25, 35),
                        activity: VbaCompilationActivity::Unproven,
                    },
                ],
            },
        )
        .unwrap();
        let nested = GenerationSnapshot::new(
            snapshot.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let selected = SourceRef::new(
            source.repository(),
            source.generation(),
            span(26, 27),
            source.content_hash(),
            None,
        );
        let projection = project_source(
            &nested,
            &[chunk(selected, "vba")],
            &mut UsageTracker::new(QueryBudget::new()),
            &control,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            projection.assessments[0].activity,
            ConditionalActivityState::Inactive
        );
        assert_eq!(projection.assessments[0].reason, None);
    }
}
