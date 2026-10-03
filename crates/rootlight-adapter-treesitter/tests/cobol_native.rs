//! Native COBOL structural projection preserves original format-specific spans.
//! Written program and storage ownership does not prove use-site dispatch,
//! COPY expansion, data-level hierarchy or compiler legality.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseRequest, ResourceKind,
    SinkError, StreamLimits, SyntaxFactKind, execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    GrammarFamily, ParserSettings, ReuseInvalidation, ReuseStatus, RuntimeConfig,
    TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, DiagnosticSeverity,
    EntityKind, EntityRecord, ExtensionSupport, FactDomain, FactRef, IrLimits,
    LEXICAL_EXTENSION_NAMESPACE, LexicalEvidenceKind, NormalizedIrDocument, OccurrenceRole,
    OccurrenceTarget, RelationPredicate, SkippedRegionReason, SourceRef, SourceSpan,
    decode_lexical_evidence_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const POLICY: MemoryAdmissionPolicy = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn provider() -> Arc<TreeSitterProvider> {
    Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                8192,
                4096,
                128,
                32,
                64,
                1,
                4 * 1024 * 1024,
                ParserSettings::new(512).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    )
}

fn limits(ir: IrLimits) -> AnalysisLimits {
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        8192,
        4096,
        128,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap()
}

struct Fixture {
    _directory: TempDir,
    text: String,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(lines: &[&str], language: &str, ending: &str) -> Self {
        let mut text = String::new();
        for line in lines {
            if language == "cobol-fixed" {
                text.push_str("       ");
            }
            text.push_str(line);
            text.push_str(ending);
        }
        Self::from_text(text)
    }

    fn from_text(text: String) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(directory.path().join("Sample.cbl"), &text).unwrap();
        let repository = derive_repository(b"cobol-native-structural-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("Sample.cbl")).unwrap(), 8192)
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([11; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _directory: directory,
            text,
            snapshot,
            source,
        }
    }

    fn span(&self, written: &str, occurrence: usize) -> SourceSpan {
        let start = self.text.match_indices(written).nth(occurrence).unwrap().0;
        SourceSpan::new(
            self.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + written.len()).unwrap(),
        )
        .unwrap()
    }

    fn parse_request<'a>(&'a self, language: &str, limits: &'a AnalysisLimits) -> ParseRequest<'a> {
        ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new(language).unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            limits,
        )
        .unwrap()
    }

    fn analyze(&self, language: &str) -> AnalysisOutput {
        let parser = provider();
        let limits = limits(IrLimits::default());
        let parsed = execute_parse(
            parser.as_ref(),
            &self.parse_request(language, &limits),
            POLICY,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let output = self
            .analyze_with(parser, language, &limits, &deadline())
            .unwrap();
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        for occurrence in &output.document().occurrences {
            assert_eq!(
                occurrence.id,
                rootlight_ir::derive_occurrence_record_id(occurrence).unwrap()
            );
            assert_eq!(occurrence.source.content_hash(), self.source.content_hash());
            assert_eq!(occurrence.source.generation(), self.source.generation());
        }
        output
    }

    fn analyze_with(
        &self,
        parser: Arc<TreeSitterProvider>,
        language: &str,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<AnalysisOutput, AdapterError> {
        let language = LanguageId::new(language).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            parser,
            rootlight_ir::ProducerIdentity::new(
                "cobol-native-contract",
                "1.0",
                content_hash(b"cobol-native-configuration"),
            )
            .unwrap(),
            language.clone(),
            "tree-sitter-cobol-reviewed",
            content_hash(language.as_str().as_bytes()),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"cobol-native-build-context")),
            limits,
        )
        .unwrap()
        .with_generated_status(false);
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            POLICY,
            cancellation,
        )
    }
}

fn fixture_at_eof(lines: &[&str], language: &str, ending: &str, final_newline: bool) -> Fixture {
    let prefix = if language == "cobol-fixed" {
        "       "
    } else {
        ""
    };
    let mut text = String::new();
    for (index, line) in lines.iter().enumerate() {
        text.push_str(prefix);
        text.push_str(line);
        if final_newline || index + 1 < lines.len() {
            text.push_str(ending);
        }
    }
    Fixture::from_text(text)
}

fn definition(document: &NormalizedIrDocument, span: SourceSpan) -> &EntityRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition && site.source.span() == span)
        .collect();
    assert_eq!(sites.len(), 1, "definition at {span:?}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition must have a source-owned entity");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap()
}

fn assert_header(
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    span: SourceSpan,
    text: &str,
) {
    let headers: Vec<_> = document
        .extensions
        .iter()
        .filter_map(|envelope| {
            if envelope.namespace != LEXICAL_EXTENSION_NAMESPACE {
                return None;
            }
            let evidence = decode_lexical_evidence_envelope(envelope).unwrap();
            (evidence.kind() == LexicalEvidenceKind::Signature
                && evidence.subject() == FactRef::Entity(entity.id))
            .then_some((envelope, evidence))
        })
        .collect();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].0.evidence.source.as_ref().unwrap().span(), span);
    assert_eq!(headers[0].1.text(), text);
    assert_eq!(
        headers[0].1.complete_text_hash(),
        content_hash(text.as_bytes())
    );
}

fn assert_unproven_reference(
    document: &NormalizedIrDocument,
    fixture: &Fixture,
    written: &str,
    occurrence: usize,
    syntax: &str,
    enclosing: Option<&EntityRecord>,
) {
    let span = fixture.span(written, occurrence);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.source.span() == span)
        .collect();
    assert_eq!(sites.len(), 1, "{syntax}: {written} at {span:?}");
    let site = sites[0];
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert_eq!(site.syntax_kind, syntax);
    assert_eq!(site.enclosing, enclosing.map(|entity| entity.id));
    assert_eq!(site.syntactic_text_hash, content_hash(written.as_bytes()));
    assert_eq!(
        site.target,
        OccurrenceTarget::Unresolved {
            text_hash: content_hash(written.as_bytes()),
        }
    );
    assert_eq!(site.provenance, document.provenance[0].id);
    assert_eq!(site.source.repository(), fixture.source.repository());
    assert_eq!(site.source.generation(), fixture.source.generation());
    assert_eq!(site.source.content_hash(), fixture.source.content_hash());
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.source.span() == span && gap.detail == "cobol-binding-target-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].domain, FactDomain::Relations);
    assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
    assert_eq!(gaps[0].provenance, site.provenance);
    assert_eq!(gaps[0].source, site.source);
}

