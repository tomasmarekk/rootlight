//! Bounded Tree-sitter parsing, pooling, incremental reuse, and diagnostics.
//!
//! Rust-owned scans, copies, sorts, staging, and commits use bounded cooperative
//! checkpoints, while Tree-sitter parsing uses its native progress callback.
//! `changed_ranges` performs eager native work before Rust iterator checkpoints;
//! hard preemption and native-allocation isolation remain responsibilities of the isolated adapter supervisor.

mod astro_reads;
mod c_preprocessor;
mod cmake_command_template;
mod cmake_header;
mod cmake_patch;
mod coffeescript_scope;
#[cfg(test)]
#[path = "runtime/dockerfile_cache_tests.rs"]
mod dockerfile_cache_tests;
#[cfg(test)]
#[path = "runtime/dockerfile_callback_owner_tests.rs"]
mod dockerfile_callback_owner_tests;
mod dockerfile_scope;
mod embedded;
mod expression;
mod fortran;
mod fsharp_admission;
mod gdscript_scope;
#[cfg(test)]
#[path = "runtime/gdscript_scope_tests.rs"]
mod gdscript_scope_tests;
mod identity_refusal;
mod identity_work;
mod idris_scope;
#[cfg(test)]
mod idris_scope_tests;
mod jsonc;
mod lean_scope;
#[cfg(test)]
mod lean_scope_tests;
mod markdown;
mod perl;
mod pug_scope;
#[cfg(test)]
#[path = "runtime/pug_scope_tests.rs"]
mod pug_scope_tests;
mod scanner_refusal;
mod sed_scope;
pub(crate) const SED_FRONTEND_POLICY: &str = sed_scope::FRONTEND_POLICY;
pub(crate) const SED_SCANNER_CONTRACT_IDENTITY: &str = sed_scope::SCANNER_CONTRACT_IDENTITY;
#[cfg(test)]
mod sed_scope_tests;
mod wolfram_scope;
#[cfg(test)]
#[path = "runtime/wolfram_scope_tests.rs"]
mod wolfram_scope_tests;

pub(crate) use c_preprocessor::C_PREPROCESSOR_PROJECTION_POLICY;
pub(crate) use cmake_command_template::CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY;
pub(crate) use cmake_header::CMAKE_HEADER_PROJECTION_POLICY;
pub(crate) use cmake_patch::CMAKE_PATCH_PROJECTION_POLICY;
pub(crate) use fortran::FORTRAN_TEMPLATE_PROJECTION_POLICY;

fn cmake_patch_input(
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
    max_units: usize,
    cancellation: &Cancellation,
) -> Result<Option<cmake_patch::CmakePatchInput>, AdapterError> {
    if request.language().as_str() != "cmake" || !request.included_ranges().is_empty() {
        return Ok(None);
    }
    let memory = request
        .limits()
        .max_reported_memory_bytes()
        .checked_sub(source.owned_bytes())
        .ok_or_else(|| provider_failure("cmake-patch-memory-limit"))?;
    let plan =
        cmake_patch::CmakePatchInput::new(source.as_bytes(), max_units, memory, cancellation)?;
    if let Some(plan) = &plan {
        require_provider_limit(
            ResourceKind::ReportedMemoryBytes,
            plan.owned_bytes(),
            memory,
        )?;
        require_provider_limit(
            ResourceKind::SyntaxNodes,
            plan.required_candidate_count(),
            max_units,
        )?;
    }
    Ok(plan)
}
pub use crate::query_pack::NativeIdentityRefusalDetails;
pub use fsharp_admission::{FSharpAdmissionKind, FSharpAdmissionWitness};
pub use identity_refusal::NativeIdentityRefusal;
pub use identity_work::NativeIdentityWorkLimit;
pub(crate) use jsonc::JSONC_PARSE_INPUT_POLICY;
pub use scanner_refusal::{NativeScannerRefusal, NativeScannerRefusalDetails};

mod native_input;

#[cfg(test)]
mod native_context_tests;

use std::{
    cell::Cell,
    collections::VecDeque,
    ops::ControlFlow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DiagnosticCode, EncodingId, IncludedRange,
    LanguageId, MemoryAdmissionPolicy, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, RemainingBudget, RequestError, ResourceKind, ResourceUsage,
    SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch, SyntaxFactSink, SyntaxKindLabel, WorkReport,
    execute_parse_transaction,
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
    query_pack::{QueryCandidate, QueryPackRegistry, StructuralRole},
    registry::{language_for, native_family_for_source},
    source_view::{LineEndingPolicy, NATIVE_SOURCE_PROJECTION_POLICY, NativeSourceView},
};

const LOGICAL_TREE_NODE_BYTES: usize = 64;

fn apply_native_vba_module_kind(
    candidates: &mut Vec<QueryCandidate>,
    request: &ParseRequest<'_>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    use rootlight_adapter_sdk::NativeVbaModuleKind;
    let Some(kind) = request.native_vba_module() else {
        return Ok(());
    };
    let mut module = None;
    let mut name = None;
    for (index, candidate) in candidates.iter().enumerate() {
        cancellation.check()?;
        if candidate.role == StructuralRole::Module
            && matches!(candidate.syntax, "vba.file" | "vba.class")
            && module.replace(index).is_some()
        {
            return Err(provider_failure("vba-module-context-ambiguous"));
        }
        if candidate.role == StructuralRole::Definition
            && candidate.syntax == "vba.module_name"
            && name.replace(*candidate).is_some()
        {
            return Err(provider_failure("vba-module-context-ambiguous"));
        }
    }
    let Some(index) = module else {
        return Err(provider_failure("vba-module-context-missing"));
    };
    if kind == NativeVbaModuleKind::Procedural {
        if candidates[index].syntax == "vba.class" {
            return Err(provider_failure("vba-module-context-conflict"));
        }
        return Ok(());
    }
    if candidates[index].syntax == "vba.class" {
        return Ok(());
    }
    let Some(mut signature) = name else {
        return Err(provider_failure("vba-module-context-name"));
    };
    // Native PROJECT/dir metadata supplies the category. The real VB_Name span
    // provides a body-independent signature without fabricating export bytes.
    signature.role = StructuralRole::Signature;
    signature.syntax = "vba.class";
    signature.required = true;
    candidates
        .try_reserve(1)
        .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
    candidates[index].syntax = "vba.class";
    candidates.push(signature);
    Ok(())
}
const LOGICAL_SYNTAX_FACT_BYTES: usize = 64;
const MIN_SYNTAX_KIND_BYTES: usize = 1;
const MIN_SYNTAX_FACT_OUTPUT_BYTES: usize = LOGICAL_SYNTAX_FACT_BYTES + MIN_SYNTAX_KIND_BYTES;
const CANCELLATION_CHECK_INTERVAL: usize = 256;
const CANCELLATION_BYTE_INTERVAL: usize = 64 * 1024;
// The safe API exposes action checkpoints, not native stack depth. Node credit
// bounds total work; depth credit bounds work without new byte progress. Actual
// syntax depth is checked on the completed tree.
const PARSE_PROGRESS_CHECKS_PER_SYNTAX_NODE: usize = 4;
const PARSE_PROGRESS_CHECKS_PER_SYNTAX_DEPTH: usize = 1024;
static NEXT_PROVIDER_ID: AtomicU64 = AtomicU64::new(1);

/// Bounded Tree-sitter parser provider.
///
/// HTML script and style bodies use the JavaScript and CSS grammars under
/// the host file's shared node, range and output budgets.
/// Markdown inline regions retain original source coordinates through excluded
/// container markers under the same bounded native parsing path.
/// Astro expressions use TSX with coordinate-preserving outer delimiters;
/// all emitted source evidence still refers to the untouched host snapshot.
pub struct TreeSitterProvider {
    provider_id: u64,
    registry: GrammarRegistry,
    query_packs: &'static QueryPackRegistry,
    capabilities: ParseCapabilities,
    config: RuntimeConfig,
    pool: ParserPool,
    cache: Mutex<ParseCache>,
}

impl TreeSitterProvider {
    /// Creates a provider with explicit parser, pool, and cache capacities.
    ///
    /// Compiled built-in queries are shared across the process; parser pools,
    /// source caches, capacities and incremental identities remain independent.
    /// Every built-in query is compiled and validated before this returns.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError`] if built-in registry/capability metadata
    /// fails validation or process-local provider identities are exhausted.
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeConfigError> {
        let provider = Self::new_deferred(config)?;
        QueryPackRegistry::audited()
            .map_err(|family| RuntimeConfigError::InvalidBuiltInQueryPack { family })?;
        Ok(provider)
    }

    /// Creates a provider that compiles each built-in query on first use.
    ///
    /// All grammar ABIs and capability metadata are validated immediately.
    /// All languages, embedded grammars and TSX remain available. Immutable
    /// queries are shared with eager providers; pools and caches remain independent.
    /// Query compilation failures return [`AdapterError::ProviderFailed`] with
    /// code `query-pack-compile` from the invoking parse or analysis operation.
    /// Cancellation is checked before and after first-use compilation or waiting;
    /// hard preemption of native compilation requires the isolated supervisor.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError`] if registry/capability metadata fails
    /// validation or process-local provider identities are exhausted.
    pub fn new_deferred(config: RuntimeConfig) -> Result<Self, RuntimeConfigError> {
        Self::with_query_packs(config, QueryPackRegistry::deferred())
    }

