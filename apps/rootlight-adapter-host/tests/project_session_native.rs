//! Live native coverage for the production isolated project-session binary.
//!
//! These tests cross the platform containment, bounded-pipe, SDK, and
//! normalized-IR validation boundaries instead of substituting an in-process
//! child handler.

use std::{
    fs,
    io::{Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rootlight_adapter_host::{
    AdapterHostError, PROJECT_ADAPTER_NAME, PROJECT_ADAPTER_VERSION,
    execute_isolated_project_adapter, negotiate_project_adapter_session,
    project_adapter_advertisement,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    FileIdentity, GenerationId, RepositoryId, content_hash, derive_file, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, CoverageScope, CoverageStatus, ExtensionSupport, FactDomain, OccurrenceRole,
    RelationPredicate,
};
use rootlight_protocol::{
    adapter_contract::{ADAPTER_NONCE_BYTES, NegotiatedSession},
    generated::{
        adapter::v1::{
            GeneratedOrigin, ProjectAnalysisRequest, ProjectInput, RequestedAnalysisTier,
            ResourceLimits,
        },
        common::v1::{
            ContentHash as WireContentHash, FileId as WireFileId, GenerationId as WireGenerationId,
            RepositoryId as WireRepositoryId,
        },
    },
};
use rootlight_vfs::RelativePath;

#[test]
fn actual_isolated_binary_returns_multifile_tier_b() {
    let executable = adapter_executable();
    let limits = resource_limits(8 * 1024 * 1024);
    let session = negotiated_session(&executable, limits);
    let origin_source = b"pub fn ping() {}\n".as_slice();
    let generated_source = b"pub fn generated() {}\n".as_slice();
    let request = project_request(
        &session,
        "rust",
        &[
            ("src/dep.rs", origin_source, false, Vec::new()),
            (
                "src/generated.rs",
                generated_source,
                true,
                vec![GeneratedOrigin {
                    generated_start_byte: 0,
                    generated_end_byte: u64::try_from(generated_source.len())
                        .expect("generated fixture length is representable"),
                    origin_path: "src/dep.rs".to_owned(),
                    origin_start_byte: 0,
                    origin_end_byte: u64::try_from(origin_source.len())
                        .expect("origin fixture length is representable"),
                    transformation: "fixture-gen".to_owned(),
                    generator_digest: Some(WireContentHash {
                        value: content_hash(b"fixture-generator").as_bytes().to_vec(),
                    }),
                }],
            ),
            (
                "src/main.rs",
                b"use crate::dep::ping;\npub fn run() { ping(); }\n".as_slice(),
                false,
                Vec::new(),
            ),
        ],
    );

    let output = execute_isolated_project_adapter(
        &executable,
        &session,
        &request,
        &ExtensionSupport::default(),
        &deadline(),
    )
    .expect("the real isolated project adapter succeeds");

    assert!(output.isolation().permits_deep_adapter());
    assert_eq!(output.document().files.len(), 3);
    assert!(output.document().provenance.iter().all(|provenance| {
        provenance.producer.name() == PROJECT_ADAPTER_NAME
            && provenance.producer.version() == PROJECT_ADAPTER_VERSION
            && provenance.tier == AnalysisTier::TierB
    }));
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    );
    assert!(
        output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(!output.document().source_mappings.is_empty());
    let generated_file = output
        .document()
        .files
        .iter()
        .find(|file| file.path == "src/generated.rs")
        .expect("generated file is present");
    let mapping_coverage = output
        .document()
        .coverage_records
        .iter()
        .find(|coverage| {
            coverage.scope == CoverageScope::File(generated_file.id)
                && coverage.domain == FactDomain::SourceMappings
        })
        .expect("generated source mapping coverage is present");
    assert_eq!(mapping_coverage.status, CoverageStatus::Complete);
    assert_eq!(
        (
            mapping_coverage.discovered,
            mapping_coverage.indexed,
            mapping_coverage.skipped,
        ),
        (1, 1, 0)
    );
}

#[test]
fn actual_isolated_binary_preserves_malformed_source_diagnostics() {
    let executable = adapter_executable();
    let limits = resource_limits(8 * 1024 * 1024);
    let session = negotiated_session(&executable, limits);
    let request = project_request(
        &session,
        "python",
        &[
            (
                "dep.py",
                b"def ping():\n    pass\n".as_slice(),
                false,
                Vec::new(),
            ),
            (
                "main.py",
                b"from dep import ping\ndef run(:\n    ping()\n".as_slice(),
                false,
                Vec::new(),
            ),
        ],
    );

    let output = execute_isolated_project_adapter(
        &executable,
        &session,
        &request,
        &ExtensionSupport::default(),
        &deadline(),
    )
    .expect("recoverable malformed input commits one isolated transaction");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    assert!(!output.document().skipped_regions.is_empty());
    assert!(
        output
            .document()
            .coverage_records
            .iter()
            .any(|coverage| coverage.status != CoverageStatus::Complete)
    );
}

#[test]
fn actual_child_output_quota_and_parent_cancellation_fail_closed() {
    let executable = adapter_executable();
    let limits = resource_limits(512);
    let session = negotiated_session(&executable, limits);
    let request = project_request(
        &session,
        "rust",
        &[
            (
                "src/dep.rs",
                b"pub fn ping() {}\n".as_slice(),
                false,
                Vec::new(),
            ),
            (
                "src/main.rs",
                b"use crate::dep::ping;\npub fn run() { ping(); }\n".as_slice(),
                false,
                Vec::new(),
            ),
        ],
    );
    assert!(matches!(
        execute_isolated_project_adapter(
            &executable,
            &session,
            &request,
            &ExtensionSupport::default(),
            &deadline(),
        ),
        Err(AdapterHostError::ProcessFailed)
    ));

    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        execute_isolated_project_adapter(
            &executable,
            &session,
            &request,
            &ExtensionSupport::default(),
            &cancellation,
        ),
        Err(AdapterHostError::Cancelled(_))
    ));
}