#[test]
fn written_programs_own_data_and_header_only_procedural_entries() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            let fixture = Fixture::new(
                &[
                    "identification division.",
                    "program-id. Alpha.",
                    "data division.",
                    "working-storage section.",
                    "01 Shared-Item pic 9.",
                    "05 FILLER pic x.",
                    "procedure division.",
                    "Work-Section section.",
                    "First-Point.",
                    "display 'first'.",
                    "100.",
                    "goback.",
                    "end program Alpha.",
                    "identification division.",
                    "program-id. Beta.",
                    "data division.",
                    "working-storage section.",
                    "01 Shared-Item pic 9.",
                    "procedure division.",
                    "First-Point.",
                    "goback.",
                    "end program Beta.",
                ],
                language,
                ending,
            );
            let output = fixture.analyze(language);
            let document = output.document();
            let alpha = definition(document, fixture.span("Alpha", 0));
            let beta = definition(document, fixture.span("Beta", 0));
            assert_eq!(alpha.kind, EntityKind::Module);
            assert_eq!(beta.kind, EntityKind::Module);
            assert_ne!(alpha.id, beta.id);
            assert_eq!(
                alpha.container,
                Some(ContainerRef::File(fixture.snapshot.file()))
            );
            assert_eq!(document.entities.len(), 8);
            for (index, owner) in [(0, alpha), (1, beta)] {
                let item = definition(document, fixture.span("Shared-Item", index));
                assert_eq!(item.kind, EntityKind::Variable);
                assert_eq!(item.language, language);
                assert_eq!(item.container, Some(ContainerRef::Entity(owner.id)));
                assert_eq!(
                    item.evidence.source.as_ref().unwrap().span(),
                    fixture.span("01 Shared-Item pic 9", index)
                );
                let paragraph = definition(document, fixture.span("First-Point", index));
                assert_eq!(paragraph.kind, EntityKind::Function);
                assert_eq!(paragraph.container, Some(ContainerRef::Entity(owner.id)));
                assert_eq!(
                    paragraph.evidence.source.as_ref().unwrap().span(),
                    fixture.span("First-Point.", index)
                );
                assert_header(
                    document,
                    paragraph,
                    fixture.span("First-Point.", index),
                    "First-Point.",
                );
            }
            let section = definition(document, fixture.span("Work-Section", 0));
            assert_eq!(section.container, Some(ContainerRef::Entity(alpha.id)));
            assert_header(
                document,
                section,
                fixture.span("Work-Section section.", 0),
                "Work-Section section.",
            );
            let numeric = definition(document, fixture.span("100", 0));
            assert_eq!(numeric.canonical_name, "100");
            assert_eq!(numeric.container, Some(ContainerRef::Entity(alpha.id)));
            assert!(
                document
                    .entities
                    .iter()
                    .all(|entity| entity.canonical_name != "FILLER")
            );
            assert!(
                document
                    .occurrences
                    .iter()
                    .all(|site| site.role != OccurrenceRole::Definition
                        || site.source.span() != fixture.span("FILLER", 0))
            );
            assert!(document.relations.iter().all(|edge| matches!(
                edge.predicate,
                RelationPredicate::Contains
                    | RelationPredicate::Declares
                    | RelationPredicate::DefinesAt
            )));
        }
    }
}

