//! Public native analyzer evidence over raw UTF-16 snapshots.
//! These regressions distinguish original byte coordinates from the private
//! decoded parser view, including nested language-owned envelope payloads.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, LexicalEvidenceKind,
    NormalizedIrDocument, ProducerIdentity, SourceRef, SourceSpan, SourceTextEncoding,
    decode_groovy_callable_envelope, decode_groovy_member_envelope,
    decode_lexical_evidence_envelope, decode_perl_binding_envelope,
    decode_vba_compilation_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SYNTAX_NODES: usize = 16_384;
const MAX_SYNTAX_DEPTH: usize = 128;
const FSHARP: &str = "// λ😀\r\nmodule Sample\r\nlet render (value: string) : string = value\r\n";
const FSHARP_SIGNATURE: &str = "// λ😀\r\nmodule Sample\r\nval render: value: string -> string\r\n";
const GROOVY: &str = "// λ😀\r\nclass Container { String title; public fold(value = 1, Object... rest) { value } }\r\n";
const PERL: &str = "# λ😀\r\npackage Measure; sub adjust { $_[0] } sub consume { adjust(3); }\r\n";
const VBA: &str = "' λ😀\r\nAttribute VB_Name = \"Options\"\r\n#Const Enabled = True\r\nSub Run()\r\n#If Enabled Then\r\nDim included As Long\r\n#Else\r\nDim excluded As Long\r\n#End If\r\nEnd Sub\r\n";

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Le,
    Be,
}

impl Encoding {
    fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Le => "utf-16le",
            Self::Be => "utf-16be",
        }
    }

    fn source_encoding(self) -> Option<SourceTextEncoding> {
        match self {
            Self::Utf8 => None,
            Self::Le => Some(SourceTextEncoding::Utf16Le),
            Self::Be => Some(SourceTextEncoding::Utf16Be),
        }
    }

    fn bytes(self, text: &str) -> Vec<u8> {
        if matches!(self, Self::Utf8) {
            return text.as_bytes().to_vec();
        }
        let mut bytes = match self {
            Self::Le => vec![0xff, 0xfe],
            Self::Be => vec![0xfe, 0xff],
            Self::Utf8 => unreachable!(),
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&match self {
                Self::Le => unit.to_le_bytes(),
                Self::Be => unit.to_be_bytes(),
                Self::Utf8 => unreachable!(),
            });
        }
        bytes
    }

    fn offset(self, prefix: &str) -> u64 {
        u64::try_from(if matches!(self, Self::Utf8) {
            prefix.len()
        } else {
            2 + prefix.encode_utf16().count() * 2
        })
        .unwrap()
    }
}

#[test]
fn utf8_controls_establish_native_encoded_fixture_semantics() {
    for (language, path, frontend, text) in cases() {
        let fixture = Fixture::new(path, text, Encoding::Utf8);
        let output = fixture.analyze(language, frontend, &limits(MAX_SYNTAX_NODES));
        assert_physical_sources(&fixture, output.document());
        assert_native_payloads(&fixture, language, output.document());
    }
}

#[test]
fn utf16_fsharp_profiles_preserve_raw_identity_and_encoded_signatures() {
    for encoding in [Encoding::Le, Encoding::Be] {
        for (language, path, text) in [
            ("fsharp", "source/example.fs", FSHARP),
            ("fsharp-signature", "source/example.fsi", FSHARP_SIGNATURE),
        ] {
            let fixture = Fixture::new(path, text, encoding);
            let output = fixture.analyze(
                language,
                "tree-sitter-fsharp-0.3.12",
                &limits(MAX_SYNTAX_NODES),
            );
            assert_physical_sources(&fixture, output.document());
            assert_native_payloads(&fixture, language, output.document());
            assert_encoded_signatures(&fixture, output.document());
        }
    }
}

