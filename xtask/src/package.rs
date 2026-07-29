//! Deterministic platform-package construction and ownership verification.
//!
//! Package state is relative, allow-listed, and removable without touching
//! user data or unowned platform resources.

#![forbid(unsafe_code)]

mod installed;

use std::collections::BTreeSet;
use std::{
    fs::{self, File},
    io::{Cursor, Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ed25519_compact::{KeyPair, Seed};
use rootlight_client::{
    ArtifactMetadata, CandidateHealthCheck, CandidateHealthError, PackageUninstallOutcome,
    ProcessCandidateHealthCheck, ReproducibilityLevel, TrustedUpdatePolicy,
    UPDATE_METADATA_SCHEMA_VERSION, UpdateCompatibility, UpdateInputPaths, UpdateMetadata,
    UpdatePublicKey, UpdateRuntimeStatus, apply_update_package_with_policy,
    canonical_artifact_signature_message, canonical_update_metadata_bytes,
    install_package_with_policy, recover_update, uninstall_package, update_runtime_status,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

const SPEC_PATH: &str = "packaging/package.toml";
const SPEC_SCHEMA: &str = "rootlight.package-spec/1";
const ARCHIVE_SCHEMA: &str = "rootlight.package-manifest/1";
const LIFECYCLE_SCHEMA: &str = "rootlight.package-lifecycle/2";
const MAX_SPEC_BYTES: u64 = 256 * 1024;
const MAX_TEMPLATE_BYTES: u64 = 1024 * 1024;
const MAX_LICENSE_BYTES: u64 = 1024 * 1024;
const MAX_NOTICE_BYTES: u64 = 1024 * 1024;
const MAX_THIRD_PARTY_LICENSE_BYTES: u64 = 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 512;
const MAX_ARCHIVE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const EXPECTED_LAUNCHER: &str = "rootlight-launcher";
const EXPECTED_BINARIES: [&str; 5] = [
    "rootlight",
    "rootlight-adapter-host",
    "rootlight-daemon",
    "rootlight-mcp",
    "rootlight-semantic-host",
];
const EXPECTED_TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
];
const DISTRIBUTION_LICENSES: [&str; 4] = [
    "tree-sitter-cpp-0.23.4-LICENSE",
    "tree-sitter-java-0.23.5-LICENSE",
    "tree-sitter-kotlin-ng-1.1.0-LICENSE",
    "tree-sitter-typescript-0.23.2-LICENSE",
];

#[derive(Debug)]
pub(crate) struct BuildOptions {
    target: String,
    version: String,
    source_revision: String,
    bin_dir: PathBuf,
    output_dir: PathBuf,
}

impl BuildOptions {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, PackageError> {
        let mut target = None;
        let mut version = None;
        let mut source_revision = None;
        let mut bin_dir = None;
        let mut output_dir = None;

        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| PackageError::MissingFlagValue(flag.clone()))?;
            match flag.as_str() {
                "--target" => assign_once(&mut target, value, "--target")?,
                "--version" => assign_once(&mut version, value, "--version")?,
                "--source-revision" => {
                    assign_once(&mut source_revision, value, "--source-revision")?;
                }
                "--bin-dir" => assign_once(&mut bin_dir, PathBuf::from(value), "--bin-dir")?,
                "--output-dir" => {
                    assign_once(&mut output_dir, PathBuf::from(value), "--output-dir")?;
                }
                _ => return Err(PackageError::UnexpectedArgument(flag)),
            }
        }

        Ok(Self {
            target: target.ok_or(PackageError::MissingRequiredFlag("--target"))?,
            version: version.ok_or(PackageError::MissingRequiredFlag("--version"))?,
            source_revision: source_revision
                .ok_or(PackageError::MissingRequiredFlag("--source-revision"))?,
            bin_dir: bin_dir.ok_or(PackageError::MissingRequiredFlag("--bin-dir"))?,
            output_dir: output_dir.ok_or(PackageError::MissingRequiredFlag("--output-dir"))?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct SmokeOptions {
    baseline_archive: PathBuf,
    archive: PathBuf,
    source_revision: String,
    output: PathBuf,
}

#[derive(Debug)]
pub(crate) struct VerifyOptions {
    archive: PathBuf,
}

impl VerifyOptions {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, PackageError> {
        match (args.next(), args.next(), args.next()) {
            (Some(flag), Some(archive), None) if flag == "--archive" => Ok(Self {
                archive: PathBuf::from(archive),
            }),
            (Some(flag), None, None) if flag == "--archive" => {
                Err(PackageError::MissingFlagValue(flag))
            }
            (Some(argument), _, _) => Err(PackageError::UnexpectedArgument(argument)),
            (None, _, _) => Err(PackageError::MissingRequiredFlag("--archive")),
        }
    }
}

impl SmokeOptions {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, PackageError> {
        let mut baseline_archive = None;
        let mut archive = None;
        let mut source_revision = None;
        let mut output = None;
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| PackageError::MissingFlagValue(flag.clone()))?;
            match flag.as_str() {
                "--baseline-archive" => {
                    assign_once(
                        &mut baseline_archive,
                        PathBuf::from(value),
                        "--baseline-archive",
                    )?;
                }
                "--archive" => assign_once(&mut archive, PathBuf::from(value), "--archive")?,
                "--source-revision" => {
                    assign_once(&mut source_revision, value, "--source-revision")?;
                }
                "--output" => assign_once(&mut output, PathBuf::from(value), "--output")?,
                _ => return Err(PackageError::UnexpectedArgument(flag)),
            }
        }
        Ok(Self {
            baseline_archive: baseline_archive
                .ok_or(PackageError::MissingRequiredFlag("--baseline-archive"))?,
            archive: archive.ok_or(PackageError::MissingRequiredFlag("--archive"))?,
            source_revision: source_revision
                .ok_or(PackageError::MissingRequiredFlag("--source-revision"))?,
            output: output.ok_or(PackageError::MissingRequiredFlag("--output"))?,
        })
    }
}

pub(crate) fn check() -> Result<(), PackageError> {
    let workspace = workspace_root()?;
    let spec = load_spec(&workspace)?;
    validate_spec(&workspace, &spec)?;
    println!(
        "package contract passed for {} binaries and {} platform targets",
        spec.binaries.len(),
        spec.platforms.len()
    );
    Ok(())
}

pub(crate) fn build(options: &BuildOptions) -> Result<(), PackageError> {
    let workspace = workspace_root()?;
    let spec = load_spec(&workspace)?;
    validate_spec(&workspace, &spec)?;
    let version = parse_version(&options.version)?;
    validate_source_revision(&options.source_revision)?;
    let platform = platform_for(&spec, &options.target)?;
    let outcome = build_archive(
        &workspace,
        &spec,
        platform,
        &version,
        &options.source_revision,
        &options.bin_dir,
        &options.output_dir,
    )?;
    println!(
        "built {} ({} bytes, sha256 {}, blake3 {})",
        outcome.archive.display(),
        outcome.bytes,
        outcome.sha256,
        outcome.blake3
    );
    Ok(())
}

pub(crate) fn smoke(options: &SmokeOptions) -> Result<(), PackageError> {
    let workspace = workspace_root()?;
    let spec = load_spec(&workspace)?;
    validate_spec(&workspace, &spec)?;
    validate_source_revision(&options.source_revision)?;
    let report = exercise_install_lifecycle(options)?;
    let mut bytes = serde_json::to_vec_pretty(&report).map_err(PackageError::SerializeManifest)?;
    bytes.push(b'\n');
    let parent = options
        .output
        .parent()
        .ok_or_else(|| PackageError::InvalidInput {
            path: options.output.clone(),
            detail: "lifecycle output has no parent directory".to_owned(),
        })?;
    prepare_output_dir(parent)?;
    persist_new_file(&options.output, &bytes)?;
    println!(
        "exact package lifecycle passed for {}",
        report.candidate_target
    );
    Ok(())
}

pub(crate) fn verify(options: &VerifyOptions) -> Result<(), PackageError> {
    let archive_name = options
        .archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PackageError::InvalidInput {
            path: options.archive.clone(),
            detail: "archive must have a UTF-8 filename".to_owned(),
        })?;
    let (sha256, blake3) = archive_digests(&options.archive)?;
    verify_checksum_sidecar(&options.archive, archive_name, "sha256", &sha256)?;
    verify_checksum_sidecar(&options.archive, archive_name, "blake3", &blake3)?;
    println!(
        "verified {} (sha256 {}, blake3 {})",
        options.archive.display(),
        sha256,
        blake3
    );
    Ok(())
}

fn archive_digests(path: &Path) -> Result<(String, String), PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| PackageError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PackageError::InvalidInput {
            path: path.to_path_buf(),
            detail: "archive must be a regular non-symlink file".to_owned(),
        });
    }
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(PackageError::InputTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_ARCHIVE_BYTES,
        });
    }
    let file = File::open(path).map_err(|error| PackageError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    let mut reader = file.take(MAX_ARCHIVE_BYTES.saturating_add(1));
    let mut sha256 = Sha256::new();
    let mut blake3 = blake3::Hasher::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| PackageError::InputIo {
                path: path.to_path_buf(),
                error,
            })?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| PackageError::InputTooLarge {
                path: path.to_path_buf(),
                maximum: MAX_ARCHIVE_BYTES,
            })?;
        if total > MAX_ARCHIVE_BYTES {
            return Err(PackageError::InputTooLarge {
                path: path.to_path_buf(),
                maximum: MAX_ARCHIVE_BYTES,
            });
        }
        sha256.update(&buffer[..read]);
        blake3.update(&buffer[..read]);
    }
    Ok((
        data_encoding::HEXLOWER.encode(&sha256.finalize()),
        blake3.finalize().to_hex().to_string(),
    ))
}

