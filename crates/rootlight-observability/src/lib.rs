//! Source-free operational evidence and deterministic support archives.
//!
//! This crate accepts only allow-listed aggregate data. It owns the privacy and
//! size boundary for support bundles so transport and CLI layers cannot add
//! repository content, identifiers, paths, or arbitrary diagnostic text.

#![forbid(unsafe_code)]

use std::io::{Cursor, Write as _};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// Current support-bundle schema version.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION: u32 = 1;
/// Maximum encoded support archive returned through daemon IPC.
pub const MAX_SUPPORT_ARCHIVE_BYTES: usize = 768 * 1024;
/// Maximum JSON payload accepted for one support entry.
pub const MAX_SUPPORT_ENTRY_BYTES: usize = 128 * 1024;

const SUPPORT_ENTRY_COUNT: usize = 5;
/// Ordered allow-list for the current support archive schema.
pub const SUPPORT_ENTRY_NAMES: [&str; SUPPORT_ENTRY_COUNT] = [
    "diagnostics/quick.json",
    "health.json",
    "manifest.json",
    "operations-summary.json",
    "redaction-report.json",
];
/// Data classes that the current support schema must explicitly omit.
pub const OMITTED_DATA_CLASSES: [&str; 12] = [
    "absolute_roots",
    "adapter_output",
    "compiler_output",
    "credentials",
    "environment",
    "identifiers",
    "paths",
    "prompts",
    "raw_logs",
    "raw_sqlite_errors",
    "source",
    "traces",
];

/// Closed daemon protocol version emitted by this support schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolVersion {
    /// Rootlight daemon protocol 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Closed target operating-system family emitted by support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    /// Linux target family.
    Linux,
    /// macOS target family.
    Macos,
    /// Windows target family.
    Windows,
    /// Another target family not yet classified by this schema.
    Other,
}

/// Closed target architecture emitted by support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Architecture {
    /// 64-bit Arm target.
    #[serde(rename = "aarch64")]
    Aarch64,
    /// 32-bit Arm target.
    #[serde(rename = "arm")]
    Arm,
    /// 32-bit x86 target.
    #[serde(rename = "x86")]
    X86,
    /// 64-bit x86 target.
    #[serde(rename = "x86_64")]
    X86_64,
    /// Another target architecture not yet classified by this schema.
    #[serde(rename = "other")]
    Other,
}

/// Closed source-free daemon lifecycle used in operational evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLifecycle {
    /// Startup or recovery is in progress.
    Starting,
    /// The daemon is ready for requests.
    Ready,
    /// Shutdown has begun and admission is closed.
    Draining,
    /// A required subsystem failed.
    Faulted,
    /// The in-process host stopped.
    Stopped,
}

/// Closed stable public error code accepted by support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// The caller supplied an invalid value.
    InvalidArgument,
    /// The requested entity does not exist.
    NotFound,
    /// The request conflicts with current state.
    Conflict,
    /// The selected generation is stale.
    StaleGeneration,
    /// The requested capability is unavailable.
    UnsupportedCapability,
    /// The result lacks requested coverage.
    IncompleteCoverage,
    /// The request exceeded a work budget.
    BudgetExceeded,
    /// A bounded resource is exhausted.
    ResourceExhausted,
    /// The operation was cancelled.
    Cancelled,
    /// An isolated adapter failed.
    AdapterFailed,
    /// Stored index data is corrupt.
    IndexCorrupt,
    /// Stored data requires migration.
    MigrationRequired,
    /// Policy denied the request.
    PermissionDenied,
    /// Protocol negotiation failed.
    ProtocolMismatch,
    /// A resource is temporarily busy.
    Busy,
    /// A failure cannot be safely disclosed.
    Internal,
}

/// Closed source-free subsystem status used in operational evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// The subsystem is operating normally.
    Healthy,
    /// The subsystem is available with a known limitation.
    Degraded,
    /// The subsystem is temporarily unavailable.
    Unavailable,
    /// The subsystem does not exist in the current product slice.
    NotConfigured,
    /// The subsystem failed validation and needs repair.
    Failed,
}

