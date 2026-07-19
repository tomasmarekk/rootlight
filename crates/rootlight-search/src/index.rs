use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{FileId, GenerationId, SymbolId};
use tantivy::{
    DocAddress, DocSet, Index, IndexReader, ReloadPolicy, TERMINATED, TantivyDocument, Term,
    indexer::NoMergePolicy,
    query::{BooleanQuery, BoostQuery, EmptyQuery, EnableScoring, Occur, Query, TermQuery},
    schema::{
        BytesOptions, Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing,
        TextOptions, Value,
    },
    tokenizer::TextAnalyzer,
};
use tantivy_fst::{Automaton as _, Regex};

use crate::{
    artifact::{ArtifactBudget, LexicalArtifactManifest, VerifiedLexicalArtifact, create_manifest},
    model::{
        BuildBudget, BuildStats, CODE_TOKENIZER, DocumentField, LexicalDocument, QueryViolation,
        SearchBudget, SearchError, SearchHit, SearchMode, SearchOutcome, SearchRequest,
    },
    tokenizer::{CodeTokenizer, has_oversized_term, normalize_text, token_texts},
};

const FORMAT_PREFIX: &str = "rootlight.lexical";
const FORMAT_VERSION: &str = "2";
const STORED_HIT_VERSION: u8 = 1;
const MIN_WRITER_HEAP_BYTES: usize = 15_000_000;
const MAX_IDENTIFIER_BYTES: usize = 512;
const MAX_QUALIFIED_BYTES: usize = 2_048;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_LABEL_BYTES: usize = 128;
const MAX_OWNERSHIP_BYTES: usize = 512;
const MAX_SIGNATURE_BYTES: usize = 4_096;
const MAX_TYPE_NAMES: usize = 64;
const MAX_TYPE_NAME_BYTES: usize = 512;
const MAX_DOCUMENTATION_BYTES: usize = 32 * 1024;
const MAX_PATTERN_WILDCARDS: usize = 4;
const MIN_PATTERN_LITERALS: usize = 2;
const HARD_MAX_DOCUMENTS: usize = 1_000_000;
const HARD_MAX_TEXT_BYTES: usize = 2 * 1024 * 1024 * 1024;
const HARD_MAX_WRITER_HEAP_BYTES: usize = 256 * 1024 * 1024;
const HARD_MAX_QUERY_BYTES: usize = 4_096;
const HARD_MAX_QUERY_CANDIDATES: usize = 100_000;
const HARD_MAX_QUERY_RESULTS: usize = 1_000;
const HARD_MAX_QUERY_TERMS: usize = 64;
const HARD_MAX_EXPANDED_TERMS: usize = 4_096;
const HARD_MAX_EXAMINED_TERMS: usize = 100_000;
const HARD_MAX_QUERY_POSTINGS: u64 = 1_000_000;
const HARD_MAX_RETURNED_TEXT_BYTES: usize = 64 * 1024 * 1024;
const HARD_MAX_QUERY_DURATION: Duration = Duration::from_secs(10);
const SORT_CHECKPOINT_STRIDE: usize = 1_024;

type QueryClause = (Occur, Box<dyn Query>);

/// Builds a lexical index in an already-private, empty staging directory.
pub struct LexicalIndexBuilder;

impl LexicalIndexBuilder {
    /// Validates, deterministically orders, and closes one generation's artifact.
    ///
    /// Publication and cleanup are intentionally outside this crate. The caller
    /// must supply an empty directory rooted in its private generation staging tree.
    /// The returned manifest is created only after the writer and index handles close.
    /// Production path-backed construction remains unavailable until the
    /// private file-handle boundary is accepted; unit tests exercise the
    /// proposed artifact contract without enabling it for callers.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] for invalid documents, exceeded budgets,
    /// cancellation, non-empty staging, artifact preflight, or redacted Tantivy failures.
    pub fn build(
        directory: &Path,
        generation: GenerationId,
        mut documents: Vec<LexicalDocument>,
        budget: BuildBudget,
        artifact_budget: ArtifactBudget,
        cancellation: &Cancellation,
    ) -> Result<LexicalArtifactManifest, SearchError> {
        crate::require_private_file_boundary(cfg!(test))?;
        cancellation.check()?;
        validate_build_budget(budget)?;
        ensure_empty_directory(directory)?;
        let (document_count, text_bytes) = prepare_documents(&mut documents, budget, cancellation)?;

        let fields = Fields::new();
        let index = Index::builder()
            .schema(fields.schema.clone())
            .create_in_dir(directory)
            .map_err(|_| operation("create"))?;
        register_tokenizer(&index);
        populate_index(
            &index,
            &fields,
            generation,
            &documents,
            document_count,
            budget,
            cancellation,
        )?;
        drop(index);
        cancellation.check()?;
        let stats = BuildStats {
            generation,
            documents: document_count,
            text_bytes,
        };
        create_manifest(directory, stats, artifact_budget, cancellation)
    }
}

/// Backend-neutral domain contract for generation-pinned lexical reads.
pub trait LexicalSearch: Send + Sync {
    /// Returns the immutable generation served by this reader.
    fn generation(&self) -> GenerationId;

