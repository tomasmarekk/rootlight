//! Encoded lexical claims are admitted only against exact bound source bytes.
//! These public transaction tests deliberately use analyzer doubles rather than
//! enable a production parser or substitute decoded VFS snapshots.

use std::{
    fs,
    ops::Range,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, AnalysisUnitId, BatchThresholds, BuildTargetId,
    CoverageReport, EncodingId, GenerationBoundSnapshot, IrRecord, LanguageId,
    MemoryAdmissionPolicy, MemoryEnforcement, ProducerDescriptor, ProjectAnalysisLimits,
    ProjectAnalysisRequest, ProjectSourceInput, SinkError, StreamLimits, execute_analysis,
    execute_project_analysis,
    testkit::{MockLanguageAnalyzer, MockProjectLanguageAnalyzer},
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::content_hash;
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EncodedSourceTextProjection,
    ExtensionEnvelope, ExtensionSupport, FactEvidence, FactRef, FileRecord, IrLimits,
    LexicalEvidenceV1, LexicalExtensionError, ProducerIdentity, ProducerKind, ProvenanceRecord,
    SourceRef, SourceSpan, SourceTextEncoding, decode_lexical_evidence_envelope,
    new_lexical_evidence_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const PREFIX: &str = "fn alpha𝄞(";
const COMMENT: &str = "/* note */";
const SUFFIX: &str = ") -> λ\r\n";

#[test]
fn encoded_lexical_analysis_preserves_both_original_byte_orders() {
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let bytes = encoded(&format!("{PREFIX}{COMMENT}{SUFFIX}"), encoding, true);
        let fixture = Fixture::new(&[bytes]);
        let limits = limits();
        let request = fixture.analysis_request(0, encoding.as_str(), &limits);
        let extension = fixture.extension(0, encoding, None, None);
        let expected_source = extension.evidence.source.clone().unwrap();
        let analyzer = fixture.analyzer(0, encoding.as_str(), vec![extension]);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline(),
        )
        .expect("verified encoded evidence commits");

        assert_eq!(output.document().extensions.len(), 1);
        let retained = &output.document().extensions[0];
        let evidence = decode_lexical_evidence_envelope(retained).unwrap();
        assert_eq!(evidence.text(), format!("{PREFIX} {SUFFIX}"));
        assert_eq!(evidence.source_encoding(), Some(encoding));
        assert_eq!(retained.evidence.source.as_ref(), Some(&expected_source));
        assert_eq!(
            retained.evidence.source.as_ref().unwrap().content_hash(),
            fixture.snapshots[0].content_hash()
        );
        assert_eq!(output.document().files[0].encoding, encoding.as_str());
        assert_eq!(
            fixture.snapshots[0].content(),
            encoded(&format!("{PREFIX}{COMMENT}{SUFFIX}"), encoding, true)
        );
    }
}

#[test]
fn encoded_lexical_analysis_rejects_false_fragments_atomically() {
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let fixture = Fixture::new(&[encoded(
            &format!("{PREFIX}{COMMENT}{SUFFIX}"),
            encoding,
            true,
        )]);
        let limits = limits();
        let request = fixture.analysis_request(0, encoding.as_str(), &limits);
        let honest = fixture.extension(0, encoding, None, None);
        let counterfeit = encoded(&format!("fn omega𝄞({COMMENT}{SUFFIX}"), encoding, false);
        let false_claim = fixture.extension(0, encoding, Some(&counterfeit), None);
        let analyzer = fixture.analyzer(0, encoding.as_str(), vec![honest.clone(), false_claim]);
        let result = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline(),
        );
        assert_eq!(result, Err(AdapterError::Sink(SinkError::SourceMismatch)));
        assert!(!format!("{result:?}").contains("omega"));

        let retry = fixture.analyzer(0, encoding.as_str(), vec![honest]);
        let output = execute_analysis(
            &retry,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline(),
        )
        .expect("a rejected invocation does not poison a later valid transaction");
        assert_eq!(output.document().extensions.len(), 1);
    }
}