    fn with_query_packs(
        config: RuntimeConfig,
        query_packs: &'static QueryPackRegistry,
    ) -> Result<Self, RuntimeConfigError> {
        #[allow(
            deprecated,
            reason = "try_update is unavailable on the supported Rust 1.90 minimum"
        )]
        let provider_id = NEXT_PROVIDER_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| RuntimeConfigError::ProviderIdentityExhausted)?;
        let registry = GrammarRegistry::audited()?;
        for descriptor in registry.descriptors() {
            if !query_packs.contains(descriptor.family()) {
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
            vec![
                EncodingId::new("utf-8")?,
                EncodingId::new("utf-16le")?,
                EncodingId::new("utf-16be")?,
            ],
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

    /// Measures the exact declaration-identity syntax facts required by one source.
    ///
    /// The preflight performs the same bounded parse, canonical query
    /// deduplication, included-range restriction, and declaration-scope closure
    /// as [`ParseProvider::parse`], but does not scan or retain optional facts.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for unsupported or invalid input, cancellation,
    /// parser work limits, query hard limits, or bounded allocation failure.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        let source = self.native_source_view(request, cancellation)?;
        let mut prepared = self.prepare_tree(
            request,
            None,
            &[],
            self.config.default_settings(),
            &source,
            cancellation,
            None,
        )?;
        if !prepared.traversal.fully_traversed {
            cancellation.check()?;
            return Ok(0);
        }
        let pack = self.query_packs.get_for_source(
            prepared.family,
            request.source().path().as_str(),
            cancellation,
        )?;
        let mut candidates = pack.extract_identity(
            prepared.family,
            &prepared.tree,
            source.as_bytes(),
            request.limits().max_syntax_nodes(),
            cancellation,
        )?;
        apply_native_vba_module_kind(&mut candidates, request, cancellation)?;
        self.extract_embedded(
            &prepared.tree,
            request,
            &source,
            &mut prepared.traversal,
            &mut candidates,
            None,
            cancellation,
        )?;
        normalize_query_candidates(candidates, request, &source, usize::MAX, cancellation)
            .map(|normalized| normalized.required)
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
                self.parse_with_previous_raw(
                    request,
                    previous,
                    edits,
                    settings,
                    sink,
                    cancellation,
                    None,
                    None,
                    None,
                )
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
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn parse_with_previous_raw(
        &self,
        request: &ParseRequest<'_>,
        previous: Option<&PreviousParse>,
        edits: &[SourceEdit],
        settings: ParserSettings,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
        identity_limit: Option<&mut Option<crate::query_pack::IntrinsicIdentityMatchLimit>>,
        identity_refusal: Option<&mut Option<crate::query_pack::identity_refusal::IdentityRefusal>>,
        scanner_refusal: Option<&mut Option<NativeScannerRefusalDetails>>,
    ) -> Result<RawParseWithPrevious, AdapterError> {
        let source = self.native_source_view(request, cancellation)?;
        let PreparedParse {
            family,
            identity,
            tree,
            mut traversal,
            reuse_status,
            reuse_key,
            c_macro_projected,
            cache_nodes,
        } = self.prepare_tree(
            request,
            previous,
            edits,
            settings,
            &source,
            cancellation,
            scanner_refusal,
        )?;
        let source_bytes = request.source().bytes();
        let extraction = if traversal.fully_traversed {
            self.extract_syntax_facts(
                family,
                &tree,
                request,
                &source,
                &mut traversal,
                sink,
                cancellation,
                identity_limit,
                identity_refusal,
            )?
        } else {
            emit_primary_diagnostic(&traversal, request, sink, cancellation)?;
            ExtractionReport { limited: false }
        };
        let usage = sink.staged_usage();
        let coverage_status = if extraction.limited && traversal.coverage != CoverageStatus::Unknown
        {
            CoverageStatus::Bounded
        } else {
            traversal.coverage
        };
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            coverage_status,
            source_bytes.len(),
            traversal.covered_source_bytes,
            traversal
                .skipped_regions
                .checked_add(usize::from(extraction.limited))
                .ok_or_else(|| provider_failure("coverage-accounting"))?,
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
        let pending = if traversal.fully_traversed && !c_macro_projected {
            Some(PendingParse {
                identity,
                tree,
                nodes: cache_nodes,
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

    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn prepare_tree(
        &self,
        request: &ParseRequest<'_>,
        previous: Option<&PreviousParse>,
        edits: &[SourceEdit],
        settings: ParserSettings,
        source: &NativeSourceView<'_>,
        cancellation: &Cancellation,
        scanner_refusal: Option<&mut Option<NativeScannerRefusalDetails>>,
    ) -> Result<PreparedParse, AdapterError> {
        let (family, native_family) =
            self.admit_parse_input(request, previous, edits, settings, cancellation)?;
        let source_bytes = request.source().bytes();
        let descriptor = self
            .registry
            .get(native_family)
            .ok_or_else(|| provider_failure("grammar-missing"))?;
        cancellation.check()?;
        let included_ranges = request.shared_included_ranges();
        cancellation.check()?;
        let identity = ParseIdentity {
            content_hash: request.source().source_ref().content_hash(),
            family: native_family,
            grammar_version: descriptor.grammar_version(),
            encoding: request.encoding().as_str().to_owned(),
            included_ranges: Arc::clone(&included_ranges),
            settings,
            virtual_line_ending: needs_virtual_matlab_line_ending(request, source.as_bytes()),
            line_endings: source.line_ending_policy(),
            cmake_header_path: request.language().as_str() == "cmake"
                && cmake_header::is_header_path(request.source().path().as_str()),
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
            native_family,
            cmake_header_path: identity.cmake_header_path,
            line_endings: identity.line_endings,
            grammar_version: identity.grammar_version,
            encoding: identity.encoding.clone(),
            included_ranges,
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

        let parsed = self.parse_native_tree_with_input(
            request,
            &language_for(native_family),
            old_tree.as_ref(),
            settings,
            source,
            None,
            None,
            0,
            cancellation,
            scanner_refusal,
        )?;
        let c_macro_projected = parsed.projection_changed || parsed.projection_unavailable;
        if c_macro_projected && matches!(reuse_status, ReuseStatus::Reused { .. }) {
            // Raw reuse cannot qualify a changed private input or withheld tree.
            reuse_status = ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute);
        }
        if matches!(reuse_status, ReuseStatus::Reused { .. }) {
            let changed_ranges = old_tree.as_ref().map_or(Ok(0), |old| {
                count_changed_ranges(
                    old,
                    &parsed.tree,
                    self.config.max_syntax_nodes(),
                    cancellation,
                )
            })?;
            reuse_status = ReuseStatus::Reused { changed_ranges };
        }
        let traversal = inspect_parsed_tree(
            &parsed,
            request,
            source,
            request.limits().max_syntax_nodes(),
            request.limits().max_syntax_depth(),
            cancellation,
        )?;
        // A complete unchanged proof counts the one retained tree. Walk-work
        // credit must not double its physical cache-retention estimate.
        let cache_nodes = if parsed.projection_nodes != 0 && !c_macro_projected {
            parsed.projection_nodes
        } else {
            traversal.processed_nodes
        };
        cancellation.check()?;
        Ok(PreparedParse {
            family,
            identity,
            tree: parsed.tree,
            traversal,
            reuse_status,
            reuse_key,
            c_macro_projected,
            cache_nodes,
        })
    }

    fn native_source_view<'a>(
        &self,
        request: &'a ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<NativeSourceView<'a>, AdapterError> {
        let maximum = self
            .config
            .max_source_bytes()
            .min(request.limits().max_source_bytes());
        require_provider_limit(
            ResourceKind::SourceBytes,
            request.source().bytes().len(),
            maximum,
        )?;
        if request.encoding().as_str() == "utf-8" {
            // Preserve chunked cancellation while the borrowed view independently
            // obtains its checked UTF-8 string without copying the snapshot.
            validate_utf8_cancellable(request.source().bytes(), cancellation)?;
        }
        NativeSourceView::new_with_policy(
            request.source().bytes(),
            request.encoding().as_str(),
            LineEndingPolicy::for_language(request.language().as_str()),
            maximum,
            request.limits().max_reported_memory_bytes(),
            cancellation,
        )
    }

    fn admit_parse_input(
        &self,
        request: &ParseRequest<'_>,
        previous: Option<&PreviousParse>,
        edits: &[SourceEdit],
        settings: ParserSettings,
        cancellation: &Cancellation,
    ) -> Result<(GrammarFamily, GrammarFamily), AdapterError> {
        cancellation.check()?;
        if settings.input_chunk_bytes() > self.config.max_source_bytes() {
            return Err(provider_failure("treesitter-settings"));
        }
        validate_edit_admission(previous, edits, &self.config, cancellation)?;
        let family = self
            .registry
            .family_for_language(request.language())
            .ok_or(RequestError::UnsupportedLanguage)?;
        let native_family = native_family_for_source(family, request.source().path().as_str());
        // Arbitrary public ranges can join unrelated physical text. Markdown
        // separately admits only a qualified complete physical-line body.
        if (native_family.is_cobol()
            || matches!(native_family, GrammarFamily::Sed | GrammarFamily::Idris))
            && !request.included_ranges().is_empty()
        {
            return Err(RequestError::EmbeddedRangesUnsupported.into());
        }
        if !matches!(
            request.encoding().as_str(),
            "utf-8" | "utf-16le" | "utf-16be"
        ) {
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
        validate_included_range_languages(
            request.included_ranges(),
            request.language(),
            cancellation,
        )?;
        cancellation.check()?;
        Ok((family, native_family))
    }

    // The caller admits the full source and included ranges. Host and embedded
    // grammars share the same pool, native work budget and cancellation path.
    #[allow(clippy::too_many_arguments)]
    fn parse_native_tree(
        &self,
        request: &ParseRequest<'_>,
        language: &tree_sitter::Language,
        old_tree: Option<&Tree>,
        settings: ParserSettings,
        source: &NativeSourceView<'_>,
        context: Option<&mut NativeParseContext>,
        cancellation: &Cancellation,
    ) -> Result<Tree, AdapterError> {
        self.parse_native_tree_with_input(
            request,
            language,
            old_tree,
            settings,
            source,
            context,
            None,
            0,
            cancellation,
            None,
        )
        .map(|parsed| parsed.tree)
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_native_tree_with_input(
        &self,
        request: &ParseRequest<'_>,
        language: &tree_sitter::Language,
        old_tree: Option<&Tree>,
        settings: ParserSettings,
        source: &NativeSourceView<'_>,
        context: Option<&mut NativeParseContext>,
        parse_input: Option<&[u8]>,
        upstream_owned_bytes: usize,
        cancellation: &Cancellation,
        scanner_refusal: Option<&mut Option<NativeScannerRefusalDetails>>,
    ) -> Result<NativeParsedTree, AdapterError> {
        if !matches!(request.language().as_str(), "c" | "cpp") {
            return self
                .parse_native_tree_once(
                    request,
                    language,
                    old_tree,
                    settings,
                    source,
                    context,
                    parse_input,
                    cancellation,
                    scanner_refusal,
                )
                .map(NativeParsedTree::ordinary);
        }
        let limit = ParseWorkLimit::from_syntax_limits(
            request.limits().max_syntax_nodes(),
            request.limits().max_syntax_depth(),
        );
        let mut local = NativeParseContext {
            expression_envelope: None,
            range_origin: (0, Point::new(0, 0)),
            remaining_progress_checks: limit.max_total_progress_checks,
        };
        let shared = match context {
            Some(context) => context,
            None => &mut local,
        };
        let tree = self.parse_native_tree_once(
            request,
            language,
            old_tree,
            settings,
            source,
            Some(&mut *shared),
            parse_input,
            cancellation,
            None,
        )?;
        let recognize = if request.language().as_str() == "cpp" {
            c_preprocessor::MacroProjection::recognize_cpp
        } else {
            c_preprocessor::MacroProjection::recognize
        };
        let outcome = recognize(
            &tree,
            source,
            parse_input.unwrap_or_else(|| source.as_bytes()),
            upstream_owned_bytes,
            request.included_ranges().len().max(1),
            c_preprocessor::ProjectionLimits {
                nodes: request
                    .limits()
                    .max_syntax_nodes()
                    .min(self.config.max_syntax_nodes()),
                depth: request
                    .limits()
                    .max_syntax_depth()
                    .min(self.config.max_syntax_depth()),
                owned_bytes: request.limits().max_reported_memory_bytes(),
            },
            cancellation,
        )?;
        let plan = match outcome {
            c_preprocessor::ProjectionOutcome::Projected(plan) => plan,
            c_preprocessor::ProjectionOutcome::Unchanged {
                work_nodes,
                work_depth,
            } => {
                return Ok(NativeParsedTree {
                    projection_nodes: work_nodes,
                    projection_depth: work_depth,
                    ..NativeParsedTree::ordinary(tree)
                });
            }
            c_preprocessor::ProjectionOutcome::Unavailable {
                work_nodes,
                work_depth,
            } => {
                return Ok(NativeParsedTree {
                    projection_nodes: work_nodes,
                    projection_depth: work_depth,
                    projection_unavailable: true,
                    ..NativeParsedTree::ordinary(tree)
                });
            }
            c_preprocessor::ProjectionOutcome::Limited {
                work_nodes,
                work_depth,
                limit,
            } => {
                return Ok(NativeParsedTree {
                    projection_nodes: work_nodes,
                    projection_depth: work_depth,
                    proof_limit: Some(limit),
                    ..NativeParsedTree::ordinary(tree)
                });
            }
        };
        // Retain only the source-proved descriptor result before allocating the
        // final native tree; raw and projected trees cannot share edit identity.
        drop(tree);
        let tree = self.parse_native_tree_once(
            request,
            language,
            None,
            settings,
            source,
            Some(shared),
            Some(plan.as_bytes()),
            cancellation,
            None,
        )?;
        Ok(NativeParsedTree {
            tree,
            projection_nodes: plan.work_nodes,
            projection_depth: plan.work_depth,
            projection_unavailable: false,
            projection_changed: true,
            proof_limit: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_native_tree_once(
        &self,
        request: &ParseRequest<'_>,
        language: &tree_sitter::Language,
        old_tree: Option<&Tree>,
        settings: ParserSettings,
        source: &NativeSourceView<'_>,
        mut context: Option<&mut NativeParseContext>,
        parse_input: Option<&[u8]>,
        cancellation: &Cancellation,
        mut scanner_refusal: Option<&mut Option<NativeScannerRefusalDetails>>,
    ) -> Result<Tree, AdapterError> {
        cancellation.check()?;
        let parse_work_limit = ParseWorkLimit::from_syntax_limits(
            request.limits().max_syntax_nodes(),
            request.limits().max_syntax_depth(),
        );
        // Short inputs may finish before the first native progress callback.
        if context
            .as_ref()
            .is_some_and(|context| context.remaining_progress_checks == 0)
        {
            return Err(provider_failure(parse_work_limit.failure_code));
        }
        let patch_input = if parse_input.is_none() {
            cmake_patch_input(
                request,
                source,
                request.limits().max_syntax_nodes(),
                cancellation,
            )?
        } else {
            None
        };
        let header_plan = if patch_input.is_none()
            && parse_input.is_none()
            && request.language().as_str() == "cmake"
            && request.included_ranges().is_empty()
        {
            cmake_header::HeaderPlan::recognize(
                request.source().path().as_str(),
                source,
                request.limits(),
                cancellation,
            )?
        } else {
            None
        };
        let header_input = header_plan
            .as_ref()
            .map(|plan| {
                plan.parse_bytes(
                    source,
                    true,
                    request.limits().max_reported_memory_bytes(),
                    cancellation,
                )
            })
            .transpose()?;
        let command_template = if patch_input.is_none()
            && header_plan.is_none()
            && parse_input.is_none()
            && request.language().as_str() == "cmake"
            && request.included_ranges().is_empty()
        {
            cmake_command_template::CommandTemplatePlan::recognize(
                source,
                request.limits(),
                cancellation,
            )?
        } else {
            None
        };
        let command_template_input = command_template
            .as_ref()
            .map(|plan| plan.parse_bytes(source, request.limits(), cancellation))
            .transpose()?;
        let fortran_input = if request.language().as_str() == "fortran" && context.is_none() {
            Some(fortran::FortranParseInput::new(
                source,
                request.limits().max_reported_memory_bytes(),
                cancellation,
            )?)
        } else {
            None
        };
        let source_bytes = parse_input
            .or_else(|| {
                patch_input
                    .as_ref()
                    .map(cmake_patch::CmakePatchInput::parse_bytes)
            })
            .or(header_input.as_deref())
            .or(command_template_input.as_deref())
            .unwrap_or_else(|| {
                fortran_input
                    .as_ref()
                    .map_or_else(|| source.as_bytes(), fortran::FortranParseInput::as_bytes)
            });
        let gdscript_scope = (request.language().as_str() == "gdscript")
            .then(gdscript_scope::begin)
            .transpose()?;
        let dockerfile_scope = (request.language().as_str() == "dockerfile")
            .then(|| dockerfile_scope::begin(cancellation))
            .transpose()?;
        let coffeescript_scope = (request.language().as_str() == "coffeescript")
            .then(|| {
                coffeescript_scope::begin(cancellation, parse_work_limit.max_total_progress_checks)
            })
            .transpose()?;
        let idris_scope = (request.language().as_str() == "idris")
            .then(|| idris_scope::begin(cancellation, parse_work_limit.max_total_progress_checks))
            .transpose()?;
        let sed_scope = (request.language().as_str() == "sed")
            .then(|| sed_scope::begin(cancellation, parse_work_limit.max_total_progress_checks))
            .transpose()?;
        let wolfram_scope = (request.language().as_str() == "wolfram")
            .then(|| wolfram_scope::begin(cancellation, parse_work_limit.max_total_progress_checks))
            .transpose()?;
        let pug_scope = (request.language().as_str() == "pug")
            .then(|| pug_scope::begin(cancellation, parse_work_limit.max_total_progress_checks))
            .transpose()?;
        let lean_scope = (request.language().as_str() == "lean")
            .then(|| {
                lean_scope::begin(
                    cancellation,
                    source.as_bytes(),
                    parse_work_limit.max_total_progress_checks,
                )
            })
            .transpose()?;
        let mut lease = self.pool.acquire(cancellation).map_err(map_pool_error)?;
        if dockerfile_scope.is_some()
            || coffeescript_scope.is_some()
            || pug_scope.is_some()
            || lean_scope.is_some()
            || wolfram_scope.is_some()
            || sed_scope.is_some()
            || idris_scope.is_some()
        {
            // Keep a refused or interrupted native scanner transaction out of the
            // reusable pool until its native scope and returned tree are admitted.
            lease.withhold_reuse();
        }
        let parser = lease.parser_mut().map_err(map_pool_error)?;
        parser
            .set_language(language)
            .map_err(|_| provider_failure("grammar-abi"))?;
        let included_ranges = tree_sitter_ranges(
            request,
            source_bytes,
            source,
            context.as_ref().map(|context| context.range_origin),
            cancellation,
        )?;
        let jsonc_input = if request.language().as_str() == "jsonc" {
            Some(jsonc::JsoncParseInput::new(
                source,
                &included_ranges,
                request.limits().max_reported_memory_bytes(),
                cancellation,
            )?)
        } else {
            None
        };
        let source_bytes = jsonc_input
            .as_ref()
            .map_or(source_bytes, jsonc::JsoncParseInput::as_bytes);
        parser
            .set_included_ranges(&included_ranges)
            .map_err(|_| provider_failure("included-ranges"))?;

        let virtual_line_ending =
            context.is_none() && needs_virtual_matlab_line_ending(request, source_bytes);
        let mut callback_cancelled = false;
        let envelope = context
            .as_ref()
            .and_then(|context| context.expression_envelope);
        let mut callback_failure = None;
        let mut native_progress = NativeParseProgress::default();
        let input_control = native_input::Control::new(
            parse_work_limit.max_total_progress_checks,
            parse_work_limit.failure_code,
            cancellation,
        );
        let shared_progress = Cell::new(
            context
                .as_ref()
                .map(|context| context.remaining_progress_checks),
        );
        let mut progress = |state: &tree_sitter::ParseState| {
            if !input_control.checkpoint(&shared_progress) {
                return ControlFlow::Break(());
            }
            match cancellation.check() {
                Ok(()) => {
                    let native_failure =
                        native_progress.record(state.current_byte_offset(), parse_work_limit);
                    callback_failure = native_failure;
                    if callback_failure.is_some() {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                }
                Err(_) => {
                    callback_cancelled = true;
                    ControlFlow::Break(())
                }
            }
        };
        let chunk_bytes = settings.input_chunk_bytes();
        let mut input = |offset: usize, _point: Point| {
            if !input_control.checkpoint(&shared_progress) {
                return b"".as_slice();
            }
            if virtual_line_ending && offset == source_bytes.len() {
                // The grammar requires a statement delimiter at EOF. This byte
                // is parser-only; query spans are clamped to the real snapshot.
                return b"\n".as_slice();
            }
            let Some(end) = input_control.end(source_bytes, offset, chunk_bytes) else {
                return b"".as_slice();
            };
            envelope.map_or_else(
                || source_bytes.get(offset..end).unwrap_or_default(),
                |envelope| envelope.chunk(source_bytes, offset, end),
            )
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        let tree = parser.parse_with_options(&mut input, old_tree, Some(options));
        #[cfg(test)]
        if request.language().as_str() == "gdscript" {
            gdscript_scope_tests::after_native_parse();
        }
        if let Some(context) = context.as_mut() {
            context.remaining_progress_checks = shared_progress.get().unwrap_or(0);
        }
        if let Some(error) = input_control.error() {
            // The native parser may return a recovery tree after the stop EOF.
            // Its typed input failure wins before any tree or cache is accepted.
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if callback_cancelled {
            parser.reset();
            cancellation.check()?;
            return Err(provider_failure("parse-cancelled"));
        }
        if let Some(code) = callback_failure {
            parser.reset();
            return Err(provider_failure(code));
        }
        cancellation.check()?;
        if let Some(scope) = dockerfile_scope
            && let Err(error) =
                dockerfile_scope::finish(scope, cancellation, scanner_refusal.as_deref_mut())
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if let Some(scope) = coffeescript_scope
            && let Err(error) =
                coffeescript_scope::finish(scope, cancellation, scanner_refusal.as_deref_mut())
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if let Some(scope) = lean_scope
            && let Err(error) =
                lean_scope::finish(scope, cancellation, scanner_refusal.as_deref_mut())
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if let Some(scope) = pug_scope
            && let Err(error) =
                pug_scope::finish(scope, cancellation, scanner_refusal.as_deref_mut())
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if let Some(scope) = idris_scope
            && let Err(error) =
                idris_scope::finish(scope, cancellation, scanner_refusal.as_deref_mut())
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if let Some(scope) = sed_scope
            && let Err(error) =
                sed_scope::finish(scope, cancellation, scanner_refusal.as_deref_mut())
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        if let Some(scope) = wolfram_scope
            && let Err(error) = wolfram_scope::finish(scope, cancellation, scanner_refusal)
        {
            parser.reset();
            drop(tree);
            return Err(error);
        }
        let tree = tree.ok_or_else(|| provider_failure("parse-aborted"))?;
        cancellation.check()?;
        lease.allow_reuse();
        drop(lease);
        if let Some(scope) = gdscript_scope {
            gdscript_scope::finish(scope)?;
        }
        Ok(tree)
    }

    #[allow(clippy::too_many_arguments)]
    fn extract_syntax_facts(
        &self,
        family: GrammarFamily,
        tree: &Tree,
        request: &ParseRequest<'_>,
        source: &NativeSourceView<'_>,
        traversal: &mut TraversalReport,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
        identity_limit: Option<&mut Option<crate::query_pack::IntrinsicIdentityMatchLimit>>,
        identity_refusal: Option<&mut Option<crate::query_pack::identity_refusal::IdentityRefusal>>,
    ) -> Result<ExtractionReport, AdapterError> {
        cancellation.check()?;
        let budget = sink.remaining_budget();
        let max_facts = pre_extraction_fact_limit(budget);
        let pack = self.query_packs.get_for_source(
            family,
            request.source().path().as_str(),
            cancellation,
        )?;
        let mut extraction = match (identity_limit, identity_refusal) {
            (identity_limit, identity_refusal)
                if identity_limit.is_some() || identity_refusal.is_some() =>
            {
                pack.extract_with_identity_limit(
                    family,
                    tree,
                    source.as_bytes(),
                    request.limits().max_syntax_nodes(),
                    max_facts,
                    cancellation,
                    identity_limit,
                    identity_refusal,
                )
            }
            _ => pack.extract(
                family,
                tree,
                source.as_bytes(),
                request.limits().max_syntax_nodes(),
                max_facts,
                cancellation,
            ),
        }?;
        apply_native_vba_module_kind(&mut extraction.candidates, request, cancellation)?;
        let embedded_limits = self.extract_embedded(
            tree,
            request,
            source,
            traversal,
            &mut extraction.candidates,
            Some(max_facts),
            cancellation,
        )?;
        emit_primary_diagnostic(traversal, request, sink, cancellation)?;
        let mut partition_limited = extraction.limit.is_some() || embedded_limits.partition;
        if partition_limited {
            emit_extraction_limit_diagnostic(request, sink, cancellation)?;
        } else if embedded_limits.context {
            emit_extraction_diagnostic("syntax-context-limit", request, sink, cancellation)?;
        }
        let normalization_fact_limit = extraction
            .fact_limit
            .min(pre_extraction_fact_limit(sink.remaining_budget()));
        let mut normalized = prepare_query_candidates(
            extraction.candidates,
            request,
            source,
            normalization_fact_limit,
            cancellation,
        )?;
        let retention = admit_exact_query_candidates(
            &mut normalized.candidates,
            sink.remaining_budget(),
            cancellation,
        )?;
        normalized.limited |= retention.limited;
        if normalized.limited && !partition_limited {
            emit_extraction_limit_diagnostic(request, sink, cancellation)?;
            partition_limited = true;
            // The diagnostic consumes shared byte and string credits. Re-admit
            // candidates before assigning parents or local IDs to the survivors.
            let retention = admit_exact_query_candidates(
                &mut normalized.candidates,
                sink.remaining_budget(),
                cancellation,
            )?;
            normalized.limited |= retention.limited;
        }
        let normalized = materialize_query_candidates(normalized, request, source, cancellation)?;
        let plan = plan_fact_batches(&normalized.facts, sink.remaining_budget(), cancellation)?;
        let fact_count = normalized.facts.len();
        if plan.emitted != fact_count {
            return Err(provider_failure("query-batch-invariant"));
        }
        emit_fact_plan(normalized.facts, plan, sink, cancellation)?;
        Ok(ExtractionReport {
            limited: partition_limited || embedded_limits.context || normalized.limited,
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

struct NativeParsedTree {
    tree: Tree,
    projection_nodes: usize,
    projection_depth: usize,
    projection_unavailable: bool,
    projection_changed: bool,
    proof_limit: Option<c_preprocessor::ProofLimit>,
}

impl NativeParsedTree {
    fn ordinary(tree: Tree) -> Self {
        Self {
            tree,
            projection_nodes: 0,
            projection_depth: 0,
            projection_unavailable: false,
            projection_changed: false,
            proof_limit: None,
        }
    }
}

struct PreparedParse {
    family: GrammarFamily,
    identity: ParseIdentity,
    tree: Tree,
    traversal: TraversalReport,
    c_macro_projected: bool,
    cache_nodes: usize,
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
            None,
            None,
            None,
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
            .field(
                "initialized_query_pack_count",
                &self.query_packs.initialized_count(),
            )
            .field(
                "initialized_query_pattern_count",
                &self.query_packs.initialized_pattern_count(),
            )
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
        source: Arc<Vec<u8>>,
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
            projection_policy: NATIVE_SOURCE_PROJECTION_POLICY,
            c_preprocessor_projection_policy: C_PREPROCESSOR_PROJECTION_POLICY,
            fortran_projection_policy: FORTRAN_TEMPLATE_PROJECTION_POLICY,
            cmake_header_projection_policy: CMAKE_HEADER_PROJECTION_POLICY,
            cmake_patch_projection_policy: CMAKE_PATCH_PROJECTION_POLICY,
            cmake_command_template_projection_policy: CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY,
            pug_scanner_contract: pug_scope::SCANNER_CONTRACT_IDENTITY,
            lean_scanner_contract: lean_scope::SCANNER_CONTRACT_IDENTITY,
            idris_scanner_contract: idris_scope::SCANNER_CONTRACT_IDENTITY,
            idris_frontend_policy: idris_scope::FRONTEND_POLICY,
            sed_scanner_contract: sed_scope::SCANNER_CONTRACT_IDENTITY,
            sed_frontend_policy: sed_scope::FRONTEND_POLICY,
            wolfram_scanner_contract: wolfram_scope::SCANNER_CONTRACT_IDENTITY,
            coffeescript_scanner_contract: coffeescript_scope::SCANNER_CONTRACT_IDENTITY,
            gdscript_scanner_contract: gdscript_scope::CACHE_CONTRACT_IDENTITY,
            dockerfile_scanner_contract: dockerfile_scope::SCANNER_CONTRACT_IDENTITY,
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
    projection_policy: &'static str,
    c_preprocessor_projection_policy: &'static str,
    fortran_projection_policy: &'static str,
    cmake_header_projection_policy: &'static str,
    cmake_patch_projection_policy: &'static str,
    cmake_command_template_projection_policy: &'static str,
    pug_scanner_contract: &'static str,
    lean_scanner_contract: &'static str,
    idris_scanner_contract: &'static str,
    idris_frontend_policy: &'static str,
    sed_scanner_contract: &'static str,
    sed_frontend_policy: &'static str,
    wolfram_scanner_contract: &'static str,
    coffeescript_scanner_contract: &'static str,
    gdscript_scanner_contract: &'static str,
    dockerfile_scanner_contract: &'static str,
    identity: ParseIdentity,
    source: Arc<Vec<u8>>,
    tree: Option<Tree>,
    accounted_bytes: usize,
    invalidation: Option<ReuseInvalidation>,
}

impl CachedParse {
    fn invalidation(invalidation: ReuseInvalidation) -> Self {
        Self {
            entry_id: 0,
            projection_policy: NATIVE_SOURCE_PROJECTION_POLICY,
            c_preprocessor_projection_policy: C_PREPROCESSOR_PROJECTION_POLICY,
            fortran_projection_policy: FORTRAN_TEMPLATE_PROJECTION_POLICY,
            cmake_header_projection_policy: CMAKE_HEADER_PROJECTION_POLICY,
            cmake_patch_projection_policy: CMAKE_PATCH_PROJECTION_POLICY,
            cmake_command_template_projection_policy: CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY,
            pug_scanner_contract: pug_scope::SCANNER_CONTRACT_IDENTITY,
            lean_scanner_contract: lean_scope::SCANNER_CONTRACT_IDENTITY,
            idris_scanner_contract: idris_scope::SCANNER_CONTRACT_IDENTITY,
            idris_frontend_policy: idris_scope::FRONTEND_POLICY,
            sed_scanner_contract: sed_scope::SCANNER_CONTRACT_IDENTITY,
            sed_frontend_policy: sed_scope::FRONTEND_POLICY,
            wolfram_scanner_contract: wolfram_scope::SCANNER_CONTRACT_IDENTITY,
            coffeescript_scanner_contract: coffeescript_scope::SCANNER_CONTRACT_IDENTITY,
            gdscript_scanner_contract: gdscript_scope::CACHE_CONTRACT_IDENTITY,
            dockerfile_scanner_contract: dockerfile_scope::SCANNER_CONTRACT_IDENTITY,
            identity: invalid_identity(),
            source: Arc::new(Vec::new()),
            tree: None,
            accounted_bytes: 0,
            invalidation: Some(invalidation),
        }
    }

    fn clone_for_reuse(&self) -> Self {
        Self {
            entry_id: self.entry_id,
            projection_policy: self.projection_policy,
            c_preprocessor_projection_policy: self.c_preprocessor_projection_policy,
            fortran_projection_policy: self.fortran_projection_policy,
            cmake_header_projection_policy: self.cmake_header_projection_policy,
            cmake_patch_projection_policy: self.cmake_patch_projection_policy,
            cmake_command_template_projection_policy: self.cmake_command_template_projection_policy,
            pug_scanner_contract: self.pug_scanner_contract,
            lean_scanner_contract: self.lean_scanner_contract,
            idris_scanner_contract: self.idris_scanner_contract,
            idris_frontend_policy: self.idris_frontend_policy,
            sed_scanner_contract: self.sed_scanner_contract,
            sed_frontend_policy: self.sed_frontend_policy,
            wolfram_scanner_contract: self.wolfram_scanner_contract,
            coffeescript_scanner_contract: self.coffeescript_scanner_contract,
            gdscript_scanner_contract: self.gdscript_scanner_contract,
            dockerfile_scanner_contract: self.dockerfile_scanner_contract,
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
        included_ranges: Arc::new(Vec::new()),
        settings: ParserSettings::new(1).expect("hard-coded parser setting is nonzero"),
        virtual_line_ending: false,
        cmake_header_path: false,
        line_endings: LineEndingPolicy::Preserve,
    }
}

fn needs_virtual_matlab_line_ending(request: &ParseRequest<'_>, source: &[u8]) -> bool {
    request.language().as_str() == "matlab"
        && request.included_ranges().is_empty()
        && source
            .last()
            .is_some_and(|last| !matches!(last, b'\n' | b'\r' | b';' | b','))
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
    try_reserve_exact_cancellable(
        &mut identities,
        edits.len(),
        cancellation,
        "incremental-identity-allocation",
    )?;
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
    if cached.c_preprocessor_projection_policy != C_PREPROCESSOR_PROJECTION_POLICY
        || cached.projection_policy != NATIVE_SOURCE_PROJECTION_POLICY
        || cached.fortran_projection_policy != FORTRAN_TEMPLATE_PROJECTION_POLICY
        || cached.cmake_header_projection_policy != CMAKE_HEADER_PROJECTION_POLICY
        || cached.cmake_patch_projection_policy != CMAKE_PATCH_PROJECTION_POLICY
        || cached.cmake_command_template_projection_policy
            != CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Dockerfile
        && cached.dockerfile_scanner_contract != dockerfile_scope::SCANNER_CONTRACT_IDENTITY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Wolfram
        && cached.wolfram_scanner_contract != wolfram_scope::SCANNER_CONTRACT_IDENTITY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Idris
        && (cached.idris_scanner_contract != idris_scope::SCANNER_CONTRACT_IDENTITY
            || cached.idris_frontend_policy != idris_scope::FRONTEND_POLICY
            || !edits.is_empty())
    {
        // The initial scanner admission qualifies exact no-op state, not edited snapshots.
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Sed
        && (cached.sed_scanner_contract != sed_scope::SCANNER_CONTRACT_IDENTITY
            || cached.sed_frontend_policy != sed_scope::FRONTEND_POLICY)
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if cached.identity.line_endings != current.line_endings {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Pug
        && cached.pug_scanner_contract != pug_scope::SCANNER_CONTRACT_IDENTITY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Lean
        && cached.lean_scanner_contract != lean_scope::SCANNER_CONTRACT_IDENTITY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Coffeescript
        && cached.coffeescript_scanner_contract != coffeescript_scope::SCANNER_CONTRACT_IDENTITY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Gdscript
        && cached.gdscript_scanner_contract != gdscript_scope::CACHE_CONTRACT_IDENTITY
    {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if cached.identity.family != current.family {
        return Ok((None, ReuseStatus::Invalidated(ReuseInvalidation::Language)));
    }
    if cached.identity.cmake_header_path != current.cmake_header_path {
        // Identical bytes may select different private inputs on .h.cmake paths.
        // A previous tree is reusable only within the same projection route.
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
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
    if cached.identity.virtual_line_ending != current.virtual_line_ending {
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::VirtualLineEnding),
        ));
    }
    if matches!(current.encoding.as_str(), "utf-16le" | "utf-16be") && !edits.is_empty() {
        // SourceEdit is explicitly sequential UTF-8. Applying it to physical
        // UTF-16 or to a projected tree would fabricate an edit contract.
        return Ok((None, ReuseStatus::Invalidated(ReuseInvalidation::Encoding)));
    }
    if current.line_endings != LineEndingPolicy::Preserve && !edits.is_empty() {
        // Physical edits do not describe projected tree offsets. Invalidate
        // before cloning/editing a tree, including no-CRLF files under this policy.
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Fortran
        && !edits.is_empty()
        && (fortran::needs_projection(current_source, cancellation)?
            || fortran::needs_projection(&cached.source, cancellation)?)
    {
        // Physical edits do not describe changes to a template parse view:
        // one marker may change the interpretation of later physical bytes.
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::CMake && !edits.is_empty() {
        // Changing one template or header marker may alter the parse view of
        // later bytes. Physical edits cannot prove equivalent projected edits.
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
        ));
    }
    if current.family == GrammarFamily::Jsonc
        && !edits.is_empty()
        && (!current.included_ranges.is_empty()
            || jsonc::needs_projection(current_source, cancellation)?
            || jsonc::needs_projection(&cached.source, cancellation)?)
    {
        // A comma or comment edit can change the parser-only mask beyond the
        // physical edit range. Fragmented input also omits host lexical state.
        // Rebuild rather than apply incomplete tree edits.
        return Ok((
            None,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
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
    if matches!(current.encoding.as_str(), "utf-16le" | "utf-16be")
        || current.line_endings != LineEndingPolicy::Preserve
    {
        if cached.source.len() != current_source.len() {
            return Ok((
                None,
                ReuseStatus::Invalidated(ReuseInvalidation::MissingEdits),
            ));
        }
        for (old, new) in cached
            .source
            .chunks(CANCELLATION_BYTE_INTERVAL)
            .zip(current_source.chunks(CANCELLATION_BYTE_INTERVAL))
        {
            cancellation.check()?;
            if old != new {
                return Ok((
                    None,
                    ReuseStatus::Invalidated(ReuseInvalidation::MissingEdits),
                ));
            }
        }
        cancellation.check()?;
        return Ok((Some(tree), ReuseStatus::Reused { changed_ranges: 0 }));
    }
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
    validate_utf8_cancellable(old_source, cancellation).map_err(ApplyEditError::Fatal)?;
    let mut source =
        copy_bytes_cancellable(old_source, cancellation, "incremental-source-allocation")
            .map_err(ApplyEditError::Fatal)?;
    let mut scratch = Vec::new();
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
        if !is_utf8_char_boundary(&source, edit.start_byte())
            || !is_utf8_char_boundary(&source, edit.old_end_byte())
        {
            return Err(ReuseInvalidation::EditNotCharacterBoundary.into());
        }
        let (start_position, old_end_position) = points_for_offsets(
            &source,
            edit.start_byte(),
            edit.old_end_byte(),
            cancellation,
        )
        .map_err(ApplyEditError::Fatal)?
        .ok_or(ReuseInvalidation::EditOutsideSource)?;
        let replacement_end =
            point_after_replacement(start_position, edit.replacement(), cancellation)
                .map_err(ApplyEditError::Fatal)?;
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
        rebuild_edited_source(
            &source,
            &mut scratch,
            edit,
            intermediate_bytes,
            cancellation,
        )
        .map_err(ApplyEditError::Fatal)?;
        tree.edit(&InputEdit {
            start_byte: edit.start_byte(),
            old_end_byte: edit.old_end_byte(),
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position: replacement_end,
        });
        std::mem::swap(&mut source, &mut scratch);
    }
    cancellation
        .check()
        .map_err(AdapterError::from)
        .map_err(ApplyEditError::Fatal)?;
    if bytes_equal_cancellable(&source, new_source, cancellation).map_err(ApplyEditError::Fatal)? {
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

fn point_after_replacement(
    start: Point,
    replacement: &[u8],
    cancellation: &Cancellation,
) -> Result<Point, AdapterError> {
    let mut row = start.row;
    let mut column = start.column;
    for_each_byte_chunk(replacement, cancellation, |chunk| {
        for byte in chunk {
            if *byte == b'\n' {
                row = row
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("incremental-point"))?;
                column = 0;
            } else {
                column = column
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("incremental-point"))?;
            }
        }
        Ok(())
    })?;
    Ok(Point { row, column })
}

fn points_for_offsets(
    source: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<Option<(Point, Point)>, AdapterError> {
    let prefix = match source.get(..end) {
        Some(prefix) if start <= end => prefix,
        _ => return Ok(None),
    };
    let mut row = 0usize;
    let mut column = 0usize;
    let mut offset = 0usize;
    let mut start_point = (start == 0).then_some(Point { row, column });
    for chunk in prefix.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        for byte in chunk {
            if offset == start {
                start_point = Some(Point { row, column });
            }
            if *byte == b'\n' {
                row = row
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("incremental-point"))?;
                column = 0;
            } else {
                column = column
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("incremental-point"))?;
            }
            offset = offset
                .checked_add(1)
                .ok_or_else(|| provider_failure("incremental-point"))?;
        }
    }
    if offset == start {
        start_point = Some(Point { row, column });
    }
    cancellation.check()?;
    Ok(start_point.map(|start_point| (start_point, Point { row, column })))
}

fn rebuild_edited_source(
    source: &[u8],
    scratch: &mut Vec<u8>,
    edit: &SourceEdit,
    expected_length: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    scratch.clear();
    try_reserve_exact_cancellable(
        scratch,
        expected_length,
        cancellation,
        "incremental-source-allocation",
    )?;
    append_bytes_cancellable(
        scratch,
        source
            .get(..edit.start_byte())
            .ok_or_else(|| provider_failure("incremental-source-range"))?,
        cancellation,
    )?;
    append_bytes_cancellable(scratch, edit.replacement(), cancellation)?;
    append_bytes_cancellable(
        scratch,
        source
            .get(edit.old_end_byte()..)
            .ok_or_else(|| provider_failure("incremental-source-range"))?,
        cancellation,
    )?;
    if scratch.len() != expected_length {
        return Err(provider_failure("incremental-source-accounting"));
    }
    Ok(())
}

fn copy_bytes_cancellable(
    source: &[u8],
    cancellation: &Cancellation,
    allocation_code: &'static str,
) -> Result<Vec<u8>, AdapterError> {
    cancellation.check()?;
    let mut copy = Vec::new();
    try_reserve_exact_cancellable(&mut copy, source.len(), cancellation, allocation_code)?;
    append_bytes_cancellable(&mut copy, source, cancellation)?;
    Ok(copy)
}

fn append_bytes_cancellable(
    destination: &mut Vec<u8>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    for_each_byte_chunk(source, cancellation, |chunk| {
        destination.extend_from_slice(chunk);
        Ok(())
    })
}

fn for_each_byte_chunk(
    source: &[u8],
    cancellation: &Cancellation,
    mut visit: impl FnMut(&[u8]) -> Result<(), AdapterError>,
) -> Result<(), AdapterError> {
    for chunk in source.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        visit(chunk)?;
    }
    cancellation.check()?;
    Ok(())
}

fn bytes_equal_cancellable(
    left: &[u8],
    right: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .chunks(CANCELLATION_BYTE_INTERVAL)
        .zip(right.chunks(CANCELLATION_BYTE_INTERVAL))
    {
        cancellation.check()?;
        if left != right {
            return Ok(false);
        }
    }
    cancellation.check()?;
    Ok(true)
}

fn count_changed_ranges(
    old_tree: &Tree,
    new_tree: &Tree,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    cancellation.check()?;
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
) -> Result<Arc<Vec<u8>>, AdapterError> {
    let copy = copy_bytes_cancellable(source, cancellation, "cache-allocation")?;
    Ok(Arc::new(copy))
}

fn tree_sitter_ranges(
    request: &ParseRequest<'_>,
    source: &[u8],
    view: &NativeSourceView<'_>,
    origin: Option<(usize, Point)>,
    cancellation: &Cancellation,
) -> Result<Vec<Range>, AdapterError> {
    cancellation.check()?;
    let mut ranges = Vec::new();
    try_reserve_exact_cancellable(
        &mut ranges,
        request.included_ranges().len(),
        cancellation,
        "range-allocation",
    )?;
    let (mut cursor, mut point) = origin.unwrap_or((0, Point { row: 0, column: 0 }));
    for included in request.included_ranges() {
        cancellation.check()?;
        let span = view.projected_span(included.span())?;
        let start =
            usize::try_from(span.start_byte()).map_err(|_| provider_failure("range-offset"))?;
        let end = usize::try_from(span.end_byte()).map_err(|_| provider_failure("range-offset"))?;
        if start < cursor
            || !is_utf8_char_boundary(source, start)
            || !is_utf8_char_boundary(source, end)
        {
            return Err(provider_failure("range-boundary"));
        }
        advance_source_point(source, &mut cursor, start, &mut point, cancellation)?;
        let start_point = point;
        advance_source_point(source, &mut cursor, end, &mut point, cancellation)?;
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

fn validate_utf8_cancellable(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut offset = 0usize;
    while offset < source.len() {
        cancellation.check()?;
        let end = offset
            .checked_add(CANCELLATION_BYTE_INTERVAL)
            .unwrap_or(source.len())
            .min(source.len());
        match std::str::from_utf8(
            source
                .get(offset..end)
                .ok_or_else(|| provider_failure("invalid-utf8"))?,
        ) {
            Ok(_) => offset = end,
            Err(error) if error.error_len().is_some() => {
                return Err(provider_failure("invalid-utf8"));
            }
            Err(error) => {
                offset = offset
                    .checked_add(error.valid_up_to())
                    .ok_or_else(|| provider_failure("invalid-utf8"))?;
                if end == source.len() || error.valid_up_to() == 0 {
                    return Err(provider_failure("invalid-utf8"));
                }
            }
        }
    }
    cancellation.check()?;
    Ok(())
}

fn is_utf8_char_boundary(source: &[u8], index: usize) -> bool {
    // Callers validate the complete buffer first, so only continuation bytes
    // can identify non-boundaries.
    index == source.len()
        || source
            .get(index)
            .is_some_and(|byte| !matches!(*byte, 0x80..=0xbf))
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

fn validate_included_range_languages(
    ranges: &[IncludedRange],
    language: &LanguageId,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    for (index, range) in ranges.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if range.language() != language {
            return Err(provider_failure("included-language"));
        }
    }
    cancellation.check()?;
    Ok(())
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
struct ExtractionReport {
    limited: bool,
}

// A context cap (ranges, nodes or native work) does not depend on the syntax
// record partition. Keep both causes when they overlap so replay cannot hide
// query or sink clipping behind an unrelated embedded-resource diagnostic.
#[derive(Debug, Default, Clone, Copy)]
struct EmbeddedExtractionLimits {
    context: bool,
    partition: bool,
}

impl EmbeddedExtractionLimits {
    fn merge(&mut self, other: Self) {
        self.context |= other.context;
        self.partition |= other.partition;
    }
}

#[derive(Debug)]
struct NormalizedFacts {
    facts: Vec<SyntaxFact>,
    required: usize,
    limited: bool,
}

struct NormalizedCandidates {
    candidates: Vec<QueryCandidate>,
    required: usize,
    limited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CandidateRetention {
    required: usize,
    limited: bool,
}

#[derive(Debug)]
struct FactDraft {
    start: usize,
    end: usize,
    role: StructuralRole,
    syntax_kind: SyntaxKindLabel,
    parent: Option<usize>,
    depth: usize,
    native_depth: usize,
}

#[derive(Debug)]
struct FactPlan {
    batches: Vec<std::ops::Range<usize>>,
    emitted: usize,
}

fn normalize_query_candidates(
    candidates: Vec<QueryCandidate>,
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
    max_facts: usize,
    cancellation: &Cancellation,
) -> Result<NormalizedFacts, AdapterError> {
    let normalized =
        prepare_query_candidates(candidates, request, source, max_facts, cancellation)?;
    materialize_query_candidates(normalized, request, source, cancellation)
}

fn prepare_query_candidates(
    mut candidates: Vec<QueryCandidate>,
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
    max_facts: usize,
    cancellation: &Cancellation,
) -> Result<NormalizedCandidates, AdapterError> {
    if request.language().as_str() == "perl" && request.included_ranges().is_empty() {
        for candidate in &mut candidates {
            cancellation.check()?;
            if candidate.syntax == "perl.file" && candidate.native_depth == 0 {
                // The native root excludes leading trivia, but a whole-file binding
                // context covers the snapshot. Included/embedded modules stay scoped.
                candidate.start = 0;
                candidate.end = source.as_bytes().len();
            }
        }
    }
    sort_cancellable_by(&mut candidates, cancellation, |left, right| {
        (
            left.start,
            left.end,
            left.role,
            left.syntax,
            left.native_depth,
        )
            .cmp(&(
                right.start,
                right.end,
                right.role,
                right.syntax,
                right.native_depth,
            ))
    })?;
    dedup_query_candidates(&mut candidates, cancellation)?;
    remove_shadowed_candidates(&mut candidates, cancellation)?;
    mark_required_scope_closure(&mut candidates, cancellation)?;
    // Included ranges can remove declarations and their enclosing scopes. Keep
    // every mandatory candidate until that restriction establishes the exact
    // demand, while pruning optional work to the caller's bounded capacity.
    let mut limited =
        prune_optional_candidates_within_limit(&mut candidates, max_facts, cancellation)?.limited;
    sort_cancellable_by(&mut candidates, cancellation, |left, right| {
        (
            left.start,
            left.end,
            left.role,
            left.syntax,
            left.native_depth,
        )
            .cmp(&(
                right.start,
                right.end,
                right.role,
                right.syntax,
                right.native_depth,
            ))
    })?;
    let mut root_count = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if candidate.role == StructuralRole::Root {
            root_count = root_count
                .checked_add(1)
                .ok_or_else(|| provider_failure("query-fact-accounting"))?;
        }
    }
    let range_count = request.included_ranges().len();
    let additional_ranges = if range_count == 0 { 0 } else { range_count - 1 };
    let expanded_roots = root_count
        .checked_mul(additional_ranges)
        .ok_or_else(|| provider_failure("query-fact-accounting"))?;
    let maximum_expansion = candidates
        .len()
        .checked_add(expanded_roots)
        .ok_or_else(|| provider_failure("query-fact-accounting"))?;
    let mut restricted = Vec::new();
    try_reserve_exact_cancellable(
        &mut restricted,
        maximum_expansion,
        cancellation,
        "query-fact-allocation",
    )?;
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if request.included_ranges().is_empty() {
            restricted.push(candidate);
            continue;
        }
        if candidate.role == StructuralRole::Root {
            for included in request.included_ranges() {
                if restricted.len().is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                    cancellation.check()?;
                }
                let span = source.projected_span(included.span())?;
                let expanded = QueryCandidate {
                    start: usize::try_from(span.start_byte())
                        .map_err(|_| provider_failure("query-span"))?,
                    end: usize::try_from(span.end_byte())
                        .map_err(|_| provider_failure("query-span"))?,
                    ..candidate
                };
                restricted.push(expanded);
            }
        } else if candidate_within_included_range(&candidate, request, source)? {
            restricted.push(candidate);
        }
    }
    sort_cancellable_by(&mut restricted, cancellation, |left, right| {
        (
            left.start,
            left.end,
            left.role,
            left.syntax,
            left.native_depth,
        )
            .cmp(&(
                right.start,
                right.end,
                right.role,
                right.syntax,
                right.native_depth,
            ))
    })?;
    dedup_query_candidates(&mut restricted, cancellation)?;
    mark_required_scope_closure(&mut restricted, cancellation)?;
    let retention =
        retain_required_candidates_within_limit(&mut restricted, max_facts, cancellation)?;
    limited |= retention.limited;
    sort_cancellable_by(&mut restricted, cancellation, |left, right| {
        (
            left.start,
            left.end,
            left.role,
            left.syntax,
            left.native_depth,
        )
            .cmp(&(
                right.start,
                right.end,
                right.role,
                right.syntax,
                right.native_depth,
            ))
    })?;

    Ok(NormalizedCandidates {
        candidates: restricted,
        required: retention.required,
        limited,
    })
}

fn candidate_syntax_base(candidate: QueryCandidate) -> &'static str {
    match (candidate.syntax, candidate.role) {
        ("python.module", StructuralRole::Module) => "python.file",
        ("javascript.program", StructuralRole::Module) => "javascript.file",
        ("typescript.program", StructuralRole::Module) => "typescript.file",
        _ => candidate.syntax,
    }
}

fn materialize_query_candidates(
    normalized: NormalizedCandidates,
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
    cancellation: &Cancellation,
) -> Result<NormalizedFacts, AdapterError> {
    let mut drafts = Vec::new();
    try_reserve_exact_cancellable(
        &mut drafts,
        normalized.candidates.len(),
        cancellation,
        "query-fact-allocation",
    )?;
    for (index, candidate) in normalized.candidates.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let base = candidate_syntax_base(candidate);
        let label = format!("{base}.{}", candidate.role.label());
        drafts.push(FactDraft {
            start: candidate.start,
            end: candidate.end,
            role: candidate.role,
            syntax_kind: SyntaxKindLabel::new(&label)
                .map_err(|_| provider_failure("query-syntax-label"))?,
            parent: None,
            depth: 0,
            native_depth: candidate.native_depth,
        });
    }
    assign_fact_parents(&mut drafts, cancellation)?;
    let mut facts = Vec::new();
    try_reserve_exact_cancellable(
        &mut facts,
        drafts.len(),
        cancellation,
        "query-fact-allocation",
    )?;
    for (index, draft) in drafts.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let local_id = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| provider_failure("query-fact-identity"))?;
        let parent = draft
            .parent
            .map(|parent| {
                u64::try_from(parent)
                    .ok()
                    .and_then(|parent| parent.checked_add(1))
                    .ok_or_else(|| provider_failure("query-fact-identity"))
            })
            .transpose()?;
        let start = u64::try_from(draft.start).map_err(|_| provider_failure("query-span"))?;
        let end = u64::try_from(draft.end).map_err(|_| provider_failure("query-span"))?;
        let projected = SourceSpan::new(request.source().source_ref().span().file(), start, end)
            .map_err(|_| provider_failure("query-span"))?;
        let span = source.physical_span(projected)?;
        facts.push(SyntaxFact::new(
            local_id,
            parent,
            draft.role.fact_kind(),
            span,
            draft.depth,
            draft.syntax_kind,
        ));
    }
    cancellation.check()?;
    Ok(NormalizedFacts {
        facts,
        required: normalized.required,
        limited: normalized.limited,
    })
}

pub(super) fn sort_cancellable_by<T: Copy>(
    values: &mut Vec<T>,
    cancellation: &Cancellation,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering + Copy,
) -> Result<(), AdapterError> {
    for chunk in values.chunks_mut(CANCELLATION_CHECK_INTERVAL) {
        cancellation.check()?;
        chunk.sort_unstable_by(compare);
    }
    if values.len() <= CANCELLATION_CHECK_INTERVAL {
        cancellation.check()?;
        return Ok(());
    }

    cancellation.check()?;
    let mut scratch = Vec::new();
    try_reserve_exact_cancellable(
        &mut scratch,
        values.len(),
        cancellation,
        "query-sort-allocation",
    )?;
    let mut width = CANCELLATION_CHECK_INTERVAL;
    while width < values.len() {
        cancellation.check()?;
        scratch.clear();
        let run_width = width
            .checked_mul(2)
            .unwrap_or(values.len())
            .min(values.len());
        let mut run_start = 0usize;
        while run_start < values.len() {
            cancellation.check()?;
            let middle = run_start.saturating_add(width).min(values.len());
            let run_end = run_start.saturating_add(run_width).min(values.len());
            let left = values
                .get(run_start..middle)
                .ok_or_else(|| provider_failure("query-sort-invariant"))?;
            let right = values
                .get(middle..run_end)
                .ok_or_else(|| provider_failure("query-sort-invariant"))?;
            merge_sorted_runs(left, right, &mut scratch, cancellation, compare)?;
            run_start = run_end;
        }
        std::mem::swap(values, &mut scratch);
        width = run_width;
    }
    cancellation.check()?;
    Ok(())
}

fn merge_sorted_runs<T: Copy>(
    left_values: &[T],
    right_values: &[T],
    output: &mut Vec<T>,
    cancellation: &Cancellation,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) -> Result<(), AdapterError> {
    let mut left = 0usize;
    let mut right = 0usize;
    let mut merged = 0usize;
    while left < left_values.len() && right < right_values.len() {
        if merged.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let left_value = *left_values
            .get(left)
            .ok_or_else(|| provider_failure("query-sort-invariant"))?;
        let right_value = *right_values
            .get(right)
            .ok_or_else(|| provider_failure("query-sort-invariant"))?;
        if compare(&left_value, &right_value).is_le() {
            output.push(left_value);
            left += 1;
        } else {
            output.push(right_value);
            right += 1;
        }
        merged += 1;
    }
    for value in &left_values[left..] {
        if merged.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        output.push(*value);
        merged += 1;
    }
    for value in &right_values[right..] {
        if merged.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        output.push(*value);
        merged += 1;
    }
    Ok(())
}

fn dedup_query_candidates(
    candidates: &mut Vec<QueryCandidate>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    if candidates.len() < 2 {
        cancellation.check()?;
        return Ok(());
    }
    let mut write = 1usize;
    for read in 1..candidates.len() {
        if read.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let candidate = *candidates
            .get(read)
            .ok_or_else(|| provider_failure("query-dedup-invariant"))?;
        let duplicate = candidates
            .get(write - 1)
            .is_some_and(|previous| previous.same_fact(candidate));
        if duplicate {
            let previous = candidates
                .get_mut(write - 1)
                .ok_or_else(|| provider_failure("query-dedup-invariant"))?;
            previous.required |= candidate.required;
        } else {
            let slot = candidates
                .get_mut(write)
                .ok_or_else(|| provider_failure("query-dedup-invariant"))?;
            *slot = candidate;
            write = write
                .checked_add(1)
                .ok_or_else(|| provider_failure("query-dedup-invariant"))?;
        }
    }
    candidates.truncate(write);
    cancellation.check()?;
    Ok(())
}

fn remove_shadowed_candidates(
    candidates: &mut Vec<QueryCandidate>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    // Most definition/reference and documentation/comment captures at one span
    // are redundant. Nix inherit instead both defines an attribute and reads an
    // outer binding or a source-set field. Perl's written package name also
    // carries unresolved package-context evidence; these roles must survive
    // independently with the same span.
    cancellation.check()?;
    let mut group_start = 0usize;
    let mut write = 0usize;
    while let Some(first) = candidates.get(group_start).copied() {
        cancellation.check()?;
        let mut group_end = group_start;
        let mut has_definition = false;
        let mut has_documentation = false;
        while let Some(candidate) = candidates.get(group_end) {
            if candidate.start != first.start || candidate.end != first.end {
                break;
            }
            if group_end.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                cancellation.check()?;
            }
            has_definition |= matches!(
                candidate.role,
                StructuralRole::Definition | StructuralRole::DefinitionPart
            );
            has_documentation |= candidate.role == StructuralRole::Documentation;
            group_end += 1;
        }
        for read in group_start..group_end {
            if read.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                cancellation.check()?;
            }
            let candidate = *candidates
                .get(read)
                .ok_or_else(|| provider_failure("query-shadow-invariant"))?;
            if !(has_definition
                && candidate.role == StructuralRole::Reference
                && !matches!(
                    candidate.syntax,
                    "nix.inherited_name" | "nix.inherited_attribute" | "perl.package_context"
                )
                || has_documentation && candidate.role == StructuralRole::Comment)
            {
                *candidates
                    .get_mut(write)
                    .ok_or_else(|| provider_failure("query-shadow-invariant"))? = candidate;
                write += 1;
            }
        }
        group_start = group_end;
    }
    candidates.truncate(write);
    cancellation.check()?;
    Ok(())
}

fn mark_required_scope_closure(
    candidates: &mut [QueryCandidate],
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let declaration_count = candidates
        .iter()
        .filter(|candidate| candidate.required && candidate.role == StructuralRole::Declaration)
        .count();
    if declaration_count == 0 {
        cancellation.check()?;
        return Ok(());
    }

    let mut declarations = Vec::new();
    try_reserve_exact_cancellable(
        &mut declarations,
        declaration_count,
        cancellation,
        "query-identity-closure-allocation",
    )?;
    for (index, candidate) in candidates.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if candidate.required && candidate.role == StructuralRole::Declaration {
            declarations.push((candidate.start, candidate.end));
        }
    }
    sort_cancellable_by(&mut declarations, cancellation, Ord::cmp)?;

    let leaf_count = declarations
        .len()
        .checked_next_power_of_two()
        .ok_or_else(|| provider_failure("query-identity-closure-accounting"))?;
    let tree_len = leaf_count
        .checked_mul(2)
        .ok_or_else(|| provider_failure("query-identity-closure-accounting"))?;
    let mut minimum_ends = Vec::new();
    try_reserve_exact_cancellable(
        &mut minimum_ends,
        tree_len,
        cancellation,
        "query-identity-closure-allocation",
    )?;
    minimum_ends.resize(tree_len, usize::MAX);
    for (index, (_, end)) in declarations.iter().copied().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let slot = leaf_count
            .checked_add(index)
            .ok_or_else(|| provider_failure("query-identity-closure-accounting"))?;
        *minimum_ends
            .get_mut(slot)
            .ok_or_else(|| provider_failure("query-identity-closure-invariant"))? = end;
    }
    for index in (1..leaf_count).rev() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let left = *minimum_ends
            .get(index * 2)
            .ok_or_else(|| provider_failure("query-identity-closure-invariant"))?;
        let right = *minimum_ends
            .get(index * 2 + 1)
            .ok_or_else(|| provider_failure("query-identity-closure-invariant"))?;
        *minimum_ends
            .get_mut(index)
            .ok_or_else(|| provider_failure("query-identity-closure-invariant"))? = left.min(right);
    }

    for (index, candidate) in candidates.iter_mut().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if candidate.role != StructuralRole::Scope {
            continue;
        }
        let first = declarations.partition_point(|(start, _)| *start < candidate.start);
        let after_last = declarations.partition_point(|(start, _)| *start <= candidate.end);
        candidate.required |= first < after_last
            && minimum_in_range(&minimum_ends, leaf_count, first, after_last)? <= candidate.end;
    }
    cancellation.check()?;
    Ok(())
}

fn minimum_in_range(
    tree: &[usize],
    leaf_count: usize,
    mut start: usize,
    mut end: usize,
) -> Result<usize, AdapterError> {
    start = start
        .checked_add(leaf_count)
        .ok_or_else(|| provider_failure("query-identity-closure-accounting"))?;
    end = end
        .checked_add(leaf_count)
        .ok_or_else(|| provider_failure("query-identity-closure-accounting"))?;
    let mut minimum = usize::MAX;
    while start < end {
        if !start.is_multiple_of(2) {
            minimum = minimum.min(
                *tree
                    .get(start)
                    .ok_or_else(|| provider_failure("query-identity-closure-invariant"))?,
            );
            start += 1;
        }
        if !end.is_multiple_of(2) {
            end -= 1;
            minimum = minimum.min(
                *tree
                    .get(end)
                    .ok_or_else(|| provider_failure("query-identity-closure-invariant"))?,
            );
        }
        start /= 2;
        end /= 2;
    }
    Ok(minimum)
}

fn retain_required_candidates_within_limit(
    candidates: &mut Vec<QueryCandidate>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<CandidateRetention, AdapterError> {
    let retention = prune_optional_candidates_within_limit(candidates, maximum, cancellation)?;
    if retention.required > maximum {
        return Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: retention.required,
            limit: maximum,
        }));
    }
    Ok(retention)
}

fn prune_optional_candidates_within_limit(
    candidates: &mut Vec<QueryCandidate>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<CandidateRetention, AdapterError> {
    let mut required = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if candidate.required {
            required = required
                .checked_add(1)
                .ok_or_else(|| provider_failure("query-identity-closure-accounting"))?;
        }
    }
    let retained = maximum.max(required);
    if candidates.len() <= retained {
        cancellation.check()?;
        return Ok(CandidateRetention {
            required,
            limited: false,
        });
    }
    sort_cancellable_by(candidates, cancellation, |left, right| {
        left.selection_rank().cmp(&right.selection_rank())
    })?;
    candidates.truncate(retained);
    cancellation.check()?;
    Ok(CandidateRetention {
        required,
        limited: true,
    })
}

fn candidate_within_included_range(
    candidate: &QueryCandidate,
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
) -> Result<bool, AdapterError> {
    let start = u64::try_from(source.to_physical_offset(candidate.start)?)
        .map_err(|_| provider_failure("query-span"))?;
    let end = u64::try_from(source.to_physical_offset(candidate.end)?)
        .map_err(|_| provider_failure("query-span"))?;
    let ranges = request.included_ranges();
    let index = ranges.partition_point(|included| included.span().end_byte() <= start);
    Ok(ranges.get(index).is_some_and(|included| {
        let span = included.span();
        start >= span.start_byte() && end <= span.end_byte()
    }))
}

fn assign_fact_parents(
    drafts: &mut [FactDraft],
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    let mut order = Vec::new();
    try_reserve_exact_cancellable(
        &mut order,
        drafts.len(),
        cancellation,
        "query-parent-allocation",
    )?;
    for index in 0..drafts.len() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        order.push(index);
    }
    sort_cancellable_by(&mut order, cancellation, |&left, &right| {
        (
            drafts[left].start,
            std::cmp::Reverse(drafts[left].end),
            drafts[left].native_depth,
            hierarchy_rank(drafts[left].role),
            drafts[left].role,
            drafts[left].syntax_kind.as_str(),
        )
            .cmp(&(
                drafts[right].start,
                std::cmp::Reverse(drafts[right].end),
                drafts[right].native_depth,
                hierarchy_rank(drafts[right].role),
                drafts[right].role,
                drafts[right].syntax_kind.as_str(),
            ))
    })?;
    let mut active = Vec::<usize>::new();
    for (position, index) in order.into_iter().enumerate() {
        if position.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let mut inspected = 0usize;
        // Cross-region metadata must not close the native container stack.
        if drafts[index].role.container_rank().is_some() {
            while active.last().is_some_and(|&parent| {
                !span_contains(
                    drafts[parent].start,
                    drafts[parent].end,
                    drafts[index].start,
                    drafts[index].end,
                )
            }) {
                if inspected.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                    cancellation.check()?;
                }
                active.pop();
                inspected = inspected
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("query-parent-accounting"))?;
            }
        }
        let mut parent = None;
        for candidate in active.iter().rev().copied() {
            if inspected.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                cancellation.check()?;
            }
            inspected = inspected
                .checked_add(1)
                .ok_or_else(|| provider_failure("query-parent-accounting"))?;
            if parent_is_valid(&drafts[candidate], &drafts[index]) {
                parent = Some(candidate);
                break;
            }
        }
        let depth = parent
            .map(|parent| {
                drafts[parent]
                    .depth
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("query-depth-accounting"))
            })
            .transpose()?
            .unwrap_or(0);
        drafts[index].parent = parent;
        drafts[index].depth = depth;
        if drafts[index].role.container_rank().is_some() {
            try_reserve_cancellable(&mut active, 1, cancellation, "query-parent-allocation")?;
            active.push(index);
        }
    }
    cancellation.check()?;
    Ok(())
}

const fn hierarchy_rank(role: StructuralRole) -> u8 {
    match role.container_rank() {
        Some(rank) => rank,
        None => 4,
    }
}

fn parent_is_valid(parent: &FactDraft, child: &FactDraft) -> bool {
    let Some(parent_rank) = parent.role.container_rank() else {
        return false;
    };
    if !span_contains(parent.start, parent.end, child.start, child.end) {
        return false;
    }
    let strict = parent.start < child.start || parent.end > child.end;
    strict
        || parent.native_depth < child.native_depth
        || (parent.native_depth == child.native_depth && parent_rank < hierarchy_rank(child.role))
}

const fn span_contains(
    outer_start: usize,
    outer_end: usize,
    inner_start: usize,
    inner_end: usize,
) -> bool {
    outer_start <= inner_start && outer_end >= inner_end
}

#[derive(Default, Clone, Copy)]
struct CandidatePacking {
    records: usize,
    output_bytes: usize,
    string_bytes: usize,
    batches: usize,
    batch_records: usize,
    batch_output_bytes: usize,
}

impl CandidatePacking {
    fn push(&mut self, usage: FactUsage, budget: RemainingBudget) -> Result<(), AdapterError> {
        let batch = budget.batch();
        let next_records = checked_query_add(self.batch_records, 1)?;
        let next_output = checked_query_add(self.batch_output_bytes, usage.output_bytes)?;
        if self.batch_records == 0
            || next_records > batch.max_records()
            || next_output > batch.max_output_bytes()
        {
            self.batches = checked_query_add(self.batches, 1)?;
            self.batch_records = 0;
            self.batch_output_bytes = 0;
        }
        self.batch_records = checked_query_add(self.batch_records, 1)?;
        self.batch_output_bytes = checked_query_add(self.batch_output_bytes, usage.output_bytes)?;
        self.records = checked_query_add(self.records, 1)?;
        self.output_bytes = checked_query_add(self.output_bytes, usage.output_bytes)?;
        self.string_bytes = checked_query_add(self.string_bytes, usage.string_bytes)?;
        Ok(())
    }

    fn fits(self, budget: RemainingBudget) -> bool {
        let remaining = budget.remaining();
        let batch = budget.batch();
        self.records <= remaining.records()
            && self.output_bytes <= remaining.output_bytes()
            && self.string_bytes <= remaining.string_bytes()
            && self.batches <= remaining.batches()
            && self.batch_records <= batch.max_records()
            && self.batch_output_bytes <= batch.max_output_bytes()
    }

    fn validate_required(self, budget: RemainingBudget) -> Result<(), AdapterError> {
        let remaining = budget.remaining();
        let batch = budget.batch();
        for (resource, observed, limit) in [
            (
                ResourceKind::RequiredSyntaxFacts,
                self.records,
                remaining.records(),
            ),
            (
                ResourceKind::OutputBytes,
                self.batch_output_bytes,
                batch.max_output_bytes(),
            ),
            (
                ResourceKind::OutputBytes,
                self.output_bytes,
                remaining.output_bytes(),
            ),
            (
                ResourceKind::StringBytes,
                self.string_bytes,
                remaining.string_bytes(),
            ),
            (ResourceKind::Batches, self.batches, remaining.batches()),
        ] {
            if observed > limit {
                return Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource,
                    observed,
                    limit,
                }));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct CandidateBatch {
    records: usize,
    output_bytes: usize,
}

#[derive(Clone, Copy)]
struct CandidateAdmission {
    candidate: QueryCandidate,
    usage: FactUsage,
    batch: usize,
    selected: bool,
}

fn checked_query_add(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_add(right)
        .ok_or_else(|| provider_failure("query-fact-accounting"))
}

fn candidate_usage(candidate: QueryCandidate) -> Result<FactUsage, AdapterError> {
    let string_bytes = checked_query_add(
        checked_query_add(candidate_syntax_base(candidate).len(), 1)?,
        candidate.role.label().len(),
    )?;
    Ok(FactUsage {
        output_bytes: checked_query_add(LOGICAL_SYNTAX_FACT_BYTES, string_bytes)?,
        string_bytes,
    })
}

fn candidate_source_key(
    candidate: QueryCandidate,
) -> (usize, usize, StructuralRole, &'static str, usize) {
    (
        candidate.start,
        candidate.end,
        candidate.role,
        candidate.syntax,
        candidate.native_depth,
    )
}

fn candidates_in_source_order(
    candidates: &[QueryCandidate],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    for (index, pair) in candidates.windows(2).enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if candidate_source_key(pair[0]) > candidate_source_key(pair[1]) {
            return Ok(false);
        }
    }
    cancellation.check()?;
    Ok(true)
}

fn admit_exact_query_candidates(
    candidates: &mut Vec<QueryCandidate>,
    budget: RemainingBudget,
    cancellation: &Cancellation,
) -> Result<CandidateRetention, AdapterError> {
    // Prepared candidates are ordered; avoid allocating a merge buffer for the full-fit path.
    if !candidates_in_source_order(candidates, cancellation)? {
        sort_cancellable_by(candidates, cancellation, |left, right| {
            candidate_source_key(*left).cmp(&candidate_source_key(*right))
        })?;
    }
    let mut required = 0usize;
    let mut all = CandidatePacking::default();
    let mut all_fit = true;
    for (index, candidate) in candidates.iter().copied().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if candidate.required {
            required = checked_query_add(required, 1)?;
        }
        if all_fit {
            all.push(candidate_usage(candidate)?, budget)?;
            all_fit = all.fits(budget);
        }
    }
    if all_fit {
        cancellation.check()?;
        return Ok(CandidateRetention {
            required,
            limited: false,
        });
    }

    let mut entries = Vec::new();
    try_reserve_exact_cancellable(
        &mut entries,
        candidates.len(),
        cancellation,
        "query-admission-allocation",
    )?;
    let mut batches = Vec::<CandidateBatch>::new();
    try_reserve_exact_cancellable(
        &mut batches,
        required.min(budget.remaining().batches()),
        cancellation,
        "query-admission-allocation",
    )?;
    let mut admitted = CandidatePacking::default();
    for (index, candidate) in candidates.iter().copied().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let usage = candidate_usage(candidate)?;
        let mut slot = 0;
        if candidate.required {
            admitted.push(usage, budget)?;
            admitted.validate_required(budget)?;
            slot = admitted
                .batches
                .checked_sub(1)
                .ok_or_else(|| provider_failure("query-batch-invariant"))?;
            if batches.len() < admitted.batches {
                batches.push(CandidateBatch::default());
            }
            let batch = batches
                .get_mut(slot)
                .ok_or_else(|| provider_failure("query-batch-invariant"))?;
            batch.records = checked_query_add(batch.records, 1)?;
            batch.output_bytes = checked_query_add(batch.output_bytes, usage.output_bytes)?;
        }
        entries.push(CandidateAdmission {
            candidate,
            usage,
            batch: slot,
            selected: candidate.required,
        });
    }
    if batches.is_empty() && budget.remaining().batches() > 0 {
        try_reserve_cancellable(&mut batches, 1, cancellation, "query-admission-allocation")?;
        batches.push(CandidateBatch::default());
    }
    let mut following_batch = batches.len().saturating_sub(1);
    for (index, entry) in entries.iter_mut().rev().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if entry.candidate.required {
            following_batch = entry.batch;
        } else {
            entry.batch = following_batch;
        }
    }
    sort_cancellable_by(&mut entries, cancellation, |left, right| {
        left.candidate
            .selection_rank()
            .cmp(&right.candidate.selection_rank())
    })?;
    let remaining = budget.remaining();
    let batch_limits = budget.batch();
    let mut limited = false;
    for (index, entry) in entries.iter_mut().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if entry.selected {
            continue;
        }
        let Some(batch) = batches.get_mut(entry.batch) else {
            limited = true;
            continue;
        };
        let records = checked_query_add(admitted.records, 1)?;
        let output = checked_query_add(admitted.output_bytes, entry.usage.output_bytes)?;
        let strings = checked_query_add(admitted.string_bytes, entry.usage.string_bytes)?;
        let batch_records = checked_query_add(batch.records, 1)?;
        let batch_output = checked_query_add(batch.output_bytes, entry.usage.output_bytes)?;
        if records > remaining.records()
            || output > remaining.output_bytes()
            || strings > remaining.string_bytes()
            || batch_records > batch_limits.max_records()
            || batch_output > batch_limits.max_output_bytes()
        {
            limited = true;
            continue;
        }
        entry.selected = true;
        admitted.records = records;
        admitted.output_bytes = output;
        admitted.string_bytes = strings;
        batch.records = batch_records;
        batch.output_bytes = batch_output;
    }
    // Required-only source-order boundaries stay fixed. Optional facts spend
    // their containing batch's slack, so acceptance never splits that closure
    // or repeatedly replans the whole file to search for an alternative packing.
    candidates.clear();
    for (index, entry) in entries.into_iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if entry.selected {
            candidates.push(entry.candidate);
        }
    }
    sort_cancellable_by(candidates, cancellation, |left, right| {
        candidate_source_key(*left).cmp(&candidate_source_key(*right))
    })?;
    cancellation.check()?;
    Ok(CandidateRetention { required, limited })
}