/// Closed host resource-pressure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePressure {
    /// Resource use is within configured bounds.
    Normal,
    /// One or more bounded resources approach policy limits.
    Elevated,
    /// Resource pressure is sustained near a configured limit.
    High,
    /// Admission must be rejected to preserve host stability.
    Critical,
    /// No bounded sampler exists for the current slice.
    Unknown,
}

/// Source-free daemon health snapshot accepted by support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSnapshot {
    /// Whether the daemon is ready for its current contract.
    pub ready: bool,
    /// Closed daemon lifecycle state.
    pub lifecycle: DaemonLifecycle,
    /// Whether operation admission is open.
    pub accepting_operations: bool,
    /// Number of accepted connections currently in flight.
    pub active_connections: u32,
    /// Configured global connection limit.
    pub connection_limit: u32,
    /// Number of admitted operations.
    pub admitted_operations: u32,
    /// Number of operations awaiting workers.
    pub queued_operations: u32,
    /// Number of operations currently executing.
    pub running_operations: u32,
    /// Configured global operation admission limit.
    pub operation_queue_limit: u32,
    /// Cached catalog status.
    pub catalog_status: HealthStatus,
    /// Current catalog schema version.
    pub catalog_schema_version: u32,
    /// Current generation subsystem status.
    pub generation_status: HealthStatus,
    /// Current adapter subsystem status.
    pub adapter_status: HealthStatus,
    /// Current watcher subsystem status.
    pub watcher_status: HealthStatus,
    /// Current endpoint ownership status.
    pub endpoint_status: HealthStatus,
    /// Current endpoint/discovery schema version.
    pub endpoint_schema_version: u32,
    /// Current bounded host-pressure classification.
    pub resource_pressure: ResourcePressure,
}

/// Closed outcome for the catalog quick check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOutcome {
    /// The checked catalog passed validation.
    Passed,
    /// The checked catalog failed validation.
    Failed,
    /// The bounded check exceeded its deadline.
    TimedOut,
    /// The check could not be admitted or executed.
    Unavailable,
}

/// Source-free quick-diagnostic snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsQuickSnapshot {
    /// Diagnostics schema version.
    pub schema_version: u32,
    /// Aggregate status after the check.
    pub overall_status: HealthStatus,
    /// Catalog quick-check outcome.
    pub catalog_quick_check: DiagnosticOutcome,
    /// Monotonic elapsed time rounded to milliseconds.
    pub duration_ms: u32,
    /// Stable public error code, when the check did not pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

/// Aggregate operation counts safe for support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationsSummary {
    /// Operations durably queued.
    pub queued: u32,
    /// Operations durably running.
    pub running: u32,
    /// Operations completing cancellation cleanup.
    pub cancelling: u32,
}

/// Inputs accepted by the support-bundle privacy boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportBundleInput {
    /// Current private daemon protocol version.
    pub protocol_version: ProtocolVersion,
    /// Sanitized target operating-system family.
    pub operating_system: OperatingSystem,
    /// Sanitized target architecture.
    pub architecture: Architecture,
    /// Source-free health snapshot.
    pub health: HealthSnapshot,
    /// Latest bounded quick-diagnostic snapshot.
    pub diagnostics: DiagnosticsQuickSnapshot,
    /// Aggregate durable operation counts.
    pub operations: OperationsSummary,
}

/// Validated encoded support bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportBundle {
    archive: Vec<u8>,
    sha256: [u8; 32],
}

impl SupportBundle {
    /// Returns the deterministic ZIP archive bytes.
    #[must_use]
    pub fn archive(&self) -> &[u8] {
        &self.archive
    }

    /// Returns the SHA-256 digest of the complete ZIP archive.
    #[must_use]
    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// Returns the encoded archive length.
    #[must_use]
    pub fn archive_bytes(&self) -> u64 {
        u64::try_from(self.archive.len())
            .unwrap_or_else(|_| unreachable!("bounded support archive length fits u64"))
    }

    /// Reports whether this archive contains repository source.
    #[must_use]
    pub const fn contains_source(&self) -> bool {
        false
    }
}

