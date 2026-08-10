//! Public behavior tests for the five whole-project semantic adapters.
//!
//! A parser-independent fixture provider exposes the same structural fact
//! classes as audited query packs, keeping these contract tests deterministic.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisRequest, AnalysisUnitId,
    BatchThresholds, BuildTargetId, CoverageReport, DiagnosticCode, EncodingId,
    GeneratedOriginMapping, GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy,
    MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport, ParseRequest,
    ProjectAnalysisLimits, ProjectAnalysisRequest, ProjectSourceInput, ResourceUsage, StreamEnd,
    StreamLimits, SyntaxFact, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel, TransformationId,
    WorkReport, execute_analysis, execute_project_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_adapters::{SemanticProjectAnalyzer, SemanticProjectLanguage};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, DiagnosticSeverity, EntityFlag, EntityKind,
    ExtensionSupport, FILE_IDENTITY_CLAIM_NAMESPACE, FactDomain, FactRef, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, RelationPredicate,
    SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceMappingKind, SourceRef, SourceSpan,
    decode_symbol_identity_claim_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

#[test]
fn every_language_emits_complete_tier_b_project_semantics() {
    for case in language_cases() {
        let fixture = ProjectFixture::new(case.paths, case.sources, case.language);
        let limits = limits();
        let request = fixture.request(&limits, AnalysisTier::TierA);
        let analyzer = analyzer(case.language, fixture.build_context);

        let output = execute_project_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("reviewed project semantics commit");
        assert_eq!(
            output.report().work().coverage().tier(),
            AnalysisTier::TierB
        );
        assert_eq!(
            output.report().work().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(output.report().build_context(), fixture.build_context);
        assert_eq!(output.report().work().coverage().domains().len(), 8);
        assert!(FactDomain::Files <= FactDomain::Extensions);
        assert!(
            output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Imports),
            "{} import was not materialized",
            case.language.as_str()
        );
        assert!(
            output.document().occurrences.iter().any(|occurrence| {
                occurrence.role == OccurrenceRole::CallSite
                    && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            }),
            "{} call was not resolved",
            case.language.as_str()
        );
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|entity| entity.display_name == "<lexical scope>"),
            "{} lexical scope was not retained",
            case.language.as_str()
        );
        assert!(
            !output.document().extensions.is_empty(),
            "{} signature evidence was not retained",
            case.language.as_str()
        );
        assert_eq!(
            output
                .document()
                .extensions
                .iter()
                .filter(|extension| extension.namespace == FILE_IDENTITY_CLAIM_NAMESPACE)
                .count(),
            output.document().files.len(),
            "{} file identity proofs were incomplete",
            case.language.as_str()
        );
        assert_eq!(
            output
                .document()
                .extensions
                .iter()
                .filter(|extension| extension.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
                .count(),
            output.document().entities.len(),
            "{} symbol identity proofs were incomplete",
            case.language.as_str()
        );
    }
}

#[test]
fn function_identity_is_stable_when_only_its_body_changes() {
    let first = ProjectFixture::new(
        ["src/lib.rs", "src/other.rs"],
        ["pub fn run() -> u32 {\n    42\n}\n", "pub fn other() {}\n"],
        SemanticProjectLanguage::Rust,
    );
    let second = ProjectFixture::new(
        ["src/lib.rs", "src/other.rs"],
        ["pub fn run() -> u32 {\n    43\n}\n", "pub fn other() {}\n"],
        SemanticProjectLanguage::Rust,
    );
    let limits = limits();
    let first_request = first.request(&limits, AnalysisTier::TierB);
    let second_request = second.request(&limits, AnalysisTier::TierB);
    let first_analyzer = analyzer(SemanticProjectLanguage::Rust, first.build_context);
    let second_analyzer = analyzer(SemanticProjectLanguage::Rust, second.build_context);

    let first_output = execute_project_analysis(
        &first_analyzer,
        &first_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("first project analysis commits");
    let second_output = execute_project_analysis(
        &second_analyzer,
        &second_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("second project analysis commits");
    let first_symbol = first_output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "run")
        .expect("first run symbol is present")
        .id;
    let second_symbol = second_output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "run")
        .expect("second run symbol is present")
        .id;

    assert_eq!(first_symbol, second_symbol);
}

