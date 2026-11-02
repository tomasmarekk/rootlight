//! Contextual native NUL admission must agree for snapshots and full streamed bytes.
//! Original control-character witnesses remain separate from malformed and foreign routes.

use std::io::{self, Write};

use rootlight_config::{ConfigLayer, ConfigSource};
use rootlight_ids::derive_repository;
use rootlight_vfs::{SnapshotMetadata, StreamingSourceTextValidator};

use crate::*;

#[derive(Default)]
struct Receiver {
    stale: bool,
    grammar: Option<NativeNulGrammar>,
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

fn capture_bytes(
    root: &RepositoryRoot,
    path: &RelativePath,
    observed: SnapshotMetadata,
    cancellation: &Cancellation,
) -> Result<(StreamedDiscoverySource, Vec<u8>), DiscoveryError> {
    let mut sink = Sink(Vec::new());
    let capture = root
        .capture_streamed_cancellable(path, observed.length.max(1), cancellation, &mut sink)
        .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?;
    let mut text = StreamingSourceTextValidator::new();
    text.push(&sink.0, cancellation)?;
    let contains_nul = text.contains_nul();
    let encoding = text.finish();
    Ok((
        StreamedDiscoverySource {
            capture,
            encoding,
            contains_nul,
            ntriples_nul_proof: None,
            prefix: sink.0[..sink.0.len().min(MAX_STREAMED_CLASSIFICATION_BYTES)].to_vec(),
        },
        sink.0,
    ))
}

impl StreamedSourceReceiver for Receiver {
    fn capture(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        cancellation: &Cancellation,
    ) -> Result<StreamedDiscoverySource, DiscoveryError> {
        capture_bytes(root, path, observed, cancellation).map(|(source, _)| source)
    }

