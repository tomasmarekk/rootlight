//! Real Haskell native captures must preserve lexical binding ownership.
//! Equation grouping and written annotations are structural evidence, not proof
//! of type inference, application dispatch, or use-site name resolution.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseRequest, StreamLimits,
    execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, EntityRecord,
    ExtensionSupport, FactRef, IrLimits, LEXICAL_EXTENSION_NAMESPACE, NormalizedIrDocument,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, decode_lexical_evidence_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

const SOURCE: &str = concat!(
    "module Sample where\n",
    "pick :: Int -> Int\n",
    "pick 0 = 1\n",
    "pick value = value\n",
    "pick value = 2\n",
    "class Label a where\n",
    "  label :: a -> Int\n",
    "  label value = 3\n",
    "  label value = 4\n",
    "outer value =\n",
    "  let\n",
    "    pick :: Int -> Int\n",
    "    pick 0 = 5\n",
    "    pick item = item\n",
    "  in pick value\n",
    "shadow xs ys = [x | x <- xs, x <- ys]\n",
);

struct Fixture {
    _directory: TempDir,
    text: String,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(text: String) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(directory.path().join("Sample.hs"), &text).unwrap();
        let repository = derive_repository(b"haskell-native-lexical-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("Sample.hs")).unwrap(), 4096)
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

    fn span(&self, written: &str, index: usize) -> SourceSpan {
        let start = self.text.match_indices(written).nth(index).unwrap().0;
        SourceSpan::new(
            self.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + written.len()).unwrap(),
        )
        .unwrap()
    }

    fn head(&self, written: &str, index: usize, name: &str) -> SourceSpan {
        assert!(written.starts_with(name));
        let start = self.span(written, index).start_byte();
        SourceSpan::new(
            self.snapshot.file(),
            start,
            start + u64::try_from(name.len()).unwrap(),
        )
        .unwrap()
    }

    fn analyze(&self) -> AnalysisOutput {
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
            IrLimits::default(),
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
                "haskell-native-contract",
                "1.0",
                content_hash(b"haskell-native-configuration"),
            )
            .unwrap(),
            language.clone(),
            "tree-sitter-haskell-0.23.1",
            content_hash(b"haskell-native-binary"),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"haskell-native-build-context")),
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
                rootlight_ir::derive_occurrence_record_id(occurrence).unwrap(),
                "occurrence identity must include its final evidence"
            );
        }
        output
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
        panic!("written definition must have a structural owner");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == *symbol)
        .unwrap()
}

fn assert_equations(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    equations: &[&str],
) {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect();
    assert_eq!(
        definitions.len(),
        equations.len(),
        "{}",
        entity.canonical_name
    );
    for equation in equations {
        let declaration = fixture.span(equation, 0);
        let name = fixture.head(equation, 0, &entity.canonical_name);
        let definition = definitions
            .iter()
            .find(|site| site.source.span() == name)
            .unwrap();
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(entity.canonical_name.as_bytes())
        );
        assert_eq!(
            definition.evidence.source.as_ref().unwrap().span(),
            declaration
        );
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.object == RelationEndpoint::Entity(entity.id)
                && relation.evidence.source.as_ref().unwrap().span() == declaration
        }));
    }
}

fn assert_annotation(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    written: &str,
    index: usize,
    entity: &EntityRecord,
) {
    let name = fixture.head(written, index, &entity.canonical_name);
    let declarations: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Declaration && site.source.span() == name)
        .collect();
    assert_eq!(declarations.len(), 1);
    let annotation = declarations[0];
    assert_eq!(
        annotation.target,
        OccurrenceTarget::Resolved { symbol: entity.id }
    );
    assert_eq!(
        annotation.evidence.source.as_ref().unwrap().span(),
        fixture.span(written, index)
    );
    let envelopes: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| {
            extension.namespace == LEXICAL_EXTENSION_NAMESPACE
                && decode_lexical_evidence_envelope(extension)
                    .unwrap()
                    .subject()
                    == FactRef::Fact(annotation.id)
        })
        .collect();
    assert_eq!(envelopes.len(), 1);
    let evidence = decode_lexical_evidence_envelope(envelopes[0]).unwrap();
    assert_eq!(evidence.text(), written);
    assert_eq!(
        evidence.complete_text_hash(),
        content_hash(written.as_bytes())
    );
}