    /// Executes one bounded domain query.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] when input, budgets, cancellation, durable data,
    /// or the backend prevents a truthful result.
    fn search_with_stats(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError>;

    /// Executes one bounded domain query and returns only its ordered hits.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] under the same conditions as
    /// [`LexicalSearch::search_with_stats`].
    fn search(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<Vec<SearchHit>, SearchError> {
        self.search_with_stats(request, budget, cancellation)
            .map(|outcome| outcome.hits)
    }

    /// Returns the currently visible document count.
    fn document_count(&self) -> u64;
}

/// Validates a lexical construction budget without creating an artifact.
///
/// # Errors
///
/// Returns [`SearchError::BuildBudgetExceeded`] when a field is zero, below
/// the backend minimum, or above a construction hard ceiling.
pub fn validate_build_admission(budget: BuildBudget) -> Result<(), SearchError> {
    validate_build_budget(budget)
}

/// Validates one query and its complete lexical budget without executing it.
///
/// # Errors
///
/// Returns [`SearchError`] for invalid syntax, result limits, or resource
/// fields outside the backend hard ceilings.
pub fn validate_search_request(
    request: &SearchRequest,
    budget: SearchBudget,
) -> Result<(), SearchError> {
    validate_request(request, budget)
}

/// A read-only lexical index pinned to one immutable generation.
pub struct LexicalIndex {
    generation: GenerationId,
    fields: Fields,
    reader: IndexReader,
    _artifact: Option<VerifiedLexicalArtifact>,
}

impl LexicalIndex {
    /// Consumes a verified artifact only when schema, metadata, count, and generation align.
    ///
    /// Production path-backed opening remains unavailable until the private
    /// file-handle boundary is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] when the index cannot be opened, contains deleted
    /// documents, or its durable identity does not match `expected_generation`.
    pub fn open(
        artifact: VerifiedLexicalArtifact,
        expected_generation: GenerationId,
    ) -> Result<Self, SearchError> {
        crate::require_private_file_boundary(cfg!(test))?;
        if artifact.generation() != expected_generation {
            return Err(SearchError::GenerationMismatch {
                expected: expected_generation,
                actual: artifact.generation(),
            });
        }
        let expected_stats = artifact.stats();
        let fields = Fields::new();
        let index = Index::open_in_dir(artifact.directory()).map_err(|_| operation("open"))?;
        register_tokenizer(&index);
        let (actual_generation, reader) = open_reader(
            &index,
            &fields,
            expected_generation,
            Some(expected_stats.documents),
        )?;
        Ok(Self {
            generation: actual_generation,
            fields,
            reader,
            _artifact: Some(artifact),
        })
    }

    /// Builds a nondurable in-memory index for a bounded first-slice session.
    ///
    /// The returned reader enforces the same document validation, generation
    /// payload, query budgets, and deterministic ranking as a durable index,
    /// but it cannot be reopened or shared across processes.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] for invalid documents, exceeded budgets,
    /// cancellation, incompatible committed metadata, or redacted Tantivy
    /// failures.
    pub fn build_ephemeral(
        generation: GenerationId,
        mut documents: Vec<LexicalDocument>,
        budget: BuildBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, SearchError> {
        cancellation.check()?;
        validate_build_budget(budget)?;
        let (document_count, _) = prepare_documents(&mut documents, budget, cancellation)?;
        let fields = Fields::new();
        let index = Index::create_in_ram(fields.schema.clone());
        register_tokenizer(&index);
        populate_index(
            &index,
            &fields,
            generation,
            &documents,
            document_count,
            budget,
            cancellation,
        )?;
        let (actual_generation, reader) =
            open_reader_checked(&index, &fields, generation, Some(document_count), || {
                cancellation.check().map_err(SearchError::from)
            })?;
        Ok(Self {
            generation: actual_generation,
            fields,
            reader,
            _artifact: None,
        })
    }

    /// Returns the immutable generation encoded in the committed index.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Executes a bounded query and applies stable `SymbolId` tie-breaking.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] for invalid input, cancellation, candidate
    /// overflow, incompatible stored data, or redacted Tantivy failures.
    pub fn search(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<Vec<SearchHit>, SearchError> {
        self.search_with_stats(request, budget, cancellation)
            .map(|outcome| outcome.hits)
    }

    /// Executes a bounded query with exact candidate and text-byte counters.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] for invalid input, cancellation, candidate
    /// overflow, incompatible stored data, or redacted Tantivy failures.
    pub fn search_with_stats(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        let control = SearchControl::new(cancellation, budget.max_duration);
        control.check()?;
        validate_request(request, budget)?;
        let searcher = self.reader.searcher();
        let query = self.fields.query(request, &searcher, budget, &control)?;
        control.check()?;
        let weight = query
            .weight(EnableScoring::enabled_from_searcher(&searcher))
            .map_err(|_| operation("prepare_query"))?;
        control.check()?;
        let normalized_query = normalize_exact(request.query.trim());
        let mut hits = Vec::new();
        let mut materialized_text_bytes = 0usize;
        for (segment_index, segment_reader) in searcher.segment_readers().iter().enumerate() {
            control.check()?;
            let segment_ord =
                u32::try_from(segment_index).map_err(|_| SearchError::IncompatibleIndex)?;
            let mut scorer = weight
                .scorer(segment_reader, 1.0)
                .map_err(|_| operation("prepare_segment"))?;
            control.check()?;
            let mut document_id = scorer.doc();
            while document_id != TERMINATED {
                control.check()?;
                if hits.len() >= budget.max_candidates {
                    return Err(SearchError::CandidateBudgetExceeded);
                }
                let score = scorer.score();
                let document = searcher
                    .doc::<TantivyDocument>(DocAddress::new(segment_ord, document_id))
                    .map_err(|_| operation("stored_document"))?;
                let (hit, hit_text_bytes) = self.fields.decode(document, score)?;
                materialized_text_bytes = materialized_text_bytes
                    .checked_add(hit_text_bytes)
                    .ok_or(SearchError::ReturnedTextBudgetExceeded)?;
                if materialized_text_bytes > budget.max_returned_text_bytes {
                    return Err(SearchError::ReturnedTextBudgetExceeded);
                }
                hits.push(RankedHit {
                    rank: lexical_rank(request.mode, &normalized_query, &hit),
                    hit,
                });
                document_id = scorer.advance();
            }
        }
        control.check()?;
        sort_with_checkpoints(
            &mut hits,
            |left, right| {
                left.rank
                    .cmp(&right.rank)
                    .then_with(|| {
                        right
                            .hit
                            .relevance_score
                            .total_cmp(&left.hit.relevance_score)
                    })
                    .then_with(|| left.hit.symbol_id.cmp(&right.hit.symbol_id))
            },
            || control.check(),
        )?;
        control.check()?;
        let matched_candidates =
            u64::try_from(hits.len()).map_err(|_| SearchError::CandidateBudgetExceeded)?;
        let materialized_text_bytes = u64::try_from(materialized_text_bytes)
            .map_err(|_| SearchError::ReturnedTextBudgetExceeded)?;
        hits.truncate(request.max_results);
        Ok(SearchOutcome {
            hits: hits.into_iter().map(|ranked| ranked.hit).collect(),
            matched_candidates,
            materialized_text_bytes,
        })
    }

    /// Returns the currently visible document count.
    #[must_use]
    pub fn document_count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

impl LexicalSearch for LexicalIndex {
    fn generation(&self) -> GenerationId {
        self.generation()
    }

    fn search_with_stats(
        &self,
        request: &SearchRequest,
        budget: SearchBudget,
        cancellation: &Cancellation,
    ) -> Result<SearchOutcome, SearchError> {
        self.search_with_stats(request, budget, cancellation)
    }

    fn document_count(&self) -> u64 {
        self.document_count()
    }
}

fn sort_with_checkpoints<T>(
    values: &mut [T],
    compare: impl Fn(&T, &T) -> Ordering,
    mut check: impl FnMut() -> Result<(), SearchError>,
) -> Result<(), SearchError> {
    check()?;
    if values.len() < 2 {
        return Ok(());
    }

    for root in (0..values.len() / 2).rev() {
        if root % SORT_CHECKPOINT_STRIDE == 0 {
            check()?;
        }
        sift_down(values, root, values.len(), &compare);
    }
    for end in (1..values.len()).rev() {
        if end % SORT_CHECKPOINT_STRIDE == 0 {
            check()?;
        }
        values.swap(0, end);
        sift_down(values, 0, end, &compare);
    }
    check()
}

fn sift_down<T>(
    values: &mut [T],
    mut root: usize,
    end: usize,
    compare: &impl Fn(&T, &T) -> Ordering,
) {
    loop {
        let Some(left) = root.checked_mul(2).and_then(|value| value.checked_add(1)) else {
            return;
        };
        if left >= end {
            return;
        }
        let mut greater = left;
        let right = left + 1;
        if right < end && compare(&values[left], &values[right]) == Ordering::Less {
            greater = right;
        }
        if compare(&values[root], &values[greater]) != Ordering::Less {
            return;
        }
        values.swap(root, greater);
        root = greater;
    }
}

fn prepare_documents(
    documents: &mut [LexicalDocument],
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<(u64, u64), SearchError> {
    if documents.len() > budget.max_documents {
        return Err(SearchError::BuildBudgetExceeded {
            resource: "documents",
        });
    }
    sort_with_checkpoints(
        documents,
        |left, right| left.symbol_id.cmp(&right.symbol_id),
        || cancellation.check().map_err(SearchError::from),
    )?;
    let mut prior = None;
    let mut text_bytes = 0usize;
    for document in documents.iter() {
        cancellation.check()?;
        if prior == Some(document.symbol_id) {
            return Err(SearchError::DuplicateSymbol);
        }
        prior = Some(document.symbol_id);
        text_bytes = text_bytes.checked_add(validate_document(document)?).ok_or(
            SearchError::BuildBudgetExceeded {
                resource: "text_bytes",
            },
        )?;
        if text_bytes > budget.max_text_bytes {
            return Err(SearchError::BuildBudgetExceeded {
                resource: "text_bytes",
            });
        }
    }
    Ok((
        u64::try_from(documents.len()).map_err(|_| SearchError::BuildBudgetExceeded {
            resource: "documents",
        })?,
        u64::try_from(text_bytes).map_err(|_| SearchError::BuildBudgetExceeded {
            resource: "text_bytes",
        })?,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildCheckpoint {
    BeforeWriter,
    BeforeDocument,
    BeforeCommit,
    AfterCommit,
}

fn populate_index(
    index: &Index,
    fields: &Fields,
    generation: GenerationId,
    documents: &[LexicalDocument],
    document_count: u64,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<(), SearchError> {
    populate_index_with_checkpoints(
        index,
        fields,
        generation,
        documents,
        document_count,
        budget,
        |_| cancellation.check().map_err(SearchError::from),
    )
}

fn populate_index_with_checkpoints(
    index: &Index,
    fields: &Fields,
    generation: GenerationId,
    documents: &[LexicalDocument],
    document_count: u64,
    budget: BuildBudget,
    mut check: impl FnMut(BuildCheckpoint) -> Result<(), SearchError>,
) -> Result<(), SearchError> {
    check(BuildCheckpoint::BeforeWriter)?;
    let mut writer = index
        .writer_with_num_threads(1, budget.indexer_memory_bytes)
        .map_err(|_| operation("writer"))?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for document in documents {
        check(BuildCheckpoint::BeforeDocument)?;
        writer
            .add_document(fields.encode(document)?)
            .map_err(|_| operation("add_document"))?;
    }
    check(BuildCheckpoint::BeforeCommit)?;
    let mut prepared = writer
        .prepare_commit()
        .map_err(|_| operation("prepare_commit"))?;
    prepared.set_payload(&format_payload(generation, document_count));
    prepared.commit().map_err(|_| operation("commit"))?;
    check(BuildCheckpoint::AfterCommit)?;
    drop(writer);
    Ok(())
}

fn open_reader(
    index: &Index,
    fields: &Fields,
    expected_generation: GenerationId,
    expected_documents: Option<u64>,
) -> Result<(GenerationId, IndexReader), SearchError> {
    if index.schema() != fields.schema {
        return Err(SearchError::IncompatibleIndex);
    }
    let metadata = index.load_metas().map_err(|_| operation("metadata"))?;
    let payload = metadata
        .payload
        .as_deref()
        .ok_or(SearchError::IncompatibleIndex)?;
    let (actual_generation, payload_documents) = parse_payload(payload)?;
    if payload_documents > HARD_MAX_DOCUMENTS as u64
        || expected_documents.is_some_and(|expected| payload_documents != expected)
    {
        return Err(SearchError::IncompatibleIndex);
    }
    if actual_generation != expected_generation {
        return Err(SearchError::GenerationMismatch {
            expected: expected_generation,
            actual: actual_generation,
        });
    }
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(|_| operation("reader"))?;
    reader.reload().map_err(|_| operation("reload"))?;
    let searcher = reader.searcher();
    if searcher
        .segment_readers()
        .iter()
        .any(|segment| segment.max_doc() != segment.num_docs())
        || searcher.num_docs() != payload_documents
    {
        return Err(SearchError::IncompatibleIndex);
    }
    Ok((actual_generation, reader))
}

fn open_reader_checked(
    index: &Index,
    fields: &Fields,
    expected_generation: GenerationId,
    expected_documents: Option<u64>,
    check: impl FnOnce() -> Result<(), SearchError>,
) -> Result<(GenerationId, IndexReader), SearchError> {
    let opened = open_reader(index, fields, expected_generation, expected_documents)?;
    check()?;
    Ok(opened)
}

#[derive(Clone)]
struct Fields {
    schema: Schema,
    stored_hit: Field,
    symbol_id: Field,
    file_id: Field,
    identifier_normalized: Field,
    identifier_text: Field,
    qualified_normalized: Field,
    qualified_text: Field,
    path_normalized: Field,
    path_text: Field,
    kind: Field,
    language: Field,
    tier: Field,
    package: Field,
    build_target: Field,
    signature: Field,
    type_names: Field,
    documentation: Field,
    generated: Field,
}

impl Fields {
    fn new() -> Self {
        let mut builder = Schema::builder();
        let stored_hit =
            builder.add_bytes_field("stored_hit", BytesOptions::default().set_stored());
        let identifiers = BytesOptions::default().set_indexed();
        let raw = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        );
        let code = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(CODE_TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        let symbol_id = builder.add_bytes_field("symbol_id", identifiers.clone());
        let file_id = builder.add_bytes_field("file_id", identifiers);
        let identifier_normalized = builder.add_text_field("identifier_normalized", raw.clone());
        let identifier_text = builder.add_text_field("identifier_text", code.clone());
        let qualified_normalized = builder.add_text_field("qualified_normalized", raw.clone());
        let qualified_text = builder.add_text_field("qualified_text", code.clone());
        let path_normalized = builder.add_text_field("path_normalized", raw);
        let path_text = builder.add_text_field("path_text", code.clone());
        let raw = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        );
        let kind = builder.add_text_field("kind", raw.clone());
        let language = builder.add_text_field("language", raw.clone());
        let tier = builder.add_text_field("tier", raw.clone());
        let package = builder.add_text_field("package", raw.clone());
        let build_target = builder.add_text_field("build_target", raw);
        let signature = builder.add_text_field("signature", code.clone());
        let type_names = builder.add_text_field("type_names", code.clone());
        let documentation = builder.add_text_field("documentation", code);
        let generated =
            builder.add_bool_field("generated", NumericOptions::default().set_indexed());
        Self {
            schema: builder.build(),
            stored_hit,
            symbol_id,
            file_id,
            identifier_normalized,
            identifier_text,
            qualified_normalized,
            qualified_text,
            path_normalized,
            path_text,
            kind,
            language,
            tier,
            package,
            build_target,
            signature,
            type_names,
            documentation,
            generated,
        }
    }

    fn encode(&self, source: &LexicalDocument) -> Result<TantivyDocument, SearchError> {
        let mut document = TantivyDocument::new();
        let stored_hit = encode_stored_hit(source)?;
        document.add_bytes(self.stored_hit, &stored_hit);
        document.add_bytes(self.symbol_id, source.symbol_id.as_bytes());
        document.add_bytes(self.file_id, source.file_id.as_bytes());
        document.add_text(
            self.identifier_normalized,
            normalize_exact(&source.identifier),
        );
        document.add_text(self.identifier_text, &source.identifier);
        document.add_text(
            self.qualified_normalized,
            normalize_exact(&source.qualified_name),
        );
        document.add_text(self.qualified_text, &source.qualified_name);
        document.add_text(self.path_normalized, normalize_exact(&source.path));
        document.add_text(self.path_text, &source.path);
        document.add_text(self.kind, &source.kind);
        document.add_text(self.language, &source.language);
        document.add_text(self.tier, &source.tier);
        if let Some(package) = &source.package {
            document.add_text(self.package, package);
        }
        if let Some(build_target) = &source.build_target {
            document.add_text(self.build_target, build_target);
        }
        if let Some(signature) = &source.signature {
            document.add_text(self.signature, signature);
        }
        for type_name in &source.type_names {
            document.add_text(self.type_names, type_name);
        }
        if let Some(documentation) = &source.documentation {
            document.add_text(self.documentation, documentation);
        }
        document.add_bool(self.generated, source.generated);
        Ok(document)
    }

    fn query(
        &self,
        request: &SearchRequest,
        searcher: &tantivy::Searcher,
        budget: SearchBudget,
        control: &SearchControl<'_>,
    ) -> Result<Box<dyn Query>, SearchError> {
        let normalized = normalize_exact(request.query.trim());
        let mut work = QueryWork::new(searcher, budget, control);
        let query = match request.mode {
            SearchMode::Exact => self.exact_query(&normalized, &mut work)?,
            SearchMode::Prefix => self.prefix_query(&normalized, &mut work)?,
            SearchMode::Text => self.text_query(&normalized, &mut work)?,
            SearchMode::SafeRegex => {
                self.pattern_query(&compile_safe_regex(&normalized)?, &mut work)?
            }
            SearchMode::Glob => self.pattern_query(&compile_safe_glob(&normalized)?, &mut work)?,
        };
        control.check()?;
        Ok(query)
    }

    fn exact_query(
        &self,
        normalized: &str,
        work: &mut QueryWork<'_>,
    ) -> Result<Box<dyn Query>, SearchError> {
        Ok(Box::new(BooleanQuery::new(vec![
            work.term_clause(
                self.identifier_normalized,
                normalized,
                IndexRecordOption::Basic,
                16.0,
            )?,
            work.term_clause(
                self.qualified_normalized,
                normalized,
                IndexRecordOption::Basic,
                12.0,
            )?,
            work.term_clause(
                self.path_normalized,
                normalized,
                IndexRecordOption::Basic,
                10.0,
            )?,
        ])))
    }

    fn prefix_query(
        &self,
        normalized: &str,
        work: &mut QueryWork<'_>,
    ) -> Result<Box<dyn Query>, SearchError> {
        let pattern = compile_literal_prefix(normalized)?;
        pattern_query(
            &pattern,
            &[
                (self.identifier_normalized, 12.0),
                (self.qualified_normalized, 8.0),
                (self.path_normalized, 6.0),
            ],
            work,
        )
    }

    fn text_query(
        &self,
        normalized: &str,
        work: &mut QueryWork<'_>,
    ) -> Result<Box<dyn Query>, SearchError> {
        let tokens = token_texts(normalized);
        if tokens.is_empty() {
            return Err(SearchError::InvalidQuery(QueryViolation::Empty));
        }
        let mut clauses = vec![
            work.term_clause(
                self.identifier_normalized,
                normalized,
                IndexRecordOption::Basic,
                24.0,
            )?,
            work.term_clause(
                self.qualified_normalized,
                normalized,
                IndexRecordOption::Basic,
                20.0,
            )?,
            work.term_clause(
                self.path_normalized,
                normalized,
                IndexRecordOption::Basic,
                18.0,
            )?,
        ];
        if normalized.chars().count() >= MIN_PATTERN_LITERALS {
            clauses.extend(pattern_clauses(
                &compile_literal_prefix(normalized)?,
                &[
                    (self.identifier_normalized, 16.0),
                    (self.qualified_normalized, 12.0),
                    (self.path_normalized, 10.0),
                ],
                work,
            )?);
        }
        let mut token_clauses = Vec::with_capacity(tokens.len());
        for token in tokens {
            let alternatives = BooleanQuery::new(vec![
                work.term_clause(
                    self.identifier_text,
                    &token,
                    IndexRecordOption::WithFreqsAndPositions,
                    10.0,
                )?,
                work.term_clause(
                    self.qualified_text,
                    &token,
                    IndexRecordOption::WithFreqsAndPositions,
                    8.0,
                )?,
                work.term_clause(
                    self.path_text,
                    &token,
                    IndexRecordOption::WithFreqsAndPositions,
                    6.0,
                )?,
                work.term_clause(
                    self.signature,
                    &token,
                    IndexRecordOption::WithFreqsAndPositions,
                    3.0,
                )?,
                work.term_clause(
                    self.type_names,
                    &token,
                    IndexRecordOption::WithFreqsAndPositions,
                    3.0,
                )?,
                work.term_clause(
                    self.documentation,
                    &token,
                    IndexRecordOption::WithFreqsAndPositions,
                    1.0,
                )?,
            ]);
            token_clauses.push((Occur::Must, Box::new(alternatives) as Box<dyn Query>));
        }
        clauses.push((Occur::Should, Box::new(BooleanQuery::new(token_clauses))));
        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    fn pattern_query(
        &self,
        pattern: &BoundedPattern,
        work: &mut QueryWork<'_>,
    ) -> Result<Box<dyn Query>, SearchError> {
        pattern_query(
            pattern,
            &[
                (self.identifier_normalized, 10.0),
                (self.qualified_normalized, 6.0),
                (self.path_normalized, 4.0),
            ],
            work,
        )
    }

    fn decode(
        &self,
        document: TantivyDocument,
        score: f32,
    ) -> Result<(SearchHit, usize), SearchError> {
        if !score.is_finite() || score < 0.0 {
            return Err(SearchError::IncompatibleIndex);
        }
        let mut values = document.field_values();
        let (field, value) = values.next().ok_or(SearchError::IncompatibleIndex)?;
        if field != self.stored_hit || values.next().is_some() {
            return Err(SearchError::IncompatibleIndex);
        }
        let bytes = value.as_bytes().ok_or(SearchError::IncompatibleIndex)?;
        decode_stored_hit(bytes, score)
    }
}

fn encode_stored_hit(source: &LexicalDocument) -> Result<Vec<u8>, SearchError> {
    let mut encoded = Vec::new();
    encoded.push(STORED_HIT_VERSION);
    encoded.extend_from_slice(source.symbol_id.as_bytes());
    encoded.extend_from_slice(source.file_id.as_bytes());
    encoded.push(u8::from(source.generated));
    for text in [
        source.identifier.as_str(),
        source.qualified_name.as_str(),
        source.path.as_str(),
        source.kind.as_str(),
        source.language.as_str(),
        source.tier.as_str(),
    ] {
        let length = u32::try_from(text.len()).map_err(|_| SearchError::BuildBudgetExceeded {
            resource: "stored_hit_bytes",
        })?;
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(text.as_bytes());
    }
    Ok(encoded)
}

fn decode_stored_hit(bytes: &[u8], score: f32) -> Result<(SearchHit, usize), SearchError> {
    let mut decoder = StoredHitDecoder::new(bytes);
    if decoder.read_u8()? != STORED_HIT_VERSION {
        return Err(SearchError::IncompatibleIndex);
    }
    let symbol_id = SymbolId::from_bytes(decoder.read_array()?);
    let file_id = FileId::from_bytes(decoder.read_array()?);
    let generated = match decoder.read_u8()? {
        0 => false,
        1 => true,
        _ => return Err(SearchError::IncompatibleIndex),
    };
    let identifier = decoder.read_text(MAX_IDENTIFIER_BYTES, TextValidation::Tokenized)?;
    let qualified_name = decoder.read_text(MAX_QUALIFIED_BYTES, TextValidation::Tokenized)?;
    let path = decoder.read_text(MAX_PATH_BYTES, TextValidation::Path)?;
    let kind = decoder.read_text(MAX_LABEL_BYTES, TextValidation::Label)?;
    let language = decoder.read_text(MAX_LABEL_BYTES, TextValidation::Label)?;
    let tier = decoder.read_text(MAX_LABEL_BYTES, TextValidation::Label)?;
    if !decoder.is_finished() {
        return Err(SearchError::IncompatibleIndex);
    }
    let text_bytes = [
        identifier.len(),
        qualified_name.len(),
        path.len(),
        kind.len(),
        language.len(),
        tier.len(),
    ]
    .into_iter()
    .try_fold(0usize, |total, length| total.checked_add(length))
    .ok_or(SearchError::IncompatibleIndex)?;
    Ok((
        SearchHit {
            symbol_id,
            file_id,
            identifier,
            qualified_name,
            path,
            kind,
            language,
            tier,
            generated,
            relevance_score: score,
        },
        text_bytes,
    ))
}

#[derive(Clone, Copy)]
enum TextValidation {
    Tokenized,
    Label,
    Path,
}

struct StoredHitDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> StoredHitDecoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, SearchError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or(SearchError::IncompatibleIndex)?;
        self.offset = self
            .offset
            .checked_add(1)
            .ok_or(SearchError::IncompatibleIndex)?;
        Ok(byte)
    }

    fn read_array<const SIZE: usize>(&mut self) -> Result<[u8; SIZE], SearchError> {
        self.take(SIZE)?
            .try_into()
            .map_err(|_| SearchError::IncompatibleIndex)
    }

    fn read_text(
        &mut self,
        limit: usize,
        validation: TextValidation,
    ) -> Result<String, SearchError> {
        let length = usize::try_from(u32::from_be_bytes(self.read_array()?))
            .map_err(|_| SearchError::IncompatibleIndex)?;
        if length == 0 || length > limit {
            return Err(SearchError::IncompatibleIndex);
        }
        let text =
            std::str::from_utf8(self.take(length)?).map_err(|_| SearchError::IncompatibleIndex)?;
        let valid = match validation {
            TextValidation::Tokenized => !text.contains('\0') && !has_oversized_term(text),
            TextValidation::Label => {
                !text.contains('\0')
                    && text.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'_' | b'-')
                    })
            }
            TextValidation::Path => validate_path(text).is_ok(),
        };
        if !valid {
            return Err(SearchError::IncompatibleIndex);
        }
        Ok(text.to_owned())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SearchError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SearchError::IncompatibleIndex)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(SearchError::IncompatibleIndex)?;
        self.offset = end;
        Ok(value)
    }

    const fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

struct RankedHit {
    rank: u8,
    hit: SearchHit,
}

struct SearchControl<'a> {
    cancellation: &'a Cancellation,
    started: Instant,
    max_duration: Duration,
}

impl<'a> SearchControl<'a> {
    fn new(cancellation: &'a Cancellation, max_duration: Duration) -> Self {
        Self {
            cancellation,
            started: Instant::now(),
            max_duration,
        }
    }

    fn check(&self) -> Result<(), SearchError> {
        self.cancellation.check()?;
        if !self.max_duration.is_zero() && self.started.elapsed() >= self.max_duration {
            return Err(SearchError::Cancelled(CancellationReason::DeadlineExceeded));
        }
        Ok(())
    }
}

struct QueryWork<'a> {
    searcher: &'a tantivy::Searcher,
    budget: SearchBudget,
    control: &'a SearchControl<'a>,
    terms: BTreeSet<(Field, Vec<u8>)>,
    examined_terms: usize,
    postings: u64,
}

impl<'a> QueryWork<'a> {
    fn new(
        searcher: &'a tantivy::Searcher,
        budget: SearchBudget,
        control: &'a SearchControl<'a>,
    ) -> Self {
        Self {
            searcher,
            budget,
            control,
            terms: BTreeSet::new(),
            examined_terms: 0,
            postings: 0,
        }
    }

    fn term_clause(
        &mut self,
        field: Field,
        value: &str,
        index_option: IndexRecordOption,
        boost: f32,
    ) -> Result<QueryClause, SearchError> {
        let term = Term::from_field_text(field, value);
        self.admit_direct(&term)?;
        Ok(boosted_term(term, index_option, boost))
    }

    fn admit_direct(&mut self, term: &Term) -> Result<(), SearchError> {
        let key = (term.field(), term.serialized_value_bytes().to_vec());
        if self.terms.contains(&key) {
            return Ok(());
        }
        self.require_term_capacity(1)?;
        self.control.check()?;
        let postings = self
            .searcher
            .doc_freq(term)
            .map_err(|_| operation("term_statistics"))?;
        self.add_postings(postings)?;
        self.terms.insert(key);
        self.control.check()
    }

    fn expand(
        &mut self,
        field: Field,
        pattern: &BoundedPattern,
        boost: f32,
    ) -> Result<Vec<QueryClause>, SearchError> {
        let mut expanded = BTreeMap::<Vec<u8>, u64>::new();
        let mut local_postings = 0u64;
        for segment_reader in self.searcher.segment_readers() {
            self.control.check()?;
            let inverted_index = segment_reader
                .inverted_index(field)
                .map_err(|_| operation("term_dictionary"))?;
            let mut stream = inverted_index
                .terms()
                .range()
                .ge(pattern.prefix.as_slice())
                .lt(pattern.upper_bound.as_slice())
                .into_stream()
                .map_err(|_| operation("term_dictionary"))?;
            loop {
                self.control.check()?;
                let advanced = stream.advance();
                self.control.check()?;
                if !advanced {
                    break;
                }
                self.admit_examined_term()?;
                if !pattern.is_match(stream.key()) {
                    continue;
                }
                let key = stream.key().to_vec();
                if self.terms.contains(&(field, key.clone())) {
                    continue;
                }
                let is_new = !expanded.contains_key(&key);
                if is_new {
                    self.require_term_capacity(
                        expanded
                            .len()
                            .checked_add(1)
                            .ok_or(SearchError::TermExpansionBudgetExceeded)?,
                    )?;
                }
                let postings = u64::from(stream.value().doc_freq);
                let entry = expanded.entry(key).or_default();
                *entry = entry
                    .checked_add(postings)
                    .ok_or(SearchError::PostingBudgetExceeded)?;
                local_postings = local_postings
                    .checked_add(postings)
                    .ok_or(SearchError::PostingBudgetExceeded)?;
                self.require_total_postings(local_postings)?;
            }
        }

        let mut clauses = Vec::with_capacity(expanded.len());
        for (key, postings) in expanded {
            self.control.check()?;
            let value = std::str::from_utf8(&key).map_err(|_| SearchError::IncompatibleIndex)?;
            let term = Term::from_field_text(field, value);
            self.require_term_capacity(1)?;
            self.add_postings(postings)?;
            self.terms.insert((field, key));
            clauses.push(boosted_term(term, IndexRecordOption::Basic, boost));
        }
        Ok(clauses)
    }

    fn admit_examined_term(&mut self) -> Result<(), SearchError> {
        let examined_terms = self
            .examined_terms
            .checked_add(1)
            .ok_or(SearchError::TermExaminationBudgetExceeded)?;
        if examined_terms > self.budget.max_examined_terms {
            return Err(SearchError::TermExaminationBudgetExceeded);
        }
        self.examined_terms = examined_terms;
        Ok(())
    }

    fn require_term_capacity(&self, additional: usize) -> Result<(), SearchError> {
        let observed = self
            .terms
            .len()
            .checked_add(additional)
            .ok_or(SearchError::TermExpansionBudgetExceeded)?;
        if observed > self.budget.max_expanded_terms {
            Err(SearchError::TermExpansionBudgetExceeded)
        } else {
            Ok(())
        }
    }

    fn require_total_postings(&self, additional: u64) -> Result<(), SearchError> {
        let observed = self
            .postings
            .checked_add(additional)
            .ok_or(SearchError::PostingBudgetExceeded)?;
        if observed > self.budget.max_postings {
            Err(SearchError::PostingBudgetExceeded)
        } else {
            Ok(())
        }
    }

    fn add_postings(&mut self, additional: u64) -> Result<(), SearchError> {
        self.require_total_postings(additional)?;
        self.postings = self
            .postings
            .checked_add(additional)
            .ok_or(SearchError::PostingBudgetExceeded)?;
        Ok(())
    }
}

fn pattern_query(
    pattern: &BoundedPattern,
    fields: &[(Field, f32)],
    work: &mut QueryWork<'_>,
) -> Result<Box<dyn Query>, SearchError> {
    let clauses = pattern_clauses(pattern, fields, work)?;
    if clauses.is_empty() {
        Ok(Box::new(EmptyQuery))
    } else {
        Ok(Box::new(BooleanQuery::new(clauses)))
    }
}

fn pattern_clauses(
    pattern: &BoundedPattern,
    fields: &[(Field, f32)],
    work: &mut QueryWork<'_>,
) -> Result<Vec<QueryClause>, SearchError> {
    let mut clauses = Vec::new();
    for &(field, boost) in fields {
        clauses.extend(work.expand(field, pattern, boost)?);
    }
    Ok(clauses)
}

struct BoundedPattern {
    regex: Regex,
    prefix: Vec<u8>,
    upper_bound: Vec<u8>,
}

impl BoundedPattern {
    fn compile(expression: String, prefix: String) -> Result<Self, SearchError> {
        if prefix.chars().count() < MIN_PATTERN_LITERALS {
            return Err(SearchError::InvalidQuery(
                QueryViolation::InsufficientLiteral,
            ));
        }
        let regex = Regex::new(&expression)
            .map_err(|_| SearchError::InvalidQuery(QueryViolation::UnsupportedPattern))?;
        let prefix = prefix.into_bytes();
        let upper_bound = prefix_upper_bound(&prefix).ok_or(SearchError::InvalidQuery(
            QueryViolation::UnsupportedPattern,
        ))?;
        Ok(Self {
            regex,
            prefix,
            upper_bound,
        })
    }

    fn is_match(&self, candidate: &[u8]) -> bool {
        let mut state = self.regex.start();
        for byte in candidate {
            state = self.regex.accept(&state, *byte);
        }
        self.regex.is_match(&state)
    }
}

fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    for index in (0..upper.len()).rev() {
        if upper[index] != u8::MAX {
            upper[index] = upper[index].checked_add(1)?;
            upper.truncate(index.checked_add(1)?);
            return Some(upper);
        }
    }
    None
}

fn compile_literal_prefix(input: &str) -> Result<BoundedPattern, SearchError> {
    BoundedPattern::compile(
        format!("{}.*", escape_regex_literal(input)),
        input.to_owned(),
    )
}

fn boosted_term(term: Term, index_option: IndexRecordOption, boost: f32) -> QueryClause {
    (
        Occur::Should,
        Box::new(BoostQuery::new(
            Box::new(TermQuery::new(term, index_option)),
            boost,
        )),
    )
}

fn validate_build_budget(budget: BuildBudget) -> Result<(), SearchError> {
    if budget.max_documents == 0 || budget.max_documents > HARD_MAX_DOCUMENTS {
        return Err(SearchError::BuildBudgetExceeded {
            resource: "documents",
        });
    }
    if budget.max_text_bytes == 0 || budget.max_text_bytes > HARD_MAX_TEXT_BYTES {
        return Err(SearchError::BuildBudgetExceeded {
            resource: "text_bytes",
        });
    }
    if !(MIN_WRITER_HEAP_BYTES..=HARD_MAX_WRITER_HEAP_BYTES).contains(&budget.indexer_memory_bytes)
    {
        return Err(SearchError::BuildBudgetExceeded {
            resource: "indexer_memory_bytes",
        });
    }
    Ok(())
}

fn ensure_empty_directory(directory: &Path) -> Result<(), SearchError> {
    let mut entries = fs::read_dir(directory).map_err(|_| operation("staging_directory"))?;
    if entries
        .next()
        .transpose()
        .map_err(|_| operation("staging_directory"))?
        .is_some()
    {
        return Err(SearchError::NonEmptyStaging);
    }
    Ok(())
}

fn validate_document(document: &LexicalDocument) -> Result<usize, SearchError> {
    let mut bytes = 0usize;
    bytes = add_required(
        bytes,
        &document.identifier,
        MAX_IDENTIFIER_BYTES,
        DocumentField::Identifier,
        true,
    )?;
    bytes = add_required(
        bytes,
        &document.qualified_name,
        MAX_QUALIFIED_BYTES,
        DocumentField::QualifiedName,
        true,
    )?;
    validate_path(&document.path)?;
    bytes = checked_text_bytes(bytes, document.path.len())?;
    bytes = add_label(bytes, &document.kind, DocumentField::Kind)?;
    bytes = add_label(bytes, &document.language, DocumentField::Language)?;
    bytes = add_label(bytes, &document.tier, DocumentField::Tier)?;
    bytes = add_optional(
        bytes,
        document.package.as_deref(),
        MAX_OWNERSHIP_BYTES,
        DocumentField::Package,
        false,
    )?;
    bytes = add_optional(
        bytes,
        document.build_target.as_deref(),
        MAX_OWNERSHIP_BYTES,
        DocumentField::BuildTarget,
        false,
    )?;
    bytes = add_optional(
        bytes,
        document.signature.as_deref(),
        MAX_SIGNATURE_BYTES,
        DocumentField::Signature,
        true,
    )?;
    if document.type_names.len() > MAX_TYPE_NAMES {
        return Err(SearchError::InvalidDocument {
            field: DocumentField::TypeName,
        });
    }
    for type_name in &document.type_names {
        bytes = add_required(
            bytes,
            type_name,
            MAX_TYPE_NAME_BYTES,
            DocumentField::TypeName,
            true,
        )?;
    }
    add_optional(
        bytes,
        document.documentation.as_deref(),
        MAX_DOCUMENTATION_BYTES,
        DocumentField::Documentation,
        true,
    )
}

fn add_label(bytes: usize, value: &str, field: DocumentField) -> Result<usize, SearchError> {
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    }) {
        return Err(SearchError::InvalidDocument { field });
    }
    add_required(bytes, value, MAX_LABEL_BYTES, field, false)
}