#[test]
fn python_symbol_identity_matches_structural_and_project_analysis() {
    let fixture = ProjectFixture::new(
        ["Lib/asyncio/helpers.py", "Lib/asyncio/base_events.py"],
        [
            "def helper():\n    return None\n",
            "class BaseEventLoop:\n    def run_until_complete(self, future):\n        return future\n\ndef _run_once(loop):\n    def nested():\n        return loop\n    return nested()\n",
        ],
        SemanticProjectLanguage::Python,
    );
    assert_real_parser_symbol_identity(
        &fixture,
        "python",
        &[
            (EntityKind::Module, "Lib/asyncio/base_events.py"),
            (EntityKind::Class, "BaseEventLoop"),
            (EntityKind::Method, "run_until_complete"),
            (EntityKind::Function, "_run_once"),
            (EntityKind::Function, "nested"),
        ],
    );
}

#[test]
fn rust_impl_method_identity_matches_structural_and_project_analysis() {
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/other.rs"],
        [
            "pub struct Demo;\nimpl Demo {\n    pub fn answer(&self) -> u32 { 42 }\n}\npub fn top_level() {}\n",
            "pub fn other() {}\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    assert_real_parser_symbol_identity(
        &fixture,
        "rust",
        &[
            (EntityKind::Struct, "Demo"),
            (EntityKind::Method, "answer"),
            (EntityKind::Function, "top_level"),
        ],
    );
}

#[test]
fn nested_rust_impl_identity_matches_structural_and_project_analysis() {
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/other.rs"],
        [
            "pub struct Outer;\npub struct Local;\nimpl Outer {\n    pub fn create(&self) {\n        impl Local {\n            pub fn nested(&self) {}\n        }\n    }\n}\n",
            "pub fn other() {}\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    assert_real_parser_symbol_identity(
        &fixture,
        "rust",
        &[
            (EntityKind::Struct, "Outer"),
            (EntityKind::Struct, "Local"),
            (EntityKind::Method, "nested"),
        ],
    );
}

#[test]
fn call_occurrences_are_owned_by_the_declaring_function() {
    let fixture = ProjectFixture::new(
        ["src/dep.rs", "src/main.rs"],
        [
            "pub fn ping() {}\n",
            "use crate::dep::ping;\npub fn run() {\n    ping();\n}\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let run = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "run")
        .expect("caller function is materialized");
    let ping = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "ping")
        .expect("callee function is materialized");
    let call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol } if symbol == ping.id
                )
        })
        .expect("resolved call is materialized");

    assert_eq!(call.enclosing, Some(run.id));
}

#[test]
fn python_same_module_calls_resolve_to_the_declared_function() {
    let fixture = ProjectFixture::new(
        ["Lib/bisect.py", "Lib/__init__.py"],
        [
            concat!(
                "def bisect_left(a, x, lo=0, hi=None, *, key=None):\n",
                "    return lo\n\n",
                "def insort_left(a, x, lo=0, hi=None, *, key=None):\n",
                "    lo = bisect_left(a, x, lo, hi, key=key)\n",
                "    a.insert(lo, x)\n",
            ),
            "",
        ],
        SemanticProjectLanguage::Python,
    );
    let output = analyze_with_real_parser(&fixture);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "insort_left")
        .expect("caller function is materialized");
    let callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "bisect_left")
        .expect("callee function is materialized");

    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject
            == rootlight_ir::RelationEndpoint::Occurrence(
                output
                    .document()
                    .occurrences
                    .iter()
                    .find(|occurrence| {
                        occurrence.role == OccurrenceRole::CallSite
                            && occurrence.enclosing == Some(caller.id)
                            && matches!(
                                occurrence.target,
                                OccurrenceTarget::Resolved { symbol } if symbol == callee.id
                            )
                    })
                    .expect("resolved call occurrence is materialized")
                    .id,
            )
            && relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(callee.id)
    }));
    let file = output
        .document()
        .files
        .iter()
        .find(|file| file.path == "Lib/bisect.py")
        .expect("Python source file is materialized");
    let relationship_coverage = output
        .document()
        .coverage_records
        .iter()
        .find(|coverage| {
            coverage.scope == rootlight_ir::CoverageScope::File(file.id)
                && coverage.domain == FactDomain::Relations
        })
        .expect("relationship coverage is materialized");
    assert_eq!(relationship_coverage.status, CoverageStatus::Bounded);
    assert!(relationship_coverage.skipped >= 1);
    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
}

#[test]
fn project_semantics_preserve_test_classification() {
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "tests/semantic.rs"],
        [
            "pub fn production() {}\n",
            "use crate::production;\n#[test]\nfn semantic_behavior() {\n    production();\n}\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "semantic_behavior")
        .expect("test function is materialized");

    assert!(test.flags.contains(&EntityFlag::Test));
}

