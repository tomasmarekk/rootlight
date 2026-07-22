//! Produces reproducible actual-token evidence for complete MCP discovery payloads.
//!
//! Runtime safety retains provider-neutral estimates; this module measures exact
//! compact JSON bytes with one pinned, fully offline tokenizer implementation.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use rootlight_bench::ActualTokenizerIdentity;
use rootlight_mcp_contract::accounting::{estimate_tokens, tool_list_payload};
use rootlight_mcp_contract::catalog::ExposureProfile;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tiktoken_rs::CoreBPE;

const REPORT_SCHEMA: &str = "rootlight.mcp-token-accounting/2";
const REPORT_FILE: &str = "token-accounting-v2.json";
const TOKENIZER_PROVIDER: &str = "openai";
const TOKENIZER_MODEL: &str = "gpt-4o";
const TOKENIZER_NAME: &str = "o200k_base";
const TOKENIZER_IMPLEMENTATION: &str = "tiktoken-rs";
const TOKENIZER_IMPLEMENTATION_VERSION: &str = "0.12.0";
const TOKENIZER_IMPLEMENTATION_REVISION: &str = "32de8dc0526d67f2c266c4e5e7c6a8ec5a0ce3d7";
const TOKENIZER_IMPLEMENTATION_REPOSITORY: &str = "https://github.com/zurawiki/tiktoken-rs";
const TOKENIZER_IMPLEMENTATION_PACKAGE_SHA256: &str =
    "027853bbf8c7763b77c5c595f1c271c7d536ced7d6f83452911b944621e57fc2";
const TOKENIZER_IMPLEMENTATION_LICENSE_SPDX: &str = "MIT";
const TOKENIZER_IMPLEMENTATION_LICENSE_URL: &str = "https://raw.githubusercontent.com/zurawiki/tiktoken-rs/32de8dc0526d67f2c266c4e5e7c6a8ec5a0ce3d7/LICENSE";
const TOKENIZER_IMPLEMENTATION_LICENSE_SHA256: &str =
    "f7c6ddf9d84fd7b8ad5917e4074d4c05e4c1dfb752a28a0058f06bd0f5e2edcc";
const TOKENIZER_ASSET_SHA256: &str =
    "446a9538cb6c348e3516120d7c08b09f57c36495e2acfffe59a5bf8b0cfb1a2d";
const TOKENIZER_ASSET_PATH: &str = "assets/o200k_base.tiktoken";
const TOKENIZER_ASSET_FORMAT: &str = "tiktoken_bpe_ranks";
const TOKENIZER_ASSET_DISTRIBUTION: &str = "embedded_in_crates_io_package";
const TOKENIZER_ASSET_ORIGIN_URL: &str =
    "https://openaipublic.blob.core.windows.net/encodings/o200k_base.tiktoken";
const TOKENIZER_ASSET_SOURCE_REPOSITORY: &str = "https://github.com/openai/tiktoken";
const TOKENIZER_ASSET_SOURCE_REVISION: &str = "08a5f3b2c987ada4fc5aa1f16c643c203fa8acaa";
const TOKENIZER_ASSET_SOURCE_REFERENCE: &str = "https://raw.githubusercontent.com/openai/tiktoken/08a5f3b2c987ada4fc5aa1f16c643c203fa8acaa/tiktoken_ext/openai_public.py";
const TOKENIZER_ASSET_LICENSE_SPDX: &str = "MIT";
const TOKENIZER_ASSET_LICENSE_BASIS: &str =
    "upstream_repository_license_without_asset_specific_notice";
const TOKENIZER_ASSET_LICENSE_URL: &str = "https://raw.githubusercontent.com/openai/tiktoken/08a5f3b2c987ada4fc5aa1f16c643c203fa8acaa/LICENSE";
const TOKENIZER_ASSET_LICENSE_SHA256: &str =
    "418cb499b436128d653d79941333a5437b7be2ea9213dcc2f04d15d5d2c51d86";
