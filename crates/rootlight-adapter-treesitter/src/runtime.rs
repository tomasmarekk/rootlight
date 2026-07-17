//! Bounded Tree-sitter parsing, pooling, incremental reuse, and diagnostics.
//!
//! Cooperative cancellation runs inside Tree-sitter's progress callback.
//! Native allocation remains an explicit M05 fallback until M13 isolation.

use std::{
    collections::VecDeque,
    ops::ControlFlow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DiagnosticCode, EncodingId,
    MemoryAdmissionPolicy, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport,
    ParseRequest, RequestError, ResourceKind, ResourceUsage, StreamEnd, SyntaxFactBatch,
    SyntaxFactSink, WorkReport, execute_parse_transaction,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_ir::{AnalysisTier, CoverageStatus, DiagnosticSeverity, SourceRef, SourceSpan};
use tree_sitter::{InputEdit, Node, ParseOptions, Point, Range, Tree};

use crate::{
    GrammarFamily, GrammarRegistry, ParserSettings, RuntimeConfig, RuntimeConfigError,
    incremental::{
        ParseIdentity, ParseReuseKey, ParseWithPrevious, PreviousParse, ReuseInvalidation,
        ReuseStatus, SourceEdit, SourceEditIdentity,
    },
    pool::{ParserPool, PoolError},
    query_pack::QueryPackRegistry,
    registry::language_for,
};

const LOGICAL_TREE_NODE_BYTES: usize = 64;
const CANCELLATION_CHECK_INTERVAL: usize = 256;
const CANCELLATION_BYTE_INTERVAL: usize = 64 * 1024;
static NEXT_PROVIDER_ID: AtomicU64 = AtomicU64::new(1);

/// Bounded first-party Tree-sitter parser provider.
pub struct TreeSitterProvider {
    provider_id: u64,
    registry: GrammarRegistry,
    query_packs: QueryPackRegistry,
    capabilities: ParseCapabilities,
    config: RuntimeConfig,
    pool: ParserPool,
    cache: Mutex<ParseCache>,
}

impl TreeSitterProvider {
    /// Creates a provider with explicit parser, pool, and cache capacities.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError`] if built-in registry/capability metadata
    /// fails validation or process-local provider identities are exhausted.
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeConfigError> {
        let provider_id = NEXT_PROVIDER_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| RuntimeConfigError::ProviderIdentityExhausted)?;
        let registry = GrammarRegistry::audited()?;
        let query_packs = QueryPackRegistry::audited()
            .map_err(|family| RuntimeConfigError::InvalidBuiltInQueryPack { family })?;
        for descriptor in registry.descriptors() {
            if query_packs.get(descriptor.family()).is_none() {
                return Err(RuntimeConfigError::InvalidBuiltInQueryPack {
                    family: descriptor.family(),
                });
            }
        }
        let languages = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.language().clone())
            .collect();
        let capabilities = ParseCapabilities::new(
            languages,
            vec![EncodingId::new("utf-8")?],
            config.max_source_bytes(),
            config.max_syntax_nodes(),
            config.max_syntax_depth(),
            config.max_included_ranges(),
            true,
            true,
            true,
            config.max_concurrent_parses(),
            MemoryEnforcement::Unavailable,
        )?;
        Ok(Self {
            provider_id,
            registry,
            query_packs,
            capabilities,
            pool: ParserPool::new(config.max_concurrent_parses()),
            cache: Mutex::new(ParseCache::new(config.max_cache_bytes())),
            config,
        })
    }

    /// Executes an admitted incremental parse and commits bounded output.
    ///
    /// Invalid or stale reuse input falls back to a clean parse and returns an
    /// explicit [`ReuseInvalidation`]. The caller explicitly selects memory
    /// admission, and the SDK enforces the same capability, deadline, report,
    /// and transactional sink checks as a clean [`rootlight_adapter_sdk::execute_parse`].
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for rejected admission, unsupported input,
    /// cancellation, parser-pool failure, native parser failure, or
    /// sink/resource rejection.
    pub fn execute_with_previous(
        &self,
        request: &ParseRequest<'_>,
        previous: Option<&PreviousParse>,
        edits: &[SourceEdit],
        settings: ParserSettings,
        memory_policy: MemoryAdmissionPolicy,
        cancellation: &Cancellation,
    ) -> Result<ParseWithPrevious, AdapterError> {
        let (output, continuation) = execute_parse_transaction(
            &self.capabilities,
            request,
            memory_policy,
            cancellation,
            |sink, cancellation| {
                self.parse_with_previous_raw(request, previous, edits, settings, sink, cancellation)
                    .map(RawParseWithPrevious::into_transaction_parts)
            },
        )?;
        let previous = match continuation.pending {
            Some(pending) => self.cache_insert(
                pending.identity,
                request.source().bytes(),
                &pending.tree,
                pending.nodes,
                cancellation,
            )?,
            None => None,
        };
        Ok(ParseWithPrevious {
            output,
            previous,
            reuse_status: continuation.reuse_status,
            reuse_key: continuation.reuse_key,
        })
    }

    /// Performs provider work inside an SDK-owned admitted transaction.
    #[allow(clippy::too_many_lines)]
    fn parse_with_previous_raw(
        &self,
        request: &ParseRequest<'_>,
        previous: Option<&PreviousParse>,
        edits: &[SourceEdit],
        settings: ParserSettings,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<RawParseWithPrevious, AdapterError> {
        cancellation.check()?;
        if settings.input_chunk_bytes() > self.config.max_source_bytes() {
            return Err(provider_failure("treesitter-settings"));
        }
        validate_edit_admission(previous, edits, &self.config, cancellation)?;
        let family = self
            .registry
            .family_for_language(request.language())
            .ok_or(RequestError::UnsupportedLanguage)?;
        if request.encoding().as_str() != "utf-8" {
            return Err(RequestError::UnsupportedEncoding.into());
        }
        let source_bytes = request.source().bytes();
        require_provider_limit(
            ResourceKind::SourceBytes,
            source_bytes.len(),
            self.config.max_source_bytes(),
        )?;
        require_provider_limit(
            ResourceKind::SyntaxNodes,
            request.limits().max_syntax_nodes(),
            self.config.max_syntax_nodes(),
        )?;
        require_provider_limit(
            ResourceKind::SyntaxDepth,
            request.limits().max_syntax_depth(),
            self.config.max_syntax_depth(),
        )?;
        require_provider_limit(
            ResourceKind::IncludedRanges,
            request.included_ranges().len(),
            self.config.max_included_ranges(),
        )?;
        let source_text =
            std::str::from_utf8(source_bytes).map_err(|_| provider_failure("invalid-utf8"))?;
        if request
            .included_ranges()
            .iter()
            .any(|range| range.language() != request.language())
        {
            return Err(provider_failure("included-language"));
        }
        let descriptor = self
            .registry
            .get(family)
            .ok_or_else(|| provider_failure("grammar-missing"))?;
        let identity = ParseIdentity {
            content_hash: request.source().source_ref().content_hash(),
            family,
            grammar_version: descriptor.grammar_version(),
            encoding: request.encoding().as_str().to_owned(),
            included_ranges: range_identities(request),
            settings,
        };
        let cached = self.resolve_previous(previous, cancellation)?;
        let previous_hash = cached
            .as_ref()
            .filter(|entry| entry.invalidation.is_none())
            .map(|entry| entry.identity.content_hash);
        let reuse_key = ParseReuseKey {
            previous_content_hash: previous_hash,
            current_content_hash: identity.content_hash,
            family,
            grammar_version: identity.grammar_version,
            encoding: identity.encoding.clone(),
            included_ranges: identity.included_ranges.clone(),
            settings,
            edits: source_edit_identities(edits, cancellation)?,
        };
        let (old_tree, mut reuse_status) = prepare_reuse(
            cached.as_ref(),
            &identity,
            source_bytes,
            edits,
            self.config.max_source_bytes(),
            cancellation,
        )?;

        let mut lease = self.pool.acquire(cancellation).map_err(map_pool_error)?;
        let parser = lease.parser_mut().map_err(map_pool_error)?;
        parser
            .set_language(&language_for(family))
            .map_err(|_| provider_failure("grammar-abi"))?;
        let included_ranges = tree_sitter_ranges(request, source_text, cancellation)?;
        parser
            .set_included_ranges(&included_ranges)
            .map_err(|_| provider_failure("included-ranges"))?;

        let mut callback_cancelled = false;
        let mut progress = |_: &tree_sitter::ParseState| match cancellation.check() {
            Ok(()) => ControlFlow::Continue(()),
            Err(_) => {
                callback_cancelled = true;
                ControlFlow::Break(())
            }
        };
        let chunk_bytes = settings.input_chunk_bytes();
        let mut input = |offset: usize, _point: Point| {
            let end = offset.saturating_add(chunk_bytes).min(source_bytes.len());
            source_bytes.get(offset..end).unwrap_or_default()
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        let tree = parser.parse_with_options(&mut input, old_tree.as_ref(), Some(options));
        if callback_cancelled {
            parser.reset();
            cancellation.check()?;
            return Err(provider_failure("parse-cancelled"));
        }
        let tree = tree.ok_or_else(|| provider_failure("parse-aborted"))?;
        cancellation.check()?;

        if matches!(reuse_status, ReuseStatus::Reused { .. }) {
            let changed_ranges = old_tree.as_ref().map_or(Ok(0), |old| {
                count_changed_ranges(old, &tree, self.config.max_syntax_nodes(), cancellation)
            })?;
            reuse_status = ReuseStatus::Reused { changed_ranges };
        }
        let traversal = inspect_tree(
            &tree,
            request,
            request.limits().max_syntax_nodes(),
            request.limits().max_syntax_depth(),
            cancellation,
        )?;
        cancellation.check()?;
        emit_primary_diagnostic(&traversal, request, sink, cancellation)?;
        let usage = sink.staged_usage();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            traversal.coverage,
            source_bytes.len(),
            traversal.covered_source_bytes,
            traversal.skipped_regions,
            Vec::new(),
        )
        .map_err(AdapterError::InvalidReport)?;
        let resources = ResourceUsage::new(
            source_bytes.len(),
            usage.records(),
            traversal.processed_nodes,
            traversal.max_depth,
            None,
            usage,
        );
        let report = WorkReport::new(
            coverage,
            resources,
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::InvalidReport)?;
        let pending = if traversal.fully_traversed {
            Some(PendingParse {
                identity,
                tree,
                nodes: traversal.processed_nodes,
            })
        } else {
            None
        };
        Ok(RawParseWithPrevious {
            report,
            pending,
            reuse_status,
            reuse_key,
        })
    }

    /// Returns source-free pool and cache accounting.
    #[must_use]
    pub fn stats(&self) -> RuntimeStats {
        let pool = self.pool.stats();
        let cache = match self.cache.lock() {
            Ok(cache) => cache.stats(),
            Err(poisoned) => poisoned.into_inner().stats(),
        };
        RuntimeStats {
            pooled_parsers: pool.created,
            available_parsers: pool.available,
            checked_out_parsers: pool.checked_out,
            cache,
        }
    }

    fn resolve_previous(
        &self,
        previous: Option<&PreviousParse>,
        cancellation: &Cancellation,
    ) -> Result<Option<CachedParse>, AdapterError> {
        cancellation.check()?;
        let Some(previous) = previous else {
            return Ok(None);
        };
        if previous.provider_id != self.provider_id {
            return Ok(Some(CachedParse::invalidation(ReuseInvalidation::Provider)));
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| provider_failure("cache-state"))?;
        let resolved = cache
            .get(previous.entry_id)
            .or_else(|| Some(CachedParse::invalidation(ReuseInvalidation::Evicted)));
        cancellation.check()?;
        Ok(resolved)
    }

    fn cache_insert(
        &self,
        identity: ParseIdentity,
        source: &[u8],
        tree: &Tree,
        nodes: usize,
        cancellation: &Cancellation,
    ) -> Result<Option<PreviousParse>, AdapterError> {
        cancellation.check()?;
        let accounted_bytes = nodes
            .checked_mul(LOGICAL_TREE_NODE_BYTES)
            .and_then(|tree_bytes| source.len().checked_add(tree_bytes))
            .ok_or_else(|| provider_failure("cache-accounting"))?;
        if accounted_bytes > self.config.max_cache_bytes() {
            return Ok(None);
        }
        let source = copy_source_for_cache(source, cancellation)?;
        cancellation.check()?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| provider_failure("cache-state"))?;
        cancellation.check()?;
        cache
            .insert(self.provider_id, identity, source, tree, nodes)
            .map_err(|_| provider_failure("cache-accounting"))
    }
}

