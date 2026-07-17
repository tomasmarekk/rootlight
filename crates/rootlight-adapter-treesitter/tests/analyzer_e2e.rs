//! Public-boundary integration contracts for the real Tree-sitter analyzer.
//!
//! The four audited grammars flow from VFS snapshots through parsing, lowering,
//! canonical normalized IR, and explicit validation without native parser types.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageAnalyzer, LanguageId, MemoryAdmissionPolicy,
    MemoryAdmissionStatus, MemoryEnforcement, ParseProvider, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, RepositoryId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageScope, CoverageStatus, EntityKind,
    ExtensionSupport, FactDomain, FactEvidence, IrDocument, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, RelationPredicate, SourceRef, SourceSpan,
    decode_ir_document, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SYNTAX_NODES: usize = 16_384;
const MAX_SYNTAX_DEPTH: usize = 128;
const BINARY_SEED: &[u8] = b"rootlight-treesitter-e2e-binary";
const BUILD_CONTEXT_SEED: &[u8] = b"rootlight-treesitter-e2e-build";
const CONFIGURATION_SEED: &[u8] = b"rootlight-treesitter-e2e-configuration";
const EXPECTED_DOMAINS: [FactDomain; 8] = [
    FactDomain::Files,
    FactDomain::Entities,
    FactDomain::Occurrences,
    FactDomain::Relations,
    FactDomain::Provenance,
    FactDomain::SourceMappings,
    FactDomain::Diagnostics,
    FactDomain::Extensions,
];

const CASES: [LanguageCase; 4] = [
    LanguageCase {
        name: "rust",
        path: "src/lib.rs",
        frontend: "tree-sitter-rust-0.24.2",
        source: include_str!("fixtures/structural/rust.rs"),
        generated: false,
        body_before: " {\n        let text = \"Ahoj 🌍\";\n        greet(name);\n        text\n    }",
        body_after: " {\r\n            let text = \"Ahoj 🌍\";\r\n            greet(name);\r\n            text\r\n    }",
    },
    LanguageCase {
        name: "python",
        path: "src/example.py",
        frontend: "tree-sitter-python-0.25.0",
        source: include_str!("fixtures/structural/python.py"),
        generated: true,
        body_before: "def greet(self, name):\n        \"\"\"Dokumentace funkce.\"\"\"\n        text = \"Ahoj 🌍\"\n        print(name)\n        \"samostatný řetězec není dokumentace\"\n        return text",
        body_after: "def greet(self, name):\r\n            \"\"\"Dokumentace funkce.\"\"\"\r\n            text = \"Ahoj 🌍\"\r\n            print(name)\r\n            \"samostatný řetězec není dokumentace\"\r\n            return text",
    },
    LanguageCase {
        name: "javascript",
        path: "src/example.js",
        frontend: "tree-sitter-javascript-0.25.0",
        source: include_str!("fixtures/structural/javascript.js"),
        generated: false,
        body_before: "greet(name) {\n    const text = \"Ahoj 🌍\";\n    console.log(name);\n    return text;\n  }",
        body_after: "greet(name) {\r\n        const text = \"Ahoj 🌍\";\r\n        console.log(name);\r\n        return text;\r\n  }",
    },
    LanguageCase {
        name: "java",
        path: "src/Greeter.java",
        frontend: "tree-sitter-java-0.23.5",
        source: include_str!("fixtures/structural/java.java"),
        generated: true,
        body_before: "String greet(String name) {\n        String text = \"Ahoj 🌍\";\n        return name + text;\n    }",
        body_after: "String greet(String name) {\r\n            String text = \"Ahoj 🌍\";\r\n            return name + text;\r\n    }",
    },
];

#[derive(Clone, Copy)]
struct LanguageCase {
    name: &'static str,
    path: &'static str,
    frontend: &'static str,
    source: &'static str,
    generated: bool,
    body_before: &'static str,
    body_after: &'static str,
}

