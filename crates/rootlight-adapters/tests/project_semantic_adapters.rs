//! Public behavior tests for the audited whole-project semantic adapters.
//!
//! Fixture providers and native query packs exercise structural facts through
//! deterministic project contracts, including source-backed cross-file targets.

use std::{
    collections::{BTreeMap, BTreeSet},
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
use rootlight_adapters::{
    PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC, SemanticProjectAnalyzer, SemanticProjectLanguage,
};
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

#[path = "project_semantic_adapters/dart.rs"]
mod dart;

#[test]
fn native_ecmascript_import_paths_do_not_guess_escaped_specifiers() {
    let source = "import {Item} from './pro\\u0076ider'; const value = Item;\n";
    let fixture = ProjectFixture::new(
        ["src/main.ts", "src/provider.ts"],
        [source, "export class Item {}"],
        SemanticProjectLanguage::TypeScript,
    );
    let output = analyze_with_real_parser(&fixture);
    let gaps: Vec<_> = output
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "ecmascript-import-evidence-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].domain, FactDomain::Relations);
    assert_eq!(
        gaps[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    let start = u64::try_from(source.find("= Item;").unwrap() + 2).unwrap();
    let reference = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.file == fixture.snapshots[0].file()
                && occurrence.source.span().start_byte() == start
        })
        .unwrap();
    assert!(matches!(
        reference.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
}

#[test]
fn native_typescript_imports_preserve_type_modifiers_and_trivia() {
    let source = "import /* 'wrong-module' */ { type Item as Typed, Item as Value, type as ordinary }\nfrom './provider'; import type {Item as Whole} from './provider'; let first: Typed; let second: Whole; const valid = Value; const named = ordinary; const invalid = Typed; const alsoInvalid = Whole; type Constructor = typeof Typed;\n";
    let fixture = ProjectFixture::new(
        ["src/main.ts", "src/provider.ts"],
        [source, "export class Item {} export const type = 1;"],
        SemanticProjectLanguage::TypeScript,
    );
    let output = analyze_with_real_parser(&fixture);
    assert_eq!(
        output
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "ecmascript-type-only-value-use")
            .count(),
        2
    );
    let item = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Item")
        .unwrap()
        .id;
    let ordinary = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "type")
        .unwrap()
        .id;
    for (needle, offset, expected) in [
        ("first: Typed", 7, Some(item)),
        ("second: Whole", 8, Some(item)),
        ("= Value;", 2, Some(item)),
        ("= ordinary;", 2, Some(ordinary)),
        ("= Typed;", 2, None),
        ("= Whole;", 2, None),
        ("typeof Typed;", 7, Some(item)),
    ] {
        let start = u64::try_from(source.find(needle).unwrap() + offset).unwrap();
        let occurrence = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        if let Some(symbol) = expected {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol },
                "{needle}"
            );
        } else {
            assert!(
                matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                "{needle}: {occurrence:?}"
            );
        }
    }
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Imports
            && relation
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == fixture.snapshots[0].file())
    }));
}

#[test]
fn typescript_imported_references_preserve_both_declaration_namespaces() {
    let source = "import {Token as Local, Both} from './provider'; let typed: Local = Local; type Query = typeof Local; let instance: Both; const constructor = Both;\n";
    let provider =
        "export type Token = string; export const Token = 'value'; export class Both {}\n";
    let fixture = ProjectFixture::new(
        ["src/main.ts", "src/provider.ts"],
        [source, provider],
        SemanticProjectLanguage::TypeScript,
    );
    let output = analyze_with_real_parser(&fixture);
    for (needle, offset, name, role, kind) in [
        (
            "typed: Local",
            7,
            "Token",
            OccurrenceRole::TypeUse,
            EntityKind::TypeAlias,
        ),
        (
            "= Local;",
            2,
            "Token",
            OccurrenceRole::Reference,
            EntityKind::Variable,
        ),
        (
            "typeof Local",
            7,
            "Token",
            OccurrenceRole::Reference,
            EntityKind::Variable,
        ),
        (
            "instance: Both",
            10,
            "Both",
            OccurrenceRole::TypeUse,
            EntityKind::Class,
        ),
        (
            "= Both;",
            2,
            "Both",
            OccurrenceRole::Reference,
            EntityKind::Class,
        ),
    ] {
        let target = output
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == kind
                    && entity.canonical_name == name
                    && entity.evidence.source.as_ref().unwrap().span().file()
                        == fixture.snapshots[1].file()
            })
            .unwrap()
            .id;
        let start = u64::try_from(source.find(needle).unwrap() + offset).unwrap();
        let occurrence = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        assert_eq!(occurrence.role, role, "{needle}");
        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Resolved { symbol: target },
            "{needle}"
        );
    }
}

#[test]
fn typescript_export_names_do_not_become_local_value_references() {
    let source = "type Token = string; const Public = 1; export {Token}; export type {Token as Public}; export {Token as Remote} from './provider';\n";
    let fixture = ProjectFixture::new(
        ["src/main.ts", "src/provider.ts"],
        [source, "export const Token = 1;"],
        SemanticProjectLanguage::TypeScript,
    );
    let output = analyze_with_real_parser(&fixture);
    let gaps: Vec<_> = output
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "ecmascript-export-target-unavailable")
        .collect();
    assert_eq!(gaps.len(), 3);
    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
    for gap in gaps {
        assert_eq!(gap.domain, FactDomain::Relations);
        assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(gap.source.span().file(), fixture.snapshots[0].file());
    }
    let token = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::TypeAlias && entity.canonical_name == "Token")
        .unwrap()
        .id;
    for (needle, offset, role, resolved) in [
        ("export {Token}", 8, OccurrenceRole::Reference, true),
        ("type {Token", 6, OccurrenceRole::TypeUse, true),
        ("as Public", 3, OccurrenceRole::Reference, false),
        (
            "export {Token as Remote",
            8,
            OccurrenceRole::Reference,
            false,
        ),
        ("as Remote", 3, OccurrenceRole::Reference, false),
    ] {
        let start = u64::try_from(source.find(needle).unwrap() + offset).unwrap();
        let occurrence = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        assert_eq!(occurrence.role, role, "{needle}");
        if resolved {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: token },
                "{needle}"
            );
        } else {
            assert!(
                matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                "{needle}: {occurrence:?}"
            );
        }
    }
}

