//! Snapshot budgets track retained bytes and the current capture, not discarded files.
//! Public discovery modes must classify the same immutable original bytes alike.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_config::ConfigSnapshot;
use rootlight_discovery::{
    DiscoveryError, DiscoveryLimits, DiscoveryManifest, DiscoveryPolicy,
    DiscoveryTruncationResource, ExclusionReason, MAX_STREAMED_CLASSIFICATION_BYTES,
    StreamedDiscoveryOptions, StreamedDiscoverySource, StreamedSourceReceiver,
    discover_manifest_streaming, discover_with_snapshots_at_limit, discover_with_streamed_sources,
};
use rootlight_ids::{FileId, content_hash, derive_repository};
use rootlight_vfs::{
    RelativePath, RepositoryRoot, SnapshotMetadata, SourceSnapshot, StreamingSourceTextValidator,
};
use tempfile::{TempDir, tempdir_in};

const TEXT: &[u8] = include_bytes!("fixtures/snapshot-retention-original/htmlfunctions.ring");
const BINARY: &[u8] = include_bytes!("fixtures/snapshot-retention-original/cubicmap.png");

fn temporary() -> TempDir {
    tempdir_in(std::env::current_dir().unwrap()).unwrap()
}

fn root(temporary: &TempDir) -> RepositoryRoot {
    RepositoryRoot::open(
        derive_repository(b"discarded-binary-snapshot-budget").id(),
        temporary.path(),
    )
    .unwrap()
}

fn config() -> ConfigSnapshot {
    ConfigSnapshot::resolve(&[]).unwrap()
}

fn limits() -> DiscoveryLimits {
    DiscoveryLimits::new(128, 8, 64 * 1024, 16).unwrap()
}

fn policy() -> DiscoveryPolicy {
    DiscoveryPolicy::build(Vec::new(), true).unwrap()
}

fn interleaved_originals(temporary: &TempDir) {
    assert_eq!(TEXT.len(), 1778);
    assert_eq!(BINARY.len(), 74);
    assert!(32 * BINARY.len() > TEXT.len() + BINARY.len());
    assert_eq!(&BINARY[..8], b"\x89PNG\r\n\x1a\n");
    for index in 0..16 {
        for prefix in ["a", "c"] {
            fs::write(
                temporary.path().join(format!("{prefix}{index:02}.png")),
                BINARY,
            )
            .unwrap();
        }
    }
    fs::write(temporary.path().join("b.ring"), TEXT).unwrap();
}

fn assert_original_manifest(manifest: &DiscoveryManifest, root: &RepositoryRoot) {
    assert!(manifest.coverage.complete);
    assert_eq!(manifest.inputs.len(), 1);
    assert_eq!(manifest.coverage.included, 1);
    assert_eq!(manifest.coverage.excluded.get("binary"), Some(&32));
    assert_eq!(manifest.exclusions.len(), 32);
    assert!(
        manifest
            .exclusions
            .iter()
            .all(|entry| entry.reason == ExclusionReason::Binary)
    );
    assert!(manifest.diagnostics.is_empty());
    let input = &manifest.inputs[0];
    assert_eq!(input.path, "b.ring");
    assert_eq!(
        input.file,
        root.file_id(&RelativePath::parse(std::path::Path::new("b.ring")).unwrap())
    );
    assert_eq!(input.bytes, u64::try_from(TEXT.len()).unwrap());
    assert_eq!(input.content_hash, content_hash(TEXT));
    assert_eq!(input.encoding, "utf-8");
    assert!(
        input
            .language_signals
            .iter()
            .any(|signal| signal.language == "ring")
    );
}

fn assert_snapshot_bytes(
    snapshots: &BTreeMap<FileId, SourceSnapshot>,
    manifest: &DiscoveryManifest,
) {
    assert_eq!(snapshots.len(), 1);
    let snapshot = snapshots.get(&manifest.inputs[0].file).unwrap();
    assert_eq!(snapshot.content(), TEXT);
    assert_eq!(snapshot.content_hash(), content_hash(TEXT));
    assert_eq!(
        snapshot.metadata().length,
        u64::try_from(TEXT.len()).unwrap()
    );
}

#[test]
fn discarded_binary_captures_do_not_accumulate_retained_snapshot_bytes() {
    let temporary = temporary();
    interleaved_originals(&temporary);
    let root = root(&temporary);
    let config = config();
    let policy = policy();
    let cancellation = Cancellation::new();
    let manifest =
        discover_manifest_streaming(&root, &config, &policy, limits(), &cancellation).unwrap();
    assert_original_manifest(&manifest, &root);
    // The extra one-file allowance still admits the largest transient binary
    // after the retained text; cumulative discarded bytes exceed the limit.
    let maximum = u64::try_from(TEXT.len() + BINARY.len()).unwrap();
    let (retained, snapshots) = discover_with_snapshots_at_limit(
        &root,
        &config,
        &policy,
        limits(),
        BTreeMap::new(),
        maximum,
        &cancellation,
    )
    .unwrap()
    .into_parts();
    assert_original_manifest(&retained, &root);
    assert_eq!(retained, manifest);
    assert_eq!(retained.hash().unwrap(), manifest.hash().unwrap());
    assert_snapshot_bytes(&snapshots, &retained);
    for entry in fs::read_dir(temporary.path()).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            fs::read(entry.path()).unwrap(),
            if entry.path().extension().unwrap() == "ring" {
                TEXT
            } else {
                BINARY
            }
        );
    }
}

#[derive(Default)]
struct Receiver {
    captured: Vec<(FileId, Vec<u8>)>,
}

struct Sink<'a> {
    bytes: Vec<u8>,
    validator: StreamingSourceTextValidator,
    cancellation: &'a Cancellation,
}

