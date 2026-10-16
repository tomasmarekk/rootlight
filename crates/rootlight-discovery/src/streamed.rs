//! Discovery over immutable streamed captures that exceed snapshot memory limits.
//!
//! The receiver owns provisional private bytes; only authenticated metadata and
//! a bounded classification prefix enter the manifest.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_config::ConfigSnapshot;
use rootlight_ids::FileId;
use rootlight_vfs::{
    RelativePath, RepositoryRoot, SnapshotMetadata, SourceSnapshot, StreamedSourceCapture,
    TextEncoding,
};

use crate::{
    DiscoveryError, DiscoveryLimits, DiscoveryPolicy, DiscoveryResult, DiscoveryState,
    IncrementalDiscovery, MAX_RETAINED_SOURCE_BYTES, ManifestReconcileInputs,
    SnapshotRetentionOptions, incremental,
};

/// Authenticated physical capture and bounded classification evidence.
#[derive(Debug)]
pub struct StreamedDiscoverySource {
    /// Complete physical identity obtained from the repository capability.
    pub capture: StreamedSourceCapture,
    /// Strict whole-file text encoding, or raw bytes without a text claim.
    pub encoding: Option<TextEncoding>,
    /// Whether the strict decoder observed a NUL scalar.
    pub contains_nul: bool,
    /// Physical prefix, bounded to [`MAX_STREAMED_CLASSIFICATION_BYTES`].
    pub prefix: Vec<u8>,
}

/// Maximum resident physical prefix retained by a streamed discovery receiver.
pub const MAX_STREAMED_CLASSIFICATION_BYTES: usize = 64 * 1024;

/// Private capture owner used when one source cannot become a snapshot.
pub trait StreamedSourceReceiver {
    /// Captures and hashes the complete source through `root` without retaining it in memory.
    ///
    /// The receiver must validate the entire text before returning an encoding,
    /// keep provisional bytes private, and compare the capture to `observed`.
    ///
    /// # Errors
    /// Returns cancellation, capture, storage admission, or source drift errors.
    fn capture(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        cancellation: &Cancellation,
    ) -> Result<StreamedDiscoverySource, DiscoveryError>;
}

/// Separate physical availability admission from resident snapshot admission.
pub struct StreamedDiscoveryOptions<'a> {
    maximum_file_bytes: u64,
    snapshot_file_bytes: u64,
    receiver: &'a mut dyn StreamedSourceReceiver,
    snapshots: SnapshotRetentionOptions,
}

impl<'a> StreamedDiscoveryOptions<'a> {
    /// Retains metadata and receiver-owned streamed captures, without small snapshots.
    ///
    /// # Errors
    /// Rejects a zero availability limit or a snapshot limit above the VFS hard cap.
    pub fn manifest(
        maximum_file_bytes: u64,
        snapshot_file_bytes: u64,
        receiver: &'a mut dyn StreamedSourceReceiver,
    ) -> Result<Self, DiscoveryError> {
        if maximum_file_bytes == 0
            || snapshot_file_bytes == 0
            || snapshot_file_bytes > rootlight_vfs::MAX_SNAPSHOT_BYTES
        {
            return Err(DiscoveryError::InvalidLimits);
        }
        Ok(Self {
            maximum_file_bytes,
            snapshot_file_bytes: snapshot_file_bytes.min(maximum_file_bytes),
            receiver,
            snapshots: SnapshotRetentionOptions::manifest_only(),
        })
    }

    /// Also retains small snapshots within the existing aggregate memory ceiling.
    #[must_use]
    pub fn with_snapshots(
        mut self,
        cached: BTreeMap<FileId, SourceSnapshot>,
        maximum_retained_bytes: u64,
    ) -> Self {
        self.snapshots = SnapshotRetentionOptions::retained(
            cached,
            maximum_retained_bytes.min(MAX_RETAINED_SOURCE_BYTES),
        );
        self
    }
}

