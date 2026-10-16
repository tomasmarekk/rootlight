use std::{
    collections::{BTreeSet, HashSet},
    time::Instant,
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, SymbolId};
use rootlight_ir::{ContainerRef, EntityFlag, EntityKind, EntityRecord, NormalizedIrDocument};
use rootlight_search::{
    BuildBudget, LexicalDocument, SearchBudget, SearchError, SymbolNameProjection,
    TextQueryCandidate, project_query_source_identifier, project_source_terms_with_coverage,
    project_symbol_name_projection, select_query_source_bytes, validate_build_admission,
};
use rootlight_storage::GenerationSnapshot;
use rootlight_vfs::{SourceSnapshot, SourceText};
use serde::Serialize;

use crate::QueryError;

/// Maximum UTF-8 source prefix admitted to one file-only fallback document.
pub const SOURCE_FALLBACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_SOURCE_IDENTIFIERS: usize = 4_096;
const MAX_SOURCE_IDENTIFIER_BYTES: usize = 240;

/// Opt-in numeric observations of one scoped Text projection, including failure.
///
/// Durations are wall microseconds. The candidate and entity-materialization
/// durations are included in signature visitation. No source text, identifiers,
/// paths or hashes are retained. A new invocation resets all observations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ScopedTextProjectionObservation {
    /// Time selecting query-relevant source words.
    pub source_selection_micros: u128,
    /// Time collecting scoped entity IDs and allocating their signature index.
    pub signature_collection_micros: u128,
    /// Time examining extension envelopes and selecting retained signatures.
    pub signature_observation_micros: u128,
    /// Time visiting retained signatures, testing candidates and projecting hits.
    pub signature_visit_micros: u128,
    /// Time testing borrowed symbol fields against the compiled Text query.
    pub candidate_check_micros: u128,
    /// Time materializing admitted entity documents and checking their quotas.
    pub entity_materialization_micros: u128,
    /// Time materializing the source document and final deterministic ordering.
    pub source_projection_micros: u128,
    /// Number of scoped entity IDs admitted to the signature index.
    pub observed_entities: usize,
    /// Number of extension envelopes examined before completion or failure.
    pub observed_envelopes: usize,
    /// Number of selected signature envelope workspaces visited.
    pub visited_signature_workspaces: usize,
    /// Number of non-conflicting signatures returned during visitation.
    pub visited_signatures: usize,
    /// Number of entities whose fields reached the candidate check.
    pub candidate_entities: usize,
    /// Number of entity documents successfully materialized.
    pub retained_entities: usize,
    /// Number of documents materialized before completion or failure.
    pub projected_documents: usize,
    /// Whether the projection returned its complete document set successfully.
    pub complete: bool,
}

fn observe_text_projection<T>(
    observation: Option<&mut ScopedTextProjectionObservation>,
    duration: fn(&mut ScopedTextProjectionObservation) -> &mut u128,
    operation: impl FnOnce(Option<&mut ScopedTextProjectionObservation>) -> Result<T, QueryError>,
) -> Result<T, QueryError> {
    let Some(observation) = observation else {
        return operation(None);
    };
    let started = Instant::now();
    let result = operation(Some(observation));
    let counter = duration(observation);
    *counter = counter.saturating_add(started.elapsed().as_micros());
    result
}

/// Incremental bounded projection of one immutable generation into lexical documents.
///
/// Entity documents are projected during construction. Callers then supply
/// source snapshots in the canonical order exposed by
/// [`Self::next_source_file`], allowing each full source body to be released
/// after its policy-specific bounded lexical document is built.
#[derive(Debug)]
#[must_use = "finish the projection after supplying every required source"]
pub struct LexicalProjectionBuilder<'generation> {
    generation: &'generation GenerationSnapshot,
    budget: BuildBudget,
    projected: Vec<LexicalDocument>,
    text_bytes: usize,
    required_source_files: Vec<FileId>,
    next_source: usize,
    full_source_terms: bool,
}

