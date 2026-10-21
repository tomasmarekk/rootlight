//! Admission distinguishes opaque encoded source from validated text and binary.

use std::{
    collections::BTreeMap,
    io::{self, Write},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};
use rootlight_ids::{content_hash, derive_repository};
use rootlight_vfs::{RelativePath, RepositoryRoot, SnapshotMetadata, StreamingSourceTextValidator};

use crate::*;

fn config() -> ConfigSnapshot {
    ConfigSnapshot::resolve(&[ConfigLayer {
        source: ConfigSource::User,
        contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'configured.opaque'='unimplemented-source'\n",
    }]).unwrap()
}

fn cases() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "mismatched.toml",
            [b"\xff\xfe\0\0".as_slice(), b"k\0e\0y\0=\0'\0v\0'\0\n\0"].concat(),
        ),
        (
            "big.toml",
            b"\0\0\xfe\xff\0\0\0k\0\0\0=\0\0\0'\0\0\0v\0\0\0'".to_vec(),
        ),
        ("broken.rs", b"\xff\xfe\0\xd8\0\0".to_vec()),
        ("broken-be.rs", b"\xfe\xff\xdc\0\0\0".to_vec()),
        (
            "opaque.rs",
            b"\xef\xbb\xbf// generated\nfunction forged() {}\0\xff".to_vec(),
        ),
        ("ambiguous.mo", b"\xff\xfe\0\xd8\0\0".to_vec()),
        ("classic.BAS", b"\xff\xfe\0\xd8\0\0".to_vec()),
        ("class.Cls", b"\xff\xfe\0\xd8\0\0".to_vec()),
        ("form.FRM", b"\xff\xfe\0\xd8\0\0".to_vec()),
        ("shared.PL", b"\xff\xfe\0\xd8\0\0".to_vec()),
        ("configured.opaque", b"\xef\xbb\xbfopaque\0\xff".to_vec()),
    ]
}

fn limits() -> DiscoveryLimits {
    DiscoveryLimits::new(64, 8, 8192, 32).unwrap()
}

fn root(temporary: &tempfile::TempDir) -> RepositoryRoot {
    RepositoryRoot::open(
        derive_repository(b"opaque-unicode-source-admission").id(),
        temporary.path(),
    )
    .unwrap()
}

fn write_cases(temporary: &tempfile::TempDir) {
    for (path, bytes) in cases() {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
}

#[test]
fn known_source_boms_preserve_exact_bytes_without_decoded_language_evidence() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write_cases(&temporary);
    let root = root(&temporary);
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let result = discover_with_snapshots(
        &root,
        &config(),
        &policy,
        limits(),
        BTreeMap::new(),
        &Cancellation::new(),
    )
    .unwrap();
    let (manifest, snapshots) = result.into_parts();
    assert_eq!(manifest.inputs.len(), cases().len());
    assert!(manifest.exclusions.is_empty());
    for (path, bytes) in cases() {
        let input = manifest
            .inputs
            .iter()
            .find(|input| input.path == path)
            .unwrap();
        let relative = RelativePath::parse(std::path::Path::new(path)).unwrap();
        assert_eq!(input.file, root.file_id(&relative));
        assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(input.content_hash, content_hash(&bytes));
        assert_eq!(input.encoding, "bytes");
        assert_eq!(input.class, InputClass::Source);
        assert_eq!(snapshots[&input.file].content(), bytes);
        assert!(input.language_signals.iter().all(|signal| !matches!(
            signal.evidence,
            LanguageEvidence::Content | LanguageEvidence::Shebang
        )));
        if path == "ambiguous.mo" {
            assert!(
                input.language_signals.is_empty(),
                "a BOM must not assert Modelica syntax"
            );
        }
        if path == "configured.opaque" {
            assert_eq!(
                input.language_signals,
                vec![LanguageSignal {
                    language: "unimplemented-source".to_owned(),
                    evidence: LanguageEvidence::Configuration
                }]
            );
        }
    }
}

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