fn assert_clause_formals(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    owner: SymbolId,
    clauses: &[(&str, &str)],
) {
    let mut identities = BTreeSet::new();
    for (clause, name) in clauses {
        let start = fixture.span(clause, 0).start_byte()
            + u64::try_from(clause.find(name).unwrap()).unwrap();
        let span = SourceSpan::new(
            fixture.snapshot.file(),
            start,
            start + u64::try_from(name.len()).unwrap(),
        )
        .unwrap();
        let parameter = defined_entity(document, span);
        assert_eq!(parameter.kind, EntityKind::Parameter);
        assert_eq!(parameter.canonical_name, *name);
        assert_eq!(parameter.container, Some(ContainerRef::Entity(owner)));
        assert!(
            identities.insert(parameter.id),
            "formals must belong to separate clauses: {clause}, {parameter:?}"
        );
    }
}

fn assert_unproven_uses(fixture: &Fixture, document: &NormalizedIrDocument) {
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::Reference
                    | OccurrenceRole::CallSite
                    | OccurrenceRole::TypeUse
                    | OccurrenceRole::ImportUse
            )
        })
        .collect();
    assert!(!uses.is_empty());
    for site in uses {
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{site:?}"
        );
        let span = site.source.span();
        let text = fixture
            .text
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        let spelling = if site.role == OccurrenceRole::CallSite {
            // Call provenance covers the application; its spelling is the
            // native terminal callee, not the argument-bearing source range.
            assert_eq!(text, "pick value");
            "pick"
        } else {
            text
        };
        assert_eq!(site.syntactic_text_hash, content_hash(spelling.as_bytes()));
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.content_hash(), fixture.source.content_hash());
    }
    assert!(document.relations.iter().all(|relation| matches!(
        relation.predicate,
        RelationPredicate::Contains | RelationPredicate::Declares | RelationPredicate::DefinesAt
    )));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "haskell-binding-target-unavailable" })
    );
}

#[test]
fn native_equations_annotations_and_generators_preserve_lexical_owners() {
    for newline in ["\n", "\r\n"] {
        let fixture = Fixture::new(SOURCE.replace('\n', newline));
        let output = fixture.analyze();
        let document = output.document();
        let top = defined_entity(document, fixture.head("pick 0 = 1", 0, "pick"));
        let outer = defined_entity(document, fixture.head("outer value", 0, "outer"));
        let local = defined_entity(document, fixture.head("pick 0 = 5", 0, "pick"));
        let class = defined_entity(document, fixture.span("Label", 0));
        let method = defined_entity(document, fixture.head("label value = 3", 0, "label"));
        assert_eq!(top.kind, EntityKind::Function);
        assert_eq!(local.kind, EntityKind::Function);
        assert_eq!(class.kind, EntityKind::Trait);
        assert_eq!(method.kind, EntityKind::Method);
        assert_ne!(top.id, local.id);
        assert_eq!(top.container, outer.container);
        assert_eq!(local.container, Some(ContainerRef::Entity(outer.id)));
        assert_eq!(method.container, Some(ContainerRef::Entity(class.id)));
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| {
                    entity.canonical_name == "pick" && entity.kind == EntityKind::Function
                })
                .count(),
            2
        );
        assert_equations(
            &fixture,
            document,
            top,
            &["pick 0 = 1", "pick value = value", "pick value = 2"],
        );
        assert_equations(
            &fixture,
            document,
            local,
            &["pick 0 = 5", "pick item = item"],
        );
        assert_equations(
            &fixture,
            document,
            method,
            &["label value = 3", "label value = 4"],
        );
        assert_annotation(&fixture, document, "pick :: Int -> Int", 0, top);
        assert_annotation(&fixture, document, "pick :: Int -> Int", 1, local);
        assert_annotation(&fixture, document, "label :: a -> Int", 0, method);
        assert_clause_formals(
            &fixture,
            document,
            top.id,
            &[("pick value = value", "value"), ("pick value = 2", "value")],
        );
        assert_clause_formals(
            &fixture,
            document,
            method.id,
            &[("label value = 3", "value"), ("label value = 4", "value")],
        );
        let shadow = defined_entity(document, fixture.head("shadow xs ys", 0, "shadow"));
        let first = defined_entity(document, fixture.head("x <- xs", 0, "x"));
        let second = defined_entity(document, fixture.head("x <- ys", 0, "x"));
        assert_ne!(first.id, second.id);
        for variable in [first, second] {
            assert_eq!(variable.kind, EntityKind::Variable);
            assert_eq!(variable.canonical_name, "x");
            assert_eq!(variable.container, Some(ContainerRef::Entity(shadow.id)));
        }
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "x"
                })
                .count(),
            2
        );
        assert_unproven_uses(&fixture, document);
    }
}
