//! F# implementation and signature profiles retain exact source declarations.
//! Unsupported syntax remains scoped capability evidence, without inferred relations.

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
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, ExtensionSupport,
    FactDomain, IrLimits, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
    SkippedRegionReason, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

struct Fixture {
    _directory: TempDir,
    text: String,
    language: LanguageId,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(language: &str, text: &str) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let file = if language == "fsharp-signature" {
            "Sample.fsi"
        } else {
            "Sample.fs"
        };
        fs::write(directory.path().join(file), text).unwrap();
        let repository = derive_repository(b"fsharp-native-structural-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(file)).unwrap(), 16 * 1024)
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
            language: LanguageId::new(language).unwrap(),
            snapshot,
            source,
        }
    }

    fn parse(
        &self,
        max_nodes: usize,
        cancellation: &Cancellation,
    ) -> Result<ParseOutput, rootlight_adapter_sdk::AdapterError> {
        let limits = limits(max_nodes);
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            self.language.clone(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        execute_parse(
            provider().as_ref(),
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )
    }

    fn analyze(&self) -> AnalysisOutput {
        self.analyze_with_ir_limits(IrLimits::default())
    }

    fn analyze_with_ir_limits(&self, ir_limits: IrLimits) -> AnalysisOutput {
        let limits = limits_with_ir(4096, ir_limits);
        let analyzer = TreeSitterAnalyzer::new(
            provider(),
            rootlight_ir::ProducerIdentity::new(
                "fsharp-native-contract",
                "1.0",
                content_hash(b"fsharp-native-configuration"),
            )
            .unwrap(),
            self.language.clone(),
            "tree-sitter-fsharp-0.3.12",
            content_hash(b"fsharp-native-binary"),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            self.language.clone(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"fsharp-native-build-context")),
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
        output
    }

    fn span(&self, written: &str) -> SourceSpan {
        let start = self.text.find(written).unwrap();
        SourceSpan::new(
            self.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + written.len()).unwrap(),
        )
        .unwrap()
    }
}

fn provider() -> Arc<TreeSitterProvider> {
    Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                16 * 1024,
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
    )
}

fn limits(max_nodes: usize) -> AnalysisLimits {
    limits_with_ir(max_nodes, IrLimits::default())
}

fn limits_with_ir(max_nodes: usize, ir_limits: IrLimits) -> AnalysisLimits {
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
        16 * 1024,
        max_nodes,
        128,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir_limits,
    )
    .unwrap()
}

#[test]
fn both_native_profiles_retain_exact_declaration_names_and_no_call_captures() {
    for (language, source) in [
        ("fsharp", include_str!("fixtures/fsharp/structural.fs")),
        (
            "fsharp-signature",
            include_str!("fixtures/fsharp/structural.fsi"),
        ),
    ] {
        let fixture = Fixture::new(language, source);
        let syntax = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            syntax.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(syntax.diagnostics().is_empty());
        let labels: Vec<_> = syntax
            .facts()
            .iter()
            .map(|fact| fact.syntax_kind().as_str())
            .collect();
        assert!(labels.contains(&"fsharp.open.import"));
        assert!(labels.contains(&"fsharp.open_type.import"));
        assert!(
            !labels
                .iter()
                .any(|label| label.ends_with(".call") || label.ends_with(".reference"))
        );
        let output = fixture.analyze();
        let document = output.document();
        for (name, kind) in [
            ("Sample.Root", EntityKind::Module),
            ("Nested", EntityKind::Module),
            ("Record", EntityKind::Struct),
            ("Choice", EntityKind::Union),
            ("Name", EntityKind::Field),
            ("Count", EntityKind::Field),
            ("Empty", EntityKind::Constructor),
            ("Named", EntityKind::Constructor),
            ("Container", EntityKind::Class),
            ("new", EntityKind::Constructor),
            ("Read", EntityKind::Method),
            ("Value", EntityKind::Property),
        ] {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name && entity.kind == kind),
                "{language}: {name} {kind:?}"
            );
        }
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "``value with spaces``")
        );
        assert!(document.relations.iter().all(|relation| matches!(
            relation.predicate,
            RelationPredicate::Contains
                | RelationPredicate::Declares
                | RelationPredicate::DefinesAt
        )));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.source.span() == fixture.span("open System"))
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "fsharp-type-representation-ambiguous")
        );
        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        if language == "fsharp" {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == "( /+ )"
                        && entity.kind == EntityKind::Function)
            );
        } else {
            assert!(document.entities.iter().any(
                |entity| entity.canonical_name == "add" && entity.kind == EntityKind::Variable
            ));
        }
    }
}