struct RawParseWithPrevious {
    report: ParseReport,
    pending: Option<PendingParse>,
    reuse_status: ReuseStatus,
    reuse_key: ParseReuseKey,
}

impl RawParseWithPrevious {
    fn into_transaction_parts(self) -> (ParseReport, RawContinuation) {
        (
            self.report,
            RawContinuation {
                pending: self.pending,
                reuse_status: self.reuse_status,
                reuse_key: self.reuse_key,
            },
        )
    }
}

struct RawContinuation {
    pending: Option<PendingParse>,
    reuse_status: ReuseStatus,
    reuse_key: ParseReuseKey,
}

struct PendingParse {
    identity: ParseIdentity,
    tree: Tree,
    nodes: usize,
}

impl ParseProvider for TreeSitterProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        self.parse_with_previous_raw(
            request,
            None,
            &[],
            self.config.default_settings(),
            sink,
            cancellation,
        )
        .map(|output| output.report)
    }
}

impl std::fmt::Debug for TreeSitterProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TreeSitterProvider")
            .field("provider_id", &self.provider_id)
            .field("capabilities", &self.capabilities)
            .field("config", &self.config)
            .field("query_pack_count", &self.query_packs.len())
            .field("query_pattern_count", &self.query_packs.pattern_count())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// Source-free retained-cache counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    /// Retained previous parse count.
    pub entries: usize,
    /// Deterministically accounted retained bytes.
    pub accounted_bytes: usize,
    /// Configured retained byte ceiling.
    pub capacity_bytes: usize,
}