#[test]
fn typescript_references_select_the_written_type_or_value_namespace() {
    let source = "type Token = string; const Token = 'value'; let typed: Token = Token; type Query = typeof Token; const object = {Token};\n";
    let language = SemanticProjectLanguage::TypeScript;
    let fixture = ProjectFixture::new(
        ["src/main.ts", "src/other.ts"],
        [source, "export const unrelated = 0;"],
        language,
    );
    let output = analyze_with_real_parser(&fixture);
    let target = |kind| {
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == "Token")
            .unwrap()
            .id
    };
    for (needle, offset, role, kind) in [
        (
            "typed: Token",
            "typed: ".len(),
            OccurrenceRole::TypeUse,
            EntityKind::TypeAlias,
        ),
        (
            "= Token;",
            "= ".len(),
            OccurrenceRole::Reference,
            EntityKind::Variable,
        ),
        (
            "typeof Token",
            "typeof ".len(),
            OccurrenceRole::Reference,
            EntityKind::Variable,
        ),
        (
            "{Token}",
            1,
            OccurrenceRole::Reference,
            EntityKind::Variable,
        ),
    ] {
        let start = u64::try_from(source.find(needle).unwrap() + offset).unwrap();
        let occurrence = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte() == start + 5
            })
            .unwrap();
        assert_eq!(occurrence.role, role, "{needle}");
        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Resolved {
                symbol: target(kind)
            },
            "{needle}"
        );
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
}

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
fn repeated_declarations_retain_each_resolved_definition_site() {
    let source = "export function shared(value) { return value; }\n\
                  export function shared(value) { return value; }\n";
    let fixture = ProjectFixture::new(
        ["src/shared.js"],
        [source],
        SemanticProjectLanguage::JavaScript,
    );
    let output = analyze_with_real_parser(&fixture);
    let entities = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.display_name == "shared")
        .collect::<Vec<_>>();
    assert_eq!(
        entities.len(),
        1,
        "repeated declarations share one public entity"
    );
    let symbol = entities[0].id;
    let definition_sites = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol }
        })
        .map(|occurrence| occurrence.source.span())
        .collect::<BTreeSet<_>>();
    let expected_sites = source
        .match_indices("shared")
        .map(|(start, name)| {
            SourceSpan::new(
                fixture.snapshots[0].file(),
                u64::try_from(start).expect("fixture offset fits"),
                u64::try_from(start + name.len()).expect("fixture offset fits"),
            )
            .expect("fixture span is valid")
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(
        definition_sites, expected_sites,
        "every repeated declaration retains its distinct resolved name site"
    );
}

#[test]
fn tsx_nested_import_calls_preserve_project_targets_and_source_evidence() {
    for target_path in ["src/provider.ts", "src/provider.tsx"] {
        let fixture = ProjectFixture::new(
            ["src/view.tsx", target_path],
            [
                "import {provide} from './provider';\nexport function View() { return <Panel title={provide()}><span>Hello</span></Panel>; }\n",
                "export function provide(): number { return 1; }\n",
            ],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        assert!(
            output
                .document()
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "syntax-error-recovery")
        );
        let caller = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "View")
            .expect("TSX component is materialized");
        let target =
            output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function
                        && entity.canonical_name == "provide"
                        && entity.evidence.source.as_ref().is_some_and(|source| {
                            source.span().file() == fixture.snapshots[1].file()
                        })
                })
                .expect("TypeScript import target is materialized");
        let target_module =
            output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Module
                        && entity.evidence.source.as_ref().is_some_and(|source| {
                            source.span().file() == fixture.snapshots[1].file()
                        })
                })
                .expect("imported module retains its source identity");
        assert!(
            output
                .document()
                .relations
                .iter()
                .any(|relation| relation.subject
                    == rootlight_ir::RelationEndpoint::File(fixture.snapshots[0].file())
                    && relation.predicate == RelationPredicate::Imports
                    && relation.object == rootlight_ir::RelationEndpoint::Entity(target_module.id))
        );
        let call = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::CallSite
                    && occurrence.enclosing == Some(caller.id)
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
            })
            .expect("call nested in JSX resolves across files");
        let start = usize::try_from(call.source.span().start_byte()).expect("fixture offset fits");
        let end = usize::try_from(call.source.span().end_byte()).expect("fixture offset fits");
        let consumer =
            std::str::from_utf8(fixture.snapshots[0].content()).expect("fixture is UTF-8");
        assert_eq!(start, consumer.find("provide()").expect("JSX call exists"));
        assert_eq!(
            fixture.snapshots[0].content().get(start..end),
            Some(b"provide".as_slice())
        );
        assert!(
            output
                .document()
                .relations
                .iter()
                .any(|relation| relation.subject
                    == rootlight_ir::RelationEndpoint::Occurrence(call.id)
                    && relation.predicate == RelationPredicate::Calls
                    && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id))
        );
    }
}

#[test]
fn javascript_closed_prefix_materializes_nested_import_call() {
    let retained_prefix = concat!(
        "import {provide} from './provider';\n",
        "export function outer() {\n",
        "  function caller() {\n",
        "    const result = provide();\n",
        "    return result;\n",
        "  }\n",
    );
    let projected_consumer = format!("{retained_prefix}}}\n");
    let fixture = ProjectFixture::new(
        ["src/consumer.js", "src/provider.js"],
        [
            projected_consumer.as_str(),
            "export function provide() { return 1; }\n",
        ],
        SemanticProjectLanguage::JavaScript,
    );
    let output = analyze_with_real_parser(&fixture);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "caller")
        .expect("nested caller is materialized");
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "provide")
        .expect("import target is materialized");
    assert!(
        output.document().entities.iter().any(|entity| {
            entity.kind == EntityKind::Variable && entity.display_name == "result"
        })
    );
    let call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(caller.id)
                && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
        })
        .expect("nested imported call is resolved");

    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
            && relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
    }));
}

#[test]
fn javascript_typed_nested_call_belongs_to_function_not_local_binding() {
    let fixture = ProjectFixture::new(
        ["src/consumer.js", "src/provider.js"],
        [
            concat!(
                "import {provide} from './provider';\n",
                "export function outer(): number {\n",
                "  function caller(input: number): number {\n",
                "    const created = provide(input);\n",
                "    return created;\n",
                "  }\n",
                "  return caller(1);\n",
                "}\n",
            ),
            "export function provide(input: number): number { return input; }\n",
        ],
        SemanticProjectLanguage::JavaScript,
    );
    let output = analyze_with_real_parser(&fixture);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "caller")
        .expect("typed nested caller is materialized");
    let local = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.display_name == "created")
        .expect("local binding is materialized");
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "provide")
        .expect("typed import target is materialized");
    let call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
        })
        .expect("typed nested import call is resolved");

    assert_eq!(call.enclosing, Some(caller.id));
    assert_ne!(call.enclosing, Some(local.id));
}

