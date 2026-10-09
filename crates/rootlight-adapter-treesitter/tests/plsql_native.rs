//! Written PL/SQL declarations retain exact source identities and native owners.
//! Applications, dynamic SQL and specification/body linkage remain unproven;
//! parser recovery must preserve valid headers without leaking invalid owners.

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
    fn new(text: &str) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let path = "Sample.pkb";
        fs::write(directory.path().join(path), text).unwrap();
        let repository = derive_repository(b"plsql-physical-structural-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 8192)
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
            text: text.to_owned(),
            snapshot,
            source,
        }
    }

    fn span(&self, written: &str, occurrence: usize) -> SourceSpan {
        let identifier = |byte: Option<&u8>| {
            byte.is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'#') || *byte >= 128
            })
        };
        let start = self
            .text
            .match_indices(written)
            .filter(|(at, _)| {
                !identifier(
                    at.checked_sub(1)
                        .and_then(|before| self.text.as_bytes().get(before)),
                ) && !identifier(self.text.as_bytes().get(at + written.len()))
            })
            .nth(occurrence)
            .unwrap()
            .0;
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
        let language = LanguageId::new("plsql").unwrap();
        let parse = ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language.clone(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let parsed = execute_parse(parser.as_ref(), &parse, POLICY, &deadline()).unwrap();
        if clean_parse {
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete,
                "{:?}",
                parsed.diagnostics()
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
                "plsql-structural-contract",
                "1.0",
                content_hash(b"plsql-contract"),
            )
            .unwrap(),
            language.clone(),
            "tree-sitter-plsql-reviewed",
            content_hash(b"plsql"),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"plsql-context")),
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
            assert_eq!(site.source.repository(), self.source.repository());
            assert_eq!(site.source.generation(), self.source.generation());
            assert_eq!(site.source.content_hash(), self.source.content_hash());
            let range = usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap();
            assert_eq!(
                site.syntactic_text_hash,
                content_hash(&self.text.as_bytes()[range])
            );
            let evidence = site.evidence.source.as_ref().unwrap();
            assert_eq!(evidence.repository(), site.source.repository());
            assert_eq!(evidence.generation(), site.source.generation());
            assert_eq!(evidence.content_hash(), site.source.content_hash());
            assert_eq!(evidence.span().file(), site.source.span().file());
            assert!(evidence.span().start_byte() <= site.source.span().start_byte());
            assert!(evidence.span().end_byte() >= site.source.span().end_byte());
            assert!(site.evidence.derivation.is_empty());
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
        panic!("written definition needs its source-owned entity");
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
    fixture: &Fixture,
    written: &str,
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
    assert_eq!(headers.len(), 1, "header for {}", entity.canonical_name);
    let source = headers[0].0.evidence.source.as_ref().unwrap();
    let text = &fixture.text[usize::try_from(source.span().start_byte()).unwrap()
        ..usize::try_from(source.span().end_byte()).unwrap()];
    assert_eq!(headers[0].1.text(), text);
    assert_eq!(text.trim_end(), written.trim_end());
    assert_eq!(
        headers[0].1.complete_text_hash(),
        content_hash(text.as_bytes())
    );
    assert_eq!(source.content_hash(), fixture.source.content_hash());
    assert_eq!(source.generation(), fixture.source.generation());
}

fn assert_unresolved(document: &NormalizedIrDocument, span: SourceSpan, syntax: &str, gap: &str) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.source.span() == span && site.syntax_kind == syntax)
        .collect();
    assert_eq!(sites.len(), 1, "{syntax} at {span:?}");
    assert_eq!(sites[0].role, OccurrenceRole::Reference);
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|region| region.source == sites[0].source
                && region.domain == FactDomain::Relations
                && region.detail == gap
                && region.reason == SkippedRegionReason::UnsupportedConstruct)
    );
}