#[test]
fn real_analyzer_produces_valid_deterministic_ir_for_all_grammars() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();

    for case in CASES {
        let fixture = Fixture::new(case, case.source.as_bytes());
        let analyzer = analyzer(&provider, case);
        assert_descriptor(&analyzer, case);
        let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);

        let first = analyze(&analyzer, &initial_request, &extensions);
        let repeated = analyze(&analyzer, &initial_request, &extensions);
        assert_eq!(
            first.document(),
            repeated.document(),
            "{} logical document changed on repeat",
            case.name
        );
        assert_eq!(
            first.report().coverage(),
            repeated.report().coverage(),
            "{} coverage report changed on repeat",
            case.name
        );
        assert_contract(&first, &fixture, case, &limits, &extensions);

        let variant_source = case.source.replacen(case.body_before, case.body_after, 1);
        assert_ne!(
            variant_source, case.source,
            "{} body fixture did not change",
            case.name
        );
        let variant = fixture.rewrite(variant_source.as_bytes());
        assert_eq!(
            fixture.snapshot.file(),
            variant.snapshot.file(),
            "{} VFS file identity changed after rewrite",
            case.name
        );
        assert_ne!(
            fixture.source.content_hash(),
            variant.source.content_hash(),
            "{} body rewrite did not change content",
            case.name
        );
        assert_ne!(
            fixture.source.generation(),
            variant.source.generation(),
            "{} body rewrite did not advance the generation",
            case.name
        );
        let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
        let reparsed = analyze(&analyzer, &variant_request, &extensions);
        assert_contract(&reparsed, &variant, case, &limits, &extensions);
        assert_eq!(
            symbol_ids(first.document()),
            symbol_ids(reparsed.document()),
            "{} symbol IDs changed after body-only whitespace/CRLF reparse",
            case.name
        );
    }
}

#[test]
fn real_analyzer_reports_invalid_utf8_without_source_material() {
    const SECRET: &str = "do-not-leak-this-source-material";
    let case = CASES[0];
    let mut bytes = SECRET.as_bytes().to_vec();
    bytes.push(0xff);
    let fixture = Fixture::new(case, &bytes);
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let limits = limits();
    let request = request(&fixture.snapshot, &fixture.source, case, &limits);

    let error = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect_err("invalid UTF-8 must fail before lowering");
    assert!(matches!(
        &error,
        AdapterError::ProviderFailed { code } if code.as_str() == "invalid-utf8"
    ));
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(SECRET));
    assert!(!rendered.contains(case.path));
}

#[test]
fn real_analyzer_distinguishes_same_declarations_in_sibling_scopes() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let cases = [
        (
            CASES[0],
            "struct A;\nstruct B;\nimpl A { fn same(&self) {} }\nimpl B { fn same(&self) {} }\n",
            "struct A;\r\nstruct B;\r\nimpl A { fn same(&self) {   } }\r\nimpl B { fn same(&self) {\r\n} }\r\n",
        ),
        (
            CASES[1],
            "if True:\n    def same():\n        return 1\nif False:\n    def same():\n        return 2\n",
            "if True:\r\n    def same():\r\n            return 1\r\nif False:\r\n    def same():\r\n            return 2\r\n",
        ),
        (
            CASES[2],
            "function outer() {\n  { const same = 1; }\n  { const same = 2; }\n}\n",
            "function outer() {\r\n    { const same = 1;   }\r\n    { const same = 2;\r\n    }\r\n}\r\n",
        ),
        (
            CASES[3],
            "class Outer { void run() { { int same = 1; } { int same = 2; } } }\n",
            "class Outer {\r\n  void run() {\r\n    { int same = 1;   }\r\n    { int same = 2;\r\n    }\r\n  }\r\n}\r\n",
        ),
    ];

    for (case, before, after) in cases {
        let fixture = Fixture::new(case, before.as_bytes());
        let analyzer = analyzer(&provider, case);
        let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);
        let initial = analyze(&analyzer, &initial_request, &extensions);
        validate_ir_document(initial.document(), limits.ir(), &extensions)
            .unwrap_or_else(|error| panic!("{} scoped IR must validate: {error}", case.name));
        let initial_ids = same_symbol_ids(initial.document());
        assert_eq!(
            initial_ids.len(),
            2,
            "{} sibling declarations must both survive lowering",
            case.name
        );
        assert_ne!(
            initial_ids[0], initial_ids[1],
            "{} sibling declarations must have distinct identities",
            case.name
        );

        let variant = fixture.rewrite(after.as_bytes());
        let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
        let reparsed = analyze(&analyzer, &variant_request, &extensions);
        validate_ir_document(reparsed.document(), limits.ir(), &extensions).unwrap_or_else(
            |error| panic!("{} reparsed scoped IR must validate: {error}", case.name),
        );
        assert_eq!(
            initial_ids,
            same_symbol_ids(reparsed.document()),
            "{} scoped symbol IDs changed after whitespace/CRLF-only edits",
            case.name
        );
    }
}