#[test]
fn nested_same_name_declarations_bind_their_own_definition_sites() {
    let fixture = ProjectFixture::new(
        ["src/shared.rs"],
        ["mod shared {\n    pub fn shared() {}\n}\n"],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let declarations = output
        .document()
        .entities
        .iter()
        .filter(|entity| {
            entity.display_name == "shared"
                && matches!(entity.kind, EntityKind::Module | EntityKind::Function)
        })
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        declarations.len(),
        2,
        "the module and nested function are distinct declarations"
    );

    let definition_targets = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.syntactic_text_hash == content_hash(b"shared")
        })
        .map(|occurrence| {
            let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                panic!("a definition must bind directly to its own declaration");
            };
            symbol
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        definition_targets, declarations,
        "an enclosing same-name module cannot contaminate the function definition"
    );
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
fn rust_local_function_inside_method_matches_structural_identity() {
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/other.rs"],
        [
            "pub struct Demo;\nimpl Demo {\n    pub fn answer(&self) -> u32 {\n        fn local() -> u32 { 42 }\n        local()\n    }\n}\n",
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
            (EntityKind::Function, "local"),
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
fn rust_type_qualified_calls_select_only_the_qualified_impl() {
    let fixture = ProjectFixture::new(
        ["src/cli.rs", "src/lib.rs"],
        [
            concat!(
                "pub struct CliOptions;\n",
                "impl CliOptions {\n",
                "    pub fn from_flags() -> Self { Self }\n",
                "}\n",
                "pub struct CliFactory;\n",
                "impl CliFactory {\n",
                "    pub fn from_flags() -> Self { Self }\n",
                "}\n",
                "pub fn check() {\n",
                "    let _ = CliFactory::from_flags();\n",
                "}\n",
            ),
            "",
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let mut methods = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Method && entity.display_name == "from_flags")
        .collect::<Vec<_>>();
    methods.sort_by_key(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .expect("method has source evidence")
            .span()
            .start_byte()
    });
    let [options_method, factory_method] = methods.as_slice() else {
        panic!("both same-name impl methods are materialized");
    };
    let call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.syntactic_text_hash == content_hash(b"from_flags")
        })
        .expect("type-qualified call is materialized");
    assert_eq!(
        call.target,
        OccurrenceTarget::Resolved {
            symbol: factory_method.id
        }
    );
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
            && relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(factory_method.id)
    }));
    assert!(output.document().relations.iter().all(|relation| {
        relation.subject != rootlight_ir::RelationEndpoint::Occurrence(call.id)
            || relation.object != rootlight_ir::RelationEndpoint::Entity(options_method.id)
    }));
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
fn bounded_python_syntax_retains_late_local_call_relationship() {
    let mut paths = vec!["Lib/search.py".to_owned()];
    let mut sources = vec![String::from(concat!(
        "from dependency import helper\n\n",
        "def choose(values):\n",
        "    return 0\n\n",
        "def insert(values):\n",
        "    return choose(values)\n",
    ))];
    for index in 0..127 {
        paths.push(format!("Lib/filler_{index:03}.py"));
        sources.push(format!(
            "from dependency import helper\n{}",
            "helper()\n".repeat(10)
        ));
    }
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::Python);
    let limits = real_parser_limits_with_project_files(128);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "insert")
        .expect("caller function is materialized");
    let callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "choose")
        .expect("callee function is materialized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(callee.id)
    }));
}

#[test]
fn bounded_python_syntax_reserves_late_file_local_call_relationship() {
    let mut paths = Vec::new();
    let mut sources = Vec::new();
    for index in 0..31 {
        paths.push(format!("Lib/noisy_{index:03}.py"));
        sources.push(format!(
            concat!(
                "from dependency import imported_a, imported_b, imported_c\n\n",
                "def local_{index}():\n",
                "    return 0\n\n",
                "def consume_{index}():\n",
                "    local_{index}()\n",
                "    local_{index}()\n",
                "    imported_a()\n",
                "    imported_b()\n",
                "    imported_c()\n",
            ),
            index = index,
        ));
    }
    paths.push("Lib/zzzzz_999.py".to_owned());
    sources.push(String::from(concat!(
        "def choose(values):\n",
        "    return 0\n\n",
        "def frequent(values):\n",
        "    return 0\n\n",
        "def insert(values):\n",
        "    values.insert(choose(values))\n",
        "    frequent(values)\n",
        "    frequent(values)\n",
    )));
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::Python);
    let limits = real_parser_limits_with_project_files(32);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "insert")
        .expect("late caller function is materialized");
    let callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "choose")
        .expect("late callee function is materialized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(callee.id)
    }));
}

#[test]
fn bounded_javascript_syntax_retains_local_and_import_diverse_calls() {
    let mut paths = vec![
        "src/000_consumer.js".to_owned(),
        "src/001_provider.js".to_owned(),
        "src/002_provider.js".to_owned(),
        "src/003_provider.js".to_owned(),
    ];
    let mut sources = vec![
        String::from(concat!(
            "import {earlyBusy} from './002_provider';\n",
            "import {middleBusy} from './003_provider';\n",
            "import {frequent, occasional} from './001_provider';\n",
            "export function localValue() { return 1; }\n",
            "export function firstCaller() {\n",
            "  const local = localValue();\n",
            "  earlyBusy(1); earlyBusy(2); earlyBusy(3); earlyBusy(4);\n",
            "  earlyBusy(5); earlyBusy(6); earlyBusy(7);\n",
            "  middleBusy(1); middleBusy(2); middleBusy(3);\n",
            "  middleBusy(4); middleBusy(5);\n",
            "  occasional(local);\n",
            "  return frequent(local);\n",
            "}\n",
            "export function secondCaller() { return frequent(2); }\n",
            "export function thirdCaller() { return frequent(3); }\n",
        )),
        String::from(concat!(
            "export function frequent(value) { return value; }\n",
            "export function occasional(value) { return value; }\n",
        )),
        String::from("export function earlyBusy(value) { return value; }\n"),
        String::from("export function middleBusy(value) { return value; }\n"),
    ];
    for index in 0..124 {
        paths.push(format!("src/zz_filler_{index:03}.js"));
        sources.push(format!(
            "export function local{index}() {{ return {index}; }}\n\
             export function run{index}() {{ return local{index}(); }}\n"
        ));
    }
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::JavaScript);
    let limits = real_parser_limits_with_project_files(128);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let local_callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "localValue")
        .expect("local callee is materialized");
    let local_caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "firstCaller")
        .expect("local caller is materialized");
    let imported_callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "frequent")
        .expect("imported callee is materialized");
    let later_imported_caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "secondCaller")
        .expect("later imported caller is materialized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(local_caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == local_callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(local_callee.id)
    }));
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(later_imported_caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == imported_callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(imported_callee.id)
    }));
}

#[test]
fn bounded_javascript_syntax_retains_each_called_grouped_import_binding() {
    let mut paths = Vec::new();
    let mut sources = Vec::new();
    for index in 0..254 {
        paths.push(format!("src/{index:03}_local.js"));
        sources.push(format!(
            "export function local{index}() {{ return {index}; }}\n\
             export function run{index}() {{ return local{index}(); }}\n"
        ));
    }
    paths.push("src/zzz_consumer.js".to_owned());
    sources.push(
        concat!(
            "import {frequent, occasional} from './zzz_provider';\n",
            "export function frequentCaller() {\n",
            "  frequent(1); frequent(2); frequent(3); frequent(4);\n",
            "  frequent(5); frequent(6); frequent(7);\n",
            "}\n",
            "export function secondaryFrequentCaller() { return frequent(8); }\n",
            "export function occasionalCaller() { return occasional(9); }\n",
        )
        .to_owned(),
    );
    paths.push("src/zzz_provider.js".to_owned());
    sources.push(
        concat!(
            "export function frequent(value) { return value; }\n",
            "export function occasional(value) { return value; }\n",
        )
        .to_owned(),
    );
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::JavaScript);
    let limits = real_parser_limits_with_project_files(256);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function && entity.display_name == "occasionalCaller"
        })
        .expect("less frequent imported caller is materialized");
    let callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "occasional")
        .expect("less frequent imported callee is materialized");
    let secondary_caller = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function && entity.display_name == "secondaryFrequentCaller"
        })
        .expect("second caller of a repeated imported binding is materialized");
    let frequent_callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "frequent")
        .expect("repeated imported callee is materialized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(callee.id)
    }));
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(secondary_caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == frequent_callee.id
            )
    }));
}

