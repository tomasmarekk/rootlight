//! Native formal-context metadata stays required without changing callable identity.
//!
//! The zero-optional path uses the real parser before the production project builder.

use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisUnitId, BatchThresholds, BuildTargetId, EncodingId,
    GenerationBoundSnapshot, ProjectAnalysisLimits, StreamLimits, SyntaxKindLabel,
};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, TreeSitterProvider};
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::IrLimits;
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::path::Path;

#[test]
fn binding_context_labels_are_exact_metadata_and_not_callable_signatures() {
    let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
    for (label, ordinary) in [
        (
            "c.formal_binding_context.signature",
            "c.parameter_list.signature",
        ),
        (
            "java.setter_binding_context.signature",
            "java.parameters.signature",
        ),
        (
            "go.call_formal_context.signature",
            "go.parameters.signature",
        ),
        (
            "go.composite_formal_context.signature",
            "go.parameters.signature",
        ),
    ] {
        let extended_label = label.replace(".signature", "_extra.signature");
        let fact = |kind, label| {
            SyntaxFact::new(1, None, kind, span, 0, SyntaxKindLabel::new(label).unwrap())
        };
        assert!(!is_symbol_signature_fact(&fact(
            SyntaxFactKind::Signature,
            label
        )));
        assert!(is_symbol_signature_fact(&fact(
            SyntaxFactKind::Signature,
            ordinary
        )));
        assert!(is_symbol_signature_fact(&fact(
            SyntaxFactKind::Signature,
            &extended_label
        )));
        assert!(!is_symbol_signature_fact(&fact(
            SyntaxFactKind::Occurrence,
            label
        )));
    }
}

