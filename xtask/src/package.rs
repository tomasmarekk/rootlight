//! Deterministic platform-package construction and ownership verification.
//!
//! Package state is relative, allow-listed, and removable without touching
//! user data or unowned platform resources.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Cursor, Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const SPEC_PATH: &str = "packaging/package.toml";
const SPEC_SCHEMA: &str = "rootlight.package-spec/1";
const ARCHIVE_SCHEMA: &str = "rootlight.package-manifest/1";
const OWNERSHIP_SCHEMA: &str = "rootlight.install-ownership/1";
const MAX_SPEC_BYTES: u64 = 256 * 1024;
const MAX_TEMPLATE_BYTES: u64 = 1024 * 1024;
const MAX_LICENSE_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const EXPECTED_BINARIES: [&str; 4] = [
    "rootlight",
    "rootlight-adapter-host",
    "rootlight-daemon",
    "rootlight-mcp",
];
const EXPECTED_TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
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
    target: String,
}

impl SmokeOptions {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, PackageError> {
        match (args.next(), args.next()) {
            (Some(flag), Some(target)) if flag == "--target" => Ok(Self { target }),
            (Some(flag), None) if flag == "--target" => Err(PackageError::MissingFlagValue(flag)),
            (Some(argument), _) => Err(PackageError::UnexpectedArgument(argument)),
            (None, _) => Err(PackageError::MissingRequiredFlag("--target")),
        }
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
        "built {} ({} bytes, sha256 {})",
        outcome.archive.display(),
        outcome.bytes,
        outcome.sha256
    );
    Ok(())
}