    fn capture_with_nul_grammar(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        requested: Option<NativeNulGrammar>,
        cancellation: &Cancellation,
    ) -> Result<NativeNulCapture, DiscoveryError> {
        let (source, mut bytes) = capture_bytes(root, path, observed, cancellation)?;
        let proof = if let Some(grammar) = requested {
            if self.stale {
                bytes.extend_from_slice(b"\n# stale");
            }
            let mut probe = NativeNulProbe::new(self.grammar.unwrap_or(grammar));
            for chunk in bytes.chunks(7) {
                probe.push(chunk, cancellation)?;
            }
            Some(probe.finish(cancellation)?)
        } else {
            None
        };
        Ok(NativeNulCapture { source, proof })
    }
}

const ORIGINAL_TURTLE: &[u8] = b"<http://a.example/s> <http://a.example/p> '\0&([]\x7f' .\n";
const ORIGINAL_TRIG: &[u8] = b"{<http://a.example/s> <http://a.example/p> '\0&([]\x7f' .}\n<http://example/graph> {<http://a.example/s> <http://a.example/p> '\0&([]\x7f' .}\n";

#[test]
fn dgraph_native_nul_snapshot_and_stream_preserve_configured_route_authority() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = b"<alice><status>\"value\0\\xFF\\v\".\r\n# c\0\n";
    let cases: [(&str, &[u8]); 7] = [
        ("configured.data", source),
        ("override.nq", source),
        ("default.nq", source),
        ("overridden.data", source),
        ("invalid.data", b"<alice\0><status>\"value\"."),
        ("binary.data", b"%PDF-1.7\n# \0\n"),
        ("foreign.bin", source),
    ];
    for (path, bytes) in cases {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
    let root = RepositoryRoot::open(
        derive_repository(b"dgraph-native-nul-routes").id(),
        temporary.path(),
    )
    .unwrap();
    let config = ConfigSnapshot::resolve(&[ConfigLayer { source: ConfigSource::User,
        contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'configured.data'='nquads-dgraph'\n'override.nq'='nquads-dgraph'\n'overridden.data'='rust'\n'invalid.data'='nquads-dgraph'\n'binary.data'='nquads-dgraph'\n" }]).unwrap();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let limits = DiscoveryLimits::new(20, 10, 4096, 20).unwrap();
    let cancellation = Cancellation::new();
    let buffered = discover(&root, &config, &policy, limits, &cancellation).unwrap();
    let streamed = discover_with_streamed_sources(
        &root,
        &config,
        &policy,
        limits,
        StreamedDiscoveryOptions::manifest(4096, 1, &mut Receiver::default()).unwrap(),
        &cancellation,
    )
    .unwrap()
    .into_parts()
    .0;
    assert_eq!(buffered.inputs, streamed.inputs);
    assert_eq!(buffered.exclusions, streamed.exclusions);
    assert_eq!(buffered.inputs.len(), 2);
    for input in &buffered.inputs {
        assert!(matches!(
            input.path.as_str(),
            "configured.data" | "override.nq"
        ));
        assert_eq!(input.content_hash, content_hash(source));
        assert_eq!(input.bytes, u64::try_from(source.len()).unwrap());
        assert!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == "nquads-dgraph")
        );
    }
    assert_eq!(buffered.exclusions.len(), cases.len() - 2);
    assert!(
        buffered
            .exclusions
            .iter()
            .all(|entry| entry.reason == ExclusionReason::Binary)
    );
    for mut receiver in [
        Receiver {
            stale: true,
            grammar: None,
        },
        Receiver {
            stale: false,
            grammar: Some(NativeNulGrammar::NQuads),
        },
        Receiver {
            stale: false,
            grammar: Some(NativeNulGrammar::NTriples),
        },
    ] {
        assert!(matches!(
            discover_with_streamed_sources(
                &root,
                &config,
                &policy,
                limits,
                StreamedDiscoveryOptions::manifest(4096, 1, &mut receiver).unwrap(),
                &cancellation
            ),
            Err(DiscoveryError::IncrementalDrift)
        ));
    }
}

fn config() -> ConfigSnapshot {
    ConfigSnapshot::resolve(&[ConfigLayer { source: ConfigSource::User,
        contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'configured.data'='turtle'\n'overridden.ttl'='rust'\n" }]).unwrap()
}

#[test]
fn contextual_native_nul_snapshot_and_stream_preserve_originals_full_tails_and_routes() {
    assert_eq!(ORIGINAL_TURTLE.len(), 53);
    assert_eq!(ORIGINAL_TRIG.len(), 133);
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let mut original = ORIGINAL_TURTLE.to_vec();
    original.extend_from_slice(b"<tail> <p> <o> .\n");
    let mut trig = ORIGINAL_TRIG.to_vec();
    trig.extend_from_slice(b"<tail> <p> <o> .\n");
    let mut late = b"# padding\n".repeat(10_000);
    late.extend_from_slice(&trig);
    let mut malformed = original.clone();
    malformed.push(0xff);
    let cases = [
        ("original.ttl", original.clone()),
        ("original.trig", trig),
        ("late.trig", late),
        ("configured.data", original.clone()),
        (
            "multiline.ttl",
            b"<s> <p> '''first\n#\0last''' .\n<tail> <p> <o> .\n".to_vec(),
        ),
        ("comment.ttl", b"# before\0after\r\n<s> <p> <o> .".to_vec()),
        ("overridden.ttl", original.clone()),
        ("foreign.n3", original.clone()),
        ("foreign.bin", original),
        ("outside.ttl", b"<s>\0<p><o>.".to_vec()),
        ("iri.trig", b"<s> <p> <o\0> .".to_vec()),
        (
            "unterminated.trig",
            b"GRAPH <g> { <s> <p> '''a\n\0b''' .".to_vec(),
        ),
        ("escape.ttl", b"<s> <p> '\0\\q' .".to_vec()),
        ("short-newline.ttl", b"<s> <p> '\0\n' .".to_vec()),
        ("invalid-tail.ttl", malformed),
        ("pdf.ttl", b"%PDF-1.7\n# \0\n".to_vec()),
        ("zip.trig", b"PK\x03\x04\n# \0\n".to_vec()),
        ("png.ttl", b"\x89PNG\r\n\x1a\n# \0\n".to_vec()),
    ];
    for (path, bytes) in &cases {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
    let root = RepositoryRoot::open(
        derive_repository(b"contextual-native-nul").id(),
        temporary.path(),
    )
    .unwrap();
    let config = config();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let limits = DiscoveryLimits::new(100, 10, 2 * 1024 * 1024, 100).unwrap();
    let cancellation = Cancellation::new();
    let buffered = discover(&root, &config, &policy, limits, &cancellation).unwrap();
    let streamed = discover_with_streamed_sources(
        &root,
        &config,
        &policy,
        limits,
        StreamedDiscoveryOptions::manifest(2 * 1024 * 1024, 1, &mut Receiver::default()).unwrap(),
        &cancellation,
    )
    .unwrap()
    .into_parts()
    .0;
    assert_eq!(buffered.inputs, streamed.inputs);
    assert_eq!(buffered.exclusions, streamed.exclusions);
    for path in [
        "original.ttl",
        "original.trig",
        "late.trig",
        "configured.data",
        "multiline.ttl",
        "comment.ttl",
    ] {
        let input = buffered
            .inputs
            .iter()
            .find(|input| input.path == path)
            .unwrap();
        let bytes = &cases.iter().find(|(name, _)| *name == path).unwrap().1;
        assert_eq!(input.encoding, "utf-8");
        assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(input.content_hash, content_hash(bytes));
        let language = if path.ends_with(".trig") {
            "trig"
        } else {
            "turtle"
        };
        assert!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == language)
        );
    }
    assert_eq!(buffered.inputs.len(), 6);
    assert_eq!(buffered.exclusions.len(), cases.len() - 6);
    assert!(
        buffered
            .exclusions
            .iter()
            .all(|entry| entry.reason == ExclusionReason::Binary)
    );
}

#[test]
fn contextual_native_nul_stream_rejects_stale_and_other_grammar_proofs() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(temporary.path().join("original.ttl"), ORIGINAL_TURTLE).unwrap();
    let root = RepositoryRoot::open(
        derive_repository(b"contextual-native-nul-drift").id(),
        temporary.path(),
    )
    .unwrap();
    let config = config();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let cancellation = Cancellation::new();
    for mut receiver in [
        Receiver {
            stale: true,
            grammar: None,
        },
        Receiver {
            stale: false,
            grammar: Some(NativeNulGrammar::Trig),
        },
    ] {
        assert!(matches!(
            discover_with_streamed_sources(
                &root,
                &config,
                &policy,
                DiscoveryLimits::new(10, 10, 1024, 10).unwrap(),
                StreamedDiscoveryOptions::manifest(1024, 1, &mut receiver).unwrap(),
                &cancellation
            ),
            Err(DiscoveryError::IncrementalDrift)
        ));
    }
}

#[test]
fn nquads_native_nul_snapshot_and_stream_agree_on_routes_and_grammar_authority() {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = b"_:s <urn:p> \"value\0tail\" <urn:g> .\r\n# comment\0\n";
    let cases: [(&str, &[u8]); 8] = [
        ("data.nq", source),
        ("configured.data", source),
        ("overridden.nq", source),
        ("foreign.nt", source),
        ("foreign.bin", source),
        ("relative.nq", b"<s><urn:p>\"value\0tail\"<urn:g>."),
        ("outside.nq", b"<s:><p:>\"value\0tail\"<g:>\0."),
        ("pdf.nq", b"%PDF-1.7\n# \0\n"),
    ];
    for (path, bytes) in cases {
        std::fs::write(temporary.path().join(path), bytes).unwrap();
    }
    let root = RepositoryRoot::open(
        derive_repository(b"nquads-native-nul-routes").id(),
        temporary.path(),
    )
    .unwrap();
    let config = ConfigSnapshot::resolve(&[ConfigLayer { source: ConfigSource::User,
        contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'configured.data'='nquads'\n'overridden.nq'='ntriples'\n" }]).unwrap();
    let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
    let limits = DiscoveryLimits::new(20, 10, 4096, 20).unwrap();
    let cancellation = Cancellation::new();
    let buffered = discover(&root, &config, &policy, limits, &cancellation).unwrap();
    let streamed = discover_with_streamed_sources(
        &root,
        &config,
        &policy,
        limits,
        StreamedDiscoveryOptions::manifest(4096, 1, &mut Receiver::default()).unwrap(),
        &cancellation,
    )
    .unwrap()
    .into_parts()
    .0;
    assert_eq!(buffered.inputs, streamed.inputs);
    assert_eq!(buffered.exclusions, streamed.exclusions);
    assert_eq!(buffered.inputs.len(), 2);
    for input in &buffered.inputs {
        assert!(matches!(input.path.as_str(), "data.nq" | "configured.data"));
        assert_eq!(input.content_hash, content_hash(source));
        assert_eq!(input.bytes, u64::try_from(source.len()).unwrap());
        assert!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == "nquads")
        );
    }
    assert_eq!(buffered.exclusions.len(), cases.len() - 2);
    for mut receiver in [
        Receiver {
            stale: true,
            grammar: None,
        },
        Receiver {
            stale: false,
            grammar: Some(NativeNulGrammar::NTriples),
        },
        Receiver {
            stale: false,
            grammar: Some(NativeNulGrammar::Turtle),
        },
    ] {
        assert!(matches!(
            discover_with_streamed_sources(
                &root,
                &config,
                &policy,
                limits,
                StreamedDiscoveryOptions::manifest(4096, 1, &mut receiver).unwrap(),
                &cancellation
            ),
            Err(DiscoveryError::IncrementalDrift)
        ));
    }
}