#[test]
fn file_descriptors_keep_exact_headers_program_owners_and_record_independence() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            let mut previous = None;
            for record in ["01 Input-Record pic x(8).", "01 Input-Record pic x(9)."] {
                let fixture = Fixture::new(
                    &[
                        "identification division.",
                        "program-id. Alpha.",
                        "environment division.",
                        "input-output section.",
                        "file-control.",
                        "select Shared-File assign to keyboard.",
                        "data division.",
                        "file section.",
                        "fd Shared-File.",
                        record,
                        "sd Sorted-File.",
                        "01 Sort-Record pic x(8).",
                        "procedure division.",
                        "open input Shared-File.",
                        "read Shared-File.",
                        "close Shared-File.",
                        "goback.",
                        "end program Alpha.",
                        "identification division.",
                        "program-id. Beta.",
                        "environment division.",
                        "input-output section.",
                        "file-control.",
                        "select Shared-File assign to keyboard.",
                        "data division.",
                        "file section.",
                        "fd Shared-File.",
                        record,
                        "sd Sorted-File.",
                        "01 Sort-Record pic x(8).",
                        "procedure division.",
                        "open input Shared-File.",
                        "read Shared-File.",
                        "close Shared-File.",
                        "goback.",
                        "end program Beta.",
                    ],
                    language,
                    ending,
                );
                let output = fixture.analyze(language);
                let document = output.document();
                let mut descriptor_ids = Vec::new();
                let mut expected_definitions = Vec::new();
                for (index, program) in ["Alpha", "Beta"].into_iter().enumerate() {
                    let owner_span = fixture.span(program, 0);
                    let owner = definition(document, owner_span);
                    assert_eq!(owner.kind, EntityKind::Module);
                    expected_definitions.push(owner_span);
                    for (name, occurrence, header) in [
                        ("Shared-File", index * 5 + 1, "Shared-File."),
                        ("Sorted-File", index, "Sorted-File."),
                    ] {
                        let span = fixture.span(name, occurrence);
                        let descriptor = definition(document, span);
                        assert_eq!(descriptor.kind, EntityKind::Variable);
                        assert_eq!(descriptor.canonical_name, name);
                        assert_eq!(descriptor.display_name, name);
                        assert_eq!(descriptor.language, language);
                        assert_eq!(descriptor.repository, fixture.source.repository());
                        assert_eq!(descriptor.generation, fixture.source.generation());
                        assert_eq!(descriptor.container, Some(ContainerRef::Entity(owner.id)));
                        let evidence = descriptor.evidence.source.as_ref().unwrap();
                        let header_span = SourceSpan::new(
                            fixture.snapshot.file(),
                            span.start_byte(),
                            span.end_byte().checked_add(1).unwrap(),
                        )
                        .unwrap();
                        assert_eq!(evidence.span(), header_span);
                        assert_eq!(
                            fixture
                                .text
                                .get(
                                    usize::try_from(header_span.start_byte()).unwrap()
                                        ..usize::try_from(header_span.end_byte()).unwrap()
                                )
                                .unwrap(),
                            header
                        );
                        assert_eq!(evidence.repository(), fixture.source.repository());
                        assert_eq!(evidence.generation(), fixture.source.generation());
                        assert_eq!(evidence.content_hash(), fixture.source.content_hash());
                        assert_eq!(descriptor.provenance, document.provenance[0].id);
                        let sites: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.source.span() == span
                            })
                            .collect();
                        assert_eq!(sites.len(), 1);
                        assert_eq!(
                            sites[0].syntax_kind,
                            "cobol.file_descriptor_name.definition"
                        );
                        assert_eq!(sites[0].syntactic_text_hash, content_hash(name.as_bytes()));
                        assert_eq!(sites[0].provenance, document.provenance[0].id);
                        descriptor_ids.push(descriptor.id);
                        expected_definitions.push(span);
                    }
                    for (name, header) in [
                        ("Input-Record", record.strip_suffix('.').unwrap()),
                        ("Sort-Record", "01 Sort-Record pic x(8)"),
                    ] {
                        let span = fixture.span(name, index);
                        let item = definition(document, span);
                        assert_eq!(item.kind, EntityKind::Variable);
                        assert_eq!(item.container, Some(ContainerRef::Entity(owner.id)));
                        assert_eq!(
                            item.evidence.source.as_ref().unwrap().span(),
                            fixture.span(header, index)
                        );
                        expected_definitions.push(span);
                    }
                }
                assert_eq!(document.entities.len(), 10);
                assert_eq!(descriptor_ids.len(), 4);
                assert_ne!(descriptor_ids[0], descriptor_ids[1]);
                assert_ne!(descriptor_ids[0], descriptor_ids[2]);
                assert_ne!(descriptor_ids[1], descriptor_ids[3]);
                let definitions: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::Definition)
                    .collect();
                assert_eq!(definitions.len(), expected_definitions.len());
                assert!(
                    definitions
                        .iter()
                        .all(|site| expected_definitions.contains(&site.source.span()))
                );
                // SELECT and I/O spellings do not create additional descriptor definitions.
                for occurrence in [0, 2, 3, 4, 5, 7, 8, 9] {
                    assert!(
                        definitions
                            .iter()
                            .all(|site| site.source.span()
                                != fixture.span("Shared-File", occurrence))
                    );
                }
                assert!(
                    document
                        .occurrences
                        .iter()
                        .filter(|site| site.role != OccurrenceRole::Definition)
                        .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
                );
                assert!(document.relations.iter().all(|edge| matches!(
                    edge.predicate,
                    RelationPredicate::Contains
                        | RelationPredicate::Declares
                        | RelationPredicate::DefinesAt
                )));
                if let Some((identities, hash)) = &previous {
                    assert_eq!(&descriptor_ids, identities);
                    assert_ne!(&fixture.source.content_hash(), hash);
                }
                previous = Some((descriptor_ids, fixture.source.content_hash()));
            }
        }
    }
}

#[test]
fn prefixed_literals_keep_public_string_and_storage_evidence_across_profiles() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for prefix in ['x', 'X'] {
                    for quote in ['\'', '"'] {
                        let literal = format!("{prefix}{quote}0aF1{quote}");
                        let declaration = format!("01 Stored pic x(4) value {literal}");
                        let fixture = fixture_at_eof(
                            &[
                                "identification division.",
                                "program-id. Literals.",
                                "data division.",
                                "working-storage section.",
                                &format!("{declaration}."),
                                "procedure division.",
                                &format!("move {literal} to Stored."),
                                "display Stored.",
                                "goback.",
                                "end program Literals.",
                            ],
                            language,
                            ending,
                            final_newline,
                        );
                        let output = fixture.analyze(language);
                        let document = output.document();
                        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
                        assert!(document.diagnostics.is_empty());
                        assert_eq!(document.entities.len(), 2);
                        let program = definition(document, fixture.span("Literals", 0));
                        assert_eq!(program.kind, EntityKind::Module);
                        assert_eq!(program.language, language);
                        assert_eq!(
                            program.container,
                            Some(ContainerRef::File(fixture.snapshot.file()))
                        );
                        let item = definition(document, fixture.span("Stored", 0));
                        assert_eq!(item.kind, EntityKind::Variable);
                        assert_eq!(item.canonical_name, "Stored");
                        assert_eq!(item.language, language);
                        assert_eq!(item.container, Some(ContainerRef::Entity(program.id)));
                        let evidence = item.evidence.source.as_ref().unwrap();
                        assert_eq!(evidence.span(), fixture.span(&declaration, 0));
                        assert_eq!(evidence.content_hash(), fixture.source.content_hash());
                        assert_eq!(evidence.generation(), fixture.source.generation());
                        let strings: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| site.role == OccurrenceRole::StringEvidence)
                            .collect();
                        assert_eq!(strings.len(), 2);
                        for occurrence in 0..2 {
                            let span = fixture.span(&literal, occurrence);
                            let sites: Vec<_> = strings
                                .iter()
                                .filter(|site| site.source.span() == span)
                                .collect();
                            assert_eq!(sites.len(), 1);
                            assert_eq!(sites[0].syntax_kind, "cobol.string.string");
                            assert_eq!(
                                sites[0].syntactic_text_hash,
                                content_hash(literal.as_bytes())
                            );
                            assert_eq!(sites[0].provenance, document.provenance[0].id);
                            assert!(matches!(
                                sites[0].target,
                                OccurrenceTarget::Unresolved { .. }
                            ));
                        }
                        for (written, occurrence, syntax) in [
                            ("Stored", 1, "cobol.storage.reference"),
                            ("Stored", 2, "cobol.storage.reference"),
                            ("Literals", 1, "cobol.end_program_name.reference"),
                        ] {
                            let span = fixture.span(written, occurrence);
                            let sites: Vec<_> = document
                                .occurrences
                                .iter()
                                .filter(|site| site.source.span() == span)
                                .collect();
                            assert_eq!(sites.len(), 1);
                            assert_eq!(sites[0].syntax_kind, syntax);
                            assert_eq!(sites[0].role, OccurrenceRole::Reference);
                            assert_eq!(
                                sites[0].syntactic_text_hash,
                                content_hash(written.as_bytes())
                            );
                            assert!(matches!(
                                sites[0].target,
                                OccurrenceTarget::Unresolved { .. }
                            ));
                            assert!(document.skipped_regions.iter().any(|region| {
                                region.detail == "cobol-binding-target-unavailable"
                                    && region.source.span() == span
                                    && region.domain == FactDomain::Relations
                                    && region.reason == SkippedRegionReason::UnsupportedConstruct
                            }));
                        }
                        assert_eq!(document.occurrences.len(), 7);
                        assert_eq!(document.skipped_regions.len(), 3);
                        assert!(document.relations.iter().all(|edge| matches!(
                            edge.predicate,
                            RelationPredicate::Contains
                                | RelationPredicate::Declares
                                | RelationPredicate::DefinesAt
                        )));
                    }
                }
            }
        }
    }
}