/// Source-free parser pool and cache counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeStats {
    /// Parsers created up to the fixed permit ceiling.
    pub pooled_parsers: usize,
    /// Parsers currently available.
    pub available_parsers: usize,
    /// Parsers currently leased.
    pub checked_out_parsers: usize,
    /// Bounded incremental cache accounting.
    pub cache: CacheStats,
}

#[derive(Debug)]
struct ParseCache {
    capacity_bytes: usize,
    accounted_bytes: usize,
    next_entry_id: u64,
    entries: VecDeque<CachedParse>,
}

impl ParseCache {
    fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            accounted_bytes: 0,
            next_entry_id: 1,
            entries: VecDeque::new(),
        }
    }

    fn get(&mut self, entry_id: u64) -> Option<CachedParse> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.entry_id == entry_id)?;
        let entry = self.entries.remove(index)?;
        let result = entry.clone_for_reuse();
        self.entries.push_back(entry);
        Some(result)
    }

    fn insert(
        &mut self,
        provider_id: u64,
        identity: ParseIdentity,
        source: Arc<[u8]>,
        tree: &Tree,
        nodes: usize,
    ) -> Result<Option<PreviousParse>, ()> {
        let tree_bytes = nodes.checked_mul(LOGICAL_TREE_NODE_BYTES).ok_or(())?;
        let accounted_bytes = source.len().checked_add(tree_bytes).ok_or(())?;
        if accounted_bytes > self.capacity_bytes {
            return Ok(None);
        }
        while self
            .accounted_bytes
            .checked_add(accounted_bytes)
            .ok_or(())?
            > self.capacity_bytes
        {
            let Some(evicted) = self.entries.pop_front() else {
                return Err(());
            };
            self.accounted_bytes = self.accounted_bytes.saturating_sub(evicted.accounted_bytes);
        }
        let entry_id = self.next_entry_id;
        self.next_entry_id = self.next_entry_id.checked_add(1).ok_or(())?;
        self.entries.push_back(CachedParse {
            entry_id,
            identity,
            source,
            tree: Some(tree.clone()),
            accounted_bytes,
            invalidation: None,
        });
        self.accounted_bytes = self
            .accounted_bytes
            .checked_add(accounted_bytes)
            .ok_or(())?;
        Ok(Some(PreviousParse {
            provider_id,
            entry_id,
        }))
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            accounted_bytes: self.accounted_bytes,
            capacity_bytes: self.capacity_bytes,
        }
    }
}