fn plan_fact_batches(
    facts: &[SyntaxFact],
    budget: RemainingBudget,
    cancellation: &Cancellation,
) -> Result<FactPlan, AdapterError> {
    let remaining = budget.remaining();
    let batch = budget.batch();
    let mut ranges = Vec::new();
    try_reserve_exact_cancellable(
        &mut ranges,
        facts.len().min(remaining.batches()),
        cancellation,
        "query-batch-allocation",
    )?;
    let mut batch_start = 0usize;
    let mut batch_records = 0usize;
    let mut batch_output = 0usize;
    let mut total_records = 0usize;
    let mut total_output = 0usize;
    let mut total_strings = 0usize;

    for (index, fact) in facts.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let usage = fact_usage(fact)?;
        if usage.output_bytes > batch.max_output_bytes() {
            break;
        }
        let next_batch_records = batch_records
            .checked_add(1)
            .ok_or_else(|| provider_failure("query-batch-accounting"))?;
        let next_batch_output = batch_output
            .checked_add(usage.output_bytes)
            .ok_or_else(|| provider_failure("query-batch-accounting"))?;
        if batch_records > 0
            && (next_batch_records > batch.max_records()
                || next_batch_output > batch.max_output_bytes())
        {
            ranges.push(batch_start..index);
            batch_start = index;
            batch_records = 0;
            batch_output = 0;
        }
        if batch_records == 0 && ranges.len() >= remaining.batches() {
            break;
        }
        let next_total_records = total_records
            .checked_add(1)
            .ok_or_else(|| provider_failure("query-stream-accounting"))?;
        let next_total_output = total_output
            .checked_add(usage.output_bytes)
            .ok_or_else(|| provider_failure("query-stream-accounting"))?;
        let next_total_strings = total_strings
            .checked_add(usage.string_bytes)
            .ok_or_else(|| provider_failure("query-stream-accounting"))?;
        if next_total_records > remaining.records()
            || next_total_output > remaining.output_bytes()
            || next_total_strings > remaining.string_bytes()
        {
            break;
        }
        batch_records = batch_records
            .checked_add(1)
            .ok_or_else(|| provider_failure("query-batch-accounting"))?;
        batch_output = batch_output
            .checked_add(usage.output_bytes)
            .ok_or_else(|| provider_failure("query-batch-accounting"))?;
        total_records = next_total_records;
        total_output = next_total_output;
        total_strings = next_total_strings;
    }
    let emitted = total_records;
    if batch_records > 0 {
        ranges.push(batch_start..emitted);
    }
    cancellation.check()?;
    Ok(FactPlan {
        batches: ranges,
        emitted,
    })
}

