//! Offline BENCH-PARSE-001 micro-evidence executable.
//!
//! The dataset is embedded in this binary and materialized only into an
//! operation-owned temporary directory beside the result destination. Neither
//! that host path nor fixture source text enters the evidence bundle.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use rootlight_adapter_sdk::{AnalysisLimits, BatchThresholds, MemoryAdmissionPolicy, StreamLimits};
use rootlight_adapter_treesitter::{
    ADAPTER_VERSION, GrammarRegistry, ParserSettings, RuntimeConfig, TREE_SITTER_RUNTIME_VERSION,
    TreeSitterProvider,
};
use rootlight_bench::{
    Availability, BenchmarkCommand, BuildProvenance, BundleLimits, DatasetEntry, DatasetManifest,
    EnvironmentEvidence, EvidenceValue, ParserBenchmarkConfig, ParserDatasetInput,
    RESULT_BUNDLE_SCHEMA_VERSION, ResultBundle, UnavailableProcessTreeSampler,
    UnavailableSemanticFacts, publish_bundle, run_parser_benchmark, verify_bundle,
};
use rootlight_ids::{GenerationId, derive_repository};
use rootlight_ir::{IrLimits, SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const DATASET_ID: &str = "rootlight-m05-parser-micro-v1";
const BENCHMARK_SEED: u64 = 0x524f_4f54_4c49_4748;
const WARMUP_ROUNDS: u32 = 1;
const TRIAL_ROUNDS: u32 = 10;
const SAMPLE_TIMEOUT_MS: u64 = 2_000;
const SAMPLE_TIMEOUT: Duration = Duration::from_millis(SAMPLE_TIMEOUT_MS);
const MAX_ARGUMENTS: usize = 5;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

const FIXTURES: [EmbeddedFixture; 4] = [
    EmbeddedFixture {
        id: "java-basic",
        grammar_family: "java",
        language: "java",
        file_name: "RootlightFixture.java",
        source: b"final class RootlightFixture {\n    static int add(int a, int b) { return a + b; }\n}\n",
    },
    EmbeddedFixture {
        id: "javascript-basic",
        grammar_family: "javascript",
        language: "javascript",
        file_name: "fixture.js",
        source: b"export function add(a, b) {\n  return a + b;\n}\n",
    },
    EmbeddedFixture {
        id: "python-basic",
        grammar_family: "python",
        language: "python",
        file_name: "fixture.py",
        source: b"def add(a: int, b: int) -> int:\n    return a + b\n",
    },
    EmbeddedFixture {
        id: "rust-basic",
        grammar_family: "rust",
        language: "rust",
        file_name: "fixture.rs",
        source: b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    },
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("m05-parser-evidence: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), EvidenceError> {
    let arguments = Arguments::parse(env::args_os())?;
    let manifest = embedded_manifest()?;
    let command = benchmark_command();
    let fixture_directory = create_fixture_directory(&arguments.output)?;
    materialize_fixtures(fixture_directory.path())?;

    let limits = BundleLimits::default();
    let repository_id = derive_repository(DATASET_ID.as_bytes()).id();
    let repository = RepositoryRoot::open(repository_id, fixture_directory.path())?;
    let generation = generation_id(&manifest.revision);
    let mut inputs = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        let relative = RelativePath::parse(Path::new(&entry.relative_path))?;
        let snapshot = repository.snapshot(&relative, limits.max_snapshot_bytes)?;
        let end =
            u64::try_from(snapshot.content().len()).map_err(|_| EvidenceError::LimitConversion)?;
        let source = SourceRef::new(
            repository_id,
            generation,
            SourceSpan::new(snapshot.file(), 0, end)?,
            snapshot.content_hash(),
            None,
        );
        inputs.push(ParserDatasetInput {
            entry: entry.clone(),
            snapshot,
            source,
            included_ranges: Vec::new(),
        });
    }

    let provider = TreeSitterProvider::new(runtime_config()?)?;
    let benchmark = run_parser_benchmark(
        &provider,
        &inputs,
        &ParserBenchmarkConfig {
            seed: BENCHMARK_SEED,
            warmup_rounds: WARMUP_ROUNDS,
            trial_rounds: TRIAL_ROUNDS,
            timeout: SAMPLE_TIMEOUT,
            limits: analysis_limits()?,
            memory_policy: MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            evidence_limits: limits,
        },
        &UnavailableProcessTreeSampler,
        &UnavailableSemanticFacts,
    )?;
    drop(provider);
    drop(inputs);
    drop(repository);
    cleanup_fixture_directory(fixture_directory)?;

    let executable = env::current_exe().map_err(|source| EvidenceError::Io {
        operation: "locate evidence executable",
        source,
    })?;
    let binary_sha256 = hash_file(&executable, MAX_BINARY_BYTES)?;
    let bundle = ResultBundle {
        environment: environment(&binary_sha256)?,
        dataset_manifest: manifest,
        build_provenance: build_provenance(&arguments.source_revision, &binary_sha256),
        command,
        raw_samples: benchmark.raw_samples,
        summary: benchmark.summary,
        coverage: benchmark.coverage,
        quality: benchmark.quality,
        agent_trajectories: Vec::new(),
        profiles: BTreeMap::new(),
        logs: BTreeMap::new(),
    };
    publish_bundle(&bundle, &arguments.output)?;
    verify_bundle(&arguments.output)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct EmbeddedFixture {
    id: &'static str,
    grammar_family: &'static str,
    language: &'static str,
    file_name: &'static str,
    source: &'static [u8],
}

fn embedded_manifest() -> Result<DatasetManifest, EvidenceError> {
    let mut entries = Vec::with_capacity(FIXTURES.len());
    for fixture in FIXTURES {
        entries.push(DatasetEntry {
            id: fixture.id.to_owned(),
            grammar_family: fixture.grammar_family.to_owned(),
            language: fixture.language.to_owned(),
            relative_path: fixture.file_name.to_owned(),
            source_sha256: sha256_hex(fixture.source),
            source_bytes: u64::try_from(fixture.source.len())
                .map_err(|_| EvidenceError::LimitConversion)?,
            physical_lines: physical_lines(fixture.source)?,
            generated: false,
        });
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    let mut revision_hasher = Sha256::new();
    for entry in &entries {
        hash_length_prefixed(&mut revision_hasher, entry.id.as_bytes())?;
        hash_length_prefixed(&mut revision_hasher, entry.source_sha256.as_bytes())?;
    }
    Ok(DatasetManifest {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        dataset_id: DATASET_ID.to_owned(),
        revision: format!("sha256:{}", hex_digest(revision_hasher.finalize())),
        scope_rule: "embedded_fixture_set_v1".to_owned(),
        loc_counting_rule: "physical_lines_newline_terminated_v1".to_owned(),
        entries,
    })
}

fn benchmark_command() -> BenchmarkCommand {
    BenchmarkCommand {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        subcommand: "m05-parser-evidence".to_owned(),
        arguments: vec![
            format!("dataset_id={DATASET_ID}"),
            format!("fixture_count={}", FIXTURES.len()),
            format!("build_profile={}", build_profile()),
        ],
        seed: BENCHMARK_SEED,
        warmup_rounds: WARMUP_ROUNDS,
        trial_rounds: TRIAL_ROUNDS,
        timeout_ms: SAMPLE_TIMEOUT_MS,
    }
}

fn environment(binary_sha256: &str) -> Result<EnvironmentEvidence, EvidenceError> {
    let registry = GrammarRegistry::audited()?;
    let grammar_versions = registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            (
                descriptor.language().as_str().to_owned(),
                descriptor.grammar_version().to_owned(),
            )
        })
        .collect();
    let grammar_source_package_checksums = registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            (
                descriptor.language().as_str().to_owned(),
                descriptor.grammar_source_sha256().to_owned(),
            )
        })
        .collect();
    let mut grammar_hashes = BTreeMap::new();
    for descriptor in registry.descriptors() {
        let language = descriptor.language().as_str();
        grammar_hashes.insert(
            format!("{language}.parser"),
            descriptor.parser_sha256().to_owned(),
        );
        if let Some(scanner_sha256) = descriptor.scanner_sha256() {
            grammar_hashes.insert(format!("{language}.scanner"), scanner_sha256.to_owned());
        }
    }
    Ok(EnvironmentEvidence {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        cpu_model: EvidenceValue::unavailable("host_inventory_not_collected"),
        cpu_topology: EvidenceValue::unavailable("host_inventory_not_collected"),
        ram_bytes: EvidenceValue::unavailable("host_inventory_not_collected"),
        operating_system: EvidenceValue::observed(env::consts::OS.to_owned()),
        kernel: EvidenceValue::unavailable("host_inventory_not_collected"),
        filesystem: EvidenceValue::unavailable("host_inventory_not_collected"),
        storage_device: EvidenceValue::unavailable("host_inventory_not_collected"),
        power_mode: EvidenceValue::unavailable("host_inventory_not_collected"),
        container_limits: EvidenceValue::unavailable("host_inventory_not_collected"),
        compiler: EvidenceValue::observed(compiler_identity().to_owned()),
        binary_sha256: EvidenceValue::observed(binary_sha256.to_owned()),
        feature_profile: build_profile().to_owned(),
        sqlite: EvidenceValue::unavailable("sqlite_not_in_scope"),
        adapter_versions: EvidenceValue::observed(BTreeMap::from([
            (
                "rootlight-adapter-treesitter".to_owned(),
                ADAPTER_VERSION.to_owned(),
            ),
            (
                "tree-sitter-runtime".to_owned(),
                TREE_SITTER_RUNTIME_VERSION.to_owned(),
            ),
        ])),
        grammar_versions: EvidenceValue::observed(grammar_versions),
        grammar_source_package_checksums: EvidenceValue::observed(grammar_source_package_checksums),
        grammar_hashes: EvidenceValue::observed(grammar_hashes),
        locale: EvidenceValue::unavailable("host_inventory_not_collected"),
        background_process_policy: EvidenceValue::unavailable(
            "background_process_policy_not_recorded",
        ),
        clock_source: EvidenceValue::observed("std_instant_monotonic".to_owned()),
        process_tree_accounting: Availability::Unavailable {
            reason_code: "platform_process_tree_sampler_not_integrated".to_owned(),
        },
    })
}