fn assert_descriptor(analyzer: &TreeSitterAnalyzer, case: LanguageCase) {
    let descriptor = analyzer.descriptor();
    assert_eq!(descriptor.identity(), &producer_identity());
    assert_eq!(descriptor.kind(), ProducerKind::Parser);
    assert_eq!(descriptor.language().as_str(), case.name);
    assert_eq!(descriptor.tier(), AnalysisTier::TierD);
    assert_eq!(
        descriptor.memory_enforcement(),
        MemoryEnforcement::Unavailable
    );
    assert!(descriptor.supports_noncritical_extensions());
}

fn assert_contract(
    output: &AnalysisOutput,
    fixture: &Fixture,
    case: LanguageCase,
    limits: &AnalysisLimits,
    extensions: &ExtensionSupport,
) {
    let document = output.document();
    validate_ir_document(document, limits.ir(), extensions)
        .unwrap_or_else(|error| panic!("{} normalized IR must validate: {error}", case.name));
    let encoded = serde_json::to_vec(document).expect("normalized IR encodes");
    let decoded =
        decode_ir_document(&encoded, limits.ir(), extensions).expect("bounded IR decode succeeds");
    assert_eq!(decoded, IrDocument::NormalizedV1_1(document.clone()));

    assert_eq!(
        output.memory_admission(),
        MemoryAdmissionStatus::UnavailableM05Fallback
    );
    assert_eq!(document.repository, fixture.source.repository());
    assert_eq!(document.generation, fixture.source.generation());
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.id, fixture.snapshot.file());
    assert_eq!(file.repository, fixture.source.repository());
    assert_eq!(file.generation, fixture.source.generation());
    assert_eq!(file.path, case.path);
    assert_eq!(file.content_hash, fixture.source.content_hash());
    assert_eq!(file.byte_length, fixture.source.span().end_byte());
    assert_eq!(file.language, case.name);
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.generated, case.generated);
    assert_eq!(file.evidence, direct_evidence(&fixture.source));

    assert_eq!(document.provenance.len(), 1);
    let provenance = &document.provenance[0];
    assert_eq!(file.provenance, provenance.id);
    assert_eq!(provenance.repository, fixture.source.repository());
    assert_eq!(provenance.generation, fixture.source.generation());
    assert_eq!(provenance.producer_kind, ProducerKind::Parser);
    assert_eq!(provenance.producer, producer_identity());
    assert_eq!(provenance.binary_digest, content_hash(BINARY_SEED));
    assert_eq!(provenance.frontend_version.as_deref(), Some(case.frontend));
    assert_eq!(provenance.language, case.name);
    assert_eq!(provenance.tier, AnalysisTier::TierD);
    assert_eq!(provenance.build_context, build_context());
    assert_eq!(provenance.input_sources, vec![fixture.source.clone()]);
    assert_eq!(provenance.evidence_sources, vec![fixture.source.clone()]);
    assert!(provenance.derivation_parents.is_empty());
    assert_eq!(provenance.rule, None);

    let coverage = output.report().coverage();
    assert_eq!(coverage.tier(), AnalysisTier::TierD);
    assert_eq!(coverage.status(), CoverageStatus::Bounded);
    assert_eq!(
        coverage.total_source_bytes(),
        fixture.snapshot.content().len()
    );
    assert_eq!(
        coverage.covered_source_bytes(),
        fixture.snapshot.content().len()
    );
    assert_eq!(coverage.skipped_regions(), document.skipped_regions.len());
    assert_eq!(
        coverage
            .domains()
            .iter()
            .map(rootlight_adapter_sdk::DomainCoverage::domain)
            .collect::<Vec<_>>(),
        EXPECTED_DOMAINS
    );
    let records_by_domain: BTreeMap<_, _> = document
        .coverage_records
        .iter()
        .map(|record| (record.domain, record))
        .collect();
    assert_eq!(
        records_by_domain.keys().copied().collect::<Vec<_>>(),
        EXPECTED_DOMAINS
    );
    for reported in coverage.domains() {
        let record = records_by_domain
            .get(&reported.domain())
            .expect("every reported domain has a normalized coverage record");
        let skipped_in_domain = document
            .skipped_regions
            .iter()
            .filter(|region| region.domain == reported.domain())
            .count();
        let indexed_in_domain = match reported.domain() {
            FactDomain::Files => document.files.len(),
            FactDomain::Entities => document.entities.len(),
            FactDomain::Occurrences => document.occurrences.len(),
            FactDomain::Relations => document.relations.len(),
            FactDomain::Provenance => document.provenance.len(),
            FactDomain::SourceMappings => 0,
            FactDomain::Diagnostics => document.diagnostics.len(),
            FactDomain::Extensions => document.extensions.len(),
        };
        assert_eq!(record.scope, CoverageScope::File(file.id));
        assert_eq!(record.domain, reported.domain());
        assert_eq!(record.tier, AnalysisTier::TierD);
        assert_eq!(record.status, reported.status());
        assert_eq!(reported.skipped(), skipped_in_domain);
        assert_eq!(reported.indexed(), indexed_in_domain);
        assert_eq!(
            reported.discovered(),
            indexed_in_domain
                .checked_add(skipped_in_domain)
                .expect("domain accounting fits")
        );
        if skipped_in_domain > 0 {
            assert_eq!(reported.status(), CoverageStatus::Bounded);
        }
        assert_eq!(
            (record.discovered, record.indexed, record.skipped),
            (
                u64::try_from(reported.discovered()).expect("discovered count fits"),
                u64::try_from(reported.indexed()).expect("indexed count fits"),
                u64::try_from(reported.skipped()).expect("skipped count fits"),
            )
        );
        assert_eq!(record.provenance, provenance.id);
        assert_eq!(record.evidence, direct_evidence(&fixture.source));
    }
    assert!(document.skipped_regions.iter().any(|region| {
        region.domain == FactDomain::Relations && region.detail == "unresolved-import-target"
    }));
    assert!(document.entities.iter().all(|entity| {
        entity.tier == AnalysisTier::TierD
            && entity.provenance == provenance.id
            && entity.evidence.source.is_some()
    }));
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    if matches!(case.name, "python" | "javascript") {
        let file_module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == case.path)
            .expect("implicit file module is emitted");
        assert!(document.occurrences.iter().all(|occurrence| {
            occurrence.role != OccurrenceRole::Definition
                || !matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol } if symbol == file_module.id
                )
        }));
    }
}