fn assign_once<T>(slot: &mut Option<T>, value: T, flag: &'static str) -> Result<(), PackageError> {
    if slot.replace(value).is_some() {
        return Err(PackageError::DuplicateFlag(flag));
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, PackageError> {
    let mut candidate = std::env::current_dir().map_err(PackageError::WorkingDir)?;
    for _ in 0..8 {
        if candidate.join("Cargo.toml").is_file() && candidate.join(SPEC_PATH).is_file() {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    Err(PackageError::InvalidSpec(
        "run package tooling from within the workspace".to_owned(),
    ))
}

fn load_spec(workspace: &Path) -> Result<PackageSpec, PackageError> {
    let path = workspace.join(SPEC_PATH);
    let bytes = read_regular_bounded(&path, MAX_SPEC_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| PackageError::InvalidUtf8 {
        path: path.clone(),
        error,
    })?;
    toml::from_str(text).map_err(PackageError::ParseSpec)
}

fn validate_spec(workspace: &Path, spec: &PackageSpec) -> Result<(), PackageError> {
    if spec.schema != SPEC_SCHEMA {
        return invalid_spec(format!("schema must be {SPEC_SCHEMA}"));
    }
    if spec.autostart_default != "disabled" {
        return invalid_spec("daemon autostart must be disabled by default");
    }
    if spec.user_data_policy != "preserve" {
        return invalid_spec("package uninstall must preserve user data");
    }
    validate_relative_path(&spec.ownership_manifest)?;
    validate_relative_path(&spec.active_version_file)?;
    validate_resource_id(&spec.launcher_binary)?;
    validate_relative_path(&spec.versions_directory)?;
    validate_relative_path(&spec.launcher_directory)?;
    validate_relative_path(&spec.update_lock_file)?;
    validate_relative_path(&spec.update_transaction_file)?;
    if spec.launcher_binary != EXPECTED_LAUNCHER {
        return invalid_spec("launcher binary must match the workspace launcher");
    }
    let state_paths = [
        spec.ownership_manifest.as_str(),
        spec.active_version_file.as_str(),
        spec.update_lock_file.as_str(),
        spec.update_transaction_file.as_str(),
    ];
    if state_paths
        .iter()
        .enumerate()
        .any(|(index, path)| state_paths[index + 1..].contains(path))
    {
        return invalid_spec("package state paths must be distinct");
    }
    if spec.versions_directory == spec.launcher_directory
        || spec
            .versions_directory
            .starts_with(&format!("{}/", spec.launcher_directory))
        || spec
            .launcher_directory
            .starts_with(&format!("{}/", spec.versions_directory))
    {
        return invalid_spec("version payloads and stable launchers must use disjoint directories");
    }
    if !(2..=8).contains(&spec.retained_versions) {
        return invalid_spec("retained versions must preserve active and last-good payloads");
    }
    if !(1024..=u64::from(u32::MAX)).contains(&spec.maximum_binary_bytes) {
        return invalid_spec("maximum binary size is outside the supported range");
    }

    let binary_names = spec
        .binaries
        .iter()
        .map(|binary| binary.name.as_str())
        .collect::<Vec<_>>();
    if binary_names != EXPECTED_BINARIES {
        return invalid_spec("binary inventory must be sorted and complete");
    }
    for binary in &spec.binaries {
        validate_resource_id(&binary.name)?;
        if binary.unix_mode != 0o755 {
            return invalid_spec(format!("{} must use mode 0755", binary.name));
        }
    }

    let targets = spec
        .platforms
        .iter()
        .map(|platform| platform.target.as_str())
        .collect::<Vec<_>>();
    if targets != EXPECTED_TARGETS {
        return invalid_spec("platform inventory must be sorted and complete");
    }
    for platform in &spec.platforms {
        let expected_suffix = if platform.target.contains("windows") {
            ".exe"
        } else {
            ""
        };
        if platform.executable_suffix != expected_suffix {
            return invalid_spec(format!(
                "{} has an invalid executable suffix",
                platform.target
            ));
        }
        validate_resource_id(&platform.autostart_resource)?;
        if !matches!(
            platform.autostart_kind.as_str(),
            "launchd_user_agent" | "systemd_user_unit" | "windows_scheduled_task"
        ) {
            return invalid_spec(format!(
                "{} has an unsupported autostart kind",
                platform.target
            ));
        }
        validate_relative_path(&platform.autostart_source)?;
        if !platform
            .autostart_source
            .starts_with("packaging/autostart/")
        {
            return invalid_spec(format!(
                "{} autostart template must remain below packaging/autostart",
                platform.target
            ));
        }
        read_regular_bounded(
            &workspace.join(&platform.autostart_source),
            MAX_TEMPLATE_BYTES,
        )?;
    }
    read_regular_bounded(&workspace.join("LICENSE"), MAX_LICENSE_BYTES)?;
    Ok(())
}

fn platform_for<'a>(spec: &'a PackageSpec, target: &str) -> Result<&'a PlatformSpec, PackageError> {
    spec.platforms
        .iter()
        .find(|platform| platform.target == target)
        .ok_or_else(|| PackageError::UnsupportedTarget(target.to_owned()))
}

fn parse_version(value: &str) -> Result<Version, PackageError> {
    let version =
        Version::parse(value).map_err(|error| PackageError::InvalidVersion(error.to_string()))?;
    if version.to_string() != value {
        return Err(PackageError::InvalidVersion(
            "version must use canonical SemVer spelling".to_owned(),
        ));
    }
    Ok(version)
}

fn build_archive(
    workspace: &Path,
    spec: &PackageSpec,
    platform: &PlatformSpec,
    version: &Version,
    source_revision: &str,
    bin_dir: &Path,
    output_dir: &Path,
) -> Result<BuildOutcome, PackageError> {
    prepare_output_dir(output_dir)?;
    let mut entries = Vec::with_capacity(spec.binaries.len() + DISTRIBUTION_LICENSES.len() + 5);
    for binary in &spec.binaries {
        let filename = format!("{}{}", binary.name, platform.executable_suffix);
        let source = bin_dir.join(&filename);
        let bytes = read_regular_bounded(&source, spec.maximum_binary_bytes)?;
        entries.push(ArchiveEntry::new(
            format!("bin/{filename}"),
            "binary",
            binary.unix_mode,
            bytes,
        )?);
    }
    let launcher_filename = format!("{}{}", spec.launcher_binary, platform.executable_suffix);
    let launcher_path = format!("launcher/{launcher_filename}");
    entries.push(ArchiveEntry::new(
        launcher_path.clone(),
        "launcher",
        0o755,
        read_regular_bounded(&bin_dir.join(&launcher_filename), spec.maximum_binary_bytes)?,
    )?);
    let template_name = Path::new(&platform.autostart_source)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            PackageError::InvalidSpec("autostart template has no UTF-8 basename".to_owned())
        })?;
    entries.push(ArchiveEntry::new(
        format!("autostart/{template_name}"),
        "autostart_template",
        0o644,
        read_regular_bounded(
            &workspace.join(&platform.autostart_source),
            MAX_TEMPLATE_BYTES,
        )?,
    )?);
    entries.push(ArchiveEntry::new(
        "LICENSE".to_owned(),
        "license",
        0o644,
        read_regular_bounded(&workspace.join("LICENSE"), MAX_LICENSE_BYTES)?,
    )?);
    entries.push(ArchiveEntry::new(
        "NOTICE".to_owned(),
        "notice",
        0o644,
        read_regular_bounded(&workspace.join("NOTICE"), MAX_NOTICE_BYTES)?,
    )?);
    for filename in DISTRIBUTION_LICENSES {
        entries.push(ArchiveEntry::new(
            format!("licenses/{filename}"),
            "third_party_license",
            0o644,
            read_regular_bounded(
                &workspace.join("adapters").join("licenses").join(filename),
                MAX_THIRD_PARTY_LICENSE_BYTES,
            )?,
        )?);
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));

    let manifest = ArchiveManifest {
        schema: ARCHIVE_SCHEMA,
        target: &platform.target,
        version: &version.to_string(),
        source_revision,
        autostart_default: &spec.autostart_default,
        autostart_kind: &platform.autostart_kind,
        autostart_resource: &platform.autostart_resource,
        user_data_policy: &spec.user_data_policy,
        ownership_manifest: &spec.ownership_manifest,
        active_version_file: &spec.active_version_file,
        launcher_binary: &launcher_path,
        versions_directory: &spec.versions_directory,
        launcher_directory: &spec.launcher_directory,
        update_lock_file: &spec.update_lock_file,
        update_transaction_file: &spec.update_transaction_file,
        retained_versions: spec.retained_versions,
        entries: entries.iter().map(ArchiveEntry::manifest).collect(),
    };
    let mut manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(PackageError::SerializeManifest)?;
    manifest_bytes.push(b'\n');
    if u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(PackageError::ManifestTooLarge);
    }
    entries.push(ArchiveEntry::new(
        "package-manifest.json".to_owned(),
        "manifest",
        0o644,
        manifest_bytes,
    )?);
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let archive_bytes = encode_zip(&entries)?;
    let sha256 = sha256(&archive_bytes);
    let blake3 = blake3_hex(&archive_bytes);
    let archive_name = format!("rootlight-{version}-{}.zip", platform.target);
    let archive_path = output_dir.join(&archive_name);
    persist_new_file(&archive_path, &archive_bytes)?;
    persist_checksum_sidecar(output_dir, &archive_name, "sha256", &sha256)?;
    persist_checksum_sidecar(output_dir, &archive_name, "blake3", &blake3)?;

    Ok(BuildOutcome {
        archive: archive_path,
        bytes: archive_bytes.len(),
        sha256,
        blake3,
    })
}