#[test]
fn bounded_javascript_syntax_prioritizes_imported_calls_over_local_fanout() {
    let mut paths = Vec::new();
    let mut sources = Vec::new();
    for index in 0..254 {
        paths.push(format!("src/{index:03}_local.js"));
        sources.push(format!(
            "export function local{index}() {{ return {index}; }}\n\
             export function run{index}() {{\n\
               local{index}(); local{index}(); local{index}(); local{index}();\n\
               local{index}(); local{index}(); local{index}(); local{index}();\n\
             }}\n"
        ));
    }
    paths.push("src/zzz_consumer.js".to_owned());
    sources.push(
        "import {remoteValue} from './zzz_provider';\n\
         export function consume() { return remoteValue(2); }\n"
            .to_owned(),
    );
    paths.push("src/zzz_provider.js".to_owned());
    sources.push("export function remoteValue(value) { return value; }\n".to_owned());
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::JavaScript);
    let limits = real_parser_limits_with_project_files(256);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "consume")
        .expect("consumer is materialized");
    let callee = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.display_name == "remoteValue")
        .expect("imported provider is materialized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == callee.id
            )
    }));
    assert!(output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(callee.id)
    }));
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
fn bounded_go_calls_prefer_cross_symbol_edges_over_recursion() {
    let recursive_calls = "    visit()\n".repeat(400);
    let source = format!(
        "package sample\nfunc visit() {{\n{recursive_calls}}}\nfunc infer() {{ visit() }}\n"
    );
    let fixture = ProjectFixture::new(
        ["thinking/visit.go"],
        [source.as_str()],
        SemanticProjectLanguage::Go,
    );
    let output = analyze_with_real_parser(&fixture);
    let visit = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "visit")
        .expect("visited function is materialized");
    let infer = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "infer")
        .expect("non-recursive caller is materialized");

    assert!(output.document().diagnostics.iter().any(|diagnostic| {
        diagnostic.code == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC
            && diagnostic.coverage_effect == CoverageStatus::Bounded
    }));
    let cross_symbol_call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(infer.id)
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol } if symbol == visit.id
                )
        })
        .expect("bounded syntax retains a cross-symbol call site");
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Occurrence(cross_symbol_call.id)
            && relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(visit.id)
    }));
}

#[test]
fn bounded_go_domain_routes_do_not_hide_production_local_relationships() {
    let routes = (0..120)
        .map(|index| format!("    router.GET(\"/route-{index}\", handle)\n"))
        .collect::<String>();
    let route_source = format!(
        "package sample\nimport \"github.com/gin-gonic/gin\"\nfunc handle() {{}}\nfunc Routes() {{\n    router := gin.Default()\n{routes}}}\n"
    );
    let recursive_calls = "    walk()\n".repeat(8);
    let relationship_source =
        format!("package sample\nfunc walk() {{\n{recursive_calls}}}\nfunc infer() {{ walk() }}\n");
    let mut paths = vec!["sample/a_routes.go".to_owned()];
    let mut sources = vec![route_source];
    for index in 0..30 {
        paths.push(format!("sample/b_worker_{index:02}.go"));
        let repeated = format!("    helper{index}()\n").repeat(24);
        sources.push(format!(
            "package sample\nfunc helper{index}() {{}}\nfunc worker{index}() {{\n{repeated}}}\n"
        ));
    }
    paths.push("sample/z_relationships.go".to_owned());
    sources.push(relationship_source);
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::Go);
    let limits = real_parser_limits_with_project_files(128);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let walk = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "walk")
        .expect("callee is materialized");
    let infer = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "infer")
        .expect("caller is materialized");

    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(infer.id)
            && matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol } if symbol == walk.id
            )
    }));
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

#[test]
fn java_test_calls_require_annotation_and_exact_receiver_type() {
    let fixture = ProjectFixture::new(
        ["src/FibonacciSearch.java", "src/FibonacciSearchTest.java"],
        [
            concat!(
                "class FibonacciSearch {\n",
                "  int search(int[] values, int target) { return target; }\n",
                "}\n",
                "class OtherSearch {\n",
                "  int search(int[] values, int target) { return -1; }\n",
                "}\n",
            ),
            concat!(
                "import org.junit.jupiter.api.Test;\n",
                "class FibonacciSearchTest {\n",
                "  @Test void findsValue() {\n",
                "    FibonacciSearch search = new FibonacciSearch();\n",
                "    search.search(new int[]{1, 2}, 2);\n",
                "  }\n",
                "  void helper() {\n",
                "    FibonacciSearch search = new FibonacciSearch();\n",
                "    search.search(new int[]{1, 2}, 2);\n",
                "  }\n",
                "}\n",
            ),
        ],
        SemanticProjectLanguage::Java,
    );
    let output = analyze_with_real_parser(&fixture);
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "search"
                && entity.qualified_name.contains("FibonacciSearch::search")
        })
        .expect("Java target is materialized");
    let spurious = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "search" && entity.qualified_name.contains("OtherSearch::search")
        })
        .expect("same-terminal Java decoy is materialized");
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "findsValue")
        .expect("annotated Java test is materialized");
    let helper = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "helper")
        .expect("unannotated Java helper is materialized");

    assert!(test.flags.contains(&EntityFlag::Test));
    assert!(!helper.flags.contains(&EntityFlag::Test));
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(test.id)
            && relation.predicate == RelationPredicate::Tests
            && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
            && relation.evidence.source.is_some()
    }));
    assert!(!output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(helper.id)
            && relation.predicate == RelationPredicate::Tests
    }));
    assert!(!output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(spurious.id)
    }));
}

#[test]
fn bounded_java_project_prioritizes_annotated_test_relationships() {
    let mut paths = Vec::new();
    let mut sources = Vec::new();
    for index in 0..126 {
        paths.push(format!("src/early/Filler{index:03}.java"));
        sources.push(format!(
            "class Filler{index:03} {{\n\
               int value{index:03}() {{ return {index}; }}\n\
               int exercise{index:03}() {{\n\
                 if (true) {{ if (true) {{ return value{index:03}(); }} }}\n\
                 return 0;\n\
               }}\n\
             }}\n"
        ));
    }
    paths.extend([
        "src/zzzzz/Worker.java".to_owned(),
        "src/zzzzz/WorkerTest.java".to_owned(),
    ]);
    sources.extend([
        concat!(
            "interface WorkerApi {\n",
            "  <T extends Comparable<T>> int execute(T[] values, T target);\n",
            "}\n",
            "@SuppressWarnings({\"rawtypes\", \"unchecked\"})\n",
            "class Worker implements WorkerApi {\n",
            "  @Override\n",
            "  public <T extends Comparable<T>> int execute(T[] values, T target) {\n",
            "    return values.length;\n",
            "  }\n",
            "}\n",
        )
        .to_owned(),
        concat!(
            "import org.junit.jupiter.api.Test;\n",
            "class WorkerTest {\n",
            "  void assertResult(int actual, String message) {}\n",
            "  @Test void exercisesReceiver() {\n",
            "    Worker worker = new Worker();\n",
            "    Worker alternate = new Worker();\n",
            "    Worker fallback = new Worker();\n",
            "    Integer[] values = {1, 2, 3};\n",
            "    assertResult(worker.execute(values, values[0]), \"first\");\n",
            "    assertResult(worker.execute(values, values[1]), \"second\");\n",
            "    assertResult(worker.execute(values, values[2]), \"third\");\n",
            "  }\n",
            "}\n",
        )
        .to_owned(),
    ]);
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::Java);
    let limits = real_parser_limits_with_project_files(128);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "execute" && entity.qualified_name.contains("Worker::execute")
        })
        .expect("Java target is materialized");
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "exercisesReceiver")
        .expect("annotated Java test is materialized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC)
    );
    assert!(test.flags.contains(&EntityFlag::Test));
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(test.id)
            && relation.predicate == RelationPredicate::Tests
            && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
            && relation.evidence.source.is_some()
    }));
}