impl<'generation> LexicalProjectionBuilder<'generation> {
    /// Starts a bounded projection and materializes its semantic entity documents.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid construction budgets, cancellation,
    /// missing direct non-external entity source identity, generation drift, exceeded
    /// bounds, or allocation failure.
    pub fn new(
        generation: &'generation GenerationSnapshot,
        budget: BuildBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, QueryError> {
        Self::with_source_policy(generation, budget, false, cancellation)
    }

    /// Starts an entity projection with text documents for every retained file.
    ///
    /// Structural support must not remove a file's lexical source evidence.
    /// The legacy constructor remains available for source-free callers and
    /// reconstruction of previously published lexical indexes.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid budgets, cancellation, generation
    /// drift, exceeded bounds, or allocation failure.
    pub fn with_all_sources(
        generation: &'generation GenerationSnapshot,
        budget: BuildBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, QueryError> {
        Self::with_source_policy(generation, budget, true, cancellation)
    }

    /// Starts an all-file projection whose bounded term batches scan full sources.
    ///
    /// Legacy constructors retain their exact prefix projection for restoration
    /// of older generations. Whole-file and symbol identities are unchanged.
    ///
    /// # Errors
    /// Returns [`QueryError`] for invalid budgets, cancellation, source identity
    /// drift, allocation failure, or exceeded construction bounds.
    pub fn with_full_source_terms(
        generation: &'generation GenerationSnapshot,
        budget: BuildBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, QueryError> {
        let mut projection = Self::with_source_policy(generation, budget, true, cancellation)?;
        projection.full_source_terms = true;
        Ok(projection)
    }

    fn with_source_policy(
        generation: &'generation GenerationSnapshot,
        budget: BuildBudget,
        all_sources: bool,
        cancellation: &Cancellation,
    ) -> Result<Self, QueryError> {
        validate_build_admission(budget)?;
        cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        let document = generation.document();
        if document.entities.len() > budget.max_documents {
            return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "documents",
            }));
        }
        let mut projected = Vec::new();
        projected
            .try_reserve(document.entities.len())
            .map_err(|_| QueryError::MemoryUnavailable)?;
        let mut text_bytes = 0usize;
        let mut signatures = RetainedSignatureIndex::new(
            document.entities.iter().map(|entity| entity.id),
            document.entities.len(),
        )?;
        for envelope in &document.extensions {
            cancellation
                .check()
                .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
            signatures.observe_with_checkpoint(envelope, || {
                cancellation
                    .check()
                    .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))
            })?;
        }
        signatures.visit_with_checkpoint(
            || {
                cancellation
                    .check()
                    .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))
            },
            |event| {
                let SignatureVisit::Evidence(symbol, signature) = event else {
                    return Ok(true);
                };
                let entity = document
                    .entities
                    .binary_search_by_key(&symbol, |entity| entity.id)
                    .ok()
                    .and_then(|index| document.entities.get(index))
                    .ok_or(QueryError::IndexDrift)?;
                // External models remain graph endpoints but have no repository
                // definition to publish as a lexical source hit.
                if entity.kind == EntityKind::ExternalSymbol
                    && entity.flags.contains(&EntityFlag::External)
                    && entity.flags.contains(&EntityFlag::Synthetic)
                    && entity.evidence.source.is_none()
                {
                    return Ok(true);
                }
                let (entity, next_text_bytes) = project_entity_document(
                    document,
                    entity,
                    signature.as_ref(),
                    text_bytes,
                    budget,
                    cancellation,
                )?;
                projected.push(entity);
                text_bytes = next_text_bytes;
                Ok(true)
            },
        )?;
        projected.sort_unstable_by_key(|document| document.symbol_id);
        let mut unsupported_files = document
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "unsupported-language")
            .filter_map(|diagnostic| {
                diagnostic
                    .source
                    .as_ref()
                    .map(|source| source.span().file())
            })
            .collect::<BTreeSet<_>>();
        unsupported_files.extend(
            generation
                .source_files()
                .entries()
                .iter()
                .map(|entry| entry.file().id),
        );
        if all_sources {
            unsupported_files.extend(document.files.iter().map(|file| file.id));
        }
        if document
            .entities
            .len()
            .checked_add(unsupported_files.len())
            .is_none_or(|documents| documents > budget.max_documents)
        {
            return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "documents",
            }));
        }
        let mut required_source_files = Vec::new();
        required_source_files
            .try_reserve_exact(unsupported_files.len())
            .map_err(|_| QueryError::MemoryUnavailable)?;
        for file_id in unsupported_files {
            cancellation
                .check()
                .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
            let file = generation
                .find_file(file_id)
                .ok_or(QueryError::IndexDrift)?;
            // Discovery already controls admission. Filtering accepted files by
            // name here would silently remove their text from global queries.
            required_source_files.push(file.id);
        }
        projected
            .try_reserve(required_source_files.len())
            .map_err(|_| QueryError::MemoryUnavailable)?;
        Ok(Self {
            generation,
            budget,
            projected,
            text_bytes,
            required_source_files,
            next_source: 0,
            full_source_terms: false,
        })
    }

    /// Returns the next canonical file whose source snapshot must be supplied.
    #[must_use]
    pub fn next_source_file(&self) -> Option<FileId> {
        self.required_source_files.get(self.next_source).copied()
    }

    /// Projects the next required source snapshot.
    ///
    /// The snapshot must match the file returned by
    /// [`Self::next_source_file`], including its path, content identity, and
    /// exact byte length.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, out-of-order or drifted source
    /// identity, exceeded text bounds, or allocation failure.
    pub fn push_source(
        &mut self,
        source: &SourceSnapshot,
        cancellation: &Cancellation,
    ) -> Result<(), QueryError> {
        cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        let expected = self.next_source_file().ok_or(QueryError::IndexDrift)?;
        if source.file() != expected {
            return Err(QueryError::IndexDrift);
        }
        let (mut projected, mut next_text_bytes) = project_source_document(
            self.generation,
            source,
            self.text_bytes,
            if self.full_source_terms {
                0
            } else {
                SOURCE_FALLBACK_TEXT_BYTES
            },
            None,
            self.budget,
            cancellation,
        )?;
        if self.full_source_terms {
            let terms = project_source_terms_with_coverage(
                source.content(),
                self.budget.max_text_bytes.saturating_sub(next_text_bytes),
                cancellation,
            )?;
            projected.source_term_chunks = terms.chunks;
            projected.source_coverage = Some(terms.coverage);
            for chunk in &projected.source_term_chunks {
                next_text_bytes = chunk
                    .iter()
                    .try_fold(next_text_bytes, |bytes, word| {
                        bytes
                            .checked_add(word.len())
                            .and_then(|bytes| bytes.checked_add(word.len()))
                            .ok_or(QueryError::IndexDrift)
                    })?
                    .checked_add(chunk.len().saturating_sub(1))
                    .ok_or(QueryError::IndexDrift)?;
            }
        }
        let next_source = self
            .next_source
            .checked_add(1)
            .ok_or(QueryError::IndexDrift)?;
        self.projected.push(projected);
        self.text_bytes = next_text_bytes;
        self.next_source = next_source;
        Ok(())
    }

    /// Projects only an authenticated file's name and path, without source text.
    ///
    /// No vocabulary or whole-source lexical completeness is inferred. This
    /// admits files retained outside parser snapshot memory while their exact
    /// content remains available through source reading and literal search.
    ///
    /// # Errors
    /// Returns cancellation, ordering, identity, allocation or construction-budget errors.
    pub fn push_source_metadata(
        &mut self,
        file: FileId,
        cancellation: &Cancellation,
    ) -> Result<(), QueryError> {
        if self.next_source_file() != Some(file) {
            return Err(QueryError::IndexDrift);
        }
        let projected =
            project_source_metadata_document(self.generation, file, self.budget, cancellation)?;
        let additional = [
            projected.identifier.len(),
            projected.qualified_name.len(),
            projected.path.len(),
            projected.kind.len(),
            projected.language.len(),
            projected.tier.len(),
        ]
        .into_iter()
        .try_fold(0usize, |sum, bytes| sum.checked_add(bytes))
        .ok_or(QueryError::IndexDrift)?;
        self.text_bytes = self
            .text_bytes
            .checked_add(additional)
            .filter(|bytes| *bytes <= self.budget.max_text_bytes)
            .ok_or(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "text_bytes",
            }))?;
        self.next_source = self
            .next_source
            .checked_add(1)
            .ok_or(QueryError::IndexDrift)?;
        self.projected.push(projected);
        Ok(())
    }

    /// Completes the projection after every required source was supplied.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::IndexDrift`] when a required source is missing,
    /// or [`QueryError::Cancelled`] when cancellation stops completion.
    pub fn finish(self, cancellation: &Cancellation) -> Result<Vec<LexicalDocument>, QueryError> {
        if self.next_source != self.required_source_files.len() {
            return Err(QueryError::IndexDrift);
        }
        cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        Ok(self.projected)
    }
}

