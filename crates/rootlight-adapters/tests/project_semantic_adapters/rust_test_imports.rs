//! Static import relevance requires authored test-module ownership.
//! A module import can make a test a compilation-dependent candidate even when
//! its body does not use that import; no macro expansion or Calls is inferred.

use super::*;
use rootlight_ir::RelationEndpoint;

const ROOT: &str =
    "pub mod commands;\n#[cfg(test)]\n#[path = \"../tests/commands.rs\"]\nmod command_tests;\n";
const COMMANDS: &str = "#[derive(Clone, Copy)]\npub enum DenoSubcommand { Run, Check }\npub fn run_subcommand(command: DenoSubcommand) -> bool {\n    matches!(command, DenoSubcommand::Run)\n}\n";
const TEST: &str = "use crate::commands::DenoSubcommand;\n#[test]\nfn deno_subcommand_routes_run() {\n    assert!(matches!(DenoSubcommand::Run, DenoSubcommand::Run));\n}\n";

fn fixture(root: &str, commands: &str, test: &str) -> ProjectFixture {
    ProjectFixture::new(
        ["src/lib.rs", "src/commands.rs", "tests/commands.rs"],
        [root, commands, test],
        SemanticProjectLanguage::Rust,
    )
}

fn static_imports(
    document: &rootlight_ir::NormalizedIrDocument,
) -> Vec<&rootlight_ir::RelationRecord> {
    document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Imports
                && matches!(relation.subject, RelationEndpoint::Entity(_))
        })
        .collect()
}

#[test]
fn explicitly_attached_original_test_inherits_static_import_relevance() {
    let fixture = fixture(ROOT, COMMANDS, TEST);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let imports = static_imports(document);
    assert_eq!(imports.len(), 1, "{document:#?}");
    let test = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "deno_subcommand_routes_run")
        .unwrap();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "DenoSubcommand" && entity.kind == EntityKind::Enum)
        .unwrap();
    assert_eq!(imports[0].subject, RelationEndpoint::Entity(test.id));
    assert_eq!(imports[0].object, RelationEndpoint::Entity(target.id));
    let source = imports[0].evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(TEST.as_bytes()));
    assert_eq!(source.span().file(), fixture.snapshots[2].file());
    assert_eq!(
        &TEST.as_bytes()[usize::try_from(source.span().start_byte()).unwrap()
            ..usize::try_from(source.span().end_byte()).unwrap()],
        b"use crate::commands::DenoSubcommand;"
    );
    for file in &document.files {
        assert!(
            imports[0]
                .evidence
                .derivation
                .contains(&rootlight_ir::FactRef::File(file.id))
        );
        assert!(
            imports[0]
                .evidence
                .derivation
                .contains(&rootlight_ir::FactRef::Fact(file.provenance))
        );
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source == *source
                && gap.detail == "rust-test-scope-import-runtime-usage-unverified")
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(
        document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role != OccurrenceRole::Definition
                && occurrence.source.span().file() == fixture.snapshots[2].file()
                && occurrence.source.span().start_byte() > 80)
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn unattached_original_and_uncertain_module_or_import_ownership_stay_unproved() {
    let cases = [
        ("pub mod commands;", COMMANDS, TEST),
        (
            "pub mod commands; #[path = \"../../tests/commands.rs\"] mod command_tests;",
            COMMANDS,
            TEST,
        ),
        (
            "pub mod commands; #[path = \"/tests/commands.rs\"] mod command_tests;",
            COMMANDS,
            TEST,
        ),
        (
            "pub mod commands; #[path = \"../tests/missing.rs\"] mod command_tests;",
            COMMANDS,
            TEST,
        ),
        (
            "pub mod commands; #[path = \"../tests/commands.rs\"] #[path = \"../tests/commands.rs\"] mod command_tests;",
            COMMANDS,
            TEST,
        ),
        (
            "pub mod commands; #[cfg(feature = \"other\")] #[path = \"../tests/commands.rs\"] mod command_tests;",
            COMMANDS,
            TEST,
        ),
        (
            "pub mod commands; #[path = \"../tests/commands.rs\"] mod command_tests; mod command_tests;",
            COMMANDS,
            TEST,
        ),
        (
            ROOT,
            "pub enum DenoSubcommand { Run } pub enum DenoSubcommand { Check }",
            TEST,
        ),
        (
            ROOT,
            "pub struct DenoSubcommand; pub enum DenoSubcommand { Check }",
            TEST,
        ),
        (ROOT, "enum DenoSubcommand { Run }", TEST),
        (
            ROOT,
            COMMANDS,
            "use crate::commands::*; #[test] fn selected() {}",
        ),
        (
            ROOT,
            COMMANDS,
            "use crate::commands::DenoSubcommand; use crate::commands::DenoSubcommand; #[test] fn selected() {}",
        ),
        (
            ROOT,
            COMMANDS,
            "use crate::commands::DenoSubcommand; struct DenoSubcommand; #[test] fn selected() {}",
        ),
        (
            ROOT,
            COMMANDS,
            "inject!(); use crate::commands::DenoSubcommand; #[test] fn selected() {}",
        ),
        (
            ROOT,
            COMMANDS,
            "use crate::commands::DenoSubcommand; #[custom] #[test] fn selected() {}",
        ),
    ];
    for (root, commands, test) in cases {
        let fixture = fixture(root, commands, test);
        let output = analyze_with_real_parser(&fixture);
        assert!(
            static_imports(output.document()).is_empty(),
            "{root}\n{commands}\n{test}"
        );
    }
}