#[test]
fn cpp_gtest_calls_require_positive_macro_and_exact_local_type() {
    let fixture = ProjectFixture::new(
        ["src/raw_props.cpp", "src/raw_props_test.cpp"],
        [
            concat!(
                "class RawProps {\n",
                "public:\n",
                "  bool has(int key) { return key > 0; }\n",
                "};\n",
                "class OtherProps {\n",
                "public:\n",
                "  bool has(int key) { return false; }\n",
                "};\n",
            ),
            concat!(
                "TEST(RawProps, Has) {\n",
                "  RawProps props;\n",
                "  props.has(1);\n",
                "}\n",
                "TEST_F(RawPropsFixture, Has) {\n",
                "  RawProps props;\n",
                "  props.has(2);\n",
                "}\n",
            ),
        ],
        SemanticProjectLanguage::Cpp,
    );
    let output = analyze_with_real_parser(&fixture);
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "has" && entity.qualified_name.contains("RawProps::has")
        })
        .expect("C++ target is materialized");
    let spurious = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "has" && entity.qualified_name.contains("OtherProps::has")
        })
        .expect("same-terminal C++ decoy is materialized");
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.flags.contains(&EntityFlag::Test))
        .expect("reviewed TEST macro is classified");
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.flags.contains(&EntityFlag::Test))
            .count(),
        2,
        "both TEST and TEST_F are positively classified"
    );

    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(test.id)
            && relation.predicate == RelationPredicate::Tests
            && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
    }));
    assert!(!output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(spurious.id)
    }));
}

#[test]
fn cpp_gtest_resolves_auto_constructed_templated_receiver() {
    let fixture = ProjectFixture::new(
        ["src/parser.h", "src/parser_test.cpp"],
        [
            concat!(
                "namespace sample::detail {\n",
                "class Value {};\n",
                "class Parser final {\n",
                "public:\n",
                "  Parser() = default;\n",
                "  [[deprecated]] explicit Parser(bool) : Parser() {}\n",
                "  template <typename Item>\n",
                "  void prepare() noexcept { postPrepare(); }\n",
                "private:\n",
                "  friend class Helper;\n",
                "  template <class Derived>\n",
                "  friend class Descriptor;\n",
                "  void preparse(const Value &value) const noexcept;\n",
                "  void postPrepare() noexcept;\n",
                "  const Value *lookup(const Value &value) const noexcept;\n",
                "  const Value &peek() const noexcept;\n",
                "  void (*callback)();\n",
                "  bool ready_{false};\n",
                "};\n",
                "}\n",
            ),
            concat!(
                "namespace sample::detail {\n",
                "TEST(ParserTest, PreparesValue) {\n",
                "  auto parser = Parser();\n",
                "  parser.prepare<Value>();\n",
                "}\n",
                "}\n",
            ),
        ],
        SemanticProjectLanguage::Cpp,
    );
    let output = analyze_with_real_parser(&fixture);
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "prepare" && entity.qualified_name.contains("Parser::prepare")
        })
        .expect("templated C++ method is materialized under its declaring type");
    let declared_method = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "postPrepare"
                && entity.qualified_name.contains("Parser::postPrepare")
        })
        .expect("declared C++ method is materialized under its declaring type");
    let pointer_return_method = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "lookup" && entity.qualified_name.contains("Parser::lookup")
        })
        .expect("pointer-return C++ method is materialized under its declaring type");
    let reference_return_method = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "peek" && entity.qualified_name.contains("Parser::peek")
        })
        .expect("reference-return C++ method is materialized under its declaring type");
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.flags.contains(&EntityFlag::Test))
        .expect("reviewed TEST macro is classified");

    assert_eq!(target.kind, EntityKind::Method);
    assert_eq!(declared_method.kind, EntityKind::Method);
    assert_eq!(pointer_return_method.kind, EntityKind::Method);
    assert_eq!(reference_return_method.kind, EntityKind::Method);
    assert!(
        !output.document().entities.iter().any(|entity| {
            entity.kind == EntityKind::Method && entity.display_name == "callback"
        })
    );
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(test.id)
            && relation.predicate == RelationPredicate::Tests
            && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
    }));
}

#[test]
fn bounded_cpp_syntax_retains_late_test_call_relationship() {
    let mut consumer = String::from(
        "namespace sample {\n\
         TEST(ParserTest, PreparesValue) {\n\
           int accumulator = 0;\n",
    );
    for _ in 0..1_200 {
        consumer.push_str("  accumulator += unresolved_value;\n");
    }
    consumer.push_str(
        "  auto parser = Parser();\n\
           parser.prepare<Value>();\n\
         }\n\
         }\n",
    );
    let fixture = ProjectFixture::new(
        ["src/parser.h", "src/parser_test.cpp"],
        [
            concat!(
                "namespace sample {\n",
                "class Value {};\n",
                "class Parser {\n",
                "public:\n",
                "  template <typename Item>\n",
                "  void prepare() noexcept {}\n",
                "};\n",
                "}\n",
            ),
            consumer.as_str(),
        ],
        SemanticProjectLanguage::Cpp,
    );
    let output = analyze_with_real_parser(&fixture);
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "prepare" && entity.qualified_name.contains("Parser::prepare")
        })
        .expect("late-call target is materialized");
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.flags.contains(&EntityFlag::Test))
        .expect("reviewed TEST macro is classified");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(test.id)
            && relation.predicate == RelationPredicate::Tests
            && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
    }));
}

#[test]
fn go_gin_literal_route_requires_reviewed_import_receiver_path_and_handler() {
    let fixture = ProjectFixture::new(
        ["routes/other.go", "routes/routes.go"],
        [
            "package routes\n",
            concat!(
                "package routes\n",
                "import \"github.com/gin-gonic/gin\"\n",
                "func Generate() {}\n",
                "func Register() {\n",
                "  router := gin.Default()\n",
                "  router.POST(\"/api/generate\", middleware.Wrap(Generate))\n",
                "}\n",
            ),
        ],
        SemanticProjectLanguage::Go,
    );
    let output = analyze_with_real_parser(&fixture);
    let handler = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "Generate")
        .expect("Gin handler is materialized");
    let route = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Route && entity.display_name == "POST /api/generate"
        })
        .expect("literal Gin route is synthesized");

    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(handler.id)
            && relation.predicate == RelationPredicate::ServesRoute
            && relation.object == rootlight_ir::RelationEndpoint::Entity(route.id)
            && relation.evidence.source.is_some()
    }));

    let dynamic = ProjectFixture::new(
        ["routes/other.go", "routes/routes.go"],
        [
            "package routes\n",
            concat!(
                "package routes\n",
                "import \"github.com/gin-gonic/gin\"\n",
                "func Generate() {}\n",
                "func Register(path string) {\n",
                "  router := gin.Default()\n",
                "  router.POST(path, Generate)\n",
                "  handler := Generate\n",
                "  router.POST(\"/api/generate\", handler)\n",
                "}\n",
            ),
        ],
        SemanticProjectLanguage::Go,
    );
    let dynamic_output = analyze_with_real_parser(&dynamic);
    assert!(
        !dynamic_output
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Route)
    );
    assert!(
        dynamic_output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "go-gin-route-incomplete")
    );
}