#[derive(Debug, Clone, Copy)]
struct FactUsage {
    output_bytes: usize,
    string_bytes: usize,
}

fn fact_usage(fact: &SyntaxFact) -> Result<FactUsage, AdapterError> {
    let string_bytes = fact.syntax_kind().as_str().len();
    let output_bytes = LOGICAL_SYNTAX_FACT_BYTES
        .checked_add(string_bytes)
        .ok_or_else(|| provider_failure("query-fact-accounting"))?;
    Ok(FactUsage {
        output_bytes,
        string_bytes,
    })
}

fn emit_fact_plan(
    facts: Vec<SyntaxFact>,
    plan: FactPlan,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut facts = facts.into_iter();
    let mut consumed = 0usize;
    let expected_emitted = plan.emitted;
    for range in plan.batches {
        cancellation.check()?;
        if range.start != consumed {
            return Err(provider_failure("query-batch-invariant"));
        }
        let batch_length = range
            .end
            .checked_sub(range.start)
            .ok_or_else(|| provider_failure("query-batch-invariant"))?;
        let mut batch_facts = Vec::new();
        try_reserve_exact_cancellable(
            &mut batch_facts,
            batch_length,
            cancellation,
            "query-batch-allocation",
        )?;
        for index in 0..batch_length {
            if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                cancellation.check()?;
            }
            batch_facts.push(
                facts
                    .next()
                    .ok_or_else(|| provider_failure("query-batch-invariant"))?,
            );
        }
        consumed = range.end;
        sink.push_cancellable(
            SyntaxFactBatch::new(sink.next_sequence(), batch_facts, Vec::new()),
            cancellation,
        )?;
    }
    cancellation.check()?;
    if consumed != expected_emitted {
        return Err(provider_failure("query-batch-invariant"));
    }
    Ok(())
}