#[test]
fn parenthesized_enum_values_preserve_case_identity_and_exact_sources() {
    for language in ["fsharp", "fsharp-signature"] {
        let following = if language == "fsharp-signature" {
            "val following: int"
        } else {
            "let following = 30"
        };
        let declaration = "Flags =\n    | First = (1u)\n    | Next = (1u <<< 0)";
        let canonical = format!("module EnumValues\ntype {declaration}\n{following}");
        for newline in ["\n", "\r\n"] {
            for trailing in [false, true] {
                let mut text = canonical.replace('\n', newline);
                if trailing {
                    text.push_str(newline);
                }
                let fixture = Fixture::new(language, &text);
                let parsed = fixture.parse(4096, &deadline()).unwrap();
                assert_eq!(
                    parsed.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert!(parsed.diagnostics().is_empty());
                for written in ["First = (1u)", "Next = (1u <<< 0)"] {
                    assert_eq!(
                        parsed
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == "fsharp.enum_constant.declaration"
                                    && fact.span() == fixture.span(written)
                            })
                            .count(),
                        1
                    );
                }
                let output = fixture.analyze();
                let document = output.document();
                let unique = |name: &str, kind| {
                    let matches: Vec<_> = document
                        .entities
                        .iter()
                        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                        .collect();
                    assert_eq!(matches.len(), 1, "{language}: {name}");
                    matches[0]
                };
                let module = unique("EnumValues", EntityKind::Module);
                let enumeration = unique("Flags", EntityKind::Enum);
                assert_eq!(enumeration.container, Some(ContainerRef::Entity(module.id)));
                assert_eq!(
                    enumeration.evidence.source.as_ref().unwrap().span(),
                    fixture.span(&declaration.replace('\n', newline))
                );
                for (name, written) in [("First", "First = (1u)"), ("Next", "Next = (1u <<< 0)")] {
                    let case = unique(name, EntityKind::Constant);
                    assert_eq!(case.container, Some(ContainerRef::Entity(enumeration.id)));
                    let source = case.evidence.source.as_ref().unwrap();
                    assert_eq!(source.span(), fixture.span(written));
                    assert_eq!(source.generation(), fixture.source.generation());
                    assert_eq!(source.repository(), fixture.source.repository());
                    assert_eq!(source.content_hash(), fixture.source.content_hash());
                    let definitions: Vec<_> = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::Definition
                                && site.target == (OccurrenceTarget::Resolved { symbol: case.id })
                        })
                        .collect();
                    assert_eq!(definitions.len(), 1);
                    assert_eq!(definitions[0].source.span(), fixture.span(name));
                    assert_eq!(definitions[0].enclosing, Some(enumeration.id));
                    assert!(document.relations.iter().any(|relation| {
                        relation.predicate == RelationPredicate::Contains
                            && relation.subject == RelationEndpoint::Entity(enumeration.id)
                            && relation.object == RelationEndpoint::Entity(case.id)
                            && relation.evidence.source.as_ref().unwrap().span()
                                == fixture.span(written)
                    }));
                }
                assert_eq!(
                    unique("following", EntityKind::Variable).container,
                    Some(ContainerRef::Entity(module.id))
                );
                assert!(!document.skipped_regions.iter().any(|gap| {
                    matches!(
                        gap.reason,
                        SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                    )
                }));
                assert!(document.relations.iter().all(|relation| {
                    matches!(
                        relation.predicate,
                        RelationPredicate::Contains
                            | RelationPredicate::Declares
                            | RelationPredicate::DefinesAt
                    )
                }));
            }
        }
    }
}