#[test]
fn bounded_go_syntax_retains_reviewed_literal_route_relationship() {
    let mut tests = String::from("package routes\nfunc TestNoise() {\n");
    for index in 0..400 {
        tests.push_str(&format!("  testNoise{index}()\n"));
    }
    tests.push_str("}\n");
    let mut routes = String::from(concat!(
        "package routes\n",
        "import (\n",
        "  \"net/http\"\n",
        "  \"github.com/gin-gonic/gin\"\n",
        ")\n",
        "func PublishReport() {}\n",
        "func Wrap(path string, handler func()) func() { return handler }\n",
        "func Register() {\n",
    ));
    for index in 0..400 {
        routes.push_str(&format!("  unresolved{index}()\n"));
    }
    routes.push_str(concat!(
        "  router := gin.Default()\n",
        "  router.POST(\"/reports/publish\", Wrap(\"/reports/publish\", PublishReport))\n",
        "  _ = http.StatusOK\n",
        "}\n",
    ));
    let fixture = ProjectFixture::new_owned(
        vec![
            "web/endpoints.go".to_owned(),
            "web/endpoints_test.go".to_owned(),
        ],
        vec![routes, tests],
        SemanticProjectLanguage::Go,
    );
    let limits = real_parser_limits_with_project_files(2);
    let output = analyze_with_real_parser_limits(&fixture, &limits);
    let handler = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "PublishReport")
        .expect("bounded Gin handler is materialized");
    let route = output
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Route && entity.display_name == "POST /reports/publish"
        })
        .expect("bounded literal Gin route is synthesized");

    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(handler.id)
            && relation.predicate == RelationPredicate::ServesRoute
            && relation.object == rootlight_ir::RelationEndpoint::Entity(route.id)
    }));
}

#[test]
fn c_function_prototypes_preserve_structural_and_project_identity() {
    let fixture = ProjectFixture::new(
        ["include/reader.h"],
        [
            "#define PUBLIC_API\nPUBLIC_API const char* describe_status(int status);\nint read_value(void);\nint (*callback_slot)(int);\nint data_value;\n",
        ],
        SemanticProjectLanguage::C,
    );
    assert_real_parser_symbol_identity(
        &fixture,
        "c",
        &[
            (EntityKind::Function, "describe_status"),
            (EntityKind::Function, "read_value"),
        ],
    );
    let output = analyze_with_real_parser(&fixture);
    assert!(!output.document().entities.iter().any(|entity| {
        entity.kind == EntityKind::Function
            && matches!(entity.display_name.as_str(), "callback_slot" | "data_value")
    }));
}

#[test]
fn bounded_c_repeated_local_calls_preserve_distinct_external_relationships() {
    let mut files = vec![
        (
            "reader.h".to_owned(),
            "int read_value(int key);\n".to_owned(),
        ),
        (
            "worker.c".to_owned(),
            format!(
                "#include \"reader.h\"\nstatic int local_value(void) {{ return 1; }}\nint invoke(void) {{\n{}    return read_value(7);\n}}\n",
                "    local_value();\n".repeat(24)
            ),
        ),
    ];
    for index in 0..16 {
        files.push((
            format!("load_{index:02}.c"),
            format!(
                "static int helper_{index}(void) {{ return 1; }}\nint work_{index}(void) {{\n{}    return 0;\n}}\n",
                format!("    helper_{index}();\n").repeat(24)
            ),
        ));
    }
    files.sort_by_cached_key(|(path, _)| {
        RelativePath::parse(Path::new(path))
            .expect("fixture path is valid")
            .identity_bytes()
            .to_vec()
    });
    let (paths, sources) = files.into_iter().unzip();
    let fixture = ProjectFixture::new_owned(paths, sources, SemanticProjectLanguage::C);
    let output =
        analyze_with_real_parser_limits(&fixture, &real_parser_limits_with_project_files(32));
    let document = output.document();
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    let caller = document
        .entities
        .iter()
        .find(|entity| entity.display_name == "invoke")
        .expect("caller identity is retained");
    let callee = document
        .entities
        .iter()
        .find(|entity| entity.display_name == "read_value")
        .expect("external declaration identity is retained");
    let call = document.occurrences.iter().find(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(caller.id)
            && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == callee.id)
    }).expect("repeated local sites must not hide the distinct external relationship");
    assert!(document.relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
            && relation.predicate == RelationPredicate::Calls
            && relation.object == rootlight_ir::RelationEndpoint::Entity(callee.id)
    }));
}

#[test]
fn bounded_rust_call_selection_preserves_owners_and_unknown_targets() {
    let source = format!(
        "fn local() {{}}\nfn read_data(key: i32) -> i32 {{ key }}\nfn run() {{\n{}    read_data(7);\n}}\nfn again() {{ read_data(8); }}\nfn dispatch() {{ unknown(9); }}\n",
        "    local();\n".repeat(200)
    );
    let fixture = ProjectFixture::new(["src/lib.rs"], [&source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC })
    );
    let callee = document
        .entities
        .iter()
        .find(|entity| entity.display_name == "read_data")
        .expect("callee identity is retained");
    for name in ["run", "again"] {
        let caller = document
            .entities
            .iter()
            .find(|entity| entity.display_name == name)
            .expect("caller identity is retained");
        assert!(document.occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(caller.id)
                && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == callee.id)
        }), "distinct callable owner {name} must retain its call");
    }
    let dispatch = document
        .entities
        .iter()
        .find(|entity| entity.display_name == "dispatch")
        .expect("unknown-call owner is retained");
    let unknown = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite && occurrence.enclosing == Some(dispatch.id)
        })
        .expect("unknown call syntax is retained");
    assert!(matches!(
        unknown.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!document.relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Occurrence(unknown.id)
            && relation.predicate == RelationPredicate::Calls
    }));
}