#[test]
fn native_query_steps_keep_identity_and_optional_uncertainty_separate() {
    let language = tree_sitter_plsql::LANGUAGE.into();
    let source = b"create or replace package Sample as\nprocedure Adjust(Value in number);\nend Sample;\n/\n";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let query = tree_sitter::Query::new(&language, include_str!("../queries/plsql.scm")).unwrap();
    assert!(
        !query
            .capture_names()
            .iter()
            .any(|name| name.contains("call"))
    );
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());
    let mut package_roles = std::collections::BTreeSet::new();
    while let Some(item) = tree_sitter::StreamingIterator::next(&mut matches) {
        let mut counts = std::collections::BTreeMap::new();
        for capture in item.captures {
            *counts.entry(capture.node.id()).or_insert(0_usize) += 1;
            if capture.node.kind() == "create_package" {
                package_roles
                    .insert(query.capture_names()[usize::try_from(capture.index).unwrap()]);
            }
        }
        assert!(counts.values().all(|count| *count <= 3));
    }
    assert_eq!(
        package_roles,
        [
            "declaration",
            "definition",
            "signature",
            "scope",
            "expression"
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn packages_nested_procedures_records_and_formals_preserve_written_ownership() {
    let lines = [
        "-- λ source",
        "create or replace package Geometry as",
        "  procedure Adjust(Value in number);",
        "  function Compute(Value in number) return number;",
        "end geometry;",
        "/",
        "create or replace package body Geometry as",
        "  type RowValue is record (FieldValue number, TextValue varchar2(20));",
        "  procedure Adjust(Value in number) as",
        "    LocalValue number;",
        "  begin",
        "    LocalValue := Value;",
        "  end adjust;",
        "  function Compute(Value in number) return number as",
        "  begin",
        "    return Value;",
        "  end compute;",
        "end geometry;",
        "/",
    ];
    for ending in ["\n", "\r\n"] {
        let fixture = Fixture::new(&format!("{}{ending}", lines.join(ending)));
        let output = fixture.analyze(true);
        let document = output.document();
        let spec = definition(document, fixture.span("Geometry", 0));
        let body = definition(document, fixture.span("Geometry", 1));
        assert_eq!(spec.kind, EntityKind::Module);
        assert_eq!(body.kind, EntityKind::Module);
        assert_ne!(spec.id, body.id);
        assert_header(
            document,
            spec,
            &fixture,
            "create or replace package Geometry as",
        );
        assert_header(
            document,
            body,
            &fixture,
            "create or replace package body Geometry as",
        );
        let prototype = definition(document, fixture.span("Adjust", 0));
        let implementation = definition(document, fixture.span("Adjust", 1));
        assert_ne!(prototype.id, implementation.id);
        assert_eq!(implementation.kind, EntityKind::Function);
        assert_eq!(prototype.container, Some(ContainerRef::Entity(spec.id)));
        assert_eq!(
            implementation.container,
            Some(ContainerRef::Entity(body.id))
        );
        assert_header(
            document,
            prototype,
            &fixture,
            "procedure Adjust(Value in number);",
        );
        assert_header(
            document,
            implementation,
            &fixture,
            "procedure Adjust(Value in number) as",
        );
        let record = definition(document, fixture.span("RowValue", 0));
        assert_eq!(record.kind, EntityKind::Struct);
        assert_eq!(record.container, Some(ContainerRef::Entity(body.id)));
        assert_header(document, record, &fixture, "type RowValue is record (");
        let field = definition(document, fixture.span("FieldValue", 0));
        assert_eq!(field.kind, EntityKind::Field);
        assert_eq!(field.container, Some(ContainerRef::Entity(record.id)));
        let formal = definition(document, fixture.span("Value", 2));
        assert_eq!(formal.kind, EntityKind::Parameter);
        assert_eq!(
            formal.container,
            Some(ContainerRef::Entity(implementation.id))
        );
        let local = definition(document, fixture.span("LocalValue", 0));
        assert_eq!(local.kind, EntityKind::Variable);
        assert_eq!(
            local.container,
            Some(ContainerRef::Entity(implementation.id))
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "plsql-declaration-linkage-unavailable")
        );
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn applications_and_dynamic_sql_do_not_fabricate_calls_or_declarations() {
    let fixture = Fixture::new(
        "create or replace package body Sample as\nprocedure Store(Value in number) as\nLocalValue number;\nStatement varchar2(1000);\nbegin\nLocalValue := Items(Value);\nSample.Store(Value);\nStatement := 'begin Phantom(Value); create table VirtualTable(Id number); end;';\nexecute immediate Statement;\nend Store;\nend Sample;\n/\n",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    assert_unresolved(
        document,
        fixture.span("Items", 0),
        "plsql.application.reference",
        "plsql-call-or-index-target-unavailable",
    );
    assert_unresolved(
        document,
        fixture.span("Store", 1),
        "plsql.application.reference",
        "plsql-call-or-index-target-unavailable",
    );
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
    assert!(
        document
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "Phantom" | "VirtualTable"))
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::StringEvidence
                && site.source.span()
                    == fixture.span(
                        "'begin Phantom(Value); create table VirtualTable(Id number); end;'",
                        0
                    ))
    );
    let formal = definition(document, fixture.span("Value", 0));
    assert_eq!(formal.kind, EntityKind::Parameter);
    assert_unresolved(
        document,
        fixture.span("Value", 1),
        "plsql.identifier.reference",
        "plsql-binding-target-unavailable",
    );
}

#[test]
fn quoted_and_mixed_case_names_keep_source_identity_without_identifier_decoding() {
    let fixture = Fixture::new(
        "create or replace package body \"Case.Name\" as\nprocedure Work$(Value# in number) as\nbegin\nnull;\nend wOrK$;\nend \"Case.Name\";\n/\n",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let package = definition(document, fixture.span("\"Case.Name\"", 0));
    assert_eq!(package.canonical_name, "\"Case.Name\"");
    let procedure = definition(document, fixture.span("Work$", 0));
    assert_eq!(procedure.canonical_name, "Work$");
    assert_eq!(procedure.container, Some(ContainerRef::Entity(package.id)));
    let formal = definition(document, fixture.span("Value#", 0));
    assert_eq!(formal.canonical_name, "Value#");
    assert_eq!(formal.kind, EntityKind::Parameter);
    assert_eq!(formal.container, Some(ContainerRef::Entity(procedure.id)));
    assert_header(
        document,
        procedure,
        &fixture,
        "procedure Work$(Value# in number) as",
    );
}

#[test]
fn ordinary_single_end_standalone_does_not_consume_following_owner() {
    let fixture = Fixture::new(
        "create or replace procedure Standalone(Value in number) as\nLocalValue number;\nbegin\nLocalValue := Value;\nend Standalone;\n/\ncreate or replace package Following as\nprocedure Independent(Value in number);\nend Following;\n/\n",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let standalone = definition(document, fixture.span("Standalone", 0));
    assert_header(
        document,
        standalone,
        &fixture,
        "create or replace procedure Standalone(Value in number) as",
    );
    let following = definition(document, fixture.span("Following", 0));
    let independent = definition(document, fixture.span("Independent", 0));
    assert_eq!(
        independent.container,
        Some(ContainerRef::Entity(following.id))
    );
    assert_ne!(
        independent.container,
        Some(ContainerRef::Entity(standalone.id))
    );
    assert!(
        standalone
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span()
            .end_byte()
            <= fixture.span("Following", 0).start_byte()
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.reason != SkippedRegionReason::ParseError)
    );
}

#[test]
fn standalone_function_keeps_its_single_end_and_independent_following_owner() {
    let fixture = Fixture::new(
        "create or replace function Measure(Value in number) return number as\nLocalValue number;\nbegin\nLocalValue := Value;\nreturn LocalValue;\nend measure;\n/\ncreate or replace package Following as\nprocedure Independent(Value in number);\nend Following;\n/\n",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let function = definition(document, fixture.span("Measure", 0));
    assert_eq!(function.kind, EntityKind::Function);
    assert_header(
        document,
        function,
        &fixture,
        "create or replace function Measure(Value in number) return number as",
    );
    for (written, kind) in [
        ("Value", EntityKind::Parameter),
        ("LocalValue", EntityKind::Variable),
    ] {
        let entity = definition(document, fixture.span(written, 0));
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.container, Some(ContainerRef::Entity(function.id)));
    }
    let following = definition(document, fixture.span("Following", 0));
    let independent = definition(document, fixture.span("Independent", 0));
    assert_eq!(
        independent.container,
        Some(ContainerRef::Entity(following.id))
    );
    assert!(
        function.evidence.source.as_ref().unwrap().span().end_byte()
            <= fixture.span("Following", 0).start_byte()
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.reason != SkippedRegionReason::ParseError)
    );
}

#[test]
fn external_call_specs_retain_written_headers_without_external_target_claims() {
    let fixture = Fixture::new(
        "create or replace procedure Send(Value in number) as language java name 'bridge.Send(java.lang.Integer)';\n/\ncreate or replace function ReadValue(Value in number) return number as external name \"read_value\" library NativeBridge;\n/\ncreate or replace package Following as\nprocedure Independent(Value in number);\nend Following;\n/\n",
    );
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_plsql::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(fixture.text.as_bytes(), None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut cursor = tree.root_node().walk();
    let routines: Vec<_> = tree
        .root_node()
        .named_children(&mut cursor)
        .filter(|node| matches!(node.kind(), "create_procedure" | "create_function"))
        .collect();
    assert_eq!(routines.len(), 2);
    for (routine, expected) in routines.iter().zip(["java_declaration", "c_declaration"]) {
        let mut cursor = routine.walk();
        let call_spec = routine
            .named_children(&mut cursor)
            .find(|node| node.kind() == "call_spec_ext")
            .unwrap();
        assert_eq!(call_spec.parent(), Some(*routine));
        assert!(!call_spec.has_error());
        let mut cursor = call_spec.walk();
        let declaration = call_spec.named_children(&mut cursor).next().unwrap();
        assert_eq!(declaration.kind(), expected);
        assert_eq!(declaration.parent(), Some(call_spec));
        assert!(!declaration.has_error());
        let mut cursor = routine.walk();
        let semicolon = routine
            .children(&mut cursor)
            .find(|node| node.kind() == ";")
            .unwrap();
        assert!(semicolon.start_byte() >= call_spec.end_byte());
        assert_eq!(&fixture.text.as_bytes()[semicolon.byte_range()], b";");
        assert!(!semicolon.is_missing());
    }
    let output = fixture.analyze(true);
    let document = output.document();
    for (index, written, header) in [
        (
            0,
            "Send",
            "create or replace procedure Send(Value in number) as",
        ),
        (
            1,
            "ReadValue",
            "create or replace function ReadValue(Value in number) return number as",
        ),
    ] {
        let external = definition(document, fixture.span(written, 0));
        assert_eq!(external.kind, EntityKind::Function);
        assert_header(document, external, &fixture, header);
        let formal = definition(document, fixture.span("Value", index));
        assert_eq!(formal.kind, EntityKind::Parameter);
        assert_eq!(formal.container, Some(ContainerRef::Entity(external.id)));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.detail == "plsql-external-implementation-unavailable"
                    && gap.source == *external.evidence.source.as_ref().unwrap())
        );
    }
    let following = definition(document, fixture.span("Following", 0));
    let independent = definition(document, fixture.span("Independent", 0));
    assert_eq!(
        independent.container,
        Some(ContainerRef::Entity(following.id))
    );
    assert!(document.entities.iter().all(|entity| !matches!(
        entity.canonical_name.as_str(),
        "read_value" | "NativeBridge" | "bridge.Send"
    )));
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::StringEvidence
                && site.source.span() == fixture.span("'bridge.Send(java.lang.Integer)'", 0))
    );
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn conditional_body_keeps_written_headers_without_selecting_a_branch() {
    let fixture = Fixture::new(
        "create or replace package body Sample as\nprocedure Stable(Value in number) as\nbegin\n$if $$Feature $then\nnull;\n$end\nend Stable;\nend Sample;\n/\n",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let package = definition(document, fixture.span("Sample", 0));
    let procedure = definition(document, fixture.span("Stable", 0));
    assert_eq!(procedure.container, Some(ContainerRef::Entity(package.id)));
    assert_header(
        document,
        procedure,
        &fixture,
        "procedure Stable(Value in number) as",
    );
    let conditional = fixture.span("$if $$Feature $then\nnull;\n$end", 0);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain
                    && gap.detail == "plsql-conditional-selection-unavailable"
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.source.span() == conditional)
        );
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.reason != SkippedRegionReason::ParseError)
    );
}