#[test]
fn go_project_semantics_preserve_test_calls() {
    let fixture = ProjectFixture::new(
        ["server/handlers.go", "server/handlers_test.go"],
        [
            "package server\nfunc GenerateHandler() {}\n",
            concat!(
                "package server\n",
                "import \"testing\"\n",
                "func TestGenerateHandler(t *testing.T) {\n",
                "    GenerateHandler()\n",
                "}\n",
            ),
        ],
        SemanticProjectLanguage::Go,
    );
    let output = analyze_with_real_parser(&fixture);
    let production = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "GenerateHandler")
        .expect("production function is materialized");
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "TestGenerateHandler")
        .expect("Go test function is materialized");

    assert!(test.flags.contains(&EntityFlag::Test));
    assert!(
        output.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(test.id)
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol } if symbol == production.id
                )
        }),
        "Go test call resolves through its package scope"
    );
}

#[test]
fn go_project_semantics_do_not_cross_package_directories() {
    let fixture = ProjectFixture::new(
        ["first/worker.go", "second/caller.go"],
        [
            "package shared\nfunc Work() {}\n",
            "package shared\nfunc Call() { Work() }\n",
        ],
        SemanticProjectLanguage::Go,
    );
    let output = analyze_with_real_parser(&fixture);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "Call")
        .expect("caller function is materialized");

    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
}

fn assert_real_parser_symbol_identity(
    fixture: &ProjectFixture,
    language: &str,
    expected: &[(EntityKind, &str)],
) {
    let limits = real_parser_limits();
    let provider = Arc::new(real_parser());
    let parser: Arc<dyn ParseProvider> = provider.clone();
    let producer = producer_identity();
    let binary_digest = content_hash(b"real-parser-binary");
    let project_analyzer = SemanticProjectAnalyzer::new(
        fixture.language,
        parser,
        producer.clone(),
        binary_digest,
        fixture.build_context,
    )
    .expect("project analyzer constructs");
    let project_output = execute_project_analysis(
        &project_analyzer,
        &fixture.request(&limits, AnalysisTier::TierB),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("project analysis commits");
    let structural_analyzer = TreeSitterAnalyzer::new(
        provider,
        producer,
        LanguageId::new(language).expect("language is valid"),
        language,
        binary_digest,
    )
    .expect("structural analyzer constructs");
    let mut structural_entities = Vec::new();
    let mut structural_claims = Vec::new();
    for (snapshot, source) in fixture.snapshots.iter().zip(&fixture.sources) {
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
            LanguageId::new(language).expect("language is valid"),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            fixture.build_context,
            &limits,
        )
        .expect("analysis request is valid")
        .with_generated_status(false);
        let output = execute_analysis(
            &structural_analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("structural analysis commits");
        structural_entities.extend(output.document().entities.iter().cloned());
        structural_claims.extend(
            output
                .document()
                .extensions
                .iter()
                .filter_map(|envelope| decode_symbol_identity_claim_envelope(envelope).ok()),
        );
    }

    for (kind, name) in expected {
        let project_id = project_output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == *kind && entity.display_name == *name)
            .unwrap_or_else(|| panic!("project analysis emits {kind:?} {name}"))
            .id;
        let structural_id = structural_entities
            .iter()
            .find(|entity| entity.kind == *kind && entity.display_name == *name)
            .unwrap_or_else(|| panic!("structural analysis emits {kind:?} {name}"))
            .id;
        let project_claim = project_output
            .document()
            .extensions
            .iter()
            .filter_map(|envelope| decode_symbol_identity_claim_envelope(envelope).ok())
            .find(|claim| claim.symbol == project_id);
        let structural_claim = structural_claims
            .iter()
            .find(|claim| claim.symbol == structural_id);
        assert_eq!(
            project_id, structural_id,
            "{kind:?} {name}; project={project_claim:?}; structural={structural_claim:?}"
        );
    }
}