const NORMALIZATION: &str = "none_exact_utf8";
const TOOLS_LIST_INPUT_KIND: &str = "mcp_tools_list_result";
const TOOLS_LIST_FRAMING: &str = "raw_compact_json_object";
const MAX_REPORT_BYTES: usize = 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Source-bound artifact options for actual tokenizer evidence.
pub(crate) struct Options {
    output_dir: PathBuf,
    source_revision: String,
}

impl Options {
    /// Parses the required output directory and source revision.
    pub(crate) fn parse(
        args: &mut impl Iterator<Item = String>,
    ) -> Result<Self, TokenAccountingError> {
        let mut output_dir = None;
        let mut source_revision = None;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--output-dir" if output_dir.is_none() => {
                    output_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or(TokenAccountingError::MissingArgument("--output-dir"))?,
                    ));
                }
                "--source-revision" if source_revision.is_none() => {
                    source_revision = Some(
                        args.next()
                            .ok_or(TokenAccountingError::MissingArgument("--source-revision"))?,
                    );
                }
                _ => return Err(TokenAccountingError::UnexpectedArgument(flag)),
            }
        }
        let output_dir = output_dir.ok_or(TokenAccountingError::IncompleteOptions)?;
        let source_revision = source_revision.ok_or(TokenAccountingError::IncompleteOptions)?;
        if !valid_source_revision(&source_revision) {
            return Err(TokenAccountingError::InvalidSourceRevision(source_revision));
        }
        Ok(Self {
            output_dir,
            source_revision,
        })
    }
}

/// Parses the sole report path accepted by `token-accounting-check`.
pub(crate) fn parse_report_path(
    args: &mut impl Iterator<Item = String>,
) -> Result<PathBuf, TokenAccountingError> {
    let Some(flag) = args.next() else {
        return Err(TokenAccountingError::MissingReport);
    };
    if flag != "--report" {
        return Err(TokenAccountingError::UnexpectedArgument(flag));
    }
    let report = PathBuf::from(
        args.next()
            .ok_or(TokenAccountingError::MissingArgument("--report"))?,
    );
    if let Some(argument) = args.next() {
        return Err(TokenAccountingError::UnexpectedArgument(argument));
    }
    Ok(report)
}

/// Writes complete per-profile payloads and actual-token accounting.
pub(crate) fn emit(options: &Options) -> Result<(), TokenAccountingError> {
    fs::create_dir_all(&options.output_dir).map_err(|source| TokenAccountingError::Io {
        path: options.output_dir.clone(),
        source,
    })?;
    let tokenizer = O200kTokenizer::new()?;
    let report = build_report(&options.source_revision, &tokenizer)?;
    for payload in &report.payloads {
        let profile = profile_from_name(&payload.profile)?;
        let encoded = serialize_payload(profile)?;
        let file = payload
            .measurement
            .input
            .file
            .as_deref()
            .ok_or(TokenAccountingError::MissingInputFile)?;
        write_bytes(&options.output_dir.join(file), &encoded)?;
    }
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    write_bytes(&options.output_dir.join(REPORT_FILE), &encoded)?;
    println!(
        "token accounting written for {} profiles using {} {}",
        report.payloads.len(),
        report.tokenizer.implementation,
        report.tokenizer.implementation_version
    );
    Ok(())
}

/// Verifies tokenizer identity, exact inputs, and all reported measurements.
pub(crate) fn check(report_path: &Path) -> Result<(), TokenAccountingError> {
    let report_bytes = read_bounded(report_path, MAX_REPORT_BYTES)?;
    let report: TokenAccountingReport = serde_json::from_slice(&report_bytes)?;
    validate_report_header(&report)?;
    let report_dir = report_path
        .parent()
        .ok_or_else(|| TokenAccountingError::UnsafePath(report_path.to_path_buf()))?;
    let tokenizer = O200kTokenizer::new()?;
    validate_payloads(report_dir, &report, &tokenizer)?;
    println!(
        "token accounting verified for {} profiles at {}",
        report.payloads.len(),
        report.source_revision
    );
    Ok(())
}

pub(crate) trait OfflineTokenizer {
    fn identity(&self) -> TokenizerIdentity;
    fn count(&self, input: &str) -> Result<u64, TokenAccountingError>;
}