fn add_required(
    bytes: usize,
    value: &str,
    limit: usize,
    field: DocumentField,
    tokenized: bool,
) -> Result<usize, SearchError> {
    if value.is_empty()
        || value.len() > limit
        || value.contains('\0')
        || (tokenized && has_oversized_term(value))
    {
        return Err(SearchError::InvalidDocument { field });
    }
    checked_text_bytes(bytes, value.len())
}

fn add_optional(
    bytes: usize,
    value: Option<&str>,
    limit: usize,
    field: DocumentField,
    tokenized: bool,
) -> Result<usize, SearchError> {
    match value {
        Some(value) => add_required(bytes, value, limit, field, tokenized),
        None => Ok(bytes),
    }
}

fn checked_text_bytes(bytes: usize, additional: usize) -> Result<usize, SearchError> {
    bytes
        .checked_add(additional)
        .ok_or(SearchError::BuildBudgetExceeded {
            resource: "text_bytes",
        })
}

fn validate_path(path: &str) -> Result<(), SearchError> {
    let invalid = path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains(['\0', '\\', ':'])
        || path.starts_with('/')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        || has_oversized_term(path);
    if invalid {
        return Err(SearchError::InvalidDocument {
            field: DocumentField::Path,
        });
    }
    Ok(())
}