/// Projects one exact source snapshot into a bounded file-only lexical document.
///
/// This streaming primitive admits one document at a time, so callers can
/// release the full source body immediately after handing the document to an
/// incremental index builder.
///
/// # Errors
///
/// Returns [`QueryError`] for source or generation drift, invalid build limits,
/// cancellation, bounded text overflow, or allocation failure.
pub fn project_source_fallback_document(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<LexicalDocument, QueryError> {
    project_source_fallback_document_with_text_limit(
        generation,
        source,
        SOURCE_FALLBACK_TEXT_BYTES,
        budget,
        cancellation,
    )
}

/// Projects exact file metadata without claiming any source vocabulary was indexed.
///
/// # Errors
/// Returns cancellation, absent file, allocation or construction-budget errors.
pub fn project_source_metadata_document(
    generation: &GenerationSnapshot,
    file: FileId,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<LexicalDocument, QueryError> {
    validate_build_admission(budget)?;
    cancellation
        .check()
        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
    let file = generation.find_file(file).ok_or(QueryError::IndexDrift)?;
    let identifier = file
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && name.len() <= 512)
        .unwrap_or("file");
    let tier = serialized_label(&analysis_tier_for_file(generation.document(), file)?)?;
    let bytes = [
        identifier.len(),
        identifier.len(),
        file.path.len(),
        4,
        file.language.len(),
        tier.len(),
    ]
    .into_iter()
    .try_fold(0usize, |sum, bytes| sum.checked_add(bytes))
    .ok_or(QueryError::IndexDrift)?;
    if bytes > budget.max_text_bytes {
        return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
            resource: "text_bytes",
        }));
    }
    Ok(LexicalDocument {
        symbol_id: None,
        file_id: file.id,
        identifier: try_clone(identifier)?,
        canonical_name: None,
        qualified_name: try_clone(identifier)?,
        symbol_name_projection: SymbolNameProjection::Inline,
        path: try_clone(&file.path)?,
        kind: "file".to_owned(),
        language: try_clone(&file.language)?,
        tier,
        package: None,
        build_target: None,
        signature: None,
        type_names: Vec::new(),
        documentation: None,
        source_identifiers: Vec::new(),
        source_text: None,
        source_term_chunks: Vec::new(),
        source_coverage: None,
        generated: file.generated,
        test: false,
        declaration_only: false,
    })
}

/// Projects one verified file's entire bounded vocabulary into one lexical document.
///
/// This is the streaming counterpart of the full-source generation builder.
/// All batches remain subject to the existing aggregate construction budget.
///
/// # Errors
/// Returns [`QueryError`] for source identity drift, invalid budgets,
/// cancellation, allocation failure, or exceeded projection limits.
pub fn project_source_document_with_full_terms(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<LexicalDocument, QueryError> {
    validate_build_admission(budget)?;
    let (mut document, bytes) =
        project_source_document(generation, source, 0, 0, None, budget, cancellation)?;
    let terms = project_source_terms_with_coverage(
        source.content(),
        budget.max_text_bytes.saturating_sub(bytes),
        cancellation,
    )?;
    document.source_term_chunks = terms.chunks;
    document.source_coverage = Some(terms.coverage);
    Ok(document)
}

/// Projects one exact source with an optionally tighter text-prefix ceiling.
///
/// Values above [`SOURCE_FALLBACK_TEXT_BYTES`] are clamped to the existing
/// hard fallback bound. A zero ceiling still produces exact file and path
/// fields without retaining source text.
///
/// # Errors
///
/// Returns the same errors as [`project_source_fallback_document`].
pub fn project_source_fallback_document_with_text_limit(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    maximum_source_text_bytes: usize,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<LexicalDocument, QueryError> {
    validate_build_admission(budget)?;
    project_source_document(
        generation,
        source,
        0,
        maximum_source_text_bytes.min(SOURCE_FALLBACK_TEXT_BYTES),
        None,
        budget,
        cancellation,
    )
    .map(|(document, _)| document)
}

fn project_source_document(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    current_text_bytes: usize,
    maximum_source_text_bytes: usize,
    selected_text: Option<&str>,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<(LexicalDocument, usize), QueryError> {
    cancellation
        .check()
        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
    let document = generation.document();
    let file = generation
        .find_file(source.file())
        .ok_or(QueryError::IndexDrift)?;
    if source.content_hash() != file.content_hash
        || source.path().as_str() != file.path
        || u64::try_from(source.content().len()).ok() != Some(file.byte_length)
    {
        return Err(QueryError::IndexDrift);
    }
    let (source_identifiers, source_text) = if let Some(text) = selected_text {
        if text.len() > maximum_source_text_bytes {
            return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "source_text",
            }));
        }
        let mut identifiers = Vec::new();
        for word in text.split_whitespace() {
            if identifiers.len() >= MAX_SOURCE_IDENTIFIERS {
                return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                    resource: "source_identifiers",
                }));
            }
            identifiers
                .try_reserve(1)
                .map_err(|_| QueryError::MemoryUnavailable)?;
            identifiers.push(try_clone(project_query_source_identifier(word)?.as_ref())?);
        }
        (
            identifiers,
            (!text.is_empty()).then(|| try_clone(text)).transpose()?,
        )
    } else {
        bounded_source_prefix(source.content(), maximum_source_text_bytes, cancellation)?
            .as_deref()
            .map(bounded_source_projection)
            .map(|(identifiers, text)| {
                let text = (!text.is_empty()).then_some(text);
                (identifiers, text)
            })
            .unwrap_or_default()
    };
    let identifier = file
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && name.len() <= 512)
        .unwrap_or("file");
    let tier = serialized_label(&analysis_tier_for_file(document, file)?)?;
    let next_text_bytes = [
        identifier.len(),
        identifier.len(),
        file.path.len(),
        "file".len(),
        file.language.len(),
        tier.len(),
        source_text.as_deref().map_or(0, str::len),
        source_identifiers.iter().map(String::len).sum(),
    ]
    .into_iter()
    .try_fold(current_text_bytes, |total, length| {
        total
            .checked_add(length)
            .filter(|value| *value <= budget.max_text_bytes)
            .ok_or(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "text_bytes",
            }))
    })?;
    Ok((
        LexicalDocument {
            symbol_id: None,
            file_id: file.id,
            identifier: try_clone(identifier)?,
            canonical_name: None,
            qualified_name: try_clone(identifier)?,
            symbol_name_projection: SymbolNameProjection::Inline,
            path: try_clone(&file.path)?,
            kind: "file".to_owned(),
            language: try_clone(&file.language)?,
            tier,
            package: None,
            build_target: None,
            signature: None,
            type_names: Vec::new(),
            documentation: None,
            source_identifiers,
            source_text,
            source_term_chunks: Vec::new(),
            source_coverage: None,
            generated: file.generated,
            test: false,
            declaration_only: false,
        },
        next_text_bytes,
    ))
}