pub(crate) struct O200kTokenizer {
    inner: CoreBPE,
}

impl O200kTokenizer {
    pub(crate) fn new() -> Result<Self, TokenAccountingError> {
        let inner =
            tiktoken_rs::o200k_base().map_err(TokenAccountingError::TokenizerInitialization)?;
        Ok(Self { inner })
    }

    /// Returns the provider-neutral identity used by shared benchmark
    /// evidence.
    pub(crate) fn benchmark_identity(&self) -> ActualTokenizerIdentity {
        ActualTokenizerIdentity {
            provider: TOKENIZER_PROVIDER.to_owned(),
            model: TOKENIZER_MODEL.to_owned(),
            tokenizer: TOKENIZER_NAME.to_owned(),
            implementation: TOKENIZER_IMPLEMENTATION.to_owned(),
            implementation_version: Some(TOKENIZER_IMPLEMENTATION_VERSION.to_owned()),
            implementation_sha256: Some(TOKENIZER_IMPLEMENTATION_PACKAGE_SHA256.to_owned()),
            asset_sha256: Some(TOKENIZER_ASSET_SHA256.to_owned()),
        }
    }
}

impl OfflineTokenizer for O200kTokenizer {
    fn identity(&self) -> TokenizerIdentity {
        TokenizerIdentity::expected()
    }

    fn count(&self, input: &str) -> Result<u64, TokenAccountingError> {
        u64::try_from(self.inner.encode_ordinary(input).len())
            .map_err(|_| TokenAccountingError::IntegerOverflow("actual token count"))
    }
}

fn build_report(
    source_revision: &str,
    tokenizer: &impl OfflineTokenizer,
) -> Result<TokenAccountingReport, TokenAccountingError> {
    let mut payloads = Vec::with_capacity(ExposureProfile::ALL.len());
    for profile in ExposureProfile::ALL {
        let encoded = serialize_payload(profile)?;
        let text = std::str::from_utf8(&encoded).map_err(|source| TokenAccountingError::Utf8 {
            path: PathBuf::from(payload_file(profile)),
            source,
        })?;
        payloads.push(ProfileAccounting {
            profile: profile.name().to_owned(),
            tool_count: u64::try_from(profile.tools().len())
                .map_err(|_| TokenAccountingError::IntegerOverflow("tool count"))?,
            measurement: measure_utf8_input(
                TOOLS_LIST_INPUT_KIND,
                Some(payload_file(profile)),
                TOOLS_LIST_FRAMING,
                text,
                tokenizer,
            )?,
        });
    }
    Ok(TokenAccountingReport {
        schema: REPORT_SCHEMA.to_owned(),
        source_revision: source_revision.to_owned(),
        tokenizer: tokenizer.identity(),
        payloads,
    })
}

fn validate_report_header(report: &TokenAccountingReport) -> Result<(), TokenAccountingError> {
    if report.schema != REPORT_SCHEMA {
        return Err(TokenAccountingError::UnsupportedSchema(
            report.schema.clone(),
        ));
    }
    if !valid_source_revision(&report.source_revision) {
        return Err(TokenAccountingError::InvalidSourceRevision(
            report.source_revision.clone(),
        ));
    }
    let expected = TokenizerIdentity::expected();
    if report.tokenizer != expected {
        return Err(TokenAccountingError::TokenizerIdentityMismatch {
            expected: serde_json::to_string(&expected)?,
            observed: serde_json::to_string(&report.tokenizer)?,
        });
    }
    if report.payloads.len() != ExposureProfile::ALL.len() {
        return Err(TokenAccountingError::ProfileCount {
            expected: ExposureProfile::ALL.len(),
            observed: report.payloads.len(),
        });
    }
    Ok(())
}