fn validate_request(request: &SearchRequest, budget: SearchBudget) -> Result<(), SearchError> {
    validate_search_budget(budget)?;
    let query = request.query.trim();
    if query.is_empty() {
        return Err(SearchError::InvalidQuery(QueryViolation::Empty));
    }
    if query.len() > budget.max_query_bytes {
        return Err(SearchError::InvalidQuery(QueryViolation::TooLong));
    }
    if has_oversized_term(query) {
        return Err(SearchError::InvalidQuery(QueryViolation::TermTooLong));
    }
    if query.chars().any(char::is_control) {
        return Err(SearchError::InvalidQuery(
            QueryViolation::UnsupportedCharacter,
        ));
    }
    if request.max_results == 0 || request.max_results > budget.max_results {
        return Err(SearchError::InvalidResultLimit);
    }
    if request.mode == SearchMode::Text && token_texts(query).len() > budget.max_terms {
        return Err(SearchError::InvalidQueryBudget {
            resource: "query_terms",
        });
    }
    Ok(())
}

fn validate_search_budget(budget: SearchBudget) -> Result<(), SearchError> {
    for (resource, value, hard_maximum) in [
        ("query_bytes", budget.max_query_bytes, HARD_MAX_QUERY_BYTES),
        (
            "candidates",
            budget.max_candidates,
            HARD_MAX_QUERY_CANDIDATES,
        ),
        ("results", budget.max_results, HARD_MAX_QUERY_RESULTS),
        ("query_terms", budget.max_terms, HARD_MAX_QUERY_TERMS),
        (
            "expanded_terms",
            budget.max_expanded_terms,
            HARD_MAX_EXPANDED_TERMS,
        ),
        (
            "examined_terms",
            budget.max_examined_terms,
            HARD_MAX_EXAMINED_TERMS,
        ),
        (
            "returned_text_bytes",
            budget.max_returned_text_bytes,
            HARD_MAX_RETURNED_TEXT_BYTES,
        ),
    ] {
        if value == 0 || value > hard_maximum {
            return Err(SearchError::InvalidQueryBudget { resource });
        }
    }
    if budget.max_postings == 0 || budget.max_postings > HARD_MAX_QUERY_POSTINGS {
        return Err(SearchError::InvalidQueryBudget {
            resource: "postings",
        });
    }
    if budget.max_duration.is_zero() || budget.max_duration > HARD_MAX_QUERY_DURATION {
        return Err(SearchError::InvalidQueryBudget {
            resource: "duration",
        });
    }
    Ok(())
}