/// Projects normalized entities into bounded source-free lexical documents.
///
/// The projection copies only indexed metadata. Repository source bodies,
/// comments, and documentation remain outside the core index.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid construction budgets, cancellation,
/// missing direct entity source identity, generation drift, or allocation
/// failure.
pub fn project_lexical_documents(
    generation: &GenerationSnapshot,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    LexicalProjectionBuilder::new(generation, budget, cancellation)?.finish(cancellation)
}

/// Projects normalized entities and unsupported retained text into bounded lexical documents.
///
/// File-only documents retain the real file identity and never synthesize a
/// semantic entity. Source text is admitted only for files explicitly marked
/// unsupported by the normalized generation.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid construction budgets, cancellation,
/// source or generation drift, exceeded bounds, or allocation failure.
pub fn project_lexical_documents_with_sources(
    generation: &GenerationSnapshot,
    sources: &[&SourceSnapshot],
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    project_with_source_snapshots(
        LexicalProjectionBuilder::new(generation, budget, cancellation)?,
        sources,
        cancellation,
    )
}

/// Projects entities and a bounded text prefix for every retained file.
///
/// File documents preserve source identity without inventing semantic entities;
/// structural support does not exclude source text from the global index.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid budgets, cancellation, missing or drifted
/// source identities, exceeded bounds, or allocation failure.
pub fn project_lexical_documents_with_all_sources(
    generation: &GenerationSnapshot,
    sources: &[&SourceSnapshot],
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    project_with_source_snapshots(
        LexicalProjectionBuilder::with_all_sources(generation, budget, cancellation)?,
        sources,
        cancellation,
    )
}

/// Projects every retained file's full bounded vocabulary alongside its symbols.
///
/// Batches share their file document, so cross-batch query terms do not create
/// duplicate hits or require an expensive query-time repository scan.
///
/// # Errors
/// Returns [`QueryError`] for invalid budgets, cancellation, source identity
/// drift, missing sources, allocation failure, or exceeded construction bounds.
pub fn project_lexical_documents_with_full_source_terms(
    generation: &GenerationSnapshot,
    sources: &[&SourceSnapshot],
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    project_with_source_snapshots(
        LexicalProjectionBuilder::with_full_source_terms(generation, budget, cancellation)?,
        sources,
        cancellation,
    )
}