#[derive(Debug)]
struct CachedParse {
    entry_id: u64,
    identity: ParseIdentity,
    source: Arc<[u8]>,
    tree: Option<Tree>,
    accounted_bytes: usize,
    invalidation: Option<ReuseInvalidation>,
}

impl CachedParse {
    fn invalidation(invalidation: ReuseInvalidation) -> Self {
        Self {
            entry_id: 0,
            identity: invalid_identity(),
            source: Arc::from([]),
            tree: None,
            accounted_bytes: 0,
            invalidation: Some(invalidation),
        }
    }

    fn clone_for_reuse(&self) -> Self {
        Self {
            entry_id: self.entry_id,
            identity: self.identity.clone(),
            source: self.source.clone(),
            tree: self.tree.clone(),
            accounted_bytes: self.accounted_bytes,
            invalidation: self.invalidation,
        }
    }
}

fn invalid_identity() -> ParseIdentity {
    ParseIdentity {
        content_hash: ContentHash::from_bytes([0; 32]),
        family: GrammarFamily::Rust,
        grammar_version: "",
        encoding: String::new(),
        included_ranges: Vec::new(),
        settings: ParserSettings::new(1).expect("hard-coded parser setting is nonzero"),
    }
}

fn validate_edit_admission(
    previous: Option<&PreviousParse>,
    edits: &[SourceEdit],
    config: &RuntimeConfig,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    if edits.is_empty() {
        return Ok(());
    }
    if previous.is_none() {
        return Err(provider_failure("incremental-edit-without-previous"));
    }
    if edits.len() > config.max_incremental_edits() {
        return Err(provider_failure("incremental-edit-limit"));
    }
    let mut replacement_bytes = 0usize;
    for (index, edit) in edits.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        replacement_bytes = replacement_bytes
            .checked_add(edit.replacement_bytes())
            .ok_or_else(|| provider_failure("incremental-replacement-limit"))?;
        if replacement_bytes > config.max_source_bytes() {
            return Err(provider_failure("incremental-replacement-limit"));
        }
    }
    cancellation.check()?;
    Ok(())
}

fn source_edit_identities(
    edits: &[SourceEdit],
    cancellation: &Cancellation,
) -> Result<Vec<SourceEditIdentity>, AdapterError> {
    let mut identities = Vec::new();
    identities
        .try_reserve_exact(edits.len())
        .map_err(|_| provider_failure("incremental-identity-allocation"))?;
    for edit in edits {
        cancellation.check()?;
        let mut hasher = blake3::Hasher::new();
        for chunk in edit.replacement().chunks(CANCELLATION_BYTE_INTERVAL) {
            cancellation.check()?;
            hasher.update(chunk);
        }
        let replacement_hash = ContentHash::from_bytes(*hasher.finalize().as_bytes());
        identities.push(SourceEditIdentity::from_edit(edit, replacement_hash));
    }
    cancellation.check()?;
    Ok(identities)
}