#[test]
fn encoded_lexical_analysis_rejects_encoding_mismatches_with_checked_alignment() {
    let encoding = SourceTextEncoding::Utf16Le;
    let fixture = Fixture::new(&[encoded(
        &format!("{PREFIX}{COMMENT}{SUFFIX}"),
        encoding,
        true,
    )]);
    let limits = limits();
    let body = fixture.snapshots[0].content().get(2..).unwrap();
    let odd_source = fixture.reference(0, 1..1 + body.len());
    let honest = fixture.extension(0, encoding, None, None);
    let evidence = decode_lexical_evidence_envelope(&honest).unwrap();
    assert_eq!(
        new_lexical_evidence_envelope(
            odd_source.repository(),
            odd_source.generation(),
            honest.provenance,
            odd_source,
            &evidence,
        ),
        Err(LexicalExtensionError::InvalidSourceProjection),
    );

    let analyzer = fixture.analyzer(0, "utf-8", vec![honest]);
    let request = fixture.analysis_request(0, "utf-8", &limits);
    assert_eq!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline()
        ),
        Err(AdapterError::Sink(SinkError::SourceMismatch)),
    );
}

#[test]
fn encoded_lexical_analysis_requires_matching_bom_and_valid_unselected_tail() {
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let source = format!("{PREFIX}{COMMENT}{SUFFIX}");
        let valid_body = encoded(&source, encoding, false);
        let valid = encoded(&source, encoding, true);
        let opposite = match encoding {
            SourceTextEncoding::Utf16Le => SourceTextEncoding::Utf16Be,
            SourceTextEncoding::Utf16Be => SourceTextEncoding::Utf16Le,
        };
        let mut malformed = valid.clone();
        malformed.extend(unit_bytes(0xd800, encoding));
        let mut nul_tail = valid.clone();
        nul_tail.extend(unit_bytes(0, encoding));
        for (bytes, direct_start) in [
            (valid_body.clone(), 0),
            (encoded(&source, opposite, true), 2),
            (malformed, 2),
            (nul_tail, 2),
        ] {
            let fixture = Fixture::new(&[bytes]);
            let limits = limits();
            let direct = fixture.reference(0, direct_start..direct_start + valid_body.len());
            let claim = fixture.extension(0, encoding, Some(&valid_body), Some(direct));
            let analyzer = fixture.analyzer(0, encoding.as_str(), vec![claim]);
            let request = fixture.analysis_request(0, encoding.as_str(), &limits);
            assert_eq!(
                execute_analysis(
                    &analyzer,
                    &request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::RequireHardOrAccounted,
                    &deadline()
                ),
                Err(AdapterError::Sink(SinkError::SourceMismatch)),
            );
        }
    }
}

#[test]
fn encoded_lexical_project_verifies_each_bound_input_atomically() {
    let encodings = [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be];
    let fixture = Fixture::new(
        &encodings.map(|encoding| encoded(&format!("{PREFIX}{COMMENT}{SUFFIX}"), encoding, true)),
    );
    let limits = limits();
    let inputs = encodings
        .iter()
        .enumerate()
        .map(|(index, encoding)| {
            ProjectSourceInput::new(
                fixture.bound(index),
                language(),
                EncodingId::new(encoding.as_str()).unwrap(),
                false,
                Vec::new(),
            )
        })
        .collect();
    let context = b"{}";
    let request = ProjectAnalysisRequest::new(
        AnalysisUnitId::new("workspace.core").unwrap(),
        BuildTargetId::new("target.core").unwrap(),
        build_context(),
        content_hash(context),
        context,
        inputs,
        AnalysisTier::TierD,
        &limits,
    )
    .unwrap();
    let honest: Vec<_> = encodings
        .iter()
        .enumerate()
        .map(|(index, encoding)| fixture.extension(index, *encoding, None, None))
        .collect();
    let analyzer = fixture.project_analyzer(&encodings, honest.clone());
    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &deadline(),
    )
    .expect("both independently bound physical inputs commit");
    assert_eq!(output.document().extensions.len(), 2);
    for extension in &output.document().extensions {
        let source = extension.evidence.source.as_ref().unwrap();
        let index = fixture
            .sources
            .iter()
            .position(|candidate| candidate.span().file() == source.span().file())
            .unwrap();
        let evidence = decode_lexical_evidence_envelope(extension).unwrap();
        assert_eq!(evidence.source_encoding(), Some(encodings[index]));
        assert_eq!(
            source.content_hash(),
            fixture.snapshots[index].content_hash()
        );
    }

    let counterfeit = encoded(&format!("fn omega𝄞({COMMENT}{SUFFIX}"), encodings[1], false);
    let false_claim = fixture.extension(1, encodings[1], Some(&counterfeit), None);
    let analyzer = fixture.project_analyzer(&encodings, vec![honest[0].clone(), false_claim]);
    assert_eq!(
        execute_project_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline()
        ),
        Err(AdapterError::Sink(SinkError::SourceMismatch)),
    );
}

