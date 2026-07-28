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
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisUnitId, BatchThresholds,
    BuildTargetId, CoverageReport, DiagnosticCode, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport,
    ParseRequest, ProjectAnalysisLimits, ProjectAnalysisRequest, ProjectSourceInput, ResourceUsage,
    StreamEnd, StreamLimits, SyntaxFact, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
    WorkReport, execute_project_analysis,
};
use rootlight_adapters::{SemanticProjectAnalyzer, SemanticProjectLanguage};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, DiagnosticSeverity, ExtensionSupport,
    FILE_IDENTITY_CLAIM_NAMESPACE, FactDomain, IrLimits, OccurrenceRole, OccurrenceTarget,
    ProducerIdentity, RelationPredicate, SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceRef, SourceSpan,
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
        let sequence = sink.next_sequence();
        sink.push(rootlight_adapter_sdk::SyntaxFactBatch::new(
            sequence,
            facts.clone(),
            diagnostics,
        ))?;
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
                facts.push(SyntaxFact::new(
                    local_id,
                    Some(1),
                    SyntaxFactKind::Occurrence,
                    span(file, offset, offset + name.len()),
                    1,
                    SyntaxKindLabel::new(&format!("{language}.call")).expect("label is valid"),
                ));
                local_id += 1;
            }
        }
    }
    facts
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
                "package main\nimport \"example/dep\"\nfunc Run() { Ping() }\n",
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