#[test]
fn zero_optional_native_contexts_keep_project_signature_arity_and_exact_relation_gap() {
    for (language, path, text, kind, label, signature, reason) in [
        (
            SemanticProjectLanguage::C,
            "written.c",
            "void owned(int value) { external(value); }\n",
            EntityKind::Function,
            "c.formal_binding_context.signature",
            "(intvalue)",
            "c-formal-proof-is-limited-to-direct-arguments-in-a-single-clean-call-statement",
        ),
        (
            SemanticProjectLanguage::C,
            "unproved.c",
            "void owned(int value) { if (1) external(value); }\n",
            EntityKind::Function,
            "c.formal_binding_context.signature",
            "(intvalue)",
            "c-formal-proof-is-limited-to-direct-arguments-in-a-single-clean-call-statement",
        ),
        (
            SemanticProjectLanguage::Go,
            "written.go",
            "package written\ntype Holder struct { data []byte }\nfunc owned(value []byte) *Holder { return &Holder{data: value} }\n",
            EntityKind::Function,
            "go.composite_formal_context.signature",
            "(value[]byte)",
            "go-formal-proof-is-limited-to-ascii-free-functions-and-direct-single-return-literal-values",
        ),
        (
            SemanticProjectLanguage::Go,
            "argument.go",
            "package written\nfunc owned(value int) { external(value) }\n",
            EntityKind::Function,
            "go.call_formal_context.signature",
            "(valueint)",
            "go-call-argument-proof-requires-clean-free-function-and-no-formal-rebinding-or-inner-function",
        ),
        (
            SemanticProjectLanguage::Java,
            "Local.java",
            "class Local { void owned(Object input) { Object receiver = input; receiver.run(); } }\n",
            EntityKind::Method,
            "java.local_receiver_context.signature",
            "(Objectinput)",
            "java-local-receiver-proof-does-not-evaluate-types-initializers-or-method-dispatch",
        ),
        (
            SemanticProjectLanguage::Java,
            "Written.java",
            "class Written { Object field; void owned(Object value) { this.field = value; } }\n",
            EntityKind::Method,
            "java.setter_binding_context.signature",
            "(Objectvalue)",
            "java-formal-binding-proof-is-limited-to-single-assignment-setters",
        ),
    ] {
        let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::write(temporary.path().join(path), text).unwrap();
        let repository: RepositoryId = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let generation: GenerationId = "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .unwrap();
        let root = RepositoryRoot::open(repository, temporary.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 64 * 1024)
            .unwrap();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(snapshot.file(), 0, text.len() as u64).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let batch = BatchThresholds::new(256, 4 * 1024 * 1024, 128, 128 * 1024).unwrap();
        let stream = StreamLimits::new(
            256,
            16 * 1024,
            4 * 1024 * 1024,
            1024,
            1024 * 1024,
            4 * 1024 * 1024,
            batch,
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            64 * 1024,
            64 * 1024,
            4096,
            256,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap()
        .with_project_limits(
            ProjectAnalysisLimits::new(1, 64 * 1024, 64 * 1024, 1, 64 * 1024, 256, 256).unwrap(),
        );
        let config = RuntimeConfig::new(
            64 * 1024,
            64 * 1024,
            4096,
            256,
            256,
            1,
            16 * 1024 * 1024,
            ParserSettings::new(4096).unwrap(),
        )
        .unwrap();
        let parser = Arc::new(TreeSitterProvider::new(config).unwrap());
        let bound = GenerationBoundSnapshot::new(&snapshot, &source).unwrap();
        let parse_request = ParseRequest::new(
            bound.clone(),
            language.language_id().unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let native = execute_parse(
            parser.as_ref(),
            &parse_request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
        let contexts: Vec<_> = native
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == label
            })
            .cloned()
            .collect();
        assert_eq!(
            contexts.len(),
            1,
            "real native context prerequisite: {path}"
        );
        let context = &contexts[0];
        let mut retained = native.facts().to_vec();
        retain_project_syntax_facts(
            language,
            &mut retained,
            text.as_bytes(),
            0,
            &BTreeMap::new(),
            None,
        );
        assert!(
            retained.contains(context),
            "required metadata survives zero optional retention"
        );
        assert!(
            retained.len() < native.facts().len(),
            "the zero allowance removes actual optional native captures"
        );
        let manifest = b"{}";
        let build_context = BuildContextIdentity::new(content_hash(b"formal-context-test"));
        let request = ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.formal-context").unwrap(),
            BuildTargetId::new("//fixture:formal-context").unwrap(),
            build_context,
            content_hash(manifest),
            manifest,
            vec![ProjectSourceInput::new(
                bound,
                language.language_id().unwrap(),
                EncodingId::utf8(),
                false,
                Vec::new(),
            )],
            AnalysisTier::TierB,
            &limits,
        )
        .unwrap();
        let producer = ProducerIdentity::new(
            "rootlight-formal-context-test",
            "1.0.0",
            content_hash(b"formal-context-test"),
        )
        .unwrap();
        let analyzer = SemanticProjectAnalyzer::new(
            language,
            parser,
            producer,
            content_hash(b"native-test-binary"),
            build_context,
        )
        .unwrap();
        let parsed = || {
            vec![ParsedInput {
                input: &request.inputs()[0],
                facts: retained.clone(),
                native_imports: BTreeMap::new(),
                diagnostics: native.diagnostics().to_vec(),
                parse_status: native.report().coverage().status(),
                syntax_nodes: native.report().resources().syntax_nodes(),
                max_syntax_depth: native.report().resources().max_syntax_depth(),
            }]
        };
        let mut builder = ProjectFactsBuilder::new(&analyzer, &request, parsed(), &cancellation);
        builder.materialize_files_and_provenance().unwrap();
        builder.collect_syntax().unwrap();
        let callable = builder
            .declarations
            .iter()
            .find(|declaration| declaration.kind == kind && declaration.name == "owned")
            .unwrap();
        assert_eq!(callable.signature, signature);
        assert_eq!(callable.arity, Some(1));
        drop(builder);
        let project = ProjectFactsBuilder::new(&analyzer, &request, parsed(), &cancellation)
            .build()
            .unwrap();
        let gaps: Vec<_> = project
            .records
            .iter()
            .filter_map(|record| match record {
                IrRecord::SkippedRegion(gap)
                    if gap.domain == FactDomain::Relations && gap.detail == reason =>
                {
                    Some(gap)
                }
                _ => None,
            })
            .collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(
            gaps[0].source,
            source_for_span(&request.inputs()[0], context.span())
        );
        assert_eq!(gaps[0].source.repository(), repository);
        assert_eq!(gaps[0].source.generation(), generation);
        assert_eq!(gaps[0].source.content_hash(), content_hash(text.as_bytes()));
        assert_ne!(project.status, CoverageStatus::Complete);
        let relation_coverage: Vec<_> = project
            .records
            .iter()
            .filter_map(|record| match record {
                IrRecord::Coverage(record)
                    if record.domain == FactDomain::Relations
                        && record.scope == CoverageScope::File(snapshot.file()) =>
                {
                    Some(record)
                }
                _ => None,
            })
            .collect();
        assert_eq!(relation_coverage.len(), 1);
        assert_ne!(relation_coverage[0].status, CoverageStatus::Complete);
        assert!(relation_coverage[0].skipped > 0);
        if language == SemanticProjectLanguage::C
            || label == "java.local_receiver_context.signature"
        {
            let full_native = vec![ParsedInput {
                input: &request.inputs()[0],
                facts: native.facts().to_vec(),
                native_imports: BTreeMap::new(),
                diagnostics: native.diagnostics().to_vec(),
                parse_status: native.report().coverage().status(),
                syntax_nodes: native.report().resources().syntax_nodes(),
                max_syntax_depth: native.report().resources().max_syntax_depth(),
            }];
            let full = ProjectFactsBuilder::new(&analyzer, &request, full_native, &cancellation)
                .build()
                .unwrap();
            let reads: Vec<_> = full
                .records
                .iter()
                .filter_map(|record| match record {
                    IrRecord::Occurrence(site)
                        if matches!(
                            site.syntax_kind.as_str(),
                            "c.formal_value.reference"
                                | "c.unproven_formal_value.reference"
                                | "java.local_receiver.reference"
                                | "java.unproven_local_receiver.reference"
                        ) =>
                    {
                        Some(site)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                reads.len(),
                1,
                "real native formal-use prerequisite: {path}"
            );
            assert!(
                matches!(reads[0].target, OccurrenceTarget::Unresolved { .. }),
                "Structural-only certificates cannot use project name matching: {path}"
            );
            assert_eq!(
                reads[0].source.content_hash(),
                content_hash(text.as_bytes())
            );
            assert_ne!(full.status, CoverageStatus::Complete);
        }
    }
}