#[test]
fn line_sequential_files_keep_source_owned_entities_without_file_bindings() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for clause in [
                    "organization is line sequential",
                    "ORGANIZATION IS LINE SEQUENTIAL",
                    "OrGaNiZaTiOn Is LiNe SeQuEnTiAl",
                ] {
                    let fixture = fixture_at_eof(
                        &[
                            "identification division.",
                            "program-id. FileLayout.",
                            "environment division.",
                            "input-output section.",
                            "file-control.",
                            "select Input-File",
                            "assign to 'records.dat'",
                            &format!("{clause}."),
                            "data division.",
                            "file section.",
                            "fd Input-File.",
                            "01 Input-Record pic x(4).",
                            "procedure division.",
                            "open input Input-File.",
                            "read Input-File.",
                            "move Input-Record to Input-Record.",
                            "close Input-File.",
                            "goback.",
                            "end program FileLayout.",
                        ],
                        language,
                        ending,
                        final_newline,
                    );
                    let output = fixture.analyze(language);
                    let document = output.document();
                    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
                    assert!(document.diagnostics.is_empty());
                    assert_eq!(document.entities.len(), 3);
                    let program = definition(document, fixture.span("FileLayout", 0));
                    assert_eq!(program.kind, EntityKind::Module);
                    assert_eq!(program.language, language);
                    for (name, occurrence, header, syntax) in [
                        (
                            "Input-File",
                            1,
                            "Input-File.",
                            "cobol.file_descriptor_name.definition",
                        ),
                        (
                            "Input-Record",
                            0,
                            "01 Input-Record pic x(4)",
                            "cobol.data_name.definition",
                        ),
                    ] {
                        let span = fixture.span(name, occurrence);
                        let item = definition(document, span);
                        assert_eq!(item.kind, EntityKind::Variable);
                        assert_eq!(item.canonical_name, name);
                        assert_eq!(item.language, language);
                        assert_eq!(item.container, Some(ContainerRef::Entity(program.id)));
                        let evidence = item.evidence.source.as_ref().unwrap();
                        assert_eq!(evidence.span(), fixture.span(header, 0));
                        assert_eq!(evidence.content_hash(), fixture.source.content_hash());
                        assert_eq!(evidence.generation(), fixture.source.generation());
                        let sites: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.source.span() == span
                            })
                            .collect();
                        assert_eq!(sites.len(), 1);
                        assert_eq!(sites[0].syntax_kind, syntax);
                        assert_eq!(sites[0].syntactic_text_hash, content_hash(name.as_bytes()));
                    }
                    for (written, occurrence, syntax) in [
                        ("Input-Record", 1, "cobol.storage.reference"),
                        ("Input-Record", 2, "cobol.storage.reference"),
                        ("FileLayout", 1, "cobol.end_program_name.reference"),
                    ] {
                        let span = fixture.span(written, occurrence);
                        let sites: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| site.source.span() == span)
                            .collect();
                        assert_eq!(sites.len(), 1);
                        assert_eq!(sites[0].role, OccurrenceRole::Reference);
                        assert_eq!(sites[0].syntax_kind, syntax);
                        assert_eq!(
                            sites[0].syntactic_text_hash,
                            content_hash(written.as_bytes())
                        );
                        assert!(matches!(
                            sites[0].target,
                            OccurrenceTarget::Unresolved { .. }
                        ));
                        assert!(document.skipped_regions.iter().any(|region| {
                            region.detail == "cobol-binding-target-unavailable"
                                && region.source.span() == span
                                && region.domain == FactDomain::Relations
                                && region.reason == SkippedRegionReason::UnsupportedConstruct
                        }));
                    }
                    // SELECT and file-I/O WORD operands do not establish binding facts.
                    for occurrence in [0, 2, 3, 4] {
                        assert!(document.occurrences.iter().all(|site| {
                            site.source.span() != fixture.span("Input-File", occurrence)
                        }));
                    }
                    let strings: Vec<_> = document
                        .occurrences
                        .iter()
                        .filter(|site| site.role == OccurrenceRole::StringEvidence)
                        .collect();
                    assert_eq!(strings.len(), 1);
                    assert_eq!(strings[0].source.span(), fixture.span("'records.dat'", 0));
                    assert_eq!(strings[0].syntax_kind, "cobol.string.string");
                    assert_eq!(
                        strings[0].syntactic_text_hash,
                        content_hash(b"'records.dat'")
                    );
                    assert!(matches!(
                        strings[0].target,
                        OccurrenceTarget::Unresolved { .. }
                    ));
                    assert_eq!(document.occurrences.len(), 7);
                    assert_eq!(document.skipped_regions.len(), 3);
                    assert!(document.relations.iter().all(|edge| matches!(
                        edge.predicate,
                        RelationPredicate::Contains
                            | RelationPredicate::Declares
                            | RelationPredicate::DefinesAt
                    )));
                }
            }
        }
    }
}