#[test]
fn encoded_lexical_analysis_rejects_false_file_encoding_metadata() {
    let encoding = SourceTextEncoding::Utf16Le;
    let fixture = Fixture::new(&[encoded(
        &format!("{PREFIX}{COMMENT}{SUFFIX}"),
        encoding,
        true,
    )]);
    let limits = limits();
    let request = fixture.analysis_request(0, encoding.as_str(), &limits);
    let extension = fixture.extension(0, encoding, None, None);
    let analyzer = fixture.analyzer(0, "utf-8", vec![extension]);
    assert_eq!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline(),
        ),
        Err(AdapterError::Sink(SinkError::SourceMismatch)),
    );
}

#[test]
fn encoded_lexical_cancelled_staging_returns_no_admitted_output() {
    let encoding = SourceTextEncoding::Utf16Le;
    let fixture = Fixture::new(&[encoded(
        &format!("{PREFIX}{COMMENT}{SUFFIX}"),
        encoding,
        true,
    )]);
    let limits = limits();
    let request = fixture.analysis_request(0, encoding.as_str(), &limits);
    let extension = fixture.extension(0, encoding, None, None);
    let analyzer = fixture
        .analyzer(0, encoding.as_str(), vec![extension])
        .with_cancellation_after_batches(1, CancellationReason::ClientRequest);
    assert_eq!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline()
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        }),
    );
}

struct Fixture {
    _temporary: TempDir,
    snapshots: Vec<SourceSnapshot>,
    sources: Vec<SourceRef>,
}

impl Fixture {
    fn new(contents: &[Vec<u8>]) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let generation = "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .unwrap();
        let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
        let snapshots: Vec<_> = contents
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                let name = format!("source-{index}.rs");
                fs::write(temporary.path().join(&name), bytes).unwrap();
                repository
                    .snapshot(&RelativePath::parse(Path::new(&name)).unwrap(), 4096)
                    .unwrap()
            })
            .collect();
        let sources = snapshots
            .iter()
            .map(|snapshot| {
                SourceRef::new(
                    repository_id,
                    generation,
                    SourceSpan::new(
                        snapshot.file(),
                        0,
                        u64::try_from(snapshot.content().len()).unwrap(),
                    )
                    .unwrap(),
                    snapshot.content_hash(),
                    None,
                )
            })
            .collect();
        Self {
            _temporary: temporary,
            snapshots,
            sources,
        }
    }

    fn bound(&self, index: usize) -> GenerationBoundSnapshot<'_> {
        GenerationBoundSnapshot::new(&self.snapshots[index], &self.sources[index]).unwrap()
    }

    fn reference(&self, index: usize, range: Range<usize>) -> SourceRef {
        let source = &self.sources[index];
        SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(
                source.span().file(),
                u64::try_from(range.start).unwrap(),
                u64::try_from(range.end).unwrap(),
            )
            .unwrap(),
            source.content_hash(),
            None,
        )
    }

    fn analysis_request<'a>(
        &'a self,
        index: usize,
        encoding: &str,
        limits: &'a AnalysisLimits,
    ) -> AnalysisRequest<'a> {
        AnalysisRequest::new_with_parse_context(
            self.bound(index),
            language(),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            limits,
        )
        .unwrap()
        .with_generated_status(false)
    }

    fn extension(
        &self,
        index: usize,
        encoding: SourceTextEncoding,
        claimed_body: Option<&[u8]>,
        direct: Option<SourceRef>,
    ) -> ExtensionEnvelope {
        let body =
            claimed_body.unwrap_or_else(|| self.snapshots[index].content().get(2..).unwrap());
        let source = direct
            .unwrap_or_else(|| self.reference(index, 2..self.snapshots[index].content().len()));
        let prefix_end = PREFIX.encode_utf16().count() * 2;
        let suffix_start = prefix_end + COMMENT.encode_utf16().count() * 2;
        let ranges = [
            0..prefix_end,
            suffix_start..suffix_start + SUFFIX.encode_utf16().count() * 2,
        ];
        let projection = EncodedSourceTextProjection::from_source(body, encoding, &ranges).unwrap();
        let evidence = LexicalEvidenceV1::from_encoded_source_projection(
            FactRef::File(source.span().file()),
            projection,
        );
        let provenance = self.provenance();
        new_lexical_evidence_envelope(
            source.repository(),
            source.generation(),
            provenance.id,
            source,
            &evidence,
        )
        .unwrap()
    }

    fn provenance(&self) -> ProvenanceRecord {
        let source = &self.sources[0];
        ProvenanceRecord {
            id: "fact1_aeaqcaibaeaqcaibaeaqcaibaeaqcaibwbicmga"
                .parse()
                .unwrap(),
            repository: source.repository(),
            generation: source.generation(),
            producer_kind: ProducerKind::Parser,
            producer: ProducerIdentity::new(
                "rootlight-sdk-test",
                "1.0",
                content_hash(b"encoded-inputs"),
            )
            .unwrap(),
            binary_digest: content_hash(b"encoded-inputs"),
            frontend_version: Some("sdk-test-grammar-1".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierD,
            build_context: build_context(),
            input_sources: self.sources.clone(),
            evidence_sources: self.sources.clone(),
            derivation_parents: Vec::new(),
            rule: None,
        }
    }

    fn descriptor(&self) -> ProducerDescriptor {
        let provenance = self.provenance();
        ProducerDescriptor::new(
            provenance.producer,
            ProducerKind::Parser,
            language(),
            AnalysisTier::TierD,
            MemoryEnforcement::AccountedInProcess,
            false,
        )
    }

    fn file(&self, index: usize, encoding: &str) -> FileRecord {
        let source = &self.sources[index];
        FileRecord {
            id: source.span().file(),
            repository: source.repository(),
            generation: source.generation(),
            path: self.snapshots[index].path().as_str().to_owned(),
            path_locator: Some(self.snapshots[index].path().to_locator()),
            content_hash: source.content_hash(),
            byte_length: source.span().end_byte(),
            language: "rust".to_owned(),
            encoding: encoding.to_owned(),
            generated: false,
            provenance: self.provenance().id,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        }
    }

    fn analyzer(
        &self,
        index: usize,
        encoding: &str,
        extensions: Vec<ExtensionEnvelope>,
    ) -> MockLanguageAnalyzer {
        let mut records = vec![
            IrRecord::File(self.file(index, encoding)),
            IrRecord::Provenance(self.provenance()),
        ];
        records.extend(extensions.into_iter().map(IrRecord::Extension));
        MockLanguageAnalyzer::new(
            self.descriptor(),
            records,
            complete_coverage(self.snapshots[index].content().len()),
            0,
        )
        .with_reported_memory_bytes(256)
    }

    fn project_analyzer(
        &self,
        encodings: &[SourceTextEncoding],
        extensions: Vec<ExtensionEnvelope>,
    ) -> MockProjectLanguageAnalyzer {
        let mut records = vec![IrRecord::Provenance(self.provenance())];
        records.extend(
            encodings
                .iter()
                .enumerate()
                .map(|(index, encoding)| IrRecord::File(self.file(index, encoding.as_str()))),
        );
        records.extend(extensions.into_iter().map(IrRecord::Extension));
        let source_bytes = self
            .snapshots
            .iter()
            .map(|snapshot| snapshot.content().len())
            .sum();
        MockProjectLanguageAnalyzer::new(
            self.descriptor(),
            records,
            complete_coverage(source_bytes),
            0,
        )
        .with_reported_memory_bytes(256)
    }
}