fn build_provenance(source_revision: &str, binary_sha256: &str) -> BuildProvenance {
    BuildProvenance {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        source_revision: source_revision.to_owned(),
        binary_revision: format!("sha256:{binary_sha256}"),
        build_profile: build_profile().to_owned(),
        features: Vec::new(),
        target: target_triple().to_owned(),
    }
}

const fn build_profile() -> &'static str {
    env!("ROOTLIGHT_BENCH_PROFILE")
}

const fn target_triple() -> &'static str {
    env!("ROOTLIGHT_BENCH_TARGET")
}

const fn compiler_identity() -> &'static str {
    env!("ROOTLIGHT_BENCH_RUSTC")
}

fn runtime_config() -> Result<RuntimeConfig, rootlight_adapter_treesitter::RuntimeConfigError> {
    RuntimeConfig::new(
        1024 * 1024,
        250_000,
        512,
        32,
        64,
        1,
        16 * 1024 * 1024,
        ParserSettings::new(16 * 1024)?,
    )
}

fn analysis_limits() -> Result<AnalysisLimits, rootlight_adapter_sdk::LimitError> {
    let batch = BatchThresholds::new(2_048, 4 * 1024 * 1024, 2_048, 1024 * 1024)?;
    let stream = StreamLimits::new(
        4_096,
        250_000,
        64 * 1024 * 1024,
        16_384,
        8 * 1024 * 1024,
        16 * 1024 * 1024,
        batch,
    )?;
    AnalysisLimits::new(
        1024 * 1024,
        250_000,
        512,
        32,
        128 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
}

fn create_fixture_directory(destination: &Path) -> Result<TempDir, EvidenceError> {
    let parent = destination_parent(destination)?;
    tempfile::Builder::new()
        .prefix(".rootlight-m05-fixtures-")
        .tempdir_in(parent)
        .map_err(|source| EvidenceError::Io {
            operation: "create fixture directory",
            source,
        })
}

fn cleanup_fixture_directory(directory: TempDir) -> Result<(), EvidenceError> {
    directory.close().map_err(|source| EvidenceError::Io {
        operation: "remove fixture directory",
        source,
    })
}

fn destination_parent(destination: &Path) -> Result<PathBuf, EvidenceError> {
    if destination.file_name().is_none() {
        return Err(EvidenceError::InvalidDestination);
    }
    let parent = destination
        .parent()
        .ok_or(EvidenceError::InvalidDestination)?;
    if parent.as_os_str().is_empty() {
        env::current_dir().map_err(|source| EvidenceError::Io {
            operation: "resolve current directory",
            source,
        })
    } else {
        Ok(parent.to_owned())
    }
}

fn materialize_fixtures(directory: &Path) -> Result<(), EvidenceError> {
    for fixture in FIXTURES {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(fixture.file_name))
            .map_err(|source| EvidenceError::Io {
                operation: "create embedded fixture",
                source,
            })?;
        file.write_all(fixture.source)
            .map_err(|source| EvidenceError::Io {
                operation: "write embedded fixture",
                source,
            })?;
        file.sync_all().map_err(|source| EvidenceError::Io {
            operation: "sync embedded fixture",
            source,
        })?;
    }
    Ok(())
}