#[test]
fn shallower_enum_case_bars_preserve_exact_symbols_and_enum_ownership() {
    for language in ["fsharp", "fsharp-signature"] {
        for nested in [false, true] {
            let (owner, prefix, declaration_indent, first_indent, bar_indent) = if nested {
                (
                    "Nested",
                    "module EnumLayout\nmodule Nested =\n",
                    "    ",
                    "          ",
                    "        ",
                )
            } else {
                ("EnumLayout", "module EnumLayout\n", "", "      ", "    ")
            };
            let enumeration = format!("State =\n{first_indent}First = 0\n{bar_indent}| Next = 1");
            let following = if language == "fsharp-signature" {
                "val following: int"
            } else {
                "let following = 30"
            };
            let canonical = format!(
                "{prefix}{declaration_indent}type {enumeration}\n{declaration_indent}{following}"
            );
            for newline in ["\n", "\r\n"] {
                for trailing_newline in [false, true] {
                    let mut text = canonical.replace('\n', newline);
                    if trailing_newline {
                        text.push_str(newline);
                    }
                    let fixture = Fixture::new(language, &text);
                    let parsed = fixture.parse(4096, &deadline()).unwrap();
                    assert_eq!(
                        parsed.report().coverage().status(),
                        CoverageStatus::Complete
                    );
                    assert!(parsed.diagnostics().is_empty());
                    let enum_written = enumeration.replace('\n', newline);
                    for (label, written) in [
                        ("fsharp.enum.declaration", enum_written.as_str()),
                        ("fsharp.enum_constant.declaration", "First = 0"),
                        ("fsharp.enum_constant.declaration", "Next = 1"),
                    ] {
                        assert_eq!(
                            parsed
                                .facts()
                                .iter()
                                .filter(|fact| fact.syntax_kind().as_str() == label
                                    && fact.span() == fixture.span(written))
                                .count(),
                            1,
                            "{language}: {label} {written}"
                        );
                    }
                    assert_eq!(
                        parsed
                            .facts()
                            .iter()
                            .filter(|fact| fact.syntax_kind().as_str()
                                == "fsharp.enum_constant.declaration")
                            .count(),
                        2
                    );
                    assert!(!parsed.facts().iter().any(|fact| {
                        let label = fact.syntax_kind().as_str();
                        label.ends_with(".call") || label.ends_with(".reference")
                    }));
                    let output = fixture.analyze();
                    let document = output.document();
                    let unique_entity = |name: &str, kind| {
                        let matches: Vec<_> = document
                            .entities
                            .iter()
                            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                            .collect();
                        assert_eq!(matches.len(), 1, "{language}: {name} {kind:?}");
                        matches[0]
                    };
                    let module = unique_entity(owner, EntityKind::Module);
                    let enumeration = unique_entity("State", EntityKind::Enum);
                    assert_eq!(enumeration.container, Some(ContainerRef::Entity(module.id)));
                    assert_eq!(
                        enumeration.evidence.source.as_ref().unwrap().span(),
                        fixture.span(&enum_written)
                    );
                    for (name, written) in [("First", "First = 0"), ("Next", "Next = 1")] {
                        let case = unique_entity(name, EntityKind::Constant);
                        assert_eq!(case.container, Some(ContainerRef::Entity(enumeration.id)));
                        let reference = case.evidence.source.as_ref().unwrap();
                        assert_eq!(reference.span(), fixture.span(written));
                        assert_eq!(reference.repository(), fixture.source.repository());
                        assert_eq!(reference.generation(), fixture.source.generation());
                        assert_eq!(reference.content_hash(), fixture.source.content_hash());
                        let definitions: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.target
                                        == (OccurrenceTarget::Resolved { symbol: case.id })
                            })
                            .collect();
                        assert_eq!(definitions.len(), 1, "{language}: {name}");
                        assert_eq!(definitions[0].source.span(), fixture.span(name));
                        assert_eq!(definitions[0].enclosing, Some(enumeration.id));
                        assert!(document.relations.iter().any(|relation| {
                            relation.predicate == RelationPredicate::Contains
                                && relation.subject == RelationEndpoint::Entity(enumeration.id)
                                && relation.object == RelationEndpoint::Entity(case.id)
                                && relation.evidence.source.as_ref().unwrap().span()
                                    == fixture.span(written)
                        }));
                    }
                    let following = unique_entity("following", EntityKind::Variable);
                    assert_eq!(following.container, Some(ContainerRef::Entity(module.id)));
                    for (entity, name) in [(enumeration, "State"), (following, "following")] {
                        let definitions: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.target
                                        == (OccurrenceTarget::Resolved { symbol: entity.id })
                            })
                            .collect();
                        assert_eq!(definitions.len(), 1, "{language}: {name}");
                        assert_eq!(definitions[0].source.span(), fixture.span(name));
                        assert_eq!(definitions[0].enclosing, Some(module.id));
                    }
                    assert!(!document.skipped_regions.iter().any(|gap| matches!(
                        gap.reason,
                        SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                    )));
                    assert!(document.relations.iter().all(|relation| matches!(
                        relation.predicate,
                        RelationPredicate::Contains
                            | RelationPredicate::Declares
                            | RelationPredicate::DefinesAt
                    )));
                }
            }
        }
    }
}