#[test]
fn symbolic_occurs_bounds_preserve_declared_and_unknown_source_evidence() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let fixture = fixture_at_eof(
                    &[
                        "identification division.",
                        "program-id. BoundProof.",
                        "data division.",
                        "working-storage section.",
                        "78 Lower-Limit value 1.",
                        "78 Upper-Limit value 3.",
                        "01 Known-Table occurs Lower-Limit to Upper-Limit times.",
                        "05 Known-Item pic x.",
                        "01 Unknown-Table occurs Missing-Low to Missing-High times.",
                        "05 Unknown-Item pic x.",
                        "01 Position pic 9.",
                        "procedure division.",
                        "move Known-Item(Position) to Unknown-Item(1).",
                        "move Known-Item(Position)(1:Position) to Known-Item(2).",
                        "end program BoundProof.",
                    ],
                    language,
                    ending,
                    final_newline,
                );
                let output = fixture.analyze(language);
                assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
                let document = output.document();
                assert!(document.diagnostics.is_empty());
                let program = definition(document, fixture.span("BoundProof", 0));
                assert_eq!(program.kind, EntityKind::Module);
                assert_eq!(
                    program.container,
                    Some(ContainerRef::File(fixture.snapshot.file()))
                );
                assert_eq!(document.entities.len(), 8);
                for (name, declaration) in [
                    ("Lower-Limit", "78 Lower-Limit value 1"),
                    ("Upper-Limit", "78 Upper-Limit value 3"),
                    (
                        "Known-Table",
                        "01 Known-Table occurs Lower-Limit to Upper-Limit times",
                    ),
                    ("Known-Item", "05 Known-Item pic x"),
                    (
                        "Unknown-Table",
                        "01 Unknown-Table occurs Missing-Low to Missing-High times",
                    ),
                    ("Unknown-Item", "05 Unknown-Item pic x"),
                    ("Position", "01 Position pic 9"),
                ] {
                    let span = fixture.span(name, 0);
                    let entity = definition(document, span);
                    assert_eq!(entity.canonical_name, name);
                    assert_eq!(entity.kind, EntityKind::Variable);
                    assert_eq!(entity.language, language);
                    assert_eq!(entity.container, Some(ContainerRef::Entity(program.id)));
                    let source = entity.evidence.source.as_ref().unwrap();
                    assert_eq!(source.span(), fixture.span(declaration, 0));
                    assert_eq!(source.content_hash(), fixture.source.content_hash());
                    assert_eq!(source.generation(), fixture.source.generation());
                    let site = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.role == OccurrenceRole::Definition && site.source.span() == span
                        })
                        .unwrap();
                    assert_eq!(site.syntax_kind, "cobol.data_name.definition");
                    assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
                    assert_eq!(site.provenance, entity.provenance);
                }
                let known = definition(document, fixture.span("Known-Table", 0));
                let unknown = definition(document, fixture.span("Unknown-Table", 0));
                for (name, occurrence, owner) in [
                    ("Lower-Limit", 1, known),
                    ("Upper-Limit", 1, known),
                    ("Missing-Low", 0, unknown),
                    ("Missing-High", 0, unknown),
                ] {
                    assert_unproven_reference(
                        document,
                        &fixture,
                        name,
                        occurrence,
                        "cobol.occurs_bound.reference",
                        Some(owner),
                    );
                }
                assert!(document.entities.iter().all(|entity| {
                    !matches!(
                        entity.canonical_name.as_str(),
                        "Missing-Low" | "Missing-High"
                    )
                }));
                for (name, occurrence) in [
                    ("Known-Item", 1),
                    ("Known-Item", 2),
                    ("Known-Item", 3),
                    ("Unknown-Item", 1),
                    ("Position", 1),
                    ("Position", 2),
                    ("Position", 3),
                ] {
                    assert_unproven_reference(
                        document,
                        &fixture,
                        name,
                        occurrence,
                        "cobol.storage.reference",
                        Some(program),
                    );
                }
                assert_unproven_reference(
                    document,
                    &fixture,
                    "BoundProof",
                    1,
                    "cobol.end_program_name.reference",
                    Some(program),
                );
                assert_eq!(document.occurrences.len(), 20);
                assert_eq!(document.skipped_regions.len(), 12);
                assert!(document.relations.iter().all(|edge| matches!(
                    edge.predicate,
                    RelationPredicate::Contains
                        | RelationPredicate::Declares
                        | RelationPredicate::DefinesAt
                )));
            }
        }
    }
}