pub(crate) fn smoke(options: &SmokeOptions) -> Result<(), PackageError> {
    let workspace = workspace_root()?;
    let spec = load_spec(&workspace)?;
    validate_spec(&workspace, &spec)?;
    let platform = platform_for(&spec, &options.target)?;
    exercise_install_lifecycle(&spec, platform)?;
    println!("package ownership smoke passed for {}", options.target);
    Ok(())
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
    if spec.ownership_manifest == spec.active_version_file {
        return invalid_spec("ownership and active-version paths must differ");
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
    let mut entries = Vec::with_capacity(spec.binaries.len() + 2);
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
    let digest = sha256(&archive_bytes);
    let archive_name = format!("rootlight-{version}-{}.zip", platform.target);
    let archive_path = output_dir.join(&archive_name);
    persist_new_file(&archive_path, &archive_bytes)?;
    let digest_path = output_dir.join(format!("{archive_name}.sha256"));
    let digest_line = format!("{digest}  {archive_name}\n");
    persist_new_file(&digest_path, digest_line.as_bytes())?;

    Ok(BuildOutcome {
        archive: archive_path,
        bytes: archive_bytes.len(),
        sha256: digest,
    })
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
    spec: &PackageSpec,
    platform: &PlatformSpec,
) -> Result<(), PackageError> {
    let sandbox = tempfile::tempdir().map_err(PackageError::WorkingDir)?;
    let install_root = sandbox.path().join("install");
    let platform_root = sandbox.path().join("platform");
    fs::create_dir_all(install_root.join("user")).map_err(PackageError::WorkingDir)?;
    fs::create_dir_all(platform_root.join(&platform.autostart_kind).join("unowned"))
        .map_err(PackageError::WorkingDir)?;
    let user_sentinel = install_root.join("user/data.bin");
    let platform_sentinel = platform_root
        .join(&platform.autostart_kind)
        .join("unowned/resource");
    fs::write(&user_sentinel, b"user-data").map_err(PackageError::WorkingDir)?;
    fs::write(&platform_sentinel, b"unowned").map_err(PackageError::WorkingDir)?;

    let first = synthetic_image(spec, platform, "1.0.0", b"first")?;
    install_image(spec, platform, &install_root, &platform_root, &first, false)?;
    if platform_resource_path(platform, &platform_root).exists() {
        return invalid_install("default install registered autostart");
    }
    require_active_version(spec, &install_root, "1.0.0")?;

    let second = synthetic_image(spec, platform, "1.1.0", b"second")?;
    install_image(
        spec,
        platform,
        &install_root,
        &platform_root,
        &second,
        false,
    )?;
    require_active_version(spec, &install_root, "1.1.0")?;
    rollback(spec, &install_root, "1.0.0")?;
    require_active_version(spec, &install_root, "1.0.0")?;
    set_autostart(spec, platform, &install_root, &platform_root, true)?;
    if !platform_resource_path(platform, &platform_root).is_file() {
        return invalid_install("explicit autostart registration was not recorded");
    }

    uninstall(spec, &install_root, &platform_root)?;
    if !user_sentinel.is_file()
        || fs::read(&user_sentinel).map_err(PackageError::WorkingDir)? != b"user-data"
    {
        return invalid_install("uninstall changed user data");
    }
    if !platform_sentinel.is_file()
        || fs::read(&platform_sentinel).map_err(PackageError::WorkingDir)? != b"unowned"
    {
        return invalid_install("uninstall changed an unowned platform resource");
    }
    if platform_resource_path(platform, &platform_root).exists() {
        return invalid_install("uninstall retained an owned platform resource");
    }
    if install_root.join("versions").is_dir()
        && directory_contains_file(&install_root.join("versions"))?
    {
        return invalid_install("uninstall retained owned package files");
    }
    Ok(())
}

fn synthetic_image(
    spec: &PackageSpec,
    platform: &PlatformSpec,
    version: &str,
    marker: &[u8],
) -> Result<PackageImage, PackageError> {
    let version = parse_version(version)?;
    let mut entries = Vec::new();
    for binary in &spec.binaries {
        entries.push(InstallEntry {
            path: format!("bin/{}{}", binary.name, platform.executable_suffix),
            bytes: marker.to_vec(),
        });
    }
    entries.push(InstallEntry {
        path: "autostart/template".to_owned(),
        bytes: b"inert".to_vec(),
    });
    Ok(PackageImage { version, entries })
}

fn install_image(
    spec: &PackageSpec,
    platform: &PlatformSpec,
    install_root: &Path,
    platform_root: &Path,
    image: &PackageImage,
    enable_autostart: bool,
) -> Result<(), PackageError> {
    for entry in &image.entries {
        validate_relative_path(&entry.path)?;
    }
    let version_prefix = format!("versions/{}/", image.version);
    let mut ownership = read_ownership(spec, install_root)?.unwrap_or_else(|| InstallOwnership {
        schema: OWNERSHIP_SCHEMA.to_owned(),
        target: platform.target.clone(),
        active_version: image.version.to_string(),
        owned_paths: Vec::new(),
        platform_resources: Vec::new(),
    });
    if ownership.target != platform.target {
        return invalid_install("existing ownership manifest targets another platform");
    }
    let mut owned = ownership.owned_paths.into_iter().collect::<BTreeSet<_>>();
    for entry in &image.entries {
        let relative = format!("{version_prefix}{}", entry.path);
        validate_relative_path(&relative)?;
        write_owned_file(install_root, &relative, &entry.bytes)?;
        owned.insert(relative);
    }
    write_owned_file(
        install_root,
        &spec.active_version_file,
        format!("{}\n", image.version).as_bytes(),
    )?;
    owned.insert(spec.active_version_file.clone());
    owned.insert(spec.ownership_manifest.clone());
    ownership.active_version = image.version.to_string();
    ownership.owned_paths = owned.into_iter().collect();
    write_ownership(spec, install_root, &ownership)?;
    if enable_autostart {
        set_autostart(spec, platform, install_root, platform_root, true)?;
    }
    Ok(())
}

fn rollback(spec: &PackageSpec, install_root: &Path, version: &str) -> Result<(), PackageError> {
    let version = parse_version(version)?;
    let mut ownership = read_ownership(spec, install_root)?
        .ok_or_else(|| PackageError::InvalidInstall("ownership manifest is missing".to_owned()))?;
    let version_prefix = format!("versions/{version}/");
    if !ownership
        .owned_paths
        .iter()
        .any(|path| path.starts_with(&version_prefix))
    {
        return invalid_install("rollback target is not an installed version");
    }
    write_owned_file(
        install_root,
        &spec.active_version_file,
        format!("{version}\n").as_bytes(),
    )?;
    ownership.active_version = version.to_string();
    write_ownership(spec, install_root, &ownership)
}

fn set_autostart(
    spec: &PackageSpec,
    platform: &PlatformSpec,
    install_root: &Path,
    platform_root: &Path,
    enabled: bool,
) -> Result<(), PackageError> {
    let mut ownership = read_ownership(spec, install_root)?
        .ok_or_else(|| PackageError::InvalidInstall("ownership manifest is missing".to_owned()))?;
    let resource = PlatformResource {
        kind: platform.autostart_kind.clone(),
        id: platform.autostart_resource.clone(),
    };
    let path = platform_resource_path(platform, platform_root);
    if enabled {
        let parent = path.parent().ok_or_else(|| {
            PackageError::InvalidInstall("platform resource has no parent".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(PackageError::WorkingDir)?;
        fs::write(&path, b"registered-by-rootlight\n").map_err(PackageError::WorkingDir)?;
        if !ownership.platform_resources.contains(&resource) {
            ownership.platform_resources.push(resource);
            ownership.platform_resources.sort();
        }
    } else {
        if path.is_file() {
            fs::remove_file(&path).map_err(PackageError::WorkingDir)?;
        }
        ownership
            .platform_resources
            .retain(|candidate| candidate != &resource);
    }
    write_ownership(spec, install_root, &ownership)
}

fn uninstall(
    spec: &PackageSpec,
    install_root: &Path,
    platform_root: &Path,
) -> Result<(), PackageError> {
    let ownership = read_ownership(spec, install_root)?
        .ok_or_else(|| PackageError::InvalidInstall("ownership manifest is missing".to_owned()))?;
    for resource in &ownership.platform_resources {
        validate_resource_id(&resource.kind)?;
        validate_resource_id(&resource.id)?;
        let path = platform_root.join(&resource.kind).join(&resource.id);
        if path.is_file() {
            fs::remove_file(path).map_err(PackageError::WorkingDir)?;
        }
    }
    let mut paths = ownership.owned_paths;
    paths.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| right.cmp(left)));
    for relative in paths {
        validate_relative_path(&relative)?;
        let path = install_root.join(relative);
        if path.is_file() {
            fs::remove_file(path).map_err(PackageError::WorkingDir)?;
        }
    }
    remove_empty_owned_directories(install_root)?;
    Ok(())
}

fn write_ownership(
    spec: &PackageSpec,
    install_root: &Path,
    ownership: &InstallOwnership,
) -> Result<(), PackageError> {
    let mut bytes =
        serde_json::to_vec_pretty(ownership).map_err(PackageError::SerializeManifest)?;
    bytes.push(b'\n');
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(PackageError::ManifestTooLarge);
    }
    write_owned_file(install_root, &spec.ownership_manifest, &bytes)
}

fn read_ownership(
    spec: &PackageSpec,
    install_root: &Path,
) -> Result<Option<InstallOwnership>, PackageError> {
    let path = install_root.join(&spec.ownership_manifest);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = read_regular_bounded(&path, MAX_MANIFEST_BYTES)?;
    let ownership: InstallOwnership =
        serde_json::from_slice(&bytes).map_err(PackageError::ParseOwnership)?;
    if ownership.schema != OWNERSHIP_SCHEMA {
        return invalid_install("ownership manifest schema is unsupported").map(Some);
    }
    let mut prior_path = None;
    for path in &ownership.owned_paths {
        validate_relative_path(path)?;
        if prior_path.is_some_and(|prior| prior >= path) {
            return invalid_install("owned paths must be sorted and unique").map(Some);
        }
        prior_path = Some(path);
    }
    let mut prior_resource = None;
    for resource in &ownership.platform_resources {
        validate_resource_id(&resource.kind)?;
        validate_resource_id(&resource.id)?;
        if prior_resource.is_some_and(|prior| prior >= resource) {
            return invalid_install("platform resources must be sorted and unique").map(Some);
        }
        prior_resource = Some(resource);
    }
    Ok(Some(ownership))
}

fn write_owned_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), PackageError> {
    validate_relative_path(relative)?;
    let path = root.join(relative);
    let parent = path.parent().ok_or_else(|| {
        PackageError::InvalidInstall("owned path has no parent directory".to_owned())
    })?;
    fs::create_dir_all(parent).map_err(PackageError::WorkingDir)?;
    if path.exists() {
        let metadata = fs::symlink_metadata(&path).map_err(PackageError::WorkingDir)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return invalid_install("owned destination is not a regular file");
        }
    }
    fs::write(path, bytes).map_err(PackageError::WorkingDir)
}

