//! Exact release-update metadata creation and optional offline signing.
//!
//! The normal release workflow emits canonical unsigned metadata for a
//! protected signer that never executes repository-built code. An offline
//! operator may additionally provide a private seed and matching public key;
//! the command reads that seed through a bounded no-follow handle, verifies its
//! identity, and persists only detached metadata and artifact signatures.

#![forbid(unsafe_code)]

use std::{
    fs::{self, File},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use ed25519_compact::{KeyPair, Seed};
use rootlight_client::{
    ArtifactMetadata, DetachedUpdateSignature, ReproducibilityLevel,
    UPDATE_METADATA_SCHEMA_VERSION, UpdateCompatibility, UpdateContext, UpdateMetadata,
    UpdatePublicKey, canonical_artifact_signature_message, canonical_update_metadata_bytes,
    verify_update,
};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

const MAX_PRIVATE_SEED_FILE_BYTES: u64 = 65;
const MAX_SUPPORTING_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RELEASE_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SHA256_HEX_BYTES: usize = 64;

#[derive(Debug)]
pub(crate) struct Options {
    archive: PathBuf,
    sbom: PathBuf,
    provenance: PathBuf,
    license_bundle: PathBuf,
    target: String,
    version: String,
    key_id: String,
    private_seed: Option<PathBuf>,
    public_key_hex: Option<String>,
    valid_from: u64,
    expires: u64,
    rollout_percentage: u8,
    catalog_schema: u32,
    protocol_major: u32,
    protocol_minor: u32,
    output_dir: PathBuf,
}

impl Options {
    pub(crate) fn parse(
        args: &mut impl Iterator<Item = String>,
    ) -> Result<Self, ReleaseUpdateError> {
        let mut archive = None;
        let mut sbom = None;
        let mut provenance = None;
        let mut license_bundle = None;
        let mut target = None;
        let mut version = None;
        let mut key_id = None;
        let mut private_seed = None;
        let mut public_key_hex = None;
        let mut valid_from = None;
        let mut expires = None;
        let mut rollout_percentage = None;
        let mut catalog_schema = None;
        let mut protocol_major = None;
        let mut protocol_minor = None;
        let mut output_dir = None;

        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| ReleaseUpdateError::MissingFlagValue(flag.clone()))?;
            match flag.as_str() {
                "--archive" => assign_once(&mut archive, PathBuf::from(value), "--archive")?,
                "--sbom" => assign_once(&mut sbom, PathBuf::from(value), "--sbom")?,
                "--provenance" => {
                    assign_once(&mut provenance, PathBuf::from(value), "--provenance")?;
                }
                "--license-bundle" => {
                    assign_once(
                        &mut license_bundle,
                        PathBuf::from(value),
                        "--license-bundle",
                    )?;
                }
                "--target" => assign_once(&mut target, value, "--target")?,
                "--version" => assign_once(&mut version, value, "--version")?,
                "--key-id" => assign_once(&mut key_id, value, "--key-id")?,
                "--private-seed" => {
                    assign_once(&mut private_seed, PathBuf::from(value), "--private-seed")?;
                }
                "--public-key-hex" => {
                    assign_once(&mut public_key_hex, value, "--public-key-hex")?;
                }
                "--valid-from" => {
                    assign_once(
                        &mut valid_from,
                        parse_number(&value, "--valid-from")?,
                        "--valid-from",
                    )?;
                }
                "--expires" => {
                    assign_once(
                        &mut expires,
                        parse_number(&value, "--expires")?,
                        "--expires",
                    )?;
                }
                "--rollout-percentage" => {
                    assign_once(
                        &mut rollout_percentage,
                        parse_number(&value, "--rollout-percentage")?,
                        "--rollout-percentage",
                    )?;
                }
                "--catalog-schema" => {
                    assign_once(
                        &mut catalog_schema,
                        parse_number(&value, "--catalog-schema")?,
                        "--catalog-schema",
                    )?;
                }
                "--protocol-major" => {
                    assign_once(
                        &mut protocol_major,
                        parse_number(&value, "--protocol-major")?,
                        "--protocol-major",
                    )?;
                }
                "--protocol-minor" => {
                    assign_once(
                        &mut protocol_minor,
                        parse_number(&value, "--protocol-minor")?,
                        "--protocol-minor",
                    )?;
                }
                "--output-dir" => {
                    assign_once(&mut output_dir, PathBuf::from(value), "--output-dir")?;
                }
                _ => return Err(ReleaseUpdateError::UnexpectedArgument(flag)),
            }
        }
        if private_seed.is_some() != public_key_hex.is_some() {
            return Err(ReleaseUpdateError::IncompleteSigningKey);
        }

        Ok(Self {
            archive: required(archive, "--archive")?,
            sbom: required(sbom, "--sbom")?,
            provenance: required(provenance, "--provenance")?,
            license_bundle: required(license_bundle, "--license-bundle")?,
            target: required(target, "--target")?,
            version: required(version, "--version")?,
            key_id: required(key_id, "--key-id")?,
            private_seed,
            public_key_hex,
            valid_from: required(valid_from, "--valid-from")?,
            expires: required(expires, "--expires")?,
            rollout_percentage: required(rollout_percentage, "--rollout-percentage")?,
            catalog_schema: required(catalog_schema, "--catalog-schema")?,
            protocol_major: required(protocol_major, "--protocol-major")?,
            protocol_minor: required(protocol_minor, "--protocol-minor")?,
            output_dir: required(output_dir, "--output-dir")?,
        })
    }
}