#[test]
fn negotiated_digest_rejects_a_replaced_executable_before_entry() {
    let executable = adapter_executable();
    let limits = resource_limits(8 * 1024 * 1024);
    let session = negotiated_session(&executable, limits);
    let replacement = TemporaryExecutable::copy_from(&executable);
    replacement.corrupt_first_byte();
    let request = project_request(
        &session,
        "rust",
        &[(
            "src/main.rs",
            b"pub fn run() {}\n".as_slice(),
            false,
            Vec::new(),
        )],
    );

    assert!(matches!(
        execute_isolated_project_adapter(
            replacement.path(),
            &session,
            &request,
            &ExtensionSupport::default(),
            &deadline(),
        ),
        Err(AdapterHostError::Process)
    ));
}

struct TemporaryExecutable(PathBuf);

impl TemporaryExecutable {
    fn copy_from(source: &Path) -> Self {
        let path = std::env::temp_dir().join(format!(
            ".rootlight-adapter-digest-test-{}{}",
            std::process::id(),
            std::env::consts::EXE_SUFFIX
        ));
        let _ = fs::remove_file(&path);
        fs::copy(source, &path).expect("fixture executable copies");
        Self(path)
    }

    fn corrupt_first_byte(&self) {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&self.0)
            .expect("fixture executable opens for mutation");
        file.seek(SeekFrom::Start(0))
            .expect("fixture executable seeks");
        file.write_all(&[0xa5]).expect("fixture executable mutates");
        file.sync_all().expect("fixture mutation syncs");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryExecutable {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn adapter_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rootlight-adapter-host"))
}

fn resource_limits(output_bytes: u64) -> ResourceLimits {
    ResourceLimits {
        wall_time_ms: 30_000,
        cpu_time_ms: 20_000,
        memory_bytes: 512 * 1024 * 1024,
        input_bytes: 1024 * 1024,
        output_bytes,
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
    negotiate_project_adapter_session(executable, [7; ADAPTER_NONCE_BYTES], limits)
        .expect("project adapter session negotiates")
}

fn project_request(
    session: &NegotiatedSession,
    language: &str,
    sources: &[(&str, &[u8], bool, Vec<GeneratedOrigin>)],
) -> ProjectAnalysisRequest {
    let repository = repository();
    let generation = GenerationId::from_bytes([2; 20]);
    let context_manifest = format!(r#"{{"language":"{language}"}}"#).into_bytes();
    let build_context = content_hash(format!("build-context:{language}").as_bytes());
    let inputs = sources
        .iter()
        .map(|(path, source, generated, origins)| {
            let relative = RelativePath::parse(Path::new(path)).expect("fixture path is valid");
            let file = derive_file(FileIdentity {
                repository,
                path_identity: relative.identity_bytes(),
            })
            .id();
            let source_digest = content_hash(source);
            ProjectInput {
                file: Some(WireFileId {
                    value: file.as_bytes().to_vec(),
                }),
                path: (*path).to_owned(),
                language: language.to_owned(),
                source_digest: Some(WireContentHash {
                    value: source_digest.as_bytes().to_vec(),
                }),
                source: source.to_vec(),
                generated: *generated,
                origins: origins.clone(),
            }
        })
        .collect();
    ProjectAnalysisRequest {
        session_id: session.session_id().to_vec(),
        request_id: vec![9; ADAPTER_NONCE_BYTES],
        repository: Some(WireRepositoryId {
            value: repository.as_bytes().to_vec(),
        }),
        generation: Some(WireGenerationId {
            value: generation.as_bytes().to_vec(),
        }),
        analysis_unit: format!("fixture.{language}"),
        target: format!("//fixture:{language}"),
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

fn repository() -> RepositoryId {
    derive_repository(b"rootlight-project-session-native").id()
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(60))
            .expect("test deadline derives"),
    )
}
