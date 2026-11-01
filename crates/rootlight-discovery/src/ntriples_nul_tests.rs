//! Buffered and streamed native N-Triples admission must use the same byte proof.
//! Negative routes and forged capture evidence retain ordinary binary safeguards.

use std::io::{self, Write};

use rootlight_cancel::Cancellation;
use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};
use rootlight_ids::derive_repository;
use rootlight_vfs::{
    RelativePath, RepositoryRoot, SnapshotMetadata, StreamingSourceTextValidator, TextEncoding,
};

use crate::*;

struct Receiver {
    stale: bool,
}

struct Sink(Vec<u8>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
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
        self.capture_with_native_nul(root, path, observed, false, cancellation)
    }

    fn capture_with_native_nul(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        requested: bool,
        cancellation: &Cancellation,
    ) -> Result<StreamedDiscoverySource, DiscoveryError> {
        let mut sink = Sink(Vec::new());
        let capture = root
            .capture_streamed_cancellable(path, observed.length.max(1), cancellation, &mut sink)
            .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?;
        let mut validator = StreamingSourceTextValidator::new();
        validator.push(&sink.0, cancellation)?;
        let contains_nul = sink.0.contains(&0);
        let encoding = validator.finish();
        let ntriples_nul_proof =
            if requested && contains_nul && encoding == Some(TextEncoding::Utf8) {
                let mut proof_bytes = sink.0.clone();
                if self.stale {
                    let offset = proof_bytes.iter().position(|byte| *byte == b'&').unwrap();
                    proof_bytes[offset] = b'+';
                }
                let mut probe = NTriplesNulProbe::new();
                for chunk in proof_bytes.chunks(7) {
                    probe.push(chunk, cancellation)?;
                }
                Some(probe.finish(cancellation)?)
            } else {
                None
            };
        Ok(StreamedDiscoverySource {
            capture,
            encoding,
            contains_nul,
            ntriples_nul_proof,
            prefix: sink.0[..sink.0.len().min(MAX_STREAMED_CLASSIFICATION_BYTES)].to_vec(),
        })
    }
}

const ORIGINAL: &[u8] =
    b"<http://a.example/s> <http://a.example/p> \"\0\t\x0b\x0c\x0e&([]\x7f\" .\n";

fn config() -> ConfigSnapshot {
    ConfigSnapshot::resolve(&[ConfigLayer { source: ConfigSource::User, contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'configured.data'='ntriples'\n'overridden.nt'='rust'\n" }]).unwrap()
}

#[test]
fn native_nul_snapshot_and_stream_preserve_routes_bytes_and_binary_controls() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    assert_eq!(ORIGINAL.len(), 57);
    let mut late = b"# padding\n".repeat(10_000);
    late.extend_from_slice(ORIGINAL);
    let mut late_binary = vec![b'a'; MAX_CLASSIFICATION_BYTES + 1];
    late_binary.push(0);
    let mut malformed = ORIGINAL.to_vec();
    malformed.push(0xff);
    let mut utf16_nul = vec![0xff, 0xfe];
    utf16_nul.extend("before\0after".encode_utf16().flat_map(u16::to_le_bytes));
    let cases = [
        ("original.nt", ORIGINAL.to_vec()),
        ("late.nt", late),
        ("configured.data", ORIGINAL.to_vec()),
        ("overridden.nt", ORIGINAL.to_vec()),
        ("comment.nt", b"# \0 comment\n".to_vec()),
        ("outside.nt", b"<s:>\0<p:><o:>.\n".to_vec()),
        ("blob.bin", late_binary),
        ("misleading.rs", ORIGINAL.to_vec()),
        ("pdf.nt", b"%PDF-1.7\n# \0\n".to_vec()),
        ("zip.nt", b"PK\x03\x04\n# \0\n".to_vec()),
        ("png.nt", b"\x89PNG\r\n\x1a\n# \0\n".to_vec()),
        ("malformed.nt", malformed),
        ("no-bom-utf16.nt", b"a\0b\0".to_vec()),
        ("utf16-nul.nt", utf16_nul),
        ("utf32.nt", vec![0xff, 0xfe, 0, 0, 0x61, 0, 0, 0]),
    ];
    for (path, bytes) in &cases {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
    let root = RepositoryRoot::open(
        derive_repository(b"native-nul-parity").id(),
        temporary.path(),
    )
    .unwrap();
    let config = config();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let limits = DiscoveryLimits::new(100, 10, 2 * 1024 * 1024, 100).unwrap();
    let cancel = Cancellation::new();
    let buffered = discover(&root, &config, &policy, limits, &cancel).unwrap();
    let streamed = discover_with_streamed_sources(
        &root,
        &config,
        &policy,
        limits,
        StreamedDiscoveryOptions::manifest(2 * 1024 * 1024, 1, &mut Receiver { stale: false })
            .unwrap(),
        &cancel,
    )
    .unwrap()
    .into_parts()
    .0;
    assert_eq!(buffered.inputs, streamed.inputs);
    assert_eq!(buffered.exclusions, streamed.exclusions);
    for path in ["original.nt", "late.nt", "configured.data", "comment.nt"] {
        let input = buffered
            .inputs
            .iter()
            .find(|input| input.path == path)
            .unwrap();
        let bytes = &cases.iter().find(|(name, _)| *name == path).unwrap().1;
        assert_eq!(input.encoding, "utf-8");
        assert_eq!(input.content_hash, content_hash(bytes));
        assert!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == "ntriples")
        );
    }
    for path in [
        "overridden.nt",
        "outside.nt",
        "blob.bin",
        "misleading.rs",
        "pdf.nt",
        "zip.nt",
        "png.nt",
        "malformed.nt",
        "no-bom-utf16.nt",
    ] {
        assert!(
            buffered
                .exclusions
                .iter()
                .any(|entry| entry.path == path && entry.reason == ExclusionReason::Binary),
            "{path}"
        );
    }
    for path in ["utf16-nul.nt", "utf32.nt"] {
        assert_eq!(
            buffered
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap()
                .encoding,
            "bytes"
        );
    }
}

#[test]
fn native_nul_stream_rejects_valid_but_stale_source_proof() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(temporary.path().join("original.nt"), ORIGINAL).unwrap();
    let root = RepositoryRoot::open(
        derive_repository(b"native-nul-stale").id(),
        temporary.path(),
    )
    .unwrap();
    let config = config();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let cancel = Cancellation::new();
    assert!(matches!(
        discover_with_streamed_sources(
            &root,
            &config,
            &policy,
            DiscoveryLimits::new(10, 10, 1024, 10).unwrap(),
            StreamedDiscoveryOptions::manifest(1024, 1, &mut Receiver { stale: true }).unwrap(),
            &cancel
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));
}