fn project_with_source_snapshots(
    mut projection: LexicalProjectionBuilder<'_>,
    sources: &[&SourceSnapshot],
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    let mut ordered_sources = Vec::new();
    ordered_sources
        .try_reserve_exact(sources.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    ordered_sources.extend_from_slice(sources);
    ordered_sources.sort_unstable_by_key(|source| source.file());
    if ordered_sources
        .windows(2)
        .any(|pair| pair[0].file() == pair[1].file())
    {
        return Err(QueryError::IndexDrift);
    }
    while let Some(file_id) = projection.next_source_file() {
        let source = ordered_sources
            .binary_search_by_key(&file_id, |source| source.file())
            .ok()
            .and_then(|index| ordered_sources.get(index).copied())
            .ok_or(QueryError::IndexDrift)?;
        projection.push_source(source, cancellation)?;
    }
    projection.finish(cancellation)
}

/// Projects one exact file's entities together with its bounded source fallback.
///
/// Keeping both document kinds in one ephemeral index lets a path-scoped text
/// query recover omitted source while preserving any structural symbols that
/// were successfully indexed for the same file.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid construction budgets, cancellation,
/// source or generation drift, exceeded bounds, or allocation failure.
pub fn project_scoped_lexical_documents_with_source(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    project_scoped_source_documents(generation, source, None, None, budget, cancellation, None)
}

/// Projects exact-file text witnesses from anywhere in a verified source snapshot.
///
/// Only query-relevant source words are retained, under the unchanged source
/// projection ceiling. Every potentially matching structural entity and full-file
/// source identity are preserved. Irrelevant entities do not enter the ephemeral
/// index, whose relevance statistics are local to this query's projection.
/// This query-time projection never changes the persisted index.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid query/build budgets, source identity drift,
/// cancellation, elapsed search duration, allocation failure or exceeded bounds.
pub fn project_scoped_lexical_documents_for_query(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    query: &str,
    search_budget: SearchBudget,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    project_scoped_lexical_documents_for_query_with_observation(
        generation,
        source,
        query,
        search_budget,
        budget,
        cancellation,
        None,
    )
}

/// Runs the ordinary scoped Text projection with optional numeric observations.
///
/// `None` performs no observation clocks or counter scans. Observations supplied
/// by a caller are reset, then retain completed work even when projection fails.
/// Query limits, cancellation and returned documents are unchanged.
///
/// # Errors
/// Returns the same errors as [`project_scoped_lexical_documents_for_query`].
pub fn project_scoped_lexical_documents_for_query_with_observation(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    query: &str,
    search_budget: SearchBudget,
    budget: BuildBudget,
    cancellation: &Cancellation,
    mut observation: Option<&mut ScopedTextProjectionObservation>,
) -> Result<Vec<LexicalDocument>, QueryError> {
    if let Some(observation) = observation.as_deref_mut() {
        *observation = ScopedTextProjectionObservation::default();
    }
    let candidate = TextQueryCandidate::new(query, search_budget)?;
    let selected = observe_text_projection(
        observation.as_deref_mut(),
        |observation| &mut observation.source_selection_micros,
        |_| {
            Ok(select_query_source_bytes(
                source.content(),
                query,
                SOURCE_FALLBACK_TEXT_BYTES,
                search_budget,
                cancellation,
            )?)
        },
    )?;
    let result = project_scoped_source_documents(
        generation,
        source,
        selected.as_deref(),
        Some(&candidate),
        budget,
        cancellation,
        observation.as_deref_mut(),
    );
    if let Some(observation) = observation {
        observation.complete = result.is_ok();
    }
    result
}

fn project_scoped_source_documents(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    selected_text: Option<&str>,
    candidate: Option<&TextQueryCandidate>,
    budget: BuildBudget,
    cancellation: &Cancellation,
    mut observation: Option<&mut ScopedTextProjectionObservation>,
) -> Result<Vec<LexicalDocument>, QueryError> {
    validate_build_admission(budget)?;
    cancellation
        .check()
        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
    let document = generation.document();
    let file = generation
        .find_file(source.file())
        .ok_or(QueryError::IndexDrift)?;
    let mut scoped_candidate = candidate.map(|candidate| candidate.for_path(&file.path));
    let (mut projected, mut signatures) = observe_text_projection(
        observation.as_deref_mut(),
        |observation| &mut observation.signature_collection_micros,
        |observation| {
            let entity_count = document
                .entities
                .iter()
                .filter(|entity| {
                    candidate.is_none_or(TextQueryCandidate::matches_symbols)
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|evidence| evidence.span().file() == file.id)
                })
                .count();
            if let Some(observation) = observation {
                observation.observed_entities = entity_count;
            }
            let document_count = if candidate.is_some() { 0 } else { entity_count }
                .checked_add(1)
                .ok_or(QueryError::Search(SearchError::BuildBudgetExceeded {
                    resource: "documents",
                }))?;
            if document_count > budget.max_documents {
                return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                    resource: "documents",
                }));
            }
            let mut projected = Vec::new();
            projected
                .try_reserve_exact(document_count)
                .map_err(|_| QueryError::MemoryUnavailable)?;
            let signatures = RetainedSignatureIndex::new(
                document
                    .entities
                    .iter()
                    .filter(|entity| {
                        candidate.is_none_or(TextQueryCandidate::matches_symbols)
                            && entity
                                .evidence
                                .source
                                .as_ref()
                                .is_some_and(|evidence| evidence.span().file() == file.id)
                    })
                    .map(|entity| entity.id),
                entity_count,
            )?;
            Ok((projected, signatures))
        },
    )?;
    let mut text_bytes = 0;
    observe_text_projection(
        observation.as_deref_mut(),
        |observation| &mut observation.signature_observation_micros,
        |mut observation| {
            for envelope in &document.extensions {
                cancellation
                    .check()
                    .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
                if let Some(observation) = observation.as_deref_mut() {
                    observation.observed_envelopes += 1;
                }
                signatures.observe_with_checkpoint(envelope, || {
                    cancellation
                        .check()
                        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))
                })?;
            }
            Ok(())
        },
    )?;
    observe_text_projection(
        observation.as_deref_mut(),
        |observation| &mut observation.signature_visit_micros,
        |mut observation| {
            signatures.visit_with_checkpoint(
                || {
                    cancellation
                        .check()
                        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))
                },
                |event| {
                    let SignatureVisit::Evidence(symbol, signature) = event else {
                        if let Some(observation) = observation.as_deref_mut() {
                            observation.visited_signature_workspaces += 1;
                        }
                        return Ok(true);
                    };
                    if let Some(observation) = observation.as_deref_mut() {
                        observation.candidate_entities += 1;
                        observation.visited_signatures += usize::from(signature.is_some());
                    }
                    let entity = document
                        .entities
                        .binary_search_by_key(&symbol, |entity| entity.id)
                        .ok()
                        .and_then(|index| document.entities.get(index))
                        .ok_or(QueryError::IndexDrift)?;
                    let admitted = observe_text_projection(
                        observation.as_deref_mut(),
                        |observation| &mut observation.candidate_check_micros,
                        |_| {
                            Ok(scoped_candidate.as_mut().is_none_or(|candidate| {
                                candidate.may_match_symbol(
                                    &entity.display_name,
                                    (entity.canonical_name != entity.display_name)
                                        .then_some(entity.canonical_name.as_str()),
                                    &entity.qualified_name,
                                    signature
                                        .as_ref()
                                        .map(rootlight_ir::LexicalEvidenceV1::text),
                                )
                            }))
                        },
                    )?;
                    if !admitted {
                        return Ok(true);
                    }
                    observe_text_projection(
                        observation.as_deref_mut(),
                        |observation| &mut observation.entity_materialization_micros,
                        |observation| {
                            if projected
                                .len()
                                .checked_add(2)
                                .is_none_or(|count| count > budget.max_documents)
                            {
                                return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                                    resource: "documents",
                                }));
                            }
                            projected
                                .try_reserve(1)
                                .map_err(|_| QueryError::MemoryUnavailable)?;
                            let (entity, next_text_bytes) = project_entity_document(
                                document,
                                entity,
                                signature.as_ref(),
                                text_bytes,
                                budget,
                                cancellation,
                            )?;
                            projected.push(entity);
                            text_bytes = next_text_bytes;
                            if let Some(observation) = observation {
                                observation.retained_entities += 1;
                                observation.projected_documents = projected.len();
                            }
                            Ok(())
                        },
                    )?;
                    Ok(true)
                },
            )
        },
    )?;
    observe_text_projection(
        observation,
        |observation| &mut observation.source_projection_micros,
        |observation| {
            projected.sort_unstable_by_key(|document| document.symbol_id);
            let (source, _) = project_source_document(
                generation,
                source,
                text_bytes,
                SOURCE_FALLBACK_TEXT_BYTES,
                selected_text,
                budget,
                cancellation,
            )?;
            projected.push(source);
            if let Some(observation) = observation {
                observation.projected_documents = projected.len();
            }
            Ok(())
        },
    )?;
    Ok(projected)
}