fn encoded(text: &str, encoding: SourceTextEncoding, bom: bool) -> Vec<u8> {
    std::iter::once(0xfeff_u16)
        .take(usize::from(bom))
        .chain(text.encode_utf16())
        .flat_map(|unit| unit_bytes(unit, encoding))
        .collect()
}

fn unit_bytes(unit: u16, encoding: SourceTextEncoding) -> [u8; 2] {
    match encoding {
        SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
        SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
    }
}

fn language() -> LanguageId {
    LanguageId::new("rust").unwrap()
}

fn build_context() -> BuildContextIdentity {
    BuildContextIdentity::new(content_hash(b"encoded-build-context"))
}

fn complete_coverage(source_bytes: usize) -> CoverageReport {
    CoverageReport::new(
        AnalysisTier::TierD,
        CoverageStatus::Complete,
        source_bytes,
        source_bytes,
        0,
        Vec::new(),
    )
    .unwrap()
}

fn limits() -> AnalysisLimits {
    let batch = BatchThresholds::new(2, 32 * 1024, 16, 4096).unwrap();
    let stream = StreamLimits::new(64, 128, 1024 * 1024, 64, 64 * 1024, 64 * 1024, batch).unwrap();
    AnalysisLimits::new(
        4096,
        4096,
        64,
        8,
        1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
    .with_project_limits(ProjectAnalysisLimits::new(4, 16 * 1024, 1024, 8, 4096, 128, 128).unwrap())
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now() + Duration::from_secs(30))
}