fn persist_checksum_sidecar(
    output_dir: &Path,
    archive_name: &str,
    algorithm: &str,
    digest: &str,
) -> Result<(), PackageError> {
    let path = output_dir.join(format!("{archive_name}.{algorithm}"));
    let line = format!("{digest}  {archive_name}\n");
    persist_new_file(&path, line.as_bytes())
}

fn verify_checksum_sidecar(
    archive: &Path,
    archive_name: &str,
    algorithm: &'static str,
    expected_digest: &str,
) -> Result<(), PackageError> {
    let sidecar = archive.with_file_name(format!("{archive_name}.{algorithm}"));
    let bytes = read_regular_bounded(&sidecar, MAX_CHECKSUM_BYTES)?;
    let expected = format!("{expected_digest}  {archive_name}\n");
    if bytes == expected.as_bytes() {
        Ok(())
    } else {
        Err(PackageError::ChecksumMismatch {
            algorithm,
            path: sidecar,
        })
    }
}

fn encode_zip(entries: &[ArchiveEntry]) -> Result<Vec<u8>, PackageError> {
    let output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(output);
    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(entry.mode);
        writer
            .start_file(&entry.path, options)
            .map_err(PackageError::Zip)?;
        writer
            .write_all(&entry.bytes)
            .map_err(PackageError::WriteArchive)?;
    }
    let output = writer.finish().map_err(PackageError::Zip)?;
    Ok(output.into_inner())
}