fn project_entity_document(
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    signature: Option<&rootlight_ir::LexicalEvidenceV1>,
    current_text_bytes: usize,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<(LexicalDocument, usize), QueryError> {
    let source = entity
        .evidence
        .source
        .as_ref()
        .ok_or(QueryError::IndexDrift)?;
    let file = document
        .files
        .binary_search_by_key(&source.span().file(), |record| record.id)
        .ok()
        .and_then(|index| document.files.get(index))
        .ok_or(QueryError::IndexDrift)?;
    if file.repository != entity.repository
        || file.generation != entity.generation
        || file.content_hash != source.content_hash()
    {
        return Err(QueryError::IndexDrift);
    }
    let kind = serialized_label(&entity.kind)?;
    let tier = serialized_label(&entity.tier)?;
    let next_text_bytes = [
        entity.display_name.len(),
        if entity.canonical_name != entity.display_name {
            entity.canonical_name.len()
        } else {
            0
        },
        entity.qualified_name.len(),
        file.path.len(),
        kind.len(),
        entity.language.len(),
        tier.len(),
        signature.map_or(0, |evidence| evidence.text().len()),
    ]
    .into_iter()
    .try_fold(current_text_bytes, |total, length| {
        total
            .checked_add(length)
            .filter(|value| *value <= budget.max_text_bytes)
            .ok_or(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "text_bytes",
            }))
    })?;
    Ok((
        LexicalDocument {
            symbol_id: Some(entity.id),
            file_id: file.id,
            identifier: try_clone(&entity.display_name)?,
            canonical_name: (entity.canonical_name != entity.display_name)
                .then(|| try_clone(&entity.canonical_name))
                .transpose()?,
            qualified_name: try_clone(&entity.qualified_name)?,
            symbol_name_projection: project_symbol_name_projection(
                &entity.display_name,
                (entity.canonical_name != entity.display_name)
                    .then_some(entity.canonical_name.as_str()),
                &entity.qualified_name,
            ),
            path: try_clone(&file.path)?,
            kind,
            language: try_clone(&entity.language)?,
            tier,
            package: None,
            build_target: None,
            signature: signature
                .map(|evidence| {
                    rootlight_search::project_signature_search_text(evidence.text(), cancellation)
                })
                .transpose()?,
            type_names: Vec::new(),
            documentation: None,
            source_identifiers: Vec::new(),
            source_text: None,
            source_term_chunks: Vec::new(),
            source_coverage: None,
            generated: file.generated,
            test: matches!(entity.kind, EntityKind::Test)
                || entity.flags.contains(&EntityFlag::Test),
            declaration_only: entity_is_declaration_only(document, entity, &file.path),
        },
        next_text_bytes,
    ))
}

/// One bounded extension pass selects references, not a generation-wide header
/// expansion. A second grouped pass decodes each chosen batch only once.
pub(super) struct RetainedSignatureIndex<'document> {
    candidates: Vec<(SymbolId, Option<SignatureReference<'document>>, bool)>,
}

#[derive(Clone, Copy)]
struct SignatureReference<'document> {
    envelope: &'document rootlight_ir::ExtensionEnvelope,
    child: Option<usize>,
    payload_hash: rootlight_ids::ContentHash,
}

pub(super) enum SignatureVisit<'document> {
    Workspace(&'document rootlight_ir::ExtensionEnvelope),
    Evidence(SymbolId, Option<rootlight_ir::LexicalEvidenceV1>),
}

impl<'document> RetainedSignatureIndex<'document> {
    pub(super) fn new(
        symbols: impl Iterator<Item = SymbolId>,
        maximum_symbols: usize,
    ) -> Result<Self, QueryError> {
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(maximum_symbols)
            .map_err(|_| QueryError::MemoryUnavailable)?;
        for symbol in symbols {
            if candidates.len() == maximum_symbols {
                return Err(QueryError::MemoryUnavailable);
            }
            candidates.push((symbol, None, false));
        }
        candidates.sort_unstable_by_key(|candidate| candidate.0);
        candidates.dedup_by_key(|candidate| candidate.0);
        Ok(Self { candidates })
    }

    pub(super) fn candidate(&self, envelope: &rootlight_ir::ExtensionEnvelope) -> bool {
        if envelope.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE
            && envelope.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION
        {
            return envelope.evidence.derivation.iter().any(|subject| {
                matches!(subject, rootlight_ir::FactRef::Entity(symbol)
                    if self.candidates.binary_search_by_key(symbol, |candidate| candidate.0).is_ok())
            });
        }
        Self::subject(envelope).is_some_and(|symbol| {
            self.candidates
                .binary_search_by_key(&symbol, |candidate| candidate.0)
                .is_ok()
        })
    }

    fn subject(envelope: &rootlight_ir::ExtensionEnvelope) -> Option<SymbolId> {
        if envelope.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE
            || envelope.version == rootlight_ir::LEXICAL_SUMMARY_BATCH_VERSION
            || envelope.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION
        {
            return None;
        }
        match envelope.evidence.derivation.as_slice() {
            [rootlight_ir::FactRef::Entity(symbol)] => Some(*symbol),
            _ => None,
        }
    }