#[test]
fn numeric_occurs_bounds_add_no_occurrences_and_keep_written_symbol_identity() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let symbolic = fixture_at_eof(
                    &[
                        "identification division.",
                        "program-id. NumericProof.",
                        "data division.",
                        "working-storage section.",
                        "78 Lower-Limit value 1.",
                        "78 Upper-Limit value 3.",
                        "01 Known-Table occurs Lower-Limit to Upper-Limit times.",
                        "05 Known-Item pic x.",
                        "01 Single-Table occurs Upper-Limit times.",
                        "05 Single-Item pic x.",
                        "procedure division.",
                        "move Known-Item(1) to Single-Item(2)(1:1).",
                        "end program NumericProof.",
                    ],
                    language,
                    ending,
                    final_newline,
                );
                let numeric = Fixture::from_text(
                    symbolic
                        .text
                        .replace("occurs Lower-Limit to Upper-Limit", "occurs 1 to 3")
                        .replace("occurs Upper-Limit", "occurs 3"),
                );
                let symbolic_output = symbolic.analyze(language);
                let numeric_output = numeric.analyze(language);
                let named = symbolic_output.document();
                let digits = numeric_output.document();
                assert_eq!(digits.entities.len(), 7);
                assert_eq!(
                    named
                        .entities
                        .iter()
                        .map(|entity| {
                            (
                                &entity.canonical_name,
                                entity.id,
                                entity.kind,
                                &entity.container,
                            )
                        })
                        .collect::<Vec<_>>(),
                    digits
                        .entities
                        .iter()
                        .map(|entity| {
                            (
                                &entity.canonical_name,
                                entity.id,
                                entity.kind,
                                &entity.container,
                            )
                        })
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    named
                        .occurrences
                        .iter()
                        .filter(|site| site.syntax_kind == "cobol.occurs_bound.reference")
                        .count(),
                    3
                );
                assert!(digits.occurrences.iter().all(|site| {
                    site.syntax_kind != "cobol.occurs_bound.reference"
                        && site.syntax_kind != "cobol.string.string"
                }));
                let program = definition(digits, numeric.span("NumericProof", 0));
                for name in ["Known-Item", "Single-Item"] {
                    assert_unproven_reference(
                        digits,
                        &numeric,
                        name,
                        1,
                        "cobol.storage.reference",
                        Some(program),
                    );
                }
                assert_unproven_reference(
                    digits,
                    &numeric,
                    "NumericProof",
                    1,
                    "cobol.end_program_name.reference",
                    Some(program),
                );
                assert_eq!(digits.occurrences.len(), 10);
                assert_eq!(named.occurrences.len(), digits.occurrences.len() + 3);
                assert_eq!(digits.skipped_regions.len(), 3);
                assert_eq!(
                    named.skipped_regions.len(),
                    digits.skipped_regions.len() + 3
                );
                assert_eq!(
                    numeric_output.report().coverage().status(),
                    CoverageStatus::Bounded
                );
                assert!(digits.relations.iter().all(|edge| matches!(
                    edge.predicate,
                    RelationPredicate::Contains
                        | RelationPredicate::Declares
                        | RelationPredicate::DefinesAt
                )));
            }
        }
    }
}

#[test]
fn symbolic_occurs_bounds_do_not_repair_unsupported_program_identity() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            for program_header in [None, Some("program-id. 'QuotedBounds'.")] {
                let mut lines = vec!["identification division."];
                lines.extend(program_header);
                lines.extend([
                    "data division.",
                    "working-storage section.",
                    "78 Written-Limit value 3.",
                    "01 Orphan-Table occurs Written-Limit to Absent-Limit times.",
                    "procedure division.",
                    "goback.",
                ]);
                let fixture = Fixture::new(&lines, language, ending);
                let output = fixture.analyze(language);
                let document = output.document();
                assert!(document.entities.is_empty());
                assert!(document.occurrences.iter().all(|site| {
                    site.role != OccurrenceRole::Definition
                        && matches!(site.target, OccurrenceTarget::Unresolved { .. })
                }));
                for (name, occurrence) in [("Written-Limit", 1), ("Absent-Limit", 0)] {
                    assert_unproven_reference(
                        document,
                        &fixture,
                        name,
                        occurrence,
                        "cobol.occurs_bound.reference",
                        None,
                    );
                }
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.detail == "stable-scope-identity-unavailable"
                        && gap.source.span()
                            == fixture.span(
                                "01 Orphan-Table occurs Written-Limit to Absent-Limit times",
                                0,
                            )
                }));
                assert!(document.relations.is_empty());
            }
        }
    }
}