fn validate_payloads(
    report_dir: &Path,
    report: &TokenAccountingReport,
    tokenizer: &impl OfflineTokenizer,
) -> Result<(), TokenAccountingError> {
    for (profile, observed) in ExposureProfile::ALL.into_iter().zip(&report.payloads) {
        if observed.profile != profile.name() {
            return Err(TokenAccountingError::ProfileOrder {
                expected: profile.name(),
                observed: observed.profile.clone(),
            });
        }
        let expected_file = payload_file(profile);
        let Some(observed_file) = observed.measurement.input.file.as_deref() else {
            return Err(TokenAccountingError::MissingInputFile);
        };
        if observed_file != expected_file || !safe_file_name(observed_file) {
            return Err(TokenAccountingError::UnsafePath(PathBuf::from(
                observed_file,
            )));
        }
        let path = report_dir.join(observed_file);
        let encoded = read_bounded(&path, MAX_PAYLOAD_BYTES)?;
        let expected = measure_payload(profile, &path, &encoded, tokenizer)?;
        if *observed != expected {
            return Err(TokenAccountingError::MeasurementMismatch {
                profile: profile.name(),
                expected: serde_json::to_string(&expected)?,
                observed: serde_json::to_string(observed)?,
            });
        }
    }
    Ok(())
}