/// Parsed support manifest used to validate transported archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportManifest {
    /// Support schema version.
    pub schema_version: u32,
    /// Daemon protocol version that emitted the archive.
    pub protocol_version: ProtocolVersion,
    /// Sanitized target operating-system family.
    pub operating_system: OperatingSystem,
    /// Sanitized target architecture.
    pub architecture: Architecture,
    /// Must remain false for this support schema.
    pub contains_source: bool,
    /// Hash and size records for every non-manifest entry.
    pub entries: Vec<SupportManifestEntry>,
}

/// One manifest record for an allow-listed support entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportManifestEntry {
    /// Allow-listed archive entry name.
    pub name: String,
    /// Uncompressed JSON byte length.
    pub bytes: u64,
    /// Lowercase SHA-256 digest of the JSON bytes.
    pub sha256: String,
}

/// Parsed redaction declaration used to validate transported archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionReport {
    /// Support schema version.
    pub schema_version: u32,
    /// Must remain false for this support schema.
    pub contains_source: bool,
    /// Exact set of sensitive data classes excluded by the builder.
    pub omitted_data_classes: Vec<String>,
}

struct SupportEntry {
    name: &'static str,
    bytes: Vec<u8>,
}

/// Builds one deterministic bounded source-free support archive.
///
/// # Errors
///
/// Returns [`SupportBundleError`] when serialization or ZIP encoding fails or
/// an entry/archive exceeds its reviewed limit.
pub fn build_support_bundle(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    let diagnostics = json_entry("diagnostics/quick.json", &input.diagnostics)?;
    let health = json_entry("health.json", &input.health)?;
    let operations = json_entry("operations-summary.json", &input.operations)?;
    let redaction = json_entry(
        "redaction-report.json",
        &RedactionReport {
            schema_version: SUPPORT_BUNDLE_SCHEMA_VERSION,
            contains_source: false,
            omitted_data_classes: OMITTED_DATA_CLASSES
                .into_iter()
                .map(str::to_owned)
                .collect(),
        },
    )?;
    let manifest = json_entry(
        "manifest.json",
        &SupportManifest {
            schema_version: SUPPORT_BUNDLE_SCHEMA_VERSION,
            protocol_version: input.protocol_version,
            operating_system: input.operating_system,
            architecture: input.architecture,
            contains_source: false,
            entries: [&diagnostics, &health, &operations, &redaction]
                .into_iter()
                .map(manifest_entry)
                .collect::<Result<_, _>>()?,
        },
    )?;
    let entries = [diagnostics, health, manifest, operations, redaction];
    debug_assert_eq!(entries.len(), SUPPORT_ENTRY_COUNT);
    let archive = encode_zip(&entries)?;
    if archive.len() > MAX_SUPPORT_ARCHIVE_BYTES {
        return Err(SupportBundleError::ArchiveTooLarge);
    }
    let sha256: [u8; 32] = Sha256::digest(&archive).into();
    Ok(SupportBundle { archive, sha256 })
}

fn json_entry(
    name: &'static str,
    value: &impl Serialize,
) -> Result<SupportEntry, SupportBundleError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(SupportBundleError::SerializeJson)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_SUPPORT_ENTRY_BYTES {
        return Err(SupportBundleError::EntryTooLarge { name });
    }
    Ok(SupportEntry { name, bytes })
}

fn manifest_entry(entry: &SupportEntry) -> Result<SupportManifestEntry, SupportBundleError> {
    Ok(SupportManifestEntry {
        name: entry.name.to_owned(),
        bytes: u64::try_from(entry.bytes.len())
            .map_err(|_| SupportBundleError::EntryTooLarge { name: entry.name })?,
        sha256: hex_digest(&entry.bytes),
    })
}

fn encode_zip(entries: &[SupportEntry]) -> Result<Vec<u8>, SupportBundleError> {
    let output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(output);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    for entry in entries {
        writer
            .start_file(entry.name, options)
            .map_err(SupportBundleError::Zip)?;
        writer
            .write_all(&entry.bytes)
            .map_err(SupportBundleError::WriteZip)?;
    }
    writer
        .finish()
        .map(Cursor::into_inner)
        .map_err(SupportBundleError::Zip)
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .unwrap_or_else(|_| unreachable!("formatting into String cannot fail"));
    }
    encoded
}