#[test]
fn utf16_nested_language_envelopes_use_original_coordinates() {
    for encoding in [Encoding::Le, Encoding::Be] {
        for (language, path, frontend, text) in cases().into_iter().skip(2) {
            let fixture = Fixture::new(path, text, encoding);
            let output = fixture.analyze(language, frontend, &limits(MAX_SYNTAX_NODES));
            assert_physical_sources(&fixture, output.document());
            assert_native_payloads(&fixture, language, output.document());
            assert_encoded_signatures(&fixture, output.document());
        }
    }
}

#[test]
fn utf16_prepared_exact_fit_replay_preserves_physical_ir() {
    for encoding in [Encoding::Le, Encoding::Be] {
        let fixture = Fixture::new("source/example.fs", FSHARP, encoding);
        let analyzer = analyzer("fsharp", "tree-sitter-fsharp-0.3.12");
        let full_limits = limits(MAX_SYNTAX_NODES);
        let request = fixture.request("fsharp", &full_limits);
        let cancellation = cancellation();
        let artifact = analyzer.prepare_artifact(&request, &cancellation).unwrap();
        assert!(artifact.syntax_partition_complete());
        assert_eq!(artifact.content_hash(), fixture.source.content_hash());
        let exact_limits = limits(artifact.syntax_fact_count());
        assert!(!artifact.is_compatible_with_limits(&exact_limits));
        assert!(artifact.is_compatible_with_prepared_limits(&exact_limits));
        let prepared = analyzer
            .analyze_prepared_artifact(
                &fixture.request("fsharp", &exact_limits),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .unwrap();
        let clean = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
        assert_eq!(prepared.document(), clean.document());
        assert_eq!(prepared.report(), clean.report());
        assert_physical_sources(&fixture, prepared.document());
        assert_encoded_signatures(&fixture, prepared.document());
    }
}

fn cases() -> [(&'static str, &'static str, &'static str, &'static str); 5] {
    [
        (
            "fsharp",
            "source/example.fs",
            "tree-sitter-fsharp-0.3.12",
            FSHARP,
        ),
        (
            "fsharp-signature",
            "source/example.fsi",
            "tree-sitter-fsharp-0.3.12",
            FSHARP_SIGNATURE,
        ),
        (
            "groovy",
            "source/example.groovy",
            "dekobon-tree-sitter-groovy-0.2.0",
            GROOVY,
        ),
        ("perl", "source/example.pm", "tree-sitter-perl-2.0.0", PERL),
        ("vba", "source/example.bas", "tree-sitter-vba-0.14.1", VBA),
    ]
}

fn assert_native_payloads(fixture: &Fixture, language: &str, document: &NormalizedIrDocument) {
    match language {
        "fsharp" | "fsharp-signature" => {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == "render")
            );
            assert!(document.occurrences.iter().any(|occurrence| {
                occurrence.source.span() == fixture.span("render")
                    && occurrence.syntactic_text_hash == content_hash(b"render")
            }));
        }
        "groovy" => {
            let callable = document
                .extensions
                .iter()
                .find_map(|envelope| {
                    (envelope.namespace == rootlight_ir::GROOVY_CALLABLE_NAMESPACE)
                        .then(|| decode_groovy_callable_envelope(envelope).unwrap())
                })
                .unwrap();
            assert_eq!(callable.name, fixture.span("fold"));
            assert_eq!(
                callable.parameters,
                fixture.span("(value = 1, Object... rest)")
            );
            assert_eq!(callable.parameter_count, 2);
            assert_eq!(callable.required_parameter_count, 1);
            assert!(callable.variadic);
            let raw_name = fixture.bytes_for_span(callable.name);
            let encoded_name = fixture.encoding.bytes("fold");
            let expected_name = if matches!(fixture.encoding, Encoding::Utf8) {
                encoded_name.as_slice()
            } else {
                &encoded_name[2..]
            };
            assert_eq!(raw_name, expected_name);
            assert_eq!(
                callable.name_identity.unwrap().spelling_hash,
                content_hash(b"fold")
            );
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == rootlight_ir::OccurrenceRole::Definition
                        && site.source.span() == callable.name
                })
                .unwrap();
            assert_eq!(
                callable.name_identity.unwrap().spelling_hash,
                definition.syntactic_text_hash
            );
            let member = document
                .extensions
                .iter()
                .find_map(|envelope| {
                    (envelope.namespace == rootlight_ir::GROOVY_MEMBER_NAMESPACE)
                        .then(|| decode_groovy_member_envelope(envelope).unwrap())
                })
                .unwrap();
            assert_eq!(member.name, fixture.span("title"));
            assert_eq!(member.owner, fixture.span("class Container { String title; public fold(value = 1, Object... rest) { value } }"));
            assert!(member.declaration.start_byte() <= member.name.start_byte());
            assert!(member.name.end_byte() <= member.declaration.end_byte());
        }
        "perl" => {
            let bindings: Vec<_> = document
                .extensions
                .iter()
                .filter(|envelope| envelope.namespace == rootlight_ir::PERL_BINDING_NAMESPACE)
                .map(|envelope| decode_perl_binding_envelope(envelope).unwrap())
                .collect();
            assert!(!bindings.is_empty());
            for binding in bindings {
                assert_eq!(binding.module(), fixture.source.span());
            }
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == "adjust")
            );
        }
        "vba" => {
            let compilation = document
                .extensions
                .iter()
                .find_map(|envelope| {
                    (envelope.namespace == rootlight_ir::VBA_COMPILATION_NAMESPACE)
                        .then(|| decode_vba_compilation_envelope(envelope).unwrap())
                })
                .unwrap();
            assert_eq!(compilation.scope, fixture.span(VBA));
            assert!(compilation.complete);
            assert_eq!(compilation.branches.len(), 2);
            let group = fixture.span("#If Enabled Then\r\nDim included As Long\r\n#Else\r\nDim excluded As Long\r\n#End If");
            assert_eq!(compilation.branches[0].group, group);
            assert_eq!(compilation.branches[1].group, group);
            assert_eq!(
                compilation.branches[0].region,
                fixture.span("Dim included As Long\r\n")
            );
            assert_eq!(
                compilation.branches[1].region,
                fixture.span("Dim excluded As Long\r\n")
            );
        }
        _ => unreachable!(),
    }
}