fn pre_extraction_fact_limit(budget: RemainingBudget) -> usize {
    let remaining = budget.remaining();
    let batch = budget.batch();
    let facts_per_batch = batch
        .max_records()
        .min(batch.max_output_bytes() / MIN_SYNTAX_FACT_OUTPUT_BYTES);
    // This is theoretical capacity; finite remaining credit bounds the result
    // even when the configured batch allowance has no practical ceiling.
    let batched_facts = remaining.batches().saturating_mul(facts_per_batch);
    batched_facts
        .min(remaining.records())
        .min(remaining.output_bytes() / MIN_SYNTAX_FACT_OUTPUT_BYTES)
        .min(remaining.string_bytes() / MIN_SYNTAX_KIND_BYTES)
}

#[derive(Debug, Clone, Copy)]
struct NativeParseContext {
    expression_envelope: Option<expression::ExpressionEnvelope>,
    range_origin: (usize, Point),
    remaining_progress_checks: usize,
}

#[derive(Debug, Clone, Copy)]
struct ParseWorkLimit {
    // Embedded children retain the original aggregate credit independently of
    // byte progress, so many advancing child parses cannot replenish it.
    max_progress_checks: usize,
    failure_code: &'static str,
    max_total_progress_checks: usize,
    max_stalled_progress_checks: usize,
}