fn analyze_with_real_parser(
    fixture: &ProjectFixture,
) -> rootlight_adapter_sdk::ProjectAnalysisOutput {
    let parser: Arc<dyn ParseProvider> = Arc::new(real_parser());
    let analyzer = SemanticProjectAnalyzer::new(
        fixture.language,
        parser,
        producer_identity(),
        content_hash(b"real-parser-binary"),
        fixture.build_context,
    )
    .expect("project analyzer constructs");
    execute_project_analysis(
        &analyzer,
        &fixture.request(&real_parser_limits(), AnalysisTier::TierB),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("project analysis commits")
}

#[test]
fn complete_generated_origin_map_is_materialized_with_complete_coverage() {
    let fixture = ProjectFixture::new(
        ["src/generator.rs", "src/api.generated.rs"],
        [
            "pub fn generate() {}\n",
            "// Code generated by fixture-gen. DO NOT EDIT.\n// source: src/generator.rs\npub fn api() {}\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    let limits = limits();
    let request =
        fixture.request_with_generated_mapping(&limits, Some(fixture.snapshots[1].content().len()));
    let analyzer = analyzer(SemanticProjectLanguage::Rust, fixture.build_context);

    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("generated origin analysis commits");

    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(output.document().source_mappings.len(), 1);
    assert_eq!(
        output.document().source_mappings[0].kind,
        SourceMappingKind::GeneratedToOrigin
    );
    let mapping = &output.document().source_mappings[0];
    let provenance = output
        .document()
        .provenance
        .iter()
        .find(|record| record.id == mapping.provenance)
        .expect("mapping provenance is present");
    let expected_rule = request.inputs()[1].origins()[0].provenance_rule();
    assert_eq!(provenance.producer_kind, ProducerKind::Derivation);
    assert_eq!(
        provenance.binary_digest,
        content_hash(b"fixture-parser-binary")
    );
    assert_eq!(provenance.rule.as_deref(), Some(expected_rule.as_str()));
    assert_eq!(
        provenance.evidence_sources,
        [mapping.from.clone(), mapping.to.clone()]
    );
    assert_eq!(mapping.evidence.source.as_ref(), Some(&mapping.from));
    assert_eq!(
        mapping.evidence.derivation,
        [FactRef::File(mapping.to.span().file())]
    );
    let coverage = output
        .document()
        .coverage_records
        .iter()
        .find(|record| {
            record.scope == rootlight_ir::CoverageScope::File(fixture.snapshots[1].file())
                && record.domain == FactDomain::SourceMappings
        })
        .expect("generated mapping coverage is present");
    assert_eq!(coverage.status, CoverageStatus::Complete);
    assert_eq!(
        (coverage.discovered, coverage.indexed, coverage.skipped),
        (1, 1, 0)
    );
}

#[test]
fn uncovered_generated_bytes_keep_source_mapping_coverage_unknown() {
    let fixture = ProjectFixture::new(
        ["src/generator.rs", "src/api.generated.rs"],
        [
            "pub fn generate() {}\n",
            "// Code generated by fixture-gen. DO NOT EDIT.\n// source: src/generator.rs\npub fn api() {}\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    let limits = limits();
    let generated_length = fixture.snapshots[1].content().len();
    let analyzer = analyzer(SemanticProjectLanguage::Rust, fixture.build_context);

    for mapped_end in [None, Some(generated_length - 1)] {
        let request = fixture.request_with_generated_mapping(&limits, mapped_end);
        let output = execute_project_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("incomplete generated origin analysis remains usable");
        let coverage = output
            .document()
            .coverage_records
            .iter()
            .find(|record| {
                record.scope == rootlight_ir::CoverageScope::File(fixture.snapshots[1].file())
                    && record.domain == FactDomain::SourceMappings
            })
            .expect("generated mapping coverage is present");

        assert_eq!(
            output.report().work().coverage().status(),
            CoverageStatus::Unknown
        );
        assert_eq!(coverage.status, CoverageStatus::Unknown);
        assert_eq!(coverage.skipped, 1);
    }
}

#[test]
fn ambiguity_is_preserved_and_output_is_canonical() {
    let fixture = ProjectFixture::new(
        ["src/dep.ts", "src/main.ts"],
        [
            "export function ping() {}\nexport function ping(value: number) {}\n",
            "import { ping } from \"./dep\";\nexport function run() { ping(); }\n",
        ],
        SemanticProjectLanguage::TypeScript,
    );
    let limits = limits();
    let request = fixture.request(&limits, AnalysisTier::TierB);
    let analyzer = analyzer(SemanticProjectLanguage::TypeScript, fixture.build_context);

    let first = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("ambiguous project commits");
    let second = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("repeat project commits");

    let candidates = first
        .document()
        .occurrences
        .iter()
        .find_map(|occurrence| match &occurrence.target {
            OccurrenceTarget::Candidates {
                symbols,
                total_count,
                completeness,
            } if occurrence.role == OccurrenceRole::CallSite => {
                Some((symbols, total_count, completeness))
            }
            _ => None,
        })
        .expect("ambiguous call retains candidates");
    assert_eq!(candidates.0.len(), 2);
    assert_eq!(*candidates.1, 2);
    assert_eq!(*candidates.2, CoverageStatus::Complete);
    assert_eq!(first.document(), second.document());
}

#[test]
fn malformed_source_commits_partial_diagnostics() {
    let fixture = ProjectFixture::new(
        ["dep.py", "main.py"],
        [
            "def ping():\n    pass\n",
            "from dep import ping\ndef run(:\n    ping()\n# BROKEN\n",
        ],
        SemanticProjectLanguage::Python,
    );
    let limits = limits();
    let request = fixture.request(&limits, AnalysisTier::TierB);
    let analyzer = analyzer(SemanticProjectLanguage::Python, fixture.build_context);

    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("recoverable malformed input commits partial facts");

    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(!output.document().diagnostics.is_empty());
    assert!(!output.document().skipped_regions.is_empty());
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    );
}

#[test]
fn oversized_project_fact_sets_commit_bounded_tier_b_output() {
    let repeated_calls = format!(
        "{}def ping():\n    pass\ndef ping():\n    pass\n",
        "ping()\n".repeat(6_000)
    );
    let fixture = ProjectFixture::new(
        ["dep.py", "main.py"],
        [repeated_calls.as_str(), "ping()\n"],
        SemanticProjectLanguage::Python,
    );
    let limits = limits();
    let request = fixture.request(&limits, AnalysisTier::TierB);
    let analyzer = analyzer(SemanticProjectLanguage::Python, fixture.build_context);

    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("oversized project facts commit bounded output");

    assert_eq!(
        output.report().work().coverage().tier(),
        AnalysisTier::TierB
    );
    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(output.document().diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "project-syntax-fact-limit"
            && diagnostic.coverage_effect == CoverageStatus::Bounded
    }));
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.display_name == "ping")
            .count(),
        1,
        "late declarations remain available to exact symbol lookup"
    );
    assert!(
        output
            .document()
            .occurrences
            .len()
            .checked_add(output.document().entities.len())
            .is_some_and(|facts| facts < 6_000)
    );
}

