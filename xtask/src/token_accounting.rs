//! Produces reproducible actual-token evidence for complete MCP discovery payloads.
//!
//! Runtime safety retains provider-neutral estimates; this module measures exact
//! compact JSON bytes with one pinned, fully offline tokenizer implementation.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use rootlight_bench::{
    ActualTokenizerIdentity, TokenInputKind, TokenMeasurement as BoundaryTokenMeasurement,
};
use rootlight_mcp_contract::accounting::{estimate_tokens, tool_list_payload};
use rootlight_mcp_contract::catalog::ExposureProfile;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tiktoken_rs::CoreBPE;

const REPORT_SCHEMA: &str = "rootlight.mcp-token-accounting/3";
const REPORT_FILE: &str = "token-accounting-v3.json";
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
const ATTRIBUTION_SCHEMA: &str = "rootlight.mcp-token-attribution/1";
const BATCH_OPERATION_FRAMING: &str = "one_canonical_batch_operation_without_delimiter";
const CONTEXT_SECTION_FRAMING: &str = "one_canonical_context_section_without_delimiter";
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
    write_retained_attribution_inputs(&options.output_dir)?;
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    write_bytes(&options.output_dir.join(REPORT_FILE), &encoded)?;
    println!(
        "token accounting written for {} profiles and {} attributions using {} {}",
        report.payloads.len(),
        report.attributions.len(),
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
    validate_attributions(report_dir, &report.attributions, &tokenizer)?;
    println!(
        "token accounting verified for {} profiles and {} attributions at {}",
        report.payloads.len(),
        report.attributions.len(),
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

/// Exact UTF-8 input assigned to one stable batch operation or context section.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LabeledTokenInput<'a> {
    label: &'a str,
    input: &'a [u8],
}

impl<'a> LabeledTokenInput<'a> {
    /// Binds a stable record label to the exact bytes presented to the tokenizer.
    pub(crate) const fn new(label: &'a str, input: &'a [u8]) -> Self {
        Self { label, input }
    }
}

const RETAINED_BATCH_OPERATIONS: [LabeledTokenInput<'static>; 2] = [
    LabeledTokenInput::new(
        "locate",
        br#"{"id":"locate","tool":"code.locate","status":"ok","data":{"matches":[]},"truncated":false,"next_cursor":null,"warnings":[]}"#,
    ),
    LabeledTokenInput::new(
        "relationships",
        br#"{"id":"relationships","tool":"symbol.relationships","status":"ok","data":{"symbols":[]},"truncated":false,"next_cursor":null,"warnings":[]}"#,
    ),
];

const RETAINED_CONTEXT_SECTIONS: [LabeledTokenInput<'static>; 3] = [
    LabeledTokenInput::new(
        "architecture",
        br#"{"section":"architecture","items":[{"role":"architecture","score":900,"tokens":18}]}"#,
    ),
    LabeledTokenInput::new(
        "definitions",
        br#"{"section":"definitions","items":[{"role":"definition","score":950,"tokens":22}]}"#,
    ),
    LabeledTokenInput::new(
        "tests",
        br#"{"section":"tests","items":[{"role":"test","score":850,"tokens":16}]}"#,
    ),
];

/// Offline per-operation or per-section evidence with checked aggregate totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenAttributionReport {
    schema: String,
    tokenizer: TokenizerIdentity,
    input_kind: TokenInputKind,
    records: Vec<LabeledTokenMeasurement>,
    aggregate: TokenAttributionTotals,
}