pub(crate) fn build(options: &Options) -> Result<(), ReleaseUpdateError> {
    if options.private_seed.is_some() != options.public_key_hex.is_some() {
        return Err(ReleaseUpdateError::IncompleteSigningKey);
    }
    let archive_name = regular_file_name(&options.archive)?;
    let (platform, architecture) = target_identity(&options.target)?;
    let archive = hash_regular_file(&options.archive, MAX_RELEASE_ARTIFACT_BYTES)?;
    let sbom = hash_regular_file(&options.sbom, MAX_SUPPORTING_ARTIFACT_BYTES)?;
    let provenance = hash_regular_file(&options.provenance, MAX_SUPPORTING_ARTIFACT_BYTES)?;
    let license_bundle = hash_regular_file(&options.license_bundle, MAX_SUPPORTING_ARTIFACT_BYTES)?;
    let metadata = UpdateMetadata {
        schema_version: UPDATE_METADATA_SCHEMA_VERSION.to_owned(),
        key_id: options.key_id.clone(),
        version: options.version.clone(),
        channel: "stable".to_owned(),
        platform: platform.to_owned(),
        architecture: architecture.to_owned(),
        valid_from_unix_seconds: options.valid_from,
        expires_unix_seconds: options.expires,
        rollout_percentage: options.rollout_percentage,
        artifact: ArtifactMetadata {
            file_name: archive_name.clone(),
            sha256: archive.sha256.clone(),
            size_bytes: archive.bytes,
            sbom_sha256: sbom.sha256,
            provenance_sha256: provenance.sha256,
            license_bundle_sha256: license_bundle.sha256,
            reproducibility: ReproducibilityLevel::BitForBit,
        },
        compatibility: UpdateCompatibility {
            minimum_catalog_schema: options.catalog_schema,
            maximum_catalog_schema: options.catalog_schema,
            protocol_major: options.protocol_major,
            minimum_protocol_minor: options.protocol_minor,
            maximum_protocol_minor: options.protocol_minor,
            migration_required_bytes: 0,
            rollback_supported: true,
            health_timeout_seconds: 30,
        },
    };
    let metadata_bytes =
        canonical_update_metadata_bytes(&metadata).map_err(ReleaseUpdateError::Metadata)?;

    prepare_output_directory(&options.output_dir)?;
    let metadata_path = options
        .output_dir
        .join(format!("{archive_name}.update.json"));
    persist_new_file(&metadata_path, &metadata_bytes)?;

    match (&options.private_seed, &options.public_key_hex) {
        (Some(private_seed), Some(public_key_hex)) => {
            let public_key = UpdatePublicKey::from_hex(public_key_hex)
                .map_err(|_| ReleaseUpdateError::InvalidPublicKey)?;
            let seed = read_private_seed(private_seed)?;
            let key_pair = KeyPair::from_seed(Seed::new(seed));
            if key_pair.pk.as_ref() != public_key.as_bytes() {
                return Err(ReleaseUpdateError::KeyMismatch);
            }
            let signature = key_pair.sk.sign(&metadata_bytes, None);
            let signature_hex = data_encoding::HEXLOWER.encode(signature.as_ref());
            let artifact_message = canonical_artifact_signature_message(&metadata)
                .map_err(ReleaseUpdateError::Metadata)?;
            let artifact_signature = key_pair.sk.sign(&artifact_message, None);
            key_pair
                .pk
                .verify(&artifact_message, &artifact_signature)
                .map_err(|_| ReleaseUpdateError::InvalidSignature)?;
            let artifact_signature_hex =
                data_encoding::HEXLOWER.encode(artifact_signature.as_ref());

            // A successful self-verification prevents publishing a signature
            // or compatibility envelope that the production verifier cannot
            // consume.
            let mut archive_file = open_regular(&options.archive, MAX_RELEASE_ARTIFACT_BYTES)?;
            let signature = DetachedUpdateSignature::from_hex(&signature_hex)
                .map_err(|_| ReleaseUpdateError::InvalidSignature)?;
            let context = UpdateContext {
                updates_enabled: true,
                current_version: "0.0.0".to_owned(),
                last_good_version: "0.0.0".to_owned(),
                channel: "stable".to_owned(),
                platform: platform.to_owned(),
                architecture: architecture.to_owned(),
                now_unix_seconds: options.valid_from,
                catalog_schema: options.catalog_schema,
                protocol_major: options.protocol_major,
                protocol_minor: options.protocol_minor,
                available_disk_bytes: u64::MAX,
                rollout_bucket: 0,
            };
            verify_update(
                &metadata_bytes,
                signature,
                public_key,
                &mut archive_file,
                &context,
            )
            .map_err(ReleaseUpdateError::SelfVerification)?;
            let signature_path = options
                .output_dir
                .join(format!("{archive_name}.update.sig"));
            persist_new_file(&signature_path, format!("{signature_hex}\n").as_bytes())?;
            let artifact_signature_path = options
                .output_dir
                .join(format!("{archive_name}.artifact.sig"));
            persist_new_file(
                &artifact_signature_path,
                format!("{artifact_signature_hex}\n").as_bytes(),
            )?;
            println!(
                "built signed update metadata for {} (sha256 {})",
                archive_name, archive.sha256
            );
        }
        (None, None) => {
            println!(
                "built unsigned update metadata for protected signing: {} (sha256 {})",
                archive_name, archive.sha256
            );
        }
        _ => return Err(ReleaseUpdateError::IncompleteSigningKey),
    }
    Ok(())
}