#[test]
fn build_context_cancellation_and_output_quota_fail_closed() {
    let fixture = ProjectFixture::new(
        ["src/dep.rs", "src/main.rs"],
        [
            "pub fn ping() {}\n",
            "use crate::dep::ping;\npub fn run() { ping(); }\n",
        ],
        SemanticProjectLanguage::Rust,
    );
    let limits = limits();
    let request = fixture.request(&limits, AnalysisTier::TierB);
    let wrong_context = BuildContextIdentity::new(content_hash(b"wrong-context"));
    let wrong_analyzer = analyzer(SemanticProjectLanguage::Rust, wrong_context);
    let error = execute_project_analysis(
        &wrong_analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect_err("context substitution fails");
    assert!(matches!(error, AdapterError::ProviderFailed { .. }));

    let analyzer = analyzer(SemanticProjectLanguage::Rust, fixture.build_context);
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    let error = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancelled,
    )
    .expect_err("pre-cancelled project does not commit");
    assert_eq!(
        error,
        AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest,
        }
    );

    let tight_limits = limits_with_output_bytes(1);
    let tight_request = fixture.request(&tight_limits, AnalysisTier::TierB);
    let error = execute_project_analysis(
        &analyzer,
        &tight_request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect_err("output quota prevents partial commit");
    assert!(matches!(error, AdapterError::Sink(_)));
}

#[derive(Clone)]
struct FixtureParser {
    capabilities: ParseCapabilities,
}

impl FixtureParser {
    fn new() -> Self {
        let languages = ["rust", "typescript", "javascript", "python", "go"]
            .into_iter()
            .map(|language| LanguageId::new(language).expect("language fixture is valid"))
            .collect();
        Self {
            capabilities: ParseCapabilities::new(
                languages,
                vec![EncodingId::utf8()],
                64 * 1024,
                64 * 1024,
                16 * 1024,
                256,
                false,
                true,
                true,
                1,
                MemoryEnforcement::Unavailable,
            )
            .expect("fixture capabilities are valid"),
        }
    }
}