fn measure_payload(
    profile: ExposureProfile,
    path: &Path,
    encoded: &[u8],
    tokenizer: &impl OfflineTokenizer,
) -> Result<ProfileAccounting, TokenAccountingError> {
    let canonical = serialize_payload(profile)?;
    if encoded != canonical {
        return Err(TokenAccountingError::PayloadMismatch {
            profile: profile.name(),
            expected: sha256_hex(&canonical),
            observed: sha256_hex(encoded),
        });
    }
    let text = std::str::from_utf8(encoded).map_err(|source| TokenAccountingError::Utf8 {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(ProfileAccounting {
        profile: profile.name().to_owned(),
        tool_count: u64::try_from(profile.tools().len())
            .map_err(|_| TokenAccountingError::IntegerOverflow("tool count"))?,
        measurement: measure_utf8_input(
            TOOLS_LIST_INPUT_KIND,
            Some(payload_file(profile)),
            TOOLS_LIST_FRAMING,
            text,
            tokenizer,
        )?,
    })
}

/// Measures one exact UTF-8 input under the pinned deterministic boundaries.
pub(crate) fn measure_utf8_input(
    input_kind: &str,
    file: Option<String>,
    framing: &str,
    input: &str,
    tokenizer: &impl OfflineTokenizer,
) -> Result<TokenMeasurement, TokenAccountingError> {
    let encoded = input.as_bytes();
    Ok(TokenMeasurement {
        input: InputEvidence {
            input_kind: input_kind.to_owned(),
            file,
            sha256: sha256_hex(encoded),
            serialized_bytes: u64::try_from(encoded.len())
                .map_err(|_| TokenAccountingError::IntegerOverflow("serialized byte count"))?,
            normalization: NORMALIZATION.to_owned(),
            framing: framing.to_owned(),
        },
        deterministic_estimated_tokens: estimate_tokens(encoded.len()),
        actual_tokens: tokenizer.count(input)?,
    })
}

fn serialize_payload(profile: ExposureProfile) -> Result<Vec<u8>, TokenAccountingError> {
    serde_json::to_vec(&tool_list_payload(profile)).map_err(TokenAccountingError::Json)
}

fn payload_file(profile: ExposureProfile) -> String {
    format!("tools-list-{}.json", profile.name())
}

fn profile_from_name(name: &str) -> Result<ExposureProfile, TokenAccountingError> {
    ExposureProfile::from_name(name)
        .ok_or_else(|| TokenAccountingError::UnknownProfile(name.to_owned()))
}

fn valid_source_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_file_name(file: &str) -> bool {
    let mut components = Path::new(file).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, TokenAccountingError> {
    let file = File::open(path).map_err(|source| TokenAccountingError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let read_limit = u64::try_from(limit)
        .map_err(|_| TokenAccountingError::IntegerOverflow("read limit"))?
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| TokenAccountingError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(TokenAccountingError::FileTooLarge {
            path: path.to_path_buf(),
            limit,
        });
    }
    Ok(bytes)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), TokenAccountingError> {
    fs::write(path, bytes).map_err(|source| TokenAccountingError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenAccountingReport {
    schema: String,
    source_revision: String,
    tokenizer: TokenizerIdentity,
    payloads: Vec<ProfileAccounting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenizerIdentity {
    provider: String,
    model: String,
    name: String,
    implementation: String,
    implementation_version: String,
    implementation_revision: String,
    implementation_repository: String,
    implementation_package_sha256: String,
    implementation_license_spdx: String,
    implementation_license_url: String,
    implementation_license_sha256: String,
    asset_sha256: String,
    asset_path: String,
    asset_format: String,
    asset_distribution: String,
    asset_origin_url: String,
    asset_source_repository: String,
    asset_source_revision: String,
    asset_source_reference: String,
    asset_license_spdx: String,
    asset_license_basis: String,
    asset_license_url: String,
    asset_license_sha256: String,
    offline: bool,
}

impl TokenizerIdentity {
    pub(crate) fn expected() -> Self {
        Self {
            provider: TOKENIZER_PROVIDER.to_owned(),
            model: TOKENIZER_MODEL.to_owned(),
            name: TOKENIZER_NAME.to_owned(),
            implementation: TOKENIZER_IMPLEMENTATION.to_owned(),
            implementation_version: TOKENIZER_IMPLEMENTATION_VERSION.to_owned(),
            implementation_revision: TOKENIZER_IMPLEMENTATION_REVISION.to_owned(),
            implementation_repository: TOKENIZER_IMPLEMENTATION_REPOSITORY.to_owned(),
            implementation_package_sha256: TOKENIZER_IMPLEMENTATION_PACKAGE_SHA256.to_owned(),
            implementation_license_spdx: TOKENIZER_IMPLEMENTATION_LICENSE_SPDX.to_owned(),
            implementation_license_url: TOKENIZER_IMPLEMENTATION_LICENSE_URL.to_owned(),
            implementation_license_sha256: TOKENIZER_IMPLEMENTATION_LICENSE_SHA256.to_owned(),
            asset_sha256: TOKENIZER_ASSET_SHA256.to_owned(),
            asset_path: TOKENIZER_ASSET_PATH.to_owned(),
            asset_format: TOKENIZER_ASSET_FORMAT.to_owned(),
            asset_distribution: TOKENIZER_ASSET_DISTRIBUTION.to_owned(),
            asset_origin_url: TOKENIZER_ASSET_ORIGIN_URL.to_owned(),
            asset_source_repository: TOKENIZER_ASSET_SOURCE_REPOSITORY.to_owned(),
            asset_source_revision: TOKENIZER_ASSET_SOURCE_REVISION.to_owned(),
            asset_source_reference: TOKENIZER_ASSET_SOURCE_REFERENCE.to_owned(),
            asset_license_spdx: TOKENIZER_ASSET_LICENSE_SPDX.to_owned(),
            asset_license_basis: TOKENIZER_ASSET_LICENSE_BASIS.to_owned(),
            asset_license_url: TOKENIZER_ASSET_LICENSE_URL.to_owned(),
            asset_license_sha256: TOKENIZER_ASSET_LICENSE_SHA256.to_owned(),
            offline: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileAccounting {
    profile: String,
    tool_count: u64,
    #[serde(flatten)]
    measurement: TokenMeasurement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenMeasurement {
    input: InputEvidence,
    deterministic_estimated_tokens: u64,
    actual_tokens: u64,
}

#[cfg(test)]
impl TokenMeasurement {
    /// Returns the exact input-bound evidence.
    pub(crate) const fn input(&self) -> &InputEvidence {
        &self.input
    }

    /// Returns the provider-neutral deterministic estimate.
    pub(crate) const fn deterministic_estimated_tokens(&self) -> u64 {
        self.deterministic_estimated_tokens
    }

    /// Returns the count produced by the pinned tokenizer.
    pub(crate) const fn actual_tokens(&self) -> u64 {
        self.actual_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputEvidence {
    input_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    sha256: String,
    serialized_bytes: u64,
    normalization: String,
    framing: String,
}

#[cfg(test)]
impl InputEvidence {
    /// Returns the exact serialized UTF-8 byte length.
    pub(crate) const fn serialized_bytes(&self) -> u64 {
        self.serialized_bytes
    }

    /// Returns the SHA-256 digest of the exact tokenizer input.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Failure while producing or checking tokenizer accounting evidence.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenAccountingError {
    /// A required option value is absent.
    #[error("missing value for {0}")]
    MissingArgument(&'static str),
    /// The required output/revision option pair is incomplete.
    #[error("token-accounting-report requires --output-dir and --source-revision")]
    IncompleteOptions,
    /// The checker was invoked without its report option.
    #[error("token-accounting-check requires --report")]
    MissingReport,
    /// A report input that must be retained as an artifact has no local file.
    #[error("token accounting input is missing its artifact file")]
    MissingInputFile,
    /// An unknown or duplicate option was supplied.
    #[error("unexpected token accounting argument: {0}")]
    UnexpectedArgument(String),
    /// The revision is not a lowercase hexadecimal object identifier.
    #[error("invalid source revision: {0}")]
    InvalidSourceRevision(String),
    /// The report uses an unsupported schema.
    #[error("unsupported token accounting schema: {0}")]
    UnsupportedSchema(String),
    /// The report does not contain exactly the required profile set.
    #[error("token accounting profile count differs: expected {expected}, observed {observed}")]
    ProfileCount {
        /// Required profile count.
        expected: usize,
        /// Reported profile count.
        observed: usize,
    },
    /// The report profile order or membership differs.
    #[error("token accounting profile differs: expected {expected}, observed {observed}")]
    ProfileOrder {
        /// Required profile.
        expected: &'static str,
        /// Reported profile.
        observed: String,
    },
    /// A profile name cannot be mapped to the canonical catalog.
    #[error("unknown exposure profile in token accounting: {0}")]
    UnknownProfile(String),
    /// A report-controlled path could escape the evidence directory.
    #[error("unsafe token accounting artifact path: {}", .0.display())]
    UnsafePath(PathBuf),
    /// The report's tokenizer identity differs from the pinned adapter.
    #[error("tokenizer identity differs: expected {expected}, observed {observed}")]
    TokenizerIdentityMismatch {
        /// Required identity.
        expected: String,
        /// Reported identity.
        observed: String,
    },
    /// A payload differs from the complete canonical profile serialization.
    #[error(
        "{profile} tools/list payload differs: expected SHA-256 {expected}, observed {observed}"
    )]
    PayloadMismatch {
        /// Exposure profile.
        profile: &'static str,
        /// Canonical payload digest.
        expected: String,
        /// Observed payload digest.
        observed: String,
    },
    /// Reported accounting differs from independently recomputed values.
    #[error(
        "reported {profile} token accounting differs: expected {expected}, observed {observed}"
    )]
    MeasurementMismatch {
        /// Exposure profile.
        profile: &'static str,
        /// Recomputed accounting.
        expected: String,
        /// Reported accounting.
        observed: String,
    },
    /// The pinned tokenizer could not initialize from its embedded asset.
    #[error("failed to initialize the pinned offline tokenizer")]
    TokenizerInitialization(#[source] anyhow::Error),
    /// A payload or report could not be encoded or decoded.
    #[error("token accounting JSON failed")]
    Json(#[from] serde_json::Error),
    /// A payload was not valid exact UTF-8 input.
    #[error("token accounting input is not UTF-8: {}", path.display())]
    Utf8 {
        /// Affected path.
        path: PathBuf,
        /// UTF-8 failure.
        #[source]
        source: std::str::Utf8Error,
    },
    /// A report or payload exceeds its defensive read limit.
    #[error("token accounting artifact exceeds {limit} bytes: {}", path.display())]
    FileTooLarge {
        /// Affected path.
        path: PathBuf,
        /// Maximum accepted bytes.
        limit: usize,
    },
    /// A platform value could not fit the stable report representation.
    #[error("token accounting value exceeds u64: {0}")]
    IntegerOverflow(&'static str),
    /// An artifact could not be read or written.
    #[error("token accounting I/O failed for {}", path.display())]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const TOKENIZER_GOLDENS: &str =
        include_str!("../../tests/fixtures/token-accounting/o200k-base-v1.json");

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TokenizerGoldenFixture {
        schema: String,
        tokenizer: String,
        asset_sha256: String,
        cases: Vec<TokenizerGoldenCase>,
        profiles: Vec<ProfileGolden>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TokenizerGoldenCase {
        name: String,
        input: String,
        utf8_bytes: u64,
        sha256: String,
        actual_tokens: u64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ProfileGolden {
        profile: String,
        serialized_bytes: u64,
        sha256: String,
        actual_tokens: u64,
    }

    #[test]
    fn options_require_a_source_bound_output_directory() {
        let options = Options::parse(
            &mut [
                "--output-dir",
                "target/evidence",
                "--source-revision",
                REVISION,
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("complete options parse");
        assert_eq!(options.output_dir, PathBuf::from("target/evidence"));
        assert_eq!(options.source_revision, REVISION);

        assert!(matches!(
            Options::parse(
                &mut ["--output-dir", "target/evidence"]
                    .into_iter()
                    .map(str::to_owned)
            ),
            Err(TokenAccountingError::IncompleteOptions)
        ));
        assert!(matches!(
            Options::parse(
                &mut ["--output-dir", "out", "--source-revision", "ABC"]
                    .into_iter()
                    .map(str::to_owned)
            ),
            Err(TokenAccountingError::InvalidSourceRevision(_))
        ));
    }

    #[test]
    fn pinned_tokenizer_matches_the_utf8_golden() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let input = "{\"message\":\"Rootlight ✓ handles UTF-8 exactly.\",\"value\":42}";

        assert_eq!(
            sha256_hex(input.as_bytes()),
            "1101465066a5f8d685f7e7782cef5392753896aa9461cf9ac011a6a62131c6d3"
        );
        assert_eq!(tokenizer.count(input).expect("tokens fit u64"), 16);
        assert_eq!(
            tokenizer.count(input).expect("repeat count fits u64"),
            tokenizer.count(input).expect("comparison count fits u64")
        );
    }

    #[test]
    fn pinned_tokenizer_matches_the_cross_platform_corpus() {
        let fixture: TokenizerGoldenFixture =
            serde_json::from_str(TOKENIZER_GOLDENS).expect("golden fixture parses");
        assert_eq!(fixture.schema, "rootlight.tokenizer-goldens/1");
        assert_eq!(fixture.tokenizer, TOKENIZER_NAME);
        assert_eq!(fixture.asset_sha256, TOKENIZER_ASSET_SHA256);
        assert_eq!(
            fixture
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            [
                "ascii",
                "compact_json",
                "unicode",
                "whitespace_control",
                "special_token_lookalikes",
            ]
        );

        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        for case in fixture.cases {
            let measurement = measure_utf8_input(
                "tokenizer_golden",
                None,
                "raw_fixture_utf8",
                &case.input,
                &tokenizer,
            )
            .expect("golden input measures");
            assert_eq!(
                measurement.input().serialized_bytes(),
                case.utf8_bytes,
                "{} UTF-8 byte count drifted",
                case.name
            );
            assert_eq!(
                measurement.input().sha256(),
                case.sha256,
                "{} digest drifted",
                case.name
            );
            assert_eq!(
                measurement.actual_tokens(),
                case.actual_tokens,
                "{} actual-token count drifted",
                case.name
            );
        }
    }

    #[test]
    fn complete_profile_token_counts_match_retained_goldens() {
        let fixture: TokenizerGoldenFixture =
            serde_json::from_str(TOKENIZER_GOLDENS).expect("golden fixture parses");
        assert_eq!(fixture.profiles.len(), ExposureProfile::ALL.len());
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");

        for (profile, golden) in ExposureProfile::ALL.into_iter().zip(fixture.profiles) {
            assert_eq!(golden.profile, profile.name());
            let encoded = serialize_payload(profile).expect("canonical payload serializes");
            let text = std::str::from_utf8(&encoded).expect("canonical payload is UTF-8");
            let measurement = measure_utf8_input(
                TOOLS_LIST_INPUT_KIND,
                None,
                TOOLS_LIST_FRAMING,
                text,
                &tokenizer,
            )
            .expect("canonical profile measures");
            assert_eq!(
                measurement.input().serialized_bytes(),
                golden.serialized_bytes,
                "{} serialized byte count drifted",
                profile.name()
            );
            assert_eq!(
                measurement.input().sha256(),
                golden.sha256,
                "{} payload digest drifted",
                profile.name()
            );
            assert_eq!(
                measurement.actual_tokens(),
                golden.actual_tokens,
                "{} actual-token count drifted",
                profile.name()
            );
        }
    }

    #[test]
    fn complete_profile_reports_are_monotonic_and_reproducible() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let first = build_report(REVISION, &tokenizer).expect("first report builds");
        let second = build_report(REVISION, &tokenizer).expect("second report builds");

        assert_eq!(first, second);
        assert_eq!(
            first
                .payloads
                .iter()
                .map(|payload| payload.profile.as_str())
                .collect::<Vec<_>>(),
            ["scout", "analysis", "developer"]
        );
        for pair in first.payloads.windows(2) {
            assert!(pair[0].tool_count < pair[1].tool_count);
            assert!(
                pair[0].measurement.input().serialized_bytes()
                    < pair[1].measurement.input().serialized_bytes()
            );
            assert!(
                pair[0].measurement.deterministic_estimated_tokens()
                    < pair[1].measurement.deterministic_estimated_tokens()
            );
            assert!(pair[0].measurement.actual_tokens() < pair[1].measurement.actual_tokens());
        }
        assert!(
            first
                .payloads
                .iter()
                .any(|payload| payload.measurement.actual_tokens()
                    != payload.measurement.deterministic_estimated_tokens())
        );
    }

    #[test]
    fn checker_rejects_mutated_tokenizer_identity_and_input_digest() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let options = Options {
            output_dir: directory.path().to_path_buf(),
            source_revision: REVISION.to_owned(),
        };
        emit(&options).expect("evidence emits");
        let report_path = directory.path().join(REPORT_FILE);
        check(&report_path).expect("fresh evidence verifies");

        let report_bytes = fs::read(&report_path).expect("report reads");
        let mut report: TokenAccountingReport =
            serde_json::from_slice(&report_bytes).expect("report parses");
        report.tokenizer.asset_sha256 = "0".repeat(64);
        write_report(&report_path, &report);
        assert!(matches!(
            check(&report_path),
            Err(TokenAccountingError::TokenizerIdentityMismatch { .. })
        ));

        report.tokenizer = TokenizerIdentity::expected();
        report.payloads[0].measurement.input.sha256 = "0".repeat(64);
        write_report(&report_path, &report);
        assert!(matches!(
            check(&report_path),
            Err(TokenAccountingError::MeasurementMismatch {
                profile: "scout",
                ..
            })
        ));
    }

    #[test]
    fn checker_rejects_missing_tokenizer_metadata() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let report = build_report(REVISION, &tokenizer).expect("report builds");
        let mut value = serde_json::to_value(report).expect("report converts to JSON");
        value
            .get_mut("tokenizer")
            .and_then(serde_json::Value::as_object_mut)
            .expect("tokenizer metadata is an object")
            .remove("implementation_version");
        let report_path = directory.path().join(REPORT_FILE);
        let bytes = serde_json::to_vec(&value).expect("mutated report serializes");
        fs::write(&report_path, bytes).expect("mutated report writes");

        assert!(matches!(
            check(&report_path),
            Err(TokenAccountingError::Json(_))
        ));
    }

    #[test]
    fn checker_rejects_nonlocal_payload_paths() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let mut report = build_report(REVISION, &tokenizer).expect("report builds");
        report.payloads[0].measurement.input.file = Some("../outside.json".to_owned());
        let report_path = directory.path().join(REPORT_FILE);
        write_report(&report_path, &report);

        assert!(matches!(
            check(&report_path),
            Err(TokenAccountingError::UnsafePath(_))
        ));
    }

    fn write_report(path: &Path, report: &TokenAccountingReport) {
        let mut bytes = serde_json::to_vec_pretty(report).expect("report serializes");
        bytes.push(b'\n');
        fs::write(path, bytes).expect("report writes");
    }
}
