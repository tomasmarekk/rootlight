//! Composed native-isolation coverage for the reviewed five-language holdout.
//!
//! The independent scorer remains in `rootlight-bench`. This test proves that
//! the same source corpus preserves its resolution classes after crossing the
//! adapter protocol, resource quotas, native sandbox, and normalized-IR gate.

#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rootlight_adapter_host::{
    PROJECT_ADAPTER_NAME, PROJECT_ADAPTER_VERSION, execute_isolated_project_adapter,
    negotiate_project_adapter_session, project_adapter_advertisement,
};
use rootlight_bench::verify_project_semantic_holdout_document;
use rootlight_cancel::Cancellation;
use rootlight_ids::{
    FileIdentity, GenerationId, RepositoryId, content_hash, derive_file, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, OccurrenceRole, OccurrenceTarget,
    RelationPredicate,
};
use rootlight_protocol::{
    adapter_contract::{ADAPTER_NONCE_BYTES, NegotiatedSession},
    generated::{
        adapter::v1::{
            ProjectAnalysisRequest, ProjectInput, RequestedAnalysisTier, ResourceLimits,
        },
        common::v1::{
            ContentHash as WireContentHash, FileId as WireFileId, GenerationId as WireGenerationId,
            RepositoryId as WireRepositoryId,
        },
    },
};
use rootlight_vfs::RelativePath;

const RUST_SOURCES: &[SourceFixture] = &[
    SourceFixture::new(
        "holdout/dep_a.rs",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/rust/dep_a.rs"
        ),
    ),
    SourceFixture::new(
        "holdout/dep_b.rs",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/rust/dep_b.rs"
        ),
    ),
    SourceFixture::new(
        "holdout/main.rs",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/rust/main.rs"
        ),
    ),
];
const TYPESCRIPT_SOURCES: &[SourceFixture] = &[
    SourceFixture::new(
        "holdout/dep-a.ts",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/typescript/dep-a.ts"
        ),
    ),
    SourceFixture::new(
        "holdout/dep-b.ts",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/typescript/dep-b.ts"
        ),
    ),
    SourceFixture::new(
        "holdout/main.ts",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/typescript/main.ts"
        ),
    ),
];
const JAVASCRIPT_SOURCES: &[SourceFixture] = &[
    SourceFixture::new(
        "holdout/dep-a.js",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/javascript/dep-a.js"
        ),
    ),
    SourceFixture::new(
        "holdout/dep-b.js",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/javascript/dep-b.js"
        ),
    ),
    SourceFixture::new(
        "holdout/main.js",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/javascript/main.js"
        ),
    ),
];
const PYTHON_SOURCES: &[SourceFixture] = &[
    SourceFixture::new(
        "holdout_dep_a.py",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/python/holdout_dep_a.py"
        ),
    ),
    SourceFixture::new(
        "holdout_dep_b.py",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/python/holdout_dep_b.py"
        ),
    ),
    SourceFixture::new(
        "holdout_main.py",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/python/holdout_main.py"
        ),
    ),
];
const GO_SOURCES: &[SourceFixture] = &[
    SourceFixture::new(
        "holdout/dep_a/dep_a.go",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/go/dep_a/dep.go"
        ),
    ),
    SourceFixture::new(
        "holdout/dep_b/dep_b.go",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/go/dep_b/dep.go"
        ),
    ),
    SourceFixture::new(
        "holdout/main/main.go",
        include_bytes!(
            "../../../crates/rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/go/main/main.go"
        ),
    ),
];
const LANGUAGE_CASES: &[LanguageCase] = &[
    LanguageCase::new("rust", RUST_SOURCES),
    LanguageCase::new("typescript", TYPESCRIPT_SOURCES),
    LanguageCase::new("javascript", JAVASCRIPT_SOURCES),
    LanguageCase::new("python", PYTHON_SOURCES),
    LanguageCase::new("go", GO_SOURCES),
];

#[derive(Clone, Copy)]
struct SourceFixture {
    path: &'static str,
    source: &'static [u8],
}

impl SourceFixture {
    const fn new(path: &'static str, source: &'static [u8]) -> Self {
        Self { path, source }
    }
}

#[derive(Clone, Copy)]
struct LanguageCase {
    language: &'static str,
    sources: &'static [SourceFixture],
}

impl LanguageCase {
    const fn new(language: &'static str, sources: &'static [SourceFixture]) -> Self {
        Self { language, sources }
    }
}