#[test]
fn recursive_siblings_keep_disjoint_source_and_module_ownership() {
    let fixture = Fixture::new(
        "fsharp",
        "module Rec\nlet rec first n = n + 1\nand second n = n + 2\n",
    );
    let syntax = fixture.parse(4096, &deadline()).unwrap();
    let declarations: Vec<_> = syntax
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "fsharp.function.declaration")
        .collect();
    assert_eq!(declarations.len(), 2);
    assert_eq!(declarations[0].span(), fixture.span("first n = n + 1"));
    assert_eq!(declarations[1].span(), fixture.span("second n = n + 2"));
    let output = fixture.analyze();
    let document = output.document();
    let module = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Rec")
        .unwrap();
    for name in ["first", "second"] {
        let function = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(function.kind, EntityKind::Function);
        assert_eq!(function.container, Some(ContainerRef::Entity(module.id)));
    }
}

#[test]
fn function_valued_signature_properties_remain_properties() {
    let fixture = Fixture::new(
        "fsharp-signature",
        "module Contracts\ntype I = interface\n    abstract member FunctionValue: (int -> int)\n    abstract member Curried: int -> int\nend\n",
    );
    let output = fixture.analyze();
    let entities = &output.document().entities;
    assert!(
        entities
            .iter()
            .any(|entity| entity.canonical_name == "FunctionValue"
                && entity.kind == EntityKind::Property)
    );
    assert!(
        entities
            .iter()
            .any(|entity| entity.canonical_name == "Curried" && entity.kind == EntityKind::Method)
    );
}

#[test]
fn indexed_accessors_remain_properties_in_both_profiles() {
    for language in ["fsharp", "fsharp-signature"] {
        for newline in ["\n", "\r\n"] {
            let text = "module Contracts\ntype Accessors = interface\n    abstract member Getter: index:int -> int with get\n    abstract member Setter: index:int -> int with set\n    abstract member Both: index:int -> int with get, set\n    abstract member Reversed: index:int -> int with set, get\n    abstract member Curried: index:int -> int\n    abstract member FunctionValue: (int -> int)\nend\n".replace('\n', newline);
            let fixture = Fixture::new(language, &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert!(parsed.diagnostics().is_empty());
            let output = fixture.analyze();
            let document = output.document();
            for (name, kind) in [
                ("Getter", EntityKind::Property),
                ("Setter", EntityKind::Property),
                ("Both", EntityKind::Property),
                ("Reversed", EntityKind::Property),
                ("Curried", EntityKind::Method),
                ("FunctionValue", EntityKind::Property),
            ] {
                let entities: Vec<_> = document
                    .entities
                    .iter()
                    .filter(|entity| entity.canonical_name == name)
                    .collect();
                assert_eq!(entities.len(), 1, "{language}: {name}");
                assert_eq!(entities[0].kind, kind, "{language}: {name}");
                let definitions: Vec<_> = parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.kind() == SyntaxFactKind::Occurrence
                            && fact.syntax_kind().as_str().ends_with(".definition")
                            && fact.span() == fixture.span(name)
                    })
                    .collect();
                assert_eq!(definitions.len(), 1, "{language}: exact definition {name}");
                assert_eq!(
                    definitions[0].syntax_kind().as_str(),
                    "fsharp.name.definition"
                );
            }
        }
    }
}