/// Support-bundle construction failure.
#[derive(Debug, thiserror::Error)]
pub enum SupportBundleError {
    /// One allow-listed entry exceeded its bounded JSON size.
    #[error("support bundle entry exceeds its size limit")]
    EntryTooLarge {
        /// Stable allow-listed entry name.
        name: &'static str,
    },
    /// The complete encoded archive exceeded its transport-safe limit.
    #[error("support bundle archive exceeds its size limit")]
    ArchiveTooLarge,
    /// Allow-listed JSON failed serialization.
    #[error("support bundle JSON serialization failed")]
    SerializeJson(#[source] serde_json::Error),
    /// ZIP metadata or entry creation failed.
    #[error("support bundle ZIP encoding failed")]
    Zip(#[source] zip::result::ZipError),
    /// Writing an allow-listed entry to the in-memory ZIP failed.
    #[error("support bundle ZIP write failed")]
    WriteZip(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn input() -> SupportBundleInput {
        SupportBundleInput {
            protocol_version: ProtocolVersion::V1_3,
            operating_system: OperatingSystem::Windows,
            architecture: Architecture::X86_64,
            health: HealthSnapshot {
                ready: true,
                lifecycle: DaemonLifecycle::Ready,
                accepting_operations: true,
                active_connections: 1,
                connection_limit: 128,
                admitted_operations: 2,
                queued_operations: 1,
                running_operations: 1,
                operation_queue_limit: 256,
                catalog_status: HealthStatus::Healthy,
                catalog_schema_version: 2,
                generation_status: HealthStatus::NotConfigured,
                adapter_status: HealthStatus::NotConfigured,
                watcher_status: HealthStatus::NotConfigured,
                endpoint_status: HealthStatus::Healthy,
                endpoint_schema_version: 2,
                resource_pressure: ResourcePressure::Unknown,
            },
            diagnostics: DiagnosticsQuickSnapshot {
                schema_version: 1,
                overall_status: HealthStatus::Healthy,
                catalog_quick_check: DiagnosticOutcome::Passed,
                duration_ms: 4,
                error_code: None,
            },
            operations: OperationsSummary {
                queued: 1,
                running: 1,
                cancelling: 0,
            },
        }
    }

    #[test]
    fn support_archive_is_deterministic_and_allow_listed() {
        let first = build_support_bundle(&input()).expect("support bundle builds");
        let second = build_support_bundle(&input()).expect("support bundle rebuilds");
        assert_eq!(first, second);
        assert!(!first.contains_source());
        assert!(first.archive().len() <= MAX_SUPPORT_ARCHIVE_BYTES);
        assert_eq!(
            <[u8; 32]>::from(Sha256::digest(first.archive())),
            first.sha256()
        );

        let cursor = Cursor::new(first.archive());
        let mut archive = zip::ZipArchive::new(cursor).expect("support ZIP opens");
        assert_eq!(archive.len(), SUPPORT_ENTRY_COUNT);
        let names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .expect("entry opens")
                    .name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "diagnostics/quick.json",
                "health.json",
                "manifest.json",
                "operations-summary.json",
                "redaction-report.json",
            ]
        );

        let mut manifest = String::new();
        archive
            .by_name("manifest.json")
            .expect("manifest opens")
            .read_to_string(&mut manifest)
            .expect("manifest reads");
        assert!(manifest.contains("\"contains_source\": false"));
        assert!(manifest.contains("diagnostics/quick.json"));
    }

    #[test]
    fn support_archive_never_accepts_arbitrary_sensitive_payloads() {
        let bundle = build_support_bundle(&input()).expect("support bundle builds");
        let forbidden = [
            b"PRIVATE_SOURCE_BODY".as_slice(),
            b"sk-secret-token".as_slice(),
            b"C:\\Users\\private\\repo".as_slice(),
            b"/home/private/repo".as_slice(),
            b"raw sqlite failure".as_slice(),
            b"prompt injection".as_slice(),
        ];
        for value in forbidden {
            assert!(
                !bundle
                    .archive()
                    .windows(value.len())
                    .any(|window| window == value)
            );
        }
    }
}