fn prepare_output_dir(path: &Path) -> Result<(), PackageError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|error| PackageError::InputIo {
            path: path.to_path_buf(),
            error,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PackageError::InvalidInput {
                path: path.to_path_buf(),
                detail: "output path must be a non-symlink directory".to_owned(),
            });
        }
    } else {
        fs::create_dir_all(path).map_err(|error| PackageError::InputIo {
            path: path.to_path_buf(),
            error,
        })?;
    }
    Ok(())
}

fn persist_new_file(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    if path.exists() {
        return Err(PackageError::OutputExists(path.to_path_buf()));
    }
    let parent = path.parent().ok_or_else(|| PackageError::InvalidInput {
        path: path.to_path_buf(),
        detail: "output has no parent directory".to_owned(),
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| PackageError::InputIo {
        path: parent.to_path_buf(),
        error,
    })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| PackageError::InputIo {
            path: path.to_path_buf(),
            error,
        })?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| PackageError::InputIo {
            path: path.to_path_buf(),
            error: error.error,
        })?;
    Ok(())
}

fn exercise_install_lifecycle(
    options: &SmokeOptions,
) -> Result<PackageLifecycleReport, PackageError> {
    let baseline_name = regular_utf8_file_name(&options.baseline_archive)?;
    let candidate_name = regular_utf8_file_name(&options.archive)?;
    let (baseline_sha256, baseline_blake3) = archive_digests(&options.baseline_archive)?;
    verify_checksum_sidecar(
        &options.baseline_archive,
        &baseline_name,
        "sha256",
        &baseline_sha256,
    )?;
    verify_checksum_sidecar(
        &options.baseline_archive,
        &baseline_name,
        "blake3",
        &baseline_blake3,
    )?;
    let (candidate_sha256, candidate_blake3) = archive_digests(&options.archive)?;
    verify_checksum_sidecar(
        &options.archive,
        &candidate_name,
        "sha256",
        &candidate_sha256,
    )?;
    verify_checksum_sidecar(
        &options.archive,
        &candidate_name,
        "blake3",
        &candidate_blake3,
    )?;
    let baseline_manifest = read_lifecycle_manifest(&options.baseline_archive)?;
    let candidate_manifest = read_lifecycle_manifest(&options.archive)?;
    if baseline_manifest.target != candidate_manifest.target
        || baseline_manifest.source_revision != options.source_revision
        || candidate_manifest.source_revision != options.source_revision
        || parse_version(&baseline_manifest.version)? >= parse_version(&candidate_manifest.version)?
    {
        return invalid_install("package lifecycle archives have incompatible identities");
    }
    let (inputs_directory, inputs, policy) =
        signed_lifecycle_inputs(&options.archive, &candidate_manifest, &candidate_sha256)?;

    let sandbox = tempfile::tempdir().map_err(PackageError::WorkingDir)?;
    let install_root = sandbox.path().join("install");
    fs::create_dir_all(install_root.join("user")).map_err(|error| PackageError::InputIo {
        path: install_root.join("user"),
        error,
    })?;
    let user_sentinel = install_root.join("user/data.bin");
    let unowned_sentinel = sandbox.path().join("unowned-resource");
    fs::write(&user_sentinel, b"user-data").map_err(|error| PackageError::InputIo {
        path: user_sentinel.clone(),
        error,
    })?;
    fs::write(&unowned_sentinel, b"unowned").map_err(|error| PackageError::InputIo {
        path: unowned_sentinel.clone(),
        error,
    })?;
    let installed = install_package_with_policy(
        &install_root,
        &options.baseline_archive,
        &baseline_sha256,
        &policy,
    )?;
    if installed.version != baseline_manifest.version
        || installed.artifact_sha256 != baseline_sha256
    {
        return invalid_install("bootstrap outcome does not match the baseline archive");
    }
    let mut health = ProcessCandidateHealthCheck;
    let catalog_state = sandbox.path().join("catalog-state");
    fs::create_dir(&catalog_state).map_err(|error| PackageError::InputIo {
        path: catalog_state.clone(),
        error,
    })?;
    let updated = apply_update_package_with_policy(
        &install_root,
        &catalog_state,
        &inputs,
        &policy,
        &mut health,
    )?;
    if updated.version != candidate_manifest.version
        || updated.previous_version != baseline_manifest.version
        || updated.artifact_sha256 != candidate_sha256
    {
        return invalid_install("update outcome does not match the candidate archive");
    }
    let active_status = update_runtime_status(&install_root)?;
    if active_status.active_version != candidate_manifest.version
        || active_status.last_good_version != baseline_manifest.version
        || active_status.recovery_required
        || active_status.transaction_phase.is_some()
    {
        return invalid_install("committed update state is inconsistent");
    }
    probe_launcher(&install_root, Duration::from_secs(30))?;
    let installed_release = installed::exercise(&install_root, &candidate_manifest.version)?;
    let recovered_status = recover_update(&install_root)?;
    if recovered_status != active_status {
        return invalid_install("clean recovery changed committed update state");
    }
    let uninstalled = uninstall_through_installed_launcher(
        &install_root,
        versions_in_manifest(&active_status, &baseline_manifest, &candidate_manifest),
        Duration::from_secs(60),
    )?;
    if !uninstalled.user_data_preserved {
        return invalid_install("uninstall did not preserve user data");
    }
    if !user_sentinel.is_file()
        || fs::read(&user_sentinel).map_err(|error| PackageError::InputIo {
            path: user_sentinel.clone(),
            error,
        })? != b"user-data"
    {
        return invalid_install("uninstall changed user data");
    }
    if !unowned_sentinel.is_file()
        || fs::read(&unowned_sentinel).map_err(|error| PackageError::InputIo {
            path: unowned_sentinel.clone(),
            error,
        })? != b"unowned"
    {
        return invalid_install("uninstall changed an unowned filesystem resource");
    }
    for owned in ["state", "versions", "current"] {
        if install_root.join(owned).exists() {
            return invalid_install("uninstall retained an owned installation tree");
        }
    }

    let rollback_sandbox = tempfile::tempdir().map_err(PackageError::WorkingDir)?;
    let rollback_root = rollback_sandbox.path().join("install");
    install_package_with_policy(
        &rollback_root,
        &options.baseline_archive,
        &baseline_sha256,
        &policy,
    )?;
    let mut failed_health = RejectCandidateHealth;
    if apply_update_package_with_policy(
        &rollback_root,
        &catalog_state,
        &inputs,
        &policy,
        &mut failed_health,
    )
    .is_ok()
    {
        return invalid_install("a rejected candidate health check committed the update");
    }
    let rollback_status = update_runtime_status(&rollback_root)?;
    if rollback_status.active_version != baseline_manifest.version
        || rollback_status.last_good_version != baseline_manifest.version
        || rollback_status.recovery_required
        || rollback_status.transaction_phase.is_some()
        || rollback_root
            .join("versions")
            .join(&candidate_manifest.version)
            .exists()
    {
        return invalid_install("failed candidate health did not fully roll back");
    }
    let rollback_recovery = recover_update(&rollback_root)?;
    if rollback_recovery != rollback_status {
        return invalid_install("post-rollback recovery changed installation state");
    }
    uninstall_package(&rollback_root)?;
    drop(inputs_directory);

    Ok(PackageLifecycleReport {
        schema: LIFECYCLE_SCHEMA,
        source_revision: options.source_revision.clone(),
        candidate_target: candidate_manifest.target,
        candidate_version: candidate_manifest.version,
        baseline_version: baseline_manifest.version,
        candidate_archive: candidate_name,
        candidate_sha256,
        baseline_archive: baseline_name,
        baseline_sha256,
        bootstrap_owned_files: installed.owned_file_count,
        committed_active_version: active_status.active_version,
        committed_last_good_version: active_status.last_good_version,
        rollback_active_version: rollback_status.active_version,
        uninstall_removed_versions: uninstalled.removed_versions,
        installed_release,
        installed_command_uninstall_observed: true,
        launcher_probe_observed: true,
        candidate_health_observed: true,
        failed_health_rollback_observed: true,
        clean_recovery_observed: true,
        user_data_preserved: true,
        unowned_data_preserved: true,
    })
}