#[test]
fn unnamed_union_payloads_keep_exact_entity_gaps_without_invented_field_names() {
    let text = "module Payloads\ntype Choice = | Unnamed of System.Int32 * System.String | Named of count:int * text:string | Grouped of (System.Object list)\n";
    for language in ["fsharp", "fsharp-signature"] {
        for newline in ["\n", "\r\n"] {
            let text = text.replace('\n', newline);
            let fixture = Fixture::new(language, &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert!(parsed.diagnostics().is_empty());
            let output = fixture.analyze();
            let document = output.document();
            for written in ["System.Int32", "System.String", "(System.Object list)"] {
                let gaps: Vec<_> = document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == FactDomain::Entities
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.source.span() == fixture.span(written)
                    })
                    .collect();
                assert_eq!(gaps.len(), 1, "{language}: payload gap {written}");
            }
            for (name, kind) in [
                ("Choice", EntityKind::Union),
                ("Unnamed", EntityKind::Constructor),
                ("Named", EntityKind::Constructor),
                ("Grouped", EntityKind::Constructor),
                ("count", EntityKind::Field),
                ("text", EntityKind::Field),
            ] {
                assert_eq!(
                    document
                        .entities
                        .iter()
                        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                        .count(),
                    1,
                    "{language}: named declaration {name}"
                );
            }
            assert_eq!(
                document
                    .entities
                    .iter()
                    .filter(|entity| entity.kind == EntityKind::Field)
                    .count(),
                2
            );
            assert!(
                output
                    .report()
                    .coverage()
                    .domains()
                    .iter()
                    .any(|domain| domain.domain() == FactDomain::Entities
                        && domain.status() == CoverageStatus::Bounded)
            );
        }
    }
}

#[test]
fn anonymous_object_members_keep_owner_gaps_instead_of_binding_containers() {
    for newline in ["\n", "\r\n"] {
        let owner = "new System.IDisposable with\n        member this.Dispose() = ()";
        let text = format!("module Objects\ntype Regular() =\n    member this.Read() = 1\nlet anonymous =\n    {{ {owner} }}\nlet following = 2\n").replace('\n', newline);
        let owner = owner.replace('\n', newline);
        let fixture = Fixture::new("fsharp", &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert!(parsed.diagnostics().is_empty());
        let output = fixture.analyze();
        let document = output.document();
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "Dispose")
        );
        let markers: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "fsharp.unsupported_anonymous_object.declaration"
            })
            .collect();
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].span(), fixture.span(&owner));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.source.span() == fixture.span(&owner))
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.source.span() == fixture.span("this.Dispose() = ()")
                    && gap.detail == "stable-scope-identity-unavailable")
        );
        let class = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Regular")
            .unwrap();
        let method = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Read")
            .unwrap();
        assert_eq!(class.kind, EntityKind::Class);
        assert_eq!(method.kind, EntityKind::Method);
        assert_eq!(method.container, Some(ContainerRef::Entity(class.id)));
        for name in ["anonymous", "following"] {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name
                        && entity.kind == EntityKind::Variable)
            );
        }
    }
}