impl Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.validator
            .push(bytes, self.cancellation)
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StreamedSourceReceiver for Receiver {
    fn capture(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        cancellation: &Cancellation,
    ) -> Result<StreamedDiscoverySource, DiscoveryError> {
        let mut sink = Sink {
            bytes: Vec::new(),
            validator: StreamingSourceTextValidator::new(),
            cancellation,
        };
        let capture = root
            .capture_streamed_cancellable(path, observed.length.max(1), cancellation, &mut sink)
            .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?;
        assert_eq!(capture.metadata(), observed);
        let contains_nul = sink.validator.contains_nul();
        let encoding = sink.validator.finish();
        let prefix = sink.bytes[..sink.bytes.len().min(MAX_STREAMED_CLASSIFICATION_BYTES)].to_vec();
        self.captured.push((capture.file(), sink.bytes));
        Ok(StreamedDiscoverySource {
            capture,
            encoding,
            contains_nul,
            prefix,
        })
    }
}

#[test]
fn streamed_and_retained_modes_keep_the_same_binary_exclusion_and_source_identity() {
    let temporary = temporary();
    interleaved_originals(&temporary);
    let root = root(&temporary);
    let config = config();
    let policy = policy();
    let cancellation = Cancellation::new();
    let manifest =
        discover_manifest_streaming(&root, &config, &policy, limits(), &cancellation).unwrap();
    let mut receiver = Receiver::default();
    let options = StreamedDiscoveryOptions::manifest(64 * 1024, 1024, &mut receiver)
        .unwrap()
        .with_snapshots(BTreeMap::new(), u64::try_from(BINARY.len()).unwrap());
    let (streamed, snapshots) =
        discover_with_streamed_sources(&root, &config, &policy, limits(), options, &cancellation)
            .unwrap()
            .into_parts();
    assert_original_manifest(&streamed, &root);
    assert_eq!(streamed, manifest);
    assert!(snapshots.is_empty());
    assert_eq!(
        receiver.captured,
        [(manifest.inputs[0].file, TEXT.to_vec())]
    );
}

#[test]
fn retained_text_over_budget_still_returns_a_bounded_incomplete_prefix() {
    let temporary = temporary();
    fs::write(temporary.path().join("a.ring"), TEXT).unwrap();
    fs::write(temporary.path().join("b.ring"), TEXT).unwrap();
    let root = root(&temporary);
    let maximum = u64::try_from(TEXT.len() + BINARY.len()).unwrap();
    let (manifest, snapshots) = discover_with_snapshots_at_limit(
        &root,
        &config(),
        &policy(),
        limits(),
        BTreeMap::new(),
        maximum,
        &Cancellation::new(),
    )
    .unwrap()
    .into_parts();
    assert!(!manifest.coverage.complete);
    assert_eq!(manifest.inputs.len(), 1);
    assert_eq!(manifest.inputs[0].path, "a.ring");
    assert_snapshot_bytes(&snapshots, &manifest);
    let truncation = manifest.coverage.truncation.unwrap();
    assert_eq!(
        truncation.resource,
        DiscoveryTruncationResource::RetainedSourceBytes
    );
    assert_eq!(truncation.limit, maximum);
    assert_eq!(truncation.observed, u64::try_from(2 * TEXT.len()).unwrap());
}

#[test]
fn incoming_binary_capture_must_still_fit_the_transient_snapshot_budget() {
    let temporary = temporary();
    fs::write(temporary.path().join("a.ring"), TEXT).unwrap();
    fs::write(temporary.path().join("b.png"), BINARY).unwrap();
    let root = root(&temporary);
    let maximum = u64::try_from(TEXT.len()).unwrap();
    let (manifest, snapshots) = discover_with_snapshots_at_limit(
        &root,
        &config(),
        &policy(),
        limits(),
        BTreeMap::new(),
        maximum,
        &Cancellation::new(),
    )
    .unwrap()
    .into_parts();
    assert!(!manifest.coverage.complete);
    assert_eq!(manifest.inputs.len(), 1);
    assert_snapshot_bytes(&snapshots, &manifest);
    let truncation = manifest.coverage.truncation.unwrap();
    assert_eq!(
        truncation.resource,
        DiscoveryTruncationResource::RetainedSourceBytes
    );
    assert_eq!(truncation.limit, maximum);
    assert_eq!(
        truncation.observed,
        u64::try_from(TEXT.len() + BINARY.len()).unwrap()
    );
    assert!(
        !manifest
            .exclusions
            .iter()
            .any(|entry| entry.path == "b.png")
    );
}

#[test]
fn discarded_binary_fix_keeps_explicit_cancellation_and_physical_file_limits() {
    let temporary = temporary();
    fs::write(temporary.path().join("a.png"), BINARY).unwrap();
    fs::write(temporary.path().join("b.ring"), TEXT).unwrap();
    let root = root(&temporary);
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy(),
            limits(),
            BTreeMap::new(),
            4096,
            &cancellation,
        ),
        Err(DiscoveryError::Cancelled(_))
    ));
    let (manifest, snapshots) = discover_with_snapshots_at_limit(
        &root,
        &config(),
        &policy(),
        DiscoveryLimits::new(128, 8, 128, 16).unwrap(),
        BTreeMap::new(),
        4096,
        &Cancellation::new(),
    )
    .unwrap()
    .into_parts();
    assert!(manifest.coverage.complete);
    assert!(manifest.inputs.is_empty());
    assert!(snapshots.is_empty());
    assert_eq!(manifest.coverage.excluded.get("binary"), Some(&1));
    assert_eq!(manifest.coverage.excluded.get("oversized"), Some(&1));
}