impl ParseProvider for FixtureParser {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        cancellation.check()?;
        let source =
            std::str::from_utf8(request.source().bytes()).expect("fixture sources are valid UTF-8");
        let file = request.source().source_ref().span().file();
        let mut facts = fixture_facts(source, file, request.language().as_str());
        let malformed = source.contains("BROKEN");
        let mut diagnostics = Vec::new();
        if malformed {
            let full = request.source().source_ref().clone();
            facts.push(SyntaxFact::new(
                next_local_id(&facts),
                Some(1),
                SyntaxFactKind::ErrorRecovery,
                full.span(),
                1,
                SyntaxKindLabel::new("fixture.error").expect("label is valid"),
            ));
            diagnostics.push(AdapterDiagnostic::new(
                DiagnosticCode::new("fixture-parse-error").expect("code is valid"),
                DiagnosticSeverity::Error,
                Some(full),
                CoverageStatus::Bounded,
            ));
        }
        facts.sort_by_key(|fact| (fact.span(), fact.local_id()));
        for chunk in facts.chunks(256) {
            sink.push(rootlight_adapter_sdk::SyntaxFactBatch::new(
                sink.next_sequence(),
                chunk.to_vec(),
                std::mem::take(&mut diagnostics),
            ))?;
        }
        let usage = sink.staged_usage();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            if malformed {
                CoverageStatus::Bounded
            } else {
                CoverageStatus::Complete
            },
            source.len(),
            if malformed { 0 } else { source.len() },
            usize::from(malformed),
            Vec::new(),
        )?;
        WorkReport::new(
            coverage,
            ResourceUsage::new(
                source.len(),
                facts.len(),
                facts.len(),
                facts.iter().map(SyntaxFact::depth).max().unwrap_or(0),
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}

fn fixture_facts(source: &str, file: rootlight_ids::FileId, language: &str) -> Vec<SyntaxFact> {
    let full = span(file, 0, source.len());
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        full,
        0,
        SyntaxKindLabel::new("fixture.root").expect("label is valid"),
    )];
    let mut local_id = 2_u64;
    if let Some((start, line)) = source
        .lines()
        .scan(0_usize, |offset, line| {
            let start = *offset;
            *offset += line.len() + 1;
            Some((start, line))
        })
        .find(|(_, line)| {
            let trimmed = line.trim_start();
            trimmed.starts_with("use ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("from ")
        })
    {
        facts.push(SyntaxFact::new(
            local_id,
            Some(1),
            SyntaxFactKind::Import,
            span(file, start, start + line.len()),
            1,
            SyntaxKindLabel::new("fixture.import").expect("label is valid"),
        ));
        local_id += 1;
    }

    for name in ["ping", "Ping", "run", "Run"] {
        for (offset, _) in source.match_indices(name) {
            let line_start = source
                .get(..offset)
                .and_then(|prefix| prefix.rfind('\n').map(|index| index + 1))
                .unwrap_or(0);
            let line_end = source
                .get(offset..)
                .and_then(|tail| tail.find('\n').map(|index| offset + index))
                .unwrap_or(source.len());
            let line = source.get(line_start..line_end).unwrap_or_default();
            let before = source.get(line_start..offset).unwrap_or_default();
            let declaration = ["fn ", "function ", "def ", "func ", "class "]
                .iter()
                .any(|keyword| before.trim_start().ends_with(keyword));
            if declaration {
                let declaration_id = local_id;
                facts.push(SyntaxFact::new(
                    declaration_id,
                    Some(1),
                    SyntaxFactKind::Declaration,
                    span(file, line_start, line_end),
                    1,
                    SyntaxKindLabel::new("fixture.declaration").expect("label is valid"),
                ));
                local_id += 1;
                facts.push(SyntaxFact::new(
                    local_id,
                    Some(declaration_id),
                    SyntaxFactKind::Occurrence,
                    span(file, offset, offset + name.len()),
                    2,
                    SyntaxKindLabel::new("fixture.identifier").expect("label is valid"),
                ));
                local_id += 1;
                if let Some(open) = line.find('(')
                    && let Some(close) = line.get(open..).and_then(|tail| tail.find(')'))
                {
                    facts.push(SyntaxFact::new(
                        local_id,
                        Some(declaration_id),
                        SyntaxFactKind::Signature,
                        span(file, line_start + open, line_start + open + close + 1),
                        2,
                        SyntaxKindLabel::new("fixture.signature").expect("label is valid"),
                    ));
                    local_id += 1;
                }
                facts.push(SyntaxFact::new(
                    local_id,
                    Some(declaration_id),
                    SyntaxFactKind::Scope,
                    span(file, line_start, line_end),
                    2,
                    SyntaxKindLabel::new("fixture.scope").expect("label is valid"),
                ));
                local_id += 1;
            } else if source
                .get(offset + name.len()..)
                .is_some_and(|tail| tail.trim_start().starts_with('('))
            {
                let call_start = qualified_call_start(before, line_start);
                facts.push(SyntaxFact::new(
                    local_id,
                    Some(1),
                    SyntaxFactKind::Occurrence,
                    span(file, call_start, offset + name.len()),
                    1,
                    SyntaxKindLabel::new(&format!("{language}.call")).expect("label is valid"),
                ));
                local_id += 1;
            }
        }
    }
    facts
}