#[test]
fn qualified_generic_types_preserve_alias_and_binding_evidence() {
    for written_type in [
        "Container<'T>.Builder",
        "Container<'T>.Middle.Builder",
        "Container<'T>.Builder<int>",
    ] {
        for language in ["fsharp", "fsharp-signature"] {
            let alias = format!("Nested<'T> = {written_type}");
            let (binding, binding_kind, signature_label, header) = if language == "fsharp-signature"
            {
                (
                    format!("val create: int -> {written_type}"),
                    EntityKind::Variable,
                    "fsharp.signature_val.signature",
                    format!("val create: int -> {written_type}"),
                )
            } else {
                (
                    format!("let create input : {written_type} = null"),
                    EntityKind::Function,
                    "fsharp.function.signature",
                    format!("create input : {written_type} "),
                )
            };
            let following = if language == "fsharp-signature" {
                "val following: int"
            } else {
                "let following = 30"
            };
            let canonical = format!("module TypeExample\ntype {alias}\n{binding}\n{following}");
            for newline in ["\n", "\r\n"] {
                for trailing_newline in [false, true] {
                    let mut text = canonical.replace('\n', newline);
                    if trailing_newline {
                        text.push_str(newline);
                    }
                    let fixture = Fixture::new(language, &text);
                    let parsed = fixture.parse(4096, &deadline()).unwrap();
                    assert_eq!(
                        parsed.report().coverage().status(),
                        CoverageStatus::Complete,
                        "{language}: {written_type}; newline={newline:?}; trailing_newline={trailing_newline}; diagnostics={:?}",
                        parsed.diagnostics()
                    );
                    assert!(parsed.diagnostics().is_empty());
                    for (label, written) in [
                        ("fsharp.type_alias.declaration", alias.as_str()),
                        (signature_label, header.as_str()),
                    ] {
                        let facts: Vec<_> = parsed
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == label
                                    && fact.span() == fixture.span(written)
                            })
                            .collect();
                        assert_eq!(facts.len(), 1, "{language}: {label}");
                        assert_eq!(facts[0].span(), fixture.span(written));
                    }
                    let output = fixture.analyze();
                    let document = output.document();
                    let named = |name: &str, kind| {
                        let found: Vec<_> = document
                            .entities
                            .iter()
                            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                            .collect();
                        assert_eq!(found.len(), 1, "{language}: {name}");
                        found[0]
                    };
                    let module = named("TypeExample", EntityKind::Module);
                    for (name, kind) in [
                        ("Nested", EntityKind::TypeAlias),
                        ("create", binding_kind),
                        ("following", EntityKind::Variable),
                    ] {
                        let entity = named(name, kind);
                        assert_eq!(entity.container, Some(ContainerRef::Entity(module.id)));
                        let reference = entity.evidence.source.as_ref().unwrap();
                        assert_eq!(reference.repository(), fixture.source.repository());
                        assert_eq!(reference.generation(), fixture.source.generation());
                        assert_eq!(reference.content_hash(), fixture.source.content_hash());
                        let definitions: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.target
                                        == (OccurrenceTarget::Resolved { symbol: entity.id })
                            })
                            .collect();
                        assert_eq!(definitions.len(), 1);
                        assert_eq!(definitions[0].source.span(), fixture.span(name));
                        assert_eq!(definitions[0].enclosing, Some(module.id));
                    }
                    assert!(!document.skipped_regions.iter().any(|gap| matches!(
                        gap.reason,
                        SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                    )));
                    // Constructed spelling does not prove external type identity.
                    assert!(document.relations.iter().all(|relation| matches!(
                        relation.predicate,
                        RelationPredicate::Contains
                            | RelationPredicate::Declares
                            | RelationPredicate::DefinesAt
                    )));
                }
            }
        }
    }
}

#[test]
fn numeric_loop_wildcards_preserve_enclosing_declarations() {
    for binder in ["_", "step"] {
        for (direction, first, last) in [("to", "1", "limit"), ("downto", "limit", "1")] {
            let canonical = format!(
                "module LoopExample\nlet repeat limit =\n    for {binder} = {first} {direction} {last} do\n        ()\nlet following = 30"
            );
            for newline in ["\n", "\r\n"] {
                for trailing_newline in [false, true] {
                    let mut text = canonical.replace('\n', newline);
                    if trailing_newline {
                        text.push_str(newline);
                    }
                    let fixture = Fixture::new("fsharp", &text);
                    let parsed = fixture.parse(4096, &deadline()).unwrap();
                    assert_eq!(
                        parsed.report().coverage().status(),
                        CoverageStatus::Complete,
                        "{binder}: {direction}; diagnostics={:?}",
                        parsed.diagnostics()
                    );
                    assert!(parsed.diagnostics().is_empty());
                    let signatures: Vec<_> = parsed
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "fsharp.function.signature"
                                && fact.span() == fixture.span("repeat limit ")
                        })
                        .collect();
                    assert_eq!(signatures.len(), 1);
                    let output = fixture.analyze();
                    let document = output.document();
                    let module = document
                        .entities
                        .iter()
                        .find(|entity| entity.canonical_name == "LoopExample")
                        .unwrap();
                    for (name, kind) in [
                        ("repeat", EntityKind::Function),
                        ("following", EntityKind::Variable),
                    ] {
                        let entities: Vec<_> = document
                            .entities
                            .iter()
                            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                            .collect();
                        assert_eq!(entities.len(), 1);
                        let entity = entities[0];
                        assert_eq!(entity.container, Some(ContainerRef::Entity(module.id)));
                        let reference = entity.evidence.source.as_ref().unwrap();
                        assert_eq!(reference.repository(), fixture.source.repository());
                        assert_eq!(reference.generation(), fixture.source.generation());
                        assert_eq!(reference.content_hash(), fixture.source.content_hash());
                        let definitions: Vec<_> = document
                            .occurrences
                            .iter()
                            .filter(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.target
                                        == (OccurrenceTarget::Resolved { symbol: entity.id })
                            })
                            .collect();
                        assert_eq!(definitions.len(), 1);
                        assert_eq!(definitions[0].source.span(), fixture.span(name));
                        assert_eq!(definitions[0].enclosing, Some(module.id));
                    }
                    // A loop binder does not establish a module-level declaration.
                    assert!(
                        !document.entities.iter().any(|entity| {
                            matches!(entity.canonical_name.as_str(), "_" | "step")
                        })
                    );
                    assert!(!document.skipped_regions.iter().any(|gap| matches!(
                        gap.reason,
                        SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                    )));
                }
            }
        }
    }
}