fn read_lifecycle_manifest(path: &Path) -> Result<LifecyclePackageManifest, PackageError> {
    let file = File::open(path).map_err(|error| PackageError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    let mut archive = ZipArchive::new(file).map_err(PackageError::Zip)?;
    let entry = archive
        .by_name("package-manifest.json")
        .map_err(PackageError::Zip)?;
    if !entry.is_file() || entry.size() > MAX_MANIFEST_BYTES {
        return Err(PackageError::ManifestTooLarge);
    }
    let capacity = usize::try_from(entry.size()).map_err(|_| PackageError::ManifestTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .take(MAX_MANIFEST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(PackageError::WriteArchive)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_MANIFEST_BYTES) {
        return Err(PackageError::ManifestTooLarge);
    }
    let manifest: LifecyclePackageManifest =
        serde_json::from_slice(&bytes).map_err(PackageError::ParseArchiveManifest)?;
    if manifest.schema != ARCHIVE_SCHEMA {
        return invalid_install("package archive manifest schema is unsupported");
    }
    validate_source_revision(&manifest.source_revision)?;
    parse_version(&manifest.version)?;
    Ok(manifest)
}

fn signed_lifecycle_inputs(
    archive: &Path,
    manifest: &LifecyclePackageManifest,
    artifact_sha256: &str,
) -> Result<(tempfile::TempDir, UpdateInputPaths, TrustedUpdatePolicy), PackageError> {
    let (platform, architecture) = target_identity(&manifest.target)?;
    let archive_name = regular_utf8_file_name(archive)?;
    let archive_bytes = fs::metadata(archive)
        .map_err(|error| PackageError::InputIo {
            path: archive.to_path_buf(),
            error,
        })?
        .len();
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|_| PackageError::RandomUnavailable)?;
    let key_pair = KeyPair::from_seed(Seed::new(seed));
    let public_key_hex = data_encoding::HEXLOWER.encode(key_pair.pk.as_ref());
    let public_key =
        UpdatePublicKey::from_hex(&public_key_hex).map_err(PackageError::UpdateMetadata)?;
    let policy = TrustedUpdatePolicy::new(
        true,
        "rootlight-lifecycle-test".to_owned(),
        public_key,
        "stable".to_owned(),
        3,
        1,
        7,
        0,
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PackageError::Clock)?
        .as_secs();
    let sbom = lifecycle_sbom(manifest);
    let provenance =
        lifecycle_provenance(&archive_name, artifact_sha256, &manifest.source_revision);
    let license_bundle = lifecycle_license_bundle(archive)?;
    let metadata = UpdateMetadata {
        schema_version: UPDATE_METADATA_SCHEMA_VERSION.to_owned(),
        key_id: "rootlight-lifecycle-test".to_owned(),
        version: manifest.version.clone(),
        channel: "stable".to_owned(),
        platform: platform.to_owned(),
        architecture: architecture.to_owned(),
        valid_from_unix_seconds: now.saturating_sub(60),
        expires_unix_seconds: now.checked_add(3_600).ok_or(PackageError::Clock)?,
        rollout_percentage: 100,
        artifact: ArtifactMetadata {
            file_name: archive_name,
            sha256: artifact_sha256.to_owned(),
            size_bytes: archive_bytes,
            sbom_sha256: sha256(&sbom),
            provenance_sha256: sha256(&provenance),
            license_bundle_sha256: sha256(&license_bundle),
            reproducibility: ReproducibilityLevel::BitForBit,
        },
        compatibility: UpdateCompatibility {
            minimum_catalog_schema: 3,
            maximum_catalog_schema: 3,
            protocol_major: 1,
            minimum_protocol_minor: 7,
            maximum_protocol_minor: 7,
            migration_required_bytes: 0,
            rollback_supported: true,
            health_timeout_seconds: 30,
        },
    };
    let metadata_bytes =
        canonical_update_metadata_bytes(&metadata).map_err(PackageError::UpdateMetadata)?;
    let signature = key_pair.sk.sign(&metadata_bytes, None);
    let signature_hex = data_encoding::HEXLOWER.encode(signature.as_ref());
    let artifact_signature_message =
        canonical_artifact_signature_message(&metadata).map_err(PackageError::UpdateMetadata)?;
    let artifact_signature = key_pair.sk.sign(&artifact_signature_message, None);
    let artifact_signature_hex = data_encoding::HEXLOWER.encode(artifact_signature.as_ref());
    let directory = tempfile::tempdir().map_err(PackageError::WorkingDir)?;
    let metadata_path = directory.path().join("candidate.update.json");
    let metadata_signature_path = directory.path().join("candidate.update.sig");
    let artifact_signature_path = directory.path().join("candidate.artifact.sig");
    let sbom_path = directory.path().join("candidate.sbom.json");
    let provenance_path = directory.path().join("candidate.provenance.json");
    let license_bundle_path = directory.path().join("candidate.licenses.zip");
    fs::write(&metadata_path, metadata_bytes).map_err(|error| PackageError::InputIo {
        path: metadata_path.clone(),
        error,
    })?;
    for (path, bytes) in [
        (
            &metadata_signature_path,
            format!("{signature_hex}\n").into_bytes(),
        ),
        (
            &artifact_signature_path,
            format!("{artifact_signature_hex}\n").into_bytes(),
        ),
        (&sbom_path, sbom),
        (&provenance_path, provenance),
        (&license_bundle_path, license_bundle),
    ] {
        fs::write(path, bytes).map_err(|error| PackageError::InputIo {
            path: path.clone(),
            error,
        })?;
    }
    let inputs = UpdateInputPaths::new(
        metadata_path,
        metadata_signature_path,
        artifact_signature_path,
        archive.to_path_buf(),
        sbom_path,
        provenance_path,
        license_bundle_path,
    );
    Ok((directory, inputs, policy))
}

fn lifecycle_sbom(manifest: &LifecyclePackageManifest) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "bomFormat": "CycloneDX",
        "components": [],
        "dependencies": [],
        "metadata": {
            "component": {
                "bom-ref": format!(
                    "urn:rootlight:distribution:{}:{}",
                    manifest.version, manifest.target
                ),
                "components": [
                    {"name": "rootlight"},
                    {"name": "rootlight-adapter-host"},
                    {"name": "rootlight-daemon"},
                    {"name": "rootlight-launcher"},
                    {"name": "rootlight-mcp"},
                    {"name": "rootlight-semantic-host"},
                    {"name": "LICENSE"},
                    {"name": "NOTICE"}
                ],
                "name": "rootlight-distribution",
                "properties": [
                    {
                        "name": "rootlight:source:revision",
                        "value": manifest.source_revision
                    },
                    {"name": "rootlight:target:triple", "value": manifest.target}
                ],
                "type": "application",
                "version": manifest.version
            },
            "properties": [
                {
                    "name": "cdx:rustc:sbom:target:triple",
                    "value": manifest.target
                },
                {"name": "rootlight:build:profile", "value": "release"},
                {
                    "name": "rootlight:source:revision",
                    "value": manifest.source_revision
                }
            ]
        },
        "specVersion": "1.5",
        "version": 1
    }))
    .expect("lifecycle SBOM uses only serializable values")
}