    pub(super) fn observe_with_checkpoint(
        &mut self,
        envelope: &'document rootlight_ir::ExtensionEnvelope,
        mut checkpoint: impl FnMut() -> Result<(), QueryError>,
    ) -> Result<(), QueryError> {
        checkpoint()?;
        if !self.candidate(envelope) {
            return Ok(());
        }
        if envelope.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION {
            let children = rootlight_ir::decode_lexical_signature_batch_envelope_with_checkpoint(
                envelope,
                || checkpoint().is_ok(),
            );
            checkpoint()?;
            let children = children.map_err(|_| QueryError::IndexDrift)?;
            for (position, child) in children.iter().enumerate() {
                checkpoint()?;
                let evidence = rootlight_ir::decode_lexical_evidence_envelope(child)
                    .map_err(|_| QueryError::IndexDrift)?;
                let rootlight_ir::FactRef::Entity(symbol) = evidence.subject() else {
                    return Err(QueryError::IndexDrift);
                };
                self.observe_reference(
                    symbol,
                    SignatureReference {
                        envelope,
                        child: Some(position),
                        payload_hash: rootlight_ids::content_hash(child.payload.as_bytes()),
                    },
                );
            }
            return Ok(());
        }
        let Some(symbol) = Self::subject(envelope) else {
            return Ok(());
        };
        let evidence = rootlight_ir::decode_lexical_evidence_envelope(envelope)
            .map_err(|_| QueryError::IndexDrift)?;
        if evidence.kind() != rootlight_ir::LexicalEvidenceKind::Signature {
            return Ok(());
        }
        self.observe_reference(
            symbol,
            SignatureReference {
                envelope,
                child: None,
                payload_hash: rootlight_ids::content_hash(envelope.payload.as_bytes()),
            },
        );
        Ok(())
    }

    fn observe_reference(&mut self, symbol: SymbolId, reference: SignatureReference<'document>) {
        let Ok(index) = self
            .candidates
            .binary_search_by_key(&symbol, |candidate| candidate.0)
        else {
            return;
        };
        let (_, previous, conflicting) = &mut self.candidates[index];
        if previous.is_some_and(|previous| previous.payload_hash != reference.payload_hash) {
            // Canonical child payloads include the exact subject and source
            // mapping. Batch wrapping must not change direct conflict semantics.
            *conflicting = true;
        }
        *previous = Some(reference);
    }

