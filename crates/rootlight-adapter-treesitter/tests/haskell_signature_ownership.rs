//! Native Haskell headers retain their exact lexical owners.
//! Child scope and field signatures are not alternative headers of the enclosing
//! callable; source syntax does not establish use-site dispatch or inferred types.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseOutput, ParseRequest,
    StreamLimits, SyntaxFactKind, execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, EntityRecord, ExtensionSupport,
    FactRef, IrLimits, LEXICAL_EXTENSION_NAMESPACE, NormalizedIrDocument, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, RelationPredicate, SourceRef, SourceSpan,
    decode_lexical_evidence_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

struct Fixture {
    _directory: TempDir,
    text: String,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(text: String) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(directory.path().join("Example.hs"), &text).unwrap();
        let repository = derive_repository(b"haskell-header-ownership-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("Example.hs")).unwrap(), 4096)
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([13; 20]),
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

    fn span(&self, text: &str, ordinal: usize) -> SourceSpan {
        let start = self.text.match_indices(text).nth(ordinal).unwrap().0;
        SourceSpan::new(
            self.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + text.len()).unwrap(),
        )
        .unwrap()
    }

    fn analyze(&self, ir: IrLimits) -> (ParseOutput, AnalysisOutput) {
        let provider = Arc::new(
            TreeSitterProvider::new(
                RuntimeConfig::new(
                    4096,
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
        );
        let language = LanguageId::new("haskell").unwrap();
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
        let limits = AnalysisLimits::new(
            4096,
            4096,
            128,
            32,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap();
        let parse = ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language.clone(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let syntax = execute_parse(
            provider.as_ref(),
            &parse,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            syntax.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(
            syntax.diagnostics().is_empty(),
            "{:?}",
            syntax.diagnostics()
        );
        let analyzer = TreeSitterAnalyzer::new(
            provider,
            ProducerIdentity::new(
                "haskell-header-contract",
                "1.0",
                content_hash(b"header-config"),
            )
            .unwrap(),
            language.clone(),
            "tree-sitter-haskell-0.23.1",
            content_hash(b"header-binary"),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"header-build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        for occurrence in &output.document().occurrences {
            assert_eq!(
                occurrence.id,
                rootlight_ir::derive_occurrence_record_id(occurrence).unwrap()
            );
        }
        assert!(output.document().relations.iter().all(|relation| matches!(
            relation.predicate,
            RelationPredicate::Contains
                | RelationPredicate::Declares
                | RelationPredicate::DefinesAt
        )));
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| matches!(
                    site.role,
                    OccurrenceRole::Reference
                        | OccurrenceRole::CallSite
                        | OccurrenceRole::TypeUse
                        | OccurrenceRole::ImportUse
                ))
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        (syntax, output)
    }
}

fn defined_entity(document: &NormalizedIrDocument, span: SourceSpan) -> &EntityRecord {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition && site.source.span() == span)
        .collect();
    assert_eq!(definitions.len(), 1, "definition at {span:?}");
    let OccurrenceTarget::Resolved { symbol } = &definitions[0].target else {
        panic!("definition has no source owner");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == *symbol)
        .unwrap()
}

fn assert_signature(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    written: &str,
) {
    let signatures: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| {
            extension.namespace == LEXICAL_EXTENSION_NAMESPACE
                && decode_lexical_evidence_envelope(extension)
                    .unwrap()
                    .subject()
                    == FactRef::Entity(entity.id)
        })
        .collect();
    assert_eq!(signatures.len(), 1, "{}", entity.canonical_name);
    let evidence = decode_lexical_evidence_envelope(signatures[0]).unwrap();
    assert_eq!(evidence.text(), written);
    assert_eq!(
        evidence.complete_text_hash(),
        content_hash(written.as_bytes())
    );
    let source = signatures[0].evidence.source.as_ref().unwrap();
    assert_eq!(source.span(), fixture.span(written, 0));
    assert_eq!(source.content_hash(), fixture.source.content_hash());
    assert_eq!(source.generation(), fixture.source.generation());
}

fn assert_no_structural_header_gaps(document: &NormalizedIrDocument) {
    assert!(
        !document.skipped_regions.iter().any(|gap| matches!(
            gap.detail.as_str(),
            "declaration-name-unavailable" | "signature-capture-unavailable"
        )),
        "{:?}",
        document.skipped_regions
    );
}

#[test]
fn written_dotted_module_and_exports_retain_exact_definition() {
    for newline in ["\n", "\r\n"] {
        let header = "module Example.Deep.Path (Item(..), identity) where";
        let fixture = Fixture::new(
            format!("{header}\ndata Item = Item\nidentity value = value\n").replace('\n', newline),
        );
        let (_, output) = fixture.analyze(IrLimits::default());
        let document = output.document();
        let module = defined_entity(document, fixture.span("Example.Deep.Path", 0));
        assert_eq!(module.kind, EntityKind::Module);
        assert_eq!(module.canonical_name, "Example.Deep.Path");
        assert_eq!(
            module.evidence.source.as_ref().unwrap().span(),
            fixture.span(header, 0)
        );
        assert_no_structural_header_gaps(document);
        let changed = Fixture::new(
            fixture
                .text
                .replace("identity value = value", "identity value = 2"),
        );
        let (_, changed_output) = changed.analyze(IrLimits::default());
        assert_eq!(
            module.id,
            defined_entity(
                changed_output.document(),
                changed.span("Example.Deep.Path", 0)
            )
            .id
        );
    }
}

#[test]
fn lambda_headers_remain_scope_owned_without_poisoning_equation_headers() {
    for newline in ["\n", "\r\n"] {
        let fixture = Fixture::new("module Example where\nmapped = map (\\item -> item) values\nrun values = map (\\value -> value) values\n".replace('\n', newline));
        let (syntax, output) = fixture.analyze(IrLimits::default());
        let document = output.document();
        assert_signature(
            &fixture,
            document,
            defined_entity(document, fixture.span("mapped", 0)),
            "mapped ",
        );
        assert_signature(
            &fixture,
            document,
            defined_entity(document, fixture.span("run", 0)),
            "run values ",
        );
        let headers: Vec<_> = syntax
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "haskell.lambda_header.signature")
            .collect();
        assert_eq!(headers.len(), 2);
        for header in headers {
            let scope = syntax
                .facts()
                .iter()
                .find(|fact| Some(fact.local_id()) == header.parent())
                .unwrap();
            assert_eq!(scope.kind(), SyntaxFactKind::Scope);
            assert_eq!(scope.syntax_kind().as_str(), "haskell.lambda.scope");
            assert!(
                scope.span().start_byte() <= header.span().start_byte()
                    && header.span().end_byte() <= scope.span().end_byte()
            );
        }
        assert_no_structural_header_gaps(document);
        let changed = Fixture::new(fixture.text.replace("\\item -> item", "\\item -> 2"));
        let (_, changed_output) = changed.analyze(IrLimits::default());
        for name in ["mapped", "run"] {
            assert_eq!(
                defined_entity(document, fixture.span(name, 0)).id,
                defined_entity(changed_output.document(), changed.span(name, 0)).id
            );
        }
    }
}

#[test]
fn pattern_variable_leaves_do_not_require_equation_headers() {
    let fixture = Fixture::new(
        concat!(
            "module Example where\n",
            "run values = do\n",
            "  result <- action\n",
            "  let (left, right) = pair\n",
            "  pure [item | item <- values]\n",
            "  case result of\n",
            "    Just found -> pure found\n",
        )
        .to_owned(),
    );
    let (syntax, output) = fixture.analyze(IrLimits::default());
    let document = output.document();
    for (name, ordinal) in [
        ("result", 0),
        ("left", 0),
        ("right", 0),
        ("item", 1),
        ("found", 0),
    ] {
        let span = fixture.span(name, ordinal);
        let entity = defined_entity(document, span);
        assert_eq!(entity.kind, EntityKind::Variable);
        assert!(
            syntax
                .facts()
                .iter()
                .any(|fact| fact.kind() == SyntaxFactKind::Declaration && fact.span() == span)
        );
        assert!(!document.extensions.iter().any(|extension| {
            extension.namespace == LEXICAL_EXTENSION_NAMESPACE
                && decode_lexical_evidence_envelope(extension)
                    .unwrap()
                    .subject()
                    == FactRef::Entity(entity.id)
        }));
    }
    assert_no_structural_header_gaps(document);
}

#[test]
fn record_constructor_and_multi_name_fields_keep_distinct_exact_signatures() {
    for newline in ["\n", "\r\n"] {
        let fixture = Fixture::new("module Example where\ndata Record = Record { first, second :: Int, label :: String }\n".replace('\n', newline));
        let (_, output) = fixture.analyze(IrLimits::default());
        let document = output.document();
        let constructor = defined_entity(document, fixture.span("Record", 1));
        assert_eq!(constructor.kind, EntityKind::Constructor);
        assert_signature(
            &fixture,
            document,
            constructor,
            "Record { first, second :: Int, label :: String }",
        );
        for (name, written) in [
            ("first", "first, second :: Int"),
            ("second", "first, second :: Int"),
            ("label", "label :: String"),
        ] {
            let field = defined_entity(document, fixture.span(name, 0));
            assert_eq!(field.kind, EntityKind::Field);
            assert_signature(&fixture, document, field, written);
        }
        assert_no_structural_header_gaps(document);
    }
}

#[test]
fn multi_name_field_evidence_obeys_optional_output_budget_without_dropping_definitions() {
    let fixture = Fixture::new(
        "module Example where\ndata Record = Record { first, second, third :: Int }\n".to_owned(),
    );
    let (_, full) = fixture.analyze(IrLimits::default());
    for name in ["first", "second", "third"] {
        assert_signature(
            &fixture,
            full.document(),
            defined_entity(full.document(), fixture.span(name, 0)),
            "first, second, third :: Int",
        );
    }
    let mut limits = IrLimits::default();
    limits.max_extensions = full.document().entities.len() + 1;
    let maximum_extensions = limits.max_extensions;
    let (_, bounded) = fixture.analyze(limits);
    assert_eq!(bounded.document().extensions.len(), maximum_extensions);
    assert_eq!(
        bounded.document().entities.len(),
        full.document().entities.len()
    );
    for name in ["first", "second", "third"] {
        assert_eq!(
            defined_entity(bounded.document(), fixture.span(name, 0)).id,
            defined_entity(full.document(), fixture.span(name, 0)).id
        );
    }
    assert!(
        bounded
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "lexical-evidence-resource-limit"
                && gap.source.span() == fixture.span("first, second, third :: Int", 0))
    );
}