fn prepare_reuse(
    cached: Option<&CachedParse>,
    current: &ParseIdentity,
    current_source: &[u8],
    edits: &[SourceEdit],
    max_source_bytes: usize,
    cancellation: &Cancellation,
) -> Result<(Option<Tree>, ReuseStatus), AdapterError> {
    let Some(cached) = cached else {
        return Ok((None, ReuseStatus::Fresh));
    };
    if let Some(reason) = cached.invalidation {
        return Ok((None, ReuseStatus::Invalidated(reason)));
    }
    if cached.identity.family != current.family {
        return Ok((None, ReuseStatus::Invalidated(ReuseInvalidation::Language)));
    }
    if cached.identity.grammar_version != current.grammar_version {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::GrammarVersion),
        ));
    }
    if cached.identity.encoding != current.encoding {
        return Ok((None, ReuseStatus::Invalidated(ReuseInvalidation::Encoding)));
    }
    if cached.identity.included_ranges != current.included_ranges {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::IncludedRanges),
        ));
    }
    if cached.identity.settings != current.settings {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::ParserSettings),
        ));
    }
    if cached.identity.content_hash != current.content_hash && edits.is_empty() {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::MissingEdits),
        ));
    }
    let Some(tree) = cached.tree.clone() else {
        return Ok((None, ReuseStatus::Invalidated(ReuseInvalidation::Evicted)));
    };
    match apply_edits(
        tree,
        &cached.source,
        current_source,
        edits,
        max_source_bytes,
        cancellation,
    ) {
        Ok(tree) => Ok((Some(tree), ReuseStatus::Reused { changed_ranges: 0 })),
        Err(ApplyEditError::Invalidation(reason)) => Ok((None, ReuseStatus::Invalidated(reason))),
        Err(ApplyEditError::Fatal(error)) => Err(error),
    }
}

fn apply_edits(
    mut tree: Tree,
    old_source: &[u8],
    new_source: &[u8],
    edits: &[SourceEdit],
    max_source_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Tree, ApplyEditError> {
    let mut source = old_source.to_vec();
    for (index, edit) in edits.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation
                .check()
                .map_err(AdapterError::from)
                .map_err(ApplyEditError::Fatal)?;
        }
        if edit.start_byte() > edit.old_end_byte() || edit.old_end_byte() > source.len() {
            return Err(ReuseInvalidation::EditOutsideSource.into());
        }
        if !source.is_char_boundary(edit.start_byte())
            || !source.is_char_boundary(edit.old_end_byte())
        {
            return Err(ReuseInvalidation::EditNotCharacterBoundary.into());
        }
        let start_position = point_for_offset(&source, edit.start_byte())
            .ok_or(ReuseInvalidation::EditOutsideSource)?;
        let old_end_position = point_for_offset(&source, edit.old_end_byte())
            .ok_or(ReuseInvalidation::EditOutsideSource)?;
        let replacement_end = point_after_replacement(start_position, edit.replacement());
        let new_end_byte = edit
            .start_byte()
            .checked_add(edit.replacement().len())
            .ok_or(ReuseInvalidation::AccountingOverflow)?;
        let removed_bytes = edit.old_end_byte().saturating_sub(edit.start_byte());
        let intermediate_bytes = source
            .len()
            .checked_sub(removed_bytes)
            .and_then(|length| length.checked_add(edit.replacement_bytes()))
            .ok_or(ReuseInvalidation::AccountingOverflow)?;
        if intermediate_bytes > max_source_bytes {
            return Err(ApplyEditError::Fatal(provider_failure(
                "incremental-source-limit",
            )));
        }
        tree.edit(&InputEdit {
            start_byte: edit.start_byte(),
            old_end_byte: edit.old_end_byte(),
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position: replacement_end,
        });
        source.splice(
            edit.start_byte()..edit.old_end_byte(),
            edit.replacement().iter().copied(),
        );
    }
    cancellation
        .check()
        .map_err(AdapterError::from)
        .map_err(ApplyEditError::Fatal)?;
    if source == new_source {
        Ok(tree)
    } else {
        Err(ReuseInvalidation::EditResultMismatch.into())
    }
}

enum ApplyEditError {
    Invalidation(ReuseInvalidation),
    Fatal(AdapterError),
}

impl From<ReuseInvalidation> for ApplyEditError {
    fn from(reason: ReuseInvalidation) -> Self {
        Self::Invalidation(reason)
    }
}

fn point_after_replacement(start: Point, replacement: &[u8]) -> Point {
    let mut row = start.row;
    let mut column = start.column;
    for byte in replacement {
        if *byte == b'\n' {
            row = row.saturating_add(1);
            column = 0;
        } else {
            column = column.saturating_add(1);
        }
    }
    Point { row, column }
}

fn point_for_offset(source: &[u8], offset: usize) -> Option<Point> {
    let prefix = source.get(..offset)?;
    let mut row = 0usize;
    let mut column = 0usize;
    for byte in prefix {
        if *byte == b'\n' {
            row = row.checked_add(1)?;
            column = 0;
        } else {
            column = column.checked_add(1)?;
        }
    }
    Some(Point { row, column })
}