#[test]
fn conditional_package_declarations_retain_each_written_branch_without_selection() {
    let fixture = Fixture::new(
        "create or replace package Selection as\n$if $$Feature $then\nprocedure FirstChoice(Value in number);\n$else\nfunction SecondChoice(Value in number) return number;\n$end\nend Selection;\n/\n",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let package = definition(document, fixture.span("Selection", 0));
    for (index, written, header) in [
        (0, "FirstChoice", "procedure FirstChoice(Value in number);"),
        (
            1,
            "SecondChoice",
            "function SecondChoice(Value in number) return number;",
        ),
    ] {
        let procedure = definition(document, fixture.span(written, 0));
        assert_eq!(procedure.kind, EntityKind::Function);
        assert_eq!(procedure.container, Some(ContainerRef::Entity(package.id)));
        assert_header(document, procedure, &fixture, header);
        let formal = definition(document, fixture.span("Value", index));
        assert_eq!(formal.kind, EntityKind::Parameter);
        assert_eq!(formal.container, Some(ContainerRef::Entity(procedure.id)));
    }
    let conditional = fixture.span("$if $$Feature $then\nprocedure FirstChoice(Value in number);\n$else\nfunction SecondChoice(Value in number) return number;\n$end", 0);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.detail == "plsql-conditional-selection-unavailable"
                    && gap.source.span() == conditional
            })
            .collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gaps[0].source.repository(), fixture.source.repository());
        assert_eq!(gaps[0].source.generation(), fixture.source.generation());
        assert_eq!(gaps[0].source.content_hash(), fixture.source.content_hash());
    }
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