fn target_identity(target: &str) -> Result<(&'static str, &'static str), ReleaseUpdateError> {
    match target {
        "aarch64-apple-darwin" => Ok(("macos", "aarch64")),
        "aarch64-unknown-linux-gnu" => Ok(("linux", "aarch64")),
        "x86_64-apple-darwin" => Ok(("macos", "x86_64")),
        "x86_64-pc-windows-msvc" => Ok(("windows", "x86_64")),
        "x86_64-unknown-linux-gnu" => Ok(("linux", "x86_64")),
        _ => Err(ReleaseUpdateError::UnsupportedTarget),
    }
}

fn read_private_seed(path: &Path) -> Result<[u8; 32], ReleaseUpdateError> {
    let file = open_regular(path, MAX_PRIVATE_SEED_FILE_BYTES)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = file.metadata().map_err(|_| ReleaseUpdateError::ReadInput)?;
        if metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(ReleaseUpdateError::InsecurePrivateSeed);
        }
    }
    let bytes = read_open_file_bounded(file, MAX_PRIVATE_SEED_FILE_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ReleaseUpdateError::InvalidPrivateSeed)?;
    let value = text.strip_suffix('\n').unwrap_or(text);
    if value.len() != SHA256_HEX_BYTES
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(ReleaseUpdateError::InvalidPrivateSeed);
    }
    let mut seed = [0_u8; 32];
    data_encoding::HEXLOWER_PERMISSIVE
        .decode_mut(value.as_bytes(), &mut seed)
        .map_err(|_| ReleaseUpdateError::InvalidPrivateSeed)?;
    Ok(seed)
}