fn require_active_version(
    spec: &PackageSpec,
    install_root: &Path,
    expected: &str,
) -> Result<(), PackageError> {
    let observed = fs::read_to_string(install_root.join(&spec.active_version_file))
        .map_err(PackageError::WorkingDir)?;
    if observed.trim_end() != expected {
        return invalid_install(format!(
            "active version is {}, expected {expected}",
            observed.trim_end()
        ));
    }
    Ok(())
}

fn platform_resource_path(platform: &PlatformSpec, platform_root: &Path) -> PathBuf {
    platform_root
        .join(&platform.autostart_kind)
        .join(&platform.autostart_resource)
}

fn remove_empty_owned_directories(root: &Path) -> Result<(), PackageError> {
    for relative in ["versions", "state"] {
        let path = root.join(relative);
        if path.is_dir() {
            remove_empty_tree(&path)?;
        }
    }
    Ok(())
}

fn remove_empty_tree(path: &Path) -> Result<bool, PackageError> {
    let mut empty = true;
    for entry in fs::read_dir(path).map_err(PackageError::WorkingDir)? {
        let entry = entry.map_err(PackageError::WorkingDir)?;
        let metadata = entry.metadata().map_err(PackageError::WorkingDir)?;
        if metadata.is_dir() {
            if !remove_empty_tree(&entry.path())? {
                empty = false;
            }
        } else {
            empty = false;
        }
    }
    if empty {
        fs::remove_dir(path).map_err(PackageError::WorkingDir)?;
    }
    Ok(empty)
}