fn count_changed_ranges(
    old_tree: &Tree,
    new_tree: &Tree,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut count = 0usize;
    for range in old_tree.changed_ranges(new_tree) {
        if count.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let _ = range;
        count = count
            .checked_add(1)
            .ok_or_else(|| provider_failure("changed-range-limit"))?;
        if count > maximum {
            return Err(provider_failure("changed-range-limit"));
        }
    }
    cancellation.check()?;
    Ok(count)
}

fn copy_source_for_cache(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Arc<[u8]>, AdapterError> {
    cancellation.check()?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(source.len())
        .map_err(|_| provider_failure("cache-allocation"))?;
    for chunk in source.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        copy.extend_from_slice(chunk);
    }
    cancellation.check()?;
    Ok(Arc::from(copy))
}

fn tree_sitter_ranges(
    request: &ParseRequest<'_>,
    source: &str,
    cancellation: &Cancellation,
) -> Result<Vec<Range>, AdapterError> {
    let mut ranges = Vec::with_capacity(request.included_ranges().len());
    let mut cursor = 0usize;
    let mut point = Point { row: 0, column: 0 };
    for included in request.included_ranges() {
        cancellation.check()?;
        let span = included.span();
        let start =
            usize::try_from(span.start_byte()).map_err(|_| provider_failure("range-offset"))?;
        let end = usize::try_from(span.end_byte()).map_err(|_| provider_failure("range-offset"))?;
        if start < cursor || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(provider_failure("range-boundary"));
        }
        advance_source_point(
            source.as_bytes(),
            &mut cursor,
            start,
            &mut point,
            cancellation,
        )?;
        let start_point = point;
        advance_source_point(
            source.as_bytes(),
            &mut cursor,
            end,
            &mut point,
            cancellation,
        )?;
        ranges.push(Range {
            start_byte: start,
            end_byte: end,
            start_point,
            end_point: point,
        });
    }
    cancellation.check()?;
    Ok(ranges)
}

fn advance_source_point(
    source: &[u8],
    cursor: &mut usize,
    target: usize,
    point: &mut Point,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let bytes = source
        .get(*cursor..target)
        .ok_or_else(|| provider_failure("range-offset"))?;
    for chunk in bytes.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        for byte in chunk {
            if *byte == b'\n' {
                point.row = point
                    .row
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("range-point"))?;
                point.column = 0;
            } else {
                point.column = point
                    .column
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("range-point"))?;
            }
        }
    }
    *cursor = target;
    Ok(())
}

fn range_identities(request: &ParseRequest<'_>) -> Vec<rootlight_adapter_sdk::IncludedRange> {
    request.included_ranges().to_vec()
}

#[derive(Debug)]
struct TraversalReport {
    processed_nodes: usize,
    max_depth: usize,
    covered_source_bytes: usize,
    skipped_regions: usize,
    coverage: CoverageStatus,
    primary_diagnostic: Option<PrimaryDiagnostic>,
    fully_traversed: bool,
}

#[derive(Debug, Clone, Copy)]
enum PrimaryDiagnostic {
    NodeLimit,
    DepthLimit,
    ErrorRecovery { start: usize, end: usize },
}

#[derive(Debug)]
struct SyntaxTraversal {
    processed_nodes: usize,
    observed_depth: usize,
    error_span: Option<(usize, usize)>,
    limited_nodes: bool,
    limited_depth: bool,
}

fn inspect_tree(
    tree: &Tree,
    request: &ParseRequest<'_>,
    max_nodes: usize,
    max_depth: usize,
    cancellation: &Cancellation,
) -> Result<TraversalReport, AdapterError> {
    let traversal = traverse_syntax(tree, max_nodes, max_depth, cancellation)?;
    cancellation.check()?;
    let SyntaxTraversal {
        processed_nodes,
        observed_depth,
        error_span,
        limited_nodes,
        limited_depth,
    } = traversal;
    let source_len = request.source().bytes().len();
    let requested_covered_bytes = if request.included_ranges().is_empty() {
        source_len
    } else {
        request
            .included_ranges()
            .iter()
            .try_fold(0usize, |total, range| {
                let span = range.span();
                let length = span
                    .end_byte()
                    .checked_sub(span.start_byte())
                    .and_then(|length| usize::try_from(length).ok())
                    .ok_or_else(|| provider_failure("range-accounting"))?;
                total
                    .checked_add(length)
                    .ok_or_else(|| provider_failure("range-accounting"))
            })?
    };
    let (coverage, skipped_regions, primary_diagnostic) = if limited_nodes {
        (
            CoverageStatus::Bounded,
            1,
            Some(PrimaryDiagnostic::NodeLimit),
        )
    } else if limited_depth {
        (
            CoverageStatus::Bounded,
            1,
            Some(PrimaryDiagnostic::DepthLimit),
        )
    } else if let Some((start, end)) = error_span {
        (
            CoverageStatus::Unknown,
            1,
            Some(PrimaryDiagnostic::ErrorRecovery { start, end }),
        )
    } else if requested_covered_bytes < source_len {
        (CoverageStatus::Bounded, 1, None)
    } else {
        (CoverageStatus::Complete, 0, None)
    };
    Ok(TraversalReport {
        processed_nodes,
        max_depth: observed_depth.min(max_depth),
        covered_source_bytes: if limited_nodes || limited_depth {
            0
        } else {
            requested_covered_bytes
        },
        skipped_regions,
        coverage,
        primary_diagnostic,
        fully_traversed: !limited_nodes && !limited_depth,
    })
}