#[test]
fn streamed_bom_admission_matches_snapshot_identity_and_never_materializes_text() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write_cases(&temporary);
    let root = root(&temporary);
    let config = config();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let cancellation = Cancellation::new();
    let expected = discover(&root, &config, &policy, limits(), &cancellation).unwrap();
    let result = discover_with_streamed_sources(
        &root,
        &config,
        &policy,
        limits(),
        StreamedDiscoveryOptions::manifest(8192, 1, &mut Receiver).unwrap(),
        &cancellation,
    )
    .unwrap();
    let (actual, snapshots) = result.into_parts();
    assert_eq!(actual, expected);
    assert_eq!(actual.inputs.len(), cases().len());
    assert!(snapshots.is_empty());
    assert!(actual.inputs.iter().all(|input| input.encoding == "bytes"));
}

#[test]
fn binary_unknown_nul_and_container_signatures_do_not_gain_source_admission() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let excluded = [
        ("unknown.data", b"\xff\xfe\0\xd8\0\0".as_slice()),
        ("embedded-nul.toml", b"key=1\0"),
        ("pdf.toml", b"%PDF-1.7\nopaque\0"),
        ("png.rs", b"\x89PNG\r\n\x1a\n\0"),
        ("zip.rs", b"PK\x03\x04\0"),
        ("wrapped.rs", b"\xff\xfePK\x03\x04\0"),
    ];
    for (path, bytes) in excluded {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
    let root = root(&temporary);
    let config = config();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let cancellation = Cancellation::new();
    let expected = discover(&root, &config, &policy, limits(), &cancellation).unwrap();
    assert!(expected.inputs.is_empty());
    assert_eq!(expected.exclusions.len(), excluded.len());
    assert!(
        expected
            .exclusions
            .iter()
            .all(|entry| entry.reason == ExclusionReason::Binary),
        "actual exclusions: {:#?}",
        expected.exclusions
    );
    let actual = discover_with_streamed_sources(
        &root,
        &config,
        &policy,
        limits(),
        StreamedDiscoveryOptions::manifest(8192, 1, &mut Receiver).unwrap(),
        &cancellation,
    )
    .unwrap();
    assert_eq!(actual.into_parts().0, expected);
}

#[test]
fn opaque_admission_keeps_policy_size_aggregate_and_cancellation_guards() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bytes = b"\xff\xfe\0\xd8\0\0";
    for path in ["a.toml", "b.toml", "ignored.toml"] {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
    let root = root(&temporary);
    let config = config();
    let policy = DiscoveryPolicy::build(
        vec![PolicyRule {
            layer: PolicyLayer::Operation,
            pattern: "ignored.toml".to_owned(),
            source: "operation".to_owned(),
        }],
        true,
    )
    .unwrap();
    let cancellation = Cancellation::new();
    let at_limit = discover_with_snapshots_at_limit(
        &root,
        &config,
        &policy,
        limits(),
        BTreeMap::new(),
        12,
        &cancellation,
    )
    .unwrap();
    assert_eq!(at_limit.manifest.inputs.len(), 2);
    assert!(at_limit.manifest.coverage.complete);
    let bounded = discover_with_snapshots_at_limit(
        &root,
        &config,
        &policy,
        limits(),
        BTreeMap::new(),
        11,
        &cancellation,
    )
    .unwrap();
    assert!(!bounded.manifest.coverage.complete);
    assert!(
        bounded
            .manifest
            .inputs
            .iter()
            .map(|input| input.bytes)
            .sum::<u64>()
            <= 11
    );
    assert!(
        at_limit
            .manifest
            .exclusions
            .iter()
            .any(|entry| entry.path == "ignored.toml")
    );
    let small = discover(
        &root,
        &config,
        &policy,
        DiscoveryLimits::new(64, 8, 5, 32).unwrap(),
        &cancellation,
    )
    .unwrap();
    assert!(small.inputs.is_empty());
    assert_eq!(small.coverage.excluded.get("oversized"), Some(&2));
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        discover(&root, &config, &policy, limits(), &cancellation),
        Err(DiscoveryError::Cancelled(_))
    ));
}