    pub(super) fn visit_with_checkpoint(
        mut self,
        mut checkpoint: impl FnMut() -> Result<(), QueryError>,
        mut visit: impl FnMut(SignatureVisit<'document>) -> Result<bool, QueryError>,
    ) -> Result<bool, QueryError> {
        checkpoint()?;
        // Most entities in a dense source have no signature. Emit this already
        // SymbolId-ordered prefix without sorting their large reference slots.
        // Conflicting batch children retain their distinct secondary sort key.
        let mut retained = 0;
        for offset in 0..self.candidates.len() {
            checkpoint()?;
            let candidate @ (symbol, reference, conflicting) = self.candidates[offset];
            if reference.is_none_or(|reference| conflicting && reference.child.is_none()) {
                if !visit(SignatureVisit::Evidence(symbol, None))? {
                    return Ok(false);
                }
            } else {
                self.candidates[retained] = candidate;
                retained += 1;
            }
        }
        self.candidates.truncate(retained);
        self.candidates
            .sort_unstable_by_key(|(symbol, reference, conflicting)| {
                (
                    reference
                        .filter(|_| !conflicting)
                        .map(|reference| reference.envelope.id),
                    reference.and_then(|reference| reference.child),
                    *symbol,
                )
            });
        checkpoint()?;
        let mut offset = 0;
        while offset < self.candidates.len() {
            checkpoint()?;
            let (symbol, reference, conflicting) = self.candidates[offset];
            let Some(reference) = reference.filter(|_| !conflicting) else {
                if !visit(SignatureVisit::Evidence(symbol, None))? {
                    return Ok(false);
                }
                offset += 1;
                continue;
            };
            let envelope = reference.envelope;
            if !visit(SignatureVisit::Workspace(envelope))? {
                return Ok(false);
            }
            let end = offset
                + self.candidates[offset..].partition_point(|(_, candidate, conflict)| {
                    !conflict
                        && candidate.is_some_and(|candidate| candidate.envelope.id == envelope.id)
                });
            let children = if envelope.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION {
                let children =
                    rootlight_ir::decode_lexical_signature_batch_envelope_with_checkpoint(
                        envelope,
                        || checkpoint().is_ok(),
                    );
                checkpoint()?;
                Some(children.map_err(|_| QueryError::IndexDrift)?)
            } else {
                None
            };
            for &(symbol, reference, _) in &self.candidates[offset..end] {
                checkpoint()?;
                let reference = reference.ok_or(QueryError::IndexDrift)?;
                let child = match (reference.child, &children) {
                    (Some(position), Some(children)) => {
                        children.get(position).ok_or(QueryError::IndexDrift)?
                    }
                    (None, None) => envelope,
                    _ => return Err(QueryError::IndexDrift),
                };
                let evidence = rootlight_ir::decode_lexical_evidence_envelope(child)
                    .map_err(|_| QueryError::IndexDrift)?;
                if evidence.subject() != rootlight_ir::FactRef::Entity(symbol) {
                    return Err(QueryError::IndexDrift);
                }
                if !visit(SignatureVisit::Evidence(symbol, Some(evidence)))? {
                    return Ok(false);
                }
            }
            offset = end;
        }
        checkpoint()?;
        Ok(true)
    }

    pub(super) const fn entry_bytes() -> usize {
        std::mem::size_of::<(SymbolId, Option<SignatureReference<'_>>, bool)>()
    }
}

fn analysis_tier_for_file(
    document: &NormalizedIrDocument,
    file: &rootlight_ir::FileRecord,
) -> Result<rootlight_ir::AnalysisTier, QueryError> {
    document
        .provenance
        .binary_search_by_key(&file.provenance, |candidate| candidate.id)
        .ok()
        .and_then(|index| document.provenance.get(index))
        .map(|provenance| provenance.tier)
        .ok_or(QueryError::IndexDrift)
}

fn bounded_source_prefix(
    content: &[u8],
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Option<String>, QueryError> {
    if maximum == 0 {
        return Ok(None);
    }
    let Some(text) = SourceText::parse(content, cancellation)
        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?
    else {
        return Ok(None);
    };
    let mut prefix = String::new();
    prefix
        .try_reserve_exact(maximum)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    for (index, character) in text.chars().enumerate() {
        if index % 1024 == 0 {
            cancellation
                .check()
                .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        }
        if prefix.len().saturating_add(character.len_utf8()) > maximum {
            break;
        }
        prefix.push(character);
    }
    Ok(Some(prefix))
}

fn bounded_source_projection(source: &str) -> (Vec<String>, String) {
    let mut identifiers = HashSet::new();
    let mut output = String::with_capacity(source.len());
    for candidate in
        source.split(|character: char| !(character == '_' || character.is_alphanumeric()))
    {
        if candidate.is_empty() || candidate.len() > MAX_SOURCE_IDENTIFIER_BYTES {
            continue;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(candidate);
        if identifiers.len() < MAX_SOURCE_IDENTIFIERS
            && candidate
                .chars()
                .next()
                .is_none_or(|character| !(character == '_' || character.is_alphabetic()))
        {
            continue;
        }
        if identifiers.len() < MAX_SOURCE_IDENTIFIERS {
            identifiers.insert(candidate);
        }
    }
    let mut identifiers = identifiers
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    identifiers.sort_unstable();
    (identifiers, output)
}

fn entity_is_declaration_only(
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    path: &str,
) -> bool {
    if entity.language == "ocaml-interface" || path_is_declaration_only(path) {
        return true;
    }
    if matches!(
        entity.kind,
        EntityKind::Interface | EntityKind::Trait | EntityKind::Protocol | EntityKind::TypeAlias
    ) {
        return true;
    }
    let Some(ContainerRef::Entity(container)) = entity.container else {
        return false;
    };
    document
        .entities
        .binary_search_by_key(&container, |candidate| candidate.id)
        .ok()
        .and_then(|index| document.entities.get(index))
        .is_some_and(|container| {
            matches!(
                container.kind,
                EntityKind::Interface | EntityKind::Trait | EntityKind::Protocol
            )
        })
}

fn path_is_declaration_only(path: &str) -> bool {
    path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts")
}

fn serialized_label(value: &impl Serialize) -> Result<String, QueryError> {
    let encoded = serde_json::to_string(value).map_err(|_| QueryError::ResultEncoding)?;
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or(QueryError::IndexDrift)
}

fn try_clone(value: &str) -> Result<String, QueryError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    cloned.push_str(value);
    Ok(cloned)
}

#[cfg(test)]
mod tests {
    use super::{bounded_source_prefix, bounded_source_projection};

    #[test]
    fn fallback_prefix_respects_tighter_utf8_boundaries() {
        let source = "abé".as_bytes();

        let cancellation = rootlight_cancel::Cancellation::new();
        assert_eq!(
            bounded_source_prefix(source, 0, &cancellation).unwrap(),
            None
        );
        assert_eq!(
            bounded_source_prefix(source, 3, &cancellation)
                .unwrap()
                .as_deref(),
            Some("ab")
        );
        assert_eq!(
            bounded_source_prefix(source, 4, &cancellation)
                .unwrap()
                .as_deref(),
            Some("abé")
        );
        let bytes = std::iter::once(0xfeff_u16)
            .chain("abé𝄞".encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            bounded_source_prefix(&bytes, 7, &cancellation)
                .unwrap()
                .as_deref(),
            Some("abé")
        );
        assert_eq!(
            bounded_source_prefix(&bytes, 8, &cancellation)
                .unwrap()
                .as_deref(),
            Some("abé𝄞")
        );
    }

    #[test]
    fn typescript_declaration_file_suffixes_are_unambiguous() {
        for path in ["index.d.ts", "index.d.mts", "index.d.cts"] {
            assert!(super::path_is_declaration_only(path));
        }
        assert!(
            !["index.ts", "index.mts", "index.cts"]
                .iter()
                .any(|path| super::path_is_declaration_only(path))
        );
    }

    #[test]
    fn ocaml_interface_entities_are_declaration_only_in_any_host() {
        use rootlight_ids::{FactId, GenerationId, RepositoryId, SymbolId};
        use rootlight_ir::{
            AnalysisTier, EntityKind, EntityRecord, EntityVisibility, FactEvidence,
            NormalizedIrDocument,
        };

        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let document = NormalizedIrDocument::empty(repository, generation);
        let mut entity = EntityRecord {
            id: SymbolId::from_bytes([3; 20]),
            repository,
            generation,
            kind: EntityKind::Function,
            language: "ocaml-interface".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: "written".to_owned(),
            display_name: "written".to_owned(),
            qualified_name: "written".to_owned(),
            container: None,
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        };
        for kind in [
            EntityKind::Function,
            EntityKind::Module,
            EntityKind::Struct,
            EntityKind::Enum,
        ] {
            entity.kind = kind;
            for path in [
                "written.mli",
                "WRITTEN.MLI",
                "examples.md",
                "configured.txt",
            ] {
                assert!(super::entity_is_declaration_only(&document, &entity, path));
            }
            entity.language = "ocaml".to_owned();
            assert!(!super::entity_is_declaration_only(
                &document,
                &entity,
                "written.ml"
            ));
            entity.language = "ocaml-interface".to_owned();
        }
    }

    #[test]
    fn fallback_text_drops_oversized_terms_without_losing_neighboring_identifiers() {
        let source = format!("before {} after", "x".repeat(241));
        let (identifiers, text) = bounded_source_projection(&source);

        assert_eq!(identifiers, ["after".to_owned(), "before".to_owned()]);
        assert_eq!(text, "before after");
    }
}