/// Discovers sources with separate bounded-memory and physical-storage admission.
///
/// # Errors
/// Propagates normal discovery failures and receiver authentication/admission failures.
pub fn discover_with_streamed_sources<'a>(
    root: &'a RepositoryRoot,
    config: &'a ConfigSnapshot,
    policy: &'a DiscoveryPolicy,
    limits: DiscoveryLimits,
    options: StreamedDiscoveryOptions<'a>,
    cancellation: &'a Cancellation,
) -> Result<DiscoveryResult, DiscoveryError> {
    let mut state = state(root, config, policy, limits, options, cancellation)?;
    state.run()?;
    state.finish()
}

/// Reconciles the same complete streamed observation that produced the manifest.
///
/// # Errors
/// Propagates discovery, private capture, incremental reconciliation, or drift failures.
pub fn discover_streamed_and_reconcile<'a>(
    root: &'a RepositoryRoot,
    config: &'a ConfigSnapshot,
    reconcile: ManifestReconcileInputs<'_>,
    policy: &'a DiscoveryPolicy,
    limits: DiscoveryLimits,
    options: StreamedDiscoveryOptions<'a>,
    cancellation: &'a Cancellation,
) -> Result<(DiscoveryResult, IncrementalDiscovery), DiscoveryError> {
    let mut state = state(root, config, policy, limits, options, cancellation)?;
    state.run()?;
    let (result, scanned) = state.finish_with_scan()?;
    let empty = BTreeMap::new();
    let incremental = incremental::reconcile_manifest_scan(
        incremental::ManifestScanReconcile {
            repository: root.repository(),
            observed_complete: result.manifest.coverage.complete,
            scanned,
            reconcile,
            observed_hashed_files: None,
            observed_hashed_snapshots: &empty,
        },
        &result.manifest,
        limits,
        cancellation,
    )?;
    Ok((result, incremental))
}

