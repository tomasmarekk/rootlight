//! Physical Fortran source retains authored structure and explicit CALL sites.
//! Template expansion, USE association and use-site binding remain scoped gaps;
//! parenthesized applications do not imply calls or resolved targets.

use std::{
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
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, EntityRecord,
    ExtensionSupport, FactDomain, FactRef, IrLimits, LEXICAL_EXTENSION_NAMESPACE,
    LexicalEvidenceKind, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, RelationPredicate,
    SkippedRegionReason, SourceRef, SourceSpan, decode_lexical_evidence_envelope,
    validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const POLICY: MemoryAdmissionPolicy = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn limits() -> AnalysisLimits {
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
        IrLimits::default(),
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
    fn new(text: &str, suffix: &str) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let path = format!("Sample{suffix}");
        fs::write(directory.path().join(&path), text).unwrap();
        let repository = derive_repository(b"fortran-physical-structural-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(&path)).unwrap(), 8192)
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
            text: text.to_owned(),
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

    fn analyze(&self, clean_parse: bool) -> AnalysisOutput {
        let parser = Arc::new(
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
        );
        let limits = limits();
        let language = LanguageId::new("fortran").unwrap();
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language.clone(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let parsed = execute_parse(parser.as_ref(), &request, POLICY, &deadline()).unwrap();
        if clean_parse {
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(
                parsed.diagnostics().is_empty(),
                "{:?}",
                parsed.diagnostics()
            );
        }
        let analyzer = TreeSitterAnalyzer::new(
            parser,
            rootlight_ir::ProducerIdentity::new(
                "fortran-structural-contract",
                "1.0",
                content_hash(b"fortran-contract"),
            )
            .unwrap(),
            language.clone(),
            "tree-sitter-fortran-reviewed",
            content_hash(b"fortran"),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"fortran-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            POLICY,
            &deadline(),
        )
        .unwrap();
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        for site in &output.document().occurrences {
            assert_eq!(
                site.id,
                rootlight_ir::derive_occurrence_record_id(site).unwrap()
            );
            assert_eq!(site.source.content_hash(), self.source.content_hash());
            assert_eq!(site.source.generation(), self.source.generation());
            let range = usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap();
            if site.role != OccurrenceRole::CallSite {
                assert_eq!(
                    site.syntactic_text_hash,
                    content_hash(&self.text.as_bytes()[range])
                );
            }
        }
        output
    }
}

fn definition(document: &NormalizedIrDocument, span: SourceSpan) -> &EntityRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition && site.source.span() == span)
        .collect();
    assert_eq!(sites.len(), 1, "definition at {span:?}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition needs its source-owned entity");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap()
}

fn assert_signature(
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    fixture: &Fixture,
    header: &str,
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
    let span = headers[0].0.evidence.source.as_ref().unwrap().span();
    let text = &fixture.text
        [usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()];
    assert_eq!(headers[0].1.text(), text);
    assert_eq!(text.trim_end(), header.trim_end());
    assert_eq!(
        headers[0].1.complete_text_hash(),
        content_hash(text.as_bytes())
    );
}

fn assert_unresolved(document: &NormalizedIrDocument, span: SourceSpan, syntax: &str, gap: &str) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.source.span() == span && site.syntax_kind == syntax)
        .collect();
    assert_eq!(sites.len(), 1, "{syntax} at {span:?}");
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|region| region.source.span() == span
                && region.domain == FactDomain::Relations
                && region.detail == gap
                && region.reason == SkippedRegionReason::UnsupportedConstruct)
    );
}