#[test]
fn missing_or_recovered_file_descriptor_headers_never_invent_name_definitions() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            for header in ["fd .", "sd Broken-Name"] {
                let fixture = Fixture::new(
                    &[
                        "identification division.",
                        "program-id. Recovered.",
                        "data division.",
                        "file section.",
                        header,
                        "01 Safe-Record pic x.",
                        "procedure division.",
                        "goback.",
                        "end program Recovered.",
                    ],
                    language,
                    ending,
                );
                let limits = limits(IrLimits::default());
                let parsed = execute_parse(
                    provider().as_ref(),
                    &fixture.parse_request(language, &limits),
                    POLICY,
                    &deadline(),
                )
                .unwrap();
                assert_eq!(
                    parsed.report().coverage().status(),
                    CoverageStatus::Unknown,
                    "{language} {ending:?} {header:?}"
                );
                assert_eq!(parsed.diagnostics().len(), 1, "{header}");
                let diagnostic = &parsed.diagnostics()[0];
                assert_eq!(diagnostic.code().as_str(), "syntax-error-recovery");
                assert_eq!(diagnostic.severity(), DiagnosticSeverity::Warning);
                assert_eq!(diagnostic.coverage_effect(), CoverageStatus::Unknown);
                let recovery_source = diagnostic.source().unwrap();
                assert_eq!(recovery_source.span().file(), fixture.snapshot.file());
                assert!(recovery_source.span().start_byte() <= recovery_source.span().end_byte());
                assert!(recovery_source.span().end_byte() <= fixture.source.span().end_byte());
                assert_eq!(
                    recovery_source,
                    &SourceRef::new(
                        fixture.source.repository(),
                        fixture.source.generation(),
                        recovery_source.span(),
                        fixture.source.content_hash(),
                        None,
                    )
                );
                assert!(
                    parsed.facts().iter().all(|fact| fact.syntax_kind().as_str()
                        != "cobol.file_descriptor_name.definition"),
                    "{header}"
                );
                let output = fixture
                    .analyze_with(provider(), language, &limits, &deadline())
                    .unwrap();
                validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default())
                    .unwrap();
                assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
                let document = output.document();
                assert_eq!(document.diagnostics.len(), 1);
                let normalized = &document.diagnostics[0];
                assert_eq!(normalized.code, "syntax-error-recovery");
                assert_eq!(normalized.severity, DiagnosticSeverity::Warning);
                assert_eq!(normalized.coverage_effect, CoverageStatus::Unknown);
                assert_eq!(normalized.repository, fixture.source.repository());
                assert_eq!(normalized.generation, fixture.source.generation());
                assert_eq!(normalized.source.as_ref(), Some(recovery_source));
                assert_eq!(normalized.evidence.source.as_ref(), Some(recovery_source));
                assert!(normalized.evidence.derivation.is_empty());
                assert_eq!(normalized.provenance, document.provenance[0].id);
                assert_eq!(
                    normalized.id,
                    rootlight_ir::derive_diagnostic_record_id(normalized).unwrap()
                );
                let recovery_gaps: Vec<_> = document
                    .skipped_regions
                    .iter()
                    .filter(|gap| gap.detail == "syntax-error-recovery")
                    .collect();
                assert_eq!(recovery_gaps.len(), 1);
                let gap = recovery_gaps[0];
                assert_eq!(gap.domain, FactDomain::Diagnostics);
                assert_eq!(gap.reason, SkippedRegionReason::ParseError);
                assert_eq!(gap.repository, fixture.source.repository());
                assert_eq!(gap.generation, fixture.source.generation());
                assert_eq!(&gap.source, recovery_source);
                assert_eq!(gap.evidence.source.as_ref(), Some(recovery_source));
                assert!(gap.evidence.derivation.is_empty());
                assert_eq!(gap.provenance, normalized.provenance);
                assert_eq!(gap.id, rootlight_ir::derive_skipped_region_id(gap).unwrap());
                assert!(
                    output
                        .document()
                        .occurrences
                        .iter()
                        .all(|site| site.syntax_kind != "cobol.file_descriptor_name.definition"),
                    "{header}"
                );
                assert!(output.document().relations.iter().all(|edge| matches!(
                    edge.predicate,
                    RelationPredicate::Contains
                        | RelationPredicate::Declares
                        | RelationPredicate::DefinesAt
                )));
            }
        }
    }
}

#[test]
fn copy_using_storage_and_procedural_targets_remain_source_bound_and_unproven() {
    for language in ["cobol-fixed", "cobol-free"] {
        for ending in ["\n", "\r\n"] {
            let fixture = Fixture::new(
                &[
                    "identification division.",
                    "program-id. Uses.",
                    "data division.",
                    "linkage section.",
                    "01 Input-Value pic 9.",
                    "01 Output-Value pic 9.",
                    "copy Shared-Book.",
                    "procedure division using by reference Input-Value.",
                    "Work.",
                    "move Input-Value to Output-Value.",
                    "perform Work.",
                    "call 'Peer' using Input-Value.",
                    "call Output-Value using Input-Value.",
                    "goback.",
                    "end program Uses.",
                ],
                language,
                ending,
            );
            let output = fixture.analyze(language);
            let document = output.document();
            for (syntax, span, role, gap) in [
                (
                    "cobol.copy.import",
                    fixture.span("copy Shared-Book.", 0),
                    OccurrenceRole::ImportUse,
                    "cobol-copy-target-unavailable",
                ),
                (
                    "cobol.using_name.reference",
                    fixture.span("Input-Value", 1),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.storage.reference",
                    fixture.span("Input-Value", 2),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.storage.reference",
                    fixture.span("Output-Value", 1),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.perform_target.reference",
                    fixture.span("Work", 1),
                    OccurrenceRole::CallSite,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.call_target.reference",
                    fixture.span("'Peer'", 0),
                    OccurrenceRole::CallSite,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.storage.reference",
                    fixture.span("Input-Value", 3),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.call_target.reference",
                    fixture.span("Output-Value", 2),
                    OccurrenceRole::CallSite,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.storage.reference",
                    fixture.span("Output-Value", 2),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.storage.reference",
                    fixture.span("Input-Value", 4),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
                (
                    "cobol.end_program_name.reference",
                    fixture.span("Uses", 1),
                    OccurrenceRole::Reference,
                    "cobol-binding-target-unavailable",
                ),
            ] {
                let sites: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| site.syntax_kind == syntax && site.source.span() == span)
                    .collect();
                assert_eq!(sites.len(), 1, "{language}: {syntax}");
                assert_eq!(sites[0].role, role);
                assert!(matches!(
                    sites[0].target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|region| region.detail == gap && region.source.span() == span)
                );
                assert_eq!(sites[0].provenance, document.provenance[0].id);
            }
            let perform: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.source.span() == fixture.span("Work", 1))
                .collect();
            assert_eq!(perform.len(), 1);
            assert_eq!(perform[0].syntax_kind, "cobol.perform_target.reference");
            assert_eq!(perform[0].role, OccurrenceRole::CallSite);
            let dynamic: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.source.span() == fixture.span("Output-Value", 2))
                .collect();
            assert_eq!(dynamic.len(), 2);
            assert_ne!(dynamic[0].id, dynamic[1].id);
            assert_eq!(
                dynamic
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::Reference
                        && site.syntax_kind == "cobol.storage.reference")
                    .count(),
                1
            );
            assert_eq!(
                dynamic
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::CallSite
                        && site.syntax_kind == "cobol.call_target.reference")
                    .count(),
                1
            );
            let strings: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::StringEvidence)
                .collect();
            assert_eq!(strings.len(), 1);
            assert_eq!(strings[0].syntax_kind, "cobol.string.string");
            assert_eq!(strings[0].source.span(), fixture.span("'Peer'", 0));
            assert_eq!(strings[0].syntactic_text_hash, content_hash(b"'Peer'"));
            assert_eq!(strings[0].provenance, document.provenance[0].id);
            assert_eq!(
                document
                    .occurrences
                    .iter()
                    .filter(|site| matches!(
                        site.role,
                        OccurrenceRole::Reference
                            | OccurrenceRole::CallSite
                            | OccurrenceRole::ImportUse
                    ))
                    .count(),
                11
            );
            assert!(
                document
                    .occurrences
                    .iter()
                    .filter(|site| site.role != OccurrenceRole::Definition)
                    .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
            );
            assert!(document.relations.iter().all(|edge| matches!(
                edge.predicate,
                RelationPredicate::Contains
                    | RelationPredicate::Declares
                    | RelationPredicate::DefinesAt
            )));
            assert_eq!(
                document
                    .entities
                    .iter()
                    .filter(|entity| entity.kind == EntityKind::Parameter)
                    .count(),
                0
            );
        }
    }
}