fn qualified_call_start(before: &str, line_start: usize) -> usize {
    let bytes = before.as_bytes();
    let mut start = bytes.len();
    while start > 0
        && (bytes[start - 1].is_ascii_alphanumeric()
            || matches!(bytes[start - 1], b'_' | b'$' | b'.' | b':'))
    {
        start -= 1;
    }
    line_start.saturating_add(start)
}

fn next_local_id(facts: &[SyntaxFact]) -> u64 {
    facts
        .iter()
        .map(SyntaxFact::local_id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

fn span(file: rootlight_ids::FileId, start: usize, end: usize) -> SourceSpan {
    SourceSpan::new(
        file,
        u64::try_from(start).expect("fixture offset fits"),
        u64::try_from(end).expect("fixture offset fits"),
    )
    .expect("fixture span is ordered")
}

struct LanguageCase {
    language: SemanticProjectLanguage,
    paths: [&'static str; 2],
    sources: [&'static str; 2],
}

fn language_cases() -> [LanguageCase; 5] {
    [
        LanguageCase {
            language: SemanticProjectLanguage::Rust,
            paths: ["src/dep.rs", "src/main.rs"],
            sources: [
                "pub fn ping() {}\n",
                "use crate::dep::ping;\npub fn run() { ping(); }\n",
            ],
        },
        LanguageCase {
            language: SemanticProjectLanguage::TypeScript,
            paths: ["src/dep.ts", "src/main.ts"],
            sources: [
                "export function ping() {}\n",
                "import { ping } from \"./dep\";\nexport function run() { ping(); }\n",
            ],
        },
        LanguageCase {
            language: SemanticProjectLanguage::JavaScript,
            paths: ["src/dep.js", "src/main.js"],
            sources: [
                "export function ping() {}\n",
                "import { ping } from \"./dep\";\nexport function run() { ping(); }\n",
            ],
        },
        LanguageCase {
            language: SemanticProjectLanguage::Python,
            paths: ["dep.py", "main.py"],
            sources: [
                "def ping():\n    pass\n",
                "from dep import ping\ndef run():\n    ping()\n",
            ],
        },
        LanguageCase {
            language: SemanticProjectLanguage::Go,
            paths: ["dep/dep.go", "main/main.go"],
            sources: [
                "package dep\nfunc Ping() {}\n",
                "package main\nimport \"example/dep\"\nfunc Run() { dep.Ping() }\n",
            ],
        },
    ]
}

struct ProjectFixture {
    _temporary: TempDir,
    snapshots: Vec<SourceSnapshot>,
    sources: Vec<SourceRef>,
    language: SemanticProjectLanguage,
    build_context: BuildContextIdentity,
}

impl ProjectFixture {
    fn new(paths: [&str; 2], sources: [&str; 2], language: SemanticProjectLanguage) -> Self {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("temporary directory is available");
        for (path, source) in paths.into_iter().zip(sources) {
            let full = temporary.path().join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("fixture directory is created");
            }
            fs::write(full, source).expect("fixture source is written");
        }
        let repository_id = repository_id();
        let repository =
            RepositoryRoot::open(repository_id, temporary.path()).expect("fixture root opens");
        let generation = generation_id();
        let snapshots = paths
            .map(|path| RelativePath::parse(Path::new(path)).expect("path is valid"))
            .into_iter()
            .map(|path| {
                repository
                    .snapshot(&path, 64 * 1024)
                    .expect("snapshot succeeds")
            })
            .collect::<Vec<_>>();
        let sources = snapshots
            .iter()
            .map(|snapshot| {
                SourceRef::new(
                    repository_id,
                    generation,
                    span(snapshot.file(), 0, snapshot.content().len()),
                    snapshot.content_hash(),
                    None,
                )
            })
            .collect();
        Self {
            _temporary: temporary,
            snapshots,
            sources,
            language,
            build_context: BuildContextIdentity::new(content_hash(b"fixture-build-context")),
        }
    }

    fn request<'a>(
        &'a self,
        limits: &'a AnalysisLimits,
        tier: AnalysisTier,
    ) -> ProjectAnalysisRequest<'a> {
        let manifest = b"{\"target\":\"fixture\"}";
        let inputs = self
            .snapshots
            .iter()
            .zip(&self.sources)
            .map(|(snapshot, source)| {
                ProjectSourceInput::new(
                    GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
                    LanguageId::new(self.language.as_str()).expect("language is valid"),
                    EncodingId::utf8(),
                    false,
                    Vec::new(),
                )
            })
            .collect();
        ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.project").expect("unit is valid"),
            BuildTargetId::new("//fixture:project").expect("target is valid"),
            self.build_context,
            content_hash(manifest),
            manifest,
            inputs,
            tier,
            limits,
        )
        .expect("project request is valid")
    }

    fn request_with_generated_mapping<'a>(
        &'a self,
        limits: &'a AnalysisLimits,
        generated_end: Option<usize>,
    ) -> ProjectAnalysisRequest<'a> {
        let manifest = b"{\"target\":\"fixture\"}";
        let generated_mapping = generated_end.map(|generated_end| {
            GeneratedOriginMapping::new(
                span(self.snapshots[1].file(), 0, generated_end),
                self.snapshots[0].path().clone(),
                self.sources[0].span(),
                TransformationId::new("fixture-gen").expect("transformation is valid"),
                Some(content_hash(b"fixture-generator")),
            )
        });
        let inputs = self
            .snapshots
            .iter()
            .zip(&self.sources)
            .enumerate()
            .map(|(index, (snapshot, source))| {
                ProjectSourceInput::new(
                    GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
                    LanguageId::new(self.language.as_str()).expect("language is valid"),
                    EncodingId::utf8(),
                    index == 1,
                    if index == 1 {
                        generated_mapping.clone().into_iter().collect()
                    } else {
                        Vec::new()
                    },
                )
            })
            .collect();
        ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.generated-project").expect("unit is valid"),
            BuildTargetId::new("//fixture:generated-project").expect("target is valid"),
            self.build_context,
            content_hash(manifest),
            manifest,
            inputs,
            AnalysisTier::TierB,
            limits,
        )
        .expect("generated project request is valid")
    }
}