#[test]
fn native_query_steps_keep_headers_and_optional_gap_markers_independent() {
    let language = tree_sitter_fortran::LANGUAGE.into();
    let source = b"module Sample\ncontains\nsubroutine Work(Value)\ninteger :: Value\nend subroutine Work\nend module Sample\n";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let query = tree_sitter::Query::new(&language, include_str!("../queries/fortran.scm")).unwrap();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());
    let mut module_roles = std::collections::BTreeSet::new();
    while let Some(item) = tree_sitter::StreamingIterator::next(&mut matches) {
        let mut counts = std::collections::BTreeMap::new();
        for capture in item.captures {
            *counts.entry(capture.node.id()).or_insert(0_usize) += 1;
            if capture.node.kind() == "module" {
                module_roles.insert(query.capture_names()[usize::try_from(capture.index).unwrap()]);
            }
        }
        assert!(counts.values().all(|count| *count <= 3));
    }
    assert_eq!(
        module_roles,
        ["module", "definition", "signature", "scope", "expression"]
            .into_iter()
            .collect()
    );
}

#[test]
fn written_modules_procedures_and_components_preserve_exact_ownership() {
    let fixture = Fixture::new(
        "module Geometry\n  type :: Point\n    integer :: X, Y = 2\n  end type Point\ncontains\n  function Measure(Value) result(Answer)\n    integer :: Value, Answer\n    Answer = Value\n  end function Measure\n  subroutine Store(Value)\n    integer :: Value\n    call Sink(Value)\n  end subroutine Store\nend module Geometry\n",
        ".f90",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let module = definition(document, fixture.span("Geometry", 0));
    let point = definition(document, fixture.span("Point", 0));
    assert_eq!(module.kind, EntityKind::Module);
    assert_signature(document, module, &fixture, "module Geometry");
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "signature-capture-unavailable"
                && gap.source.span() == fixture.source.span())
    );
    assert_eq!(point.kind, EntityKind::Struct);
    assert_eq!(point.container, Some(ContainerRef::Entity(module.id)));
    for name in ["X", "Y"] {
        let field = definition(document, fixture.span(name, 0));
        assert_eq!(field.kind, EntityKind::Field);
        assert_eq!(field.container, Some(ContainerRef::Entity(point.id)));
    }
    for name in ["Measure", "Store"] {
        let procedure = definition(document, fixture.span(name, 0));
        assert_eq!(procedure.kind, EntityKind::Function);
        assert_eq!(procedure.canonical_name, name);
        assert_eq!(procedure.display_name, name);
        assert_eq!(procedure.container, Some(ContainerRef::Entity(module.id)));
    }
    let measure = definition(document, fixture.span("Measure", 0));
    let parameter = definition(document, fixture.span("Value", 0));
    assert_eq!(parameter.kind, EntityKind::Parameter);
    assert_eq!(parameter.container, Some(ContainerRef::Entity(measure.id)));
    assert_signature(
        document,
        measure,
        &fixture,
        "function Measure(Value) result(Answer)",
    );
    assert_unresolved(
        document,
        fixture.span("Value", 2),
        "fortran.identifier.reference",
        "fortran-binding-target-unavailable",
    );
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].syntax_kind, "fortran.explicit_subroutine.call");
    assert_eq!(calls[0].source.span(), fixture.span("call Sink(Value)", 0));
    assert_eq!(calls[0].syntactic_text_hash, content_hash(b"Sink"));
    assert!(matches!(
        calls[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn declarator_targets_exclude_initializers_and_array_applications_are_not_calls() {
    let fixture = Fixture::new(
        "module Data\n  integer :: First = Seed, Second(3), Third[*]\ncontains\n  subroutine Update(Index)\n    integer :: Index, Result\n    Result = Second(Index)\n    call Sink(Result)\n  end subroutine Update\nend module Data\n",
        ".f95",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    for name in ["First", "Second", "Third"] {
        assert_eq!(
            definition(document, fixture.span(name, 0)).kind,
            EntityKind::Variable
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Seed")
    );
    assert_unresolved(
        document,
        fixture.span("Second", 1),
        "fortran.application.reference",
        "fortran-call-or-index-target-unavailable",
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .count(),
        1
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn continued_headers_keep_physical_crlf_spans() {
    let fixture = Fixture::new(
        "module Mixed\r\ncontains\r\n  subroutine &\r\n    & Measure(Value)\r\n    integer :: Value\r\n  end subroutine Measure\r\nend module Mixed\r\n",
        ".f08",
    );
    let output = fixture.analyze(true);
    let procedure = definition(output.document(), fixture.span("Measure", 0));
    assert_eq!(procedure.kind, EntityKind::Function);
    assert_signature(
        output.document(),
        procedure,
        &fixture,
        "subroutine &\r\n    & Measure(Value)",
    );
}

#[test]
fn physical_templates_keep_literal_names_without_partial_interpolated_identity() {
    let fixture = Fixture::new(
        "#:set KIND = 'integer'\nmodule Literal\ncontains\n  subroutine Stable(Value)\n    integer :: Value\n  end subroutine Stable\n  subroutine generated_${KIND}$(Value)\n    use Hidden\n    integer :: Value\n    call Leaked(Value)\n  end subroutine generated_${KIND}$\nend module Literal\n",
        ".fypp",
    );
    let output = fixture.analyze(false);
    let document = output.document();
    assert_eq!(
        definition(document, fixture.span("Literal", 0)).kind,
        EntityKind::Module
    );
    assert_eq!(
        definition(document, fixture.span("Stable", 0)).kind,
        EntityKind::Function
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name.starts_with("generated"))
    );
    assert!(!document.occurrences.iter().any(
        |site| site.role == OccurrenceRole::CallSite || site.role == OccurrenceRole::ImportUse
    ));
    let rejected_start = fixture.span("generated_${KIND}$", 0).start_byte();
    let rejected_end = fixture.span("generated_${KIND}$", 1).end_byte();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() >= rejected_start
                && site.source.span().end_byte() <= rejected_end)
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|region| region.domain == domain
                    && region.source.span() == fixture.source.span()
                    && region.detail == "fortran-template-expansion-unavailable"
                    && region.reason == SkippedRegionReason::UnsupportedConstruct)
        );
    }
}

#[test]
fn use_association_and_separate_procedure_linkage_remain_unproved() {
    let fixture = Fixture::new(
        "module Parent\n  use Foreign, only: Local => Remote\n  interface\n    module subroutine Work(Value)\n      integer :: Value\n    end subroutine Work\n  end interface\nend module Parent\nsubmodule (Parent) Child\ncontains\n  module procedure Work\n  end procedure Work\nend submodule Child\n",
        ".f03",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    assert_eq!(
        definition(document, fixture.span("Work", 0)).kind,
        EntityKind::Function
    );
    assert_eq!(
        definition(document, fixture.span("Work", 2)).kind,
        EntityKind::Function
    );
    assert!(document.skipped_regions.iter().any(|region| region.detail
        == "fortran-use-association-target-unavailable"
        && region.domain == FactDomain::Relations));
    assert!(document.skipped_regions.iter().any(|region| region.detail
        == "fortran-procedure-linkage-unavailable"
        && region.domain == FactDomain::Relations));
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn type_bound_calls_retain_physical_evidence_without_a_guessed_terminal_target() {
    let fixture = Fixture::new(
        "module Objects\ncontains\n  subroutine Run(Object, Value)\n    class(*) :: Object\n    integer :: Value\n    call Object%Apply(Value)\n  end subroutine Run\nend module Objects\n",
        ".f90",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    let call = calls[0];
    assert_eq!(call.syntax_kind, "fortran.unavailable_name.call");
    assert_eq!(
        call.source.span(),
        fixture.span("call Object%Apply(Value)", 0)
    );
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    for (domain, detail) in [
        (
            FactDomain::Occurrences,
            "fortran-reference-name-unavailable",
        ),
        (FactDomain::Relations, "fortran-call-target-unavailable"),
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == call.source.span()
                    && gap.domain == domain
                    && gap.detail == detail
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct)
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}