impl ParseWorkLimit {
    fn from_syntax_limits(max_nodes: usize, max_depth: usize) -> Self {
        let node_checks = max_nodes.saturating_mul(PARSE_PROGRESS_CHECKS_PER_SYNTAX_NODE);
        let depth_checks = max_depth.saturating_mul(PARSE_PROGRESS_CHECKS_PER_SYNTAX_DEPTH);
        if node_checks <= depth_checks {
            Self {
                max_progress_checks: node_checks.max(1),
                failure_code: "syntax-node-parse-work-limit",
                max_total_progress_checks: node_checks.max(1),
                max_stalled_progress_checks: depth_checks.max(1),
            }
        } else {
            Self {
                max_progress_checks: depth_checks.max(1),
                failure_code: "syntax-depth-parse-work-limit",
                max_total_progress_checks: node_checks.max(1),
                max_stalled_progress_checks: depth_checks.max(1),
            }
        }
    }
}

#[derive(Debug, Default)]
struct NativeParseProgress {
    total_checks: usize,
    high_watermark_byte: usize,
    stalled_checks: usize,
}

impl NativeParseProgress {
    fn record(&mut self, byte_offset: usize, limit: ParseWorkLimit) -> Option<&'static str> {
        self.total_checks = self.total_checks.saturating_add(1);
        if byte_offset > self.high_watermark_byte {
            self.high_watermark_byte = byte_offset;
            self.stalled_checks = 0;
        } else {
            // Revisiting an earlier parse position cannot restore work credit.
            // Native reductions and tree balancing may retain the same offset;
            // this guards stalled work, while inspection checks actual depth.
            self.stalled_checks = self.stalled_checks.saturating_add(1);
        }
        if self.total_checks >= limit.max_total_progress_checks {
            Some("syntax-node-parse-work-limit")
        } else if self.stalled_checks >= limit.max_stalled_progress_checks {
            Some("syntax-depth-parse-work-limit")
        } else {
            None
        }
    }
}

fn emit_extraction_limit_diagnostic(
    request: &ParseRequest<'_>,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    emit_extraction_diagnostic("syntax-extraction-limit", request, sink, cancellation)
}

fn emit_extraction_diagnostic(
    code: &str,
    request: &ParseRequest<'_>,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    let diagnostic = AdapterDiagnostic::new(
        DiagnosticCode::new(code).map_err(|_| provider_failure("diagnostic-code"))?,
        DiagnosticSeverity::Warning,
        Some(request.source().source_ref().clone()),
        CoverageStatus::Bounded,
    );
    sink.push_cancellable(
        SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), vec![diagnostic]),
        cancellation,
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum PrimaryDiagnostic {
    NodeLimit,
    DepthLimit,
    ErrorRecovery { start: usize, end: usize },
    CProjectionUnavailable { start: usize, end: usize },
}

fn mark_c_projection_unavailable(
    traversal: &mut TraversalReport,
    request: &ParseRequest<'_>,
) -> Result<(), AdapterError> {
    let full = request.source().source_ref().span();
    let start = request
        .included_ranges()
        .first()
        .map_or(full.start_byte(), |range| range.span().start_byte());
    let end = request
        .included_ranges()
        .last()
        .map_or(full.end_byte(), |range| range.span().end_byte());
    let start = usize::try_from(start).map_err(|_| provider_failure("c-projection-source-span"))?;
    let end = usize::try_from(end).map_err(|_| provider_failure("c-projection-source-span"))?;
    traversal.primary_diagnostic = Some(PrimaryDiagnostic::CProjectionUnavailable { start, end });
    traversal.coverage = CoverageStatus::Unknown;
    traversal.covered_source_bytes = 0;
    traversal.skipped_regions = 1;
    traversal.fully_traversed = false;
    Ok(())
}

#[derive(Debug)]
struct SyntaxTraversal {
    processed_nodes: usize,
    observed_depth: usize,
    error_span: Option<(usize, usize)>,
    limited_nodes: bool,
    limited_depth: bool,
}

fn inspect_parsed_tree(
    parsed: &NativeParsedTree,
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
    max_nodes: usize,
    max_depth: usize,
    cancellation: &Cancellation,
) -> Result<TraversalReport, AdapterError> {
    cancellation.check()?;
    if parsed.projection_unavailable || parsed.proof_limit.is_some() {
        // A stopped proof already visited the admitted raw tree partition.
        // Re-inspecting it would spend the same credit twice without producing
        // any safe facts for this unavailable or limited result.
        let mut report = TraversalReport {
            processed_nodes: parsed.projection_nodes,
            max_depth: parsed.projection_depth.min(max_depth),
            covered_source_bytes: 0,
            skipped_regions: 1,
            coverage: CoverageStatus::Bounded,
            primary_diagnostic: parsed.proof_limit.map(|limit| match limit {
                c_preprocessor::ProofLimit::Nodes => PrimaryDiagnostic::NodeLimit,
                c_preprocessor::ProofLimit::Depth => PrimaryDiagnostic::DepthLimit,
            }),
            fully_traversed: false,
        };
        if parsed.projection_unavailable {
            mark_c_projection_unavailable(&mut report, request)?;
        }
        return Ok(report);
    }
    let mut report = inspect_tree(
        &parsed.tree,
        request,
        source,
        max_nodes.saturating_sub(parsed.projection_nodes),
        max_depth,
        cancellation,
    )?;
    report.processed_nodes = report
        .processed_nodes
        .checked_add(parsed.projection_nodes)
        .ok_or(SinkError::AccountingOverflow)?;
    report.max_depth = report.max_depth.max(parsed.projection_depth);
    Ok(report)
}

fn inspect_tree(
    tree: &Tree,
    request: &ParseRequest<'_>,
    source: &NativeSourceView<'_>,
    max_nodes: usize,
    max_depth: usize,
    cancellation: &Cancellation,
) -> Result<TraversalReport, AdapterError> {
    let pointer_source = matches!(request.language().as_str(), "vba" | "visual-basic-classic")
        .then(|| source.as_bytes());
    let issue_kind = (request.language().as_str() == "sed").then_some("syntax_issue");
    let traversal = traverse_syntax(
        tree,
        max_nodes,
        max_depth,
        pointer_source,
        issue_kind,
        cancellation,
    )?;
    cancellation.check()?;
    let SyntaxTraversal {
        processed_nodes,
        observed_depth,
        error_span,
        limited_nodes,
        limited_depth,
    } = traversal;
    let error_span = if request.language().as_str() == "jsonc"
        && error_span.is_none()
        && !limited_nodes
        && !limited_depth
    {
        jsonc::invalid_document_span(tree, cancellation)?
    } else {
        error_span
    };
    let source_len = request.source().bytes().len();
    let requested_covered_bytes = if request.included_ranges().is_empty() {
        source_len
    } else {
        included_range_source_bytes(request.included_ranges(), cancellation)?
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
        // MATLAB's parser-only final delimiter has no physical evidence bytes.
        // Its EOF recovery endpoints collapse to the real source end only.
        let (start, end) = if needs_virtual_matlab_line_ending(request, source.as_bytes()) {
            (
                start.min(source.as_bytes().len()),
                end.min(source.as_bytes().len()),
            )
        } else {
            (start, end)
        };
        let start = source.to_physical_offset(start)?;
        let end = source.to_physical_offset(end)?;
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

fn included_range_source_bytes(
    ranges: &[IncludedRange],
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut total = 0usize;
    for (index, range) in ranges.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let span = range.span();
        let length = span
            .end_byte()
            .checked_sub(span.start_byte())
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| provider_failure("range-accounting"))?;
        total = total
            .checked_add(length)
            .ok_or_else(|| provider_failure("range-accounting"))?;
    }
    cancellation.check()?;
    Ok(total)
}

fn traverse_syntax(
    tree: &Tree,
    max_nodes: usize,
    max_depth: usize,
    pointer_source: Option<&[u8]>,
    issue_kind: Option<&str>,
    cancellation: &Cancellation,
) -> Result<SyntaxTraversal, AdapterError> {
    cancellation.check()?;
    let root = tree.root_node();
    let tree_has_error = root.has_error();
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
        if error_span.is_none()
            && (node.is_error() || node.is_missing() || issue_kind == Some(node.kind()))
        {
            let recovered_pointer = if let Some(source) = pointer_source {
                crate::query_pack::vba::is_pointer_comparison_recovery(node, source, cancellation)?
            } else {
                false
            };
            if !recovered_pointer {
                error_span = Some((node.start_byte(), node.end_byte()));
            }
        }
        if depth >= max_depth && node.child_count() > 0 {
            limited_depth = true;
            continue;
        }
        let check_child_errors = error_span.is_none()
            && tree_has_error
            && !node.is_error()
            && !node.is_missing()
            && node.has_error();
        let (children_limited, visible_error_child) = push_children_bounded(
            node,
            depth,
            &mut stack,
            max_nodes.saturating_sub(processed_nodes),
            check_child_errors,
            cancellation,
        )?;
        limited_nodes |= children_limited;
        // Invisible missing leaves can raise an ancestor's error cost without
        // appearing in child(). Explicit visible recovery keeps its own handling.
        if check_child_errors && !children_limited && !visible_error_child {
            error_span = Some((node.start_byte(), node.end_byte()));
        }
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
    check_child_errors: bool,
    cancellation: &Cancellation,
) -> Result<(bool, bool), AdapterError> {
    cancellation.check()?;
    let child_capacity = remaining.saturating_sub(stack.len());
    let total_children = node.child_count();
    let child_count = total_children.min(child_capacity);
    let mut visible_error_child = false;
    try_reserve_cancellable(
        stack,
        child_count,
        cancellation,
        "syntax-traversal-allocation",
    )?;
    for index in (0..child_count).rev() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let Ok(index) = u32::try_from(index) else {
            continue;
        };
        if let Some(child) = node.child(index) {
            if check_child_errors {
                visible_error_child |= child.has_error();
            }
            stack.push((child, depth.saturating_add(1)));
        }
    }
    cancellation.check()?;
    Ok((total_children > child_capacity, visible_error_child))
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
        PrimaryDiagnostic::CProjectionUnavailable { start, end } => (
            if request.language().as_str() == "cpp" {
                "cpp-preprocessor-replacement-not-analyzed"
            } else {
                "c-preprocessor-replacement-not-analyzed"
            },
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
    sink.push_cancellable(
        SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), vec![diagnostic]),
        cancellation,
    )?;
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