fn directory_contains_file(path: &Path) -> Result<bool, PackageError> {
    for entry in fs::read_dir(path).map_err(PackageError::WorkingDir)? {
        let entry = entry.map_err(PackageError::WorkingDir)?;
        let metadata = entry.metadata().map_err(PackageError::WorkingDir)?;
        if metadata.is_file() || (metadata.is_dir() && directory_contains_file(&entry.path())?) {
            return Ok(true);
        }
    }
    Ok(false)
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageSpec {
    schema: String,
    autostart_default: String,
    user_data_policy: String,
    ownership_manifest: String,
    active_version_file: String,
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
}

#[derive(Debug)]
struct PackageImage {
    version: Version,
    entries: Vec<InstallEntry>,
}

#[derive(Debug)]
struct InstallEntry {
    path: String,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct PlatformResource {
    kind: String,
    id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallOwnership {
    schema: String,
    target: String,
    active_version: String,
    owned_paths: Vec<String>,
    platform_resources: Vec<PlatformResource>,
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
    #[error("package ownership state is invalid JSON")]
    ParseOwnership(#[source] serde_json::Error),
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
    #[error("immutable package output already exists: {0}")]
    OutputExists(PathBuf),
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
        for target in EXPECTED_TARGETS {
            exercise_install_lifecycle(
                &spec,
                platform_for(&spec, target).expect("platform exists"),
            )
            .expect("install lifecycle passes");
        }
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
    }
}