#[test]
fn reviewed_csharp_php_and_c_calls_resolve_only_through_static_rules() {
    for (language, paths, sources, caller_name) in [
        (
            SemanticProjectLanguage::CSharp,
            ["src/Other.cs", "src/Worker.cs"],
            [
                "class Other {}\n",
                "class Worker { int Target(int value) { return value; } int Run() { return Target(1); } }\n",
            ],
            "Run",
        ),
        (
            SemanticProjectLanguage::Php,
            ["src/Other.php", "src/Worker.php"],
            [
                "<?php class Other {}\n",
                "<?php class Worker { function target($value) { return $value; } function run() { return $this->target(1); } }\n",
            ],
            "run",
        ),
        (
            SemanticProjectLanguage::C,
            ["src/other.c", "src/worker.c"],
            [
                "static int target(int value) { return value + 1; }\n",
                "int target(int value) { return value; }\nint run(void) { return target(1); }\n",
            ],
            "run",
        ),
    ] {
        let fixture = ProjectFixture::new(paths, sources, language);
        let output = analyze_with_real_parser(&fixture);
        let caller = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.display_name == caller_name)
            .expect("reviewed caller is materialized");
        assert!(
            output.document().occurrences.iter().any(|occurrence| {
                occurrence.role == OccurrenceRole::CallSite
                    && occurrence.enclosing == Some(caller.id)
                    && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            }),
            "{} call did not resolve through its reviewed rule",
            language.as_str()
        );
    }
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
    analyze_with_real_parser_limits(fixture, &real_parser_limits())
}

fn analyze_with_real_parser_limits(
    fixture: &ProjectFixture,
    limits: &AnalysisLimits,
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
        &fixture.request(limits, AnalysisTier::TierB),
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
fn singleton_retains_large_declaration_identity_set_and_bounds_candidate_fanout() {
    let mut source = String::new();
    for index in 0..300 {
        source.push_str(&format!(
            "function ping(argument_{index}: number): number {{ return {index}; }}\n"
        ));
    }
    source.push_str("ping()\n");
    source.push_str(&"run()\n".repeat(300));
    let fixture = ProjectFixture::new(
        ["src/large.ts"],
        [source.as_str()],
        SemanticProjectLanguage::TypeScript,
    );
    let mut ir_limits = IrLimits::default();
    ir_limits.max_nested_items_per_record = 8;
    let limits = limits_with_ir_limits(ir_limits);
    let request = fixture.request(&limits, AnalysisTier::TierB);
    let analyzer = analyzer(SemanticProjectLanguage::TypeScript, fixture.build_context);

    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("large singleton declaration identity set commits bounded output");

    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(output.document().diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "project-syntax-fact-limit"
            && diagnostic.coverage_effect == CoverageStatus::Bounded
    }));
    let ping_symbols = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.display_name == "ping")
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ping_symbols.len(),
        300,
        "the optional syntax budget cannot discard declaration identities"
    );
    let signatures = output
        .document()
        .extensions
        .iter()
        .filter_map(|envelope| decode_symbol_identity_claim_envelope(envelope).ok())
        .filter(|claim| ping_symbols.contains(&claim.symbol))
        .map(|claim| claim.signature_discriminator)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        signatures.len(),
        300,
        "each retained declaration keeps its unique signature discriminator"
    );
    let call_sites = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect::<Vec<_>>();
    assert!(
        call_sites.len() <= 256,
        "optional occurrences remain subject to the fixed syntax budget"
    );
    let candidate_occurrence = call_sites
        .iter()
        .copied()
        .find(|occurrence| {
            matches!(
                &occurrence.target,
                OccurrenceTarget::Candidates {
                    total_count: 300,
                    completeness: CoverageStatus::Bounded,
                    ..
                }
            )
        })
        .expect("the retained ping call reports bounded candidates");
    let OccurrenceTarget::Candidates {
        symbols,
        total_count,
        completeness,
    } = &candidate_occurrence.target
    else {
        unreachable!("candidate occurrence was selected above");
    };
    assert_eq!(symbols.len(), 8);
    assert_eq!(*total_count, 300);
    assert_eq!(*completeness, CoverageStatus::Bounded);
    assert!(symbols.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|relation| {
                relation.subject
                    == rootlight_ir::RelationEndpoint::Occurrence(candidate_occurrence.id)
                    && relation.predicate == RelationPredicate::DispatchCandidate
            })
            .count(),
        symbols.len(),
        "only the bounded canonical candidate sample produces relations"
    );
}

#[test]
fn bounded_project_analysis_preserves_structural_declaration_kinds() {
    let repeated_calls = "target();\n".repeat(400);
    let java = format!(
        "package example.deep;\nclass Worker {{\n  int field = 1;\n  Worker() {{}}\n  int target() {{ return field; }}\n  int run() {{ int local = 0;\n{repeated_calls}  return local;\n  }}\n}}\n"
    );
    let typescript = format!(
        "class Worker {{\n  field = 1;\n  target() {{ return this.field; }}\n  run() {{ let local = 0;\n{repeated_calls}    return local;\n  }}\n}}\nconst helper = () => 1;\n"
    );
    let mut javascript = format!(
        "class Worker {{\n  field = 1;\n  target() {{ return this.field; }}\n  run() {{ let local = 0;\n{repeated_calls}    return local;\n  }}\n}}\nconst helper = () => 1;\nfunction scopedData() {{\n  const [data, setData] = getData();\n  useEffect(() => {{\n    if (data) {{ const data = load(); setData(data); }}\n  }});\n}}\nfunction firstHooks() {{ api({{ onError() {{}}, onReady() {{}} }}); }}\nfunction secondHooks() {{ api({{ onError() {{}}, onReady() {{}} }}); }}\n"
    );
    javascript.push_str(
        r#"
function renderStream() {
  const {pipe} = renderToPipeableStream(Root, {
    onShellReady() {
      const injector = new Transform({
        transform(chunk, encoding, callback) {
          callback(chunk, encoding);
        },
      });
      pipe(injector);
    },
    onError(error) {
      report(error);
    },
  });
}
function iterableValues() {
  const iterable = {
    async *[Symbol.asyncIterator]() {
      for (let i = 0; i < 30; i++) {
        yield i;
      }
    },
  };
  return iterable;
}
"#,
    );

    let fixtures = [
        ProjectFixture::new(
            ["src/Helper.java", "src/Worker.java"],
            [
                "package example.deep;\nclass Helper { int value = 1; }\n",
                java.as_str(),
            ],
            SemanticProjectLanguage::Java,
        ),
        ProjectFixture::new(
            ["src/helper.ts", "src/worker.ts"],
            ["export const support = 1;\n", typescript.as_str()],
            SemanticProjectLanguage::TypeScript,
        ),
        ProjectFixture::new(
            ["src/helper.js", "src/worker.js"],
            ["export const support = 1;\n", javascript.as_str()],
            SemanticProjectLanguage::JavaScript,
        ),
    ];
    for fixture in &fixtures {
        let declarations = assert_bounded_project_preserves_structural_declarations(fixture);
        if fixture.language == SemanticProjectLanguage::JavaScript {
            for expected in [
                (EntityKind::Method, "onShellReady"),
                (EntityKind::Method, "onError"),
                (EntityKind::Variable, "i"),
            ] {
                assert!(
                    declarations.contains(&(expected.0, expected.1.to_owned())),
                    "the JavaScript regression must exercise {expected:?}"
                );
            }
        }
    }
}