#[test]
fn missing_or_quoted_program_identity_does_not_reparent_children_to_the_file() {
    for language in ["cobol-fixed", "cobol-free"] {
        for program_header in [None, Some("program-id. 'Quoted'.")] {
            let mut lines = vec!["identification division."];
            lines.extend(program_header);
            lines.extend([
                "data division.",
                "working-storage section.",
                "01 Orphan pic 9.",
                "procedure division.",
                "Work.",
                "goback.",
            ]);
            let fixture = Fixture::new(&lines, language, "\n");
            let output = fixture.analyze(language);
            assert!(output.document().entities.is_empty());
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "declaration-name-unavailable")
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "stable-scope-identity-unavailable"
                        && gap.source.span() == fixture.span("01 Orphan pic 9", 0))
            );
            assert!(
                output
                    .document()
                    .occurrences
                    .iter()
                    .all(|site| site.role != OccurrenceRole::Definition)
            );
        }
    }
}

#[test]
fn body_edits_preserve_written_symbols_and_duplicate_definition_witnesses() {
    for language in ["cobol-fixed", "cobol-free"] {
        let mut previous = None;
        for body in ["display 'first'.", "display 'second'."] {
            let fixture = Fixture::new(
                &[
                    "identification division.",
                    "program-id. Stable.",
                    "data division.",
                    "working-storage section.",
                    "01 Repeated pic 9.",
                    "01 Repeated pic 9.",
                    "procedure division.",
                    "Work.",
                    body,
                    "goback.",
                    "end program Stable.",
                ],
                language,
                "\r\n",
            );
            let output = fixture.analyze(language);
            let document = output.document();
            let first = definition(document, fixture.span("Repeated", 0));
            let second = definition(document, fixture.span("Repeated", 1));
            assert_eq!(first.id, second.id);
            let mut identities: Vec<_> = document
                .entities
                .iter()
                .map(|entity| (entity.canonical_name.clone(), entity.id))
                .collect();
            identities.sort();
            if let Some((expected, hash)) = &previous {
                assert_eq!(&identities, expected);
                assert_ne!(&fixture.source.content_hash(), hash);
            }
            previous = Some((identities, fixture.source.content_hash()));
        }
    }
}

#[test]
fn fixed_and_free_profiles_have_distinct_incremental_native_identity() {
    let fixture = Fixture::new(
        &[
            "identification division.",
            "program-id. Profiles.",
            "procedure division.",
            "goback.",
        ],
        "cobol-fixed",
        "\n",
    );
    let provider = provider();
    let limits = limits(IrLimits::default());
    let fixed = provider
        .execute_with_previous(
            &fixture.parse_request("cobol-fixed", &limits),
            None,
            &[],
            ParserSettings::new(512).unwrap(),
            POLICY,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fixed.report().coverage().status(), CoverageStatus::Complete);
    assert_eq!(fixed.reuse_key().native_family(), GrammarFamily::CobolFixed);
    let free = provider
        .execute_with_previous(
            &fixture.parse_request("cobol-free", &limits),
            fixed.previous(),
            &[],
            ParserSettings::new(512).unwrap(),
            POLICY,
            &deadline(),
        )
        .unwrap();
    assert_eq!(free.report().coverage().status(), CoverageStatus::Complete);
    assert_eq!(free.reuse_key().native_family(), GrammarFamily::CobolFree);
    assert_eq!(
        free.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::Language)
    );
    let reused = provider
        .execute_with_previous(
            &fixture.parse_request("cobol-free", &limits),
            free.previous(),
            &[],
            ParserSettings::new(512).unwrap(),
            POLICY,
            &deadline(),
        )
        .unwrap();
    assert!(matches!(reused.reuse_status(), ReuseStatus::Reused { .. }));
    assert_eq!(reused.output().facts(), free.output().facts());
}

#[test]
fn malformed_fragments_and_resource_or_cancellation_failures_do_not_claim_complete_projection() {
    for language in ["cobol-fixed", "cobol-free"] {
        let limits = limits(IrLimits::default());
        for fragment in ["01 Orphan pic 9.", "Work."] {
            let fixture = Fixture::new(&[fragment], language, "\n");
            let parsed = execute_parse(
                provider().as_ref(),
                &fixture.parse_request(language, &limits),
                POLICY,
                &deadline(),
            )
            .unwrap();
            assert_ne!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(!parsed.diagnostics().is_empty());
            assert!(
                parsed
                    .facts()
                    .iter()
                    .all(|fact| fact.kind() != SyntaxFactKind::Declaration
                        || fact.syntax_kind().as_str() != "cobol.program.declaration")
            );
        }
        let fixture = Fixture::new(
            &[
                "identification division.",
                "program-id. Bounded.",
                "data division.",
                "working-storage section.",
                "01 Item pic 9.",
                "procedure division.",
                "goback.",
            ],
            language,
            "\n",
        );
        let mut constrained = IrLimits::default();
        constrained.max_entities = 1;
        assert!(matches!(
            fixture.analyze_with(
                provider(),
                language,
                &self::limits(constrained),
                &deadline()
            ),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::Records,
                observed: 2,
                limit: 1,
            }))
        ));
        let cancellation = deadline();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                provider().as_ref(),
                &fixture.parse_request(language, &limits),
                POLICY,
                &cancellation
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            fixture.analyze_with(provider(), language, &limits, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