fn lifecycle_provenance(
    archive_name: &str,
    artifact_sha256: &str,
    source_revision: &str,
) -> Vec<u8> {
    let statement = serde_json::to_vec(&serde_json::json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{
            "name": archive_name,
            "digest": {"sha256": artifact_sha256}
        }],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "resolvedDependencies": [{
                    "uri": "git+https://github.com/tomasmarekk/rootlight",
                    "digest": {"gitCommit": source_revision}
                }]
            }
        }
    }))
    .expect("lifecycle provenance uses only serializable values");
    serde_json::to_vec(&serde_json::json!({
        "dsseEnvelope": {
            "payload": data_encoding::BASE64.encode(&statement),
            "payloadType": "application/vnd.in-toto+json",
            "signatures": [{"keyid": "rootlight-lifecycle-test", "sig": "local"}]
        },
        "verificationMaterial": {
            "tlogEntries": [{"logIndex": "local-lifecycle"}]
        }
    }))
    .expect("lifecycle provenance envelope uses only serializable values")
}

fn lifecycle_license_bundle(archive_path: &Path) -> Result<Vec<u8>, PackageError> {
    let archive_file = File::open(archive_path).map_err(|error| PackageError::InputIo {
        path: archive_path.to_path_buf(),
        error,
    })?;
    let mut archive = ZipArchive::new(archive_file).map_err(PackageError::Zip)?;
    let output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(output);
    let mut names = vec![
        ("LICENSE".to_owned(), MAX_LICENSE_BYTES),
        ("NOTICE".to_owned(), MAX_NOTICE_BYTES),
    ];
    names.extend(
        DISTRIBUTION_LICENSES
            .map(|name| (format!("licenses/{name}"), MAX_THIRD_PARTY_LICENSE_BYTES)),
    );
    for (name, maximum) in names {
        let mut entry = archive.by_name(&name).map_err(PackageError::Zip)?;
        if entry.size() > maximum {
            return Err(PackageError::InputTooLarge {
                path: archive_path.to_path_buf(),
                maximum,
            });
        }
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| PackageError::InputIo {
                path: archive_path.to_path_buf(),
                error,
            })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
            return Err(PackageError::InputTooLarge {
                path: archive_path.to_path_buf(),
                maximum,
            });
        }
        writer
            .start_file(
                &name,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Stored)
                    .unix_permissions(0o644),
            )
            .map_err(PackageError::Zip)?;
        writer
            .write_all(&bytes)
            .map_err(PackageError::WriteArchive)?;
    }
    writer
        .finish()
        .map_err(PackageError::Zip)
        .map(Cursor::into_inner)
}

fn target_identity(target: &str) -> Result<(&'static str, &'static str), PackageError> {
    match target {
        "aarch64-apple-darwin" => Ok(("macos", "aarch64")),
        "aarch64-unknown-linux-gnu" => Ok(("linux", "aarch64")),
        "x86_64-apple-darwin" => Ok(("macos", "x86_64")),
        "x86_64-pc-windows-msvc" => Ok(("windows", "x86_64")),
        "x86_64-unknown-linux-gnu" => Ok(("linux", "x86_64")),
        _ => Err(PackageError::UnsupportedTarget(target.to_owned())),
    }
}

fn regular_utf8_file_name(path: &Path) -> Result<String, PackageError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 255)
        .map(str::to_owned)
        .ok_or_else(|| PackageError::InvalidInput {
            path: path.to_path_buf(),
            detail: "package archive must have a bounded UTF-8 filename".to_owned(),
        })
}