fn state<'a>(
    root: &'a RepositoryRoot,
    config: &'a ConfigSnapshot,
    policy: &'a DiscoveryPolicy,
    mut limits: DiscoveryLimits,
    options: StreamedDiscoveryOptions<'a>,
    cancellation: &'a Cancellation,
) -> Result<DiscoveryState<'a>, DiscoveryError> {
    limits.max_file_bytes = options.maximum_file_bytes;
    let mut state = DiscoveryState::new(
        root,
        config,
        policy,
        limits,
        options.snapshots,
        None,
        cancellation,
    )?;
    state.streamed_receiver = Some(options.receiver);
    state.snapshot_file_bytes = options.snapshot_file_bytes;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use rootlight_config::{ConfigLayer, ConfigSource};
    use rootlight_ids::{content_hash, derive_repository};
    use rootlight_vfs::StreamingSourceTextValidator;

    use super::*;

    struct Receiver;

    struct Sink<'a> {
        prefix: Vec<u8>,
        validator: StreamingSourceTextValidator,
        cancellation: &'a Cancellation,
    }

    impl Write for Sink<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.validator
                .push(bytes, self.cancellation)
                .map_err(io::Error::other)?;
            let count = bytes
                .len()
                .min(MAX_STREAMED_CLASSIFICATION_BYTES - self.prefix.len());
            self.prefix.extend_from_slice(&bytes[..count]);
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
                prefix: Vec::new(),
                validator: StreamingSourceTextValidator::new(),
                cancellation,
            };
            let capture = root
                .capture_streamed_cancellable(path, observed.length.max(1), cancellation, &mut sink)
                .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?;
            let contains_nul = sink.validator.contains_nul();
            Ok(StreamedDiscoverySource {
                capture,
                encoding: sink.validator.finish(),
                contains_nul,
                prefix: sink.prefix,
            })
        }
    }

    fn config() -> ConfigSnapshot {
        ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::Defaults,
            contents: "version = \"1.0\"",
        }])
        .unwrap()
    }

    #[test]
    fn streamed_discovery_keeps_exact_encodings_and_raw_unknown_bytes_without_snapshots() {
        let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let text = format!("// {}\nfn visible() {{}}\n", "λ😀".repeat(50));
        let mut cases = vec![("utf8.rs", text.as_bytes().to_vec(), "utf-8")];
        for (name, little, label) in [("le.rs", true, "utf-16le"), ("be.rs", false, "utf-16be")] {
            let bytes = std::iter::once(0xfeff)
                .chain(text.encode_utf16())
                .flat_map(|unit| {
                    if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect();
            cases.push((name, bytes, label));
        }
        let mut raw = vec![b'a'; 300];
        raw.push(0xff);
        cases.push(("raw.rs", raw, "bytes"));
        for (name, bytes, _) in &cases {
            std::fs::write(temporary.path().join(name), bytes).unwrap()
        }
        std::fs::write(temporary.path().join("small.rs"), b"fn small() {}\n").unwrap();
        let root = RepositoryRoot::open(
            derive_repository(b"streamed-discovery-encodings").id(),
            temporary.path(),
        )
        .unwrap();
        let limits = DiscoveryLimits::new(100, 10, 128, 20).unwrap();
        let config = config();
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let cancellation = Cancellation::new();
        let legacy = crate::discover(&root, &config, &policy, limits, &cancellation).unwrap();
        assert_eq!(legacy.coverage.excluded.get("oversized"), Some(&4));
        let result = discover_with_streamed_sources(
            &root,
            &config,
            &policy,
            limits,
            StreamedDiscoveryOptions::manifest(u64::MAX, 128, &mut Receiver)
                .unwrap()
                .with_snapshots(BTreeMap::new(), 128),
            &cancellation,
        )
        .unwrap();
        let (manifest, snapshots) = result.into_parts();
        assert_eq!(manifest.inputs.len(), 5);
        assert_eq!(snapshots.len(), 1);
        assert!(manifest.coverage.complete);
        assert!(!manifest.coverage.excluded.contains_key("oversized"));
        for (name, bytes, encoding) in cases {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == name)
                .unwrap();
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
            assert_eq!(input.content_hash, content_hash(&bytes));
            assert_eq!(input.encoding, encoding);
            assert!(
                input
                    .language_signals
                    .iter()
                    .any(|signal| signal.language == "rust")
            );
            assert!(!snapshots.contains_key(&input.file));
        }
    }

    #[test]
    fn above_snapshot_cap_is_streamed_and_incremental_hashing_never_allocates_a_snapshot() {
        let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let path = temporary.path().join("large.rs");
        let length = rootlight_vfs::MAX_SNAPSHOT_BYTES + 1;
        let mut file = std::fs::File::create(&path).unwrap();
        let block = [b'a'; 64 * 1024];
        for _ in 0..rootlight_vfs::MAX_SNAPSHOT_BYTES / u64::try_from(block.len()).unwrap() {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"z").unwrap();
        drop(file);
        let root = RepositoryRoot::open(
            derive_repository(b"streamed-discovery-above-cap").id(),
            temporary.path(),
        )
        .unwrap();
        let config = config();
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let limits = DiscoveryLimits::new(100, 10, rootlight_vfs::MAX_SNAPSHOT_BYTES, 20).unwrap();
        let cancellation = Cancellation::new();
        let context = crate::IncrementalDiscoveryContext::new(
            config.hash(),
            rootlight_ids::derive_fact("streamed-test", b"provider").id(),
            content_hash(b"revision"),
        );
        let (result, first) = discover_streamed_and_reconcile(
            &root,
            &config,
            ManifestReconcileInputs::new(None, context),
            &policy,
            limits,
            StreamedDiscoveryOptions::manifest(
                u64::MAX,
                rootlight_vfs::MAX_SNAPSHOT_BYTES,
                &mut Receiver,
            )
            .unwrap(),
            &cancellation,
        )
        .unwrap();
        let (manifest, snapshots) = result.into_parts();
        assert!(snapshots.is_empty());
        assert_eq!(manifest.inputs.len(), 1);
        assert_eq!(manifest.inputs[0].bytes, length);
        let mut availability_limits = limits;
        availability_limits.max_file_bytes = u64::MAX;
        let refreshed = crate::discover_incremental_with_progress(
            &root,
            Some(first.baseline()),
            context,
            &policy,
            crate::IncrementalDiscoveryOptions::new(
                rootlight_incremental::ReconcileMode::Audit,
                availability_limits,
            )
            .without_hashed_snapshot_retention(),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert_eq!(refreshed.baseline(), first.baseline());
        assert_eq!(refreshed.hashed_files().len(), 1);
    }
}