fn traverse_syntax(
    tree: &Tree,
    max_nodes: usize,
    max_depth: usize,
    cancellation: &Cancellation,
) -> Result<SyntaxTraversal, AdapterError> {
    cancellation.check()?;
    let root = tree.root_node();
    let mut stack = vec![(root, 0usize)];
    let mut processed_nodes = 0usize;
    let mut observed_depth = 0usize;
    let mut error_span = None;
    let mut limited_nodes = false;
    let mut limited_depth = false;
    while let Some((node, depth)) = stack.pop() {
        if processed_nodes.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if processed_nodes >= max_nodes {
            limited_nodes = true;
            break;
        }
        processed_nodes = processed_nodes
            .checked_add(1)
            .ok_or_else(|| provider_failure("node-accounting"))?;
        observed_depth = observed_depth.max(depth);
        if error_span.is_none() && (node.is_error() || node.is_missing()) {
            error_span = Some((node.start_byte(), node.end_byte()));
        }
        if depth >= max_depth && node.child_count() > 0 {
            limited_depth = true;
            continue;
        }
        limited_nodes |= push_children_bounded(
            node,
            depth,
            &mut stack,
            max_nodes.saturating_sub(processed_nodes),
        );
    }
    cancellation.check()?;
    Ok(SyntaxTraversal {
        processed_nodes,
        observed_depth,
        error_span,
        limited_nodes,
        limited_depth,
    })
}

fn push_children_bounded<'tree>(
    node: Node<'tree>,
    depth: usize,
    stack: &mut Vec<(Node<'tree>, usize)>,
    remaining: usize,
) -> bool {
    let child_capacity = remaining.saturating_sub(stack.len());
    let total_children = node.child_count();
    let child_count = total_children.min(child_capacity);
    for index in (0..child_count).rev() {
        let Ok(index) = u32::try_from(index) else {
            continue;
        };
        if let Some(child) = node.child(index) {
            stack.push((child, depth.saturating_add(1)));
        }
    }
    total_children > child_capacity
}

fn emit_primary_diagnostic(
    traversal: &TraversalReport,
    request: &ParseRequest<'_>,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let Some(primary) = traversal.primary_diagnostic else {
        return Ok(());
    };
    cancellation.check()?;
    let (code, source, coverage) = match primary {
        PrimaryDiagnostic::NodeLimit => (
            "syntax-node-limit",
            Some(request.source().source_ref().clone()),
            CoverageStatus::Bounded,
        ),
        PrimaryDiagnostic::DepthLimit => (
            "syntax-depth-limit",
            Some(request.source().source_ref().clone()),
            CoverageStatus::Bounded,
        ),
        PrimaryDiagnostic::ErrorRecovery { start, end } => (
            "syntax-error-recovery",
            source_ref_for_span(request.source().source_ref(), start, end),
            CoverageStatus::Unknown,
        ),
    };
    let diagnostic = AdapterDiagnostic::new(
        DiagnosticCode::new(code).map_err(|_| provider_failure("diagnostic-code"))?,
        DiagnosticSeverity::Warning,
        source,
        coverage,
    );
    sink.push(SyntaxFactBatch::new(
        sink.next_sequence(),
        Vec::new(),
        vec![diagnostic],
    ))?;
    Ok(())
}

fn source_ref_for_span(source: &SourceRef, start: usize, end: usize) -> Option<SourceRef> {
    let start = u64::try_from(start).ok()?;
    let end = u64::try_from(end).ok()?;
    let span = SourceSpan::new(source.span().file(), start, end).ok()?;
    Some(SourceRef::new(
        source.repository(),
        source.generation(),
        span,
        source.content_hash(),
        None,
    ))
}

fn map_pool_error(error: PoolError) -> AdapterError {
    match error {
        PoolError::Cancelled(cancelled) => cancelled.into(),
        PoolError::AccountingOverflow => provider_failure("pool-accounting"),
        PoolError::Poisoned => provider_failure("pool-state"),
        PoolError::MissingParser => provider_failure("pool-parser"),
    }
}

fn provider_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("hard-coded diagnostic code is valid"),
    }
}