fn hash_regular_file(path: &Path, maximum: u64) -> Result<FileDigest, ReleaseUpdateError> {
    let mut file = open_regular(path, maximum)?;
    let bytes = file
        .metadata()
        .map_err(|_| ReleaseUpdateError::ReadInput)?
        .len();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ReleaseUpdateError::ReadInput)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(FileDigest {
        bytes,
        sha256: data_encoding::HEXLOWER.encode(&digest.finalize()),
    })
}

fn regular_file_name(path: &Path) -> Result<String, ReleaseUpdateError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 255)
        .map(str::to_owned)
        .ok_or(ReleaseUpdateError::InvalidInput)
}

fn open_regular(path: &Path, maximum: u64) -> Result<File, ReleaseUpdateError> {
    let file = open_regular_no_follow(path)?;
    let metadata = file.metadata().map_err(|_| ReleaseUpdateError::ReadInput)?;
    if !metadata.is_file() || metadata.len() > maximum || is_reparse_point(&metadata) {
        return Err(ReleaseUpdateError::InvalidInput);
    }
    Ok(file)
}

fn read_open_file_bounded(file: File, maximum: u64) -> Result<Vec<u8>, ReleaseUpdateError> {
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReleaseUpdateError::ReadInput)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(ReleaseUpdateError::InvalidInput);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> Result<File, ReleaseUpdateError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| ReleaseUpdateError::ReadInput)?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn open_regular_no_follow(path: &Path) -> Result<File, ReleaseUpdateError> {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt as _};

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| ReleaseUpdateError::ReadInput)
}

#[cfg(not(any(unix, windows)))]
fn open_regular_no_follow(_path: &Path) -> Result<File, ReleaseUpdateError> {
    Err(ReleaseUpdateError::UnsupportedPlatform)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn prepare_output_directory(path: &Path) -> Result<(), ReleaseUpdateError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|_| ReleaseUpdateError::WriteOutput)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ReleaseUpdateError::InvalidOutput);
        }
    } else {
        fs::create_dir_all(path).map_err(|_| ReleaseUpdateError::WriteOutput)?;
    }
    Ok(())
}

fn persist_new_file(path: &Path, bytes: &[u8]) -> Result<(), ReleaseUpdateError> {
    if path.exists() {
        return Err(ReleaseUpdateError::OutputExists);
    }
    let parent = path.parent().ok_or(ReleaseUpdateError::InvalidOutput)?;
    let mut temporary =
        NamedTempFile::new_in(parent).map_err(|_| ReleaseUpdateError::WriteOutput)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|_| ReleaseUpdateError::WriteOutput)?;
    temporary
        .persist_noclobber(path)
        .map_err(|_| ReleaseUpdateError::WriteOutput)?;
    Ok(())
}

fn parse_number<T>(value: &str, flag: &'static str) -> Result<T, ReleaseUpdateError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| ReleaseUpdateError::InvalidFlagValue(flag))
}