fn same_symbol_ids(document: &rootlight_ir::NormalizedIrDocument) -> Vec<SymbolId> {
    let mut ids = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same")
        .map(|entity| entity.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn symbol_ids(
    document: &rootlight_ir::NormalizedIrDocument,
) -> BTreeMap<(EntityKind, String, String), SymbolId> {
    let symbols: BTreeMap<_, _> = document
        .entities
        .iter()
        .map(|entity| {
            (
                (
                    entity.kind,
                    entity.language.clone(),
                    entity.qualified_name.clone(),
                ),
                entity.id,
            )
        })
        .collect();
    assert!(
        !symbols.is_empty(),
        "fixture must produce semantic entities"
    );
    symbols
}

fn analyzer(provider: &Arc<TreeSitterProvider>, case: LanguageCase) -> TreeSitterAnalyzer {
    let parser: Arc<dyn ParseProvider> = provider.clone();
    TreeSitterAnalyzer::new(
        parser,
        producer_identity(),
        language(case),
        case.frontend,
        content_hash(BINARY_SEED),
    )
    .expect("analyzer configuration is valid")
}

fn analyze(
    analyzer: &TreeSitterAnalyzer,
    request: &AnalysisRequest<'_>,
    extensions: &ExtensionSupport,
) -> AnalysisOutput {
    execute_analysis(
        analyzer,
        request,
        extensions.clone(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("real analyzer commits canonical IR")
}

fn request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    case: LanguageCase,
    limits: &'a AnalysisLimits,
) -> AnalysisRequest<'a> {
    AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds to source"),
        language(case),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        limits,
    )
    .expect("analysis request is valid")
    .with_generated_status(case.generated)
}

fn provider() -> TreeSitterProvider {
    let settings = ParserSettings::new(4096).expect("parser settings are valid");
    let config = RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        64,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .expect("runtime configuration is valid");
    TreeSitterProvider::new(config).expect("audited provider initializes")
}

fn limits() -> AnalysisLimits {
    let batch =
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).expect("batch limits are valid");
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
}