fn assert_encoded_signatures(fixture: &Fixture, document: &NormalizedIrDocument) {
    let mut count = 0;
    let mut signatures = Vec::new();
    for envelope in &document.extensions {
        if envelope.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE
            || envelope.version == rootlight_ir::LEXICAL_SUMMARY_BATCH_VERSION
        {
            continue;
        }
        if envelope.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION {
            signatures
                .extend(rootlight_ir::decode_lexical_signature_batch_envelope(envelope).unwrap());
        } else {
            signatures.push(envelope.clone());
        }
    }
    for envelope in &signatures {
        let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
        if lexical.kind() != LexicalEvidenceKind::Signature {
            continue;
        }
        count += 1;
        assert_eq!(
            envelope.version,
            rootlight_ir::LEXICAL_ENCODED_PROJECTION_VERSION
        );
        assert_eq!(
            lexical.source_encoding(),
            fixture.encoding.source_encoding()
        );
        lexical
            .verify_source_bytes(
                fixture.bytes_for_span(envelope.evidence.source.as_ref().unwrap().span()),
            )
            .unwrap();
    }
    assert!(count > 0);
}

fn assert_physical_sources(fixture: &Fixture, document: &NormalizedIrDocument) {
    validate_ir_document(document, &IrLimits::default(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    assert_eq!(
        document.files[0].evidence.source.as_ref(),
        Some(&fixture.source)
    );
    assert_eq!(
        document.files[0].content_hash,
        fixture.source.content_hash()
    );
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(fixture.raw.len()).unwrap()
    );
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(
            document
                .occurrences
                .iter()
                .map(|occurrence| &occurrence.source),
        )
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|envelope| envelope.evidence.source.as_ref()),
        )
        .chain(
            document
                .provenance
                .iter()
                .flat_map(|proof| proof.input_sources.iter().chain(&proof.evidence_sources)),
        )
    {
        assert_eq!(reference.repository(), fixture.source.repository());
        assert_eq!(reference.generation(), fixture.source.generation());
        assert_eq!(reference.span().file(), fixture.source.span().file());
        assert_eq!(reference.content_hash(), fixture.source.content_hash());
        assert!(reference.span().end_byte() <= fixture.source.span().end_byte());
        if !matches!(fixture.encoding, Encoding::Utf8) {
            assert!(reference.span().start_byte().is_multiple_of(2));
            assert!(reference.span().end_byte().is_multiple_of(2));
        }
    }
}