fn assert_conditional_gap(document: &NormalizedIrDocument, fixture: &Fixture, written: &str) {
    let span = fixture.span(written, 0);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.detail == "plsql-conditional-selection-unavailable"
                    && gap.source.span() == span
            })
            .collect();
        assert_eq!(gaps.len(), 1, "{domain:?} conditional at {span:?}");
        assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gaps[0].source.repository(), fixture.source.repository());
        assert_eq!(gaps[0].source.generation(), fixture.source.generation());
        assert_eq!(gaps[0].source.content_hash(), fixture.source.content_hash());
    }
}

#[test]
fn function_properties_retain_result_cache_dependencies_and_neighbor_owners() {
    for body in [false, true] {
        let qualifier = if body { " body" } else { "" };
        let before = if body {
            "procedure BeforeStep(BeforeArg in number) as begin null; end BeforeStep;"
        } else {
            "procedure BeforeStep(BeforeArg in number);"
        };
        let cache_header = "function ReadValue(ReadArg in number) return number result_cache relies_on (DataA, DataB)";
        let cache_tail = if body {
            " as\nbegin return ReadArg; end ReadValue;"
        } else {
            ";"
        };
        let after = if body {
            "procedure AfterStep(AfterArg in number) as begin null; end AfterStep;"
        } else {
            "procedure AfterStep(AfterArg in number);"
        };
        let fixture = Fixture::new(&format!(
            "create or replace package{qualifier} CacheKit as\n{before}\n{cache_header}{cache_tail}\n{after}\nend CacheKit;\n/\n"
        ));
        let output = fixture.analyze(true);
        let document = output.document();
        let package = definition(document, fixture.span("CacheKit", 0));
        assert_eq!(package.kind, EntityKind::Module);
        for (written, formal) in [
            ("BeforeStep", "BeforeArg"),
            ("ReadValue", "ReadArg"),
            ("AfterStep", "AfterArg"),
        ] {
            let routine = definition(document, fixture.span(written, 0));
            assert_eq!(routine.kind, EntityKind::Function);
            assert_eq!(routine.container, Some(ContainerRef::Entity(package.id)));
            let parameter = definition(document, fixture.span(formal, 0));
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(parameter.container, Some(ContainerRef::Entity(routine.id)));
        }
        let cached = definition(document, fixture.span("ReadValue", 0));
        let header = format!("{cache_header}{}", if body { " as" } else { ";" });
        assert_header(document, cached, &fixture, &header);
        for written in ["DataA", "DataB"] {
            assert_unresolved(
                document,
                fixture.span(written, 0),
                "plsql.identifier.reference",
                "plsql-binding-target-unavailable",
            );
        }
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn function_properties_retain_conditional_headers_and_empty_written_branches() {
    let properties = "$if $$CacheEnabled $then\nresult_cache\n$elsif $$PureEnabled $then\ndeterministic\n$else\n$end";
    for body in [false, true] {
        let qualifier = if body { " body" } else { "" };
        let before = if body {
            "procedure BeforeStep(BeforeArg in number) as begin null; end BeforeStep;"
        } else {
            "procedure BeforeStep(BeforeArg in number);"
        };
        let cache_header =
            format!("function ReadValue(ReadArg in number) return number\n{properties}");
        let cache_tail = if body {
            "\nas\nbegin return ReadArg; end ReadValue;"
        } else {
            ";"
        };
        let after = if body {
            "procedure AfterStep(AfterArg in number) as begin null; end AfterStep;"
        } else {
            "procedure AfterStep(AfterArg in number);"
        };
        let fixture = Fixture::new(&format!(
            "create or replace package{qualifier} CacheKit as\n{before}\n{cache_header}{cache_tail}\n{after}\nend CacheKit;\n/\n"
        ));
        let output = fixture.analyze(true);
        let document = output.document();
        let package = definition(document, fixture.span("CacheKit", 0));
        for (written, formal) in [
            ("BeforeStep", "BeforeArg"),
            ("ReadValue", "ReadArg"),
            ("AfterStep", "AfterArg"),
        ] {
            let routine = definition(document, fixture.span(written, 0));
            assert_eq!(routine.kind, EntityKind::Function);
            assert_eq!(routine.container, Some(ContainerRef::Entity(package.id)));
            let parameter = definition(document, fixture.span(formal, 0));
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(parameter.container, Some(ContainerRef::Entity(routine.id)));
        }
        let cached = definition(document, fixture.span("ReadValue", 0));
        let header = format!("{cache_header}{}", if body { "\nas" } else { ";" });
        assert_header(document, cached, &fixture, &header);
        assert_conditional_gap(document, &fixture, properties);
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::CallSite)
        );
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn function_properties_retain_nested_split_cache_dependencies_without_selection() {
    let dependencies = "$if $$TrackDependencies $then\nrelies_on (DataA, DataB)\n$elsif $$AlternateDependencies $then\nrelies_on (DataC)\n$else\n$end";
    for nested in [false, true] {
        let properties = if nested {
            format!("$if $$CacheEnabled $then\nresult_cache\n{dependencies}\n$else\n$end")
        } else {
            format!("result_cache\n{dependencies}")
        };
        let header =
            format!("function ReadValue(ReadArg in number) return number\n{properties}\nas");
        let fixture = Fixture::new(&format!(
            "create or replace package body CacheKit as\nprocedure BeforeStep(BeforeArg in number) as begin null; end BeforeStep;\n{header}\nbegin return ReadArg; end ReadValue;\nprocedure AfterStep(AfterArg in number) as begin null; end AfterStep;\nend CacheKit;\n/\n"
        ));
        let output = fixture.analyze(true);
        let document = output.document();
        let package = definition(document, fixture.span("CacheKit", 0));
        let cached = definition(document, fixture.span("ReadValue", 0));
        assert_header(document, cached, &fixture, &header);
        for (written, formal) in [
            ("BeforeStep", "BeforeArg"),
            ("ReadValue", "ReadArg"),
            ("AfterStep", "AfterArg"),
        ] {
            let routine = definition(document, fixture.span(written, 0));
            assert_eq!(routine.kind, EntityKind::Function);
            assert_eq!(routine.container, Some(ContainerRef::Entity(package.id)));
            let parameter = definition(document, fixture.span(formal, 0));
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(parameter.container, Some(ContainerRef::Entity(routine.id)));
        }
        assert_conditional_gap(document, &fixture, dependencies);
        if nested {
            assert_conditional_gap(document, &fixture, &properties);
        }
        for written in ["DataA", "DataB", "DataC"] {
            assert_unresolved(
                document,
                fixture.span(written, 0),
                "plsql.identifier.reference",
                "plsql-binding-target-unavailable",
            );
        }
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn function_properties_after_result_cache_are_not_forced_into_dependency_context() {
    let properties = "$if $$PureEnabled $then\ndeterministic\n$else\n$end";
    for body in [false, true] {
        let qualifier = if body { " body" } else { "" };
        let header = format!(
            "function ReadValue(ReadArg in number) return number result_cache\n{properties}{}",
            if body { "\nas" } else { ";" }
        );
        let tail = if body {
            "\nbegin return ReadArg; end ReadValue;"
        } else {
            ""
        };
        let after = if body {
            "procedure AfterStep(AfterArg in number) as begin null; end AfterStep;"
        } else {
            "procedure AfterStep(AfterArg in number);"
        };
        let fixture = Fixture::new(&format!(
            "create or replace package{qualifier} CacheKit as\n{header}{tail}\n{after}\nend CacheKit;\n/\n"
        ));
        let output = fixture.analyze(true);
        let document = output.document();
        let package = definition(document, fixture.span("CacheKit", 0));
        let cached = definition(document, fixture.span("ReadValue", 0));
        assert_header(document, cached, &fixture, &header);
        for (written, formal) in [("ReadValue", "ReadArg"), ("AfterStep", "AfterArg")] {
            let routine = definition(document, fixture.span(written, 0));
            assert_eq!(routine.kind, EntityKind::Function);
            assert_eq!(routine.container, Some(ContainerRef::Entity(package.id)));
            let parameter = definition(document, fixture.span(formal, 0));
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(parameter.container, Some(ContainerRef::Entity(routine.id)));
        }
        assert_conditional_gap(document, &fixture, properties);
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn function_properties_empty_elsif_and_else_branches_stay_physical() {
    let conditional = "$if $$CacheEnabled $then\n$elsif $$AlternateCache $then\n$else\n$end";
    for prefix in ["", "result_cache\n"] {
        let header =
            format!("function ReadValue(ReadArg in number) return number\n{prefix}{conditional};");
        let fixture = Fixture::new(&format!(
            "create or replace package CacheKit as\n{header}\nprocedure AfterStep(AfterArg in number);\nend CacheKit;\n/\n"
        ));
        let output = fixture.analyze(true);
        let document = output.document();
        let package = definition(document, fixture.span("CacheKit", 0));
        let cached = definition(document, fixture.span("ReadValue", 0));
        assert_header(document, cached, &fixture, &header);
        for (written, formal) in [("ReadValue", "ReadArg"), ("AfterStep", "AfterArg")] {
            let routine = definition(document, fixture.span(written, 0));
            assert_eq!(routine.container, Some(ContainerRef::Entity(package.id)));
            let parameter = definition(document, fixture.span(formal, 0));
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(parameter.container, Some(ContainerRef::Entity(routine.id)));
        }
        assert_conditional_gap(document, &fixture, conditional);
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn function_properties_missing_conditional_end_cannot_invent_header_identity() {
    let fixture = Fixture::new(
        "create or replace package CacheKit as\nprocedure BeforeStep(BeforeArg in number);\nfunction Broken(Leaked in number) return number\n$if $$CacheEnabled $then\nresult_cache;\nprocedure AfterStep(AfterArg in number);\nend CacheKit;\n/\n",
    );
    let output = fixture.analyze(false);
    let document = output.document();
    assert!(
        document
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "Broken" | "Leaked"))
    );
    assert!(document.occurrences.iter().all(|site| {
        site.role != OccurrenceRole::Definition
            || ![fixture.span("Broken", 0), fixture.span("Leaked", 0)].contains(&site.source.span())
    }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}

#[test]
fn recovered_malformed_headers_and_wrong_closing_names_do_not_leak_descendants() {
    for body in [
        "procedure Broken(Leaked in) as\nLocalLeak number;\nbegin null; end Broken;",
        "procedure Broken(Leaked in number) as\nLocalLeak number;\nbegin null; end Other;",
    ] {
        let fixture = Fixture::new(&format!(
            "create or replace package body Sample as\n{body}\nprocedure Stable(Value in number) as\nbegin null; end Stable;\nend Sample;\n/\n"
        ));
        let output = fixture.analyze(false);
        let document = output.document();
        assert!(document.entities.iter().all(|entity| !matches!(
            entity.canonical_name.as_str(),
            "Broken" | "Leaked" | "LocalLeak"
        )));
        assert!(document.occurrences.iter().all(|site| {
            ![fixture.span("Leaked", 0), fixture.span("LocalLeak", 0)].contains(&site.source.span())
        }));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "plsql-source-identity-unavailable"
                    && gap.domain == FactDomain::Entities)
        );
    }
}