fn source_ref_for_projected_span(
    source: &SourceRef,
    view: &NativeSourceView<'_>,
    start: usize,
    end: usize,
) -> Result<SourceRef, AdapterError> {
    source_ref_for_span(
        source,
        view.to_physical_offset(start)?,
        view.to_physical_offset(end)?,
    )
    .ok_or_else(|| provider_failure("projected-source-span"))
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

fn try_reserve_exact_cancellable<T>(
    values: &mut Vec<T>,
    additional: usize,
    cancellation: &Cancellation,
    allocation_code: &'static str,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    values
        .try_reserve_exact(additional)
        .map_err(|_| provider_failure(allocation_code))
}

fn try_reserve_cancellable<T>(
    values: &mut Vec<T>,
    additional: usize,
    cancellation: &Cancellation,
    allocation_code: &'static str,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    values
        .try_reserve(additional)
        .map_err(|_| provider_failure(allocation_code))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, time::Duration};

    #[test]
    fn native_work_allows_monotonic_progress_but_keeps_the_total_limit() {
        let limit = ParseWorkLimit::from_syntax_limits(1024, 1);
        let mut progress = NativeParseProgress::default();
        for offset in 1..limit.max_total_progress_checks {
            assert_eq!(progress.record(offset, limit), None);
        }
        assert_eq!(progress.stalled_checks, 0);
        assert!(progress.total_checks > limit.max_stalled_progress_checks);
        assert_eq!(
            progress.record(limit.max_total_progress_checks, limit),
            Some("syntax-node-parse-work-limit")
        );
    }

    #[test]
    fn native_work_rejects_equal_and_backtracking_offsets_without_refunding_credit() {
        let limit = ParseWorkLimit::from_syntax_limits(1024, 1);
        for offsets in [[0, 0, 0], [9, 1, 10]] {
            let mut progress = NativeParseProgress::default();
            assert_eq!(progress.record(10, limit), None);
            for index in 0..limit.max_stalled_progress_checks - 1 {
                assert_eq!(progress.record(offsets[index % offsets.len()], limit), None);
            }
            assert_eq!(progress.high_watermark_byte, 10);
            assert_eq!(
                progress.record(10, limit),
                Some("syntax-depth-parse-work-limit")
            );
        }
    }

    #[test]
    fn native_work_resets_only_stalled_credit_after_a_new_high_watermark() {
        let limit = ParseWorkLimit::from_syntax_limits(1024, 1);
        let mut progress = NativeParseProgress::default();
        assert_eq!(progress.record(10, limit), None);
        for _ in 0..limit.max_stalled_progress_checks - 2 {
            assert_eq!(progress.record(9, limit), None);
        }
        assert_eq!(progress.record(10, limit), None);
        assert_eq!(
            progress.stalled_checks,
            limit.max_stalled_progress_checks - 1
        );
        let total_before = progress.total_checks;
        assert_eq!(progress.record(11, limit), None);
        assert_eq!(progress.stalled_checks, 0);
        assert_eq!(progress.total_checks, total_before + 1);
        for _ in 0..limit.max_stalled_progress_checks - 1 {
            assert_eq!(progress.record(10, limit), None);
        }
        assert_eq!(
            progress.record(11, limit),
            Some("syntax-depth-parse-work-limit")
        );
    }

    #[test]
    fn native_work_saturates_counters_and_preserves_shared_limit_selection() {
        let saturated = ParseWorkLimit::from_syntax_limits(usize::MAX, usize::MAX);
        assert_eq!(saturated.max_progress_checks, usize::MAX);
        assert_eq!(saturated.max_total_progress_checks, usize::MAX);
        assert_eq!(saturated.max_stalled_progress_checks, usize::MAX);
        assert_eq!(saturated.failure_code, "syntax-node-parse-work-limit");
        let mut total = NativeParseProgress {
            total_checks: usize::MAX - 1,
            ..NativeParseProgress::default()
        };
        assert_eq!(
            total.record(1, saturated),
            Some("syntax-node-parse-work-limit")
        );
        assert_eq!(
            total.record(2, saturated),
            Some("syntax-node-parse-work-limit")
        );
        assert_eq!(total.total_checks, usize::MAX);
        let mut stalled = NativeParseProgress {
            stalled_checks: usize::MAX - 1,
            ..NativeParseProgress::default()
        };
        assert_eq!(
            stalled.record(0, saturated),
            Some("syntax-depth-parse-work-limit")
        );
        assert_eq!(
            stalled.record(0, saturated),
            Some("syntax-depth-parse-work-limit")
        );
        assert_eq!(stalled.stalled_checks, usize::MAX);
        for (nodes, depth, shared_checks, code) in [
            (1, 64, 4, "syntax-node-parse-work-limit"),
            (1024, 1, 1024, "syntax-depth-parse-work-limit"),
            (256, 1, 1024, "syntax-node-parse-work-limit"),
        ] {
            let limit = ParseWorkLimit::from_syntax_limits(nodes, depth);
            assert_eq!(limit.max_progress_checks, shared_checks);
            assert_eq!(limit.failure_code, code);
        }
        let minimum = ParseWorkLimit::from_syntax_limits(0, 0);
        assert_eq!(minimum.max_progress_checks, 1);
        assert_eq!(
            NativeParseProgress::default().record(1, minimum),
            Some("syntax-node-parse-work-limit")
        );
    }

    #[test]
    fn deferred_provider_metadata_and_debug_do_not_compile_queries() {
        let registry = Box::leak(Box::new(QueryPackRegistry::uninitialized_for_test()));
        let config = RuntimeConfig::new(
            4096,
            1024,
            64,
            8,
            8,
            1,
            4096,
            ParserSettings::new(256).unwrap(),
        )
        .unwrap();
        let provider = TreeSitterProvider::with_query_packs(config, registry).unwrap();
        assert_eq!(registry.initialized_count(), 0);
        assert_eq!(provider.registry.descriptors().len(), 84);
        assert_eq!(provider.capabilities.languages().len(), 84);
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
        let debug = format!("{provider:?}");
        assert!(debug.contains("initialized_query_pack_count: 0"));
        assert!(debug.contains("initialized_query_pattern_count: 0"));
        assert_eq!(registry.initialized_count(), 0);
    }

    #[test]
    fn providers_share_immutable_queries_but_keep_runtime_state_private() {
        let barrier = std::sync::Barrier::new(4);
        let barrier = &barrier;
        let providers = std::thread::scope(|scope| {
            let handles = (1usize..=4)
                .map(|capacity| {
                    scope.spawn(move || {
                        let config = RuntimeConfig::new(
                            4096,
                            1024,
                            64,
                            8,
                            8,
                            capacity,
                            capacity * 4096,
                            ParserSettings::new(256).expect("valid input chunk"),
                        )
                        .expect("valid provider configuration");
                        barrier.wait();
                        if capacity % 2 == 0 {
                            TreeSitterProvider::new_deferred(config)
                                .expect("deferred provider initializes")
                        } else {
                            TreeSitterProvider::new(config).expect("audited provider initializes")
                        }
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("provider initializer completes"))
                .collect::<Vec<_>>()
        });
        let first = providers.first().expect("four providers initialized");
        let lease = first.pool.acquire(&Cancellation::new()).unwrap();
        assert_eq!(first.stats().checked_out_parsers, 1);
        let previous = PreviousParse {
            provider_id: first.provider_id,
            entry_id: 1,
        };
        for (slot, provider) in providers.iter().enumerate().skip(1) {
            assert_ne!(first.provider_id, provider.provider_id);
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
            assert_eq!(provider.stats().cache.capacity_bytes, (slot + 1) * 4096);
            assert_eq!(provider.config.max_concurrent_parses(), slot + 1);
            assert_eq!(
                provider
                    .resolve_previous(Some(&previous), &Cancellation::new())
                    .unwrap()
                    .unwrap()
                    .invalidation,
                Some(ReuseInvalidation::Provider)
            );
            for descriptor in first.registry.descriptors() {
                assert!(std::ptr::eq(
                    first.query_packs.get(descriptor.family()).unwrap(),
                    provider.query_packs.get(descriptor.family()).unwrap(),
                ));
            }
            assert!(std::ptr::eq(
                first
                    .query_packs
                    .get_for_source(
                        GrammarFamily::TypeScript,
                        "source.tsx",
                        &Cancellation::new()
                    )
                    .unwrap(),
                provider
                    .query_packs
                    .get_for_source(
                        GrammarFamily::TypeScript,
                        "source.tsx",
                        &Cancellation::new()
                    )
                    .unwrap(),
            ));
        }
        drop(lease);
        assert_eq!(first.stats().checked_out_parsers, 0);
    }

    #[test]
    fn shadowed_captures_do_not_displace_distinct_facts_at_capacity() {
        let candidate = |start, end, role| QueryCandidate {
            start,
            end,
            role,
            syntax: "identifier",
            required: role == StructuralRole::Definition,
            native_depth: 1,
        };
        let expected = vec![
            candidate(0, 4, StructuralRole::Definition),
            candidate(0, 5, StructuralRole::Reference),
            candidate(8, 12, StructuralRole::Documentation),
            candidate(14, 18, StructuralRole::Reference),
            candidate(14, 18, StructuralRole::Call),
            candidate(20, 24, StructuralRole::Comment),
        ];
        let mut captures = expected.clone();
        captures.push(candidate(0, 4, StructuralRole::Reference));
        captures.push(candidate(8, 12, StructuralRole::Comment));
        captures.sort_by_key(|c| (c.start, c.end, c.role));
        let cancellation = Cancellation::new();
        remove_shadowed_candidates(&mut captures, &cancellation).unwrap();
        let retention =
            prune_optional_candidates_within_limit(&mut captures, expected.len(), &cancellation)
                .unwrap();
        assert!(!retention.limited);
        assert_eq!(retention.required, 1);
        let identities = |items: &[QueryCandidate]| {
            let mut identities: Vec<_> = items.iter().map(|c| (c.start, c.end, c.role)).collect();
            identities.sort();
            identities
        };
        assert_eq!(identities(&captures), identities(&expected));
    }

    #[test]
    fn distinct_reference_roles_survive_same_span_definitions_without_redundant_reads() {
        for (syntax, expected) in [
            ("nix.inherited_name", 2),
            ("nix.inherited_attribute", 2),
            ("perl.package_context", 2),
            ("perl.identifier", 1),
            ("nix.identifier", 1),
        ] {
            let mut captures = [StructuralRole::Definition, StructuralRole::Reference]
                .into_iter()
                .map(|role| QueryCandidate {
                    start: 0,
                    end: 4,
                    role,
                    syntax,
                    required: role == StructuralRole::Definition,
                    native_depth: 1,
                })
                .collect();
            remove_shadowed_candidates(&mut captures, &Cancellation::new()).unwrap();
            assert_eq!(captures.len(), expected);
            assert!(
                captures
                    .iter()
                    .any(|capture| capture.role == StructuralRole::Definition)
            );
        }
    }

    #[test]
    fn shadowed_capture_normalization_honors_empty_input_cancellation() {
        let cancellation = Cancellation::new();
        let mut captures = Vec::new();
        remove_shadowed_candidates(&mut captures, &cancellation).unwrap();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(remove_shadowed_candidates(&mut captures, &cancellation).is_err());
    }

    #[test]
    fn every_reuse_identity_dimension_has_a_distinct_invalidation() {
        let base = invalid_identity();
        let tree = language_tree(GrammarFamily::Rust, b"fn one() {}");
        let cached = CachedParse {
            entry_id: 1,
            projection_policy: NATIVE_SOURCE_PROJECTION_POLICY,
            c_preprocessor_projection_policy: C_PREPROCESSOR_PROJECTION_POLICY,
            fortran_projection_policy: FORTRAN_TEMPLATE_PROJECTION_POLICY,
            cmake_header_projection_policy: CMAKE_HEADER_PROJECTION_POLICY,
            cmake_patch_projection_policy: CMAKE_PATCH_PROJECTION_POLICY,
            cmake_command_template_projection_policy: CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY,
            pug_scanner_contract: pug_scope::SCANNER_CONTRACT_IDENTITY,
            lean_scanner_contract: lean_scope::SCANNER_CONTRACT_IDENTITY,
            idris_scanner_contract: idris_scope::SCANNER_CONTRACT_IDENTITY,
            idris_frontend_policy: idris_scope::FRONTEND_POLICY,
            sed_scanner_contract: sed_scope::SCANNER_CONTRACT_IDENTITY,
            sed_frontend_policy: sed_scope::FRONTEND_POLICY,
            wolfram_scanner_contract: wolfram_scope::SCANNER_CONTRACT_IDENTITY,
            coffeescript_scanner_contract: coffeescript_scope::SCANNER_CONTRACT_IDENTITY,
            gdscript_scanner_contract: gdscript_scope::CACHE_CONTRACT_IDENTITY,
            dockerfile_scanner_contract: dockerfile_scope::SCANNER_CONTRACT_IDENTITY,
            identity: base.clone(),
            source: Arc::new(b"fn one() {}".to_vec()),
            tree: Some(tree),
            accounted_bytes: 1,
            invalidation: None,
        };
        let mut old_projection = cached.clone_for_reuse();
        old_projection.projection_policy = "rootlight.treesitter.source-projection/0";
        assert_invalidated(
            &old_projection,
            base.clone(),
            ReuseInvalidation::FrontendRoute,
        );
        let mut old_c = cached.clone_for_reuse();
        old_c.c_preprocessor_projection_policy =
            "rootlight.treesitter.c-preprocessor-replacement-input/0";
        assert_invalidated(&old_c, base.clone(), ReuseInvalidation::FrontendRoute);
        let mut old_header = cached.clone_for_reuse();
        old_header.cmake_header_projection_policy = "rootlight.treesitter.cmake-header-template/0";
        assert_invalidated(&old_header, base.clone(), ReuseInvalidation::FrontendRoute);
        let mut old_patch = cached.clone_for_reuse();
        old_patch.cmake_patch_projection_policy =
            "rootlight.treesitter.cmake-physical-unified-patch/0";
        assert_invalidated(&old_patch, base.clone(), ReuseInvalidation::FrontendRoute);
        let mut changed = base.clone();
        changed.family = GrammarFamily::Python;
        assert_invalidated(&cached, changed, ReuseInvalidation::Language);
        let mut changed = base.clone();
        changed.cmake_header_path = true;
        assert_invalidated(&cached, changed, ReuseInvalidation::FrontendRoute);
        let mut changed = base.clone();
        changed.grammar_version = "other";
        assert_invalidated(&cached, changed, ReuseInvalidation::GrammarVersion);
        let mut changed = base.clone();
        changed.encoding = "other".to_owned();
        assert_invalidated(&cached, changed, ReuseInvalidation::Encoding);
        let mut changed = base.clone();
        Arc::make_mut(&mut changed.included_ranges).push(
            rootlight_adapter_sdk::IncludedRange::new(
                SourceSpan::new(rootlight_ids::FileId::from_bytes([1; 20]), 0, 1)
                    .expect("test span is ordered"),
                rootlight_adapter_sdk::LanguageId::new("rust").expect("test language is valid"),
            ),
        );
        assert_invalidated(&cached, changed, ReuseInvalidation::IncludedRanges);
        let mut changed = base.clone();
        changed.settings = ParserSettings::new(2).expect("test setting is nonzero");
        assert_invalidated(&cached, changed, ReuseInvalidation::ParserSettings);
        let mut changed = base.clone();
        changed.virtual_line_ending = true;
        assert_invalidated(&cached, changed, ReuseInvalidation::VirtualLineEnding);
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
        let cancellation = Cancellation::new();
        assert_eq!(
            points_for_offsets("é\r\nx".as_bytes(), 4, 4, &cancellation)
                .expect("point scan succeeds"),
            Some((Point { row: 1, column: 0 }, Point { row: 1, column: 0 }))
        );
        assert_eq!(
            points_for_offsets("é\r\nx".as_bytes(), 4, 5, &cancellation)
                .expect("point scan succeeds"),
            Some((Point { row: 1, column: 0 }, Point { row: 1, column: 1 }))
        );
    }

    #[test]
    fn chunked_utf8_validation_preserves_scalars_across_checkpoints() {
        let mut source = vec![b'a'; CANCELLATION_BYTE_INTERVAL - 1];
        source.extend_from_slice("é".as_bytes());

        validate_utf8_cancellable(&source, &Cancellation::new())
            .expect("a scalar split across validation chunks remains valid");
        source.pop();
        assert!(matches!(
            validate_utf8_cancellable(&source, &Cancellation::new()),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "invalid-utf8"
        ));
    }

    #[test]
    fn byte_chunk_work_observes_a_deadline_established_after_work_begins() {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let cancellation = Cancellation::with_deadline(deadline);
        let source = vec![b'x'; CANCELLATION_BYTE_INTERVAL * 2];
        let mut visited = 0usize;

        let result = for_each_byte_chunk(&source, &cancellation, |_| {
            visited += 1;
            if visited == 1 {
                assert!(cancellation.check_at(deadline).is_err());
            }
            Ok(())
        });

        assert_eq!(visited, 1);
        assert!(matches!(
            result,
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::DeadlineExceeded
            })
        ));
    }

    #[test]
    fn cancellable_sort_observes_a_deadline_established_inside_a_sort_run() {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let cancellation = Cancellation::with_deadline(deadline);
        let established = Cell::new(false);
        let mut values = (0..4096_u64).rev().collect::<Vec<_>>();

        let result = sort_cancellable_by(&mut values, &cancellation, |left, right| {
            if !established.replace(true) {
                assert!(cancellation.check_at(deadline).is_err());
            }
            left.cmp(right)
        });

        assert!(matches!(
            result,
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::DeadlineExceeded
            })
        ));
    }

    #[test]
    fn cancellable_sort_matches_the_canonical_order() {
        let mut values = (0..4097_u64)
            .map(|value| value.wrapping_mul(7919) % 4097)
            .collect::<Vec<_>>();
        let mut expected = values.clone();
        expected.sort_unstable();

        sort_cancellable_by(&mut values, &Cancellation::new(), Ord::cmp)
            .expect("bounded sort succeeds");

        assert_eq!(values, expected);
    }

    #[test]
    fn traversal_and_edit_preparation_observe_cancellation() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        let tree = language_tree(GrammarFamily::Rust, b"fn cancelled() {}");

        assert!(matches!(
            traverse_syntax(&tree, 1024, 64, None, None, &cancellation),
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

    #[test]
    fn nonallocating_fact_accounting_matches_the_sdk_sink_contract() {
        let fact = SyntaxFact::new(
            1,
            None,
            rootlight_adapter_sdk::SyntaxFactKind::Signature,
            SourceSpan::new(rootlight_ids::FileId::from_bytes([1; 20]), 0, 1)
                .expect("test span is ordered"),
            0,
            SyntaxKindLabel::new("rust.parameters.signature").expect("test syntax label is valid"),
        );
        let observed = fact_usage(&fact).expect("fact usage is representable");
        let expected = SyntaxFactBatch::new(0, vec![fact], Vec::new())
            .usage()
            .expect("SDK batch usage is representable");

        assert_eq!(observed.output_bytes, expected.output_bytes());
        assert_eq!(observed.string_bytes, expected.string_bytes());
        assert_eq!(expected.records(), 1);
    }

    #[test]
    fn pre_extraction_limit_accounts_all_output_dimensions() {
        let batch = rootlight_adapter_sdk::BatchThresholds::new(128, 128, 8, 4096)
            .expect("test batch limits are valid");
        let stream = rootlight_adapter_sdk::StreamLimits::new(
            8192,
            1_048_576,
            128,
            64,
            64 * 1024,
            32,
            batch,
        )
        .expect("test stream limits are valid");
        let file = rootlight_ids::FileId::from_bytes([3; 20]);
        let source = SourceRef::new(
            rootlight_ids::RepositoryId::from_bytes([1; 16]),
            rootlight_ids::GenerationId::from_bytes([2; 20]),
            SourceSpan::new(file, 0, 1).expect("test source span is ordered"),
            ContentHash::from_bytes([4; 32]),
            None,
        );
        let sink = rootlight_adapter_sdk::BoundedSyntaxSink::new(source, stream, 64);

        assert_eq!(pre_extraction_fact_limit(sink.remaining_budget()), 1);
    }

    #[test]
    fn pre_extraction_limit_with_unbounded_batches_preserves_finite_credit() {
        for (records, output_bytes, string_bytes, expected) in
            [(3, 512, 512, 3), (8, 130, 512, 2), (8, 512, 1, 1)]
        {
            let sink = exact_admission_sink_with_records(
                records,
                usize::MAX,
                output_bytes,
                string_bytes,
                512.min(output_bytes),
            );
            assert_eq!(pre_extraction_fact_limit(sink.remaining_budget()), expected);
        }
        assert_exact_admission_preserves_late_definition(usize::MAX, 150, 512, 150);
    }

    fn exact_admission_facts() -> [SyntaxFact; 2] {
        let file = rootlight_ids::FileId::from_bytes([3; 20]);
        [
            SyntaxFact::new(
                1,
                None,
                rootlight_adapter_sdk::SyntaxFactKind::Signature,
                SourceSpan::new(file, 0, 8).expect("optional span is ordered"),
                0,
                SyntaxKindLabel::new("fsharp.parameter_header.expression")
                    .expect("optional syntax label is valid"),
            ),
            SyntaxFact::new(
                2,
                None,
                rootlight_adapter_sdk::SyntaxFactKind::Occurrence,
                SourceSpan::new(file, 16, 20).expect("definition span is ordered"),
                0,
                SyntaxKindLabel::new("fsharp.name.definition")
                    .expect("definition syntax label is valid"),
            ),
        ]
    }

    fn exact_admission_candidates() -> Vec<QueryCandidate> {
        vec![
            QueryCandidate {
                start: 0,
                end: 8,
                role: StructuralRole::Expression,
                syntax: "fsharp.parameter_header",
                required: false,
                native_depth: 0,
            },
            QueryCandidate {
                start: 16,
                end: 20,
                role: StructuralRole::Definition,
                syntax: "fsharp.name",
                required: true,
                native_depth: 0,
            },
        ]
    }

    fn exact_admission_fact(candidate: QueryCandidate, local_id: u64) -> SyntaxFact {
        SyntaxFact::new(
            local_id,
            None,
            candidate.role.fact_kind(),
            SourceSpan::new(
                rootlight_ids::FileId::from_bytes([3; 20]),
                u64::try_from(candidate.start).unwrap(),
                u64::try_from(candidate.end).unwrap(),
            )
            .unwrap(),
            0,
            SyntaxKindLabel::new(&format!(
                "{}.{}",
                candidate_syntax_base(candidate),
                candidate.role.label(),
            ))
            .unwrap(),
        )
    }

    fn exact_admission_sink(
        maximum_batches: usize,
        output_bytes: usize,
        string_bytes: usize,
        batch_output_bytes: usize,
    ) -> rootlight_adapter_sdk::BoundedSyntaxSink {
        exact_admission_sink_with_records(
            2,
            maximum_batches,
            output_bytes,
            string_bytes,
            batch_output_bytes,
        )
    }

    fn exact_admission_sink_with_records(
        maximum_records: usize,
        maximum_batches: usize,
        output_bytes: usize,
        string_bytes: usize,
        batch_output_bytes: usize,
    ) -> rootlight_adapter_sdk::BoundedSyntaxSink {
        let batch = rootlight_adapter_sdk::BatchThresholds::new(
            maximum_records,
            batch_output_bytes,
            1,
            1024,
        )
        .expect("test batch limits are valid");
        let stream = rootlight_adapter_sdk::StreamLimits::new(
            maximum_batches,
            maximum_records,
            output_bytes,
            1,
            1024,
            string_bytes,
            batch,
        )
        .expect("test stream limits are valid");
        let source = SourceRef::new(
            rootlight_ids::RepositoryId::from_bytes([1; 16]),
            rootlight_ids::GenerationId::from_bytes([2; 20]),
            SourceSpan::new(rootlight_ids::FileId::from_bytes([3; 20]), 0, 32)
                .expect("test source span is ordered"),
            ContentHash::from_bytes([4; 32]),
            None,
        );
        rootlight_adapter_sdk::BoundedSyntaxSink::new(source, stream, 64)
    }

    fn assert_exact_admission_preserves_late_definition(
        maximum_batches: usize,
        output_bytes: usize,
        string_bytes: usize,
        batch_output_bytes: usize,
    ) {
        let facts = exact_admission_facts();
        let optional_usage = fact_usage(&facts[0]).expect("optional usage is representable");
        let definition_usage = fact_usage(&facts[1]).expect("definition usage is representable");
        assert_eq!(optional_usage.output_bytes, 98);
        assert_eq!(optional_usage.string_bytes, 34);
        assert_eq!(definition_usage.output_bytes, 86);
        assert_eq!(definition_usage.string_bytes, 22);
        let sink = exact_admission_sink(
            maximum_batches,
            output_bytes,
            string_bytes,
            batch_output_bytes,
        );
        let cancellation = Cancellation::new();
        assert_eq!(
            pre_extraction_fact_limit(sink.remaining_budget()),
            2,
            "both candidates reach exact planning before any coarse pruning"
        );
        let required_only =
            plan_fact_batches(&facts[1..], sink.remaining_budget(), &cancellation).unwrap();
        assert_eq!(required_only.emitted, 1, "the definition fits by itself");
        let mut candidates = exact_admission_candidates();
        let retention =
            admit_exact_query_candidates(&mut candidates, sink.remaining_budget(), &cancellation)
                .unwrap();
        assert!(retention.limited, "optional clipping remains explicit");
        assert_eq!(retention.required, 1);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.role == StructuralRole::Definition
                    && candidate.start == 16
                    && candidate.end == 20),
            "optional evidence must not displace the fitting late definition: {candidates:?}"
        );
        let admitted_facts = candidates
            .iter()
            .copied()
            .enumerate()
            .map(|(index, candidate)| {
                exact_admission_fact(candidate, u64::try_from(index + 1).unwrap())
            })
            .collect::<Vec<_>>();
        let plan =
            plan_fact_batches(&admitted_facts, sink.remaining_budget(), &cancellation).unwrap();
        assert_eq!(
            plan.emitted,
            admitted_facts.len(),
            "every admitted survivor fits exact emission"
        );
    }

    #[test]
    fn exact_output_admission_preserves_late_required_definition() {
        assert_exact_admission_preserves_late_definition(4, 150, 512, 150);
    }

    #[test]
    fn exact_string_admission_preserves_late_required_definition() {
        assert_exact_admission_preserves_late_definition(4, 512, 40, 512);
    }

    #[test]
    fn exact_batch_admission_preserves_late_required_definition() {
        assert_exact_admission_preserves_late_definition(1, 512, 512, 150);
    }

    #[test]
    fn exact_fact_admission_retains_all_fitting_facts() {
        let facts = exact_admission_facts();
        let sink = exact_admission_sink(4, 512, 512, 512);
        let plan =
            plan_fact_batches(&facts, sink.remaining_budget(), &Cancellation::new()).unwrap();
        assert_eq!(plan.emitted, facts.len());
        assert_eq!(plan.batches, vec![0..facts.len()]);
        let mut candidates = exact_admission_candidates();
        let expected = candidates.clone();
        let retention = admit_exact_query_candidates(
            &mut candidates,
            sink.remaining_budget(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(!retention.limited);
        assert_eq!(retention.required, 1);
        assert_eq!(candidates, expected);
    }

    #[test]
    fn exact_fact_admission_orders_unordered_candidates() {
        let sink = exact_admission_sink(4, 512, 512, 512);
        let expected = exact_admission_candidates();
        let mut candidates = expected.iter().copied().rev().collect::<Vec<_>>();
        let retention = admit_exact_query_candidates(
            &mut candidates,
            sink.remaining_budget(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(!retention.limited);
        assert_eq!(retention.required, 1);
        assert_eq!(candidates, expected);
    }

    #[test]
    fn exact_candidate_source_order_check_handles_long_and_empty_inputs() {
        let cancellation = Cancellation::new();
        assert!(candidates_in_source_order(&[], &cancellation).unwrap());
        let template = exact_admission_candidates()[0];
        let mut candidates = (0..CANCELLATION_CHECK_INTERVAL + 2)
            .map(|index| QueryCandidate {
                start: index * 2,
                end: index * 2 + 1,
                ..template
            })
            .collect::<Vec<_>>();
        assert!(candidates_in_source_order(&candidates, &cancellation).unwrap());
        let last = candidates.len() - 1;
        candidates.swap(last - 1, last);
        assert!(!candidates_in_source_order(&candidates, &cancellation).unwrap());

        let sink = exact_admission_sink(4, 512, 512, 512);
        let retention =
            admit_exact_query_candidates(&mut Vec::new(), sink.remaining_budget(), &cancellation)
                .unwrap();
        assert!(!retention.limited);
        assert_eq!(retention.required, 0);
    }

    #[test]
    fn exact_fact_admission_preserves_precancellation() {
        let facts = exact_admission_facts();
        let sink = exact_admission_sink(1, 512, 512, 150);
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            plan_fact_batches(&facts, sink.remaining_budget(), &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
        let mut candidates = exact_admission_candidates();
        assert!(matches!(
            admit_exact_query_candidates(&mut candidates, sink.remaining_budget(), &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            admit_exact_query_candidates(&mut Vec::new(), sink.remaining_budget(), &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn exact_fact_admission_does_not_downgrade_mandatory_overflow() {
        let mut candidates = vec![
            QueryCandidate {
                start: 0,
                end: 4,
                role: StructuralRole::Definition,
                syntax: "fsharp.name",
                required: true,
                native_depth: 1,
            },
            QueryCandidate {
                start: 16,
                end: 20,
                role: StructuralRole::Definition,
                syntax: "fsharp.name",
                required: true,
                native_depth: 1,
            },
        ];
        assert!(matches!(
            retain_required_candidates_within_limit(&mut candidates, 1, &Cancellation::new()),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: 2,
                limit: 1,
            }))
        ));
        let sink = exact_admission_sink_with_records(1, 4, 512, 512, 512);
        assert!(matches!(
            admit_exact_query_candidates(
                &mut candidates,
                sink.remaining_budget(),
                &Cancellation::new()
            ),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: 2,
                limit: 1,
            }))
        ));
    }

    #[test]
    fn exact_fact_admission_reports_mandatory_output_string_and_batch_deficits() {
        for (
            maximum_batches,
            output_bytes,
            string_bytes,
            batch_output_bytes,
            resource,
            observed,
            limit,
        ) in [
            (4, 150, 512, 150, ResourceKind::OutputBytes, 172, 150),
            (4, 512, 40, 512, ResourceKind::StringBytes, 44, 40),
            (1, 512, 512, 150, ResourceKind::Batches, 2, 1),
        ] {
            let mut candidates = exact_admission_candidates();
            candidates[0] = QueryCandidate {
                start: 0,
                end: 4,
                ..candidates[1]
            };
            let sink = exact_admission_sink(
                maximum_batches,
                output_bytes,
                string_bytes,
                batch_output_bytes,
            );
            let error = admit_exact_query_candidates(
                &mut candidates,
                sink.remaining_budget(),
                &Cancellation::new(),
            )
            .unwrap_err();
            assert!(
                matches!(&error,
                    AdapterError::Sink(SinkError::StreamLimit {
                        resource: actual_resource, observed: actual_observed, limit: actual_limit,
                    }) if *actual_resource == resource && *actual_observed == observed && *actual_limit == limit
                ),
                "wrong mandatory deficit: {error:?}"
            );
        }
    }

    #[test]
    fn exact_candidate_accounting_matches_materialized_labels() {
        let mut candidates = exact_admission_candidates();
        for syntax in ["python.module", "javascript.program", "typescript.program"] {
            candidates.push(QueryCandidate {
                start: 0,
                end: 32,
                role: StructuralRole::Module,
                syntax,
                required: true,
                native_depth: 0,
            });
        }
        for candidate in candidates {
            let observed = candidate_usage(candidate).unwrap();
            let fact = exact_admission_fact(candidate, 1);
            let expected = SyntaxFactBatch::new(0, vec![fact], Vec::new())
                .usage()
                .unwrap();
            assert_eq!(observed.output_bytes, expected.output_bytes());
            assert_eq!(observed.string_bytes, expected.string_bytes());
        }
    }

    #[test]
    fn exact_fact_admission_clips_before_parent_identity_assignment() {
        let candidate = |start, end, role, syntax, native_depth, required| QueryCandidate {
            start,
            end,
            role,
            syntax,
            native_depth,
            required,
        };
        let mut candidates = vec![
            candidate(0, 32, StructuralRole::Root, "test", 0, true),
            candidate(0, 32, StructuralRole::Scope, "test", 1, true),
            candidate(
                0,
                8,
                StructuralRole::Expression,
                "fsharp.parameter_header",
                2,
                false,
            ),
            candidate(16, 20, StructuralRole::Declaration, "test", 2, true),
            candidate(16, 20, StructuralRole::Definition, "fsharp.name", 3, true),
        ];
        let sink = exact_admission_sink_with_records(5, 2, 350, 512, 350);
        let cancellation = Cancellation::new();
        let retention =
            admit_exact_query_candidates(&mut candidates, sink.remaining_budget(), &cancellation)
                .unwrap();
        assert!(retention.limited);
        assert_eq!(retention.required, 4);
        assert_eq!(candidates.len(), 4);
        assert!(
            candidates
                .windows(2)
                .all(|pair| candidate_source_key(pair[0]) <= candidate_source_key(pair[1]))
        );
        let mut drafts = candidates
            .iter()
            .map(|candidate| FactDraft {
                start: candidate.start,
                end: candidate.end,
                role: candidate.role,
                syntax_kind: SyntaxKindLabel::new(&format!(
                    "{}.{}",
                    candidate_syntax_base(*candidate),
                    candidate.role.label(),
                ))
                .unwrap(),
                parent: None,
                depth: 0,
                native_depth: candidate.native_depth,
            })
            .collect::<Vec<_>>();
        assign_fact_parents(&mut drafts, &cancellation).unwrap();
        assert_eq!(
            drafts.iter().map(|draft| draft.role).collect::<Vec<_>>(),
            vec![
                StructuralRole::Root,
                StructuralRole::Scope,
                StructuralRole::Declaration,
                StructuralRole::Definition,
            ]
        );
        assert_eq!(
            drafts.iter().map(|draft| draft.parent).collect::<Vec<_>>(),
            vec![None, Some(0), Some(1), Some(2)]
        );
        assert_eq!(
            drafts.iter().map(|draft| draft.depth).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn wide_fact_planning_observes_cancellation_before_allocation_work() {
        let file = rootlight_ids::FileId::from_bytes([3; 20]);
        let fact = SyntaxFact::new(
            1,
            None,
            rootlight_adapter_sdk::SyntaxFactKind::Occurrence,
            SourceSpan::new(file, 0, 1).expect("test span is ordered"),
            0,
            SyntaxKindLabel::new("rust.identifier.reference").expect("test syntax label is valid"),
        );
        let facts = vec![fact; 4096];
        let batch = rootlight_adapter_sdk::BatchThresholds::new(64, 64 * 1024, 4, 1024)
            .expect("test batch limits are valid");
        let stream = rootlight_adapter_sdk::StreamLimits::new(
            64,
            4096,
            4 * 1024 * 1024,
            16,
            4096,
            128 * 1024,
            batch,
        )
        .expect("test stream limits are valid");
        let source = SourceRef::new(
            rootlight_ids::RepositoryId::from_bytes([1; 16]),
            rootlight_ids::GenerationId::from_bytes([2; 20]),
            SourceSpan::new(file, 0, 1).expect("test source span is ordered"),
            ContentHash::from_bytes([4; 32]),
            None,
        );
        let sink = rootlight_adapter_sdk::BoundedSyntaxSink::new(source, stream, 64);
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));

        assert!(matches!(
            plan_fact_batches(&facts, sink.remaining_budget(), &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn cancelled_reservation_precedes_capacity_failure() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        let mut values = Vec::<u8>::new();

        assert!(matches!(
            try_reserve_exact_cancellable(
                &mut values,
                usize::MAX,
                &cancellation,
                "query-batch-allocation",
            ),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn included_range_work_observes_cancellation() {
        let file = rootlight_ids::FileId::from_bytes([5; 20]);
        let language = LanguageId::new("rust").expect("test language is valid");
        let ranges = vec![
            IncludedRange::new(
                SourceSpan::new(file, 0, 2).expect("test span is ordered"),
                language.clone(),
            ),
            IncludedRange::new(
                SourceSpan::new(file, 4, 7).expect("test span is ordered"),
                language.clone(),
            ),
        ];
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));

        assert_cancelled(validate_included_range_languages(
            &ranges,
            &language,
            &cancellation,
        ));
        assert_cancelled(included_range_source_bytes(&ranges, &cancellation));
    }

    #[test]
    fn included_range_validation_and_coverage_preserve_semantics() {
        let file = rootlight_ids::FileId::from_bytes([6; 20]);
        let language = LanguageId::new("rust").expect("test language is valid");
        let ranges = vec![
            IncludedRange::new(
                SourceSpan::new(file, 0, 2).expect("test span is ordered"),
                language.clone(),
            ),
            IncludedRange::new(
                SourceSpan::new(file, 4, 7).expect("test span is ordered"),
                language.clone(),
            ),
        ];
        let cancellation = Cancellation::new();

        validate_included_range_languages(&ranges, &language, &cancellation)
            .expect("matching languages are accepted");
        assert_eq!(
            included_range_source_bytes(&ranges, &cancellation)
                .expect("bounded coverage accounting succeeds"),
            5
        );
    }

    fn assert_cancelled<T>(result: Result<T, AdapterError>) {
        assert!(matches!(
            result,
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
        let scope = (family == GrammarFamily::Gdscript)
            .then(tree_sitter_gdscript::ScannerScope::begin)
            .transpose()
            .expect("native test scanner scope begins");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(family))
            .expect("audited grammar ABI is supported");
        let tree = parser
            .parse(source, None)
            .expect("test parser has a language");
        drop(parser);
        if let Some(scope) = scope {
            scope
                .finish()
                .expect("native test scanner scope admits the tree");
        }
        tree
    }
}

#[cfg(test)]
mod haxe_native_input_qualification;

#[cfg(test)]
mod ring_native_input_qualification;

#[cfg(test)]
mod crossing_metadata_parent_tests {
    use super::*;

    fn draft(start: usize, end: usize, role: StructuralRole, native_depth: usize) -> FactDraft {
        FactDraft {
            start,
            end,
            role,
            syntax_kind: SyntaxKindLabel::new(&format!("native.written.{}", role.label())).unwrap(),
            parent: None,
            depth: 0,
            native_depth,
        }
    }

    fn crossing_drafts() -> Vec<FactDraft> {
        vec![
            draft(0, 319, StructuralRole::Root, 0),
            draft(0, 319, StructuralRole::Module, 0),
            draft(3, 184, StructuralRole::Module, 0),
            draft(161, 166, StructuralRole::Declaration, 3),
            draft(161, 166, StructuralRole::Definition, 3),
            draft(161, 272, StructuralRole::Signature, 0),
        ]
    }

    #[test]
    fn crossing_signature_keeps_native_child_module_declaration_and_definition_owners() {
        let mut drafts = crossing_drafts();
        assign_fact_parents(&mut drafts, &Cancellation::new()).unwrap();
        assert_eq!(drafts[2].parent, Some(1));
        assert_eq!(
            drafts[5].parent,
            Some(1),
            "cross-region metadata belongs to the common host"
        );
        assert_eq!(
            drafts[3].parent,
            Some(2),
            "metadata must not close the native child module"
        );
        assert_eq!(
            drafts[4].parent,
            Some(3),
            "the Definition keeps its own Declaration"
        );
        assert_eq!(drafts[3].depth, 3);
        assert_eq!(drafts[4].depth, 4);
    }

    #[test]
    fn ordinary_scope_advancement_and_expired_noncontainer_parent_search_remain_exact() {
        let mut drafts = vec![
            draft(0, 100, StructuralRole::Root, 0),
            draft(0, 100, StructuralRole::Module, 0),
            draft(10, 30, StructuralRole::Scope, 1),
            draft(12, 18, StructuralRole::Declaration, 2),
            draft(12, 15, StructuralRole::Definition, 3),
            draft(13, 14, StructuralRole::Signature, 4),
            draft(32, 34, StructuralRole::Reference, 1),
            draft(50, 70, StructuralRole::Scope, 1),
            draft(55, 60, StructuralRole::Declaration, 2),
            draft(55, 58, StructuralRole::Definition, 3),
            draft(80, 84, StructuralRole::Reference, 1),
        ];
        assign_fact_parents(&mut drafts, &Cancellation::new()).unwrap();
        assert_eq!(
            drafts.iter().map(|fact| fact.parent).collect::<Vec<_>>(),
            vec![
                None,
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(3),
                Some(1),
                Some(1),
                Some(7),
                Some(8),
                Some(1)
            ]
        );
    }

    #[test]
    fn equal_boundary_duplicate_and_empty_metadata_keep_source_owner_rules() {
        for end in [0, 319] {
            let mut drafts = vec![
                draft(0, end, StructuralRole::Root, 0),
                draft(0, end, StructuralRole::Module, 0),
                draft(0, end, StructuralRole::Signature, 0),
                draft(0, end, StructuralRole::Signature, 0),
            ];
            assign_fact_parents(&mut drafts, &Cancellation::new()).unwrap();
            assert_eq!(
                drafts.iter().map(|fact| fact.parent).collect::<Vec<_>>(),
                vec![None, Some(0), Some(1), Some(1)]
            );
        }
        let mut drafts = crossing_drafts();
        drafts.push(draft(161, 272, StructuralRole::Signature, 0));
        drafts.push(draft(161, 161, StructuralRole::Signature, 0));
        assign_fact_parents(&mut drafts, &Cancellation::new()).unwrap();
        assert_eq!(drafts[3].parent, Some(2));
        assert_eq!(drafts[4].parent, Some(3));
        assert_eq!(drafts[5].parent, Some(1));
        assert_eq!(drafts[6].parent, Some(1));
        assert_eq!(drafts[7].parent, Some(3));
    }

    #[test]
    fn cancelled_parent_assignment_refuses_before_draft_state_changes() {
        let mut drafts = crossing_drafts();
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            assign_fact_parents(&mut drafts, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(
            drafts
                .iter()
                .all(|fact| fact.parent.is_none() && fact.depth == 0)
        );
    }
}

#[cfg(test)]
mod coffeescript_callback_owner_tests;
#[cfg(test)]
mod coffeescript_scope_tests;