fn generation_id(revision: &str) -> GenerationId {
    let digest = Sha256::digest(revision.as_bytes());
    let mut bytes = [0_u8; 20];
    bytes.copy_from_slice(&digest[..20]);
    GenerationId::from_bytes(bytes)
}

fn physical_lines(bytes: &[u8]) -> Result<u64, EvidenceError> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    let trailing = usize::from(bytes.last() != Some(&b'\n'));
    let lines = newlines
        .checked_add(trailing)
        .ok_or(EvidenceError::LimitConversion)?;
    u64::try_from(lines).map_err(|_| EvidenceError::LimitConversion)
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), EvidenceError> {
    let length = u64::try_from(bytes.len()).map_err(|_| EvidenceError::LimitConversion)?;
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

fn hash_file(path: &Path, maximum_bytes: u64) -> Result<String, EvidenceError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| EvidenceError::Io {
        operation: "inspect evidence executable",
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum_bytes {
        return Err(EvidenceError::InvalidExecutable);
    }
    let mut file = File::open(path).map_err(|source| EvidenceError::Io {
        operation: "open evidence executable",
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(|source| EvidenceError::Io {
            operation: "hash evidence executable",
            source,
        })?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| EvidenceError::LimitConversion)?)
            .filter(|observed| *observed <= maximum_bytes)
            .ok_or(EvidenceError::InvalidExecutable)?;
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[derive(Debug)]
struct Arguments {
    output: PathBuf,
    source_revision: String,
}

impl Arguments {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, EvidenceError> {
        let mut arguments = arguments.into_iter();
        let mut argument_count = 0_usize;
        let _program =
            next_argument(&mut arguments, &mut argument_count)?.ok_or(EvidenceError::Usage)?;
        let mut values = BTreeMap::<String, OsString>::new();
        while let Some(flag) = next_argument(&mut arguments, &mut argument_count)? {
            let flag = flag.into_string().map_err(|_| EvidenceError::Usage)?;
            if !matches!(flag.as_str(), "--output" | "--source-revision")
                || values.contains_key(&flag)
            {
                return Err(EvidenceError::Usage);
            }
            let value =
                next_argument(&mut arguments, &mut argument_count)?.ok_or(EvidenceError::Usage)?;
            values.insert(flag, value);
        }
        if argument_count != MAX_ARGUMENTS {
            return Err(EvidenceError::Usage);
        }
        let output = values
            .remove("--output")
            .map(PathBuf::from)
            .ok_or(EvidenceError::Usage)?;
        let source_revision = values
            .remove("--source-revision")
            .ok_or(EvidenceError::Usage)?
            .into_string()
            .map_err(|_| EvidenceError::Usage)?;
        if !values.is_empty()
            || !matches!(source_revision.len(), 40 | 64)
            || !source_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(EvidenceError::Usage);
        }
        Ok(Self {
            output,
            source_revision,
        })
    }
}

fn next_argument<I>(
    arguments: &mut I,
    argument_count: &mut usize,
) -> Result<Option<OsString>, EvidenceError>
where
    I: Iterator<Item = OsString>,
{
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    *argument_count = argument_count.checked_add(1).ok_or(EvidenceError::Usage)?;
    if *argument_count > MAX_ARGUMENTS || argument.as_encoded_bytes().len() > MAX_ARGUMENT_BYTES {
        return Err(EvidenceError::Usage);
    }
    Ok(Some(argument))
}

#[derive(Debug, thiserror::Error)]
enum EvidenceError {
    #[error("usage: m05-parser-evidence --output PATH --source-revision LOWERCASE_SHA")]
    Usage,
    #[error("result destination is invalid")]
    InvalidDestination,
    #[error("evidence executable is invalid")]
    InvalidExecutable,
    #[error("resource limit is not representable")]
    LimitConversion,
    #[error("{operation} failed")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Vfs(#[from] rootlight_vfs::VfsError),
    #[error(transparent)]
    Span(#[from] rootlight_ir::IrValidationError),
    #[error(transparent)]
    Runtime(#[from] rootlight_adapter_treesitter::RuntimeConfigError),
    #[error(transparent)]
    Registry(#[from] rootlight_adapter_treesitter::RegistryError),
    #[error(transparent)]
    Limits(#[from] rootlight_adapter_sdk::LimitError),
    #[error(transparent)]
    Benchmark(#[from] rootlight_bench::ParserRunError),
    #[error(transparent)]
    Bundle(#[from] rootlight_bench::BundleError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_command_are_deterministic_and_source_free() {
        let first_manifest = embedded_manifest().expect("embedded manifest is valid");
        let second_manifest = embedded_manifest().expect("embedded manifest is valid");
        assert_eq!(first_manifest, second_manifest);
        assert_eq!(first_manifest.entries.len(), 4);
        assert!(
            first_manifest
                .entries
                .windows(2)
                .all(|entries| entries[0].id < entries[1].id)
        );
        assert!(first_manifest.entries.iter().all(|entry| {
            entry.source_sha256.len() == 64
                && entry
                    .source_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                && entry.physical_lines > 0
                && !entry.generated
        }));

        let first_command = benchmark_command();
        let second_command = benchmark_command();
        assert_eq!(first_command, second_command);
        assert_eq!(first_command.warmup_rounds, 1);
        assert_eq!(first_command.trial_rounds, 10);
        assert!(first_command.arguments.iter().all(|argument| {
            !argument.contains('\\')
                && !argument.contains('/')
                && !FIXTURES
                    .iter()
                    .any(|fixture| argument.as_bytes() == fixture.source)
        }));
    }

    #[test]
    fn arguments_reject_missing_extra_and_noncanonical_values() {
        let valid = [
            "m05-parser-evidence",
            "--output",
            "result",
            "--source-revision",
            "0123456789abcdef0123456789abcdef01234567",
        ];
        Arguments::parse(valid.into_iter().map(OsString::from))
            .expect("complete source-free arguments parse");

        let extra = [
            "m05-parser-evidence",
            "--output",
            "result",
            "--source-revision",
            "0123456789abcdef0123456789abcdef01234567",
            "--extra",
            "value",
        ];
        assert!(matches!(
            Arguments::parse(extra.into_iter().map(OsString::from)),
            Err(EvidenceError::Usage)
        ));

        let uppercase = [
            "m05-parser-evidence",
            "--output",
            "result",
            "--source-revision",
            "0123456789ABCDEF0123456789ABCDEF01234567",
        ];
        assert!(matches!(
            Arguments::parse(uppercase.into_iter().map(OsString::from)),
            Err(EvidenceError::Usage)
        ));

        let oversized = vec![
            OsString::from("m05-parser-evidence"),
            OsString::from("--output"),
            OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1)),
            OsString::from("--source-revision"),
            OsString::from("0123456789abcdef0123456789abcdef01234567"),
        ];
        assert!(matches!(
            Arguments::parse(oversized),
            Err(EvidenceError::Usage)
        ));
    }

    #[test]
    fn relative_destination_uses_current_directory() {
        let expected = env::current_dir().expect("current directory is available");
        let observed =
            destination_parent(Path::new("relative-result")).expect("relative parent resolves");
        assert_eq!(observed, expected);
    }

    #[test]
    fn compile_and_parser_metadata_are_exactly_populated() {
        let environment = environment(&"00".repeat(32)).expect("audited registry initializes");
        assert_eq!(
            environment.operating_system,
            EvidenceValue::observed(env::consts::OS.to_owned())
        );
        assert_eq!(
            environment.compiler,
            EvidenceValue::observed(compiler_identity().to_owned())
        );
        assert!(matches!(
            &environment.adapter_versions,
            EvidenceValue::Observed { .. }
        ));
        if let EvidenceValue::Observed {
            value: adapter_versions,
        } = &environment.adapter_versions
        {
            assert_eq!(
                adapter_versions.get("rootlight-adapter-treesitter"),
                Some(&ADAPTER_VERSION.to_owned())
            );
            assert_eq!(
                adapter_versions.get("tree-sitter-runtime"),
                Some(&TREE_SITTER_RUNTIME_VERSION.to_owned())
            );
        }
        assert!(matches!(
            &environment.grammar_versions,
            EvidenceValue::Observed { .. }
        ));
        if let EvidenceValue::Observed {
            value: grammar_versions,
        } = &environment.grammar_versions
        {
            assert_eq!(grammar_versions.len(), 4);
        }
        assert!(matches!(
            &environment.grammar_source_package_checksums,
            EvidenceValue::Observed { .. }
        ));
        if let EvidenceValue::Observed {
            value: source_package_checksums,
        } = &environment.grammar_source_package_checksums
        {
            assert_eq!(source_package_checksums.len(), 4);
        }
        assert!(matches!(
            &environment.grammar_hashes,
            EvidenceValue::Observed { .. }
        ));
        if let EvidenceValue::Observed {
            value: grammar_hashes,
        } = &environment.grammar_hashes
        {
            assert_eq!(grammar_hashes.len(), 7);
            assert!(grammar_hashes.contains_key("java.parser"));
            assert!(!grammar_hashes.contains_key("java.scanner"));
            assert!(grammar_hashes.values().all(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            }));
        }

        let provenance =
            build_provenance("0123456789abcdef0123456789abcdef01234567", &"00".repeat(32));
        assert_eq!(provenance.target, target_triple());
        assert_eq!(provenance.build_profile, build_profile());
    }

    #[test]
    fn fixture_directory_cleanup_is_verified() {
        let parent = tempfile::tempdir().expect("test parent is available");
        let destination = parent.path().join("result");
        let fixtures =
            create_fixture_directory(&destination).expect("secure fixture directory is created");
        let fixture_path = fixtures.path().to_owned();
        materialize_fixtures(&fixture_path).expect("embedded fixtures materialize");
        cleanup_fixture_directory(fixtures).expect("fixture cleanup succeeds");
        assert!(!fixture_path.exists());
    }
}