#[test]
fn body_edits_preserve_header_identity_and_signature_evidence() {
    let before_fixture = Fixture::new("fsharp", "module Stable\nlet pick value = value + 1\n");
    let after_fixture = Fixture::new("fsharp", "module Stable\nlet pick value = value + 999\n");
    let before = before_fixture.analyze();
    let after = after_fixture.analyze();
    let named = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "pick")
            .unwrap()
            .clone()
    };
    assert_eq!(named(&before).id, named(&after).id);
    let signatures = |fixture: &Fixture| {
        fixture
            .parse(4096, &deadline())
            .unwrap()
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "fsharp.function.signature")
            .map(|fact| {
                fixture
                    .text
                    .get(
                        usize::try_from(fact.span().start_byte()).unwrap()
                            ..usize::try_from(fact.span().end_byte()).unwrap(),
                    )
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(signatures(&before_fixture), signatures(&after_fixture));
    assert!(
        !signatures(&before_fixture)
            .iter()
            .any(|header| header.contains("value +"))
    );
}

#[test]
fn excluded_declarations_produce_exact_scoped_entity_gaps() {
    let fixture = Fixture::new(
        "fsharp",
        "module Gaps\nlet (left, right) = (1, 2)\ntype Alias = string\nmodule AliasModule = System\n",
    );
    let output = fixture.analyze();
    let document = output.document();
    for span in [
        fixture.span("(left, right) = (1, 2)"),
        fixture.span("Alias = string"),
        fixture.span("module AliasModule = System"),
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.source.span() == span),
            "missing scoped gap: {span:?}"
        );
    }
    assert!(!document.entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "left" | "right" | "Alias" | "AliasModule"
    )));
    assert!(
        output
            .report()
            .coverage()
            .domains()
            .iter()
            .any(|domain| domain.domain() == FactDomain::Entities
                && domain.status() == CoverageStatus::Bounded)
    );
}

#[test]
fn parenthesized_type_ambiguity_remains_scoped_in_both_profiles() {
    let text = "module Types\ntype Parenthesized = (string)\ntype Nested = ((``QuotedCase``))\ntype Qualified = (System.String)\ntype Generic = (list<int>)\ntype 'a Identity = ('a)\n";
    for language in ["fsharp", "fsharp-signature"] {
        let fixture = Fixture::new(language, text);
        let output = fixture.analyze();
        let document = output.document();
        for written in ["Parenthesized = (string)", "Nested = ((``QuotedCase``))"] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == FactDomain::Entities
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.source.span() == fixture.span(written))
            );
        }
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "Parenthesized" | "Nested"))
        );
        for name in ["Qualified", "Generic", "Identity"] {
            assert!(document.entities.iter().any(
                |entity| entity.canonical_name == name && entity.kind == EntityKind::TypeAlias
            ));
        }
    }
}

#[test]
fn branch_local_bindings_do_not_coalesce_without_lexical_scope_evidence() {
    let fixture = Fixture::new(
        "fsharp",
        "module Locals\nlet outer flag =\n    if flag then\n        let local x = x + 1\n        local 1\n    else\n        let local x = x + 2\n        local 2\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(parsed.diagnostics().is_empty());
    let output = fixture.analyze();
    let document = output.document();
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "outer" && entity.kind == EntityKind::Function)
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "local")
    );
    for written in ["local x = x + 1", "local x = x + 2"] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.source.span() == fixture.span(written)),
            "missing local binding gap: {written}"
        );
    }
}