fn assert_bounded_project_preserves_structural_declarations(
    fixture: &ProjectFixture,
) -> BTreeSet<(EntityKind, String)> {
    let project_output = analyze_with_real_parser(fixture);
    assert!(
        project_output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "project-syntax-fact-limit"),
        "{} fixture must exercise bounded project syntax",
        fixture.language.as_str()
    );

    let limits = real_parser_limits();
    let provider = Arc::new(real_parser());
    let analyzer = TreeSitterAnalyzer::new(
        provider,
        producer_identity(),
        LanguageId::new(fixture.language.as_str()).expect("language is valid"),
        fixture.language.as_str(),
        content_hash(b"real-parser-binary"),
    )
    .expect("structural analyzer constructs");
    let mut structural_entities = Vec::new();
    for (snapshot, source) in fixture.snapshots.iter().zip(&fixture.sources) {
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
            LanguageId::new(fixture.language.as_str()).expect("language is valid"),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            fixture.build_context,
            &limits,
        )
        .expect("analysis request is valid")
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("structural analysis commits");
        structural_entities.extend(output.document().entities.iter().cloned());
    }

    let declarations = |entities: &[rootlight_ir::EntityRecord]| {
        entities
            .iter()
            .filter(|entity| {
                entity.kind != EntityKind::ExternalSymbol
                    && !entity.flags.contains(&EntityFlag::Synthetic)
            })
            .filter_map(|entity| {
                entity.evidence.source.as_ref().map(|source| {
                    (
                        (source.span(), entity.kind, entity.canonical_name.clone()),
                        entity.id,
                    )
                })
            })
            .collect::<BTreeMap<_, _>>()
    };
    let structural = declarations(&structural_entities);
    let project = declarations(&project_output.document().entities);
    for (key, structural_id) in &structural {
        assert_eq!(
            project.get(key),
            Some(structural_id),
            "{} project declaration does not preserve the structural key and identity: {key:?}",
            fixture.language.as_str(),
        );
    }
    structural
        .into_keys()
        .map(|(_source, kind, name)| (kind, name))
        .collect()
}

#[test]
fn bounded_ecmascript_imports_and_nested_bindings_keep_structural_identity() {
    let mut source = String::from(
        "import {provide} from './provider'; const [top] = values; export function run([argument]) { { const {local} = argument; } { const {local} = argument; } use(value => { const {nested} = value; return nested; }, value => { const {nested} = value; return nested; }); return provide(argument); }\n",
    );
    for _ in 0..500 {
        source.push_str("run(values);\n");
    }
    for (language, consumer, provider) in [
        (
            SemanticProjectLanguage::JavaScript,
            "src/consumer.js",
            "src/provider.js",
        ),
        (
            SemanticProjectLanguage::TypeScript,
            "src/consumer.ts",
            "src/provider.ts",
        ),
    ] {
        let fixture = ProjectFixture::new(
            [consumer, provider],
            [
                source.as_str(),
                "export function provide(value) { return value; }\n",
            ],
            language,
        );
        let declarations = assert_bounded_project_preserves_structural_declarations(&fixture);
        for expected in [
            (EntityKind::Import, "provide"),
            (EntityKind::Variable, "top"),
            (EntityKind::Variable, "local"),
            (EntityKind::Variable, "nested"),
            (EntityKind::Parameter, "argument"),
            (EntityKind::Parameter, "value"),
        ] {
            assert!(
                declarations.contains(&(expected.0, expected.1.to_owned())),
                "{expected:?}"
            );
        }
    }
}

#[test]
fn excessive_parser_diagnostics_commit_a_bounded_summary() {
    let fixture = ProjectFixture::new(
        ["dep.py", "main.py"],
        ["BROKEN\nBROKEN\n", "BROKEN\nBROKEN\n"],
        SemanticProjectLanguage::Python,
    );
    let mut ir_limits = IrLimits::default();
    ir_limits.max_diagnostics = 3;
    let limits = limits_with_ir_limits(ir_limits);
    let request = fixture.request(&limits, AnalysisTier::TierB);
    let analyzer = analyzer(SemanticProjectLanguage::Python, fixture.build_context);

    let output = execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("excessive parser diagnostics commit bounded output");

    assert_eq!(
        output.report().work().coverage().status(),
        CoverageStatus::Bounded
    );
    assert_eq!(output.document().diagnostics.len(), 3);
    assert!(output.document().diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "project-parser-diagnostics-truncated"
            && diagnostic.coverage_effect == CoverageStatus::Bounded
    }));
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
            for index in 0..source.matches("BROKEN").count() {
                diagnostics.push(AdapterDiagnostic::new(
                    DiagnosticCode::new(&format!("fixture-parse-error-{index}"))
                        .expect("code is valid"),
                    DiagnosticSeverity::Error,
                    Some(full.clone()),
                    CoverageStatus::Bounded,
                ));
            }
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
                let declaration_kind = if before.trim_start().ends_with("class ") {
                    format!("{language}.class.declaration")
                } else {
                    format!("{language}.function.declaration")
                };
                facts.push(SyntaxFact::new(
                    declaration_id,
                    Some(1),
                    SyntaxFactKind::Declaration,
                    span(file, line_start, line_end),
                    1,
                    SyntaxKindLabel::new(&declaration_kind).expect("label is valid"),
                ));
                local_id += 1;
                facts.push(SyntaxFact::new(
                    local_id,
                    Some(declaration_id),
                    SyntaxFactKind::Occurrence,
                    span(file, offset, offset + name.len()),
                    2,
                    SyntaxKindLabel::new(&format!("{language}.definition"))
                        .expect("label is valid"),
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
    fn new<const N: usize>(
        paths: [&str; N],
        sources: [&str; N],
        language: SemanticProjectLanguage,
    ) -> Self {
        Self::new_owned(
            paths.into_iter().map(str::to_owned).collect(),
            sources.into_iter().map(str::to_owned).collect(),
            language,
        )
    }

    fn new_owned(
        paths: Vec<String>,
        sources: Vec<String>,
        language: SemanticProjectLanguage,
    ) -> Self {
        assert_eq!(paths.len(), sources.len());
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("temporary directory is available");
        for (path, source) in paths.iter().zip(&sources) {
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
            .iter()
            .map(|path| RelativePath::parse(Path::new(path)).expect("path is valid"))
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
        let mut inputs: Vec<_> = self
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
        inputs.sort_by(|left, right| {
            left.source()
                .path()
                .identity_bytes()
                .cmp(right.source().path().identity_bytes())
        });
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
    real_parser_limits_with_project_files(16)
}

fn real_parser_limits_with_project_files(max_files: usize) -> AnalysisLimits {
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
        ProjectAnalysisLimits::new(max_files, 512 * 1024, 64 * 1024, 128, 128 * 1024, 256, 256)
            .expect("project limits are valid"),
    )
}

fn limits() -> AnalysisLimits {
    limits_with_output_bytes(4 * 1024 * 1024)
}

fn limits_with_output_bytes(max_output_bytes: usize) -> AnalysisLimits {
    limits_with_output_bytes_and_ir_limits(max_output_bytes, IrLimits::default())
}

fn limits_with_ir_limits(ir_limits: IrLimits) -> AnalysisLimits {
    limits_with_output_bytes_and_ir_limits(4 * 1024 * 1024, ir_limits)
}

fn limits_with_output_bytes_and_ir_limits(
    max_output_bytes: usize,
    ir_limits: IrLimits,
) -> AnalysisLimits {
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
        ir_limits,
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