fn require_provider_limit(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<(), AdapterError> {
    if observed > limit {
        Err(RequestError::ProviderLimit {
            resource,
            observed,
            limit,
        }
        .into())
    } else {
        Ok(())
    }
}

trait ByteBoundary {
    fn is_char_boundary(&self, index: usize) -> bool;
}

impl ByteBoundary for [u8] {
    fn is_char_boundary(&self, index: usize) -> bool {
        std::str::from_utf8(self).is_ok_and(|text| text.is_char_boundary(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reuse_identity_dimension_has_a_distinct_invalidation() {
        let base = invalid_identity();
        let tree = language_tree(GrammarFamily::Rust, b"fn one() {}");
        let cached = CachedParse {
            entry_id: 1,
            identity: base.clone(),
            source: Arc::from(&b"fn one() {}"[..]),
            tree: Some(tree),
            accounted_bytes: 1,
            invalidation: None,
        };
        let mut changed = base.clone();
        changed.family = GrammarFamily::Python;
        assert_invalidated(&cached, changed, ReuseInvalidation::Language);
        let mut changed = base.clone();
        changed.grammar_version = "other";
        assert_invalidated(&cached, changed, ReuseInvalidation::GrammarVersion);
        let mut changed = base.clone();
        changed.encoding = "other".to_owned();
        assert_invalidated(&cached, changed, ReuseInvalidation::Encoding);
        let mut changed = base.clone();
        changed
            .included_ranges
            .push(rootlight_adapter_sdk::IncludedRange::new(
                SourceSpan::new(rootlight_ids::FileId::from_bytes([1; 20]), 0, 1)
                    .expect("test span is ordered"),
                rootlight_adapter_sdk::LanguageId::new("rust").expect("test language is valid"),
            ));
        assert_invalidated(&cached, changed, ReuseInvalidation::IncludedRanges);
        let mut changed = base.clone();
        changed.settings = ParserSettings::new(2).expect("test setting is nonzero");
        assert_invalidated(&cached, changed, ReuseInvalidation::ParserSettings);
        let mut changed = base;
        changed.content_hash = ContentHash::from_bytes([2; 32]);
        assert_invalidated(&cached, changed, ReuseInvalidation::MissingEdits);
    }

    #[test]
    fn edit_validation_rejects_boundaries_and_result_mismatch() {
        let tree = language_tree(GrammarFamily::Rust, "fn café() {}".as_bytes());
        let split_scalar = SourceEdit::new(7, 8, "x").expect("test edit has an ordered byte range");
        assert!(matches!(
            apply_edits(
                tree.clone(),
                "fn café() {}".as_bytes(),
                b"fn xafe() {}",
                &[split_scalar],
                usize::MAX,
                &Cancellation::new(),
            ),
            Err(ApplyEditError::Invalidation(
                ReuseInvalidation::EditNotCharacterBoundary
            ))
        ));
        let mismatch = SourceEdit::new(3, 8, "other").expect("test edit is valid");
        assert!(matches!(
            apply_edits(
                tree,
                "fn café() {}".as_bytes(),
                b"different",
                &[mismatch],
                usize::MAX,
                &Cancellation::new(),
            ),
            Err(ApplyEditError::Invalidation(
                ReuseInvalidation::EditResultMismatch
            ))
        ));
    }

    #[test]
    fn source_points_use_utf8_byte_columns_and_crlf_rows() {
        assert_eq!(
            point_for_offset("é\r\nx".as_bytes(), 4),
            Some(Point { row: 1, column: 0 })
        );
        assert_eq!(
            point_for_offset("é\r\nx".as_bytes(), 5),
            Some(Point { row: 1, column: 1 })
        );
    }

    #[test]
    fn traversal_and_edit_preparation_observe_cancellation() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        let tree = language_tree(GrammarFamily::Rust, b"fn cancelled() {}");

        assert!(matches!(
            traverse_syntax(&tree, 1024, 64, &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
        let changed = language_tree(GrammarFamily::Rust, b"fn changed() {}");
        assert!(matches!(
            count_changed_ranges(&tree, &changed, 1024, &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            copy_source_for_cache(b"source", &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));

        let config = RuntimeConfig::new(
            1024,
            1024,
            64,
            4,
            4,
            1,
            4096,
            ParserSettings::new(64).expect("test parser setting is valid"),
        )
        .expect("test runtime configuration is valid");
        let previous = PreviousParse {
            provider_id: 1,
            entry_id: 1,
        };
        let edit = SourceEdit::new(0, 0, "x").expect("test edit is valid");
        assert!(matches!(
            validate_edit_admission(Some(&previous), &[edit], &config, &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }

    fn assert_invalidated(
        cached: &CachedParse,
        current: ParseIdentity,
        expected: ReuseInvalidation,
    ) {
        assert!(matches!(
            prepare_reuse(
                Some(cached),
                &current,
                &cached.source,
                &[],
                usize::MAX,
                &Cancellation::new(),
            )
            .expect("reuse identity validation succeeds"),
            (None, ReuseStatus::Invalidated(observed)) if observed == expected
        ));
    }

    fn language_tree(family: GrammarFamily, source: &[u8]) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(family))
            .expect("audited grammar ABI is supported");
        parser
            .parse(source, None)
            .expect("test parser has a language")
    }
}