fn assign_once<T>(
    slot: &mut Option<T>,
    value: T,
    flag: &'static str,
) -> Result<(), ReleaseUpdateError> {
    if slot.replace(value).is_some() {
        return Err(ReleaseUpdateError::DuplicateFlag(flag));
    }
    Ok(())
}

fn required<T>(value: Option<T>, flag: &'static str) -> Result<T, ReleaseUpdateError> {
    value.ok_or(ReleaseUpdateError::MissingRequiredFlag(flag))
}

#[derive(Debug)]
struct FileDigest {
    bytes: u64,
    sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReleaseUpdateError {
    #[error("required argument {0} is missing")]
    MissingRequiredFlag(&'static str),
    #[error("argument {0} requires a value")]
    MissingFlagValue(String),
    #[error("argument {0} was provided more than once")]
    DuplicateFlag(&'static str),
    #[error("argument {0} has an invalid value")]
    InvalidFlagValue(&'static str),
    #[error("unexpected argument: {0}")]
    UnexpectedArgument(String),
    #[error("release update input could not be read")]
    ReadInput,
    #[error("release update input is invalid")]
    InvalidInput,
    #[error("release update target is unsupported")]
    UnsupportedTarget,
    #[cfg(not(any(unix, windows)))]
    #[error("release update tooling is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("release update private seed is invalid")]
    InvalidPrivateSeed,
    #[cfg(unix)]
    #[error("release update private seed permissions are unsafe")]
    InsecurePrivateSeed,
    #[error("release update public key is invalid")]
    InvalidPublicKey,
    #[error("release update signing requires both a private seed and public key")]
    IncompleteSigningKey,
    #[error("release update private and public keys do not match")]
    KeyMismatch,
    #[error("release update signature is invalid")]
    InvalidSignature,
    #[error("release update metadata is invalid")]
    Metadata(#[source] rootlight_client::UpdateError),
    #[error("release update self-verification failed")]
    SelfVerification(#[source] rootlight_client::UpdateError),
    #[error("release update output directory is invalid")]
    InvalidOutput,
    #[error("release update output could not be written")]
    WriteOutput,
    #[error("release update output already exists")]
    OutputExists,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private_seed(path: &Path, value: &str) {
        fs::write(path, value).expect("seed fixture writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("seed fixture becomes private");
        }
    }

    #[test]
    fn supported_targets_map_to_signed_metadata_identity() {
        assert_eq!(
            target_identity("aarch64-apple-darwin").expect("target is supported"),
            ("macos", "aarch64")
        );
        assert_eq!(
            target_identity("aarch64-unknown-linux-gnu").expect("target is supported"),
            ("linux", "aarch64")
        );
        assert_eq!(
            target_identity("x86_64-apple-darwin").expect("target is supported"),
            ("macos", "x86_64")
        );
        assert_eq!(
            target_identity("x86_64-pc-windows-msvc").expect("target is supported"),
            ("windows", "x86_64")
        );
        assert_eq!(
            target_identity("x86_64-unknown-linux-gnu").expect("target is supported"),
            ("linux", "x86_64")
        );
        assert!(target_identity("powerpc-apple-darwin").is_err());
    }

    #[test]
    fn private_seed_parser_requires_exact_lowercase_hex() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary.path().join("seed");
        write_private_seed(&path, &format!("{}\n", "ab".repeat(32)));
        assert_eq!(read_private_seed(&path).expect("seed parses"), [0xab; 32]);

        write_private_seed(&path, &"AB".repeat(32));
        assert!(read_private_seed(&path).is_err());
    }

    #[test]
    fn release_assets_self_verify_before_publication() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let archive = temporary
            .path()
            .join("rootlight-0.1.0-x86_64-pc-windows-msvc.zip");
        let sbom = temporary.path().join("distribution.cdx.json");
        let provenance = temporary.path().join("provenance.json");
        let licenses = temporary.path().join("licenses.zip");
        let private_seed = temporary.path().join("seed");
        let output = temporary.path().join("output");
        fs::write(&archive, b"exact candidate bytes").expect("archive fixture writes");
        fs::write(&sbom, b"sbom").expect("SBOM fixture writes");
        fs::write(&provenance, b"provenance").expect("provenance fixture writes");
        fs::write(&licenses, b"licenses").expect("license fixture writes");
        let seed = [0x11_u8; 32];
        write_private_seed(
            &private_seed,
            &format!("{}\n", data_encoding::HEXLOWER.encode(&seed)),
        );
        let key_pair = KeyPair::from_seed(Seed::new(seed));
        let public_key_hex = data_encoding::HEXLOWER.encode(key_pair.pk.as_ref());
        let options = Options {
            archive: archive.clone(),
            sbom,
            provenance,
            license_bundle: licenses,
            target: "x86_64-pc-windows-msvc".to_owned(),
            version: "0.1.0".to_owned(),
            key_id: "rootlight-release-test".to_owned(),
            private_seed: Some(private_seed),
            public_key_hex: Some(public_key_hex),
            valid_from: 1_000,
            expires: 2_000,
            rollout_percentage: 100,
            catalog_schema: 3,
            protocol_major: 1,
            protocol_minor: 7,
            output_dir: output.clone(),
        };

        build(&options).expect("release update assets build");

        let archive_name = archive
            .file_name()
            .expect("archive has a name")
            .to_string_lossy();
        let metadata = output.join(format!("{archive_name}.update.json"));
        let signature = output.join(format!("{archive_name}.update.sig"));
        let artifact_signature = output.join(format!("{archive_name}.artifact.sig"));
        assert!(metadata.is_file());
        for path in [signature, artifact_signature] {
            assert_eq!(
                fs::read_to_string(path)
                    .expect("signature reads")
                    .trim()
                    .len(),
                128
            );
        }
    }

    #[test]
    fn unsigned_metadata_is_canonical_and_has_no_local_signature() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let archive = temporary
            .path()
            .join("rootlight-0.1.0-x86_64-unknown-linux-gnu.zip");
        let sbom = temporary.path().join("distribution.cdx.json");
        let provenance = temporary.path().join("provenance.json");
        let licenses = temporary.path().join("licenses.zip");
        let output = temporary.path().join("output");
        fs::write(&archive, b"exact candidate bytes").expect("archive fixture writes");
        fs::write(&sbom, b"sbom").expect("SBOM fixture writes");
        fs::write(&provenance, b"provenance").expect("provenance fixture writes");
        fs::write(&licenses, b"licenses").expect("license fixture writes");
        let options = Options {
            archive: archive.clone(),
            sbom,
            provenance,
            license_bundle: licenses,
            target: "x86_64-unknown-linux-gnu".to_owned(),
            version: "0.1.0".to_owned(),
            key_id: "rootlight-release-test".to_owned(),
            private_seed: None,
            public_key_hex: None,
            valid_from: 1_000,
            expires: 2_000,
            rollout_percentage: 100,
            catalog_schema: 3,
            protocol_major: 1,
            protocol_minor: 7,
            output_dir: output.clone(),
        };

        build(&options).expect("unsigned release metadata builds");

        let archive_name = archive
            .file_name()
            .expect("archive has a name")
            .to_string_lossy();
        let metadata_path = output.join(format!("{archive_name}.update.json"));
        let metadata_bytes = fs::read(&metadata_path).expect("metadata reads");
        let parsed: UpdateMetadata =
            serde_json::from_slice(&metadata_bytes).expect("metadata parses");
        assert_eq!(
            metadata_bytes,
            canonical_update_metadata_bytes(&parsed).expect("metadata canonicalizes")
        );
        assert!(!output.join(format!("{archive_name}.update.sig")).exists());
        assert!(!output.join(format!("{archive_name}.artifact.sig")).exists());
    }
}