impl TokenAttributionReport {
    /// Measures batch children in their caller-supplied request order.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] for missing or duplicate labels,
    /// non-UTF-8 input, tokenizer failure, or an overflowing aggregate.
    pub(crate) fn measure_batch_operations(
        inputs: &[LabeledTokenInput<'_>],
        tokenizer: &impl OfflineTokenizer,
    ) -> Result<Self, TokenAccountingError> {
        Self::measure(
            TokenInputKind::BatchOperation,
            inputs,
            RecordOrdering::Preserve,
            BATCH_OPERATION_FRAMING,
            tokenizer,
        )
    }

    /// Measures context sections in canonical lexical label order.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] for missing or duplicate labels,
    /// non-UTF-8 input, tokenizer failure, or an overflowing aggregate.
    pub(crate) fn measure_context_sections(
        inputs: &[LabeledTokenInput<'_>],
        tokenizer: &impl OfflineTokenizer,
    ) -> Result<Self, TokenAccountingError> {
        Self::measure(
            TokenInputKind::ContextSection,
            inputs,
            RecordOrdering::Lexical,
            CONTEXT_SECTION_FRAMING,
            tokenizer,
        )
    }

    /// Verifies exact batch-operation inputs against retained evidence.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] when the report is malformed, its
    /// tokenizer identity differs, or any record or aggregate has drifted.
    pub(crate) fn verify_batch_operations(
        &self,
        inputs: &[LabeledTokenInput<'_>],
        tokenizer: &impl OfflineTokenizer,
    ) -> Result<(), TokenAccountingError> {
        self.verify(
            TokenInputKind::BatchOperation,
            inputs,
            RecordOrdering::Preserve,
            BATCH_OPERATION_FRAMING,
            tokenizer,
        )
    }

    /// Verifies exact context-section inputs against retained evidence.
    ///
    /// # Errors
    ///
    /// Returns [`TokenAccountingError`] when the report is malformed, its
    /// tokenizer identity differs, or any record or aggregate has drifted.
    pub(crate) fn verify_context_sections(
        &self,
        inputs: &[LabeledTokenInput<'_>],
        tokenizer: &impl OfflineTokenizer,
    ) -> Result<(), TokenAccountingError> {
        self.verify(
            TokenInputKind::ContextSection,
            inputs,
            RecordOrdering::Lexical,
            CONTEXT_SECTION_FRAMING,
            tokenizer,
        )
    }

    fn measure(
        input_kind: TokenInputKind,
        inputs: &[LabeledTokenInput<'_>],
        ordering: RecordOrdering,
        framing: &'static str,
        tokenizer: &impl OfflineTokenizer,
    ) -> Result<Self, TokenAccountingError> {
        validate_labeled_inputs(input_kind, inputs)?;
        let mut ordered = inputs.to_vec();
        if ordering == RecordOrdering::Lexical {
            ordered.sort_unstable_by_key(|input| input.label);
        }
        let mut records = Vec::new();
        records
            .try_reserve_exact(ordered.len())
            .map_err(|_| TokenAccountingError::MemoryUnavailable)?;
        for input in ordered {
            records.push(LabeledTokenMeasurement {
                label: input.label.to_owned(),
                input_file: attribution_input_file(input_kind, input.label),
                measurement: measure_boundary_input(
                    input_kind,
                    input.label,
                    input.input,
                    framing,
                    tokenizer,
                )?,
            });
        }
        let aggregate = TokenAttributionTotals::from_records(&records)?;
        let report = Self {
            schema: ATTRIBUTION_SCHEMA.to_owned(),
            tokenizer: tokenizer.identity(),
            input_kind,
            records,
            aggregate,
        };
        report.validate()?;
        Ok(report)
    }

    fn verify(
        &self,
        input_kind: TokenInputKind,
        inputs: &[LabeledTokenInput<'_>],
        ordering: RecordOrdering,
        framing: &'static str,
        tokenizer: &impl OfflineTokenizer,
    ) -> Result<(), TokenAccountingError> {
        self.validate()?;
        if self.tokenizer != tokenizer.identity() {
            return Err(TokenAccountingError::TokenizerIdentityMismatch {
                expected: serde_json::to_string(&tokenizer.identity())?,
                observed: serde_json::to_string(&self.tokenizer)?,
            });
        }
        let expected = Self::measure(input_kind, inputs, ordering, framing, tokenizer)?;
        if *self != expected {
            return Err(TokenAccountingError::AttributionMismatch {
                kind: token_input_kind_name(input_kind),
                expected: serde_json::to_string(&expected)?,
                observed: serde_json::to_string(self)?,
            });
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), TokenAccountingError> {
        if self.schema != ATTRIBUTION_SCHEMA {
            return Err(TokenAccountingError::UnsupportedSchema(self.schema.clone()));
        }
        let expected_identity = TokenizerIdentity::expected();
        if self.tokenizer != expected_identity {
            return Err(TokenAccountingError::TokenizerIdentityMismatch {
                expected: serde_json::to_string(&expected_identity)?,
                observed: serde_json::to_string(&self.tokenizer)?,
            });
        }
        if self.records.is_empty() {
            return Err(TokenAccountingError::MissingAttributionRecords(
                token_input_kind_name(self.input_kind),
            ));
        }
        let mut labels = std::collections::BTreeSet::new();
        for record in &self.records {
            validate_record_label(&record.label)?;
            if !labels.insert(record.label.as_str()) {
                return Err(TokenAccountingError::DuplicateAttributionLabel(
                    record.label.clone(),
                ));
            }
            let expected_file = attribution_input_file(self.input_kind, &record.label);
            if record.input_file != expected_file || !safe_file_name(&record.input_file) {
                return Err(TokenAccountingError::UnsafePath(PathBuf::from(
                    &record.input_file,
                )));
            }
            record.measurement.validate().map_err(|source| {
                TokenAccountingError::BoundaryMeasurement {
                    label: record.label.clone(),
                    source,
                }
            })?;
            if record.measurement.input_kind != self.input_kind {
                return Err(TokenAccountingError::AttributionKindMismatch {
                    label: record.label.clone(),
                    expected: token_input_kind_name(self.input_kind),
                    observed: token_input_kind_name(record.measurement.input_kind),
                });
            }
            if record.measurement.actual_tokens.is_none() {
                return Err(TokenAccountingError::MissingActualTokenCount(
                    record.label.clone(),
                ));
            }
        }
        if self.input_kind == TokenInputKind::ContextSection
            && self
                .records
                .windows(2)
                .any(|pair| pair[0].label >= pair[1].label)
        {
            return Err(TokenAccountingError::NonCanonicalAttributionOrder);
        }
        let expected = TokenAttributionTotals::from_records(&self.records)?;
        if self.aggregate != expected {
            return Err(TokenAccountingError::AttributionTotalsMismatch {
                expected,
                observed: self.aggregate,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabeledTokenMeasurement {
    label: String,
    input_file: String,
    measurement: BoundaryTokenMeasurement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenAttributionTotals {
    record_count: u64,
    serialized_bytes: u64,
    deterministic_estimated_tokens: u64,
    actual_tokens: u64,
}

impl TokenAttributionTotals {
    fn from_records(records: &[LabeledTokenMeasurement]) -> Result<Self, TokenAccountingError> {
        let mut total = Self {
            record_count: u64::try_from(records.len())
                .map_err(|_| TokenAccountingError::IntegerOverflow("attribution record count"))?,
            serialized_bytes: 0,
            deterministic_estimated_tokens: 0,
            actual_tokens: 0,
        };
        for record in records {
            total.serialized_bytes = total
                .serialized_bytes
                .checked_add(record.measurement.serialized_bytes)
                .ok_or(TokenAccountingError::IntegerOverflow(
                    "attribution serialized byte total",
                ))?;
            total.deterministic_estimated_tokens = total
                .deterministic_estimated_tokens
                .checked_add(record.measurement.deterministic_estimated_tokens)
                .ok_or(TokenAccountingError::IntegerOverflow(
                    "attribution estimated token total",
                ))?;
            total.actual_tokens = total
                .actual_tokens
                .checked_add(record.measurement.actual_tokens.ok_or_else(|| {
                    TokenAccountingError::MissingActualTokenCount(record.label.clone())
                })?)
                .ok_or(TokenAccountingError::IntegerOverflow(
                    "attribution actual token total",
                ))?;
        }
        Ok(total)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordOrdering {
    Preserve,
    Lexical,
}

fn validate_labeled_inputs(
    input_kind: TokenInputKind,
    inputs: &[LabeledTokenInput<'_>],
) -> Result<(), TokenAccountingError> {
    if inputs.is_empty() {
        return Err(TokenAccountingError::MissingAttributionRecords(
            token_input_kind_name(input_kind),
        ));
    }
    let mut labels = std::collections::BTreeSet::new();
    for input in inputs {
        validate_record_label(input.label)?;
        if !labels.insert(input.label) {
            return Err(TokenAccountingError::DuplicateAttributionLabel(
                input.label.to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_record_label(label: &str) -> Result<(), TokenAccountingError> {
    if label.is_empty() {
        return Err(TokenAccountingError::MissingAttributionLabel);
    }
    if label.len() > 128
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(TokenAccountingError::InvalidAttributionLabel(
            label.to_owned(),
        ));
    }
    Ok(())
}

fn measure_boundary_input(
    input_kind: TokenInputKind,
    label: &str,
    input: &[u8],
    framing: &'static str,
    tokenizer: &impl OfflineTokenizer,
) -> Result<BoundaryTokenMeasurement, TokenAccountingError> {
    let text =
        std::str::from_utf8(input).map_err(|source| TokenAccountingError::AttributionUtf8 {
            label: label.to_owned(),
            source,
        })?;
    Ok(BoundaryTokenMeasurement::from_input(
        input_kind,
        input,
        estimate_tokens(input.len()),
        Some(tokenizer.count(text)?),
        NORMALIZATION,
        framing,
    ))
}

fn attribution_input_file(input_kind: TokenInputKind, label: &str) -> String {
    format!("{}-{label}.json", token_input_kind_name(input_kind))
}

const fn token_input_kind_name(input_kind: TokenInputKind) -> &'static str {
    match input_kind {
        TokenInputKind::Request => "request",
        TokenInputKind::Response => "response",
        TokenInputKind::Source => "source",
        TokenInputKind::ToolList => "tool_list",
        TokenInputKind::BatchOperation => "batch_operation",
        TokenInputKind::ContextSection => "context_section",
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
        attributions: vec![
            TokenAttributionReport::measure_batch_operations(
                &RETAINED_BATCH_OPERATIONS,
                tokenizer,
            )?,
            TokenAttributionReport::measure_context_sections(
                &RETAINED_CONTEXT_SECTIONS,
                tokenizer,
            )?,
        ],
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
    if report.attributions.len() != 2 {
        return Err(TokenAccountingError::AttributionCount {
            expected: 2,
            observed: report.attributions.len(),
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

fn write_retained_attribution_inputs(output_dir: &Path) -> Result<(), TokenAccountingError> {
    for (input_kind, inputs) in [
        (
            TokenInputKind::BatchOperation,
            RETAINED_BATCH_OPERATIONS.as_slice(),
        ),
        (
            TokenInputKind::ContextSection,
            RETAINED_CONTEXT_SECTIONS.as_slice(),
        ),
    ] {
        for input in inputs {
            write_bytes(
                &output_dir.join(attribution_input_file(input_kind, input.label)),
                input.input,
            )?;
        }
    }
    Ok(())
}

fn validate_attributions(
    report_dir: &Path,
    attributions: &[TokenAttributionReport],
    tokenizer: &impl OfflineTokenizer,
) -> Result<(), TokenAccountingError> {
    for (index, attribution) in attributions.iter().enumerate() {
        attribution.validate()?;
        let expected_kind = match index {
            0 => TokenInputKind::BatchOperation,
            1 => TokenInputKind::ContextSection,
            _ => {
                return Err(TokenAccountingError::AttributionCount {
                    expected: 2,
                    observed: attributions.len(),
                });
            }
        };
        if attribution.input_kind != expected_kind {
            return Err(TokenAccountingError::AttributionOrder {
                index,
                expected: token_input_kind_name(expected_kind),
                observed: token_input_kind_name(attribution.input_kind),
            });
        }

        let mut retained = Vec::new();
        retained
            .try_reserve_exact(attribution.records.len())
            .map_err(|_| TokenAccountingError::MemoryUnavailable)?;
        for record in &attribution.records {
            let path = report_dir.join(&record.input_file);
            retained.push((
                record.label.clone(),
                read_bounded(&path, MAX_PAYLOAD_BYTES)?,
            ));
        }
        let inputs: Vec<_> = retained
            .iter()
            .map(|(label, input)| LabeledTokenInput::new(label, input))
            .collect();
        match expected_kind {
            TokenInputKind::BatchOperation => {
                attribution.verify_batch_operations(&inputs, tokenizer)?;
            }
            TokenInputKind::ContextSection => {
                attribution.verify_context_sections(&inputs, tokenizer)?;
            }
            TokenInputKind::Request
            | TokenInputKind::Response
            | TokenInputKind::Source
            | TokenInputKind::ToolList => {
                return Err(TokenAccountingError::AttributionOrder {
                    index,
                    expected: "batch_operation_or_context_section",
                    observed: token_input_kind_name(expected_kind),
                });
            }
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
    attributions: Vec<TokenAttributionReport>,
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
    /// A report does not contain exactly the retained attribution boundaries.
    #[error("token attribution count differs: expected {expected}, observed {observed}")]
    AttributionCount {
        /// Required attribution count.
        expected: usize,
        /// Reported attribution count.
        observed: usize,
    },
    /// Retained attribution boundaries are not in canonical report order.
    #[error("token attribution {index} differs: expected {expected}, observed {observed}")]
    AttributionOrder {
        /// Zero-based report position.
        index: usize,
        /// Required semantic boundary.
        expected: &'static str,
        /// Reported semantic boundary.
        observed: &'static str,
    },
    /// A profile name cannot be mapped to the canonical catalog.
    #[error("unknown exposure profile in token accounting: {0}")]
    UnknownProfile(String),
    /// A batch or context attribution report contains no records.
    #[error("token attribution for {0} requires at least one record")]
    MissingAttributionRecords(&'static str),
    /// A batch operation or context section omitted its stable label.
    #[error("token attribution record label is missing")]
    MissingAttributionLabel,
    /// A batch operation or context section label is not stable and bounded.
    #[error("invalid token attribution record label: {0}")]
    InvalidAttributionLabel(String),
    /// Stable labels must uniquely identify records within one attribution.
    #[error("duplicate token attribution record label: {0}")]
    DuplicateAttributionLabel(String),
    /// Context-section records were not retained in canonical lexical order.
    #[error("context-section token attribution order is not canonical")]
    NonCanonicalAttributionOrder,
    /// A retained boundary differs from the report-level boundary.
    #[error("token attribution kind differs for {label}: expected {expected}, observed {observed}")]
    AttributionKindMismatch {
        /// Stable record label.
        label: String,
        /// Report-level boundary.
        expected: &'static str,
        /// Record-level boundary.
        observed: &'static str,
    },
    /// Actual counts are mandatory in offline attribution evidence.
    #[error("token attribution record {0} is missing its actual token count")]
    MissingActualTokenCount(String),
    /// A retained record failed the shared boundary contract.
    #[error("token attribution record {label} is invalid")]
    BoundaryMeasurement {
        /// Stable record label.
        label: String,
        /// Shared measurement failure.
        #[source]
        source: rootlight_bench::TokenAccountingError,
    },
    /// Caller-supplied exact bytes were not valid UTF-8 tokenizer input.
    #[error("token attribution input is not UTF-8 for {label}")]
    AttributionUtf8 {
        /// Stable record label.
        label: String,
        /// UTF-8 failure.
        #[source]
        source: std::str::Utf8Error,
    },
    /// Retained records do not reconcile with their reported aggregate.
    #[error("token attribution totals differ: expected {expected:?}, observed {observed:?}")]
    AttributionTotalsMismatch {
        /// Totals recomputed from records.
        expected: TokenAttributionTotals,
        /// Totals retained in the report.
        observed: TokenAttributionTotals,
    },
    /// Exact caller inputs differ from the retained attribution evidence.
    #[error("{kind} token attribution differs: expected {expected}, observed {observed}")]
    AttributionMismatch {
        /// Semantic attribution boundary.
        kind: &'static str,
        /// Recomputed canonical evidence.
        expected: String,
        /// Retained evidence.
        observed: String,
    },
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
    /// A bounded token attribution allocation failed.
    #[error("memory unavailable while building token attribution")]
    MemoryUnavailable,
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
    const ATTRIBUTION_GOLDENS: &str =
        include_str!("../../tests/fixtures/token-accounting/attribution-v1.json");

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

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AttributionGoldenFixture {
        schema: String,
        tokenizer: TokenizerIdentity,
        attributions: Vec<AttributionGolden>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AttributionGolden {
        input_kind: TokenInputKind,
        records: Vec<AttributionRecordGolden>,
        aggregate: TokenAttributionTotals,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AttributionRecordGolden {
        label: String,
        input_file: String,
        input: String,
        input_sha256: String,
        serialized_bytes: u64,
        deterministic_estimated_tokens: u64,
        actual_tokens: u64,
        normalization: String,
        framing: String,
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
    fn retained_attribution_fixture_reconstructs_every_exact_input() {
        let fixture: AttributionGoldenFixture =
            serde_json::from_str(ATTRIBUTION_GOLDENS).expect("attribution fixture parses");
        assert_eq!(fixture.schema, "rootlight.token-attribution-goldens/1");
        assert_eq!(fixture.tokenizer, TokenizerIdentity::expected());
        assert_eq!(fixture.attributions.len(), 2);
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");

        for golden in fixture.attributions {
            let inputs: Vec<_> = golden
                .records
                .iter()
                .map(|record| LabeledTokenInput::new(&record.label, record.input.as_bytes()))
                .collect();
            let report = match golden.input_kind {
                TokenInputKind::BatchOperation => {
                    TokenAttributionReport::measure_batch_operations(&inputs, &tokenizer)
                        .expect("retained batch operations measure")
                }
                TokenInputKind::ContextSection => {
                    TokenAttributionReport::measure_context_sections(&inputs, &tokenizer)
                        .expect("retained context sections measure")
                }
                TokenInputKind::Request
                | TokenInputKind::Response
                | TokenInputKind::Source
                | TokenInputKind::ToolList => {
                    panic!("attribution fixture contains a non-attribution input kind")
                }
            };

            assert_eq!(report.tokenizer, fixture.tokenizer);
            assert_eq!(report.aggregate, golden.aggregate);
            assert_eq!(report.records.len(), golden.records.len());
            for (record, retained) in report.records.iter().zip(golden.records) {
                assert_eq!(record.label, retained.label);
                assert_eq!(record.input_file, retained.input_file);
                assert_eq!(record.measurement.input_sha256, retained.input_sha256);
                assert_eq!(
                    record.measurement.serialized_bytes,
                    retained.serialized_bytes
                );
                assert_eq!(
                    record.measurement.deterministic_estimated_tokens,
                    retained.deterministic_estimated_tokens
                );
                assert_eq!(
                    record.measurement.actual_tokens,
                    Some(retained.actual_tokens)
                );
                assert_eq!(record.measurement.normalization, retained.normalization);
                assert_eq!(record.measurement.framing, retained.framing);
            }
        }
    }

    #[test]
    fn batch_operation_records_preserve_order_and_reconcile_totals() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let inputs = [
            LabeledTokenInput::new(
                "locate",
                br#"{"id":"locate","status":"ok","data":{"matches":[]}}"#,
            ),
            LabeledTokenInput::new(
                "explain",
                br#"{"id":"explain","status":"ok","data":{"symbols":[]}}"#,
            ),
        ];

        let report = TokenAttributionReport::measure_batch_operations(&inputs, &tokenizer)
            .expect("batch operations measure");
        report
            .verify_batch_operations(&inputs, &tokenizer)
            .expect("batch operation evidence verifies");

        assert_eq!(report.input_kind, TokenInputKind::BatchOperation);
        assert_eq!(
            report
                .records
                .iter()
                .map(|record| record.label.as_str())
                .collect::<Vec<_>>(),
            ["locate", "explain"]
        );
        assert_eq!(
            report.aggregate.serialized_bytes,
            report
                .records
                .iter()
                .map(|record| record.measurement.serialized_bytes)
                .sum::<u64>()
        );
        assert_eq!(
            report.aggregate.actual_tokens,
            report
                .records
                .iter()
                .map(|record| {
                    record
                        .measurement
                        .actual_tokens
                        .expect("offline measurements always retain actual counts")
                })
                .sum::<u64>()
        );
    }

    #[test]
    fn context_section_records_sort_labels_and_reconcile_totals() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let inputs = [
            LabeledTokenInput::new(
                "tests",
                br#"{"section":"tests","items":[{"test_id":"test1_example"}]}"#,
            ),
            LabeledTokenInput::new(
                "definitions",
                br#"{"section":"definitions","items":[{"symbol_id":"sym1_example"}]}"#,
            ),
            LabeledTokenInput::new("architecture", br#"{"section":"architecture","items":[]}"#),
        ];

        let report = TokenAttributionReport::measure_context_sections(&inputs, &tokenizer)
            .expect("context sections measure");
        report
            .verify_context_sections(&inputs, &tokenizer)
            .expect("context section evidence verifies");

        assert_eq!(report.input_kind, TokenInputKind::ContextSection);
        assert_eq!(
            report
                .records
                .iter()
                .map(|record| record.label.as_str())
                .collect::<Vec<_>>(),
            ["architecture", "definitions", "tests"]
        );
        assert_eq!(
            report.aggregate.deterministic_estimated_tokens,
            report
                .records
                .iter()
                .map(|record| record.measurement.deterministic_estimated_tokens)
                .sum::<u64>()
        );
        assert_eq!(
            report.aggregate.actual_tokens,
            report
                .records
                .iter()
                .map(|record| {
                    record
                        .measurement
                        .actual_tokens
                        .expect("offline measurements always retain actual counts")
                })
                .sum::<u64>()
        );
    }

    #[test]
    fn attribution_generation_rejects_missing_duplicate_and_invalid_inputs() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        assert!(matches!(
            TokenAttributionReport::measure_batch_operations(&[], &tokenizer),
            Err(TokenAccountingError::MissingAttributionRecords(
                "batch_operation"
            ))
        ));

        let duplicate = [
            LabeledTokenInput::new("same", br#"{"value":1}"#),
            LabeledTokenInput::new("same", br#"{"value":2}"#),
        ];
        assert!(matches!(
            TokenAttributionReport::measure_context_sections(&duplicate, &tokenizer),
            Err(TokenAccountingError::DuplicateAttributionLabel(label)) if label == "same"
        ));

        let missing = [LabeledTokenInput::new("", br#"{"value":1}"#)];
        assert!(matches!(
            TokenAttributionReport::measure_batch_operations(&missing, &tokenizer),
            Err(TokenAccountingError::MissingAttributionLabel)
        ));

        let invalid_utf8 = [LabeledTokenInput::new("invalid", &[0xff])];
        assert!(matches!(
            TokenAttributionReport::measure_batch_operations(&invalid_utf8, &tokenizer),
            Err(TokenAccountingError::AttributionUtf8 { label, .. }) if label == "invalid"
        ));
    }

    #[test]
    fn attribution_verifier_rejects_identity_input_and_total_drift() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let inputs = [
            LabeledTokenInput::new("architecture", br#"{"items":[]}"#),
            LabeledTokenInput::new("tests", br#"{"items":[1]}"#),
        ];
        let report = TokenAttributionReport::measure_context_sections(&inputs, &tokenizer)
            .expect("context sections measure");

        let changed_inputs = [
            LabeledTokenInput::new("architecture", br#"{"items":[]}"#),
            LabeledTokenInput::new("tests", br#"{"items":[1,2]}"#),
        ];
        assert!(matches!(
            report.verify_context_sections(&changed_inputs, &tokenizer),
            Err(TokenAccountingError::AttributionMismatch {
                kind: "context_section",
                ..
            })
        ));

        let mut changed_identity = report.clone();
        changed_identity.tokenizer.implementation_version = "0.0.0".to_owned();
        assert!(matches!(
            changed_identity.verify_context_sections(&inputs, &tokenizer),
            Err(TokenAccountingError::TokenizerIdentityMismatch { .. })
        ));

        let mut changed_digest = report.clone();
        changed_digest.records[0].measurement.input_sha256 = "0".repeat(64);
        assert!(matches!(
            changed_digest.verify_context_sections(&inputs, &tokenizer),
            Err(TokenAccountingError::AttributionMismatch {
                kind: "context_section",
                ..
            })
        ));

        let mut changed_total = report;
        changed_total.aggregate.serialized_bytes = changed_total
            .aggregate
            .serialized_bytes
            .checked_add(1)
            .expect("small test total cannot overflow");
        assert!(matches!(
            changed_total.verify_context_sections(&inputs, &tokenizer),
            Err(TokenAccountingError::AttributionTotalsMismatch { .. })
        ));
    }

    #[test]
    fn attribution_report_requires_complete_tokenizer_identity() {
        let tokenizer = O200kTokenizer::new().expect("pinned tokenizer initializes");
        let report = TokenAttributionReport::measure_batch_operations(
            &[LabeledTokenInput::new("locate", br#"{"matches":[]}"#)],
            &tokenizer,
        )
        .expect("batch operation measures");
        let mut value = serde_json::to_value(report).expect("report converts to JSON");
        value
            .get_mut("tokenizer")
            .and_then(serde_json::Value::as_object_mut)
            .expect("tokenizer identity is an object")
            .remove("implementation_package_sha256");

        assert!(serde_json::from_value::<TokenAttributionReport>(value).is_err());
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
    fn checker_reconstructs_every_retained_attribution_input() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let options = Options {
            output_dir: directory.path().to_path_buf(),
            source_revision: REVISION.to_owned(),
        };
        emit(&options).expect("evidence emits");
        let report_path = directory.path().join(REPORT_FILE);
        check(&report_path).expect("fresh retained inputs verify");

        let report_bytes = fs::read(&report_path).expect("report reads");
        let report: TokenAccountingReport =
            serde_json::from_slice(&report_bytes).expect("report parses");
        assert_eq!(report.attributions.len(), 2);
        assert_eq!(
            report
                .attributions
                .iter()
                .flat_map(|attribution| &attribution.records)
                .count(),
            RETAINED_BATCH_OPERATIONS.len() + RETAINED_CONTEXT_SECTIONS.len()
        );

        for record in report
            .attributions
            .iter()
            .flat_map(|attribution| &attribution.records)
        {
            let path = directory.path().join(&record.input_file);
            let original = fs::read(&path).expect("retained exact input reads");
            let mut changed = original.clone();
            changed.push(b' ');
            fs::write(&path, changed).expect("changed exact input writes");
            assert!(matches!(
                check(&report_path),
                Err(TokenAccountingError::AttributionMismatch { .. })
            ));
            fs::write(&path, original).expect("exact input restores");
            check(&report_path).expect("restored exact input verifies");
        }
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
