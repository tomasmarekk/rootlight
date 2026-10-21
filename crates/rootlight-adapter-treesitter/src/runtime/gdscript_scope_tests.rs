//! Cache reuse requires the exact checked scanner frame independently of version.
//! The positive canary uses the same thread-affine native scope as production.

use super::*;

#[test]
fn gdscript_cached_tree_requires_exact_scanner_status_source_contract() {
    let source = b"class_name Written\n";
    let scope = tree_sitter_gdscript::ScannerScope::begin().unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_gdscript::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    scope.finish().unwrap();
    assert!(!tree.root_node().has_error());
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Gdscript;
    identity.grammar_version = "6.1.0";
    identity.encoding = "utf-8".to_owned();
    identity.content_hash = rootlight_ids::content_hash(source);
    let mut cached = CachedParse::invalidation(ReuseInvalidation::FrontendRoute);
    cached.invalidation = None;
    cached.identity = identity.clone();
    cached.source = Arc::new(source.to_vec());
    cached.tree = Some(tree);
    let (old, status) = prepare_reuse(
        Some(&cached),
        &identity,
        source,
        &[],
        source.len(),
        &Cancellation::new(),
    )
    .unwrap();
    assert!(old.is_some());
    assert_eq!(status, ReuseStatus::Reused { changed_ranges: 0 });
    for predecessor in [
        "gdscript-pristine-scanner",
        "gdscript-scanner-checked-state/1;scanner=stale",
        "gdscript-scanner-checked-state/1;status=stale",
        "gdscript-scanner-checked-state/1;controls=stale",
    ] {
        let mut changed = cached.clone_for_reuse();
        changed.gdscript_scanner_contract = predecessor;
        let (old, status) = prepare_reuse(
            Some(&changed),
            &identity,
            source,
            &[],
            source.len(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(old.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
}

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::AtomicBool,
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, StreamLimits,
};
use rootlight_cancel::CancellationReason;
use rootlight_ids::{SymbolId, content_hash};
use rootlight_ir::{
    BuildContextIdentity, ContainerRef, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};

use crate::TreeSitterAnalyzer;

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/gdscript/GroupCel.gd");
const POST_PARSE_PANIC: &str = "gdscript-native-post-parse-test-unwind";

thread_local! {
    static PANIC_AFTER_NATIVE_PARSE: Cell<bool> = const { Cell::new(false) };
}

/// Consume the test thread's one-shot unwind hook outside native callbacks.
pub(super) fn after_native_parse() {
    // Consume before unwinding so a reused worker thread cannot inherit the hook.
    if PANIC_AFTER_NATIVE_PARSE.replace(false) {
        std::panic::panic_any(POST_PARSE_PANIC);
    }
}

struct PostParsePanicReset;

impl PostParsePanicReset {
    fn arm() -> Self {
        assert!(!PANIC_AFTER_NATIVE_PARSE.replace(true));
        Self
    }
}

impl Drop for PostParsePanicReset {
    fn drop(&mut self) {
        PANIC_AFTER_NATIVE_PARSE.set(false);
    }
}

struct LifecycleFixture {
    _directory: tempfile::TempDir,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl LifecycleFixture {
    fn new(bytes: &[u8]) -> Self {
        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::write(directory.path().join("written.gd"), bytes).unwrap();
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let repository = RepositoryRoot::open(repository_id, directory.path()).unwrap();
        let relative = RelativePath::parse(std::path::Path::new("written.gd")).unwrap();
        let snapshot = repository.snapshot(&relative, 1024 * 1024).unwrap();
        let source = SourceRef::new(
            repository_id,
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .unwrap(),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _directory: directory,
            snapshot,
            source,
        }
    }

    fn request<'a>(&'a self, limits: &'a AnalysisLimits) -> AnalysisRequest<'a> {
        AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new("gdscript").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"gdscript-lifecycle-build")),
            limits,
        )
        .unwrap()
        .with_generated_status(false)
    }
}

fn lifecycle_provider() -> Arc<TreeSitterProvider> {
    let config = RuntimeConfig::new(
        1024 * 1024,
        1_000_000,
        4096,
        16,
        16,
        1,
        8 * 1024 * 1024,
        ParserSettings::new(1).unwrap(),
    )
    .unwrap();
    Arc::new(TreeSitterProvider::new_deferred(config).unwrap())
}

fn lifecycle_limits() -> AnalysisLimits {
    let batch = BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .unwrap();
    AnalysisLimits::new(
        1024 * 1024,
        1_000_000,
        4096,
        16,
        32 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
}

fn lifecycle_analyzer(provider: &Arc<TreeSitterProvider>) -> TreeSitterAnalyzer {
    let parser: Arc<dyn ParseProvider> = provider.clone();
    TreeSitterAnalyzer::new(
        parser,
        ProducerIdentity::new(
            "gdscript-lifecycle",
            "1.0",
            content_hash(b"gdscript-config"),
        )
        .unwrap(),
        LanguageId::new("gdscript").unwrap(),
        "gdscript-native-source",
        content_hash(b"gdscript-lifecycle-binary"),
    )
    .unwrap()
}

fn assert_returned_without_cache(provider: &TreeSitterProvider) {
    let stats = provider.stats();
    assert_eq!(stats.pooled_parsers, 1);
    assert_eq!(stats.available_parsers, 1);
    assert_eq!(stats.checked_out_parsers, 0);
    assert_eq!(stats.cache.entries, 0);
    assert_eq!(stats.cache.accounted_bytes, 0);
}

fn assert_current_source(source: &SourceRef, fixture: &LifecycleFixture) {
    assert_eq!(source.repository(), fixture.source.repository());
    assert_eq!(source.generation(), fixture.source.generation());
    assert_eq!(source.span().file(), fixture.source.span().file());
    assert_eq!(source.content_hash(), fixture.source.content_hash());
    assert!(source.span().end_byte() <= u64::try_from(ORIGINAL.len()).unwrap());
}

fn exact_original_definition(
    document: &NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact {kind:?} {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition must resolve to its own entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    symbol
}

fn assert_next_clean_original(provider: &Arc<TreeSitterProvider>) {
    let fixture = LifecycleFixture::new(ORIGINAL);
    let limits = lifecycle_limits();
    let request = fixture.request(&limits);
    let (output, artifact) = lifecycle_analyzer(provider)
        .analyze_and_capture(
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert!(
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap()
            > 0
    );
    let document = output.document();
    validate_ir_document(document, limits.ir(), &ExtensionSupport::default()).unwrap();
    for site in &document.occurrences {
        assert_current_source(&site.source, &fixture);
    }
    for entity in &document.entities {
        assert_current_source(entity.evidence.source.as_ref().unwrap(), &fixture);
    }
    let owner = exact_original_definition(document, "GroupCel", EntityKind::Class, 11, 19);
    for (name, kind, start, end) in [
        ("_init", EntityKind::Constructor, 181, 186),
        ("get_image", EntityKind::Method, 281, 290),
        ("get_class_name", EntityKind::Method, 384, 398),
    ] {
        let member = exact_original_definition(document, name, kind, start, end);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == member)
            .unwrap();
        assert_eq!(entity.container, Some(ContainerRef::Entity(owner)));
        let relations: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(member)
            })
            .collect();
        assert_eq!(relations.len(), 1);
        let source = relations[0].evidence.source.as_ref().unwrap();
        assert_current_source(source, &fixture);
        assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
    }
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 422
            && site.source.span().end_byte() == 430
    }));
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::RefersTo
    )));
    assert_returned_without_cache(provider);

    // A successful continuation proves that the empty cache was not disabled.
    let cached = provider
        .execute_with_previous(
            &request.to_parse_request(),
            None,
            &[],
            ParserSettings::new(1).unwrap(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert!(cached.previous().is_some());
    let stats = provider.stats();
    assert_eq!(stats.pooled_parsers, 1);
    assert_eq!(stats.available_parsers, 1);
    assert_eq!(stats.checked_out_parsers, 0);
    assert_eq!(stats.cache.entries, 1);
    assert!(stats.cache.accounted_bytes > 0);
}

#[test]
fn cancellation_during_native_lexing_returns_pool_and_scope_before_next_original() {
    let provider = lifecycle_provider();
    let fixture = LifecycleFixture::new(ORIGINAL);
    let limits = lifecycle_limits();
    let cancellation = Cancellation::new();
    let cancelled = cancellation.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&fired);
    let mut lease = provider.pool.acquire(&Cancellation::new()).unwrap();
    lease
        .parser_mut()
        .unwrap()
        .set_logger(Some(Box::new(move |kind, _| {
            // Cancel without panicking across the native logger boundary.
            if kind == tree_sitter::LogType::Lex && !observed.swap(true, Ordering::SeqCst) {
                let _ = cancelled.cancel(CancellationReason::ClientRequest);
            }
        })));
    drop(lease);
    assert_returned_without_cache(&provider);
    assert!(!fired.load(Ordering::SeqCst));
    assert!(cancellation.reason().is_none());

    let result = lifecycle_analyzer(&provider).analyze_and_capture(
        &fixture.request(&limits),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    );
    assert!(fired.load(Ordering::SeqCst));
    assert!(matches!(
        result,
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_returned_without_cache(&provider);
    assert_next_clean_original(&provider);
}

#[test]
fn scanner_capacity_refusal_returns_pool_and_scope_before_next_original() {
    let provider = lifecycle_provider();
    let mut source = String::from("class_name Capacity\nfunc _init():\n");
    for depth in 1..=1018 {
        source.push_str(&" ".repeat(depth));
        source.push_str("if true:\n");
    }
    source.push_str(&" ".repeat(1019));
    source.push_str("pass\n");
    let limits = lifecycle_limits();
    assert!(source.len() < limits.max_source_bytes());
    assert!(limits.max_syntax_depth() >= 2048);
    assert!(limits.max_syntax_nodes() >= 1_000_000);
    let fixture = LifecycleFixture::new(source.as_bytes());
    let result = lifecycle_analyzer(&provider).analyze_and_capture(
        &fixture.request(&limits),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    );
    assert!(matches!(result, Err(AdapterError::ProviderFailed { code })
        if code.as_str() == "gdscript-scanner-capacity"));
    assert_returned_without_cache(&provider);
    assert_next_clean_original(&provider);
}

#[test]
fn unwind_after_native_parse_returns_pool_and_scope_before_next_original() {
    let provider = lifecycle_provider();
    let fixture = LifecycleFixture::new(ORIGINAL);
    let limits = lifecycle_limits();
    let reset = PostParsePanicReset::arm();
    let result = catch_unwind(AssertUnwindSafe(|| {
        lifecycle_analyzer(&provider).analyze_and_capture(
            &fixture.request(&limits),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
    }));
    assert!(!PANIC_AFTER_NATIVE_PARSE.get());
    drop(reset);
    let payload = result.expect_err("the post-parse hook must unwind the actual product call");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&POST_PARSE_PANIC));
    assert_returned_without_cache(&provider);
    assert_next_clean_original(&provider);
}