fn probe_launcher(install_root: &Path, timeout: Duration) -> Result<(), PackageError> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let launcher = install_root
        .join("current/bin")
        .join(format!("rootlight{suffix}"));
    let mut child = Command::new(launcher)
        .arg("--update-health-probe")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(PackageError::Launcher)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(PackageError::Clock)?;
    loop {
        if let Some(status) = child.try_wait().map_err(PackageError::Launcher)? {
            return if status.success() {
                Ok(())
            } else {
                invalid_install("installed stable launcher health probe failed")
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            child.wait().map_err(PackageError::Launcher)?;
            return invalid_install("installed stable launcher health probe timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn uninstall_through_installed_launcher(
    install_root: &Path,
    expected_versions: usize,
    timeout: Duration,
) -> Result<PackageUninstallOutcome, PackageError> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let launcher = install_root
        .join("current/bin")
        .join(format!("rootlight{suffix}"));
    let status = Command::new(&launcher)
        .arg("update")
        .arg("uninstall")
        .arg("--root")
        .arg(install_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(PackageError::Launcher)?;
    if !status.success() {
        return invalid_install("installed launcher uninstall command failed");
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(PackageError::Clock)?;
    while ["state", "versions", "current"]
        .iter()
        .any(|name| install_root.join(name).exists())
    {
        if Instant::now() >= deadline {
            return invalid_install("installed launcher uninstall cleanup timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(PackageUninstallOutcome {
        user_data_preserved: true,
        removed_versions: expected_versions,
        deferred_cleanup: cfg!(windows),
    })
}

fn versions_in_manifest(
    status: &UpdateRuntimeStatus,
    baseline: &LifecyclePackageManifest,
    candidate: &LifecyclePackageManifest,
) -> usize {
    BTreeSet::from([
        status.active_version.as_str(),
        status.last_good_version.as_str(),
        baseline.version.as_str(),
        candidate.version.as_str(),
    ])
    .len()
}

struct RejectCandidateHealth;

impl CandidateHealthCheck for RejectCandidateHealth {
    fn check(
        &mut self,
        _candidate_version_root: &Path,
        _catalog_state_root: &Path,
        _timeout: Duration,
    ) -> Result<(), CandidateHealthError> {
        Err(CandidateHealthError)
    }
}

fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| PackageError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PackageError::InvalidInput {
            path: path.to_path_buf(),
            detail: "input must be a regular non-symlink file".to_owned(),
        });
    }
    if metadata.len() > maximum {
        return Err(PackageError::InputTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    let file = File::open(path).map_err(|error| PackageError::InputIo {
        path: path.to_path_buf(),
        error,
    })?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| PackageError::InputIo {
            path: path.to_path_buf(),
            error,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(PackageError::InputTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    Ok(bytes)
}

fn validate_relative_path(value: &str) -> Result<(), PackageError> {
    if value.is_empty()
        || value.len() > 240
        || value.contains('\\')
        || value.contains(':')
        || value.contains("//")
    {
        return invalid_spec(format!("invalid relative path {value:?}"));
    }
    let path = Path::new(value);
    for component in path.components() {
        let Component::Normal(component) = component else {
            return invalid_spec(format!("invalid relative path {value:?}"));
        };
        let component = component.to_str().ok_or_else(|| {
            PackageError::InvalidSpec(format!("relative path is not UTF-8: {value:?}"))
        })?;
        let normalized = component.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            ".aws"
                | ".azure"
                | ".git"
                | ".gnupg"
                | ".kube"
                | ".ssh"
                | "credentials"
                | "projects"
                | "repositories"
                | "secrets"
                | "workspace"
        ) {
            return invalid_spec(format!("relative path enters a protected area: {value:?}"));
        }
    }
    Ok(())
}

fn validate_resource_id(value: &str) -> Result<(), PackageError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid_spec(format!("invalid resource identifier {value:?}"));
    }
    Ok(())
}

fn validate_source_revision(value: &str) -> Result<(), PackageError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(PackageError::InvalidSourceRevision);
    }
    Ok(())
}

fn invalid_spec<T>(detail: impl Into<String>) -> Result<T, PackageError> {
    Err(PackageError::InvalidSpec(detail.into()))
}

fn invalid_install<T>(detail: impl Into<String>) -> Result<T, PackageError> {
    Err(PackageError::InvalidInstall(detail.into()))
}

fn sha256(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageSpec {
    schema: String,
    autostart_default: String,
    user_data_policy: String,
    ownership_manifest: String,
    active_version_file: String,
    launcher_binary: String,
    versions_directory: String,
    launcher_directory: String,
    update_lock_file: String,
    update_transaction_file: String,
    retained_versions: u8,
    maximum_binary_bytes: u64,
    binaries: Vec<BinarySpec>,
    platforms: Vec<PlatformSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinarySpec {
    name: String,
    unix_mode: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformSpec {
    target: String,
    executable_suffix: String,
    autostart_kind: String,
    autostart_resource: String,
    autostart_source: String,
}

#[derive(Debug)]
struct ArchiveEntry {
    path: String,
    kind: &'static str,
    mode: u32,
    bytes: Vec<u8>,
    sha256: String,
}

impl ArchiveEntry {
    fn new(
        path: String,
        kind: &'static str,
        mode: u32,
        bytes: Vec<u8>,
    ) -> Result<Self, PackageError> {
        validate_relative_path(&path)?;
        let sha256 = sha256(&bytes);
        Ok(Self {
            path,
            kind,
            mode,
            bytes,
            sha256,
        })
    }

    fn manifest(&self) -> ArchiveManifestEntry<'_> {
        ArchiveManifestEntry {
            path: &self.path,
            kind: self.kind,
            bytes: self.bytes.len(),
            sha256: &self.sha256,
            unix_mode: self.mode,
        }
    }
}

#[derive(Debug, Serialize)]
struct ArchiveManifest<'a> {
    schema: &'static str,
    target: &'a str,
    version: &'a str,
    source_revision: &'a str,
    autostart_default: &'a str,
    autostart_kind: &'a str,
    autostart_resource: &'a str,
    user_data_policy: &'a str,
    ownership_manifest: &'a str,
    active_version_file: &'a str,
    launcher_binary: &'a str,
    versions_directory: &'a str,
    launcher_directory: &'a str,
    update_lock_file: &'a str,
    update_transaction_file: &'a str,
    retained_versions: u8,
    entries: Vec<ArchiveManifestEntry<'a>>,
}

#[derive(Debug, Serialize)]
struct ArchiveManifestEntry<'a> {
    path: &'a str,
    kind: &'static str,
    bytes: usize,
    sha256: &'a str,
    unix_mode: u32,
}

#[derive(Debug)]
struct BuildOutcome {
    archive: PathBuf,
    bytes: usize,
    sha256: String,
    blake3: String,
}

#[derive(Debug, Deserialize)]
struct LifecyclePackageManifest {
    schema: String,
    target: String,
    version: String,
    source_revision: String,
}

#[derive(Debug, Serialize)]
struct PackageLifecycleReport {
    schema: &'static str,
    source_revision: String,
    candidate_target: String,
    candidate_version: String,
    baseline_version: String,
    candidate_archive: String,
    candidate_sha256: String,
    baseline_archive: String,
    baseline_sha256: String,
    bootstrap_owned_files: usize,
    committed_active_version: String,
    committed_last_good_version: String,
    rollback_active_version: String,
    uninstall_removed_versions: usize,
    installed_release: installed::InstalledReleaseEvidence,
    installed_command_uninstall_observed: bool,
    launcher_probe_observed: bool,
    candidate_health_observed: bool,
    failed_health_rollback_observed: bool,
    clean_recovery_observed: bool,
    user_data_preserved: bool,
    unowned_data_preserved: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PackageError {
    #[error("failed to determine or create a working directory")]
    WorkingDir(#[source] std::io::Error),
    #[error("package input {path} could not be read")]
    InputIo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("package input {path} is invalid: {detail}")]
    InvalidInput { path: PathBuf, detail: String },
    #[error("package input {path} exceeds {maximum} bytes")]
    InputTooLarge { path: PathBuf, maximum: u64 },
    #[error("package input {path} is not UTF-8")]
    InvalidUtf8 {
        path: PathBuf,
        #[source]
        error: std::str::Utf8Error,
    },
    #[error("package specification is not valid TOML")]
    ParseSpec(#[source] toml::de::Error),
    #[error("package specification is invalid: {0}")]
    InvalidSpec(String),
    #[error("package archive manifest is invalid JSON")]
    ParseArchiveManifest(#[source] serde_json::Error),
    #[error("package install state is invalid: {0}")]
    InvalidInstall(String),
    #[error("unsupported package target {0}")]
    UnsupportedTarget(String),
    #[error("invalid package version: {0}")]
    InvalidVersion(String),
    #[error("source revision must be a canonical 40- or 64-character lowercase hex digest")]
    InvalidSourceRevision,
    #[error("required argument {0} is missing")]
    MissingRequiredFlag(&'static str),
    #[error("argument {0} requires a value")]
    MissingFlagValue(String),
    #[error("argument {0} was provided more than once")]
    DuplicateFlag(&'static str),
    #[error("unexpected argument: {0}")]
    UnexpectedArgument(String),
    #[error("package manifest serialization failed")]
    SerializeManifest(#[source] serde_json::Error),
    #[error("package manifest exceeds its size limit")]
    ManifestTooLarge,
    #[error("package ZIP encoding failed")]
    Zip(#[source] zip::result::ZipError),
    #[error("package ZIP write failed")]
    WriteArchive(#[source] std::io::Error),
    #[error("package lifecycle update metadata is invalid")]
    UpdateMetadata(#[source] rootlight_client::UpdateError),
    #[error(transparent)]
    FilesystemUpdate(#[from] rootlight_client::FilesystemUpdateError),
    #[error("package lifecycle secure randomness is unavailable")]
    RandomUnavailable,
    #[error("package lifecycle clock is unavailable")]
    Clock,
    #[error("installed package launcher failed")]
    Launcher(#[source] std::io::Error),
    #[error("installed package process check failed during {operation}")]
    InstalledProcess {
        operation: &'static str,
        #[source]
        source: rootlight_sandbox::ProcessError,
    },
    #[error("installed package IO check failed during {operation}")]
    InstalledIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("installed package runtime check failed during {operation}")]
    InstalledRuntime {
        operation: &'static str,
        #[source]
        source: rootlight_runtime::RuntimeError,
    },
    #[error("installed package client check failed during {operation}")]
    InstalledClient {
        operation: &'static str,
        #[source]
        source: rootlight_client::ClientError,
    },
    #[error("immutable package output already exists: {0}")]
    OutputExists(PathBuf),
    #[error("{algorithm} checksum sidecar does not match package archive: {path}")]
    ChecksumMismatch {
        algorithm: &'static str,
        path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use tempfile::tempdir;
    use zip::ZipArchive;

    use super::*;

    #[test]
    fn checked_spec_covers_every_required_platform() {
        let workspace = workspace_root().expect("workspace root");
        let spec = load_spec(&workspace).expect("spec parses");

        validate_spec(&workspace, &spec).expect("spec validates");
        assert_eq!(
            spec.platforms
                .iter()
                .map(|platform| platform.target.as_str())
                .collect::<Vec<_>>(),
            EXPECTED_TARGETS
        );
    }

    #[test]
    fn relative_paths_reject_escape_and_sensitive_areas() {
        for invalid in [
            "../escape",
            "/absolute",
            "bin\\rootlight",
            "versions/.ssh/key",
            "versions/repositories/source",
            "C:/rootlight",
        ] {
            assert!(validate_relative_path(invalid).is_err(), "{invalid}");
        }
        validate_relative_path("versions/1.0.0/bin/rootlight").expect("owned path is accepted");
    }

    #[test]
    fn package_archive_is_deterministic_and_manifest_bound() {
        let workspace = workspace_root().expect("workspace root");
        let spec = load_spec(&workspace).expect("spec parses");
        let platform = platform_for(&spec, "x86_64-unknown-linux-gnu").expect("platform exists");
        let sandbox = tempdir().expect("sandbox");
        let binaries = sandbox.path().join("bin");
        let first_output = sandbox.path().join("first");
        let second_output = sandbox.path().join("second");
        fs::create_dir(&binaries).expect("binary directory");
        for binary in &spec.binaries {
            fs::write(
                binaries.join(&binary.name),
                format!("fixture:{}", binary.name),
            )
            .expect("binary fixture");
        }
        fs::write(
            binaries.join(&spec.launcher_binary),
            format!("fixture:{}", spec.launcher_binary),
        )
        .expect("launcher fixture");
        let version = Version::parse("1.2.3").expect("version");

        let first = build_archive(
            &workspace,
            &spec,
            platform,
            &version,
            "1111111111111111111111111111111111111111",
            &binaries,
            &first_output,
        )
        .expect("first package");
        let second = build_archive(
            &workspace,
            &spec,
            platform,
            &version,
            "1111111111111111111111111111111111111111",
            &binaries,
            &second_output,
        )
        .expect("second package");
        let first_bytes = fs::read(&first.archive).expect("first bytes");
        let second_bytes = fs::read(&second.archive).expect("second bytes");
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(first.sha256, sha256(&first_bytes));
        assert_eq!(first.blake3, blake3_hex(&first_bytes));
        assert_eq!(
            fs::read_to_string(first_output.join(format!(
                "{}.sha256",
                first.archive.file_name().expect("archive filename").to_string_lossy()
            )))
            .expect("sha256 sidecar"),
            format!(
                "{}  {}\n",
                first.sha256,
                first
                    .archive
                    .file_name()
                    .expect("archive filename")
                    .to_string_lossy()
            )
        );
        assert_eq!(
            fs::read_to_string(first_output.join(format!(
                "{}.blake3",
                first.archive.file_name().expect("archive filename").to_string_lossy()
            )))
            .expect("blake3 sidecar"),
            format!(
                "{}  {}\n",
                first.blake3,
                first
                    .archive
                    .file_name()
                    .expect("archive filename")
                    .to_string_lossy()
            )
        );

        let mut archive = ZipArchive::new(Cursor::new(first_bytes)).expect("ZIP parses");
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).expect("entry").name().to_owned())
            .collect::<Vec<_>>();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        let mut manifest = String::new();
        archive
            .by_name("package-manifest.json")
            .expect("manifest exists")
            .read_to_string(&mut manifest)
            .expect("manifest reads");
        assert!(manifest.contains("\"autostart_default\": \"disabled\""));
        assert!(manifest.contains("\"user_data_policy\": \"preserve\""));
        assert!(names.contains(&"NOTICE".to_owned()));
        for filename in DISTRIBUTION_LICENSES {
            assert!(names.contains(&format!("licenses/{filename}")));
        }
        verify(&VerifyOptions {
            archive: first.archive.clone(),
        })
        .expect("checksum sidecars verify");
    }

    #[test]
    fn package_verification_rejects_a_tampered_sidecar() {
        let sandbox = tempdir().expect("sandbox");
        let archive = sandbox.path().join("rootlight-1.2.3-test.zip");
        fs::write(&archive, b"archive").expect("archive fixture");
        fs::write(
            sandbox.path().join("rootlight-1.2.3-test.zip.sha256"),
            format!("{}  rootlight-1.2.3-test.zip\n", sha256(b"other")),
        )
        .expect("sha256 sidecar");
        fs::write(
            sandbox.path().join("rootlight-1.2.3-test.zip.blake3"),
            format!("{}  rootlight-1.2.3-test.zip\n", blake3_hex(b"archive")),
        )
        .expect("blake3 sidecar");

        let error = verify(&VerifyOptions { archive })
            .expect_err("tampered sha256 sidecar must be rejected");
        assert!(matches!(
            error,
            PackageError::ChecksumMismatch {
                algorithm: "sha256",
                ..
            }
        ));
    }

    #[test]
    fn options_reject_duplicates_and_missing_values() {
        let mut duplicate = [
            "--target",
            "x86_64-unknown-linux-gnu",
            "--target",
            "x86_64-unknown-linux-gnu",
        ]
        .into_iter()
        .map(str::to_owned);
        assert!(BuildOptions::parse(&mut duplicate).is_err());

        let mut missing = ["--target"].into_iter().map(str::to_owned);
        assert!(SmokeOptions::parse(&mut missing).is_err());

        let mut trailing = ["--archive", "package.zip", "unexpected"]
            .into_iter()
            .map(str::to_owned);
        assert!(VerifyOptions::parse(&mut trailing).is_err());
    }
}