#[test]
fn reviewed_five_language_holdout_survives_the_isolated_product_path() {
    let executable = adapter_executable();
    for case in LANGUAGE_CASES {
        let limits = resource_limits();
        let session = negotiated_session(&executable, limits);
        let request = project_request(&session, *case);
        let output = execute_isolated_project_adapter(
            &executable,
            &session,
            &request,
            &Default::default(),
            &deadline(),
        )
        .unwrap_or_else(|error| panic!("{} isolated holdout failed: {error}", case.language));
        let document = output.document();

        assert!(
            output.isolation().permits_deep_adapter(),
            "{} deep adapter was not isolated",
            case.language
        );
        verify_project_semantic_holdout_document(case.language, document, build_context(*case))
            .unwrap_or_else(|error| {
                panic!(
                    "{} composed answer-key verification failed: {error}",
                    case.language
                )
            });
        assert_eq!(document.files.len(), 3, "{} file count", case.language);
        assert!(document.provenance.iter().all(|provenance| {
            provenance.producer.name() == PROJECT_ADAPTER_NAME
                && provenance.producer.version() == PROJECT_ADAPTER_VERSION
                && provenance.tier == AnalysisTier::TierB
        }));

        let calls = document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 12, "{} call-site count", case.language);
        assert_eq!(
            calls
                .iter()
                .filter(|occurrence| matches!(occurrence.target, OccurrenceTarget::Resolved { .. }))
                .count(),
            8,
            "{} exact resolution count",
            case.language
        );
        assert_eq!(
            calls
                .iter()
                .filter(|occurrence| {
                    matches!(
                        &occurrence.target,
                        OccurrenceTarget::Candidates {
                            symbols,
                            total_count: 2,
                            completeness: CoverageStatus::Unknown,
                        } if symbols.len() == 2
                    )
                })
                .count(),
            2,
            "{} dynamic candidate count",
            case.language
        );
        assert_eq!(
            calls
                .iter()
                .filter(|occurrence| {
                    matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
                })
                .count(),
            2,
            "{} unresolved count",
            case.language
        );

        let relation_count = |predicate| {
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == predicate)
                .count()
        };
        assert_eq!(
            relation_count(RelationPredicate::Calls),
            8,
            "{} exact call relations",
            case.language
        );
        assert_eq!(
            relation_count(RelationPredicate::DispatchCandidate),
            4,
            "{} dispatch candidate relations",
            case.language
        );
        assert!(
            relation_count(RelationPredicate::Imports) >= 2,
            "{} import relations",
            case.language
        );
        assert!(
            document.relations.iter().any(|relation| matches!(
                relation.predicate,
                RelationPredicate::Embeds
                    | RelationPredicate::Extends
                    | RelationPredicate::Implements
            )),
            "{} hierarchy relation",
            case.language
        );
    }
}

fn adapter_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rootlight-adapter-host"))
}

fn resource_limits() -> ResourceLimits {
    ResourceLimits {
        wall_time_ms: 30_000,
        cpu_time_ms: 20_000,
        memory_bytes: 512 * 1024 * 1024,
        input_bytes: 1024 * 1024,
        output_bytes: 8 * 1024 * 1024,
        files: 16,
        processes: 1,
        handles: 64,
        retries: 0,
    }
}

fn negotiated_session(executable: &Path, limits: ResourceLimits) -> NegotiatedSession {
    let advertisement =
        project_adapter_advertisement(executable).expect("project advertisement validates");
    assert_eq!(advertisement.identity().name, PROJECT_ADAPTER_NAME);
    assert!(advertisement.hard_limits().output_bytes >= limits.output_bytes);
    negotiate_project_adapter_session(executable, [11; ADAPTER_NONCE_BYTES], limits)
        .expect("project adapter session negotiates")
}

fn project_request(session: &NegotiatedSession, case: LanguageCase) -> ProjectAnalysisRequest {
    let repository = repository(case.language);
    let generation = GenerationId::from_bytes([17; 20]);
    let context_manifest = format!(r#"{{"language":"{}"}}"#, case.language).into_bytes();
    let build_context = build_context(case).digest();
    let inputs = case
        .sources
        .iter()
        .map(|fixture| {
            let relative =
                RelativePath::parse(Path::new(fixture.path)).expect("fixture path is valid");
            let file = derive_file(FileIdentity {
                repository,
                path_identity: relative.identity_bytes(),
            })
            .id();
            ProjectInput {
                file: Some(WireFileId {
                    value: file.as_bytes().to_vec(),
                }),
                path: fixture.path.to_owned(),
                language: case.language.to_owned(),
                source_digest: Some(WireContentHash {
                    value: content_hash(fixture.source).as_bytes().to_vec(),
                }),
                source: fixture.source.to_vec(),
                generated: false,
                origins: Vec::new(),
            }
        })
        .collect();
    ProjectAnalysisRequest {
        session_id: session.session_id().to_vec(),
        request_id: vec![23; ADAPTER_NONCE_BYTES],
        repository: Some(WireRepositoryId {
            value: repository.as_bytes().to_vec(),
        }),
        generation: Some(WireGenerationId {
            value: generation.as_bytes().to_vec(),
        }),
        analysis_unit: format!("semantic-holdout.{}", case.language),
        target: format!("//semantic-holdout:{}", case.language),
        build_context: Some(WireContentHash {
            value: build_context.as_bytes().to_vec(),
        }),
        config_digest: Some(WireContentHash {
            value: content_hash(&context_manifest).as_bytes().to_vec(),
        }),
        inputs,
        context_manifest,
        requested_tier: RequestedAnalysisTier::TierB as i32,
    }
}

fn build_context(case: LanguageCase) -> BuildContextIdentity {
    BuildContextIdentity::new(content_hash(
        format!("holdout-context:{}", case.language).as_bytes(),
    ))
}

fn repository(language: &str) -> RepositoryId {
    derive_repository(format!("rootlight-native-holdout:{language}").as_bytes()).id()
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(60))
            .expect("test deadline derives"),
    )
}