#[test]
fn unused_and_renamed_module_imports_are_static_candidates_without_runtime_calls() {
    for test in [
        "use crate::commands::DenoSubcommand; #[test] fn selected() {}",
        "use crate::commands::DenoSubcommand as Command; #[test] fn selected() { let Command = 0; }",
    ] {
        let fixture = fixture(ROOT, COMMANDS, test);
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        assert_eq!(static_imports(document).len(), 1, "{test}");
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn path_overrides_do_not_supply_unqualified_nested_module_layouts() {
    for (root, test, physical, unrelated) in [
        (
            ROOT,
            "mod nested; use self::nested::Target; #[test] fn chosen() {}",
            "tests/nested.rs",
            "src/command_tests/nested.rs",
        ),
        (
            ROOT,
            "mod inline { mod nested; use self::nested::Target; #[test] fn chosen() {} }",
            "tests/inline/nested.rs",
            "src/command_tests/inline/nested.rs",
        ),
        (
            "pub mod commands; #[path = \"../tests\"] mod command_tests { mod nested; use self::nested::Target; #[test] fn chosen() {} }",
            "fn unrelated() {}",
            "tests/nested.rs",
            "src/command_tests/nested.rs",
        ),
    ] {
        let fixture = ProjectFixture::new(
            [
                "src/lib.rs",
                "src/commands.rs",
                "tests/commands.rs",
                physical,
                unrelated,
            ],
            [
                root,
                COMMANDS,
                test,
                "pub struct Target;",
                "pub struct Target;",
            ],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        assert!(static_imports(output.document()).is_empty(), "{test}");
    }
}

struct MutatedImportParser(u8);

impl ParseProvider for MutatedImportParser {
    fn capabilities(&self) -> &ParseCapabilities {
        static PARSER: std::sync::OnceLock<TreeSitterProvider> = std::sync::OnceLock::new();
        PARSER.get_or_init(real_parser).capabilities()
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let output = rootlight_adapter_sdk::execute_parse(
            &real_parser(),
            request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        let text = std::str::from_utf8(request.source().bytes()).unwrap();
        let definitions: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str().ends_with(".definition")
                    && text.get(
                        usize::try_from(fact.span().start_byte()).unwrap()
                            ..usize::try_from(fact.span().end_byte()).unwrap(),
                    ) == Some("Target")
            })
            .collect();
        let first = definitions.first().map(|fact| fact.local_id());
        let last = definitions.last().map(|fact| fact.local_id());
        let target_file = text.starts_with("pub enum Target");
        let facts: Vec<_> = output
            .facts()
            .iter()
            .filter_map(|fact| {
                if target_file && self.0 == 2 && Some(fact.local_id()) == last {
                    return None;
                }
                if self.0 == 3 && fact.syntax_kind().as_str() == "rust.proof_context.signature" {
                    return None;
                }
                if self.0 == 4
                    && fact.syntax_kind().as_str() == "rust.attribute.signature"
                    && text.get(
                        usize::try_from(fact.span().start_byte()).unwrap()
                            ..usize::try_from(fact.span().end_byte()).unwrap(),
                    ) == Some("#[custom]")
                {
                    return None;
                }
                if target_file && self.0 == 1 && Some(fact.local_id()) == first {
                    let start = text.find("{ Target").unwrap() + 2;
                    return Some(SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        span(fact.span().file(), start, start + 6),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    ));
                }
                Some(fact.clone())
            })
            .collect();
        for chunk in facts.chunks(256) {
            sink.push(rootlight_adapter_sdk::SyntaxFactBatch::new(
                sink.next_sequence(),
                chunk.to_vec(),
                Vec::new(),
            ))?;
        }
        let length = request.source().bytes().len();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Complete,
            length,
            length,
            0,
            Vec::new(),
        )?;
        let usage = sink.staged_usage();
        WorkReport::new(
            coverage,
            ResourceUsage::new(
                length,
                facts.len(),
                output.report().resources().syntax_nodes(),
                facts.iter().map(SyntaxFact::depth).max().unwrap_or(0),
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}

#[test]
fn static_test_imports_reject_shifted_or_withheld_target_and_missing_context() {
    let root = "pub mod commands; #[path = \"../tests/commands.rs\"] mod command_tests;";
    let test = "use crate::commands::Target; #[test] fn selected() {}";
    for (mode, source, expected) in [
        (0, "pub enum Target { Target }", 1),
        (1, "pub enum Target { Target }", 0),
        (2, "pub enum Target { First } pub enum Target { Second }", 0),
        (3, "pub enum Target { Target }", 0),
        (4, "#[custom] pub enum Target { Target }", 0),
    ] {
        let fixture = fixture(root, source, test);
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            Arc::new(MutatedImportParser(mode)),
            producer_identity(),
            content_hash(b"mutated-test-import-parser"),
            fixture.build_context,
        )
        .unwrap();
        let limits = real_parser_limits();
        let output = execute_project_analysis(
            &analyzer,
            &fixture.request(&limits, AnalysisTier::TierB),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            static_imports(output.document()).len(),
            expected,
            "mode {mode}"
        );
    }
}

#[test]
fn static_test_imports_keep_relation_limits_and_cancellation_strict() {
    let fixture = fixture(ROOT, COMMANDS, TEST);
    let output = analyze_with_real_parser(&fixture);
    let required = output.document().relations.len();
    let mut ir = IrLimits::default();
    ir.max_relations = required;
    let limits = native_limits(ir.clone());
    assert_eq!(
        static_imports(analyze_with_real_parser_limits(&fixture, &limits).document()).len(),
        1
    );
    ir.max_relations = required - 1;
    let limits = native_limits(ir);
    let analyzer = SemanticProjectAnalyzer::new(
        SemanticProjectLanguage::Rust,
        Arc::new(real_parser()),
        producer_identity(),
        content_hash(b"real-parser-binary"),
        fixture.build_context,
    )
    .unwrap();
    assert!(
        execute_project_analysis(
            &analyzer,
            &fixture.request(&limits, AnalysisTier::TierB),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        )
        .is_err()
    );
    let limits = real_parser_limits();
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(
        execute_project_analysis(
            &analyzer,
            &fixture.request(&limits, AnalysisTier::TierB),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        )
        .is_err()
    );
}

fn native_limits(ir: IrLimits) -> AnalysisLimits {
    let base = real_parser_limits();
    AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        ir,
    )
    .unwrap()
    .with_project_limits(base.project().unwrap())
}

#[test]
fn attached_test_imports_retain_encoded_physical_source_provenance() {
    for encoding in ["utf-8-bom", "utf-16le", "utf-16be"] {
        let mut fixture = fixture(ROOT, COMMANDS, TEST);
        let encode = |text: &str| -> Vec<u8> {
            match encoding {
                "utf-8-bom" => [0xef, 0xbb, 0xbf].into_iter().chain(text.bytes()).collect(),
                "utf-16le" => [0xff, 0xfe]
                    .into_iter()
                    .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                    .collect(),
                "utf-16be" => [0xfe, 0xff]
                    .into_iter()
                    .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
                    .collect(),
                _ => unreachable!(),
            }
        };
        let bytes = [encode(ROOT), encode(COMMANDS), encode(TEST)];
        let paths = ["src/lib.rs", "src/commands.rs", "tests/commands.rs"];
        for (path, bytes) in paths.into_iter().zip(&bytes) {
            fs::write(fixture._temporary.path().join(path), bytes).unwrap();
        }
        let repository = RepositoryRoot::open(repository_id(), fixture._temporary.path()).unwrap();
        fixture.snapshots = paths
            .into_iter()
            .map(|path| {
                repository
                    .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 64 * 1024)
                    .unwrap()
            })
            .collect();
        fixture.sources = fixture
            .snapshots
            .iter()
            .zip(&bytes)
            .map(|(snapshot, bytes)| {
                SourceRef::new(
                    repository_id(),
                    generation_id(),
                    span(snapshot.file(), 0, bytes.len()),
                    content_hash(bytes),
                    None,
                )
            })
            .collect();
        let mut inputs: Vec<_> = fixture
            .snapshots
            .iter()
            .zip(&fixture.sources)
            .map(|(snapshot, source)| {
                ProjectSourceInput::new(
                    GenerationBoundSnapshot::new(snapshot, source).unwrap(),
                    LanguageId::new("rust").unwrap(),
                    EncodingId::new(if encoding == "utf-8-bom" {
                        "utf-8"
                    } else {
                        encoding
                    })
                    .unwrap(),
                    false,
                    Vec::new(),
                )
            })
            .collect();
        inputs.sort_by(|left, right| {
            left.source()
                .path()
                .identity_bytes()
                .cmp(right.source().path().identity_bytes())
        });
        let limits = real_parser_limits();
        let manifest = b"{\"target\":\"fixture\"}";
        let request = ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.project").unwrap(),
            BuildTargetId::new("//fixture:project").unwrap(),
            fixture.build_context,
            content_hash(manifest),
            manifest,
            inputs,
            AnalysisTier::TierB,
            &limits,
        )
        .unwrap();
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            Arc::new(real_parser()),
            producer_identity(),
            content_hash(b"real-parser-binary"),
            fixture.build_context,
        )
        .unwrap();
        let output = execute_project_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let document = output.document();
        let imports = static_imports(document);
        assert_eq!(imports.len(), 1, "{encoding}");
        let source = imports[0].evidence.source.as_ref().unwrap();
        assert_eq!(source.content_hash(), content_hash(&bytes[2]));
        let start = usize::try_from(source.span().start_byte()).unwrap();
        let end = usize::try_from(source.span().end_byte()).unwrap();
        let import = "use crate::commands::DenoSubcommand;";
        let expected = encode(import);
        let bom = if encoding == "utf-8-bom" { 3 } else { 2 };
        assert_eq!(&bytes[2][start..end], &expected[bom..]);
        assert_eq!(start, bom);
    }
}