fn analyzer(
    language: SemanticProjectLanguage,
    build_context: BuildContextIdentity,
) -> SemanticProjectAnalyzer {
    SemanticProjectAnalyzer::new(
        language,
        Arc::new(FixtureParser::new()),
        ProducerIdentity::new(
            "rootlight-project-fixture",
            "1.0.0",
            content_hash(language.as_str().as_bytes()),
        )
        .expect("producer identity is valid"),
        content_hash(b"fixture-parser-binary"),
        build_context,
    )
    .expect("fixture analyzer constructs")
}

fn real_parser() -> TreeSitterProvider {
    let settings = ParserSettings::new(4096).expect("parser settings are valid");
    let config = RuntimeConfig::new(
        64 * 1024,
        64 * 1024,
        4096,
        256,
        256,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .expect("runtime configuration is valid");
    TreeSitterProvider::new(config).expect("audited provider initializes")
}

fn producer_identity() -> ProducerIdentity {
    ProducerIdentity::new(
        "rootlight-project-identity-test",
        "1.0.0",
        content_hash(b"project-identity-test-config"),
    )
    .expect("producer identity is valid")
}

fn real_parser_limits() -> AnalysisLimits {
    let batch =
        BatchThresholds::new(256, 4 * 1024 * 1024, 128, 128 * 1024).expect("batch is valid");
    let stream = StreamLimits::new(
        256,
        16 * 1024,
        4 * 1024 * 1024,
        1024,
        1024 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream is valid");
    AnalysisLimits::new(
        64 * 1024,
        64 * 1024,
        4096,
        256,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
    .with_project_limits(
        ProjectAnalysisLimits::new(16, 512 * 1024, 64 * 1024, 128, 128 * 1024, 256, 256)
            .expect("project limits are valid"),
    )
}

fn limits() -> AnalysisLimits {
    limits_with_output_bytes(4 * 1024 * 1024)
}

fn limits_with_output_bytes(max_output_bytes: usize) -> AnalysisLimits {
    let batch =
        BatchThresholds::new(256, max_output_bytes, 128, 128 * 1024).expect("batch is valid");
    let stream = StreamLimits::new(
        256,
        16 * 1024,
        max_output_bytes,
        1024,
        1024 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream is valid");
    AnalysisLimits::new(
        64 * 1024,
        64 * 1024,
        16 * 1024,
        256,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
    .with_project_limits(
        ProjectAnalysisLimits::new(16, 512 * 1024, 64 * 1024, 128, 128 * 1024, 256, 256)
            .expect("project limits are valid"),
    )
}

fn repository_id() -> RepositoryId {
    "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        .parse()
        .expect("repository identity parses")
}

fn generation_id() -> GenerationId {
    "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
        .parse()
        .expect("generation identity parses")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("deadline derives"),
    )
}
