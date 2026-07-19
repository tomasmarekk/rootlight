//! Public-boundary contracts for semantic context admission and emission.
//!
//! Fixtures bind canonical Tier B facts to an immutable source and build
//! identity, then verify deterministic transactional output and fail-closed use.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapters::{
    ContextAnalyzer, ImportedSemanticContext, LanguageProfile, LanguageSemantics,
    initial_semantic_registry,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageRecord, CoverageScope, CoverageStatus,
    ExtensionSupport, FactDomain, FactEvidence, FileRecord, IrLimits, NormalizedIrDocument,
    ProducerIdentity, ProducerKind, ProvenanceRecord, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir_in;

const SOURCE: &[u8] = b"pub fn target() {}\nfn caller() { target(); }\n";
const DOMAINS: [FactDomain; 8] = [
    FactDomain::Files,
    FactDomain::Entities,
    FactDomain::Occurrences,
    FactDomain::Relations,
    FactDomain::Provenance,
    FactDomain::SourceMappings,
    FactDomain::Diagnostics,
    FactDomain::Extensions,
];

#[test]
fn imported_context_commits_deterministically_and_rejects_other_builds() {
    let fixture = Fixture::new();
    let limits = limits();
    let context = ImportedSemanticContext::new(
        "rust",
        AnalysisTier::TierB,
        fixture.build_context,
        fixture.document.clone(),
        limits.ir(),
    )
    .expect("fixture context is valid");
    let profile = LanguageProfile::new("rust", AnalysisTier::TierA, LanguageSemantics::Static)
        .expect("Rust profile is valid");
    let analyzer = ContextAnalyzer::new(context, profile, ProducerKind::Compiler)
        .expect("context analyzer configuration is valid");
    let request = fixture.request(fixture.build_context, &limits);

    let first = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("matching context commits");
    let repeated = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("matching context repeats");
    assert_eq!(first.document(), repeated.document());
    assert_eq!(first.report(), repeated.report());
    assert_eq!(first.report().coverage().tier(), AnalysisTier::TierB);
    assert_eq!(first.report().coverage().domains().len(), DOMAINS.len());

    let wrong_request = fixture.request(
        BuildContextIdentity::new(content_hash(b"other-build")),
        &limits,
    );
    assert!(
        execute_analysis(
            &analyzer,
            &wrong_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(),
        )
        .is_err()
    );
}

#[test]
fn shared_registry_keeps_language_profiles_bounded_and_independent() {
    let registry = initial_semantic_registry().expect("built-in registry is valid");
    let observed = registry
        .iter()
        .map(|profile| {
            (
                profile.language().as_str(),
                profile.maximum_tier(),
                profile.semantics(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            ("go", AnalysisTier::TierA, LanguageSemantics::Static),
            (
                "javascript",
                AnalysisTier::TierB,
                LanguageSemantics::Dynamic,
            ),
            ("python", AnalysisTier::TierB, LanguageSemantics::Dynamic,),
            ("rust", AnalysisTier::TierA, LanguageSemantics::Static),
            ("typescript", AnalysisTier::TierA, LanguageSemantics::Static,),
        ]
    );

    let rust = LanguageId::new("rust").expect("language identity is valid");
    assert_eq!(
        registry
            .get(&rust)
            .expect("Rust profile is registered")
            .language(),
        &rust
    );
}

#[test]
fn context_requires_deep_tier_and_complete_domain_accounting() {
    let fixture = Fixture::new();
    let limits = limits();
    assert!(
        ImportedSemanticContext::new(
            "rust",
            AnalysisTier::TierD,
            fixture.build_context,
            fixture.document.clone(),
            limits.ir(),
        )
        .is_err()
    );

    let mut incomplete = fixture.document;
    incomplete.coverage_records.pop();
    assert!(
        ImportedSemanticContext::new(
            "rust",
            AnalysisTier::TierB,
            fixture.build_context,
            incomplete,
            limits.ir(),
        )
        .is_err()
    );
}

struct Fixture {
    _temporary: tempfile::TempDir,
    snapshot: rootlight_vfs::SourceSnapshot,
    source: SourceRef,
    build_context: BuildContextIdentity,
    document: NormalizedIrDocument,
}

impl Fixture {
    fn new() -> Self {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("local temporary directory is available");
        fs::create_dir_all(temporary.path().join("src")).expect("source directory is created");
        fs::write(temporary.path().join("src/lib.rs"), SOURCE).expect("fixture source is written");
        let repository = derive_repository(b"context-import-fixture").id();
        let root =
            RepositoryRoot::open(repository, temporary.path()).expect("temporary root opens");
        let relative = RelativePath::parse(Path::new("src/lib.rs")).expect("path is valid");
        let snapshot = root
            .snapshot(&relative, 1024 * 1024)
            .expect("fixture snapshot is stable");
        let generation = GenerationId::from_bytes([17; 20]);
        let byte_length = u64::try_from(SOURCE.len()).expect("fixture length fits");
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(snapshot.file(), 0, byte_length).expect("source span is valid"),
            snapshot.content_hash(),
            None,
        );
        let build_context = BuildContextIdentity::new(content_hash(b"context-import-build"));
        let provenance = FactId::from_bytes([3; 20]);
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.provenance.push(ProvenanceRecord {
            id: provenance,
            repository,
            generation,
            producer_kind: ProducerKind::Compiler,
            producer: ProducerIdentity::new("fixture-compiler", "1.0", build_context.digest())
                .expect("producer identity is valid"),
            binary_digest: content_hash(b"fixture-binary"),
            frontend_version: Some("fixture-frontend-1".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            build_context,
            input_sources: vec![source.clone()],
            evidence_sources: vec![source.clone()],
            derivation_parents: Vec::new(),
            rule: None,
        });
        document.files.push(FileRecord {
            id: snapshot.file(),
            repository,
            generation,
            path: "src/lib.rs".to_owned(),
            path_locator: None,
            content_hash: snapshot.content_hash(),
            byte_length,
            language: "rust".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance,
            evidence: direct_evidence(&source),
        });
        for (index, domain) in DOMAINS.into_iter().enumerate() {
            let discovered = match domain {
                FactDomain::Files | FactDomain::Provenance => 1,
                _ => 0,
            };
            document.coverage_records.push(CoverageRecord {
                id: FactId::from_bytes(
                    [u8::try_from(index + 20).expect("coverage identity seed fits"); 20],
                ),
                repository,
                generation,
                scope: CoverageScope::File(snapshot.file()),
                domain,
                tier: AnalysisTier::TierB,
                status: CoverageStatus::Complete,
                discovered,
                indexed: discovered,
                skipped: 0,
                provenance,
                evidence: direct_evidence(&source),
            });
        }
        Self {
            _temporary: temporary,
            snapshot,
            source,
            build_context,
            document,
        }
    }

    fn request<'a>(
        &'a self,
        build_context: BuildContextIdentity,
        limits: &'a AnalysisLimits,
    ) -> AnalysisRequest<'a> {
        AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source)
                .expect("snapshot binds to source"),
            LanguageId::new("rust").expect("language is valid"),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierB,
            build_context,
            limits,
        )
        .expect("request is valid")
        .with_generated_status(false)
    }
}

fn direct_evidence(source: &SourceRef) -> FactEvidence {
    FactEvidence {
        source: Some(source.clone()),
        derivation: Vec::new(),
    }
}

fn limits() -> AnalysisLimits {
    let batch =
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).expect("batch limits are valid");
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    )
}