#[test]
fn rejected_owner_names_do_not_reparent_short_child_declarations() {
    let owner = "LongOwner".repeat(12);
    for language in ["fsharp", "fsharp-signature"] {
        let signature = language == "fsharp-signature";
        for owner_shape in ["class", "module", "namespace"] {
            let (text, owner_label, child_label) = match (owner_shape, signature) {
                ("class", false) => (
                    format!(
                        "module Bounds\ntype {owner}(seed: int) =\n    member this.Read amount = seed + amount\n"
                    ),
                    "fsharp.class.declaration",
                    "fsharp.method.declaration",
                ),
                ("class", true) => (
                    format!(
                        "module Bounds\ntype {owner} =\n    new: int -> {owner}\n    member Read: int -> int\n"
                    ),
                    "fsharp.class.declaration",
                    "fsharp.signature_method.declaration",
                ),
                ("module", false) => (
                    format!("module {owner}\nlet Read amount = amount\n"),
                    "fsharp.module.declaration",
                    "fsharp.function.declaration",
                ),
                ("module", true) => (
                    format!("module {owner}\nval Read: int -> int\n"),
                    "fsharp.module.declaration",
                    "fsharp.signature_val.declaration",
                ),
                ("namespace", false) => (
                    format!(
                        "namespace {owner}\ntype Box(seed: int) =\n    member this.Read amount = seed + amount\n"
                    ),
                    "fsharp.namespace.declaration",
                    "fsharp.method.declaration",
                ),
                ("namespace", true) => (
                    format!(
                        "namespace {owner}\ntype Box =\n    new: int -> Box\n    member Read: int -> int\n"
                    ),
                    "fsharp.namespace.declaration",
                    "fsharp.signature_method.declaration",
                ),
                _ => unreachable!(),
            };
            let fixture = Fixture::new(language, &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert!(parsed.diagnostics().is_empty());
            let owner_span = parsed
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == owner_label)
                .unwrap()
                .span();
            let child_span = parsed
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == child_label)
                .unwrap()
                .span();
            let mut ir_limits = IrLimits::default();
            ir_limits.max_string_bytes = 64;
            let output = fixture.analyze_with_ir_limits(ir_limits);
            let document = output.document();
            assert!(!document.entities.iter().any(|entity| entity.canonical_name == owner || entity.canonical_name == "Read"), "reparented child: {language}/{owner_shape}");
            for (span, detail) in [
                (owner_span, "declaration-name-unavailable"),
                (child_span, "stable-scope-identity-unavailable"),
            ] {
                assert!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.domain == FactDomain::Entities
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.source.span() == span
                            && gap.detail == detail),
                    "missing owner fence gap: {language}/{owner_shape}/{detail}"
                );
            }
        }
    }
}

#[test]
fn named_signature_argument_contracts_have_leaf_parameter_gaps() {
    let fixture = Fixture::new(
        "fsharp-signature",
        "module Contracts\ntype Container =\n    new: seed:int -> Container\n    member Read: amount:int -> int\n    member Anonymous: int -> int\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert!(parsed.diagnostics().is_empty());
    let markers: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "fsharp.unsupported_parameters.declaration")
        .collect();
    assert_eq!(markers.len(), 2);
    let output = fixture.analyze();
    let document = output.document();
    for written in ["seed:", "amount:"] {
        assert!(
            markers
                .iter()
                .any(|fact| fact.span() == fixture.span(written))
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.source.span() == fixture.span(written))
        );
    }
    for (name, kind) in [
        ("Container", EntityKind::Class),
        ("new", EntityKind::Constructor),
        ("Read", EntityKind::Method),
        ("Anonymous", EntityKind::Method),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind)
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter)
    );
}

#[test]
fn cancellation_and_node_limits_keep_the_existing_runtime_contract() {
    let fixture = Fixture::new("fsharp", include_str!("fixtures/fsharp/structural.fs"));
    let cancellation = deadline();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        fixture.parse(4096, &cancellation),
        Err(rootlight_adapter_sdk::AdapterError::Cancelled { .. })
    ));
    let limited = fixture.parse(8, &deadline()).unwrap();
    assert_eq!(
        limited.report().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(
        limited
            .facts()
            .iter()
            .all(|fact| fact.kind() != SyntaxFactKind::Declaration)
    );
    assert!(!limited.diagnostics().is_empty());
}