fn analyzer(language: &str, frontend: &str) -> TreeSitterAnalyzer {
    let provider = TreeSitterProvider::new(
        RuntimeConfig::new(
            MAX_SOURCE_BYTES,
            MAX_SYNTAX_NODES,
            MAX_SYNTAX_DEPTH,
            32,
            64,
            1,
            16 * 1024 * 1024,
            ParserSettings::new(4096).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "rootlight-native-encoding-test",
            "1.0",
            content_hash(b"native-encoding-config"),
        )
        .unwrap(),
        rootlight_adapter_sdk::LanguageId::new(language).unwrap(),
        frontend,
        content_hash(b"native-encoding-binary"),
    )
    .unwrap()
}

fn limits(syntax_records: usize) -> AnalysisLimits {
    let stream = |records| {
        StreamLimits::new(
            128,
            records,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            4 * 1024 * 1024,
            BatchThresholds::new(128.min(records), 1024 * 1024, 32, 128 * 1024).unwrap(),
        )
        .unwrap()
    };
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        16 * 1024 * 1024,
        stream(syntax_records),
        stream(MAX_SYNTAX_NODES),
        IrLimits::default(),
    )
    .unwrap()
}

fn cancellation() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

struct Fixture {
    _temporary: TempDir,
    text: String,
    raw: Vec<u8>,
    encoding: Encoding,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(path: &str, text: &str, encoding: Encoding) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let relative = RelativePath::parse(Path::new(path)).unwrap();
        fs::create_dir_all(temporary.path().join(Path::new(path).parent().unwrap())).unwrap();
        let raw = encoding.bytes(text);
        fs::write(temporary.path().join(path), &raw).unwrap();
        let repository = derive_repository(b"native-encoding-contract").id();
        let root = RepositoryRoot::open(repository, temporary.path()).unwrap();
        let snapshot = root
            .snapshot(&relative, u64::try_from(MAX_SOURCE_BYTES).unwrap())
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([23; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _temporary: temporary,
            text: text.to_owned(),
            raw,
            encoding,
            snapshot,
            source,
        }
    }

    fn request<'a>(&'a self, language: &str, limits: &'a AnalysisLimits) -> AnalysisRequest<'a> {
        AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            rootlight_adapter_sdk::LanguageId::new(language).unwrap(),
            EncodingId::new(self.encoding.name()).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"native-encoding-build")),
            limits,
        )
        .unwrap()
        .with_generated_status(false)
    }

    fn analyze(&self, language: &str, frontend: &str, limits: &AnalysisLimits) -> AnalysisOutput {
        execute_analysis(
            &analyzer(language, frontend),
            &self.request(language, limits),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation(),
        )
        .unwrap()
    }

    fn span(&self, needle: &str) -> SourceSpan {
        let start = self.text.find(needle).unwrap();
        SourceSpan::new(
            self.source.span().file(),
            self.encoding.offset(&self.text[..start]),
            self.encoding.offset(&self.text[..start + needle.len()]),
        )
        .unwrap()
    }

    fn bytes_for_span(&self, span: SourceSpan) -> &[u8] {
        &self.raw
            [usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()]
    }
}