fn register_tokenizer(index: &Index) {
    index
        .tokenizers()
        .register(CODE_TOKENIZER, TextAnalyzer::from(CodeTokenizer::default()));
}

fn normalize_exact(input: &str) -> String {
    normalize_text(input)
}

fn escape_regex_literal(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for character in input.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn compile_safe_regex(input: &str) -> Result<BoundedPattern, SearchError> {
    let mut output = String::with_capacity(input.len());
    let mut prefix = String::new();
    let mut prefix_open = true;
    let mut wildcard_count = 0usize;
    let mut literal_count = 0usize;
    let mut previous_star = false;
    let mut previous_literal = false;

    for (position, character) in input.chars().enumerate() {
        match character {
            character
                if character.is_alphanumeric()
                    || matches!(character, '_' | '-' | ':' | '/' | '$') =>
            {
                output.push_str(&escape_regex_literal(&character.to_string()));
                if prefix_open {
                    prefix.push(character);
                }
                literal_count += 1;
                previous_star = false;
                previous_literal = true;
            }
            '.' => {
                if position == 0 {
                    return Err(SearchError::InvalidQuery(QueryViolation::LeadingWildcard));
                }
                output.push('.');
                wildcard_count += 1;
                prefix_open = false;
                previous_star = false;
                previous_literal = false;
            }
            '*' => {
                if position == 0 {
                    return Err(SearchError::InvalidQuery(QueryViolation::LeadingWildcard));
                }
                if previous_star {
                    return Err(SearchError::InvalidQuery(
                        QueryViolation::UnsupportedPattern,
                    ));
                }
                output.push('*');
                wildcard_count += 1;
                if previous_literal {
                    literal_count = literal_count.saturating_sub(1);
                    if prefix_open {
                        prefix.pop();
                    }
                }
                prefix_open = false;
                previous_star = true;
                previous_literal = false;
            }
            _ => {
                return Err(SearchError::InvalidQuery(
                    QueryViolation::UnsupportedPattern,
                ));
            }
        }
        if wildcard_count > MAX_PATTERN_WILDCARDS {
            return Err(SearchError::InvalidQuery(QueryViolation::TooManyWildcards));
        }
    }
    if literal_count < MIN_PATTERN_LITERALS {
        return Err(SearchError::InvalidQuery(
            QueryViolation::InsufficientLiteral,
        ));
    }
    BoundedPattern::compile(output, prefix)
}

fn compile_safe_glob(input: &str) -> Result<BoundedPattern, SearchError> {
    let mut output = String::with_capacity(input.len() * 2);
    let mut prefix = String::new();
    let mut prefix_open = true;
    let mut wildcard_count = 0usize;
    let mut literal_count = 0usize;
    let mut previous_star = false;

    for (position, character) in input.chars().enumerate() {
        match character {
            '*' => {
                if position == 0 {
                    return Err(SearchError::InvalidQuery(QueryViolation::LeadingWildcard));
                }
                if previous_star {
                    return Err(SearchError::InvalidQuery(
                        QueryViolation::UnsupportedPattern,
                    ));
                }
                output.push_str(".*");
                wildcard_count += 1;
                prefix_open = false;
                previous_star = true;
            }
            '?' => {
                if position == 0 {
                    return Err(SearchError::InvalidQuery(QueryViolation::LeadingWildcard));
                }
                output.push('.');
                wildcard_count += 1;
                prefix_open = false;
                previous_star = false;
            }
            character
                if character.is_alphanumeric()
                    || matches!(character, '_' | '-' | ':' | '/' | '.' | '$') =>
            {
                output.push_str(&escape_regex_literal(&character.to_string()));
                if prefix_open {
                    prefix.push(character);
                }
                literal_count += 1;
                previous_star = false;
            }
            _ => {
                return Err(SearchError::InvalidQuery(
                    QueryViolation::UnsupportedPattern,
                ));
            }
        }
        if wildcard_count > MAX_PATTERN_WILDCARDS {
            return Err(SearchError::InvalidQuery(QueryViolation::TooManyWildcards));
        }
    }
    if literal_count < MIN_PATTERN_LITERALS {
        return Err(SearchError::InvalidQuery(
            QueryViolation::InsufficientLiteral,
        ));
    }
    BoundedPattern::compile(output, prefix)
}

fn lexical_rank(mode: SearchMode, normalized_query: &str, hit: &SearchHit) -> u8 {
    let identifier = normalize_exact(&hit.identifier);
    let qualified = normalize_exact(&hit.qualified_name);
    let path = normalize_exact(&hit.path);
    match mode {
        SearchMode::Exact => {
            if identifier == normalized_query {
                0
            } else if qualified == normalized_query {
                1
            } else {
                2
            }
        }
        SearchMode::Prefix | SearchMode::Text => {
            if identifier == normalized_query {
                0
            } else if qualified == normalized_query {
                1
            } else if path == normalized_query {
                2
            } else if identifier.starts_with(normalized_query) {
                3
            } else if qualified.starts_with(normalized_query) {
                4
            } else if path.starts_with(normalized_query) {
                5
            } else {
                6
            }
        }
        SearchMode::SafeRegex | SearchMode::Glob => 6,
    }
}

fn format_payload(generation: GenerationId, documents: u64) -> String {
    format!(
        "{FORMAT_PREFIX};version={FORMAT_VERSION};generation={generation};documents={documents}"
    )
}

fn parse_payload(payload: &str) -> Result<(GenerationId, u64), SearchError> {
    let mut fields = payload.split(';');
    if fields.next() != Some(FORMAT_PREFIX)
        || fields
            .next()
            .and_then(|field| field.strip_prefix("version="))
            != Some(FORMAT_VERSION)
    {
        return Err(SearchError::IncompatibleIndex);
    }
    let generation = fields
        .next()
        .and_then(|field| field.strip_prefix("generation="))
        .and_then(|value| GenerationId::from_str(value).ok())
        .ok_or(SearchError::IncompatibleIndex)?;
    let documents = fields
        .next()
        .and_then(|field| field.strip_prefix("documents="))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(SearchError::IncompatibleIndex)?;
    if fields.next().is_some() {
        return Err(SearchError::IncompatibleIndex);
    }
    Ok((generation, documents))
}

fn operation(operation: &'static str) -> SearchError {
    SearchError::IndexOperation { operation }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs,
        sync::{Arc, Barrier},
    };

    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::{FileId, GenerationId, SymbolId};
    use tempfile::TempDir;

    use super::*;

    fn generation(byte: u8) -> GenerationId {
        GenerationId::from_bytes([byte; 20])
    }

    fn document(byte: u8, identifier: &str, path: &str) -> LexicalDocument {
        LexicalDocument {
            symbol_id: SymbolId::from_bytes([byte; 20]),
            file_id: FileId::from_bytes([byte.wrapping_add(1); 20]),
            identifier: identifier.to_owned(),
            qualified_name: format!("crate::{identifier}"),
            path: path.to_owned(),
            kind: "function".to_owned(),
            language: "rust".to_owned(),
            tier: "syntax_exact".to_owned(),
            package: Some("rootlight-search".to_owned()),
            build_target: Some("lib".to_owned()),
            signature: Some(format!("fn {identifier}(input: QueryBudget)")),
            type_names: vec!["QueryBudget".to_owned()],
            documentation: Some("Runs bounded deterministic lexical search.".to_owned()),
            generated: false,
        }
    }

    fn open_index(
        directory: &Path,
        manifest: LexicalArtifactManifest,
        expected_generation: GenerationId,
    ) -> LexicalIndex {
        let artifact = VerifiedLexicalArtifact::verify(
            directory,
            manifest,
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("artifact verifies");
        LexicalIndex::open(artifact, expected_generation).expect("index opens")
    }

    fn build(documents: Vec<LexicalDocument>) -> (TempDir, LexicalArtifactManifest, LexicalIndex) {
        let directory = TempDir::new().expect("temp directory");
        let manifest = LexicalIndexBuilder::build(
            directory.path(),
            generation(7),
            documents,
            BuildBudget::default(),
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("index builds");
        let index = open_index(directory.path(), manifest.clone(), generation(7));
        (directory, manifest, index)
    }

    #[test]
    fn ephemeral_index_uses_the_same_generation_and_query_contract() {
        let expected_generation = generation(19);
        let index = LexicalIndex::build_ephemeral(
            expected_generation,
            vec![
                document(2, "HTTPServer", "src/net/server.rs"),
                document(1, "query_budget", "src/search/budget.rs"),
            ],
            BuildBudget::default(),
            &Cancellation::new(),
        )
        .expect("ephemeral index builds");

        assert_eq!(index.generation(), expected_generation);
        assert_eq!(index.document_count(), 2);
        assert_eq!(
            search(&index, "query_budget", SearchMode::Exact)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
    }

    fn search(index: &LexicalIndex, query: &str, mode: SearchMode) -> Vec<SearchHit> {
        index
            .search(
                &SearchRequest {
                    query: query.to_owned(),
                    mode,
                    max_results: 10,
                },
                SearchBudget::default(),
                &Cancellation::new(),
            )
            .expect("query succeeds")
    }

    #[test]
    fn exact_prefix_path_docs_and_compound_identifier_search() {
        let (_directory, _manifest, index) = build(vec![
            document(2, "HTTPServer", "src/net/server.rs"),
            document(1, "query_budget", "src/search/budget.rs"),
            document(3, "CaféValue", "src/unicode.rs"),
        ]);

        assert_eq!(
            search(&index, "httpserver", SearchMode::Exact)[0].symbol_id,
            SymbolId::from_bytes([2; 20])
        );
        assert_eq!(
            search(&index, "http", SearchMode::Prefix)[0].symbol_id,
            SymbolId::from_bytes([2; 20])
        );
        assert_eq!(
            search(&index, "httpser", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([2; 20])
        );
        assert_eq!(
            search(&index, "src/search", SearchMode::Prefix)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
        assert_eq!(
            search(&index, "search budget", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
        assert_eq!(
            search(&index, "deterministic lexical", SearchMode::Text).len(),
            3
        );
        assert_eq!(
            search(&index, "server rs", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([2; 20])
        );
        assert_eq!(
            search(&index, "café value", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([3; 20])
        );
    }

    #[test]
    fn search_outcome_counts_candidates_before_result_truncation() {
        let (_directory, _manifest, index) = build(vec![
            document(2, "HTTPServer", "src/net/server.rs"),
            document(1, "QueryBudget", "src/search/budget.rs"),
            document(3, "QueryPlan", "src/query/plan.rs"),
        ]);
        let outcome = index
            .search_with_stats(
                &SearchRequest {
                    query: "query".to_owned(),
                    mode: SearchMode::Prefix,
                    max_results: 1,
                },
                SearchBudget::default(),
                &Cancellation::new(),
            )
            .expect("bounded query succeeds");

        assert_eq!(outcome.matched_candidates, 2);
        assert_eq!(outcome.hits.len(), 1);
        assert!(outcome.materialized_text_bytes > 0);
    }

    #[test]
    fn safe_regex_and_glob_are_bounded_before_automaton_compilation() {
        let (_directory, _manifest, index) = build(vec![
            document(1, "query_budget", "src/search.rs"),
            document(2, "$fetchData", "web/fetch.js"),
        ]);
        assert_eq!(search(&index, "query.*", SearchMode::SafeRegex).len(), 1);
        assert_eq!(search(&index, "query*", SearchMode::Glob).len(), 1);
        assert_eq!(search(&index, "src/search*", SearchMode::Glob).len(), 1);
        assert_eq!(search(&index, "$fetch.*", SearchMode::SafeRegex).len(), 1);
        assert_eq!(search(&index, "$fetch*", SearchMode::Glob).len(), 1);
        for pattern in [
            "*", ".*query", "q*", "query**", "[query]", "q.....u", "a*b*.*", "ab*cdef",
        ] {
            let error = index
                .search(
                    &SearchRequest {
                        query: pattern.to_owned(),
                        mode: SearchMode::SafeRegex,
                        max_results: 10,
                    },
                    SearchBudget::default(),
                    &Cancellation::new(),
                )
                .expect_err("unsafe pattern is rejected");
            assert!(matches!(error, SearchError::InvalidQuery(_)));
        }
        for pattern in [
            "*", "?query", "q*", "query**", "[query]", "q?????u", "a*bcde",
        ] {
            let error = index
                .search(
                    &SearchRequest {
                        query: pattern.to_owned(),
                        mode: SearchMode::Glob,
                        max_results: 10,
                    },
                    SearchBudget::default(),
                    &Cancellation::new(),
                )
                .expect_err("unsafe glob is rejected");
            assert!(matches!(error, SearchError::InvalidQuery(_)));
        }
    }

    #[test]
    fn ranking_ties_are_stable_across_input_order_and_reopen() {
        let left = document(1, "same_name", "src/a.rs");
        let right = document(2, "same_name", "src/b.rs");
        let (directory, manifest, index) = build(vec![right.clone(), left.clone()]);
        let first: Vec<_> = search(&index, "same name", SearchMode::Text)
            .into_iter()
            .map(|hit| hit.symbol_id)
            .collect();
        let (_other_directory, _other_manifest, other_index) = build(vec![left, right]);
        let other: Vec<_> = search(&other_index, "same name", SearchMode::Text)
            .into_iter()
            .map(|hit| hit.symbol_id)
            .collect();
        drop(index);
        let reopened = open_index(directory.path(), manifest, generation(7));
        let second: Vec<_> = search(&reopened, "same name", SearchMode::Text)
            .into_iter()
            .map(|hit| hit.symbol_id)
            .collect();

        assert_eq!(
            first,
            [SymbolId::from_bytes([1; 20]), SymbolId::from_bytes([2; 20])]
        );
        assert_eq!(first, other);
        assert_eq!(first, second);
    }

    #[test]
    fn ordering_is_stable_across_permutations_and_parallel_callers() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<LexicalIndex>();
        let documents = [
            document(1, "same_name", "src/a.rs"),
            document(2, "same_name", "src/b.rs"),
            document(3, "same_name", "src/c.rs"),
        ];
        let permutations = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let expected = [
            SymbolId::from_bytes([1; 20]),
            SymbolId::from_bytes([2; 20]),
            SymbolId::from_bytes([3; 20]),
        ];

        for permutation in permutations {
            let ordered = permutation
                .map(|index| documents[index].clone())
                .into_iter()
                .collect::<Vec<_>>();
            let index = LexicalIndex::build_ephemeral(
                generation(29),
                ordered,
                BuildBudget::default(),
                &Cancellation::new(),
            )
            .expect("permuted ephemeral index builds");
            let actual = search(&index, "same name", SearchMode::Text)
                .into_iter()
                .map(|hit| hit.symbol_id)
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }

        let shared = Arc::new(
            LexicalIndex::build_ephemeral(
                generation(29),
                documents.to_vec(),
                BuildBudget::default(),
                &Cancellation::new(),
            )
            .expect("shared ephemeral index builds"),
        );
        for caller_count in [1_usize, 2, 4] {
            let barrier = Arc::new(Barrier::new(caller_count));
            let handles = (0..caller_count)
                .map(|_| {
                    let index = Arc::clone(&shared);
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        search(&index, "same name", SearchMode::Text)
                            .into_iter()
                            .map(|hit| hit.symbol_id)
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                assert_eq!(
                    handle.join().expect("parallel search caller joins"),
                    expected
                );
            }
        }
    }

    #[test]
    fn ordering_checks_cancellation_during_bounded_sort_work() {
        let mut interrupted = (0_u32..4_096).rev().collect::<Vec<_>>();
        let checks = Cell::new(0_usize);
        let error = sort_with_checkpoints(&mut interrupted, Ord::cmp, || {
            let current = checks.get() + 1;
            checks.set(current);
            if current == 3 {
                Err(SearchError::Cancelled(CancellationReason::ClientRequest))
            } else {
                Ok(())
            }
        })
        .expect_err("an internal ordering checkpoint cancels the sort");
        assert_eq!(
            error,
            SearchError::Cancelled(CancellationReason::ClientRequest)
        );
        assert_eq!(checks.get(), 3);

        let mut completed = (0_u32..4_096).rev().collect::<Vec<_>>();
        sort_with_checkpoints(&mut completed, Ord::cmp, || Ok(()))
            .expect("bounded ordering completes");
        assert!(completed.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn build_checks_cancellation_after_commit() {
        let fields = Fields::new();
        let index = Index::create_in_ram(fields.schema.clone());
        register_tokenizer(&index);
        let documents = vec![document(1, "item", "src/lib.rs")];
        let checkpoints = std::cell::RefCell::new(Vec::new());

        let error = populate_index_with_checkpoints(
            &index,
            &fields,
            generation(31),
            &documents,
            1,
            BuildBudget::default(),
            |checkpoint| {
                checkpoints.borrow_mut().push(checkpoint);
                if checkpoint == BuildCheckpoint::AfterCommit {
                    Err(SearchError::Cancelled(CancellationReason::ClientRequest))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("post-commit cancellation is surfaced");

        assert_eq!(
            error,
            SearchError::Cancelled(CancellationReason::ClientRequest)
        );
        assert_eq!(
            checkpoints.into_inner(),
            [
                BuildCheckpoint::BeforeWriter,
                BuildCheckpoint::BeforeDocument,
                BuildCheckpoint::BeforeCommit,
                BuildCheckpoint::AfterCommit,
            ]
        );
    }

    #[test]
    fn ephemeral_reader_checks_cancellation_after_open() {
        let fields = Fields::new();
        let index = Index::create_in_ram(fields.schema.clone());
        register_tokenizer(&index);
        let documents = vec![document(1, "item", "src/lib.rs")];
        populate_index(
            &index,
            &fields,
            generation(32),
            &documents,
            1,
            BuildBudget::default(),
            &Cancellation::new(),
        )
        .expect("fixture index commits");
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let checked_after_open = Cell::new(false);

        let error = match open_reader_checked(&index, &fields, generation(32), Some(1), || {
            checked_after_open.set(true);
            cancellation.check().map_err(SearchError::from)
        }) {
            Ok(_) => panic!("post-open cancellation must be surfaced"),
            Err(error) => error,
        };

        assert!(checked_after_open.get());
        assert_eq!(
            error,
            SearchError::Cancelled(CancellationReason::ClientRequest)
        );
    }

    #[test]
    fn exact_and_prefix_classes_dominate_bm25_scores() {
        let exact = document(1, "query_budget", "src/query.rs");
        let mut bm25_heavy = document(2, "unrelated", "src/other.rs");
        bm25_heavy.documentation = Some("query budget ".repeat(128));
        let (_directory, _manifest, index) = build(vec![bm25_heavy, exact]);

        assert_eq!(
            search(&index, "query_budget", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
        assert_eq!(
            search(&index, "query", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
    }

    #[test]
    fn canonical_unicode_forms_share_exact_and_tokenized_identity() {
        let (_directory, _manifest, index) = build(vec![
            document(1, "Cafe\u{301}Value", "src/unicode.rs"),
            document(2, "Straße", "src/german.rs"),
            document(3, "Σίσυφος", "src/greek.rs"),
        ]);

        assert_eq!(
            search(&index, "CaféValue", SearchMode::Exact)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
        assert_eq!(
            search(&index, "café value", SearchMode::Text)[0].symbol_id,
            SymbolId::from_bytes([1; 20])
        );
        assert_eq!(
            search(&index, "STRASSE", SearchMode::Exact)[0].symbol_id,
            SymbolId::from_bytes([2; 20])
        );
        assert_eq!(
            search(&index, "σίσυφοσ", SearchMode::Exact)[0].symbol_id,
            SymbolId::from_bytes([3; 20])
        );
    }

    #[test]
    fn opening_requires_exact_generation_alignment() {
        let (directory, manifest, index) = build(vec![document(1, "item", "src/lib.rs")]);
        drop(index);

        let artifact = VerifiedLexicalArtifact::verify(
            directory.path(),
            manifest,
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("artifact verifies");
        assert_eq!(
            LexicalIndex::open(artifact, generation(8))
                .err()
                .expect("wrong generation fails"),
            SearchError::GenerationMismatch {
                expected: generation(8),
                actual: generation(7),
            }
        );
    }

    #[test]
    fn opening_rejects_segments_with_deleted_documents() {
        let (directory, original_manifest, index) = build(vec![
            document(1, "first", "src/first.rs"),
            document(2, "second", "src/second.rs"),
        ]);
        drop(index);
        let fields = Fields::new();
        let raw_index = Index::open_in_dir(directory.path()).expect("raw index");
        register_tokenizer(&raw_index);
        let mut writer: tantivy::IndexWriter<TantivyDocument> = raw_index
            .writer_with_num_threads(1, BuildBudget::default().indexer_memory_bytes)
            .expect("writer");
        writer.set_merge_policy(Box::new(NoMergePolicy));
        writer.delete_term(Term::from_field_bytes(
            fields.symbol_id,
            SymbolId::from_bytes([1; 20]).as_bytes(),
        ));
        let mut prepared = writer.prepare_commit().expect("prepare delete commit");
        prepared.set_payload(&format_payload(generation(7), 1));
        prepared.commit().expect("delete commit");
        drop(writer);
        drop(raw_index);

        let replacement_manifest = create_manifest(
            directory.path(),
            BuildStats {
                documents: 1,
                ..original_manifest.stats()
            },
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("replacement manifest");
        let artifact = VerifiedLexicalArtifact::verify(
            directory.path(),
            replacement_manifest,
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("artifact verifies");
        assert_eq!(
            LexicalIndex::open(artifact, generation(7))
                .err()
                .expect("deleted segment is rejected"),
            SearchError::IncompatibleIndex
        );
    }

    #[test]
    fn empty_index_is_openable_and_measurable() {
        let directory = TempDir::new().expect("temp directory");
        let manifest = LexicalIndexBuilder::build(
            directory.path(),
            generation(3),
            Vec::new(),
            BuildBudget::default(),
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("empty index builds");
        let stats = manifest.stats();
        let index = open_index(directory.path(), manifest.clone(), generation(3));

        assert_eq!(stats.documents, 0);
        assert_eq!(index.document_count(), 0);
        assert!(manifest.total_bytes() > 0);
    }

    #[test]
    fn lexical_component_size_measurement_matches_directory_bytes() {
        const DOCUMENTS: usize = 64;
        let mut lexical = Vec::with_capacity(DOCUMENTS);
        for index in 0..DOCUMENTS {
            let identifier = format!("handle_request_{index:04}");
            lexical.push(document(
                u8::try_from(index).expect("fixture byte"),
                &identifier,
                &format!("src/generated/module_{index:04}.rs"),
            ));
        }
        let directory = TempDir::new().expect("temp directory");
        let manifest = LexicalIndexBuilder::build(
            directory.path(),
            generation(4),
            lexical,
            BuildBudget::default(),
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("fixture index builds");
        let stats = manifest.stats();
        let index_bytes = manifest.total_bytes();
        let manual_bytes = fs::read_dir(directory.path())
            .expect("index directory")
            .map(|entry| {
                entry
                    .expect("index entry")
                    .metadata()
                    .expect("index metadata")
                    .len()
            })
            .sum::<u64>();

        assert_eq!(index_bytes, manual_bytes);
        assert!(index_bytes > 0);
        assert!(stats.text_bytes > 0);
    }

    #[test]
    fn build_rejects_duplicates_invalid_paths_and_nonempty_staging() {
        let duplicate = document(1, "one", "src/one.rs");
        let mut duplicate_two = document(1, "two", "src/two.rs");
        duplicate_two.file_id = FileId::from_bytes([9; 20]);
        let directory = TempDir::new().expect("temp directory");
        assert_eq!(
            LexicalIndexBuilder::build(
                directory.path(),
                generation(1),
                vec![duplicate, duplicate_two],
                BuildBudget::default(),
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::DuplicateSymbol)
        );

        let invalid_directory = TempDir::new().expect("temp directory");
        let invalid = document(1, "one", "../secret.rs");
        assert_eq!(
            LexicalIndexBuilder::build(
                invalid_directory.path(),
                generation(1),
                vec![invalid],
                BuildBudget::default(),
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::InvalidDocument {
                field: DocumentField::Path,
            })
        );

        let occupied = TempDir::new().expect("temp directory");
        fs::write(occupied.path().join("foreign"), b"untouched").expect("fixture");
        assert_eq!(
            LexicalIndexBuilder::build(
                occupied.path(),
                generation(1),
                Vec::new(),
                BuildBudget::default(),
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::NonEmptyStaging)
        );
        assert_eq!(
            fs::read(occupied.path().join("foreign")).expect("fixture remains"),
            b"untouched"
        );
    }

    #[test]
    fn cancellation_prevents_build_and_search_work() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        let directory = TempDir::new().expect("temp directory");
        assert_eq!(
            LexicalIndexBuilder::build(
                directory.path(),
                generation(1),
                vec![document(1, "item", "src/lib.rs")],
                BuildBudget::default(),
                ArtifactBudget::default(),
                &cancellation,
            ),
            Err(SearchError::Cancelled(CancellationReason::ClientRequest))
        );
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("directory readable")
                .count(),
            0
        );

        let (_directory, _manifest, index) = build(vec![document(1, "item", "src/lib.rs")]);
        assert_eq!(
            index.search(
                &SearchRequest {
                    query: "item".to_owned(),
                    mode: SearchMode::Exact,
                    max_results: 1,
                },
                SearchBudget::default(),
                &cancellation,
            ),
            Err(SearchError::Cancelled(CancellationReason::ClientRequest))
        );

        let expired = Cancellation::with_deadline(std::time::Instant::now());
        assert_eq!(
            index.search(
                &SearchRequest {
                    query: "item".to_owned(),
                    mode: SearchMode::Exact,
                    max_results: 1,
                },
                SearchBudget::default(),
                &expired,
            ),
            Err(SearchError::Cancelled(CancellationReason::DeadlineExceeded))
        );
    }

    #[test]
    fn stored_documents_contain_exactly_one_bounded_hit_record() {
        let (_directory, _manifest, index) = build(vec![document(1, "item", "src/lib.rs")]);
        let fields = &index.fields;
        let searcher = index.reader.searcher();
        let query = TermQuery::new(
            Term::from_field_text(fields.identifier_normalized, "item"),
            IndexRecordOption::Basic,
        );
        let address = searcher
            .search(
                &query,
                &tantivy::collector::TopDocs::with_limit(1).order_by_score(),
            )
            .expect("search")[0]
            .1;
        let stored = searcher
            .doc::<TantivyDocument>(address)
            .expect("stored document");

        assert_eq!(
            fields
                .schema
                .fields()
                .filter(|(_field, entry)| entry.is_stored())
                .count(),
            1
        );
        let stored_values: Vec<_> = stored.field_values().collect();
        assert_eq!(stored_values.len(), 1);
        assert_eq!(stored_values[0].0, fields.stored_hit);
        assert!(stored_values[0].1.as_bytes().is_some());

        let mut duplicate = TantivyDocument::new();
        let encoded = encode_stored_hit(&document(1, "item", "src/lib.rs")).expect("stored hit");
        duplicate.add_bytes(fields.stored_hit, &encoded);
        duplicate.add_bytes(fields.stored_hit, &encoded);
        assert_eq!(
            fields.decode(duplicate, 1.0),
            Err(SearchError::IncompatibleIndex)
        );
    }

    #[test]
    fn candidate_and_result_budgets_fail_closed() {
        let (_directory, _manifest, index) = build(vec![
            document(1, "same_item", "src/a.rs"),
            document(2, "same_item", "src/b.rs"),
        ]);
        let request = SearchRequest {
            query: "same item".to_owned(),
            mode: SearchMode::Text,
            max_results: 2,
        };
        assert_eq!(
            index.search(
                &request,
                SearchBudget {
                    max_candidates: 1,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::CandidateBudgetExceeded)
        );
        assert_eq!(
            index.search(
                &request,
                SearchBudget {
                    max_results: 1,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::InvalidResultLimit)
        );
        assert_eq!(
            index.search(
                &request,
                SearchBudget {
                    max_candidates: HARD_MAX_QUERY_CANDIDATES + 1,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::InvalidQueryBudget {
                resource: "candidates",
            })
        );
        assert_eq!(
            index.search(
                &request,
                SearchBudget {
                    max_returned_text_bytes: 1,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::ReturnedTextBudgetExceeded)
        );
    }

    #[test]
    fn expansion_posting_and_time_budgets_fail_before_unbounded_work() {
        let (_directory, _manifest, index) = build(vec![
            document(1, "alpha_one", "src/a.rs"),
            document(2, "alpha_two", "src/b.rs"),
            document(3, "alpha_one", "src/c.rs"),
        ]);
        let prefix = SearchRequest {
            query: "alpha".to_owned(),
            mode: SearchMode::Prefix,
            max_results: 2,
        };
        assert_eq!(
            index.search(
                &prefix,
                SearchBudget {
                    max_expanded_terms: 1,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::TermExpansionBudgetExceeded)
        );

        let exact = SearchRequest {
            query: "alpha_one".to_owned(),
            mode: SearchMode::Exact,
            max_results: 1,
        };
        assert_eq!(
            index.search(
                &exact,
                SearchBudget {
                    max_postings: 1,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::PostingBudgetExceeded)
        );
        assert_eq!(
            index.search(
                &exact,
                SearchBudget {
                    max_duration: Duration::from_nanos(1),
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::Cancelled(CancellationReason::DeadlineExceeded))
        );

        let (_directory, _manifest, no_match_index) = build(vec![
            document(11, "abaa", "src/aa.rs"),
            document(12, "abab", "src/ab.rs"),
            document(13, "abac", "src/ac.rs"),
        ]);
        assert_eq!(
            no_match_index.search(
                &SearchRequest {
                    query: "ab.z".to_owned(),
                    mode: SearchMode::SafeRegex,
                    max_results: 1,
                },
                SearchBudget {
                    max_examined_terms: 2,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::TermExaminationBudgetExceeded)
        );
    }

    #[test]
    fn term_examination_budget_accepts_exact_cap_and_rejects_additional_term() {
        let (_directory, _manifest, index) = build(vec![
            document(11, "abaa", "src/aa.rs"),
            document(12, "abab", "src/ab.rs"),
            document(13, "abac", "src/ac.rs"),
        ]);
        let request = SearchRequest {
            query: "ab.z".to_owned(),
            mode: SearchMode::SafeRegex,
            max_results: 1,
        };

        assert_eq!(
            index.search(
                &request,
                SearchBudget {
                    max_examined_terms: 3,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Ok(Vec::new())
        );
        assert_eq!(
            index.search(
                &request,
                SearchBudget {
                    max_examined_terms: 2,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::TermExaminationBudgetExceeded)
        );
    }

    #[test]
    fn query_and_build_limits_are_enforced_before_indexing() {
        let directory = TempDir::new().expect("temp directory");
        assert_eq!(
            LexicalIndexBuilder::build(
                directory.path(),
                generation(1),
                vec![document(1, "item", "src/lib.rs")],
                BuildBudget {
                    max_documents: 0,
                    ..BuildBudget::default()
                },
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::BuildBudgetExceeded {
                resource: "documents",
            })
        );
        let (_directory, _manifest, index) = build(vec![document(1, "item", "src/lib.rs")]);
        assert_eq!(
            index.search(
                &SearchRequest {
                    query: " ".to_owned(),
                    mode: SearchMode::Text,
                    max_results: 1,
                },
                SearchBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::InvalidQuery(QueryViolation::Empty))
        );
        assert_eq!(
            index.search(
                &SearchRequest {
                    query: "item\nother".to_owned(),
                    mode: SearchMode::Text,
                    max_results: 1,
                },
                SearchBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::InvalidQuery(
                QueryViolation::UnsupportedCharacter
            ))
        );
        assert_eq!(
            index.search(
                &SearchRequest {
                    query: "one two three".to_owned(),
                    mode: SearchMode::Text,
                    max_results: 1,
                },
                SearchBudget {
                    max_terms: 2,
                    ..SearchBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::InvalidQueryBudget {
                resource: "query_terms",
            })
        );
    }

    #[test]
    fn payload_parser_rejects_extensions_and_malformed_counts() {
        assert_eq!(
            parse_payload("rootlight.lexical;version=1;generation=nope;documents=1"),
            Err(SearchError::IncompatibleIndex)
        );
        assert_eq!(
            parse_payload(&format!(
                "rootlight.lexical;version=2;generation={};documents=1;extra=x",
                generation(1)
            )),
            Err(SearchError::IncompatibleIndex)
        );
    }
}