fn producer_identity() -> ProducerIdentity {
    ProducerIdentity::new(
        "rootlight-treesitter-e2e",
        "1.0",
        content_hash(CONFIGURATION_SEED),
    )
    .expect("producer identity is valid")
}

fn build_context() -> BuildContextIdentity {
    BuildContextIdentity::new(content_hash(BUILD_CONTEXT_SEED))
}

fn language(case: LanguageCase) -> LanguageId {
    LanguageId::new(case.name).expect("language identity is valid")
}

fn direct_evidence(source: &SourceRef) -> FactEvidence {
    FactEvidence {
        source: Some(source.clone()),
        derivation: Vec::new(),
    }
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    )
}

struct Fixture {
    temporary: Arc<TempDir>,
    relative: RelativePath,
    repository: RepositoryId,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(case: LanguageCase, bytes: &[u8]) -> Self {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary =
            Arc::new(tempdir_in(current).expect("local temporary directory is available"));
        let relative = RelativePath::parse(Path::new(case.path)).expect("fixture path is valid");
        if let Some(parent) = Path::new(case.path).parent() {
            fs::create_dir_all(temporary.path().join(parent))
                .expect("fixture source directory is created");
        }
        fs::write(temporary.path().join(case.path), bytes).expect("fixture source is written");
        let repository = derive_repository(case.name.as_bytes()).id();
        let (snapshot, source) = capture(
            &temporary,
            repository,
            GenerationId::from_bytes([17; 20]),
            &relative,
        );
        Self {
            temporary,
            relative,
            repository,
            snapshot,
            source,
        }
    }

    fn rewrite(&self, bytes: &[u8]) -> Self {
        fs::write(self.temporary.path().join(self.relative.as_str()), bytes)
            .expect("updated fixture source is written");
        let (snapshot, source) = capture(
            &self.temporary,
            self.repository,
            GenerationId::from_bytes([18; 20]),
            &self.relative,
        );
        Self {
            temporary: Arc::clone(&self.temporary),
            relative: self.relative.clone(),
            repository: self.repository,
            snapshot,
            source,
        }
    }
}

fn capture(
    temporary: &TempDir,
    repository_id: RepositoryId,
    generation: GenerationId,
    relative: &RelativePath,
) -> (SourceSnapshot, SourceRef) {
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let snapshot = repository
        .snapshot(
            relative,
            u64::try_from(MAX_SOURCE_BYTES).expect("snapshot limit fits"),
        )
        .expect("fixture snapshot is stable");
    let byte_length = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    let source = SourceRef::new(
        repository_id,
        generation,
        SourceSpan::new(snapshot.file(), 0, byte_length).expect("full span is ordered"),
        snapshot.content_hash(),
        None,
    );
    (snapshot, source)
}
