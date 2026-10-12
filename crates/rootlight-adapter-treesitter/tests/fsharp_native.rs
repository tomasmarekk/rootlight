//! F# implementation and signature profiles retain exact source declarations.
//! Simple formal reads have exact local targets; unsupported syntax remains scoped evidence.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseOutput, ParseProvider,
    ParseRequest, StreamLimits, SyntaxFactKind, execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    FSharpAdmissionKind, FSharpAdmissionWitness, ParserSettings, RuntimeConfig, TreeSitterAnalyzer,
    TreeSitterProvider,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, ExtensionSupport,
    FactDomain, FactRef, IrLimits, LEXICAL_EXTENSION_NAMESPACE, LexicalEvidenceFormat,
    LexicalEvidenceKind, LexicalEvidenceV1, OccurrenceRole, OccurrenceTarget, RelationEndpoint,
    RelationPredicate, SkippedRegionReason, SourceRef, SourceSpan,
    decode_lexical_evidence_envelope, validate_ir_document, validate_lexical_evidence_envelope,
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
        self.parse_with_limits(&limits, cancellation)
    }

    fn parse_with_limits(
        &self,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<ParseOutput, rootlight_adapter_sdk::AdapterError> {
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            self.language.clone(),
            EncodingId::utf8(),
            Vec::new(),
            limits,
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
        self.analyze_with_parser(provider(), ir_limits)
    }

    fn analyze_with_parser(
        &self,
        parser: Arc<dyn ParseProvider>,
        ir_limits: IrLimits,
    ) -> AnalysisOutput {
        self.try_analyze_with_parser(parser, ir_limits).unwrap()
    }

    fn try_analyze_with_parser(
        &self,
        parser: Arc<dyn ParseProvider>,
        ir_limits: IrLimits,
    ) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
        let limits = limits_with_ir(4096, ir_limits);
        self.try_analyze_with_limits(parser, &limits)
    }

    fn try_analyze_with_limits(
        &self,
        parser: Arc<dyn ParseProvider>,
        limits: &AnalysisLimits,
    ) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
        let analyzer = TreeSitterAnalyzer::new(
            parser,
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
            limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )?;
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        Ok(output)
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

fn admission_probe_bytes(
    bytes: &[u8],
    encoding: &str,
    limits: &AnalysisLimits,
    cancellation: &Cancellation,
) -> Result<Option<FSharpAdmissionWitness>, rootlight_adapter_sdk::AdapterError> {
    let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(directory.path().join("Sample.fs"), bytes).unwrap();
    let repository = derive_repository(b"fsharp-admission-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("Sample.fs")).unwrap(),
            16 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([17; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let request = ParseRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("fsharp").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        limits,
    )
    .unwrap();
    provider().probe_fsharp_admission(&request, cancellation)
}

fn admission_probe_limits(nodes: usize, depth: usize, memory: usize) -> AnalysisLimits {
    let base = limits(4096);
    AnalysisLimits::new(
        base.max_source_bytes(),
        nodes,
        depth,
        base.max_embedded_ranges(),
        memory,
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap()
}

#[test]
fn admission_probe_retains_compound_declarations_and_true_eof() {
    for (kind, declaration) in [
        (
            FSharpAdmissionKind::NamedFunction,
            "let identity value = value",
        ),
        (
            FSharpAdmissionKind::BoundLambda,
            "let mapper = fun value -> value",
        ),
        (
            FSharpAdmissionKind::RecordType,
            "type Record = { Value: int }",
        ),
        (
            FSharpAdmissionKind::UnionType,
            "type Choice = | Empty | Value of int",
        ),
        (
            FSharpAdmissionKind::InterfaceType,
            "type I = interface\n    abstract member Read: unit -> int\nend",
        ),
        (
            FSharpAdmissionKind::ObjectType,
            "type Container(seed: int) =\n    member this.Value = seed",
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            for ending in ["", newline] {
                let declaration = declaration.replace('\n', newline);
                let prefix = format!("// source ownership{newline}let seed = 1 + 2{newline}");
                let text = format!("{prefix}{declaration}{ending}");
                let witness =
                    admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                        .unwrap()
                        .unwrap_or_else(|| panic!("missing {kind:?} witness for {text:?}"));
                assert_eq!(witness.kind(), kind);
                assert_eq!(
                    witness.declaration().content_hash(),
                    content_hash(text.as_bytes())
                );
                assert_eq!(
                    witness.sample().content_hash(),
                    content_hash(text.as_bytes())
                );
                assert_eq!(
                    witness.declaration().span().start_byte(),
                    u64::try_from(prefix.len()).unwrap()
                );
                assert_eq!(
                    witness.declaration().span().end_byte(),
                    u64::try_from(prefix.len() + declaration.len()).unwrap()
                );
                assert_eq!(witness.sample().span().start_byte(), 0);
                assert_eq!(
                    witness.sample().span().end_byte(),
                    u64::try_from(text.len()).unwrap()
                );
            }
        }
    }
}

#[test]
fn admission_probe_exception_prefix_keeps_later_compound_witness() {
    for exception in [
        "exception Missing",
        "exception Payload of int * string",
        "exception Detail of int with\n    member this.Code = 1",
    ] {
        for (kind, declaration) in [
            (
                FSharpAdmissionKind::NamedFunction,
                "let identity value = value",
            ),
            (
                FSharpAdmissionKind::RecordType,
                "type Record = { Value: int }",
            ),
        ] {
            for newline in ["\n", "\r\n"] {
                for ending in ["", newline] {
                    let exception = exception.replace('\n', newline);
                    let prefix = format!("// owned exception prefix{newline}{exception}{newline}");
                    let text = format!("{prefix}{declaration}{ending}");
                    let fixture = Fixture::new("fsharp", &text);
                    let parsed = fixture.parse(4096, &deadline()).unwrap();
                    assert_eq!(
                        parsed.report().coverage().status(),
                        CoverageStatus::Complete
                    );
                    assert!(
                        parsed.diagnostics().is_empty(),
                        "{text:?}: {:?}",
                        parsed.diagnostics()
                    );
                    let witness =
                        admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                            .unwrap()
                            .unwrap_or_else(|| {
                                panic!("missing later {kind:?} witness for {text:?}")
                            });
                    assert_eq!(witness.kind(), kind);
                    assert_eq!(
                        witness.declaration().content_hash(),
                        content_hash(text.as_bytes())
                    );
                    assert_eq!(
                        witness.sample().content_hash(),
                        content_hash(text.as_bytes())
                    );
                    assert_eq!(
                        witness.declaration().generation(),
                        GenerationId::from_bytes([17; 20])
                    );
                    assert_eq!(
                        witness.sample().generation(),
                        witness.declaration().generation()
                    );
                    assert_eq!(
                        witness.declaration().span().file(),
                        witness.sample().span().file()
                    );
                    assert_eq!(
                        witness.declaration().span().start_byte(),
                        u64::try_from(prefix.len()).unwrap()
                    );
                    assert_eq!(
                        witness.declaration().span().end_byte(),
                        u64::try_from(prefix.len() + declaration.len()).unwrap()
                    );
                    assert_eq!(
                        text.as_bytes()
                            .get(prefix.len()..prefix.len() + declaration.len()),
                        Some(declaration.as_bytes())
                    );
                    assert_eq!(witness.sample().span().start_byte(), 0);
                    assert_eq!(
                        witness.sample().span().end_byte(),
                        u64::try_from(text.len()).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn admission_probe_exception_prefix_is_not_positive_or_borrowed_evidence() {
    for text in [
        "exception Missing\n",
        "exception Payload of int\n",
        "exception Detail of int with\n    member this.Code = 1\n",
        "exception Missing\nlet answer = 1\n",
        "exception Missing\nexception Other\nlet answer = 1\n",
        "exception\nlet identity value = value\n",
        "exception Payload of\nlet identity value = value\n",
        "exception Detail of int with\n    member this.Code =\nlet identity value = value\n",
        "foreign words\nexception Missing\nlet identity value = value\n",
        "exception Missing\nforeign words\nlet identity value = value\n",
        "exception Missing\nmodule Nested =\n    let identity value = value\n",
        "exception Missing\nlet wrapper =\n    let identity value = value\n    identity 1\n",
        "exception Missing\nlet identity value = value\nlet unfinished = (\n",
    ] {
        for newline in ["\n", "\r\n"] {
            let text = text.replace('\n', newline);
            assert!(
                admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                    .unwrap()
                    .is_none(),
                "unproved exception-prefix admission for {text:?}"
            );
        }
    }
}

#[test]
fn admission_probe_exception_prefix_requires_complete_sampled_witness_line() {
    let declaration = "let identity value = value";
    for newline in ["\n", "\r\n"] {
        let prefix = format!("exception Boundary{newline}");
        let padding_bytes = 8192 - prefix.len() - declaration.len() - 2 * newline.len() - 2;
        let padding = format!("//{}{newline}", " ".repeat(padding_bytes));
        let complete =
            format!("{prefix}{padding}{declaration}{newline}// source continues{newline}");
        let witness =
            admission_probe_bytes(complete.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("missing complete exception-prefix witness for {newline:?}")
                });
        assert_eq!(witness.kind(), FSharpAdmissionKind::NamedFunction);
        assert_eq!(
            witness.declaration().span().start_byte(),
            u64::try_from(prefix.len() + padding.len()).unwrap()
        );
        assert_eq!(
            witness.declaration().span().end_byte(),
            u64::try_from(8192 - newline.len()).unwrap()
        );
        assert_eq!(witness.sample().span().end_byte(), 8192);
        assert_eq!(
            witness.declaration().content_hash(),
            content_hash(complete.as_bytes())
        );
        assert_eq!(
            witness.sample().content_hash(),
            content_hash(complete.as_bytes())
        );
        assert_ne!(
            witness.sample().content_hash(),
            content_hash(&complete.as_bytes()[..8192])
        );
        let partial = format!(
            "{prefix}//{}{newline}{declaration}{newline}// source continues{newline}",
            " ".repeat(padding_bytes + 1)
        );
        assert!(
            admission_probe_bytes(partial.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .is_none(),
            "partial final witness line admitted for {newline:?}"
        );
    }
}

fn assert_constructorless_member_admission(declaration: &str) {
    for newline in ["\n", "\r\n"] {
        for ending in ["", newline] {
            let declaration = declaration.replace('\n', newline);
            let prefix = format!("// owned type body{newline}let seed = 1{newline}");
            let text = format!("{prefix}{declaration}{ending}");
            let fixture = Fixture::new("fsharp", &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert!(
                parsed.diagnostics().is_empty(),
                "{text:?}: {:?}",
                parsed.diagnostics()
            );
            assert!(
                parsed
                    .facts()
                    .iter()
                    .all(|fact| fact.kind() != SyntaxFactKind::ErrorRecovery)
            );
            let witness =
                admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                    .unwrap()
                    .unwrap_or_else(|| {
                        panic!("missing constructorless member witness for {text:?}")
                    });
            assert_eq!(witness.kind(), FSharpAdmissionKind::ObjectType);
            assert_eq!(
                witness.declaration().content_hash(),
                content_hash(text.as_bytes())
            );
            assert_eq!(
                witness.sample().content_hash(),
                content_hash(text.as_bytes())
            );
            assert_eq!(
                witness.declaration().generation(),
                GenerationId::from_bytes([17; 20])
            );
            assert_eq!(
                witness.declaration().span().file(),
                witness.sample().span().file()
            );
            assert_eq!(
                witness.declaration().span().start_byte(),
                u64::try_from(prefix.len()).unwrap()
            );
            assert_eq!(
                witness.declaration().span().end_byte(),
                u64::try_from(prefix.len() + declaration.len()).unwrap()
            );
            assert_eq!(witness.sample().span().start_byte(), 0);
            assert_eq!(
                witness.sample().span().end_byte(),
                u64::try_from(text.len()).unwrap()
            );
        }
    }
}

#[test]
fn admission_probe_retains_constructorless_abstract_members() {
    for declaration in [
        "type Contract =\n    abstract Read: unit -> int",
        "type Contract =\n    abstract member Read: unit -> int",
    ] {
        assert_constructorless_member_admission(declaration);
    }
}

#[test]
fn admission_probe_retains_constructorless_object_members() {
    assert_constructorless_member_admission("type Holder =\n    member this.Value = 3");
}

#[test]
fn admission_probe_retains_constructorless_static_members() {
    assert_constructorless_member_admission("type Utility =\n    static member Echo value = value");
}

#[test]
fn admission_probe_does_not_borrow_constructorless_members_across_ownership_barriers() {
    for text in [
        "type System.Object with\n    member this.Read() = 1\n",
        "foreign words\ntype Utility =\n    static member Echo value = value\n",
        "type Alias = int\n()\ntype Utility =\n    static member Echo value = value\n",
        "type Holder =\n    member this.Value =\n",
    ] {
        assert!(
            admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .is_none(),
            "unowned constructorless member witness for {text:?}"
        );
    }
}

#[test]
fn admission_probe_does_not_promote_atomic_or_unowned_syntax() {
    for text in [
        "let answer = 1\n",
        "let first = 1\nlet second = 2\n",
        "type Choice = Empty\n",
        "type Alias = System.String\n",
        "let text = \"let identity value = value\"\n",
        "// let identity value = value\nlet answer = 1\n",
        "foreign words\nlet identity value = value\n",
        "let answer = 1\nmodule Nested =\n    let identity value = value\n",
        "let identity value =\n",
        "let identity value == value\n",
        "let mapper = fun value ->\n",
        "type Record = { Value: int\n",
        "type Choice = Value of\n",
        "let identity value = value\nlet unfinished = (\n",
    ] {
        assert!(
            admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .is_none(),
            "unproved admission for {text:?}"
        );
    }
}

#[test]
fn admission_probe_retains_direct_root_module_and_namespace_ownership() {
    for (kind, declaration) in [
        (
            FSharpAdmissionKind::NamedFunction,
            "let identity value = value",
        ),
        (
            FSharpAdmissionKind::BoundLambda,
            "let mapper = fun value -> value",
        ),
        (
            FSharpAdmissionKind::RecordType,
            "type Record = { Value: int }",
        ),
        (
            FSharpAdmissionKind::UnionType,
            "type Choice = | Empty | Value of int",
        ),
        (
            FSharpAdmissionKind::InterfaceType,
            "type I = interface\n    abstract member Read: unit -> int\nend",
        ),
        (
            FSharpAdmissionKind::ObjectType,
            "type Container(seed: int) =\n    member this.Value = seed",
        ),
    ] {
        for header in ["module Admission", "namespace Admission"] {
            for newline in ["\n", "\r\n"] {
                for ending in ["", newline] {
                    let declaration = declaration.replace('\n', newline);
                    let prefix =
                        format!("// owned header{newline}{header}{newline}let seed = 1{newline}");
                    let text = format!("{prefix}{declaration}{ending}");
                    let witness =
                        admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                            .unwrap()
                            .unwrap_or_else(|| panic!("missing {kind:?} witness for {text:?}"));
                    assert_eq!(witness.kind(), kind);
                    assert_eq!(
                        witness.declaration().span().start_byte(),
                        u64::try_from(prefix.len()).unwrap()
                    );
                    assert_eq!(
                        witness.declaration().span().end_byte(),
                        u64::try_from(prefix.len() + declaration.len()).unwrap()
                    );
                    assert_eq!(
                        witness.declaration().content_hash(),
                        content_hash(text.as_bytes())
                    );
                    assert_eq!(
                        witness.sample().span().end_byte(),
                        u64::try_from(text.len()).unwrap()
                    );
                }
            }
        }
    }
    for prefix in [
        "namespace global\n",
        "module rec Admission\n",
        "[<AutoOpen>]\nmodule Admission\n",
        "namespace First\ntype Alias = int\nnamespace Second\n",
    ] {
        let declaration = "type Record = { Value: int }";
        let text = format!("{prefix}{declaration}");
        let witness = admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
            .unwrap()
            .unwrap_or_else(|| panic!("missing root-owned witness for {text:?}"));
        assert_eq!(witness.kind(), FSharpAdmissionKind::RecordType);
        assert_eq!(
            witness.declaration().span().start_byte(),
            u64::try_from(prefix.len()).unwrap()
        );
    }
}

#[test]
fn admission_probe_retains_complete_recursive_binding_groups() {
    for (kind, group) in [
        (
            FSharpAdmissionKind::NamedFunction,
            "let rec first value = second value\nand second value = first value",
        ),
        (
            FSharpAdmissionKind::NamedFunction,
            "let rec seed = 1\nand identity value = value\nand tail = 2",
        ),
        (
            FSharpAdmissionKind::BoundLambda,
            "let rec mapper = fun value -> value\nand identity value = mapper value",
        ),
        (
            FSharpAdmissionKind::NamedFunction,
            "let rec (* first *) first (value: int) : int = second value\n(* connector *) and second (value: int) : int = first value",
        ),
    ] {
        for header in ["", "module Admission\n", "namespace Admission\n"] {
            for newline in ["\n", "\r\n"] {
                for ending in ["", newline] {
                    let header = header.replace('\n', newline);
                    let group = group.replace('\n', newline);
                    let text = format!("{header}{group}{ending}");
                    let witness =
                        admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                            .unwrap()
                            .unwrap_or_else(|| {
                                panic!("missing {kind:?} group witness for {text:?}")
                            });
                    assert_eq!(witness.kind(), kind);
                    assert_eq!(
                        witness.declaration().span().start_byte(),
                        u64::try_from(header.len()).unwrap()
                    );
                    assert_eq!(
                        witness.declaration().span().end_byte(),
                        u64::try_from(header.len() + group.len()).unwrap()
                    );
                    assert_eq!(
                        witness.declaration().content_hash(),
                        content_hash(text.as_bytes())
                    );
                    assert_eq!(
                        witness.sample().span().end_byte(),
                        u64::try_from(text.len()).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn admission_probe_keeps_container_and_group_prefix_barriers() {
    for text in [
        "module Admission\nforeign words\nlet identity value = value\n",
        "namespace Admission\nforeign words\ntype Record = { Value: int }\n",
        "namespace First\nforeign words\nnamespace Second\ntype Record = { Value: int }\n",
        "module Admission\nmodule Nested =\n    let identity value = value\n",
        "module Admission\nmodule Nested =\n    let seed = 1\nlet identity value = value\n",
        "namespace Admission\nmodule Nested =\n    let identity value = value\n",
        "foreign words\nlet rec first value = value\nand second value = value\n",
        "let rec first value = value\nand second value =\n",
        "let rec first value = value\nand = value\n",
        "let rec first value = value\nand second value == value\n",
        "let rec first value = value\nand second value = value\nand\n",
        "let rec first value = value\nand second value = value\nlet unfinished = (\n",
        "module Admission\nlet rec first value = value\nand second value = value\nlet unfinished = (\n",
        "module Admission\nlet answer = 1\n",
        "let rec first = 1\nand second = 2\n",
    ] {
        assert!(
            admission_probe_bytes(text.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .is_none(),
            "unproved admission for {text:?}"
        );
    }
}

#[test]
fn admission_probe_container_and_recursive_groups_keep_complete_line_bounds() {
    for declaration in [
        "let identity value = value",
        "let rec first value = value\nand second value = value",
    ] {
        let header = "module Admission\n";
        let padding = format!(
            "//{}\n",
            " ".repeat(8192 - header.len() - declaration.len() - 3)
        );
        let partial = format!("{header}{padding}{declaration}\n// source continues\n");
        assert!(
            admission_probe_bytes(partial.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .is_none()
        );
        let padding = format!(
            "//{}\n",
            " ".repeat(8192 - header.len() - declaration.len() - 4)
        );
        let complete = format!("{header}{padding}{declaration}\n// source continues\n");
        let witness =
            admission_probe_bytes(complete.as_bytes(), "utf-8", &limits(4096), &deadline())
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("missing complete-line container witness for {declaration:?}")
                });
        assert_eq!(witness.declaration().span().end_byte(), 8191);
        assert_eq!(witness.sample().span().end_byte(), 8192);
        assert_eq!(
            witness.sample().content_hash(),
            content_hash(complete.as_bytes())
        );
    }
}

#[test]
fn admission_probe_keeps_original_hash_and_complete_line_sample_boundary() {
    let declaration = "let identity value = value";
    let padding = format!("//{}\n", " ".repeat(8192 - declaration.len() - 3));
    let partial = format!("{padding}{declaration}\n// source continues\n");
    assert!(
        admission_probe_bytes(partial.as_bytes(), "utf-8", &limits(4096), &deadline())
            .unwrap()
            .is_none()
    );
    let declaration = format!("{declaration}\n");
    let padding = format!("//{}\n", " ".repeat(8192 - declaration.len() - 3));
    let complete = format!("{padding}{declaration}// source continues\n");
    let witness = admission_probe_bytes(complete.as_bytes(), "utf-8", &limits(4096), &deadline())
        .unwrap()
        .unwrap();
    assert_eq!(witness.sample().span().end_byte(), 8192);
    assert_eq!(witness.declaration().span().end_byte(), 8191);
    assert_eq!(
        witness.sample().content_hash(),
        content_hash(complete.as_bytes())
    );
    assert_ne!(
        witness.sample().content_hash(),
        content_hash(&complete.as_bytes()[..8192])
    );
    let late = format!("//{}\n{declaration}", " ".repeat(8192));
    assert!(
        admission_probe_bytes(late.as_bytes(), "utf-8", &limits(4096), &deadline())
            .unwrap()
            .is_none()
    );
    let prefix = "let identity value = value\n//";
    let split = format!("{prefix}{}🧭\n", " ".repeat(8191 - prefix.len()));
    let witness = admission_probe_bytes(split.as_bytes(), "utf-8", &limits(4096), &deadline())
        .unwrap()
        .unwrap();
    assert_eq!(witness.sample().span().end_byte(), 8191);
    assert_eq!(
        witness.sample().content_hash(),
        content_hash(split.as_bytes())
    );
}

#[test]
fn admission_probe_utf16_ranges_and_both_byte_caps_are_original_source_bound() {
    for (kind, prefix, declaration, following) in [
        (
            FSharpAdmissionKind::NamedFunction,
            "\u{feff}// ☃ 🧭\nlet seed = 1\n",
            "let identity value = value",
            "",
        ),
        (
            FSharpAdmissionKind::NamedFunction,
            "\u{feff}// ☃ 🧭\nmodule Admission\nlet seed = 1\n",
            "let identity value = value",
            "",
        ),
        (
            FSharpAdmissionKind::NamedFunction,
            "\u{feff}// ☃ 🧭\nnamespace Admission\n",
            "let rec first value = value\nand second value = value",
            "",
        ),
        (
            FSharpAdmissionKind::ObjectType,
            "\u{feff}// ☃ 🧭\nmodule Admission\nlet seed = 1\n",
            "type Contract =\n    abstract Read: unit -> int",
            "\nlet tail = 0",
        ),
    ] {
        for encoding in ["utf-16le", "utf-16be"] {
            let encode = |text: &str| -> Vec<u8> {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            };
            // The following declaration closes the type's offside scope before
            // the sampled partial comment; that comment cannot complete a type.
            let text = format!("{prefix}{declaration}{following}\n//{}", "中".repeat(3000));
            let raw = encode(&text);
            let witness = admission_probe_bytes(&raw, encoding, &limits(4096), &deadline())
                .unwrap()
                .unwrap_or_else(|| panic!("missing {kind:?} witness for {encoding}"));
            assert_eq!(witness.kind(), kind);
            assert_eq!(
                witness.declaration().span().start_byte(),
                u64::try_from(encode(prefix).len()).unwrap()
            );
            assert_eq!(
                witness.declaration().span().end_byte(),
                u64::try_from(encode(&format!("{prefix}{declaration}")).len()).unwrap()
            );
            assert_eq!(witness.sample().content_hash(), content_hash(&raw));
            let end = usize::try_from(witness.sample().span().end_byte()).unwrap();
            assert!(end <= 8192 && end < raw.len());
            let units: Vec<_> = raw[..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|unit| {
                    let bytes = [unit[0], unit[1]];
                    if encoding == "utf-16le" {
                        u16::from_le_bytes(bytes)
                    } else {
                        u16::from_be_bytes(bytes)
                    }
                })
                .collect();
            assert!(String::from_utf16(&units).unwrap().len() <= 8192);
            let late = encode(&format!("\u{feff}//{}\n{declaration}\n", " ".repeat(4096)));
            assert!(
                admission_probe_bytes(&late, encoding, &limits(4096), &deadline())
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn admission_probe_resource_errors_and_cancellation_are_not_negative_evidence() {
    let text = b"let identity value = value\n";
    for limits in [
        admission_probe_limits(1, 128, 16 * 1024 * 1024),
        admission_probe_limits(4096, 1, 16 * 1024 * 1024),
        admission_probe_limits(4096, 128, 1),
    ] {
        let error = admission_probe_bytes(text, "utf-8", &limits, &deadline()).unwrap_err();
        let bounded = match &error {
            rootlight_adapter_sdk::AdapterError::RejectedRequest(
                rootlight_adapter_sdk::RequestError::ProviderLimit { .. },
            ) => true,
            rootlight_adapter_sdk::AdapterError::ProviderFailed { code } => matches!(
                code.as_str(),
                "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
            ),
            _ => false,
        };
        assert!(bounded, "unexpected bounded outcome: {error:?}");
    }
    let cancellation = deadline();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        admission_probe_bytes(text, "utf-8", &limits(4096), &cancellation),
        Err(rootlight_adapter_sdk::AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let malformed = [0xff, b'\n'];
    assert!(
        matches!(admission_probe_bytes(&malformed, "utf-8", &limits(4096), &deadline()),
            Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code }) if code.as_str() == "invalid-utf8"
        )
    );
}

#[test]
fn admission_probe_rejects_other_profiles_and_included_parser_ranges() {
    let fixture = Fixture::new("fsharp", "let identity value = value\n");
    let limits = limits(4096);
    let source = GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap();
    let signature = ParseRequest::new(
        source.clone(),
        LanguageId::new("fsharp-signature").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        &limits,
    )
    .unwrap();
    assert!(matches!(
        provider().probe_fsharp_admission(&signature, &deadline()),
        Err(rootlight_adapter_sdk::AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::UnsupportedLanguage
        ))
    ));
    let included = rootlight_adapter_sdk::IncludedRange::new(
        fixture.source.span(),
        LanguageId::new("fsharp").unwrap(),
    );
    let ranged = ParseRequest::new(
        source,
        LanguageId::new("fsharp").unwrap(),
        EncodingId::utf8(),
        vec![included],
        &limits,
    )
    .unwrap();
    assert!(matches!(
        provider().probe_fsharp_admission(&ranged, &deadline()),
        Err(rootlight_adapter_sdk::AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::EmbeddedRangesUnsupported
        ))
    ));
}

#[test]
fn whole_file_context_requires_complete_native_input_not_root_extent() {
    use rootlight_adapter_sdk::IncludedRange;

    for (language, first, last, marker) in [
        (
            "fsharp",
            "let first value = value + 1",
            "let last value = value + 2",
            "fsharp.qualified_context.expression",
        ),
        (
            "fsharp-signature",
            "val first: int -> int",
            "val last: int -> int",
            "fsharp.signature_context.expression",
        ),
    ] {
        for ending in ["\n", "\r\n"] {
            let text = format!(
                " \t\n// leading trivia\nmodule Whole\n{first}\n// excluded middle\n{last}\n \t\n"
            )
            .replace('\n', ending);
            let fixture = Fixture::new(language, &text);
            let gap = "// excluded middle";
            let middle = text.find(gap).unwrap();
            let after_middle = middle + gap.len() + ending.len();
            let limits = limits(4096);
            for (ranges, complete) in [
                (Vec::new(), true),
                (std::iter::once(0..text.len()).collect::<Vec<_>>(), true),
                (std::iter::once(0..middle).collect::<Vec<_>>(), false),
                (vec![0..middle, after_middle..text.len()], false),
            ] {
                let included = ranges
                    .iter()
                    .map(|range| {
                        IncludedRange::new(
                            SourceSpan::new(
                                fixture.snapshot.file(),
                                u64::try_from(range.start).unwrap(),
                                u64::try_from(range.end).unwrap(),
                            )
                            .unwrap(),
                            fixture.language.clone(),
                        )
                    })
                    .collect();
                let request = ParseRequest::new(
                    GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
                    fixture.language.clone(),
                    EncodingId::utf8(),
                    included,
                    &limits,
                )
                .unwrap();
                let parsed = execute_parse(
                    provider().as_ref(),
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                let contexts = parsed
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == marker)
                    .collect::<Vec<_>>();
                assert_eq!(
                    contexts.len(),
                    usize::from(complete),
                    "{language}: {ranges:?}"
                );
                if complete {
                    assert_eq!(contexts[0].span(), fixture.source.span());
                }
                for fact in parsed
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == "fsharp.name.definition")
                {
                    let start = usize::try_from(fact.span().start_byte()).unwrap();
                    let end = usize::try_from(fact.span().end_byte()).unwrap();
                    assert!(
                        ranges.is_empty()
                            || ranges
                                .iter()
                                .any(|range| { range.start <= start && end <= range.end })
                    );
                }
            }
        }
    }
}

fn assert_implicit_object_lowering(
    language: &str,
    declaration: &str,
    owner_name: &str,
    owner_kind: EntityKind,
    members: &[(&str, EntityKind, &str)],
) {
    for newline in ["\n", "\r\n"] {
        let text = format!("module ObjectTypes\n{declaration}\n").replace('\n', newline);
        let fixture = Fixture::new(language, &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(parsed.diagnostics().is_empty());
        let output = fixture.analyze();
        let document = output.document();
        let unique = |name: &str, kind: EntityKind| {
            let matches: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == language
                        && entity.canonical_name == name
                        && entity.kind == kind
                })
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{language}: {name} {kind:?}; entities: {:?}; member facts: {:?}",
                document
                    .entities
                    .iter()
                    .map(|entity| (
                        entity.canonical_name.as_str(),
                        entity.kind,
                        entity.language.as_str()
                    ))
                    .take(16)
                    .collect::<Vec<_>>(),
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| { fact.syntax_kind().as_str().starts_with("fsharp.signature_") })
                    .map(|fact| (fact.syntax_kind().as_str(), fact.span()))
                    .take(16)
                    .collect::<Vec<_>>()
            );
            matches[0]
        };
        let check_source = |reference: &SourceRef, written: &str, span: SourceSpan| {
            assert_eq!(reference.repository(), fixture.source.repository());
            assert_eq!(reference.generation(), fixture.source.generation());
            assert_eq!(reference.content_hash(), fixture.source.content_hash());
            assert_eq!(reference.span(), span);
            assert_eq!(
                text.as_bytes().get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap()
                ),
                Some(written.as_bytes())
            );
        };
        let module = unique("ObjectTypes", EntityKind::Module);
        let owner = unique(owner_name, owner_kind);
        assert_eq!(owner.container, Some(ContainerRef::Entity(module.id)));
        let owner_written = declaration
            .strip_prefix("type ")
            .unwrap()
            .replace('\n', newline);
        check_source(
            owner.evidence.source.as_ref().unwrap(),
            &owner_written,
            fixture.span(&owner_written),
        );
        for (entity, parent, written) in std::iter::once((owner, module, owner_written)).chain(
            members.iter().map(|(name, kind, written)| {
                (unique(name, *kind), owner, written.replace('\n', newline))
            }),
        ) {
            assert_eq!(entity.container, Some(ContainerRef::Entity(parent.id)));
            check_source(
                entity.evidence.source.as_ref().unwrap(),
                &written,
                fixture.span(&written),
            );
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .collect();
            assert_eq!(
                definitions.len(),
                1,
                "{language}: {}",
                entity.canonical_name
            );
            assert_eq!(definitions[0].enclosing, Some(parent.id));
            let start = entity
                .evidence
                .source
                .as_ref()
                .unwrap()
                .span()
                .start_byte()
                .checked_add(u64::try_from(written.find(&entity.canonical_name).unwrap()).unwrap())
                .unwrap();
            let end = start
                .checked_add(u64::try_from(entity.canonical_name.len()).unwrap())
                .unwrap();
            let span = SourceSpan::new(fixture.snapshot.file(), start, end).unwrap();
            check_source(&definitions[0].source, &entity.canonical_name, span);
            let contains: Vec<_> = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(parent.id)
                        && relation.object == RelationEndpoint::Entity(entity.id)
                })
                .collect();
            assert_eq!(contains.len(), 1);
            check_source(
                contains[0].evidence.source.as_ref().unwrap(),
                &written,
                fixture.span(&written),
            );
        }
        assert!(!document.skipped_regions.iter().any(|gap| {
            matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
            ) || matches!(
                gap.detail.as_str(),
                "fsharp-type-representation-ambiguous" | "stable-scope-identity-unavailable"
            )
        }));
        assert!(document.relations.iter().all(|relation| matches!(
            relation.predicate,
            RelationPredicate::Contains
                | RelationPredicate::Declares
                | RelationPredicate::DefinesAt
        )));
    }
}

#[test]
fn implicit_object_implementation_members_materialize_class_owners() {
    for (declaration, name, member_name, member_kind, member_written) in [
        (
            "type Utility =\n    static member Value = 1",
            "Utility",
            "Value",
            EntityKind::Property,
            "Value = 1",
        ),
        (
            "type Holder =\n    member this.Value = 1",
            "Holder",
            "Value",
            EntityKind::Property,
            "this.Value = 1",
        ),
        (
            "type Reader =\n    member this.Read() = 1",
            "Reader",
            "Read",
            EntityKind::Method,
            "this.Read() = 1",
        ),
    ] {
        assert_implicit_object_lowering(
            "fsharp",
            declaration,
            name,
            EntityKind::Class,
            &[(member_name, member_kind, member_written)],
        );
    }
}

#[test]
fn implicit_object_abstract_contracts_materialize_interface_owners() {
    for language in ["fsharp", "fsharp-signature"] {
        for keyword in ["abstract", "abstract member"] {
            let members = [
                ("Read", EntityKind::Method, "Read: unit -> int"),
                ("Value", EntityKind::Property, "Value: int"),
                ("``abstract``", EntityKind::Property, "``abstract``: int"),
                (
                    "FunctionValue",
                    EntityKind::Property,
                    "FunctionValue: (int -> int)",
                ),
                ("Curried", EntityKind::Method, "Curried: int -> int"),
            ];
            for (tail, expected_count) in [
                (String::new(), 2),
                (
                    format!(
                        "\n    {keyword} ``abstract``: int\n    {keyword} FunctionValue: (int -> int)\n    {keyword} Curried: int -> int"
                    ),
                    5,
                ),
            ] {
                let declaration = format!(
                    "type Contract =\n    {keyword} Read: unit -> int\n    {keyword} Value: int{tail}"
                );
                assert_implicit_object_lowering(
                    language,
                    &declaration,
                    "Contract",
                    EntityKind::Interface,
                    &members[..expected_count],
                );
            }
        }
    }
}

fn assert_implicit_object_owner_gap(
    language: &str,
    text: &str,
    marker_label: &str,
    absent_names: &[&str],
) {
    let fixture = Fixture::new(language, text);
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(parsed.diagnostics().is_empty());
    let markers: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == marker_label)
        .collect();
    assert_eq!(markers.len(), 1, "{language}: {marker_label}");
    let output = fixture.analyze();
    let document = output.document();
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| { absent_names.contains(&entity.canonical_name.as_str()) })
    );
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.domain == FactDomain::Entities
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                && gap.source.span() == markers[0].span()
        })
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].source.repository(), fixture.source.repository());
    assert_eq!(gaps[0].source.generation(), fixture.source.generation());
    assert_eq!(gaps[0].source.content_hash(), fixture.source.content_hash());
    assert_eq!(gaps[0].evidence.source.as_ref(), Some(&gaps[0].source));
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls
            | RelationPredicate::DispatchCandidate
            | RelationPredicate::Implements
    )));
}

#[test]
fn implicit_object_nonabstract_signatures_remain_unclassified() {
    for keyword in ["member", "static member"] {
        let text = format!("module ObjectTypes\ntype Opaque =\n    {keyword} Read: unit -> int\n");
        assert_implicit_object_owner_gap(
            "fsharp-signature",
            &text,
            "fsharp.ambiguous_type.declaration",
            &["Opaque", "Read"],
        );
    }
}

#[test]
fn implicit_object_unproved_owners_do_not_promote_members() {
    for language in ["fsharp", "fsharp-signature"] {
        assert_implicit_object_owner_gap(
            language,
            "module ObjectTypes\ntype Alias = string\n",
            "fsharp.ambiguous_type.declaration",
            &["Alias"],
        );
        assert_implicit_object_owner_gap(
            language,
            "module ObjectTypes\n[<Opaque>]\ntype Unclassified =\n    abstract Read: unit -> int\n",
            "fsharp.ambiguous_type.declaration",
            &["Unclassified", "Read"],
        );
    }
    for (text, marker_label, names) in [
        (
            "module ObjectTypes\n[<Opaque>]\ntype Unclassified =\n    member this.Value = 1\n",
            "fsharp.ambiguous_type.declaration",
            &["Unclassified", "Value"][..],
        ),
        (
            "module ObjectTypes\ntype System.Object with\n    member this.Read() = 1\n",
            "fsharp.unsupported_type_extension.declaration",
            &["Read"][..],
        ),
        (
            "module ObjectTypes\nlet anonymous =\n    { new System.IDisposable with\n        member this.Dispose() = () }\n",
            "fsharp.unsupported_anonymous_object.declaration",
            &["Dispose"][..],
        ),
    ] {
        assert_implicit_object_owner_gap("fsharp", text, marker_label, names);
    }
}

fn signature_mask_claims(output: &AnalysisOutput) -> Vec<(SourceRef, rootlight_ir::FSharpBinding)> {
    output
        .document()
        .extensions
        .iter()
        .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
        .map(|envelope| {
            let evidence = rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap();
            (
                envelope.evidence.source.as_ref().unwrap().clone(),
                evidence.binding().clone(),
            )
        })
        .collect()
}

#[test]
fn qualified_aliases_and_scalar_enums_conserve_only_outer_type_roots() {
    use rootlight_ir::FSharpBinding;
    for (declaration, typename, marker, expected_read) in [
        (
            "type Alias = System.Int32",
            "Alias",
            "fsharp.qualified_alias_type.expression",
            true,
        ),
        (
            "type public (* head *) Alias (* tail *) = System.Int32",
            "Alias",
            "fsharp.qualified_alias_type.expression",
            true,
        ),
        (
            "type Pair = int * int",
            "Pair",
            "fsharp.qualified_alias_type.expression",
            true,
        ),
        (
            "type Callback = int -> int",
            "Callback",
            "fsharp.qualified_alias_type.expression",
            true,
        ),
        (
            "type Choice = First = 1 | Library = 2",
            "Choice",
            "fsharp.qualified_enum_type.expression",
            true,
        ),
        (
            "type public (* head *) Choice (* tail *) = First = 1 | Library = 2",
            "Choice",
            "fsharp.qualified_enum_type.expression",
            true,
        ),
        (
            "type ``Choice.part`` = First = 'a' | Library = 'b'",
            "``Choice.part``",
            "fsharp.qualified_enum_type.expression",
            true,
        ),
        (
            "type Library = System.Int32",
            "Library",
            "fsharp.qualified_alias_type.expression",
            false,
        ),
        (
            "type Library = First = 1 | Next = 2",
            "Library",
            "fsharp.qualified_enum_type.expression",
            false,
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            let text =
                format!("module Consumer\n{declaration}\nlet invoke input = Library.carry input\n")
                    .replace('\n', newline);
            let fixture = Fixture::new("fsharp", &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete,
                "{declaration}"
            );
            let witnesses = parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == marker)
                .collect::<Vec<_>>();
            assert_eq!(witnesses.len(), 1, "{declaration}");
            assert_eq!(witnesses[0].span(), fixture.span(declaration));
            let output = fixture.analyze();
            let claims = signature_mask_claims(&output);
            assert_eq!(
                claims
                    .iter()
                    .filter(|(_, claim)| matches!(claim, FSharpBinding::Context))
                    .count(),
                1,
                "{declaration}"
            );
            let containers = claims
                .iter()
                .filter_map(|(_, claim)| {
                    if let FSharpBinding::Container { roots, .. } = claim {
                        Some(roots)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(containers.len(), 1);
            let unquoted = typename
                .strip_prefix("``")
                .and_then(|name| name.strip_suffix("``"))
                .unwrap_or(typename);
            let mut roots = vec![content_hash(unquoted.as_bytes()), content_hash(b"invoke")];
            roots.sort();
            assert_eq!(*containers[0], roots, "{declaration}");
            assert_eq!(
                claims
                    .iter()
                    .filter(|(_, claim)| matches!(claim, FSharpBinding::Reference { .. }))
                    .count(),
                usize::from(expected_read),
                "{declaration}"
            );
            assert_eq!(
                claims
                    .iter()
                    .filter(|(_, claim)| matches!(claim, FSharpBinding::Call { .. }))
                    .count(),
                usize::from(expected_read),
                "{declaration}"
            );
            let exports = claims
                .iter()
                .filter_map(|(_, claim)| {
                    if let FSharpBinding::Export { symbol, .. } = claim {
                        Some(*symbol)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(exports.len(), 1);
            let invoke = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "invoke")
                .unwrap();
            assert_eq!(exports, vec![invoke.id]);
            let type_entity = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == typename)
                .unwrap();
            assert!(matches!(
                type_entity.kind,
                EntityKind::TypeAlias | EntityKind::Enum
            ));
            let definition = output
                .document()
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == (OccurrenceTarget::Resolved {
                                symbol: type_entity.id,
                            })
                })
                .unwrap();
            assert_eq!(definition.source.span(), fixture.span(typename));
            if typename == "Choice" {
                let case = output
                    .document()
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Constant && entity.canonical_name == "Library"
                    })
                    .unwrap();
                assert_eq!(case.container, Some(ContainerRef::Entity(type_entity.id)));
            }
        }
    }
}

#[test]
fn qualified_unions_and_classes_conserve_outer_roots_without_member_exports() {
    use rootlight_ir::FSharpBinding;
    for (declaration, names, expected_read) in [
        (
            "type Choice = First | Next",
            vec!["Choice", "First", "Next"],
            true,
        ),
        (
            "type Choice = First of int | Library of int",
            vec!["Choice", "First", "Library"],
            false,
        ),
        (
            "type public Choice = First | Next of Library: int",
            vec!["Choice", "First", "Next"],
            true,
        ),
        (
            "type Choice = First of ``Library|field``: int (* separator | *) | Next",
            vec!["Choice", "First", "Next"],
            true,
        ),
        (
            "type ``Choice.part`` = | ``Library.part`` of (string | null) | Next",
            vec!["Choice.part", "Library.part", "Next"],
            true,
        ),
        ("type Choice = | Library", vec!["Choice", "Library"], false),
        ("type Object = class end", vec!["Object"], true),
        (
            "type public Holder(seed: int) =\n    let hidden = seed\n    do ignore hidden\n    member this.Library = hidden",
            vec!["Holder"],
            true,
        ),
        (
            "type Library(seed: int) =\n    member this.Read() = seed",
            vec!["Library"],
            false,
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            let text =
                format!("module Consumer\n{declaration}\nlet invoke input = Library.carry input\n")
                    .replace('\n', newline);
            let fixture = Fixture::new("fsharp", &text);
            let output = fixture.analyze();
            let claims = signature_mask_claims(&output);
            assert!(
                claims
                    .iter()
                    .any(|(_, claim)| matches!(claim, FSharpBinding::Context)),
                "{declaration}"
            );
            let mut expected = names
                .iter()
                .chain(std::iter::once(&"invoke"))
                .map(|name| content_hash(name.as_bytes()))
                .collect::<Vec<_>>();
            expected.sort();
            let roots = claims
                .iter()
                .filter_map(|(_, claim)| {
                    if let FSharpBinding::Container { roots, .. } = claim {
                        Some(roots)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(roots, vec![&expected], "{declaration}");
            for call in [false, true] {
                let count = claims
                    .iter()
                    .filter(|(_, claim)| {
                        if call {
                            matches!(claim, FSharpBinding::Call { .. })
                        } else {
                            matches!(claim, FSharpBinding::Reference { .. })
                        }
                    })
                    .count();
                assert_eq!(
                    count,
                    usize::from(expected_read),
                    "{declaration}: call={call}"
                );
            }
            let exports = claims
                .iter()
                .filter_map(|(_, claim)| {
                    if let FSharpBinding::Export { symbol, .. } = claim {
                        Some(*symbol)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            let invoke = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "invoke")
                .unwrap();
            assert_eq!(exports, vec![invoke.id], "{declaration}");
            assert!(!claims.iter().any(|(_, claim)| matches!(
                claim,
                FSharpBinding::SignatureType { .. } | FSharpBinding::SignatureValue { .. }
            )));
        }
    }
}

#[test]
fn qualified_union_case_capture_loss_duplication_and_widening_cannot_hide_roots() {
    use rootlight_ir::FSharpBinding;
    let fixture = Fixture::new(
        "fsharp",
        "module Consumer\ntype Choice = First of int | Library of int\nlet invoke input = Library.carry input\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert!(
        signature_mask_claims(&full)
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    for (label, fragment) in [
        ("fsharp.union_case.signature", "Library of int"),
        ("fsharp.name.definition", "Library"),
    ] {
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| {
                    fact.syntax_kind().as_str() == label && fact.span() == fixture.span(fragment)
                })
                .unwrap();
            if duplicate {
                let original = &facts[index];
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    original.parent(),
                    original.kind(),
                    original.span(),
                    original.depth(),
                    original.syntax_kind().clone(),
                ));
            } else {
                facts.remove(index);
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert!(
                !signature_mask_claims(&output)
                    .iter()
                    .any(|(_, claim)| matches!(
                        claim,
                        FSharpBinding::Context
                            | FSharpBinding::Reference { .. }
                            | FSharpBinding::Call { .. }
                    )),
                "{label}: {duplicate}"
            );
        }
    }
    for widened in [false, true] {
        let mut facts = parsed.facts().to_vec();
        let removed = fixture.span("Library of int");
        facts.retain(|fact| {
            fact.span().start_byte() < removed.start_byte()
                || fact.span().end_byte() > removed.end_byte()
        });
        if widened {
            for fact in &mut facts {
                if fact.span() == fixture.span("First of int")
                    && matches!(
                        fact.syntax_kind().as_str(),
                        "fsharp.union_case.declaration" | "fsharp.union_case.signature"
                    )
                {
                    *fact = rootlight_adapter_sdk::SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        fixture.span("First of int | Library of int"),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    );
                }
            }
        }
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert!(
            !signature_mask_claims(&output)
                .iter()
                .any(|(_, claim)| matches!(
                    claim,
                    FSharpBinding::Context
                        | FSharpBinding::Reference { .. }
                        | FSharpBinding::Call { .. }
                )),
            "widened: {widened}"
        );
    }
}

#[test]
fn qualified_class_header_loss_duplication_and_body_widening_close_context() {
    use rootlight_ir::FSharpBinding;
    let fixture = Fixture::new(
        "fsharp",
        "module Consumer\ntype Holder(seed: int) =\n    member this.Read() = seed\nlet invoke input = Library.carry input\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert!(
        signature_mask_claims(&fixture.analyze())
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    for case in 0..3 {
        let mut facts = parsed.facts().to_vec();
        let index = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == "fsharp.class.signature")
            .unwrap();
        if case == 0 {
            facts.remove(index);
        } else {
            let original = &facts[index];
            if case == 1 {
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    original.parent(),
                    original.kind(),
                    original.span(),
                    original.depth(),
                    original.syntax_kind().clone(),
                ));
            } else {
                let end = fixture.span("member this.Read() ").end_byte();
                facts[index] = rootlight_adapter_sdk::SyntaxFact::new(
                    original.local_id(),
                    original.parent(),
                    original.kind(),
                    SourceSpan::new(fixture.snapshot.file(), original.span().start_byte(), end)
                        .unwrap(),
                    original.depth(),
                    original.syntax_kind().clone(),
                );
            }
        }
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert!(
            !signature_mask_claims(&output)
                .iter()
                .any(|(_, claim)| matches!(
                    claim,
                    FSharpBinding::Context
                        | FSharpBinding::Reference { .. }
                        | FSharpBinding::Call { .. }
                )),
            "class header case {case}"
        );
    }
}

#[test]
fn qualified_type_witness_loss_and_duplication_preserve_identity_without_complete_context() {
    use rootlight_ir::FSharpBinding;
    for (declaration, marker) in [
        (
            "type Alias = System.Int32",
            "fsharp.qualified_alias_type.expression",
        ),
        (
            "type Choice = First = 1 | Next = 2",
            "fsharp.qualified_enum_type.expression",
        ),
        (
            "type Choice = First | Next of int",
            "fsharp.qualified_union_type.expression",
        ),
        (
            "type Holder(seed: int) =\n    member this.Read() = seed",
            "fsharp.qualified_class_type.expression",
        ),
    ] {
        let text =
            format!("module Consumer\n{declaration}\nlet invoke input = Library.carry input\n");
        let fixture = Fixture::new("fsharp", &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        let full = fixture.analyze();
        assert!(
            signature_mask_claims(&full)
                .iter()
                .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
        );
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == marker)
                .unwrap();
            if duplicate {
                let fact = &facts[index];
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            } else {
                facts.remove(index);
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert_eq!(
                clipped.document().entities,
                full.document().entities,
                "{marker}: {duplicate}"
            );
            let definitions = |output: &AnalysisOutput| {
                output
                    .document()
                    .occurrences
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::Definition)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                definitions(&clipped),
                definitions(&full),
                "{marker}: {duplicate}"
            );
            assert!(
                !signature_mask_claims(&clipped)
                    .iter()
                    .any(|(_, claim)| matches!(
                        claim,
                        FSharpBinding::Context
                            | FSharpBinding::Reference { .. }
                            | FSharpBinding::Call { .. }
                    )),
                "{marker}: {duplicate}"
            );
        }
    }
}

#[test]
fn qualified_type_unreviewed_bodies_remain_closed() {
    use rootlight_ir::FSharpBinding;
    for declaration in [
        "type Alias = int",
        "[<Opaque>] type Alias = System.Int32",
        "type Alias<[<Opaque>] 'T> = System.Int32",
        "type Alias = System.Int32\nand Other = System.Int64",
        "type Alias = private System.Int32",
        "type Choice = Case = (1)",
        "type Choice = Case = 1.0",
        "type Choice = Case = \"text\"",
        "type Choice = Case = true",
        "type Choice<'T> = First | Next of 'T",
        "[<RequireQualifiedAccess>] type Choice = First | Next",
        "type Choice = [<Opaque>] First | Next",
        "type Choice = private First | Next",
        "type Choice = First | Next\nand Other = Another",
        "type Choice = First | Next with\n    member this.Read() = 1",
        "type Object<'T>(seed: 'T) =\n    member this.Read() = seed",
        "type private Object(seed: int) =\n    member this.Read() = seed",
        "[<Opaque>] type Object = class end",
        "type Object = struct end",
    ] {
        let text =
            format!("module Consumer\n{declaration}\nlet invoke input = Library.carry input\n");
        let fixture = Fixture::new("fsharp", &text);
        let output = fixture.analyze();
        assert!(
            !signature_mask_claims(&output)
                .iter()
                .any(|(_, claim)| matches!(claim, FSharpBinding::Context)),
            "{declaration}"
        );
    }
    let fixture = Fixture::new(
        "fsharp-signature",
        "module Consumer\ntype Alias = int\nval offset: int\n",
    );
    assert!(
        !signature_mask_claims(&fixture.analyze())
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
}

#[test]
fn qualified_alias_definition_tampering_cannot_omit_authored_typename_roots() {
    use rootlight_ir::FSharpBinding;
    for (declaration, relocated_name) in [
        ("type Library = System.Int32", "Int32"),
        ("type Library = First = 1 | Next = 2", "First"),
        ("type Library = First of int | Next", "First"),
        (
            "type Library(seed: int) =\n    member this.Read() = seed",
            "seed",
        ),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!("module Consumer\n{declaration}\nlet invoke input = Library.carry input\n"),
        );
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        let full = fixture.analyze();
        assert!(
            signature_mask_claims(&full)
                .iter()
                .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
        );
        for wrong_span in [
            fixture.span(relocated_name),
            SourceSpan::new(
                fixture.snapshot.file(),
                fixture.span("Library").start_byte(),
                fixture.span("Library").start_byte() + 3,
            )
            .unwrap(),
        ] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| {
                    fact.syntax_kind().as_str() == "fsharp.name.definition"
                        && fact.span() == fixture.span("Library")
                })
                .unwrap();
            let original = &facts[index];
            facts[index] = rootlight_adapter_sdk::SyntaxFact::new(
                original.local_id(),
                original.parent(),
                original.kind(),
                wrong_span,
                original.depth(),
                original.syntax_kind().clone(),
            );
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert!(
                !signature_mask_claims(&output)
                    .iter()
                    .any(|(_, claim)| matches!(
                        claim,
                        FSharpBinding::Context
                            | FSharpBinding::Reference { .. }
                            | FSharpBinding::Call { .. }
                    )),
                "tampered definition {wrong_span:?}"
            );
        }
    }
}

#[test]
fn signature_public_masks_conserve_plain_values_nested_roots_and_owned_opens() {
    use rootlight_ir::FSharpBinding;
    let text = "namespace Contracts\nmodule Library =\n    open External\n    val offset: int\n    val carry: int -> int -> int\n    val tupled: int * int -> int\n    val returned: int -> (int -> int)\n    module ``Nested.part`` =\n        val ``read.part``: int\n";
    let fixture = Fixture::new("fsharp-signature", text);
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let claims = signature_mask_claims(&output);
    assert_eq!(
        claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::Context))
            .count(),
        1
    );
    assert!(!claims.iter().any(|(_, claim)| matches!(
        claim,
        FSharpBinding::Export { .. }
            | FSharpBinding::Reference { .. }
            | FSharpBinding::Call { .. }
            | FSharpBinding::ImportedReference { .. }
            | FSharpBinding::ImportedCall { .. }
    )));
    let values = claims
        .iter()
        .filter_map(|(source, claim)| {
            if let FSharpBinding::SignatureValue { path, symbol } = claim {
                Some((source, path, symbol))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 5);
    for (name, declaration, components) in [
        (
            "offset",
            "val offset: int",
            vec!["Contracts", "Library", "offset"],
        ),
        (
            "carry",
            "val carry: int -> int -> int",
            vec!["Contracts", "Library", "carry"],
        ),
        (
            "tupled",
            "val tupled: int * int -> int",
            vec!["Contracts", "Library", "tupled"],
        ),
        (
            "returned",
            "val returned: int -> (int -> int)",
            vec!["Contracts", "Library", "returned"],
        ),
        (
            "``read.part``",
            "val ``read.part``: int",
            vec!["Contracts", "Library", "Nested.part", "read.part"],
        ),
    ] {
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Variable);
        let retained = values
            .iter()
            .filter(|(_, _, symbol)| **symbol == entity.id)
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "{name}");
        let (source, path, _) = retained[0];
        assert_eq!(source.span(), fixture.span(declaration));
        assert_eq!(source.repository(), fixture.source.repository());
        assert_eq!(source.generation(), fixture.source.generation());
        assert_eq!(source.content_hash(), fixture.source.content_hash());
        assert_eq!(
            **path,
            components
                .into_iter()
                .map(|part| content_hash(part.as_bytes()))
                .collect::<Vec<_>>()
        );
    }
    let containers = claims
        .iter()
        .filter_map(|(source, claim)| {
            if let FSharpBinding::Container { path, roots } = claim {
                Some((source.span(), path, roots))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(containers.len(), 3);
    for (components, names) in [
        (vec!["Contracts"], vec!["Library"]),
        (
            vec!["Contracts", "Library"],
            vec!["Nested.part", "carry", "offset", "returned", "tupled"],
        ),
        (
            vec!["Contracts", "Library", "Nested.part"],
            vec!["read.part"],
        ),
    ] {
        let path = components
            .into_iter()
            .map(|part| content_hash(part.as_bytes()))
            .collect::<Vec<_>>();
        let mut roots = names
            .into_iter()
            .map(|part| content_hash(part.as_bytes()))
            .collect::<Vec<_>>();
        roots.sort();
        let matching = containers
            .iter()
            .filter(|(_, actual, _)| **actual == path)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(*matching[0].2, roots);
    }
    let opens = claims
        .iter()
        .filter_map(|(source, claim)| {
            if let FSharpBinding::Open { path, owner } = claim {
                Some((source.span(), path, owner))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].0, fixture.span("open External"));
    assert_eq!(*opens[0].1, vec![content_hash(b"External")]);
    let owner = containers
        .iter()
        .find(|(_, path, _)| **path == vec![content_hash(b"Contracts"), content_hash(b"Library")])
        .unwrap()
        .0;
    assert_eq!(*opens[0].2, owner);
    let manifests = claims
        .iter()
        .filter_map(|(_, claim)| {
            if let FSharpBinding::ImportContext { containers, opens } = claim {
                Some((containers, opens))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(manifests.len(), 1);
    let mut spans = containers
        .iter()
        .map(|(span, _, _)| *span)
        .collect::<Vec<_>>();
    spans.sort();
    assert_eq!(*manifests[0].0, spans);
    assert_eq!(*manifests[0].1, vec![fixture.span("open External")]);
}

#[test]
fn signature_field_only_records_conserve_type_roots_without_value_exports() {
    use rootlight_ir::FSharpBinding;
    let text = "namespace Contracts\nmodule Library =\n    type public (* head *) Pair (* tail *) = { mutable offset: int; carry: int -> int }\n    val offset: int\n    module Nested =\n        type ``Item.part`` = { ``Value.part``: string }\n        val carry: int -> int\n";
    let fixture = Fixture::new("fsharp-signature", text);
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let claims = signature_mask_claims(&output);
    assert_eq!(
        claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::Context))
            .count(),
        1
    );
    let types = claims
        .iter()
        .filter_map(|(source, claim)| {
            if let FSharpBinding::SignatureType { path, symbol } = claim {
                Some((source, path, symbol))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(types.len(), 2);
    for (name, declaration, parts) in [
        (
            "Pair",
            "type public (* head *) Pair (* tail *) = { mutable offset: int; carry: int -> int }",
            vec!["Contracts", "Library", "Pair"],
        ),
        (
            "``Item.part``",
            "type ``Item.part`` = { ``Value.part``: string }",
            vec!["Contracts", "Library", "Nested", "Item.part"],
        ),
    ] {
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Struct);
        let matching = types
            .iter()
            .filter(|(_, _, symbol)| **symbol == entity.id)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        let (source, path, _) = matching[0];
        assert_eq!(source.span(), fixture.span(declaration));
        assert_eq!(source.repository(), fixture.source.repository());
        assert_eq!(source.generation(), fixture.source.generation());
        assert_eq!(source.content_hash(), fixture.source.content_hash());
        assert_eq!(
            **path,
            parts
                .into_iter()
                .map(|part| content_hash(part.as_bytes()))
                .collect::<Vec<_>>()
        );
    }
    for (parts, names) in [
        (vec!["Contracts"], vec!["Library"]),
        (
            vec!["Contracts", "Library"],
            vec!["Pair", "Nested", "offset"],
        ),
        (
            vec!["Contracts", "Library", "Nested"],
            vec!["Item.part", "carry"],
        ),
    ] {
        let path = parts
            .into_iter()
            .map(|part| content_hash(part.as_bytes()))
            .collect::<Vec<_>>();
        let mut expected = names
            .into_iter()
            .map(|name| content_hash(name.as_bytes()))
            .collect::<Vec<_>>();
        expected.sort();
        let roots = claims
            .iter()
            .filter_map(|(_, claim)| {
                if let FSharpBinding::Container {
                    path: actual,
                    roots,
                } = claim
                    && *actual == path
                {
                    Some(roots)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(roots, vec![&expected]);
    }
    assert_eq!(
        claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::SignatureValue { .. }))
            .count(),
        2
    );
    assert!(!claims.iter().any(|(_, claim)| matches!(
        claim,
        FSharpBinding::Export { .. }
            | FSharpBinding::Call { .. }
            | FSharpBinding::ImportedCall { .. }
    )));
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Parameter))
    );
    let pair = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Pair")
        .unwrap();
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(
                |entity| entity.container == Some(ContainerRef::Entity(pair.id))
                    && entity.kind == EntityKind::Field
            )
            .count(),
        2
    );
}

#[test]
fn signature_aliases_conserve_type_roots_without_executable_or_type_target_inference() {
    use rootlight_ir::FSharpBinding;
    let text = "namespace Contracts\nmodule Library =\n    type public (* head *) Receiver (* tail *) = System.Int32\n    type Pair = { Value: int }\n    val offset: int\n    module Nested =\n        type ``Callback.part`` = int -> int\n        val carry: int -> int\n";
    let fixture = Fixture::new("fsharp-signature", text);
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "fsharp.type_alias.scope")
    );
    let output = fixture.analyze();
    let claims = signature_mask_claims(&output);
    assert_eq!(
        claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::Context))
            .count(),
        1
    );
    let aliases = claims
        .iter()
        .filter_map(|(source, claim)| {
            if let FSharpBinding::SignatureAlias { path, symbol } = claim {
                Some((source, path, symbol))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(aliases.len(), 2);
    for (name, declaration, parts) in [
        (
            "Receiver",
            "type public (* head *) Receiver (* tail *) = System.Int32",
            vec!["Contracts", "Library", "Receiver"],
        ),
        (
            "``Callback.part``",
            "type ``Callback.part`` = int -> int",
            vec!["Contracts", "Library", "Nested", "Callback.part"],
        ),
    ] {
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::TypeAlias);
        assert_eq!(entity.language.as_str(), "fsharp-signature");
        let matching = aliases
            .iter()
            .filter(|(_, _, symbol)| **symbol == entity.id)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        let (source, path, _) = matching[0];
        assert_eq!(source.span(), fixture.span(declaration));
        let entity_source = entity.evidence.source.as_ref().unwrap();
        assert!(source.span().start_byte() < entity_source.span().start_byte());
        assert_eq!(source.span().end_byte(), entity_source.span().end_byte());
        assert_eq!(source.repository(), fixture.source.repository());
        assert_eq!(source.generation(), fixture.source.generation());
        assert_eq!(source.content_hash(), fixture.source.content_hash());
        assert_eq!(
            **path,
            parts
                .into_iter()
                .map(|part| content_hash(part.as_bytes()))
                .collect::<Vec<_>>()
        );
        assert!(!claims.iter().any(|(_, claim)| matches!(claim, FSharpBinding::SignatureType { symbol, .. } if *symbol == entity.id)));
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    && occurrence.source.span() == fixture.span(name))
        );
    }
    for (parts, names) in [
        (vec!["Contracts"], vec!["Library"]),
        (
            vec!["Contracts", "Library"],
            vec!["Receiver", "Pair", "offset", "Nested"],
        ),
        (
            vec!["Contracts", "Library", "Nested"],
            vec!["Callback.part", "carry"],
        ),
    ] {
        let path = parts
            .into_iter()
            .map(|part| content_hash(part.as_bytes()))
            .collect::<Vec<_>>();
        let mut expected = names
            .into_iter()
            .map(|name| content_hash(name.as_bytes()))
            .collect::<Vec<_>>();
        expected.sort();
        let roots = claims
            .iter()
            .filter_map(|(_, claim)| {
                if let FSharpBinding::Container {
                    path: actual,
                    roots,
                } = claim
                    && *actual == path
                {
                    Some(roots)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(roots, vec![&expected]);
    }
    assert_eq!(
        claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::SignatureType { .. }))
            .count(),
        1
    );
    assert_eq!(
        claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::SignatureValue { .. }))
            .count(),
        2
    );
    assert!(!claims.iter().any(|(_, claim)| matches!(
        claim,
        FSharpBinding::Export { .. }
            | FSharpBinding::Reference { .. }
            | FSharpBinding::Call { .. }
            | FSharpBinding::ImportedReference { .. }
            | FSharpBinding::ImportedCall { .. }
    )));
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Parameter))
    );
}

#[test]
fn signature_alias_witness_loss_duplicates_and_source_tampering_close_masks() {
    use rootlight_ir::FSharpBinding;
    let fixture = Fixture::new(
        "fsharp-signature",
        "module Contracts\ntype Library = System.Int32\nval offset: int\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert!(
        signature_mask_claims(&full)
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    for (label, span) in [
        (
            "fsharp.signature_alias_type.expression",
            fixture.span("type Library = System.Int32"),
        ),
        (
            "fsharp.type_alias.signature",
            fixture.span("Library = System.Int32"),
        ),
        ("fsharp.name.definition", fixture.span("Library")),
    ] {
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label && fact.span() == span)
                .unwrap();
            if duplicate {
                let fact = &facts[index];
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            } else {
                facts.remove(index);
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert!(
                !signature_mask_claims(&output)
                    .iter()
                    .any(|(_, claim)| matches!(
                        claim,
                        FSharpBinding::Context
                            | FSharpBinding::SignatureAlias { .. }
                            | FSharpBinding::SignatureValue { .. }
                    )),
                "{label}: {duplicate}"
            );
            if label == "fsharp.signature_alias_type.expression" {
                assert_eq!(output.document().entities, full.document().entities);
            }
        }
    }
    for case in 0..7 {
        let mut facts = parsed.facts().to_vec();
        if case == 6 {
            let whole = fixture.span("type Library = System.Int32");
            facts.retain(|fact| {
                fact.span().start_byte() < whole.start_byte()
                    || fact.span().end_byte() > whole.end_byte()
            });
        } else {
            let (label, span, wrong_span) = match case {
                0 => (
                    "fsharp.name.definition",
                    fixture.span("Library"),
                    fixture.span("Int32"),
                ),
                1 => (
                    "fsharp.name.definition",
                    fixture.span("Library"),
                    fixture.span("Lib"),
                ),
                2 => (
                    "fsharp.signature_alias_type.expression",
                    fixture.span("type Library = System.Int32"),
                    fixture.span("Library = System.Int32"),
                ),
                3 => (
                    "fsharp.type_alias.signature",
                    fixture.span("Library = System.Int32"),
                    fixture.span("Library"),
                ),
                _ => (
                    "fsharp.signature_alias_type.expression",
                    fixture.span("type Library = System.Int32"),
                    fixture.span("type Library = System.Int32"),
                ),
            };
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label && fact.span() == span)
                .unwrap();
            let original = &facts[index];
            let parent = if case == 4 {
                parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == "fsharp.type_alias.declaration")
                    .map(|fact| fact.local_id())
            } else if case == 5 {
                parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == "fsharp.file.scope")
                    .map(|fact| fact.local_id())
            } else {
                original.parent()
            };
            facts[index] = rootlight_adapter_sdk::SyntaxFact::new(
                original.local_id(),
                parent,
                original.kind(),
                wrong_span,
                original.depth(),
                original.syntax_kind().clone(),
            );
        }
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        if case == 4 {
            // A full wrapper cannot be contained by its inner alias declaration.
            assert!(
                matches!(fixture.try_analyze_with_parser(Arc::new(parser), IrLimits::default()), Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-lowering-parent")
            );
            continue;
        }
        let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert!(
            !signature_mask_claims(&output)
                .iter()
                .any(|(_, claim)| matches!(
                    claim,
                    FSharpBinding::Context
                        | FSharpBinding::SignatureAlias { .. }
                        | FSharpBinding::SignatureValue { .. }
                )),
            "tampered alias case {case}"
        );
    }
    let collision = Fixture::new(
        "fsharp-signature",
        "module Contracts\ntype offset = System.Int32\nval offset: int\n",
    );
    let output = collision.analyze();
    assert!(output.document().entities.iter().any(|entity| entity.kind == EntityKind::TypeAlias && entity.canonical_name == "offset"));
    assert!(
        !signature_mask_claims(&output)
            .iter()
            .any(|(_, claim)| matches!(
                claim,
                FSharpBinding::Context | FSharpBinding::SignatureAlias { .. }
            ))
    );
    let mut limits = IrLimits::default();
    limits.max_extensions = full.document().extensions.len() - signature_mask_claims(&full).len();
    let clipped = fixture.analyze_with_ir_limits(limits);
    assert!(
        !signature_mask_claims(&clipped)
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    assert!(clipped.document().skipped_regions.iter().any(|gap| {
        gap.reason == SkippedRegionReason::ResourceLimit
            && gap.detail == "fsharp-project-evidence-resource-limit"
    }));
    assert!(clipped.document().entities.iter().any(|entity| {
        entity.kind == EntityKind::TypeAlias && entity.canonical_name == "Library"
    }));
}

#[test]
fn signature_record_capture_loss_duplicates_and_source_tampering_close_masks() {
    use rootlight_ir::FSharpBinding;
    let fixture = Fixture::new(
        "fsharp-signature",
        "module Contracts\ntype Library = { Value: int }\nval offset: int\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert!(
        signature_mask_claims(&full)
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    for (label, span) in [
        (
            "fsharp.signature_record_type.expression",
            fixture.span("type Library = { Value: int }"),
        ),
        (
            "fsharp.record.scope",
            fixture.span("Library = { Value: int }"),
        ),
        ("fsharp.record.signature", fixture.span("Library ")),
        ("fsharp.name.definition", fixture.span("Library")),
    ] {
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label && fact.span() == span)
                .unwrap();
            if duplicate {
                let fact = &facts[index];
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            } else {
                facts.remove(index);
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            if label == "fsharp.record.scope" && !duplicate {
                // The scope is mandatory ancestry, unlike the optional mask marker.
                assert!(
                    matches!(fixture.try_analyze_with_parser(Arc::new(parser), IrLimits::default()), Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-lowering-parent")
                );
                continue;
            }
            let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert!(
                !signature_mask_claims(&output)
                    .iter()
                    .any(|(_, claim)| matches!(
                        claim,
                        FSharpBinding::Context
                            | FSharpBinding::SignatureType { .. }
                            | FSharpBinding::SignatureValue { .. }
                    )),
                "{label}: {duplicate}"
            );
            if label == "fsharp.signature_record_type.expression" {
                assert_eq!(output.document().entities, full.document().entities);
            }
        }
    }
    for case in 0..7 {
        let mut facts = parsed.facts().to_vec();
        if case == 6 {
            let whole = fixture.span("type Library = { Value: int }");
            facts.retain(|fact| {
                fact.span().start_byte() < whole.start_byte()
                    || fact.span().end_byte() > whole.end_byte()
            });
        } else {
            let label = if case < 2 {
                "fsharp.name.definition"
            } else {
                "fsharp.signature_record_type.expression"
            };
            let span = if case < 2 {
                fixture.span("Library")
            } else {
                fixture.span("type Library = { Value: int }")
            };
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label && fact.span() == span)
                .unwrap();
            let original = &facts[index];
            let wrong_span = match case {
                0 => fixture.span("Value"),
                1 => fixture.span("Lib"),
                2 => fixture.span("Library = { Value: int }"),
                3 => SourceSpan::new(
                    fixture.snapshot.file(),
                    span.start_byte(),
                    span.end_byte() - 1,
                )
                .unwrap(),
                _ => span,
            };
            let parent = if case == 4 {
                parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == "fsharp.record.scope")
                    .map(|fact| fact.local_id())
            } else if case == 5 {
                parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == "fsharp.file.scope")
                    .map(|fact| fact.local_id())
            } else {
                original.parent()
            };
            facts[index] = rootlight_adapter_sdk::SyntaxFact::new(
                original.local_id(),
                parent,
                original.kind(),
                wrong_span,
                original.depth(),
                original.syntax_kind().clone(),
            );
        }
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        if case == 4 {
            // The whole wrapper cannot fit inside the inner record's scope.
            assert!(
                matches!(fixture.try_analyze_with_parser(Arc::new(parser), IrLimits::default()), Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-lowering-parent")
            );
            continue;
        }
        let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert!(
            !signature_mask_claims(&output)
                .iter()
                .any(|(_, claim)| matches!(
                    claim,
                    FSharpBinding::Context
                        | FSharpBinding::SignatureType { .. }
                        | FSharpBinding::SignatureValue { .. }
                )),
            "tampered record case {case}"
        );
    }
    let mut limits = IrLimits::default();
    limits.max_extensions = full.document().extensions.len() - 1;
    assert!(
        !signature_mask_claims(&fixture.analyze_with_ir_limits(limits))
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
}

#[test]
fn signature_unreviewed_shapes_close_masks_without_losing_structural_declarations() {
    use rootlight_ir::FSharpBinding;
    for declaration in [
        "[<Opaque>] val hidden: int",
        "val private hidden: int",
        "val internal hidden: int",
        "val mutable hidden: int",
        "val inline hidden: int",
        "val hidden<'T>: 'T -> 'T",
        "val hidden: int = 1",
        "[<Opaque>] type Record = { Value: int }",
        "type Record = { [<Opaque>] Value: int }",
        "type Record<'T> = { Value: 'T }",
        "type private Record = { Value: int }",
        "type internal Record = { Value: int }",
        "type Record = private { Value: int }",
        "type Record = { Value: int } with\n    member Read: unit -> int",
        "type Record = { Value: int }\nand Other = { Value: int }",
        "type Record = int",
        "type Alias<'T> = System.Int32",
        "type private Alias = System.Int32",
        "type internal Alias = System.Int32",
        "[<Opaque>] type Alias = System.Int32",
        "type Alias = private System.Int32",
        "type Alias = System.Int32\nand Other = System.Int64",
        "type Record = First | Next",
        "type Record = class end",
        "type publicValue = { Value: int }",
        "module Alias = External",
        "module private Hidden =\n    val value: int",
        "#if ACTIVE\nval hidden: int\n#endif",
    ] {
        let text = format!("module Contracts\nval publicValue: int\n{declaration}\n");
        let fixture = Fixture::new("fsharp-signature", &text);
        let output = fixture.analyze();
        let claims = signature_mask_claims(&output);
        assert!(
            !claims.iter().any(|(_, claim)| matches!(
                claim,
                FSharpBinding::Context
                    | FSharpBinding::SignatureValue { .. }
                    | FSharpBinding::SignatureType { .. }
                    | FSharpBinding::SignatureAlias { .. }
            )),
            "{declaration}"
        );
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "publicValue"),
            "{declaration}"
        );
        assert!(
            output.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Extensions
                    && gap.detail == "fsharp-signature-mask-unavailable"
            }),
            "{declaration}"
        );
    }
}

#[test]
fn signature_capture_loss_duplicates_and_extension_limits_never_publish_complete_masks() {
    use rootlight_ir::FSharpBinding;
    let fixture = Fixture::new(
        "fsharp-signature",
        "module Contracts\nopen External\nval offset: int\nval carry: int -> int\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert!(
        signature_mask_claims(&full)
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    for label in [
        "fsharp.signature_context.expression",
        "fsharp.module.signature",
        "fsharp.signature_val.signature",
        "fsharp.name.definition",
        "fsharp.open.import",
    ] {
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label)
                .unwrap();
            if duplicate {
                let fact = &facts[index];
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            } else {
                facts.remove(index);
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert!(
                !signature_mask_claims(&output)
                    .iter()
                    .any(|(_, claim)| matches!(claim, FSharpBinding::Context)),
                "{label}: {duplicate}"
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "fsharp-signature-mask-unavailable")
            );
        }
    }
    let mut limits = IrLimits::default();
    limits.max_extensions = full.document().extensions.len() - 1;
    let clipped = fixture.analyze_with_ir_limits(limits);
    assert!(
        !signature_mask_claims(&clipped)
            .iter()
            .any(|(_, claim)| matches!(claim, FSharpBinding::Context))
    );
    assert!(
        clipped
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ResourceLimit
                && gap.detail == "fsharp-project-evidence-resource-limit")
    );
}

#[test]
fn signature_mask_sources_and_embedded_manifests_use_original_encoded_coordinates() {
    use rootlight_ir::FSharpBinding;
    let text = "// neutral λ\r\nmodule Contracts\r\nopen External\r\ntype ``Item.part`` = { mutable ``Value.part``: int }\r\ntype ``Alias.part`` = System.Int32\r\nval ``value.part``: int\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut bytes = match encoding {
            "utf-8" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            "utf-16be" => vec![0xfe, 0xff],
            _ => unreachable!(),
        };
        if encoding == "utf-8" {
            bytes.extend_from_slice(text.as_bytes());
        } else {
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(directory.path().join("Sample.fsi"), &bytes).unwrap();
        let repository = derive_repository(b"fsharp-signature-encoded-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("Sample.fsi")).unwrap(),
                16 * 1024,
            )
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([17; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let limits = limits(4096);
        let analyzer = TreeSitterAnalyzer::new(
            provider(),
            rootlight_ir::ProducerIdentity::new(
                "fsharp-signature-encoded-contract",
                "1.0",
                content_hash(b"fsharp-signature-encoded-configuration"),
            )
            .unwrap(),
            LanguageId::new("fsharp-signature").unwrap(),
            "tree-sitter-fsharp-0.3.12",
            content_hash(b"fsharp-signature-encoded-binary"),
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("fsharp-signature").unwrap(),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"fsharp-signature-encoded-build")),
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
        let claims = signature_mask_claims(&output);
        let physical = |fragment: &str| {
            let start = text.find(fragment).unwrap();
            let (start, end) = if encoding == "utf-8" {
                (3 + start, 3 + start + fragment.len())
            } else {
                let start = 2 + 2 * text[..start].encode_utf16().count();
                (start, start + 2 * fragment.encode_utf16().count())
            };
            SourceSpan::new(
                snapshot.file(),
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap()
        };
        let values = claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::SignatureValue { .. }))
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 1, "{encoding}");
        assert_eq!(values[0].0.span(), physical("val ``value.part``: int"));
        let types = claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::SignatureType { .. }))
            .collect::<Vec<_>>();
        assert_eq!(types.len(), 1, "{encoding}");
        assert_eq!(
            types[0].0.span(),
            physical("type ``Item.part`` = { mutable ``Value.part``: int }")
        );
        let aliases = claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::SignatureAlias { .. }))
            .collect::<Vec<_>>();
        assert_eq!(aliases.len(), 1, "{encoding}");
        assert_eq!(
            aliases[0].0.span(),
            physical("type ``Alias.part`` = System.Int32")
        );
        let containers = claims
            .iter()
            .filter_map(|(source, claim)| {
                if matches!(claim, FSharpBinding::Container { .. }) {
                    Some(source.span())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(containers.len(), 1);
        assert_eq!(
            containers[0].start_byte(),
            physical("module Contracts").start_byte()
        );
        assert_eq!(
            containers[0].end_byte(),
            physical("val ``value.part``: int").end_byte()
        );
        let open = claims
            .iter()
            .filter_map(|(source, claim)| {
                if let FSharpBinding::Open { owner, .. } = claim {
                    Some((source.span(), *owner))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(open, vec![(physical("open External"), containers[0])]);
        let manifests = claims
            .iter()
            .filter_map(|(source, claim)| {
                if let FSharpBinding::ImportContext { containers, opens } = claim {
                    Some((source, containers, opens))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].0.span(), source.span());
        assert_eq!(*manifests[0].1, containers);
        assert_eq!(*manifests[0].2, vec![physical("open External")]);
        let contexts = claims
            .iter()
            .filter(|(_, claim)| matches!(claim, FSharpBinding::Context))
            .collect::<Vec<_>>();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].0, source);
        for (claim_source, _) in &claims {
            assert_eq!(claim_source.repository(), source.repository());
            assert_eq!(claim_source.generation(), source.generation());
            assert_eq!(claim_source.content_hash(), source.content_hash());
        }
    }
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
        assert!(!labels.iter().any(|label| label.ends_with(".call")));
        let reads = labels
            .iter()
            .filter(|label| label.ends_with(".reference"))
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), if language == "fsharp" { 7 } else { 0 });
        assert!(
            reads
                .iter()
                .all(|label| **label == "fsharp.simple_function_body_read.reference")
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
                | RelationPredicate::RefersTo
        )));
        let references = document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .collect::<Vec<_>>();
        assert_eq!(references.len(), if language == "fsharp" { 7 } else { 0 });
        if language == "fsharp" {
            assert_eq!(
                references
                    .iter()
                    .filter(|relation| document
                        .entities
                        .iter()
                        .any(|entity| entity.kind == EntityKind::Parameter
                            && relation.object == RelationEndpoint::Entity(entity.id)))
                    .count(),
                6
            );
            assert_exact_lexical_reference(
                &fixture,
                &output,
                "``value with spaces``",
                "value",
                None,
                "let ``value with spaces`` = value",
                "value",
            );
        }
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

// Native measure syntax must preserve written declaration identity and source evidence.
// These cases use the existing F# adapter Fixture; they do not resolve unit names or types.

#[test]
fn reciprocal_aliases_preserve_exact_written_declaration_evidence() {
    for language in ["fsharp", "fsharp-signature"] {
        let following = if language == "fsharp-signature" {
            "val sentinel: int"
        } else {
            "let sentinel = 73"
        };
        for value in ["/ duration", "/duration"] {
            let alias = format!("rate = {value}");
            let canonical =
                format!("module ReciprocalExample\n[<Measure>]\ntype {alias}\n{following}");
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
                        "{language}: {value}; diagnostics={:?}",
                        parsed.diagnostics()
                    );
                    assert!(parsed.diagnostics().is_empty());
                    for label in [
                        "fsharp.type_alias.declaration",
                        "fsharp.type_alias.signature",
                    ] {
                        assert_eq!(
                            parsed
                                .facts()
                                .iter()
                                .filter(|fact| {
                                    fact.syntax_kind().as_str() == label
                                        && fact.span() == fixture.span(&alias)
                                })
                                .count(),
                            1,
                            "{language}: {label}"
                        );
                    }
                    assert_eq!(
                        parsed
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == "fsharp.name.definition"
                                    && fact.span() == fixture.span("rate")
                            })
                            .count(),
                        1
                    );
                    let output = fixture.analyze();
                    let document = output.document();
                    let aliases: Vec<_> = document
                        .entities
                        .iter()
                        .filter(|entity| {
                            entity.canonical_name == "rate" && entity.kind == EntityKind::TypeAlias
                        })
                        .collect();
                    assert_eq!(aliases.len(), 1);
                    let entity = aliases[0];
                    let reference = entity.evidence.source.as_ref().unwrap();
                    assert_eq!(reference.span(), fixture.span(&alias));
                    assert_eq!(reference.repository(), fixture.source.repository());
                    assert_eq!(reference.generation(), fixture.source.generation());
                    assert_eq!(reference.content_hash(), fixture.source.content_hash());
                    let definitions: Vec<_> = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::Definition
                                && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                        })
                        .collect();
                    assert_eq!(definitions.len(), 1);
                    assert_eq!(definitions[0].source.span(), fixture.span("rate"));
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
fn measured_attribute_constants_preserve_exact_value_contract_evidence() {
    for language in ["fsharp", "fsharp-signature"] {
        let attribute = "[<Witness(17.25</duration>)>]";
        let (binding, following, declaration_label, signature_label) =
            if language == "fsharp-signature" {
                (
                    "val sample: float",
                    "val sentinel: int",
                    "fsharp.signature_val.declaration",
                    "fsharp.signature_val.signature",
                )
            } else {
                (
                    "let sample = 17.25",
                    "let sentinel = 73",
                    "fsharp.variable.declaration",
                    "fsharp.variable.signature",
                )
            };
        let canonical = format!("module ReciprocalExample\n{attribute}\n{binding}\n{following}");
        for newline in ["\n", "\r\n"] {
            for trailing_newline in [false, true] {
                let mut text = canonical.replace('\n', newline);
                if trailing_newline {
                    text.push_str(newline);
                }
                let fixture = Fixture::new(language, &text);
                let attributed = format!("{attribute}{newline}{binding}");
                // Implementation heads exclude enclosing attributes; signature vals own them.
                let (declaration, header) = if language == "fsharp-signature" {
                    (attributed.as_str(), attributed.as_str())
                } else {
                    ("sample = 17.25", "sample ")
                };
                let parsed = fixture.parse(4096, &deadline()).unwrap();
                assert_eq!(
                    parsed.report().coverage().status(),
                    CoverageStatus::Complete,
                    "{language}: diagnostics={:?}",
                    parsed.diagnostics()
                );
                assert!(parsed.diagnostics().is_empty());
                for (label, written) in [
                    (declaration_label, declaration),
                    (signature_label, header),
                    ("fsharp.name.definition", "sample"),
                ] {
                    assert_eq!(
                        parsed
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == label
                                    && fact.span() == fixture.span(written)
                            })
                            .count(),
                        1,
                        "{language}: {label}"
                    );
                }
                assert!(parsed.facts().iter().all(|fact| {
                    !fact.syntax_kind().as_str().ends_with(".reference")
                        && !fact.syntax_kind().as_str().ends_with(".call")
                }));
                let output = fixture.analyze();
                let document = output.document();
                let values: Vec<_> = document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity.canonical_name == "sample" && entity.kind == EntityKind::Variable
                    })
                    .collect();
                assert_eq!(values.len(), 1);
                let entity = values[0];
                let reference = entity.evidence.source.as_ref().unwrap();
                assert_eq!(reference.span(), fixture.span(declaration));
                assert_eq!(reference.repository(), fixture.source.repository());
                assert_eq!(reference.generation(), fixture.source.generation());
                assert_eq!(reference.content_hash(), fixture.source.content_hash());
                let definitions: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    })
                    .collect();
                assert_eq!(definitions.len(), 1);
                assert_eq!(definitions[0].source.span(), fixture.span("sample"));
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

#[test]
fn conditional_namespace_headers_preserve_own_formals_without_branch_inference() {
    let canonical = "// ☃ 🧭\n#if FEATURE\nnamespace Variant.Primary\n#else\nnamespace Variant.Secondary\n#endif\nmodule Helpers =\n    type Entry = { Count: int }\n    let seed = 1\n    let echo (value: int) = value";
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            let mut source = canonical.replace('\n', newline);
            if trailing_newline {
                source.push_str(newline);
            }
            let fixture = Fixture::new("fsharp", &source);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(parsed.diagnostics().is_empty());
            let output = fixture.analyze();
            assert_conditional_namespace_evidence(&fixture, &parsed, &output, newline);
            assert_exact_lexical_reference(
                &fixture,
                &output,
                "echo",
                "value",
                Some("echo"),
                "= value",
                "value",
            );
            let document = output.document();
            for (name, kind, written) in [
                ("echo", EntityKind::Function, "echo (value: int) = value"),
                ("value", EntityKind::Parameter, "value"),
            ] {
                let entities = document
                    .entities
                    .iter()
                    .filter(|entity| entity.kind == kind && entity.canonical_name == name)
                    .collect::<Vec<_>>();
                assert_eq!(entities.len(), 1);
                let entity = entities[0];
                let source = entity.evidence.source.as_ref().unwrap();
                assert_eq!(source.span(), fixture.span(written));
                let definitions = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(definitions.len(), 1);
                assert_eq!(definitions[0].source.span(), fixture.span(name));
                for reference in [source, &definitions[0].source] {
                    assert_eq!(reference.repository(), fixture.source.repository());
                    assert_eq!(reference.generation(), fixture.source.generation());
                    assert_eq!(reference.content_hash(), fixture.source.content_hash());
                }
            }
            assert_eq!(
                document
                    .occurrences
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::Reference)
                    .count(),
                1
            );
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
                    .count(),
                1
            );
            assert!(!document.relations.iter().any(|edge| {
                matches!(
                    edge.predicate,
                    RelationPredicate::Calls | RelationPredicate::DispatchCandidate
                )
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.reason == SkippedRegionReason::ParseError
                    || (gap.detail == "fsharp-formal-binding-target-unavailable"
                        && gap.source.span() == lexical_site_span(&fixture, "= value", "value"))
            }));
            let malformed = source.replace(
                "namespace Variant.Secondary",
                "namespace Variant..Secondary",
            );
            let fixture = Fixture::new("fsharp", &malformed);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
            let recovery = parsed
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
                .collect::<Vec<_>>();
            assert_eq!(recovery.len(), 1);
            assert_eq!(recovery[0].coverage_effect(), CoverageStatus::Unknown);
            let output = fixture.analyze();
            let document = output.document();
            let read_span = lexical_site_span(&fixture, "= value", "value");
            let reads = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference && site.source.span() == read_span
                })
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1);
            assert_eq!(reads[0].syntactic_text_hash, content_hash(b"value"));
            assert_eq!(
                reads[0].target,
                OccurrenceTarget::Unresolved {
                    text_hash: content_hash(b"value")
                }
            );
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
            }));
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.source == reads[0].source
                            && gap.domain == FactDomain::Relations
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.detail == "fsharp-formal-binding-target-unavailable"
                    })
                    .count(),
                1
            );
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.reason == SkippedRegionReason::ParseError
                    && gap.detail == "syntax-error-recovery"
                    && Some(&gap.source) == recovery[0].source()
            }));
        }
    }
}

fn assert_conditional_namespace_evidence(
    fixture: &Fixture,
    parsed: &ParseOutput,
    output: &AnalysisOutput,
    newline: &str,
) {
    for name in ["Variant.Primary", "Variant.Secondary"] {
        let header = format!("namespace {name}");
        for (kind, label, span) in [
            (
                SyntaxFactKind::Signature,
                "fsharp.namespace.signature",
                fixture.span(&header),
            ),
            (
                SyntaxFactKind::Occurrence,
                "fsharp.name.definition",
                fixture.span(name),
            ),
        ] {
            assert_eq!(
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.kind() == kind
                            && fact.syntax_kind().as_str() == label
                            && fact.span() == span
                    })
                    .count(),
                1,
                "{name}: {label}"
            );
        }
    }
    let document = output.document();
    for (written, label) in [
        (
            "#if FEATURE\nnamespace Variant.Primary\n#else\nnamespace Variant.Secondary\n#endif",
            "fsharp.preprocessor.scope",
        ),
        (
            "#else\nnamespace Variant.Secondary",
            "fsharp.preprocessor_branch.scope",
        ),
    ] {
        let written = written.replace('\n', newline);
        let span = fixture.span(&written);
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Scope
                        && fact.syntax_kind().as_str() == label
                        && fact.span() == span
                })
                .count(),
            1
        );
        let reference = SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            span,
            fixture.source.content_hash(),
            None,
        );
        for (domain, detail) in [
            (
                FactDomain::Entities,
                "fsharp-conditional-namespace-owner-unavailable",
            ),
            (
                FactDomain::Relations,
                "fsharp-conditional-compilation-evaluation-unavailable",
            ),
        ] {
            let gaps = document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.source == reference
                        && gap.domain == domain
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.detail == detail
                })
                .collect::<Vec<_>>();
            assert_eq!(gaps.len(), 1);
            assert_eq!(gaps[0].evidence.source.as_ref(), Some(&reference));
        }
    }
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
}

#[test]
fn conditional_namespace_signature_headers_preserve_following_val_identity() {
    let canonical = "// ☃ 🧭\n#if FEATURE\nnamespace Variant.Primary\n#else\nnamespace Variant.Secondary\n#endif\nmodule Helpers =\n    val sentinel: int";
    for newline in ["\n", "\r\n"] {
        for trailing_newline in [false, true] {
            let mut source = canonical.replace('\n', newline);
            if trailing_newline {
                source.push_str(newline);
            }
            let fixture = Fixture::new("fsharp-signature", &source);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(parsed.diagnostics().is_empty());
            assert_eq!(
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.kind() == SyntaxFactKind::Declaration
                            && fact.syntax_kind().as_str() == "fsharp.signature_val.declaration"
                            && fact.span() == fixture.span("val sentinel: int")
                    })
                    .count(),
                1
            );
            let output = fixture.analyze();
            assert_conditional_namespace_evidence(&fixture, &parsed, &output, newline);
            let document = output.document();
            let entities = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "sentinel"
                })
                .collect::<Vec<_>>();
            assert_eq!(entities.len(), 1);
            let entity = entities[0];
            let reference = entity.evidence.source.as_ref().unwrap();
            assert_eq!(reference.span(), fixture.span("val sentinel: int"));
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            assert_eq!(definitions[0].source.span(), fixture.span("sentinel"));
            for reference in [reference, &definitions[0].source] {
                assert_eq!(reference.repository(), fixture.source.repository());
                assert_eq!(reference.generation(), fixture.source.generation());
                assert_eq!(reference.content_hash(), fixture.source.content_hash());
            }
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| { gap.reason == SkippedRegionReason::ParseError })
            );
            assert!(!document.relations.iter().any(|edge| {
                matches!(
                    edge.predicate,
                    RelationPredicate::RefersTo
                        | RelationPredicate::Calls
                        | RelationPredicate::DispatchCandidate
                )
            }));
        }
    }
}

#[test]
fn conditional_bindings_close_prior_homonyms_but_preserve_own_formals() {
    let fixture = Fixture::new(
        "fsharp",
        "module Branches\nlet chosen original = original\nlet before input = later input\n#if FEATURE\nlet chosen first = first\nlet later next = next\n#else\nlet chosen second = second\n#endif\nlet after input = chosen input\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(parsed.diagnostics().is_empty());
    let output = fixture.analyze();
    let document = output.document();
    for (written, name) in [
        ("chosen original = original", "original"),
        ("before input = later input", "input"),
        ("chosen first = first", "first"),
        ("later next = next", "next"),
        ("chosen second = second", "second"),
        ("after input = chosen input", "input"),
    ] {
        let header = written.split_once(" = ").unwrap().0;
        let start = fixture.text.find(written).unwrap() + header.rfind(name).unwrap();
        let definition_span = SourceSpan::new(
            fixture.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + name.len()).unwrap(),
        )
        .unwrap();
        let parameters = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span() == definition_span)
            })
            .collect::<Vec<_>>();
        assert_eq!(parameters.len(), 1, "{written}");
        let parameter = parameters[0];
        let Some(ContainerRef::Entity(owner)) = parameter.container else {
            panic!("formal has no authored function owner: {written}");
        };
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].source.span(), definition_span);
        let read_span = lexical_site_span(&fixture, written, name);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == read_span
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let read = reads[0];
        assert_eq!(read.enclosing, Some(owner));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter.id));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
        for reference in [&definitions[0].source, &read.source] {
            assert_eq!(reference.repository(), fixture.source.repository());
            assert_eq!(reference.generation(), fixture.source.generation());
            assert_eq!(reference.content_hash(), fixture.source.content_hash());
        }
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.source == read.source && gap.detail == "fsharp-formal-binding-target-unavailable"
        }));
    }
    for (written, name) in [
        ("before input = later input", "later"),
        ("after input = chosen input", "chosen"),
    ] {
        let span = lexical_site_span(&fixture, written, name);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let read = reads[0];
        assert_eq!(
            read.target,
            OccurrenceTarget::Unresolved {
                text_hash: content_hash(name.as_bytes())
            }
        );
        assert_eq!(read.syntactic_text_hash, content_hash(name.as_bytes()));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(read.id)
        }));
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.source == read.source
                        && gap.domain == FactDomain::Relations
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.detail == "fsharp-formal-binding-target-unavailable"
                })
                .count(),
            1
        );
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        8
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        6
    );
    assert!(!document.relations.iter().any(|edge| {
        matches!(
            edge.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )
    }));
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
}

#[test]
fn malformed_reciprocal_alias_preserves_recovery_and_following_value_identity() {
    for language in ["fsharp", "fsharp-signature"] {
        let (following, declaration, declaration_label) = if language == "fsharp-signature" {
            (
                "val sentinel: int",
                "val sentinel: int",
                "fsharp.signature_val.declaration",
            )
        } else {
            (
                "let sentinel = 73",
                "sentinel = 73",
                "fsharp.variable.declaration",
            )
        };
        let canonical =
            format!("module ReciprocalExample\n[<Measure>]\ntype rate = / duration /\n{following}");
        for newline in ["\n", "\r\n"] {
            for trailing_newline in [false, true] {
                let mut text = canonical.replace('\n', newline);
                if trailing_newline {
                    text.push_str(newline);
                }
                let fixture = Fixture::new(language, &text);
                let parsed = fixture.parse(4096, &deadline()).unwrap();
                assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
                let recovery: Vec<_> = parsed
                    .diagnostics()
                    .iter()
                    .filter(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
                    .collect();
                assert_eq!(recovery.len(), 1);
                let diagnostic = recovery[0];
                assert_eq!(diagnostic.coverage_effect(), CoverageStatus::Unknown);
                let recovery_source = diagnostic.source().unwrap();
                assert_eq!(recovery_source.repository(), fixture.source.repository());
                assert_eq!(recovery_source.generation(), fixture.source.generation());
                assert_eq!(
                    recovery_source.content_hash(),
                    fixture.source.content_hash()
                );
                assert!(recovery_source.span().start_byte() < recovery_source.span().end_byte());
                assert!(recovery_source.span().end_byte() <= fixture.span(following).start_byte());
                for (label, written, kind) in [
                    (declaration_label, declaration, SyntaxFactKind::Declaration),
                    (
                        "fsharp.name.definition",
                        "sentinel",
                        SyntaxFactKind::Occurrence,
                    ),
                ] {
                    assert_eq!(
                        parsed
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.kind() == kind
                                    && fact.syntax_kind().as_str() == label
                                    && fact.span() == fixture.span(written)
                            })
                            .count(),
                        1,
                        "{language}: {label}"
                    );
                }
                let output = fixture.analyze();
                assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
                let document = output.document();
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.reason == SkippedRegionReason::ParseError
                        && gap.detail == "syntax-error-recovery"
                        && gap.source == *recovery_source
                }));
                let values: Vec<_> = document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity.canonical_name == "sentinel" && entity.kind == EntityKind::Variable
                    })
                    .collect();
                assert_eq!(values.len(), 1);
                let entity = values[0];
                let reference = entity.evidence.source.as_ref().unwrap();
                assert_eq!(reference.span(), fixture.span(declaration));
                assert_eq!(reference.repository(), fixture.source.repository());
                assert_eq!(reference.generation(), fixture.source.generation());
                assert_eq!(reference.content_hash(), fixture.source.content_hash());
                let definitions: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    })
                    .collect();
                assert_eq!(definitions.len(), 1);
                assert_eq!(definitions[0].source.span(), fixture.span("sentinel"));
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
fn callable_signatures_keep_headers_separate_from_formal_proofs() {
    for (parameters, body, names) in [
        ("value", "value", &[("value", "value")][..]),
        ("(value)", "value", &[("value", "(value)")][..]),
        ("(value: int)", "value", &[("value", "(value: int)")][..]),
        (
            "left (right: int)",
            "left + right",
            &[("left", "left"), ("right", "(right: int)")][..],
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            let header = format!("carry {parameters} ");
            let text = format!("module Signature{newline}let {header}= {body}{newline}");
            let fixture = Fixture::new("fsharp", &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(parsed.diagnostics().is_empty());
            let matching = |label| {
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == label)
                    .collect::<Vec<_>>()
            };
            let declarations = matching("fsharp.function.declaration");
            let headers = matching("fsharp.function.signature");
            let markers = matching("fsharp.simple_parameters.signature");
            assert_eq!(declarations.len(), 1);
            assert_eq!(headers.len(), 1);
            assert_eq!(markers.len(), 1);
            assert_eq!(headers[0].kind(), SyntaxFactKind::Signature);
            assert_eq!(markers[0].kind(), SyntaxFactKind::Signature);
            assert_eq!(headers[0].span(), fixture.span(&header));
            assert_eq!(markers[0].span(), fixture.span(parameters));
            assert_ne!(headers[0].local_id(), markers[0].local_id());
            assert_ne!(headers[0].span(), markers[0].span());
            for proof in [headers[0], markers[0]] {
                let mut parent = proof.parent();
                let mut owner = None;
                for _ in 0..parsed.facts().len() {
                    let Some(id) = parent else { break };
                    let ancestor = parsed
                        .facts()
                        .iter()
                        .find(|fact| fact.local_id() == id)
                        .expect("retained proof parent exists");
                    if ancestor.syntax_kind().as_str() == "fsharp.function.declaration" {
                        owner = Some(ancestor.local_id());
                        break;
                    }
                    parent = ancestor.parent();
                }
                assert_eq!(owner, Some(declarations[0].local_id()));
            }
            let output = fixture.analyze();
            assert_exact_callable_signature(&fixture, &output, "carry", &header, names);
        }
    }
}

fn assert_exact_callable_signature(
    fixture: &Fixture,
    output: &AnalysisOutput,
    function_name: &str,
    header: &str,
    parameter_names: &[(&str, &str)],
) -> LexicalEvidenceV1 {
    let document = output.document();
    let functions = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Function && entity.canonical_name == function_name
        })
        .collect::<Vec<_>>();
    assert_eq!(functions.len(), 1);
    let function = functions[0];
    let function_source = function.evidence.source.as_ref().unwrap();
    assert_eq!(function_source.repository(), fixture.source.repository());
    assert_eq!(function_source.generation(), fixture.source.generation());
    assert_eq!(function_source.span().file(), fixture.snapshot.file());
    assert_eq!(
        function_source.content_hash(),
        fixture.source.content_hash()
    );
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: function.id,
                    })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0].source,
        SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            fixture.span(function_name),
            fixture.source.content_hash(),
            None,
        )
    );
    let signatures = document
        .extensions
        .iter()
        .filter(|envelope| envelope.namespace == LEXICAL_EXTENSION_NAMESPACE)
        .filter(|envelope| {
            let evidence = decode_lexical_evidence_envelope(envelope).unwrap();
            evidence.kind() == LexicalEvidenceKind::Signature
                && evidence.subject() == FactRef::Entity(function.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(signatures.len(), 1, "missing callable header: {header}");
    let expected_source = SourceRef::new(
        fixture.source.repository(),
        fixture.source.generation(),
        fixture.span(header),
        fixture.source.content_hash(),
        None,
    );
    let evidence = validate_lexical_evidence_envelope(
        signatures[0],
        FactRef::Entity(function.id),
        &expected_source,
        function.provenance,
    )
    .unwrap();
    assert_eq!(evidence.kind(), LexicalEvidenceKind::Signature);
    assert_eq!(evidence.format(), LexicalEvidenceFormat::SourceText);
    assert_eq!(evidence.text(), header);
    assert_eq!(
        evidence.complete_text_hash(),
        content_hash(header.as_bytes())
    );
    assert!(!evidence.is_truncated());
    evidence.verify_source_text(header).unwrap();
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.detail == "signature-capture-unavailable" && gap.source == *function_source
    }));
    let parameters = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.container == Some(ContainerRef::Entity(function.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), parameter_names.len());
    for (name, pattern) in parameter_names {
        let parameter = parameters
            .iter()
            .find(|parameter| parameter.canonical_name == *name)
            .unwrap();
        let parameter_source = parameter.evidence.source.as_ref().unwrap();
        assert_eq!(
            *parameter_source,
            SourceRef::new(
                fixture.source.repository(),
                fixture.source.generation(),
                fixture.span(name),
                fixture.source.content_hash(),
                None,
            )
        );
        assert_exact_parameter_header(fixture, output, function_name, name, pattern);
    }
    evidence
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
    let formal = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
            .unwrap()
            .id
    };
    assert_eq!(formal(&before), formal(&after));
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
    let before_signature = assert_exact_callable_signature(
        &before_fixture,
        &before,
        "pick",
        "pick value ",
        &[("value", "value")],
    );
    let after_signature = assert_exact_callable_signature(
        &after_fixture,
        &after,
        "pick",
        "pick value ",
        &[("value", "value")],
    );
    assert_eq!(before_signature, after_signature);
    assert_eq!(
        assert_exact_parameter_header(&before_fixture, &before, "pick", "value", "value"),
        assert_exact_parameter_header(&after_fixture, &after, "pick", "value", "value"),
    );
}

#[test]
fn parameter_headers_keep_authored_patterns_and_exact_leaf_sources() {
    for (formals, body, parameters) in [
        ("value", "value", &[("value", "value")][..]),
        ("(value)", "value", &[("value", "(value)")][..]),
        ("(value: int)", "value", &[("value", "(value: int)")][..]),
        (
            "left (δ: int)",
            "left + δ",
            &[("left", "left"), ("δ", "(δ: int)")][..],
        ),
        (
            "(``value.with spaces``: int)",
            "``value.with spaces``",
            &[("``value.with spaces``", "(``value.with spaces``: int)")][..],
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            let text = format!("module Headers{newline}let carry {formals} = {body}{newline}");
            let fixture = Fixture::new("fsharp", &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(parsed.diagnostics().is_empty());
            let headers = parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "fsharp.parameter_header.expression")
                .collect::<Vec<_>>();
            assert_eq!(headers.len(), parameters.len(), "{formals}");
            let output = fixture.analyze();
            for (name, pattern) in parameters {
                let expected_header = authored_parameter_span(&fixture, "carry", pattern);
                assert_eq!(
                    headers
                        .iter()
                        .filter(|fact| fact.span() == expected_header)
                        .count(),
                    1
                );
                assert!(
                    headers
                        .iter()
                        .all(|fact| fact.kind() == SyntaxFactKind::Signature)
                );
                assert_exact_parameter_header(&fixture, &output, "carry", name, pattern);
            }
            assert!(
                !output
                    .document()
                    .relations
                    .iter()
                    .any(|relation| { relation.predicate == RelationPredicate::Calls })
            );
        }
    }
}

#[test]
fn parameter_headers_optional_removal_preserves_identity_and_formal_targets() {
    let fixture = Fixture::new(
        "fsharp",
        "module Headers\nlet first (value: int) = value + 1\nlet second (value: int) = value + 2\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let complete = parsed.facts().to_vec();
    let clipped = complete
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() != "fsharp.parameter_header.expression")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(complete.len() - clipped.len(), 2);
    let analyze = |facts| {
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        fixture.analyze_with_parser(Arc::new(parser), IrLimits::default())
    };
    let full = analyze(complete);
    let clipped = analyze(clipped);
    assert_eq!(full.document().entities, clipped.document().entities);
    assert_eq!(full.document().occurrences, clipped.document().occurrences);
    assert_eq!(full.document().relations, clipped.document().relations);
    for function in ["first", "second"] {
        assert_exact_parameter_header(&fixture, &full, function, "value", "(value: int)");
        let owner = clipped
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == function)
            .unwrap();
        let parameter = clipped
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        assert_eq!(parameter.canonical_name, "value");
        assert!(!clipped.document().extensions.iter().any(|envelope| {
            envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                lexical.kind() == LexicalEvidenceKind::Signature
                    && lexical.subject() == FactRef::Entity(parameter.id)
            }
        }));
        assert_eq!(
            clipped
                .document()
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.domain == FactDomain::Extensions
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.detail == "signature-capture-unavailable"
                        && gap.source == *parameter.evidence.source.as_ref().unwrap()
                })
                .count(),
            1
        );
    }
}

#[test]
fn parameter_headers_do_not_claim_unsupported_or_incomplete_patterns() {
    for formals in [
        "x (left, Some right)",
        "x ((typed: int))",
        "x (Some item)",
        "x [item]",
        "x { Field = item }",
        "x (typed: int",
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!("module Patterns\nlet choose {formals} = x\n"),
        );
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert!(
            !parsed.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "fsharp.parameter_header.expression"
            }),
            "{formals}"
        );
        let output = fixture.analyze();
        let document = output.document();
        assert!(
            !document.extensions.iter().any(|envelope| {
                envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                    let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                    lexical.kind() == LexicalEvidenceKind::Signature
                        && document.entities.iter().any(|entity| {
                            entity.kind == EntityKind::Parameter
                                && lexical.subject() == FactRef::Entity(entity.id)
                        })
                }
            }),
            "{formals}"
        );
        assert!(
            !document.relations.iter().any(|relation| {
                matches!(
                    relation.predicate,
                    RelationPredicate::RefersTo | RelationPredicate::Calls
                )
            }),
            "{formals}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Entities || gap.reason == SkippedRegionReason::ParseError
            }),
            "{formals}"
        );
    }
}

#[test]
fn parameter_headers_text_limit_keeps_exact_fit_and_scopes_one_byte_over() {
    let maximum = rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES;
    let pattern = |size: usize| format!("(value:{}int)", " ".repeat(size - "(value:int)".len()));
    let exact_pattern = pattern(maximum);
    let over_pattern = pattern(maximum + 1);
    assert_eq!(exact_pattern.len(), maximum);
    assert_eq!(over_pattern.len(), maximum + 1);
    let exact_fixture = Fixture::new(
        "fsharp",
        &format!("module Bounded\nlet carry {exact_pattern} = value\n"),
    );
    let over_fixture = Fixture::new(
        "fsharp",
        &format!("module Bounded\nlet carry {over_pattern} = value\n"),
    );
    let exact = exact_fixture.analyze();
    let over = over_fixture.analyze();
    assert_exact_parameter_header(&exact_fixture, &exact, "carry", "value", &exact_pattern);
    let named = |output: &AnalysisOutput, kind, name: &str| {
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == name)
            .unwrap()
            .id
    };
    for (kind, name) in [
        (EntityKind::Function, "carry"),
        (EntityKind::Parameter, "value"),
    ] {
        assert_eq!(named(&exact, kind, name), named(&over, kind, name));
    }
    let document = over.document();
    let function = named(&over, EntityKind::Function, "carry");
    let parameter = named(&over, EntityKind::Parameter, "value");
    let leaf_source = SourceRef::new(
        over_fixture.source.repository(),
        over_fixture.source.generation(),
        over_fixture.span("value"),
        over_fixture.source.content_hash(),
        None,
    );
    let definition = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: parameter })
        })
        .collect::<Vec<_>>();
    assert_eq!(definition.len(), 1);
    assert_eq!(definition[0].source, leaf_source);
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.target == (OccurrenceTarget::Resolved { symbol: parameter })
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].enclosing, Some(function));
    assert_eq!(
        reads[0].source.repository(),
        over_fixture.source.repository()
    );
    assert_eq!(
        reads[0].source.generation(),
        over_fixture.source.generation()
    );
    assert_eq!(
        reads[0].source.content_hash(),
        over_fixture.source.content_hash()
    );
    let edges = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(reads[0].id)
        })
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&reads[0].source));
    assert!(!document.extensions.iter().any(|envelope| {
        envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
            let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
            lexical.kind() == LexicalEvidenceKind::Signature
                && lexical.subject() == FactRef::Entity(parameter)
        }
    }));
    let header_source = SourceRef::new(
        over_fixture.source.repository(),
        over_fixture.source.generation(),
        authored_parameter_span(&over_fixture, "carry", &over_pattern),
        over_fixture.source.content_hash(),
        None,
    );
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == FactDomain::Extensions
                    && gap.reason == SkippedRegionReason::ResourceLimit
                    && gap.detail == "lexical-evidence-resource-limit"
                    && gap.source == header_source
                    && gap.evidence.source.as_ref() == Some(&header_source)
            })
            .count(),
        1
    );
    assert!(
        !document.skipped_regions.iter().any(|gap| {
            gap.detail == "signature-capture-unavailable" && gap.source == leaf_source
        })
    );
}

#[test]
fn parameter_headers_extension_clipping_preserves_exact_identity_records() {
    let fixture = Fixture::new("fsharp", "module Bounded\nlet carry (value: int) = value\n");
    let full = fixture.analyze();
    assert_exact_parameter_header(&fixture, &full, "carry", "value", "(value: int)");
    let mut exact_limits = IrLimits::default();
    exact_limits.max_extensions = full.document().extensions.len();
    let exact = fixture.analyze_with_ir_limits(exact_limits);
    assert_eq!(exact.document().extensions, full.document().extensions);
    let mandatory = full.document().entities.len() + full.document().files.len();
    assert!(mandatory < full.document().extensions.len());
    for maximum in [full.document().extensions.len() - 1, mandatory] {
        let mut clipped_limits = IrLimits::default();
        clipped_limits.max_extensions = maximum;
        let clipped = fixture.analyze_with_ir_limits(clipped_limits);
        assert_eq!(clipped.document().extensions.len(), maximum);
        assert_eq!(clipped.document().entities, full.document().entities);
        assert_eq!(clipped.document().occurrences, full.document().occurrences);
        assert_eq!(clipped.document().relations, full.document().relations);
        let parameter = clipped
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
            .unwrap();
        assert!(!clipped.document().extensions.iter().any(|envelope| {
            envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                lexical.kind() == LexicalEvidenceKind::Signature
                    && lexical.subject() == FactRef::Entity(parameter.id)
            }
        }));
        let header_source = SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            authored_parameter_span(&fixture, "carry", "(value: int)"),
            fixture.source.content_hash(),
            None,
        );
        assert_eq!(
            clipped
                .document()
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.domain == FactDomain::Extensions
                        && gap.reason == SkippedRegionReason::ResourceLimit
                        && gap.detail == "lexical-evidence-resource-limit"
                        && gap.source == header_source
                        && gap.evidence.source.as_ref() == Some(&header_source)
                })
                .count(),
            1
        );
    }
}

#[test]
fn parameter_headers_tight_core_budgets_do_not_charge_optional_proof_twice() {
    use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};

    let pattern = format!(
        "(value:{}int)",
        " ".repeat(rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES - "(value:int)".len(),)
    );
    let fixture = Fixture::new(
        "fsharp",
        &format!("module Bounded\nlet carry {pattern} = value\n"),
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let complete = parsed.facts().to_vec();
    let essential = complete
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() != "fsharp.parameter_header.expression")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(complete.len() - essential.len(), 1);
    let capabilities = provider().capabilities().clone();
    let analyze = |facts: &[rootlight_adapter_sdk::SyntaxFact], limits: IrLimits| {
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            capabilities.clone(),
            facts.to_vec(),
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        fixture.try_analyze_with_parser(Arc::new(parser), limits)
    };
    let quota_error = |error, resource, maximum| {
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit {
            resource: observed_resource, observed, limit,
        }) if observed_resource == resource && observed > maximum && limit == maximum),
            "unexpected essential-only rejection: {error:?}"
        );
    };
    let minimum = |set: fn(&mut IrLimits, usize), maximum, resource| {
        let mut low = 0;
        let mut high = maximum;
        let mut limits = IrLimits::default();
        set(&mut limits, high);
        analyze(&essential, limits).unwrap();
        while low < high {
            let middle = low + (high - low) / 2;
            let mut limits = IrLimits::default();
            set(&mut limits, middle);
            match analyze(&essential, limits) {
                Ok(_) => high = middle,
                Err(error) => {
                    quota_error(error, resource, middle);
                    low = middle + 1;
                }
            }
        }
        assert!(low > 0);
        let mut limits = IrLimits::default();
        set(&mut limits, low - 1);
        quota_error(analyze(&essential, limits).unwrap_err(), resource, low - 1);
        low
    };
    let defaults = IrLimits::default();
    let string_limit = minimum(
        |limits, maximum| limits.max_total_string_bytes = maximum,
        defaults.max_total_string_bytes,
        ResourceKind::StringBytes,
    );
    let skipped_limit = minimum(
        |limits, maximum| limits.max_skipped_regions = maximum,
        defaults.max_skipped_regions,
        ResourceKind::Records,
    );
    let record_limit = minimum(
        |limits, maximum| limits.max_total_records = maximum,
        defaults.max_total_records,
        ResourceKind::Records,
    );
    let full = analyze(&complete, defaults.clone()).unwrap();
    assert_exact_parameter_header(&fixture, &full, "carry", "value", &pattern);
    let parameter = full
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
        .unwrap();
    let header_source = SourceRef::new(
        fixture.source.repository(),
        fixture.source.generation(),
        authored_parameter_span(&fixture, "carry", &pattern),
        fixture.source.content_hash(),
        None,
    );
    let mut string_limits = defaults.clone();
    string_limits.max_total_string_bytes = string_limit;
    let mut skipped_limits = defaults.clone();
    skipped_limits.max_skipped_regions = skipped_limit;
    let mut record_limits = defaults.clone();
    record_limits.max_total_records = record_limit;
    let mut combined_limits = defaults;
    combined_limits.max_total_string_bytes = string_limit;
    combined_limits.max_skipped_regions = skipped_limit;
    combined_limits.max_total_records = record_limit;
    for limits in [
        string_limits,
        skipped_limits,
        record_limits,
        combined_limits,
    ] {
        let baseline = analyze(&essential, limits.clone()).unwrap();
        let witness = analyze(&complete, limits.clone()).unwrap();
        for output in [&baseline, &witness] {
            assert_eq!(output.document().files, full.document().files);
            assert_eq!(output.document().entities, full.document().entities);
            assert_eq!(output.document().occurrences, full.document().occurrences);
            assert_eq!(output.document().relations, full.document().relations);
            assert_eq!(output.document().provenance, full.document().provenance);
            assert_eq!(
                output.document().source_mappings,
                full.document().source_mappings
            );
            assert_eq!(output.document().diagnostics, full.document().diagnostics);
        }
        assert_eq!(
            witness.report().resources().output_records(),
            baseline.report().resources().output_records()
        );
        if limits.max_total_string_bytes == string_limit {
            assert!(!witness.document().extensions.iter().any(|envelope| {
                envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                    let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                    lexical.kind() == LexicalEvidenceKind::Signature
                        && lexical.subject() == FactRef::Entity(parameter.id)
                }
            }));
            assert_eq!(
                witness
                    .document()
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == FactDomain::Extensions
                            && gap.reason == SkippedRegionReason::ResourceLimit
                            && gap.detail == "lexical-evidence-resource-limit"
                            && gap.source == header_source
                            && gap.evidence.source.as_ref() == Some(&header_source)
                    })
                    .count(),
                1
            );
            assert_eq!(
                witness.document().skipped_regions.len(),
                baseline.document().skipped_regions.len()
            );
        } else {
            assert_exact_parameter_header(&fixture, &witness, "carry", "value", &pattern);
            assert_eq!(
                witness.document().skipped_regions.len() + 1,
                baseline.document().skipped_regions.len()
            );
        }
    }
}

fn authored_parameter_span(fixture: &Fixture, function: &str, pattern: &str) -> SourceSpan {
    let prefix = format!("let {function} ");
    let start = fixture.text.find(&prefix).unwrap() + prefix.len();
    let start = start + fixture.text.get(start..).unwrap().find(pattern).unwrap();
    SourceSpan::new(
        fixture.snapshot.file(),
        u64::try_from(start).unwrap(),
        u64::try_from(start + pattern.len()).unwrap(),
    )
    .unwrap()
}

fn assert_exact_parameter_header(
    fixture: &Fixture,
    output: &AnalysisOutput,
    function_name: &str,
    name: &str,
    pattern: &str,
) -> LexicalEvidenceV1 {
    let document = output.document();
    let function = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function && entity.canonical_name == function_name
        })
        .unwrap();
    let parameters = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == name
                && entity.container == Some(ContainerRef::Entity(function.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), 1);
    let parameter = parameters[0];
    let header_span = authored_parameter_span(fixture, function_name, pattern);
    let leaf_start = header_span.start_byte() + u64::try_from(pattern.find(name).unwrap()).unwrap();
    let leaf_span = SourceSpan::new(
        fixture.snapshot.file(),
        leaf_start,
        leaf_start + u64::try_from(name.len()).unwrap(),
    )
    .unwrap();
    let leaf_source = SourceRef::new(
        fixture.source.repository(),
        fixture.source.generation(),
        leaf_span,
        fixture.source.content_hash(),
        None,
    );
    assert_eq!(parameter.evidence.source.as_ref(), Some(&leaf_source));
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: parameter.id,
                    })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].source, leaf_source);
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: parameter.id,
                    })
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].enclosing, Some(function.id));
    assert_eq!(reads[0].source.repository(), fixture.source.repository());
    assert_eq!(reads[0].source.generation(), fixture.source.generation());
    assert_eq!(
        reads[0].source.content_hash(),
        fixture.source.content_hash()
    );
    assert_eq!(reads[0].source.span().file(), fixture.snapshot.file());
    let function_span = function.evidence.source.as_ref().unwrap().span();
    assert!(function_span.start_byte() <= reads[0].source.span().start_byte());
    assert!(reads[0].source.span().end_byte() <= function_span.end_byte());
    assert_eq!(
        fixture.text.get(
            usize::try_from(reads[0].source.span().start_byte()).unwrap()
                ..usize::try_from(reads[0].source.span().end_byte()).unwrap(),
        ),
        Some(name)
    );
    let edges = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(reads[0].id)
        })
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter.id));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&reads[0].source));
    let signatures = document
        .extensions
        .iter()
        .filter(|envelope| {
            envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                lexical.kind() == LexicalEvidenceKind::Signature
                    && lexical.subject() == FactRef::Entity(parameter.id)
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(signatures.len(), 1, "missing parameter header: {pattern}");
    let expected_source = SourceRef::new(
        fixture.source.repository(),
        fixture.source.generation(),
        header_span,
        fixture.source.content_hash(),
        None,
    );
    let lexical = validate_lexical_evidence_envelope(
        signatures[0],
        FactRef::Entity(parameter.id),
        &expected_source,
        parameter.provenance,
    )
    .unwrap();
    assert_eq!(lexical.format(), LexicalEvidenceFormat::SourceText);
    assert_eq!(lexical.text(), pattern);
    assert_eq!(
        lexical.complete_text_hash(),
        content_hash(pattern.as_bytes())
    );
    assert!(!lexical.is_truncated());
    lexical.verify_source_text(pattern).unwrap();
    assert!(
        !document.skipped_regions.iter().any(|gap| {
            gap.detail == "signature-capture-unavailable" && gap.source == leaf_source
        })
    );
    lexical
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
    assert!(
        !parsed
            .facts()
            .iter()
            .any(|fact| { fact.syntax_kind().as_str() == "fsharp.parameter_header.expression" })
    );
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

#[test]
fn module_value_and_function_bodies_resolve_prior_lexical_bindings() {
    for newline in ["\n", "\r\n"] {
        let text =
            "module Reads\nlet initial = 1\nlet alias = initial\nlet carry argument = initial\n"
                .replace('\n', newline);
        let fixture = Fixture::new("fsharp", &text);
        let output = fixture.analyze();
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "alias",
            "initial",
            None,
            "alias = initial",
            "initial",
        );
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "carry",
            "initial",
            None,
            "carry argument = initial",
            "initial",
        );
        assert_eq!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .count(),
            2
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn optional_binding_and_application_facts_keep_exact_lexical_owners() {
    let fixture = Fixture::new(
        "fsharp",
        "module Owners\nlet helper left right = left\nlet invoke argument = helper argument 1\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let declaration_span = fixture.span("invoke argument = helper argument 1");
    let declaration = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.function.declaration"
                && fact.span() == declaration_span
        })
        .unwrap();
    let scope = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.function.scope"
                && fact.span() == declaration_span
        })
        .unwrap();
    let marker = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.nonrecursive_binding.expression"
                && fact.span() == declaration_span
        })
        .unwrap();
    assert_eq!(marker.parent(), Some(declaration.local_id()));
    let definition = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.name.definition"
                && fact.span() == fixture.span("invoke")
        })
        .unwrap();
    assert!(matches!(definition.parent(), Some(parent)
        if parent == scope.local_id() || parent == declaration.local_id()));
    let read_span = lexical_site_span(&fixture, "helper argument 1", "helper");
    let read = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.simple_function_body_read.reference"
                && fact.span() == read_span
        })
        .unwrap();
    assert_eq!(read.parent(), Some(scope.local_id()));
    let call = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.application.call"
                && fact.span() == fixture.span("helper argument 1")
        })
        .unwrap();
    assert_eq!(call.parent(), Some(scope.local_id()));
    for argument in parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "fsharp.application_argument.expression")
    {
        assert_eq!(argument.parent(), Some(scope.local_id()));
    }
}

#[test]
fn direct_application_spines_resolve_exact_saturated_module_functions() {
    for newline in ["\n", "\r\n"] {
        let text = "module Applications\nlet helper left right = left\nlet invoke argument = helper argument 1\n"
            .replace('\n', newline);
        let fixture = Fixture::new("fsharp", &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        let calls = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "fsharp.application.call")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].span(), fixture.span("helper argument 1"));
        let arguments = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "fsharp.application_argument.expression")
            .map(|fact| fact.span())
            .collect::<Vec<_>>();
        let argument_start = u64::try_from(text.rfind("argument 1").unwrap()).unwrap();
        assert_eq!(
            arguments,
            [
                SourceSpan::new(fixture.snapshot.file(), argument_start, argument_start + 8)
                    .unwrap(),
                fixture.span("1"),
            ]
        );
        let output = fixture.analyze();
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "invoke",
            "helper",
            None,
            "helper argument 1",
            "helper",
        );
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "invoke",
            "argument",
            Some("invoke"),
            "helper argument 1",
            "argument",
        );
        let document = output.document();
        let caller = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "invoke")
            .unwrap();
        let callee = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "helper")
            .unwrap();
        let calls = document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].subject, RelationEndpoint::Entity(caller.id));
        assert_eq!(calls[0].object, RelationEndpoint::Entity(callee.id));
        assert_eq!(
            calls[0].evidence.source.as_ref().unwrap().span(),
            fixture.span("helper argument 1")
        );
    }
}

#[test]
fn parenthesized_and_nested_direct_calls_keep_exact_argument_boundaries() {
    for (body, expected, expected_arguments) in [
        (
            "helper(argument)",
            vec![("helper(argument)", "helper")],
            vec!["(argument)"],
        ),
        (
            "outer inner(argument)",
            vec![
                ("outer inner(argument)", "outer"),
                ("inner(argument)", "inner"),
            ],
            vec!["inner(argument)", "(argument)"],
        ),
        (
            "outer (inner argument)",
            vec![
                ("outer (inner argument)", "outer"),
                ("inner argument", "inner"),
            ],
            vec!["(inner argument)", "argument"],
        ),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!(
                "module Calls\nlet helper input = input\nlet inner input = input\nlet outer input = input\nlet invoke argument = {body}\n"
            ),
        );
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        let mut arguments = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "fsharp.application_argument.expression")
            .map(|fact| {
                fixture
                    .text
                    .get(
                        usize::try_from(fact.span().start_byte()).unwrap()
                            ..usize::try_from(fact.span().end_byte()).unwrap(),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        arguments.sort_unstable();
        let mut expected_arguments = expected_arguments;
        expected_arguments.sort_unstable();
        assert_eq!(arguments, expected_arguments, "{body}");
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "fsharp.application.call")
                .count(),
            expected.len()
        );
        let output = fixture.analyze();
        let document = output.document();
        let caller = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "invoke")
            .unwrap();
        let calls = document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), expected.len(), "{body}");
        for (site, name) in expected {
            let target = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            let matching = calls
                .iter()
                .filter(|call| call.evidence.source.as_ref().unwrap().span() == fixture.span(site))
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "{body}: {site}");
            assert_eq!(matching[0].subject, RelationEndpoint::Entity(caller.id));
            assert_eq!(matching[0].object, RelationEndpoint::Entity(target.id));
        }
    }
}

#[test]
fn tuple_formals_keep_each_written_definition_and_reference() {
    for (formals, names) in [
        ("(left, right)", &["left", "right"][..]),
        ("first (left, right)", &["first", "left", "right"][..]),
        ("(left: int, right: int)", &["left", "right"][..]),
        ("(left, (middle, right))", &["left", "middle", "right"][..]),
        ("(δ, ``right value``)", &["δ", "``right value``"][..]),
    ] {
        for newline in ["\n", "\r\n"] {
            let body = names.join(" + ");
            let text = format!("module TupleFormals\nlet combine {formals} = {body}\n")
                .replace('\n', newline);
            let fixture = Fixture::new("fsharp", &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete,
                "{formals}"
            );
            let output = fixture.analyze();
            let document = output.document();
            let function = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "combine"
                })
                .unwrap();
            let parameters = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.container == Some(ContainerRef::Entity(function.id))
                })
                .collect::<Vec<_>>();
            assert_eq!(parameters.len(), names.len(), "{formals}");
            for name in names {
                let parameter = parameters
                    .iter()
                    .find(|entity| entity.canonical_name == *name)
                    .unwrap();
                let expected = lexical_site_span(&fixture, &format!("combine {formals}"), name);
                let source = parameter.evidence.source.as_ref().unwrap();
                assert_eq!(source.span(), expected, "{formals}: {name}");
                assert_eq!(source.repository(), fixture.source.repository());
                assert_eq!(source.generation(), fixture.source.generation());
                assert_eq!(source.content_hash(), fixture.source.content_hash());
                let definitions = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target
                                == (OccurrenceTarget::Resolved {
                                    symbol: parameter.id,
                                })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(definitions.len(), 1, "{formals}: {name}");
                assert_eq!(definitions[0].source, *source);
                assert_exact_lexical_reference(
                    &fixture,
                    &output,
                    "combine",
                    name,
                    Some("combine"),
                    &format!("= {body}"),
                    name,
                );
                let pattern = if formals == "(left: int, right: int)" {
                    format!("{name}: int")
                } else {
                    (*name).to_owned()
                };
                assert_exact_parameter_header(&fixture, &output, "combine", name, &pattern);
            }
            assert!(
                !document.skipped_regions.iter().any(|gap| {
                    gap.domain == FactDomain::Entities && gap.source.span() == fixture.span(formals)
                }),
                "{formals}"
            );
        }
    }
}

#[test]
fn tuple_formals_count_argument_slots_independently_of_names() {
    for (formals, body, call, named) in [
        ("(left, right)", "left", "carry (input, input)", 2),
        (
            "(left, right) extra",
            "right",
            "carry (input, input) input",
            3,
        ),
        (
            "extra (left, right)",
            "extra",
            "carry input (input, input)",
            3,
        ),
        ("(left, right) ()", "right", "carry (input, input) ()", 2),
        ("() (left, right)", "left", "carry () (input, input)", 2),
        (
            "(left, (middle, right))",
            "middle",
            "carry (input, (input, input))",
            3,
        ),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!(
                "module TupleCalls\nlet carry {formals} = {body}\nlet invoke input = {call}\n"
            ),
        );
        let output = fixture.analyze();
        let document = output.document();
        let carry = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "carry")
            .unwrap();
        let invoke = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "invoke")
            .unwrap();
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.container == Some(ContainerRef::Entity(carry.id))
                })
                .count(),
            named,
            "{formals}"
        );
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "carry",
            body,
            Some("carry"),
            &format!("= {body}"),
            body,
        );
        let calls = document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1, "{formals}: {call}");
        assert_eq!(calls[0].subject, RelationEndpoint::Entity(invoke.id));
        assert_eq!(calls[0].object, RelationEndpoint::Entity(carry.id));
        assert_eq!(
            calls[0].evidence.source.as_ref().unwrap().span(),
            fixture.span(call)
        );
    }
}

#[test]
fn tuple_formals_keep_partial_and_overapplied_calls_unproved() {
    for (formals, call) in [
        ("(left, right) extra", "carry (input, input)"),
        ("(left, right)", "carry input input"),
        ("(left, right)", "carry (input, input) input"),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!("module TupleCalls\nlet carry {formals} = left\nlet invoke input = {call}\n"),
        );
        let output = fixture.analyze();
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "carry",
            "left",
            Some("carry"),
            "= left",
            "left",
        );
        assert_exact_lexical_reference(&fixture, &output, "invoke", "carry", None, call, "carry");
        assert!(
            !output.document().relations.iter().any(|edge| matches!(
                edge.predicate,
                RelationPredicate::Calls | RelationPredicate::DispatchCandidate
            )),
            "{formals}: {call}"
        );
    }
}

#[test]
fn tuple_formals_in_local_functions_keep_captures_and_call_slots() {
    for text in [
        "module Locals\nlet outer seed =\n    let combine (left, right) extra = left + right + extra + seed\n    combine (seed, seed) seed\n",
        "module Locals\nlet outer seed = let combine (left, right) extra = left + right + extra + seed in combine (seed, seed) seed\n",
    ] {
        let fixture = Fixture::new("fsharp", text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        let output = fixture.analyze();
        for (name, owner) in [
            ("left", "combine"),
            ("right", "combine"),
            ("extra", "combine"),
            ("seed", "outer"),
        ] {
            assert_exact_lexical_reference(
                &fixture,
                &output,
                "combine",
                name,
                Some(owner),
                "left + right + extra + seed",
                name,
            );
        }
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "outer",
            "combine",
            None,
            "combine (seed, seed) seed",
            "combine",
        );
        let document = output.document();
        let outer = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "outer")
            .unwrap();
        let combine = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "combine")
            .unwrap();
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter
                    && entity.container == Some(ContainerRef::Entity(combine.id)))
                .count(),
            3
        );
        let calls = document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].subject, RelationEndpoint::Entity(outer.id));
        assert_eq!(calls[0].object, RelationEndpoint::Entity(combine.id));
        assert_eq!(
            calls[0].evidence.source.as_ref().unwrap().span(),
            fixture.span("combine (seed, seed) seed")
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities)
        );
    }
}

#[test]
fn tuple_formal_optional_proof_loss_preserves_authored_identities() {
    let fixture = Fixture::new(
        "fsharp",
        "module TupleProofs\nlet left = 99\nlet carry (left, right) = left\nlet invoke input = carry (input, input)\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &full,
        "carry",
        "left",
        Some("carry"),
        "= left",
        "left",
    );
    assert_eq!(
        full.document()
            .relations
            .iter()
            .filter(|edge| { edge.predicate == RelationPredicate::Calls })
            .count(),
        1
    );
    let formals = fixture.span("(left, right)");
    for label in [
        "fsharp.compound_parameters.expression",
        "fsharp.tuple_parameter.expression",
        "fsharp.parameter_header.expression",
    ] {
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| {
                    fact.syntax_kind().as_str() == label
                        && formals.start_byte() <= fact.span().start_byte()
                        && fact.span().end_byte() <= formals.end_byte()
                })
                .unwrap();
            if duplicate {
                let fact = &facts[index];
                let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            } else {
                facts.remove(index);
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert_eq!(
                clipped.document().entities,
                full.document().entities,
                "{label}: {duplicate}"
            );
            let definitions = |output: &AnalysisOutput| {
                output
                    .document()
                    .occurrences
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::Definition)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                definitions(&clipped),
                definitions(&full),
                "{label}: {duplicate}"
            );
            assert!(
                !clipped.document().relations.iter().any(|edge| matches!(
                    edge.predicate,
                    RelationPredicate::Calls | RelationPredicate::DispatchCandidate
                )),
                "{label}: {duplicate}"
            );
            let outer = clipped
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "left" && entity.kind != EntityKind::Parameter
                })
                .unwrap();
            let body = lexical_site_span(&fixture, "= left", "left");
            assert!(
                !clipped.document().occurrences.iter().any(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.source.span() == body
                        && site.target == (OccurrenceTarget::Resolved { symbol: outer.id })
                }),
                "{label}: {duplicate}"
            );
            assert!(
                clipped.document().skipped_regions.iter().any(|gap| {
                    gap.domain == FactDomain::Relations
                        && gap.source.span() == fixture.span("carry (input, input)")
                        && gap.detail == "fsharp-direct-call-target-unavailable"
                }),
                "{label}: {duplicate}"
            );
        }
    }
}

#[test]
fn tuple_formal_leaf_loss_cannot_reveal_an_outer_homonym() {
    let fixture = Fixture::new(
        "fsharp",
        "module TupleProofs\nlet left = 99\nlet carry (left, right) = left\nlet invoke input = carry (input, input)\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let leaf = lexical_site_span(&fixture, "carry (left, right)", "left");
    for label in ["fsharp.parameter.declaration", "fsharp.name.definition"] {
        let removed = parsed
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == label && fact.span() == leaf)
            .unwrap()
            .local_id();
        let facts = parsed
            .facts()
            .iter()
            .filter(|fact| {
                let mut ancestor = Some(fact.local_id());
                while let Some(id) = ancestor {
                    if id == removed {
                        return false;
                    }
                    ancestor = parsed
                        .facts()
                        .iter()
                        .find(|fact| fact.local_id() == id)
                        .and_then(rootlight_adapter_sdk::SyntaxFact::parent);
                }
                true
            })
            .cloned()
            .collect();
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        let document = clipped.document();
        let body = lexical_site_span(&fixture, "= left", "left");
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == body)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "{label}");
        assert!(
            matches!(reads[0].target, OccurrenceTarget::Unresolved { .. }),
            "{label}"
        );
        assert!(
            !document.relations.iter().any(|edge| {
                matches!(
                    edge.predicate,
                    RelationPredicate::Calls | RelationPredicate::DispatchCandidate
                ) || edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
            }),
            "{label}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations
                    && gap.source == reads[0].source
                    && gap.detail == "fsharp-formal-binding-target-unavailable"
            }),
            "{label}"
        );
    }
}

#[test]
fn tuple_formal_duplicate_names_close_reads_and_call_targets() {
    for (formals, call) in [
        ("(value, value)", "carry (input, input)"),
        ("(value: int, value: int)", "carry (input, input)"),
        ("value (value, other)", "carry input (input, input)"),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!(
                "module TupleDuplicates\nlet value = 99\nlet carry {formals} = value\nlet invoke input = {call}\n"
            ),
        );
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{formals}"
        );
        let output = fixture.analyze();
        let document = output.document();
        let carry = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "carry")
            .unwrap();
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && matches!(site.target, OccurrenceTarget::Resolved { symbol }
                    if document.entities.iter().any(|entity| entity.id == symbol
                        && entity.kind == EntityKind::Parameter && entity.canonical_name == "value"
                        && entity.container == Some(ContainerRef::Entity(carry.id))))
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 2, "{formals}");
        let body = lexical_site_span(&fixture, "= value", "value");
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == body)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "{formals}");
        assert!(
            matches!(reads[0].target, OccurrenceTarget::Unresolved { .. }),
            "{formals}"
        );
        assert!(
            !document.relations.iter().any(|edge| {
                matches!(
                    edge.predicate,
                    RelationPredicate::Calls | RelationPredicate::DispatchCandidate
                ) || edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
            }),
            "{formals}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations
                    && gap.source == reads[0].source
                    && gap.detail == "fsharp-formal-binding-target-unavailable"
            }),
            "{formals}"
        );
    }
}

#[test]
fn unit_formals_preserve_module_reads_and_saturated_call_sources() {
    for (formal, unit) in [(" ()", "()"), ("()", "()"), (" ( )", "( )")] {
        for call in [format!("run {unit}"), format!("run{unit}")] {
            let fixture = Fixture::new(
                "fsharp",
                &format!(
                    "module Units\nlet seed = 1\nlet run{formal} = seed\nlet invoke argument = {call}\n"
                ),
            );
            let output = fixture.analyze();
            let document = output.document();
            let run = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "run")
                .unwrap();
            let caller = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "invoke")
                .unwrap();
            assert!(!document.entities.iter().any(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.container == Some(ContainerRef::Entity(run.id))
            }));
            assert_exact_lexical_reference(
                &fixture, &output, "run", "seed", None, "= seed", "seed",
            );
            assert_exact_lexical_reference(&fixture, &output, "invoke", "run", None, &call, "run");
            let calls = document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Calls)
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 1, "{unit}: {call}");
            assert_eq!(calls[0].subject, RelationEndpoint::Entity(caller.id));
            assert_eq!(calls[0].object, RelationEndpoint::Entity(run.id));
            assert_eq!(
                calls[0].evidence.source.as_ref().unwrap().span(),
                lexical_site_span(&fixture, &call, &call)
            );
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Entities && gap.source.span() == fixture.span(unit)
            }));
        }
    }
}

#[test]
fn unit_formals_count_mixed_and_repeated_slots_without_synthetic_parameters() {
    for (formals, body, call, expected_parameters) in [
        ("(value: int) ()", "value", "carry input ()", 1),
        ("() (value: int)", "value", "carry () input", 1),
        ("() ()", "seed", "carry () ()", 0),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!(
                "module Units\nlet seed = 1\nlet carry {formals} = {body}\nlet invoke input = {call}\n"
            ),
        );
        let output = fixture.analyze();
        let document = output.document();
        let carry = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "carry")
            .unwrap();
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter
                    && entity.container == Some(ContainerRef::Entity(carry.id)))
                .count(),
            expected_parameters
        );
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "carry",
            body,
            (expected_parameters == 1).then_some("carry"),
            &format!("= {body}"),
            body,
        );
        assert_exact_lexical_reference(&fixture, &output, "invoke", "carry", None, call, "carry");
        let calls = document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1, "{formals}: {call}");
        assert_eq!(calls[0].object, RelationEndpoint::Entity(carry.id));
        assert_eq!(
            calls[0].evidence.source.as_ref().unwrap().span(),
            lexical_site_span(&fixture, call, call)
        );
    }
}

#[test]
fn unit_formals_keep_partial_and_overapplied_spines_unproved() {
    for (formals, call) in [
        ("value ()", "carry input"),
        ("value ()", "carry input () 1"),
        ("() ()", "carry ()"),
        ("()", "carry () ()"),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!(
                "module Units\nlet seed = 1\nlet carry {formals} = seed\nlet invoke input = {call}\n"
            ),
        );
        let output = fixture.analyze();
        assert_exact_lexical_reference(&fixture, &output, "invoke", "carry", None, call, "carry");
        assert!(!output.document().relations.iter().any(|relation| matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )));
        assert!(output.document().skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations
                && gap.source.span() == lexical_site_span(&fixture, call, call)
                && gap.detail == "fsharp-direct-call-target-unavailable"
        }));
    }
}

#[test]
fn unit_formals_optional_witness_removal_preserves_authored_identities() {
    let fixture = Fixture::new(
        "fsharp",
        "module Units\nlet carry (value: int) () = value\nlet invoke input = carry input ()\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert_eq!(
        full.document()
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    for (label, duplicate) in [
        ("fsharp.unit_parameters.expression", false),
        ("fsharp.unit_parameter.expression", false),
        ("fsharp.parameter_header.expression", false),
        ("fsharp.unit_parameter.expression", true),
    ] {
        let mut facts = parsed.facts().to_vec();
        let index = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == label)
            .unwrap();
        if duplicate {
            let slot = &facts[index];
            let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
            facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                id,
                slot.parent(),
                slot.kind(),
                slot.span(),
                slot.depth(),
                slot.syntax_kind().clone(),
            ));
        } else {
            facts.remove(index);
        }
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert_eq!(
            clipped.document().entities,
            full.document().entities,
            "{label}"
        );
        let definitions = |output: &AnalysisOutput| {
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition)
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(definitions(&clipped), definitions(&full), "{label}");
        assert!(
            !clipped
                .document()
                .relations
                .iter()
                .any(|relation| { relation.predicate == RelationPredicate::Calls }),
            "{label}"
        );
        assert!(
            clipped.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations
                    && gap.source.span() == fixture.span("carry input ()")
                    && gap.detail == "fsharp-direct-call-target-unavailable"
            }),
            "{label}"
        );
    }
}

#[test]
fn recursive_module_functions_resolve_exact_self_calls() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recursion\nlet rec repeat value = repeat value\n",
    );
    let output = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "repeat",
        "repeat",
        None,
        "= repeat value",
        "repeat",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "repeat",
        "value",
        Some("repeat"),
        "= repeat value",
        "value",
    );
    let repeat = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "repeat")
        .unwrap();
    let calls = output
        .document()
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(repeat.id));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(repeat.id));
    assert_eq!(
        calls[0].evidence.source.as_ref().unwrap().span(),
        lexical_site_span(&fixture, "= repeat value", "repeat value")
    );
}

#[test]
fn ordinary_lambdas_keep_exact_formals_captures_and_closed_callback_calls() {
    let fixture = Fixture::new(
        "fsharp",
        "module Lambdas\nlet seed = 3\nlet echo value = value\nlet build input =\n    let capture = fun value -> value + input + seed\n    let shadow = fun input -> input\n    let nested = fun outer -> fun inner -> outer + inner + input\n    let sibling = fun inner -> inner\n    let blocked = fun echo -> echo input\n    capture input + shadow input + nested input input + sibling input + blocked input + echo input\nlet outside () = inner\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let document = output.document();
    let mut symbols = std::collections::BTreeMap::new();
    for (key, name, kind) in [
        ("seed", "seed", EntityKind::Variable),
        ("echo", "echo", EntityKind::Function),
        ("build", "build", EntityKind::Function),
        ("capture", "capture", EntityKind::Variable),
        ("shadow", "shadow", EntityKind::Variable),
        ("nested", "nested", EntityKind::Variable),
        ("sibling", "sibling", EntityKind::Variable),
        ("blocked", "blocked", EntityKind::Variable),
        ("outside", "outside", EntityKind::Function),
    ] {
        let matching = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{key}");
        symbols.insert(key, matching[0].id);
    }
    let parameters = [
        ("echo.value", "echo", "echo value = value", "value"),
        ("build.input", "build", "build input =", "input"),
        (
            "capture.value",
            "capture",
            "capture = fun value ->",
            "value",
        ),
        ("shadow.input", "shadow", "shadow = fun input ->", "input"),
        ("nested.outer", "nested", "nested = fun outer ->", "outer"),
        (
            "nested.inner",
            "nested",
            "fun outer -> fun inner ->",
            "inner",
        ),
        (
            "sibling.inner",
            "sibling",
            "sibling = fun inner ->",
            "inner",
        ),
        ("blocked.echo", "blocked", "blocked = fun echo ->", "echo"),
    ];
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        parameters.len()
    );
    for (key, owner, written, name) in parameters {
        let start = fixture.text.find(written).unwrap() + written.find(name).unwrap();
        let expected = SourceSpan::new(
            fixture.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + name.len()).unwrap(),
        )
        .unwrap();
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span() == expected)
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{key}");
        let parameter = matching[0];
        assert_eq!(
            parameter.container,
            Some(ContainerRef::Entity(symbols[owner]))
        );
        symbols.insert(key, parameter.id);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{key}");
        assert_eq!(definitions[0].source.span(), expected);
        assert_eq!(
            definitions[0].source,
            *parameter.evidence.source.as_ref().unwrap()
        );
    }
    assert_ne!(symbols["echo.value"], symbols["capture.value"]);
    assert_ne!(symbols["build.input"], symbols["shadow.input"]);
    assert_ne!(symbols["nested.inner"], symbols["sibling.inner"]);
    let reads = [
        ("echo", "echo.value", "echo value = value", "value", true),
        (
            "capture",
            "capture.value",
            "value + input + seed",
            "value",
            false,
        ),
        (
            "capture",
            "build.input",
            "value + input + seed",
            "input",
            false,
        ),
        ("capture", "seed", "value + input + seed", "seed", false),
        (
            "shadow",
            "shadow.input",
            "shadow = fun input -> input",
            "input",
            true,
        ),
        (
            "nested",
            "nested.outer",
            "outer + inner + input",
            "outer",
            false,
        ),
        (
            "nested",
            "nested.inner",
            "outer + inner + input",
            "inner",
            false,
        ),
        (
            "nested",
            "build.input",
            "outer + inner + input",
            "input",
            false,
        ),
        (
            "sibling",
            "sibling.inner",
            "sibling = fun inner -> inner",
            "inner",
            true,
        ),
        (
            "blocked",
            "blocked.echo",
            "blocked = fun echo -> echo input",
            "echo",
            true,
        ),
        (
            "blocked",
            "build.input",
            "blocked = fun echo -> echo input",
            "input",
            false,
        ),
    ];
    for (owner, target, written, name, last) in reads {
        let start = fixture.text.find(written).unwrap()
            + if last {
                written.rfind(name).unwrap()
            } else {
                written.find(name).unwrap()
            };
        let span = SourceSpan::new(
            fixture.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + name.len()).unwrap(),
        )
        .unwrap();
        let matching = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{written}: {name}");
        let read = matching[0];
        assert_eq!(read.enclosing, Some(symbols[owner]));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: symbols[target]
            }
        );
        assert_eq!(read.source.repository(), fixture.source.repository());
        assert_eq!(read.source.generation(), fixture.source.generation());
        assert_eq!(read.source.content_hash(), fixture.source.content_hash());
        let relations = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), 1);
        assert_eq!(
            relations[0].object,
            RelationEndpoint::Entity(symbols[target])
        );
        assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
    }
    let leaked_span = lexical_site_span(&fixture, "outside () = inner", "inner");
    let leaked = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == leaked_span)
        .collect::<Vec<_>>();
    assert_eq!(leaked.len(), 1);
    assert!(matches!(
        leaked[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(leaked[0].id))
    );
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(symbols["build"]));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(symbols["echo"]));
    assert_eq!(
        calls[0].evidence.source.as_ref().unwrap().span(),
        lexical_site_span(&fixture, "echo input\nlet outside", "echo input")
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        3
    );
}

#[test]
fn module_value_lambdas_keep_multiple_formals_and_inner_local_bindings() {
    let fixture = Fixture::new(
        "fsharp",
        "module Lambdas\nlet apply = fun left right -> let cached = left in cached + right\nlet outside () = left\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let document = output.document();
    let mut symbols = std::collections::BTreeMap::new();
    for (name, kind) in [
        ("apply", EntityKind::Variable),
        ("cached", EntityKind::Variable),
        ("outside", EntityKind::Function),
        ("left", EntityKind::Parameter),
        ("right", EntityKind::Parameter),
    ] {
        let matching = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{name}");
        let entity = matching[0];
        symbols.insert(name, entity.id);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{name}");
        assert_eq!(definitions[0].source.span(), fixture.span(name));
        for source in [
            entity.evidence.source.as_ref().unwrap(),
            &definitions[0].source,
        ] {
            assert_eq!(source.repository(), fixture.source.repository());
            assert_eq!(source.generation(), fixture.source.generation());
            assert_eq!(source.span().file(), fixture.snapshot.file());
            assert_eq!(source.content_hash(), fixture.source.content_hash());
        }
        if kind == EntityKind::Parameter {
            assert_eq!(
                entity.evidence.source.as_ref().unwrap().span(),
                fixture.span(name)
            );
        }
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        2
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        1
    );
    for name in ["left", "right", "cached"] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbols[name])
            .unwrap();
        assert_eq!(
            entity.container,
            Some(ContainerRef::Entity(symbols["apply"]))
        );
    }
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == symbols["cached"])
            .unwrap()
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span(),
        fixture.span("cached = left")
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        4
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        3
    );
    for (owner, target, written, name) in [
        ("cached", Some("left"), "cached = left", "left"),
        ("apply", Some("cached"), "cached + right", "cached"),
        ("apply", Some("right"), "cached + right", "right"),
        ("outside", None, "outside () = left", "left"),
    ] {
        let expected = lexical_site_span(&fixture, written, name);
        let matching = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == expected)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{written}: {name}");
        let read = matching[0];
        assert_eq!(read.enclosing, Some(symbols[owner]));
        assert_eq!(read.source.repository(), fixture.source.repository());
        assert_eq!(read.source.generation(), fixture.source.generation());
        assert_eq!(read.source.content_hash(), fixture.source.content_hash());
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: symbols[target]
                }
            );
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
        } else {
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
    assert!(!document.relations.iter().any(|edge| matches!(
        edge.predicate,
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate
    )));
}

#[test]
fn atomic_lambda_formals_keep_exact_headers_and_lexical_targets() {
    for newline in ["\n", "\r\n"] {
        for (head, body, arguments, patterns) in [
            (
                "(value)",
                "value + input",
                "input",
                vec![("value", "(value)")],
            ),
            (
                "(value: int)",
                "value + input",
                "input",
                vec![("value", "(value: int)")],
            ),
            ("()", "input", "()", vec![]),
            (
                "(value: int) () (other)",
                "value + other + input",
                "input () input",
                vec![("value", "(value: int)"), ("other", "(other)")],
            ),
            (
                "(input: int)",
                "input",
                "input",
                vec![("input", "(input: int)")],
            ),
        ] {
            let text = format!("module LambdaShapes\n// ☃ 🧭\nlet outer input =\n    let callback = fun {head} -> {body}\n    callback {arguments}\n").replace('\n', newline);
            let fixture = Fixture::new("fsharp", &text);
            assert_eq!(
                fixture
                    .parse(4096, &deadline())
                    .unwrap()
                    .report()
                    .coverage()
                    .status(),
                CoverageStatus::Complete
            );
            let output = fixture.analyze();
            let document = output.document();
            let callback = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "callback" && entity.kind == EntityKind::Variable
                })
                .unwrap();
            let outer = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "outer" && entity.kind == EntityKind::Function
                })
                .unwrap();
            let outer_input = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "input"
                        && entity.kind == EntityKind::Parameter
                        && entity.container == Some(ContainerRef::Entity(outer.id))
                })
                .unwrap();
            assert_eq!(
                document
                    .entities
                    .iter()
                    .filter(|entity| entity.kind == EntityKind::Function)
                    .count(),
                1
            );
            assert_eq!(
                document
                    .entities
                    .iter()
                    .filter(|entity| entity.kind == EntityKind::Parameter)
                    .count(),
                patterns.len() + 1
            );
            let mut parameters = std::collections::BTreeMap::new();
            for (name, pattern) in patterns {
                let header_span = fixture.span(pattern);
                let start =
                    header_span.start_byte() + u64::try_from(pattern.find(name).unwrap()).unwrap();
                let leaf_span = SourceSpan::new(
                    fixture.snapshot.file(),
                    start,
                    start + u64::try_from(name.len()).unwrap(),
                )
                .unwrap();
                let matches = document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity.kind == EntityKind::Parameter
                            && entity.canonical_name == name
                            && entity.container == Some(ContainerRef::Entity(callback.id))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(matches.len(), 1, "{head}: {name}");
                let parameter = matches[0];
                assert_eq!(
                    parameter.evidence.source.as_ref().unwrap().span(),
                    leaf_span
                );
                let definitions = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target
                                == (OccurrenceTarget::Resolved {
                                    symbol: parameter.id,
                                })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(definitions.len(), 1);
                assert_eq!(
                    definitions[0].source,
                    *parameter.evidence.source.as_ref().unwrap()
                );
                let signatures = document
                    .extensions
                    .iter()
                    .filter(|envelope| {
                        envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                            let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                            lexical.kind() == LexicalEvidenceKind::Signature
                                && lexical.subject() == FactRef::Entity(parameter.id)
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(signatures.len(), 1);
                let header_source = SourceRef::new(
                    fixture.source.repository(),
                    fixture.source.generation(),
                    header_span,
                    fixture.source.content_hash(),
                    None,
                );
                let lexical = validate_lexical_evidence_envelope(
                    signatures[0],
                    FactRef::Entity(parameter.id),
                    &header_source,
                    parameter.provenance,
                )
                .unwrap();
                assert_eq!(lexical.text(), pattern);
                assert!(!lexical.is_truncated());
                lexical.verify_source_text(pattern).unwrap();
                parameters.insert(name, parameter.id);
            }
            let body_start = text.find(&format!("-> {body}")).unwrap() + 3;
            let body_end = body_start + body.len();
            let reads = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.source.span().start_byte() >= u64::try_from(body_start).unwrap()
                        && site.source.span().end_byte() <= u64::try_from(body_end).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), body.split(" + ").count());
            for read in reads {
                let span = read.source.span();
                let name = text
                    .get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    )
                    .unwrap();
                let target = parameters.get(name).copied().unwrap_or(outer_input.id);
                assert_eq!(read.enclosing, Some(callback.id));
                assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: target });
                assert_eq!(read.source.repository(), fixture.source.repository());
                assert_eq!(read.source.generation(), fixture.source.generation());
                assert_eq!(read.source.content_hash(), fixture.source.content_hash());
                let edges = document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.predicate == RelationPredicate::RefersTo
                            && edge.subject == RelationEndpoint::Occurrence(read.id)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].object, RelationEndpoint::Entity(target));
                assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
            }
            assert!(!document.relations.iter().any(|edge| matches!(
                edge.predicate,
                RelationPredicate::Calls | RelationPredicate::DispatchCandidate
            )));
        }
    }
}

#[test]
fn unsupported_lambda_patterns_and_malformed_heads_cannot_reveal_outer_targets() {
    for text in [
        "module Barriers\nlet seed = 1\nlet outer input =\n    let callback = fun (seed, other) -> seed + input\n    input\n",
        "module Barriers\nlet seed = 1\nlet outer input =\n    let callback = fun (Some seed) -> seed + input\n    input\n",
        "module Barriers\nlet seed = 1\nlet outer input = fun seed ->\n",
    ] {
        let fixture = Fixture::new("fsharp", text);
        let output = fixture.analyze();
        let document = output.document();
        let start = text.find("fun ").unwrap();
        let lambda_end = text
            .get(start..)
            .unwrap()
            .find("\n    input\n")
            .map_or(text.len(), |offset| start + offset);
        for read in document.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                && site.source.span().end_byte() <= u64::try_from(lambda_end).unwrap()
        }) {
            assert!(
                matches!(read.target, OccurrenceTarget::Unresolved { .. }),
                "{text}"
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(read.id)),
                "{text}"
            );
        }
        assert!(
            !document.relations.iter().any(|edge| matches!(
                edge.predicate,
                RelationPredicate::Calls | RelationPredicate::DispatchCandidate
            )),
            "{text}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| matches!(
                gap.reason,
                SkippedRegionReason::UnsupportedConstruct | SkippedRegionReason::ParseError
            )),
            "{text}"
        );
    }
}

#[test]
fn local_module_function_bindings_keep_exact_heads_and_captured_reads() {
    let fixture = Fixture::new(
        "fsharp",
        "module Locals\nlet seed = 3\nlet outer (input: int) =\n    let cached = input\n    let add (extra: int) = cached + extra\n    add seed\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let document = output.document();
    let outer = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "outer")
        .unwrap();
    for (name, kind, declaration) in [
        ("cached", EntityKind::Variable, "cached = input"),
        (
            "add",
            EntityKind::Function,
            "add (extra: int) = cached + extra",
        ),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing ordinary local binding: {name}"));
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.container, Some(ContainerRef::Entity(outer.id)));
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().span(),
            fixture.span(declaration)
        );
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0].source.span(),
            lexical_site_span(&fixture, declaration, name)
        );
    }
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "cached",
        "input",
        Some("outer"),
        "cached = input",
        "input",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "add",
        "cached",
        None,
        "cached + extra",
        "cached",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "add",
        "extra",
        Some("add"),
        "cached + extra",
        "extra",
    );
    assert_exact_lexical_reference(&fixture, &output, "outer", "add", None, "add seed", "add");
    assert_exact_lexical_reference(&fixture, &output, "outer", "seed", None, "add seed", "seed");
    let add = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "add")
        .unwrap();
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(outer.id));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(add.id));
    assert_eq!(
        calls[0].evidence.source.as_ref().unwrap().span(),
        fixture.span("add seed")
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
}

#[test]
fn local_module_function_nonrecursive_initializers_keep_outer_formals_and_unit_calls() {
    let fixture = Fixture::new(
        "fsharp",
        "module Locals\nlet outer input =\n    let input = input + 1\n    let read () = input\n    read ()\nlet after input = input\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let document = output.document();
    let read = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "read")
        .expect("ordinary local unit function must have its exact authored identity");
    let outer = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "outer")
        .unwrap();
    assert_eq!(read.container, Some(ContainerRef::Entity(outer.id)));
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter
                && entity.container == Some(ContainerRef::Entity(read.id)))
    );
    let local = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "input")
        .unwrap();
    let formal = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "input"
                && entity.container == Some(ContainerRef::Entity(outer.id))
        })
        .unwrap();
    let initializer = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span() == lexical_site_span(&fixture, "input = input + 1", "input")
        })
        .unwrap();
    assert_eq!(initializer.enclosing, Some(local.id));
    assert_eq!(
        initializer.target,
        OccurrenceTarget::Resolved { symbol: formal.id }
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(initializer.id)
                && edge.object == RelationEndpoint::Entity(formal.id)
                && edge.evidence.source.as_ref() == Some(&initializer.source))
            .count(),
        1
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "read",
        "input",
        None,
        "read () = input",
        "input",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "outer",
        "read",
        None,
        "    read ()",
        "read",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "after",
        "input",
        Some("after"),
        "after input = input",
        "input",
    );
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(outer.id));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(read.id));
    assert_eq!(
        calls[0].evidence.source.as_ref().unwrap().span(),
        lexical_site_span(&fixture, "    read ()", "read ()")
    );
}

#[test]
fn local_module_function_scopes_separate_repeated_heads_without_body_or_offset_identity() {
    let text = "module Locals\nlet outer seed =\n    let item = seed\n    let before = item\n    let item = before + 1\n    item + before\n";
    let fixture = Fixture::new("fsharp", text);
    let output = fixture.analyze();
    let document = output.document();
    let outer = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "outer")
        .unwrap();
    let before = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "before")
        .unwrap();
    let mut items = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "item")
        .collect::<Vec<_>>();
    items.sort_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte());
    assert_eq!(items.len(), 2);
    assert_ne!(items[0].id, items[1].id);
    for item in &items {
        assert_eq!(item.container, Some(ContainerRef::Entity(outer.id)));
    }
    for (written, name, owner, target) in [
        ("before = item", "item", before.id, items[0].id),
        ("item + before", "item", outer.id, items[1].id),
        ("item + before", "before", outer.id, before.id),
    ] {
        let read = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span() == lexical_site_span(&fixture, written, name)
            })
            .unwrap();
        assert_eq!(read.enclosing, Some(owner));
        assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: target });
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
                    && edge.object == RelationEndpoint::Entity(target)
                    && edge.evidence.source.as_ref() == Some(&read.source))
                .count(),
            1
        );
    }
    let edited = Fixture::new("fsharp", &text.replace("before + 1", "before + 222"));
    let edited_output = edited.analyze();
    let identities = |output: &AnalysisOutput| {
        let mut identities = output
            .document()
            .entities
            .iter()
            .map(|entity| {
                (
                    entity.canonical_name.clone(),
                    entity.kind,
                    entity.id,
                    entity.container,
                )
            })
            .collect::<Vec<_>>();
        identities.sort();
        identities
    };
    assert_eq!(identities(&output), identities(&edited_output));
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "ambiguous-symbol-identity")
    );
}

#[test]
fn local_module_function_explicit_continuations_keep_outer_reads_and_calls() {
    let fixture = Fixture::new(
        "fsharp",
        "module Locals\nlet outer input = let value = input in let add extra = value + extra in add input\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "value",
        "input",
        Some("outer"),
        "value = input",
        "input",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "add",
        "value",
        None,
        "value + extra",
        "value",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "add",
        "extra",
        Some("add"),
        "value + extra",
        "extra",
    );
    assert_exact_lexical_reference(&fixture, &output, "outer", "add", None, "add input", "add");
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "outer",
        "input",
        Some("outer"),
        "add input",
        "input",
    );
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
}

#[test]
fn local_module_function_nested_functions_capture_each_exact_outer_formal() {
    let fixture = Fixture::new(
        "fsharp",
        "module Locals\nlet outer input =\n    let parent x =\n        let child y = input + x + y\n        child x\n    parent input\n",
    );
    let output = fixture.analyze();
    for (name, parameter_owner) in [("input", "outer"), ("x", "parent"), ("y", "child")] {
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "child",
            name,
            Some(parameter_owner),
            "input + x + y",
            name,
        );
    }
    assert_exact_lexical_reference(
        &fixture, &output, "parent", "child", None, "child x", "child",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "parent",
        "x",
        Some("parent"),
        "child x",
        "x",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "outer",
        "parent",
        None,
        "parent input",
        "parent",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "outer",
        "input",
        Some("outer"),
        "parent input",
        "input",
    );
    let document = output.document();
    let owner = |name: &str| {
        document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap()
    };
    assert_eq!(
        owner("parent").container,
        Some(ContainerRef::Entity(owner("outer").id))
    );
    assert_eq!(
        owner("child").container,
        Some(ContainerRef::Entity(owner("parent").id))
    );
    for (consumer, target, written) in [
        ("parent", "child", "child x"),
        ("outer", "parent", "parent input"),
    ] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Entity(owner(consumer).id)
                    && edge.object == RelationEndpoint::Entity(owner(target).id)
                    && edge.evidence.source.as_ref().unwrap().span() == fixture.span(written))
                .count(),
            1
        );
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        2
    );
}

#[test]
fn local_module_function_optional_scope_proof_loss_preserves_authored_identities() {
    let fixture = Fixture::new(
        "fsharp",
        "module Locals\nlet outer input =\n    let value = input\n    let add extra = value + extra\n    add input\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert_eq!(
        full.document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    for label in [
        "fsharp.local_binding.expression",
        "fsharp.local_group.expression",
        "fsharp.local_continuation.expression",
    ] {
        let facts = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() != label)
            .cloned()
            .collect();
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert_eq!(
            clipped.document().entities,
            full.document().entities,
            "{label}"
        );
        let definitions = |output: &AnalysisOutput| {
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition)
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(definitions(&clipped), definitions(&full), "{label}");
        assert!(
            !clipped
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{label}"
        );
        assert_exact_lexical_reference(
            &fixture,
            &clipped,
            "add",
            "extra",
            Some("add"),
            "value + extra",
            "extra",
        );
    }
}

#[test]
fn recursive_module_groups_resolve_siblings_and_prior_module_values() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recursion\nlet seed = 1\nlet rec first value = second value + seed\nand second value = first value\nlet after input = first input\n",
    );
    let output = fixture.analyze();
    for (owner, target, target_owner, written, name) in [
        ("first", "second", None, "= second value + seed", "second"),
        (
            "first",
            "value",
            Some("first"),
            "= second value + seed",
            "value",
        ),
        ("first", "seed", None, "= second value + seed", "seed"),
        ("second", "first", None, "= first value", "first"),
        ("second", "value", Some("second"), "= first value", "value"),
        ("after", "first", None, "= first input", "first"),
    ] {
        assert_exact_lexical_reference(
            &fixture,
            &output,
            owner,
            target,
            target_owner,
            written,
            name,
        );
    }
    let document = output.document();
    let function = |name| {
        document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
            .unwrap()
            .id
    };
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 3);
    for (owner, target, written, application) in [
        ("first", "second", "= second value + seed", "second value"),
        ("second", "first", "= first value", "first value"),
        ("after", "first", "= first input", "first input"),
    ] {
        let span = lexical_site_span(&fixture, written, application);
        let matching = calls
            .iter()
            .filter(|edge| edge.evidence.source.as_ref().unwrap().span() == span)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(
            matching[0].subject,
            RelationEndpoint::Entity(function(owner))
        );
        assert_eq!(
            matching[0].object,
            RelationEndpoint::Entity(function(target))
        );
    }
}

#[test]
fn recursive_module_groups_keep_unit_slots_and_own_formal_precedence() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recursion\nlet seed = 1\nlet rec first () = second seed ()\nand second (first: int) () = first\nlet after () = first ()\n",
    );
    let output = fixture.analyze();
    for (owner, target, target_owner, written, name) in [
        ("first", "second", None, "= second seed ()", "second"),
        ("first", "seed", None, "= second seed ()", "seed"),
        ("second", "first", Some("second"), "= first\n", "first"),
        ("after", "first", None, "= first ()", "first"),
    ] {
        assert_exact_lexical_reference(
            &fixture,
            &output,
            owner,
            target,
            target_owner,
            written,
            name,
        );
    }
    let document = output.document();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        1
    );
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    let function = |name| {
        document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
            .unwrap()
            .id
    };
    for (owner, target, written, application) in [
        ("first", "second", "= second seed ()", "second seed ()"),
        ("after", "first", "= first ()", "first ()"),
    ] {
        let span = lexical_site_span(&fixture, written, application);
        let matching = calls
            .iter()
            .filter(|edge| edge.evidence.source.as_ref().unwrap().span() == span)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(
            matching[0].subject,
            RelationEndpoint::Entity(function(owner))
        );
        assert_eq!(
            matching[0].object,
            RelationEndpoint::Entity(function(target))
        );
    }
}

#[test]
fn recursive_module_groups_keep_forward_reads_and_formal_dispatch_closed() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recursion\nlet before input = first input\nlet rec first second = second 1\nand second value = first value\nlet after input = second input\n",
    );
    let output = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "first",
        "second",
        Some("first"),
        "= second 1",
        "second",
    );
    let document = output.document();
    let before = lexical_site_span(&fixture, "= first input", "first");
    let read = document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == before)
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for forbidden in [
        lexical_site_span(&fixture, "= first input", "first input"),
        lexical_site_span(&fixture, "= second 1", "second 1"),
    ] {
        assert!(
            !calls
                .iter()
                .any(|edge| edge.evidence.source.as_ref().unwrap().span() == forbidden)
        );
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations
                && gap.source.span() == forbidden
                && gap.detail == "fsharp-direct-call-target-unavailable"
        }));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
}

#[test]
fn recursive_module_groups_use_tuple_slot_arity_and_latest_heads() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recursion\nlet second value = value\nlet rec first value = second value\nand second (left, right) = left\nlet after input = second input\n",
    );
    let output = fixture.analyze();
    let document = output.document();
    let definition = lexical_site_span(&fixture, "and second (left, right)", "second");
    let target = document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Definition && site.source.span() == definition)
        .unwrap();
    let OccurrenceTarget::Resolved { symbol } = target.target else {
        panic!("the written recursive function head must have an exact identity");
    };
    for (written, name) in [
        ("first value = second value", "second"),
        ("= second input", "second"),
    ] {
        let span = lexical_site_span(&fixture, written, name);
        let site = document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
            .unwrap();
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(symbol));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
    }
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (caller, written, application) in [
        ("first", "first value = second value", "second value"),
        ("after", "= second input", "second input"),
    ] {
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == caller)
            .unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(symbol)
                        && edge.evidence.source.as_ref().unwrap().span()
                            == lexical_site_span(&fixture, written, application)
                })
                .count(),
            1,
            "{caller}"
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| { edge.predicate == RelationPredicate::DispatchCandidate })
    );
}

#[test]
fn recursive_module_witness_loss_preserves_definitions_and_independent_formals() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recursion\nlet rec first value = second value\nand second value = first value\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    assert_eq!(
        full.document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        2
    );
    for (label, duplicate) in [
        ("fsharp.recursive_group.expression", false),
        ("fsharp.recursive_prefix.expression", false),
        ("fsharp.recursive_member.expression", false),
        ("fsharp.recursive_separator.expression", false),
        ("fsharp.recursive_group.expression", true),
        ("fsharp.recursive_member.expression", true),
    ] {
        let mut facts = parsed.facts().to_vec();
        let index = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == label)
            .unwrap();
        if duplicate {
            let slot = &facts[index];
            let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
            facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                id,
                slot.parent(),
                slot.kind(),
                slot.span(),
                slot.depth(),
                slot.syntax_kind().clone(),
            ));
        } else {
            facts.remove(index);
        }
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert_eq!(
            clipped.document().entities,
            full.document().entities,
            "{label}"
        );
        let definitions = |output: &AnalysisOutput| {
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition)
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(definitions(&clipped), definitions(&full), "{label}");
        for (owner, written) in [("first", "= second value"), ("second", "= first value")] {
            assert_exact_lexical_reference(
                &fixture,
                &clipped,
                owner,
                "value",
                Some(owner),
                written,
                "value",
            );
        }
        assert!(
            !clipped
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{label}"
        );
    }
}

#[test]
fn own_formals_shadow_module_bindings_without_claiming_dispatch() {
    let fixture = Fixture::new(
        "fsharp",
        "module Shadowing\nlet helper input = input\nlet invoke helper argument = helper argument\n",
    );
    let output = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "invoke",
        "helper",
        Some("invoke"),
        "helper argument\n",
        "helper",
    );
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "invoke",
        "argument",
        Some("invoke"),
        "helper argument\n",
        "argument",
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn partial_overapplied_and_variable_callees_keep_reads_without_exact_calls() {
    for (prefix, body, name) in [
        ("", "helper argument", "helper"),
        ("", "helper argument 1 2", "helper"),
        ("let alias = helper\n", "alias argument 1", "alias"),
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!(
                "module Dispatch\nlet helper left right = left\n{prefix}let invoke argument = {body}\n"
            ),
        );
        let output = fixture.analyze();
        assert_exact_lexical_reference(&fixture, &output, "invoke", name, None, body, name);
        assert!(!output.document().relations.iter().any(|relation| matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.source.span() == fixture.span(body)
                    && gap.detail == "fsharp-direct-call-target-unavailable")
        );
    }
}

#[test]
fn lexical_lookup_does_not_cross_sibling_module_boundaries() {
    let fixture = Fixture::new(
        "fsharp",
        "module Root\nmodule Left =\n    let value = 1\n    let leftRead argument = value\nmodule Right =\n    let rightRead argument = value\n",
    );
    let output = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "leftRead",
        "value",
        None,
        "leftRead argument = value",
        "value",
    );
    let span = lexical_site_span(&fixture, "rightRead argument = value", "value");
    let read = output
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(read.id))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations && gap.source == read.source)
    );
}

#[test]
fn nonrecursive_lookup_respects_declaration_order_and_initializer_shadowing() {
    let fixture = Fixture::new(
        "fsharp",
        "module Order\nlet before argument = later argument\nlet later argument = argument\nlet after argument = later argument\n",
    );
    let output = fixture.analyze();
    let span = lexical_site_span(&fixture, "before argument = later argument", "later");
    let read = output
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "after",
        "later",
        None,
        "after argument = later argument",
        "later",
    );
    let after = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "after")
        .unwrap();
    let calls = output
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(after.id));

    let fixture = Fixture::new("fsharp", "module Initializers\nlet value = value\n");
    let output = fixture.analyze();
    let declarations = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "value" && entity.kind == EntityKind::Variable)
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 1);
    let current = declarations[0];
    let span = lexical_site_span(&fixture, "value = value", "value");
    let read = output
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(read.enclosing, Some(current.id));
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(read.id))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations && gap.source == read.source)
    );
}

#[test]
fn unsupported_pattern_binders_cannot_reveal_outer_module_targets() {
    for (text, site) in [
        (
            "module Barriers\nlet seed = 1\nlet choose seed (left, Some right) = seed\n",
            "choose seed (left, Some right) = seed",
        ),
        (
            "module Barriers\nlet seed = 1\nlet (seed, other) = (2, 3)\nlet choose argument = seed\n",
            "choose argument = seed",
        ),
    ] {
        let fixture = Fixture::new("fsharp", text);
        let output = fixture.analyze();
        let span = lexical_site_span(&fixture, site, "seed");
        let read = output
            .document()
            .occurrences
            .iter()
            .find(|read| read.role == OccurrenceRole::Reference && read.source.span() == span)
            .unwrap();
        assert!(
            matches!(read.target, OccurrenceTarget::Unresolved { .. }),
            "{text}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::RefersTo
                    && relation.subject == RelationEndpoint::Occurrence(read.id)),
            "{text}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct),
            "{text}"
        );
    }
}

#[test]
fn match_scrutinee_and_sibling_binders_keep_exact_isolated_targets() {
    let fixture = Fixture::new(
        "fsharp",
        "module Branches\nlet item = 11\nlet choose input =\n    let result =\n        match input with\n        | item -> item\n        | item -> item\n        | _ -> item\n    result + item\nlet outside input = item\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let scopes = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "fsharp.match_branch.scope")
        .collect::<Vec<_>>();
    assert_eq!(scopes.len(), 3);
    for label in [
        "fsharp.match_branch_header.expression",
        "fsharp.match_branch_pattern.expression",
        "fsharp.match_branch_body.expression",
    ] {
        let markers = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == label)
            .collect::<Vec<_>>();
        assert_eq!(markers.len(), 3, "{label}");
        for marker in markers {
            assert!(
                scopes
                    .iter()
                    .any(|scope| Some(scope.local_id()) == marker.parent()),
                "{label}"
            );
        }
    }
    let output = fixture.analyze();
    let document = output.document();
    let branch_names = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.canonical_name == "item"
                && entity.evidence.source.as_ref().is_some_and(|source| {
                    source.span().end_byte() - source.span().start_byte() == 4
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(branch_names.len(), 2);
    assert_ne!(branch_names[0].id, branch_names[1].id);
    let branch_reads = fixture
        .text
        .match_indices("item -> item")
        .map(|(start, _)| {
            (
                SourceSpan::new(
                    fixture.snapshot.file(),
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + 4).unwrap(),
                )
                .unwrap(),
                SourceSpan::new(
                    fixture.snapshot.file(),
                    u64::try_from(start + 8).unwrap(),
                    u64::try_from(start + 12).unwrap(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    for (definition, body) in branch_reads {
        let entity = branch_names
            .iter()
            .find(|entity| entity.evidence.source.as_ref().unwrap().span() == definition)
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == body)
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: entity.id }
        );
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    && site.source.span() == definition)
                .count(),
            1
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(entity.id)
                    && edge.evidence.source.as_ref() == Some(&site.source))
                .count(),
            1
        );
    }
    assert_exact_lexical_reference(
        &fixture,
        &output,
        "result",
        "input",
        Some("choose"),
        "match input with",
        "input",
    );
    let outer_item = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Variable
                && entity.canonical_name == "item"
                && entity.evidence.source.as_ref().unwrap().span().start_byte()
                    == fixture.span("item = 11").start_byte()
        })
        .unwrap();
    for written in ["| _ -> item", "result + item", "outside input = item"] {
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span() == lexical_site_span(&fixture, written, "item")
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved {
                symbol: outer_item.id
            },
            "{written}"
        );
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        2
    );
}

#[test]
fn match_nested_locals_lambdas_and_tuple_patterns_preserve_branch_captures() {
    let fixture = Fixture::new(
        "fsharp",
        "module Branches\nlet choose input = match input with | (item, _) -> let copy = item in (fun value -> copy + item + value) | _ -> (fun value -> value)\n",
    );
    let output = fixture.analyze();
    let document = output.document();
    let branch = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "item")
        .unwrap();
    let copy = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "copy")
        .unwrap();
    for (written, name, target) in [
        ("copy = item", "item", branch.id),
        ("copy + item + value", "item", branch.id),
        ("copy + item + value", "copy", copy.id),
    ] {
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span() == lexical_site_span(&fixture, written, name)
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target },
            "{written}:{name}"
        );
    }
    let parameters = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "item" && entity.kind == EntityKind::Parameter)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn unsupported_match_patterns_guards_and_shadow_qualifiers_stay_closed() {
    use rootlight_ir::FSharpBinding;
    for pattern in [
        "Some item",
        "Item",
        "``item``",
        "[item]",
        "(item: int)",
        "item when item > 0",
    ] {
        let text = format!(
            "module Branches\nlet item = 11\nlet choose input = match input with | {pattern} -> item\n"
        );
        let fixture = Fixture::new("fsharp", &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{pattern}"
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "fsharp.match_branch.scope")
                .count(),
            1
        );
        assert!(
            !parsed
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "fsharp.match_branch_header.expression"),
            "{pattern}"
        );
        let output = fixture.analyze();
        assert_exact_lexical_reference(
            &fixture,
            &output,
            "choose",
            "input",
            Some("choose"),
            "match input with",
            "input",
        );
        let body_span = lexical_site_span(&fixture, "-> item", "item");
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && site.source.span() == body_span),
            "{pattern}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.source.span().start_byte() <= body_span.start_byte()
                    && gap.source.span().end_byte() >= body_span.end_byte()),
            "{pattern}"
        );
    }
    let fixture = Fixture::new(
        "fsharp",
        "module Branches\nmodule library =\n    let carry input = input\nlet choose input = match input with | library -> library.carry input\n",
    );
    let output = fixture.analyze();
    assert!(
        !signature_mask_claims(&output)
            .iter()
            .any(|(_, claim)| matches!(
                claim,
                FSharpBinding::Reference { .. } | FSharpBinding::Call { .. }
            ))
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn match_optional_witness_loss_duplicates_and_tampering_cannot_reveal_outer_homonyms() {
    let fixture = Fixture::new(
        "fsharp",
        "module Branches\nlet item = 11\nlet choose input = match input with | item -> item\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    let full = fixture.analyze();
    let body = lexical_site_span(&fixture, "item -> item", "item");
    for label in [
        "fsharp.match_branch_header.expression",
        "fsharp.match_branch_pattern.expression",
        "fsharp.match_branch_body.expression",
    ] {
        for mutation in 0..4 {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label)
                .unwrap();
            let original = facts[index].clone();
            match mutation {
                0 => {
                    facts.remove(index);
                }
                1 => {
                    let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
                    facts.push(rootlight_adapter_sdk::SyntaxFact::new(
                        id,
                        original.parent(),
                        original.kind(),
                        original.span(),
                        original.depth(),
                        original.syntax_kind().clone(),
                    ));
                }
                2 | 3 => {
                    let parent = if mutation == 3 {
                        Some(
                            facts
                                .iter()
                                .find(|fact| fact.syntax_kind().as_str() == "fsharp.file.scope")
                                .unwrap()
                                .local_id(),
                        )
                    } else {
                        original.parent()
                    };
                    let span = if mutation == 2 {
                        fixture.span("item -> item")
                    } else {
                        original.span()
                    };
                    facts[index] = rootlight_adapter_sdk::SyntaxFact::new(
                        original.local_id(),
                        parent,
                        original.kind(),
                        span,
                        original.depth(),
                        original.syntax_kind().clone(),
                    );
                }
                _ => unreachable!(),
            }
            let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                provider().capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let replay = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
            assert_eq!(
                replay.document().entities,
                full.document().entities,
                "{label}:{mutation}"
            );
            let site = replay
                .document()
                .occurrences
                .iter()
                .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == body)
                .unwrap();
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{label}:{mutation}"
            );
            assert!(
                !replay
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(site.id)),
                "{label}:{mutation}"
            );
            assert_exact_lexical_reference(
                &fixture,
                &replay,
                "choose",
                "input",
                Some("choose"),
                "match input with",
                "input",
            );
        }
    }
    let mut facts = parsed.facts().to_vec();
    let index = facts
        .iter()
        .position(|fact| fact.syntax_kind().as_str() == "fsharp.match_branch_body.expression")
        .unwrap();
    let original = facts[index].clone();
    // Moving a witness outside its mandatory parent invalidates the provider
    // graph; it is distinct from a contained optional-proof failure.
    facts[index] = rootlight_adapter_sdk::SyntaxFact::new(
        original.local_id(),
        original.parent(),
        original.kind(),
        fixture.span("match input with"),
        original.depth(),
        original.syntax_kind().clone(),
    );
    let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
        provider().capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    assert!(
        matches!(fixture.try_analyze_with_parser(Arc::new(parser), IrLimits::default()), Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-lowering-parent")
    );
    let branch = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "fsharp.match_branch.scope")
        .unwrap();
    let binder = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.syntax_kind().as_str() == "fsharp.variable.declaration"
                && fact.parent() == Some(branch.local_id())
        })
        .unwrap();
    let facts = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.local_id() != binder.local_id() && fact.parent() != Some(binder.local_id())
        })
        .cloned()
        .collect();
    let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
        provider().capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let replay = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
    let site = replay
        .document()
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Reference && site.source.span() == body)
        .unwrap();
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    let facts = parsed
        .facts()
        .iter()
        .filter(|fact| fact.local_id() != branch.local_id())
        .cloned()
        .collect();
    let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
        provider().capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    assert!(
        matches!(fixture.try_analyze_with_parser(Arc::new(parser), IrLimits::default()), Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-lowering-parent")
    );
}

#[test]
fn match_binder_definitions_and_reads_preserve_bom_and_utf16_geometry() {
    for newline in ["\n", "\r\n"] {
        let text =
            "// neutral λ\nmodule Branches\nlet choose input = match input with | (δ, _) -> δ\n"
                .replace('\n', newline);
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            let mut bytes = match encoding {
                "utf-8" => vec![0xef, 0xbb, 0xbf],
                "utf-16le" => vec![0xff, 0xfe],
                "utf-16be" => vec![0xfe, 0xff],
                _ => unreachable!(),
            };
            if encoding == "utf-8" {
                bytes.extend_from_slice(text.as_bytes());
            } else {
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
            }
            let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
            fs::write(directory.path().join("Sample.fs"), &bytes).unwrap();
            let repository = derive_repository(b"fsharp-match-encoded-contract").id();
            let root = RepositoryRoot::open(repository, directory.path()).unwrap();
            let snapshot = root
                .snapshot(
                    &RelativePath::parse(Path::new("Sample.fs")).unwrap(),
                    16 * 1024,
                )
                .unwrap();
            let source = SourceRef::new(
                repository,
                GenerationId::from_bytes([19; 20]),
                SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
                snapshot.content_hash(),
                None,
            );
            let limits = limits(4096);
            let analyzer = TreeSitterAnalyzer::new(
                provider(),
                rootlight_ir::ProducerIdentity::new(
                    "fsharp-match-encoded-contract",
                    "1.0",
                    content_hash(b"fsharp-match-encoded-config"),
                )
                .unwrap(),
                LanguageId::new("fsharp").unwrap(),
                "tree-sitter-fsharp-0.3.12",
                content_hash(b"fsharp-match-encoded-binary"),
            )
            .unwrap();
            let request = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
                LanguageId::new("fsharp").unwrap(),
                EncodingId::new(encoding).unwrap(),
                Vec::new(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"fsharp-match-encoded-build")),
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
            validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default())
                .unwrap();
            let physical = |start: usize, length: usize| {
                let offset = if encoding == "utf-8" {
                    3 + start
                } else {
                    2 + 2 * text[..start].encode_utf16().count()
                };
                let length = if encoding == "utf-8" {
                    length
                } else {
                    2 * text[start..start + length].encode_utf16().count()
                };
                SourceSpan::new(
                    snapshot.file(),
                    u64::try_from(offset).unwrap(),
                    u64::try_from(offset + length).unwrap(),
                )
                .unwrap()
            };
            let binder = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "δ")
                .unwrap();
            assert_eq!(
                binder.evidence.source.as_ref().unwrap().span(),
                physical(text.find('δ').unwrap(), "δ".len()),
                "{encoding}"
            );
            let read = output
                .document()
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.source.span() == physical(text.rfind('δ').unwrap(), "δ".len())
                })
                .unwrap();
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved { symbol: binder.id },
                "{encoding}"
            );
            assert_eq!(read.source.repository(), repository);
            assert_eq!(read.source.content_hash(), source.content_hash());
            assert_eq!(read.source.generation(), source.generation());
            assert_eq!(
                output
                    .document()
                    .occurrences
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: binder.id }))
                    .count(),
                1
            );
        }
    }
}

fn lexical_site_span(fixture: &Fixture, written: &str, name: &str) -> SourceSpan {
    let start = fixture.text.rfind(written).unwrap() + written.rfind(name).unwrap();
    SourceSpan::new(
        fixture.snapshot.file(),
        u64::try_from(start).unwrap(),
        u64::try_from(start + name.len()).unwrap(),
    )
    .unwrap()
}

fn assert_exact_lexical_reference(
    fixture: &Fixture,
    output: &AnalysisOutput,
    enclosing: &str,
    target: &str,
    target_owner: Option<&str>,
    written: &str,
    name: &str,
) {
    let document = output.document();
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == enclosing)
        .unwrap();
    let target_owner = target_owner.map(|name| {
        document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function)
            .unwrap()
            .id
    });
    let target = document
        .entities
        .iter()
        .find(|entity| {
            entity.canonical_name == target
                && target_owner.map_or(entity.kind != EntityKind::Parameter, |owner| {
                    entity.container == Some(ContainerRef::Entity(owner))
                })
        })
        .unwrap();
    let span = lexical_site_span(fixture, written, name);
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(read.enclosing, Some(owner.id));
    assert_eq!(read.source.repository(), fixture.source.repository());
    assert_eq!(read.source.generation(), fixture.source.generation());
    assert_eq!(read.source.content_hash(), fixture.source.content_hash());
    let edges = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(read.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(target.id));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
}

#[test]
fn simple_formal_reads_have_exact_local_targets_and_source_evidence() {
    for name in ["value", "``value.with spaces``", "δ"] {
        for newline in ["\n", "\r\n"] {
            let text =
                format!("module Formals\nlet identity {name} = {name}\n").replace('\n', newline);
            let fixture = Fixture::new("fsharp", &text);
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert_eq!(
                parsed
                    .facts()
                    .iter()
                    .filter(
                        |fact| fact.syntax_kind().as_str() == "fsharp.simple_parameters.signature"
                    )
                    .count(),
                1
            );
            let output = fixture.analyze();
            let document = output.document();
            let function = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "identity")
                .unwrap();
            let parameters = document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter)
                .collect::<Vec<_>>();
            assert_eq!(parameters.len(), 1);
            let parameter = parameters[0];
            assert_eq!(parameter.canonical_name, name);
            assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
            let reference = parameter.evidence.source.as_ref().unwrap();
            assert_eq!(reference.span(), fixture.span(name));
            let reads = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1);
            let read = reads[0];
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                }
            );
            assert_eq!(read.enclosing, Some(function.id));
            let start = u64::try_from(text.rfind(name).unwrap()).unwrap();
            assert_eq!(
                read.source.span(),
                SourceSpan::new(
                    fixture.snapshot.file(),
                    start,
                    start + u64::try_from(name.len()).unwrap()
                )
                .unwrap()
            );
            let edges = document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .collect::<Vec<_>>();
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].subject, RelationEndpoint::Occurrence(read.id));
            assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter.id));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
            for source in [reference, &read.source] {
                assert_eq!(source.repository(), fixture.source.repository());
                assert_eq!(source.generation(), fixture.source.generation());
                assert_eq!(source.content_hash(), fixture.source.content_hash());
            }
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Calls)
            );
        }
    }
}

#[test]
fn parenthesized_single_formals_have_exact_local_targets_and_source_evidence() {
    for name in ["value", "``value.with spaces``", "δ"] {
        for formal in [format!("({name})"), format!("({name}: int)")] {
            for newline in ["\n", "\r\n"] {
                let text = format!("module Formals\nlet identity {formal} = {name}\n")
                    .replace('\n', newline);
                assert_exact_parenthesized_formals(&text, "identity", &[name]);
            }
        }
    }
}

#[test]
fn mixed_bare_and_typed_single_formals_keep_distinct_exact_targets() {
    for newline in ["\n", "\r\n"] {
        let text = "module Formals\nlet choose x (typed: int) = x + typed\n".replace('\n', newline);
        assert_exact_parenthesized_formals(&text, "choose", &["x", "typed"]);
    }
}

fn assert_exact_parenthesized_formals(text: &str, function_name: &str, names: &[&str]) {
    let fixture = Fixture::new("fsharp", text);
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| { fact.syntax_kind().as_str() == "fsharp.simple_parameters.signature" })
            .count(),
        1
    );
    let output = fixture.analyze();
    let document = output.document();
    let function = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function && entity.canonical_name == function_name
        })
        .unwrap();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| { entity.kind == EntityKind::Parameter })
            .count(),
        names.len()
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| { site.role == OccurrenceRole::Reference })
            .count(),
        names.len()
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| { relation.predicate == RelationPredicate::RefersTo })
            .count(),
        names.len()
    );
    for name in names {
        let expected_definition = fixture.span(name);
        let parameter_fact = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "fsharp.parameter.declaration"
                    && fact.span() == expected_definition
            })
            .unwrap();
        let definition_facts = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "fsharp.name.definition"
                    && fact.parent() == Some(parameter_fact.local_id())
                    && fact.span() == expected_definition
            })
            .collect::<Vec<_>>();
        assert_eq!(definition_facts.len(), 1);
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == *name)
            .unwrap();
        assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
        let parameter_source = parameter.evidence.source.as_ref().unwrap();
        assert_eq!(parameter_source.span(), expected_definition);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].source, *parameter_source);
        let start = u64::try_from(text.rfind(*name).unwrap()).unwrap();
        let expected_read = SourceSpan::new(
            fixture.snapshot.file(),
            start,
            start + u64::try_from(name.len()).unwrap(),
        )
        .unwrap();
        let read = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == expected_read
            })
            .unwrap();
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        assert_eq!(read.enclosing, Some(function.id));
        let relations = document
            .relations
            .iter()
            .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
        assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
        assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
        for source in [parameter_source, &definitions[0].source, &read.source] {
            assert_eq!(source.repository(), fixture.source.repository());
            assert_eq!(source.generation(), fixture.source.generation());
            assert_eq!(source.span().file(), fixture.snapshot.file());
            assert_eq!(source.content_hash(), fixture.source.content_hash());
        }
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "fsharp-formal-binding-target-unavailable" && gap.source == read.source
        }));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| { relation.predicate == RelationPredicate::Calls })
    );
}

#[test]
fn recursive_heads_resolve_only_their_own_same_named_formals() {
    let fixture = Fixture::new(
        "fsharp",
        "module Rec\nlet rec first x = x + 1\nand second x = x + 2\n",
    );
    let output = fixture.analyze();
    let document = output.document();
    let parameters = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 2);
    for read in reads {
        let OccurrenceTarget::Resolved { symbol } = read.target else {
            panic!("Unresolved own formal")
        };
        let parameter = parameters
            .iter()
            .find(|parameter| parameter.id == symbol)
            .unwrap();
        assert_eq!(
            parameter.container,
            read.enclosing.map(ContainerRef::Entity)
        );
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::RefersTo
                    && relation.subject == RelationEndpoint::Occurrence(read.id)
                    && relation.object == RelationEndpoint::Entity(symbol))
        );
    }
}

#[test]
fn qualified_lowercase_union_case_does_not_replace_a_quoted_formal() {
    let text = "module Qualified\n[<RequireQualifiedAccess>]\ntype Choice = | ``value``\nlet identity ``value`` = ``value``\n";
    let fixture = Fixture::new("fsharp", text);
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let output = fixture.analyze();
    let document = output.document();
    let parameters = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter && entity.canonical_name == "``value``"
        })
        .collect::<Vec<_>>();
    let constructors = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Constructor && entity.canonical_name == "``value``"
        })
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), 1);
    assert_eq!(constructors.len(), 1);
    assert_ne!(parameters[0].id, constructors[0].id);
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameters[0].id
        }
    );
    assert_eq!(
        parameters[0].container,
        read.enclosing.map(ContainerRef::Entity)
    );
    let start = u64::try_from(text.rfind("``value``").unwrap()).unwrap();
    assert_eq!(
        read.source.span(),
        SourceSpan::new(fixture.snapshot.file(), start, start + 9).unwrap()
    );
    let relations = document
        .relations
        .iter()
        .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(
        relations[0].object,
        RelationEndpoint::Entity(parameters[0].id)
    );
    assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
    assert_eq!(read.source.generation(), fixture.source.generation());
    assert_eq!(read.source.content_hash(), fixture.source.content_hash());
}

#[test]
fn unreviewed_binders_never_expose_an_outer_formal() {
    for body in [
        "fun (x, other) -> x",
        "match x with | Some x -> x | None -> 0",
        "for x in [1; 2] do ignore x",
        "seq { for x in [1; 2] do yield x }",
        "\n    let rec local x = local x\n    local x",
    ] {
        for newline in ["\n", "\r\n"] {
            let body = body.replace('\n', newline);
            let fixture = Fixture::new(
                "fsharp",
                &format!("module Barriers{newline}let outer x = {body}{newline}"),
            );
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete,
                "{body}"
            );
            let output = fixture.analyze();
            let document = output.document();
            let body_span = fixture.span(body.trim_start());
            let witnesses = parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "fsharp.unsupported_body.expression"
                        && fact.span() == body_span
                })
                .collect::<Vec<_>>();
            assert_eq!(witnesses.len(), 1, "{body}");
            assert_eq!(witnesses[0].kind(), SyntaxFactKind::Signature);
            assert_exact_unsupported_body_gap(&fixture, &output, "outer", body_span);
            let outer = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "outer")
                .unwrap();
            let parameter = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.container == Some(ContainerRef::Entity(outer.id))
                })
                .unwrap();
            let outer_reads = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role != OccurrenceRole::Definition
                        && site.target
                            == (OccurrenceTarget::Resolved {
                                symbol: parameter.id,
                            })
                })
                .collect::<Vec<_>>();
            if body.starts_with("match ") {
                assert_eq!(outer_reads.len(), 1, "{body}");
                assert_eq!(outer_reads[0].role, OccurrenceRole::Reference);
                assert_eq!(
                    outer_reads[0].source.span(),
                    lexical_site_span(&fixture, "match x with", "x")
                );
            } else {
                assert!(outer_reads.is_empty(), "{body}");
            }
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Calls)
            );
            assert_ne!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
        }
    }
}

#[test]
fn direct_formal_and_primitive_bodies_do_not_claim_unknown_call_work() {
    for body in ["δ", "42"] {
        for newline in ["\n", "\r\n"] {
            let fixture = Fixture::new(
                "fsharp",
                &format!("module Proven{newline}let carry δ = {body}{newline}"),
            );
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(parsed.diagnostics().is_empty());
            assert!(!parsed.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "fsharp.unsupported_body.expression"
            }));
            let output = fixture.analyze();
            let document = output.document();
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| { gap.domain == FactDomain::Relations })
            );
            let relations = output
                .report()
                .coverage()
                .domains()
                .iter()
                .find(|domain| domain.domain() == FactDomain::Relations)
                .unwrap();
            assert_eq!(relations.status(), CoverageStatus::Complete);
            assert_eq!(relations.skipped(), 0);
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| { relation.predicate == RelationPredicate::RefersTo })
                    .count(),
                usize::from(body == "δ")
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| { relation.predicate == RelationPredicate::Calls })
            );
            if body == "δ" {
                assert_exact_parameter_header(&fixture, &output, "carry", "δ", "δ");
            }
        }
    }
}

#[test]
fn unsupported_body_gaps_do_not_contaminate_proven_sibling_formal_reads() {
    for (body, resolved_reads) in [("fun (δ, other) -> δ", 0), ("δ + 1", 1), ("Other.δ", 0)] {
        for newline in ["\n", "\r\n"] {
            let fixture = Fixture::new(
                "fsharp",
                &format!(
                    "module Siblings{newline}let proven δ = δ{newline}let uncertain δ = {body}{newline}",
                ),
            );
            let parsed = fixture.parse(4096, &deadline()).unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(parsed.diagnostics().is_empty());
            let body_span = fixture.span(body);
            let witnesses = parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "fsharp.unsupported_body.expression")
                .collect::<Vec<_>>();
            assert_eq!(witnesses.len(), 1);
            assert_eq!(witnesses[0].span(), body_span);
            let output = fixture.analyze();
            assert_exact_parameter_header(&fixture, &output, "proven", "δ", "δ");
            assert_exact_unsupported_body_gap(&fixture, &output, "uncertain", body_span);
            let document = output.document();
            let proven = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "proven"
                })
                .unwrap();
            assert!(
                proven.evidence.source.as_ref().unwrap().span().end_byte()
                    <= body_span.start_byte()
            );
            let uncertain = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "uncertain"
                })
                .unwrap();
            let parameter = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.container == Some(ContainerRef::Entity(uncertain.id))
                })
                .unwrap();
            let reads = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.target
                            == (OccurrenceTarget::Resolved {
                                symbol: parameter.id,
                            })
                })
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), resolved_reads);
            for read in reads {
                assert_eq!(read.enclosing, Some(uncertain.id));
                assert!(body_span.start_byte() <= read.source.span().start_byte());
                assert!(read.source.span().end_byte() <= body_span.end_byte());
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.predicate == RelationPredicate::RefersTo
                                && relation.subject == RelationEndpoint::Occurrence(read.id)
                                && relation.object == RelationEndpoint::Entity(parameter.id)
                                && relation.evidence.source.as_ref() == Some(&read.source)
                        })
                        .count(),
                    1
                );
            }
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| { gap.domain == FactDomain::Relations })
                    .count(),
                1 + usize::from(body == "Other.δ")
            );
            let qualified_gaps = document
                .skipped_regions
                .iter()
                .filter(|gap| gap.detail == "fsharp-qualified-target-unavailable")
                .collect::<Vec<_>>();
            assert_eq!(qualified_gaps.len(), usize::from(body == "Other.δ"));
            if let Some(gap) = qualified_gaps.first() {
                assert_eq!(gap.source.span(), body_span);
                assert_eq!(gap.domain, FactDomain::Relations);
                let reads = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.syntax_kind == "fsharp.qualified_body_read.reference"
                            && site.source.span() == body_span
                    })
                    .collect::<Vec<_>>();
                assert_eq!(reads.len(), 1);
                assert!(matches!(
                    reads[0].target,
                    OccurrenceTarget::Unresolved { .. }
                ));
            }
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| { relation.predicate == RelationPredicate::Calls })
            );
            assert_ne!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
        }
    }
}

#[test]
fn applying_formals_retains_reads_without_claiming_callable_relations() {
    for newline in ["\n", "\r\n"] {
        let fixture = Fixture::new(
            "fsharp",
            &format!("module Application{newline}let invoke fn δ = fn δ{newline}",),
        );
        let start = fixture.text.rfind("fn δ").unwrap();
        let body = SourceSpan::new(
            fixture.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + "fn δ".len()).unwrap(),
        )
        .unwrap();
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "fsharp.unsupported_body.expression"
                        && fact.span() == body
                })
                .count(),
            1
        );
        let output = fixture.analyze();
        assert_exact_parameter_header(&fixture, &output, "invoke", "fn", "fn");
        assert_exact_parameter_header(&fixture, &output, "invoke", "δ", "δ");
        assert_exact_unsupported_body_gap(&fixture, &output, "invoke", body);
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|relation| { relation.predicate == RelationPredicate::RefersTo })
                .count(),
            2
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| { relation.predicate == RelationPredicate::Calls })
        );
    }
}

#[test]
fn clipped_unsupported_body_witness_preserves_identity_and_bounded_coverage() {
    let fixture = Fixture::new("fsharp", "module Bounded\nlet carry δ = δ + 1\n");
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let is_mandatory = |fact: &rootlight_adapter_sdk::SyntaxFact| {
        !fact.syntax_kind().as_str().ends_with(".expression")
            && !fact.syntax_kind().as_str().ends_with(".reference")
    };
    let mandatory_count = parsed
        .facts()
        .iter()
        .filter(|fact| is_mandatory(fact))
        .count();
    let maximum_records = mandatory_count + 1;
    assert!(maximum_records < parsed.facts().len());
    let base = limits(4096);
    let original = base.syntax_stream();
    let batch = original.batch();
    let syntax = StreamLimits::new(
        original.max_batches(),
        maximum_records,
        original.max_output_bytes(),
        original.max_diagnostics(),
        original.max_diagnostic_bytes(),
        original.max_string_bytes(),
        BatchThresholds::new(
            batch.max_records().min(maximum_records),
            batch.max_output_bytes(),
            batch.max_diagnostics(),
            batch.max_diagnostic_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let bounded_limits = AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        syntax,
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap();
    let clipped = fixture
        .parse_with_limits(&bounded_limits, &deadline())
        .unwrap();
    assert_eq!(
        clipped.report().coverage().status(),
        CoverageStatus::Bounded
    );
    assert_eq!(
        clipped.report().resources().output_records(),
        maximum_records
    );
    assert!(
        !clipped
            .facts()
            .iter()
            .any(|fact| { fact.syntax_kind().as_str() == "fsharp.unsupported_body.expression" })
    );
    let diagnostics = clipped
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code().as_str() == "syntax-extraction-limit")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].source(), Some(&fixture.source));
    assert_eq!(diagnostics[0].coverage_effect(), CoverageStatus::Bounded);
    let mandatory_shape = |facts: &[rootlight_adapter_sdk::SyntaxFact]| {
        facts
            .iter()
            .filter(|fact| is_mandatory(fact))
            .map(|fact| {
                let parent = fact.parent().map(|id| {
                    let owner = facts.iter().find(|owner| owner.local_id() == id).unwrap();
                    (owner.syntax_kind().as_str().to_owned(), owner.span())
                });
                (
                    fact.kind(),
                    fact.syntax_kind().as_str().to_owned(),
                    fact.span(),
                    fact.depth(),
                    parent,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        mandatory_shape(clipped.facts()),
        mandatory_shape(parsed.facts())
    );
    let full = fixture.analyze();
    assert_exact_unsupported_body_gap(&fixture, &full, "carry", fixture.span("δ + 1"));
    let bounded = fixture
        .try_analyze_with_limits(provider(), &bounded_limits)
        .unwrap();
    let document = bounded.document();
    assert_eq!(document.files, full.document().files);
    assert_eq!(document.entities, full.document().entities);
    let definitions = |output: &AnalysisOutput| {
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(definitions(&bounded), definitions(&full));
    let structural_relations = |output: &AnalysisOutput| {
        output
            .document()
            .relations
            .iter()
            .filter(|relation| {
                !matches!(
                    relation.predicate,
                    RelationPredicate::RefersTo | RelationPredicate::Calls
                )
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(structural_relations(&bounded), structural_relations(&full));
    assert_eq!(
        bounded.report().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(
        bounded
            .report()
            .coverage()
            .domains()
            .iter()
            .all(|domain| { domain.status() != CoverageStatus::Complete })
    );
    let relations = bounded
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == FactDomain::Relations)
        .unwrap();
    assert_eq!(relations.status(), CoverageStatus::Bounded);
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == FactDomain::Diagnostics
                    && gap.reason == SkippedRegionReason::ResourceLimit
                    && gap.detail == "syntax-extraction-limit"
                    && gap.source == fixture.source
                    && gap.evidence.source.as_ref() == Some(&fixture.source)
            })
            .count(),
        1
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "fsharp-body-binding-and-call-analysis-unavailable" })
    );
}

#[test]
fn native_attributes_keep_headers_and_formal_reads_with_exact_partiality() {
    for newline in ["\n", "\r\n"] {
        let header = format!("[<AutoOpen>]{newline}module Sample");
        let fixture = Fixture::new(
            "fsharp",
            &format!("{header}{newline}let carry δ = δ{newline}",),
        );
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(parsed.diagnostics().is_empty());
        let attribute = fixture.span("AutoOpen");
        let witnesses = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "fsharp.unsupported_attribute.expression")
            .collect::<Vec<_>>();
        assert_eq!(witnesses.len(), 1);
        assert_eq!(witnesses[0].kind(), SyntaxFactKind::Signature);
        assert_eq!(witnesses[0].span(), attribute);
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "fsharp.module.signature"
                        && fact.span() == fixture.span(&header)
                })
                .count(),
            1
        );
        let output = fixture.analyze();
        let document = output.document();
        assert!(document.diagnostics.is_empty());
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Sample")
            .unwrap();
        let source = SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            attribute,
            fixture.source.content_hash(),
            None,
        );
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "fsharp-attribute-binding-analysis-unavailable")
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 2);
        for domain in [FactDomain::Occurrences, FactDomain::Relations] {
            let matching = gaps
                .iter()
                .filter(|gap| gap.domain == domain)
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            let gap = matching[0];
            assert_eq!(gap.repository, source.repository());
            assert_eq!(gap.generation, source.generation());
            assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
            assert_eq!(gap.source, source);
            assert_eq!(gap.evidence.source.as_ref(), Some(&source));
            assert_eq!(gap.provenance, module.provenance);
            let coverage = document
                .coverage_records
                .iter()
                .find(|coverage| {
                    coverage.domain == domain
                        && coverage.scope
                            == rootlight_ir::CoverageScope::File(fixture.snapshot.file())
                })
                .unwrap();
            assert_ne!(coverage.status, CoverageStatus::Complete);
            assert_eq!(coverage.skipped, 1);
            let reported = output
                .report()
                .coverage()
                .domains()
                .iter()
                .find(|entry| entry.domain() == domain)
                .unwrap();
            assert_eq!(reported.status(), coverage.status);
            assert_eq!(reported.skipped(), 1);
        }
        let signatures = document
            .extensions
            .iter()
            .filter(|envelope| {
                envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                    let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                    lexical.kind() == LexicalEvidenceKind::Signature
                        && lexical.subject() == FactRef::Entity(module.id)
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1);
        let header_source = SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            fixture.span(&header),
            fixture.source.content_hash(),
            None,
        );
        let lexical = validate_lexical_evidence_envelope(
            signatures[0],
            FactRef::Entity(module.id),
            &header_source,
            module.provenance,
        )
        .unwrap();
        assert_eq!(lexical.format(), LexicalEvidenceFormat::SourceText);
        assert_eq!(lexical.text(), header);
        assert_eq!(
            lexical.complete_text_hash(),
            content_hash(header.as_bytes())
        );
        assert!(!lexical.is_truncated());
        lexical.verify_source_text(&header).unwrap();
        assert_exact_parameter_header(&fixture, &output, "carry", "δ", "δ");
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let start = fixture.text.rfind('δ').unwrap();
        let read_source = SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            SourceSpan::new(
                fixture.snapshot.file(),
                u64::try_from(start).unwrap(),
                u64::try_from(start + "δ".len()).unwrap(),
            )
            .unwrap(),
            fixture.source.content_hash(),
            None,
        );
        assert_eq!(reads[0].source, read_source);
        assert_eq!(reads[0].provenance, module.provenance);
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| { relation.predicate == RelationPredicate::RefersTo })
                .count(),
            1
        );
        assert!(
            document
                .relations
                .iter()
                .filter(|relation| { relation.predicate == RelationPredicate::RefersTo })
                .all(|relation| relation.provenance == reads[0].provenance)
        );
        assert_eq!(
            output.report().coverage().total_source_bytes(),
            fixture.text.len()
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            fixture.text.len()
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| { relation.predicate == RelationPredicate::Calls })
        );
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
    }
}

fn assert_exact_unsupported_body_gap(
    fixture: &Fixture,
    output: &AnalysisOutput,
    function: &str,
    body: SourceSpan,
) {
    let document = output.document();
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == function)
        .unwrap();
    let owner_span = owner.evidence.source.as_ref().unwrap().span();
    assert!(owner_span.start_byte() <= body.start_byte());
    assert!(body.end_byte() <= owner_span.end_byte());
    let source = SourceRef::new(
        fixture.source.repository(),
        fixture.source.generation(),
        body,
        fixture.source.content_hash(),
        None,
    );
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.detail == "fsharp-body-binding-and-call-analysis-unavailable"
                && gap.source == source
        })
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].repository, source.repository());
    assert_eq!(gaps[0].generation, source.generation());
    assert_eq!(gaps[0].domain, FactDomain::Relations);
    assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
    assert_eq!(gaps[0].evidence.source.as_ref(), Some(&source));
    assert_eq!(gaps[0].provenance, owner.provenance);
    let coverage = document
        .coverage_records
        .iter()
        .find(|coverage| {
            coverage.domain == FactDomain::Relations
                && coverage.scope == rootlight_ir::CoverageScope::File(fixture.snapshot.file())
        })
        .unwrap();
    assert_eq!(coverage.repository, source.repository());
    assert_eq!(coverage.generation, source.generation());
    assert_eq!(coverage.evidence.source.as_ref(), Some(&fixture.source));
    assert_ne!(coverage.status, CoverageStatus::Complete);
    assert_eq!(
        coverage.skipped,
        u64::try_from(
            document
                .skipped_regions
                .iter()
                .filter(|gap| gap.domain == FactDomain::Relations)
                .count()
        )
        .unwrap()
    );
    let reported = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|coverage| coverage.domain() == FactDomain::Relations)
        .unwrap();
    assert_eq!(reported.status(), coverage.status);
    assert_eq!(
        reported.skipped(),
        usize::try_from(coverage.skipped).unwrap()
    );
}

#[test]
fn unsupported_formal_patterns_cannot_enable_a_partial_binding_plan() {
    for formals in [
        "x (left, Some right)",
        "x ((typed: int))",
        "x (Some item)",
        "x [item]",
        "x { Field = item }",
        "x Some",
        "x _",
    ] {
        let fixture = Fixture::new(
            "fsharp",
            &format!("module Patterns\nlet choose {formals} = x\n"),
        );
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{formals}"
        );
        assert!(
            !parsed
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "fsharp.simple_parameters.signature"),
            "{formals}"
        );
        let output = fixture.analyze();
        let document = output.document();
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::RefersTo),
            "{formals}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.source.span() == fixture.span(formals)),
            "{formals}"
        );
    }
}

#[test]
fn unbound_body_reads_keep_exact_scoped_relationship_gaps() {
    let fixture = Fixture::new("fsharp", "module Missing\nlet choose x = missing\n");
    let output = fixture.analyze();
    let document = output.document();
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert!(matches!(
        reads[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(reads[0].source.span(), fixture.span("missing"));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations
                && gap.detail == "fsharp-formal-binding-target-unavailable"
                && gap.source == reads[0].source)
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::RefersTo)
    );
}

#[test]
fn duplicate_single_formals_keep_exact_scoped_relationship_gaps() {
    for formals in [
        "x (x: int)",
        "x(x: int)",
        "(x: int) (x: int)",
        "(x: int) (x: string)",
    ] {
        let text = format!("module Duplicates\nlet choose {formals} = x\n");
        let fixture = Fixture::new("fsharp", &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "fsharp.simple_parameters.signature")
                .count(),
            1
        );
        let mut clipped_facts = parsed.facts().to_vec();
        let first_header = clipped_facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == "fsharp.parameter_header.expression")
            .unwrap();
        assert_eq!(
            clipped_facts
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "fsharp.parameter_header.expression"
                })
                .count(),
            2
        );
        clipped_facts.remove(first_header);
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            clipped_facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let output = fixture.analyze();
        let replay = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert_eq!(replay.document().entities, output.document().entities);
        assert_eq!(replay.document().occurrences, output.document().occurrences);
        assert_eq!(replay.document().relations, output.document().relations);
        for output in [&output, &replay] {
            let document = output.document();
            let reads = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1);
            let read = reads[0];
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            let start = u64::try_from(text.rfind('x').unwrap()).unwrap();
            assert_eq!(
                read.source.span(),
                SourceSpan::new(fixture.snapshot.file(), start, start + 1).unwrap()
            );
            assert_eq!(read.source.repository(), fixture.source.repository());
            assert_eq!(read.source.generation(), fixture.source.generation());
            assert_eq!(read.source.content_hash(), fixture.source.content_hash());
            assert!(document.skipped_regions.iter().any(|gap| gap.detail
                == "fsharp-formal-binding-target-unavailable"
                && gap.source == read.source));
            assert!(!document.relations.iter().any(|relation| matches!(
                relation.predicate,
                RelationPredicate::RefersTo | RelationPredicate::Calls
            )));
            let parameter_definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && matches!(site.target, OccurrenceTarget::Resolved { symbol }
                        if document.entities.iter().any(|entity| {
                            entity.id == symbol && entity.kind == EntityKind::Parameter
                        }))
                })
                .collect::<Vec<_>>();
            assert_eq!(parameter_definitions.len(), 2);
            for definition in parameter_definitions {
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .filter(|gap| {
                            gap.domain == FactDomain::Extensions
                                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                                && gap.detail == "signature-capture-unavailable"
                                && gap.source == definition.source
                        })
                        .count(),
                    1
                );
            }
            assert!(!document.extensions.iter().any(|envelope| {
                envelope.namespace == LEXICAL_EXTENSION_NAMESPACE && {
                    let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
                    lexical.kind() == LexicalEvidenceKind::Signature
                        && document.entities.iter().any(|entity| {
                            entity.kind == EntityKind::Parameter
                                && lexical.subject() == FactRef::Entity(entity.id)
                        })
                }
            }));
        }
    }
}

#[test]
fn recovery_keeps_clean_local_formals_captures_and_saturated_calls() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recovery\nopen Missing.Library\n[<Measure>]\ntype rate = / duration /\nlet moduleCarry value = value\nlet outer seed =\n    let cached = seed\n    let inner own = own + cached + seed\n    inner seed\nlet caller argument = moduleCarry argument\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        fixture.text.len()
    );
    let recovery = parsed
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
        .collect::<Vec<_>>();
    assert_eq!(recovery.len(), 1);
    assert_eq!(parsed.diagnostics().len(), 1);
    let recovery_source = recovery[0].source().unwrap();
    assert!(recovery_source.span().end_byte() <= fixture.span("outer seed").start_byte());
    let output = fixture.analyze();
    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert!(output.document().skipped_regions.iter().any(|gap| {
        gap.reason == SkippedRegionReason::ParseError
            && gap.detail == "syntax-error-recovery"
            && gap.source == *recovery_source
    }));
    for (owner, target, target_owner, written, name) in [
        ("cached", "seed", Some("outer"), "cached = seed", "seed"),
        ("inner", "own", Some("inner"), "own + cached + seed", "own"),
        ("inner", "cached", None, "own + cached + seed", "cached"),
        (
            "inner",
            "seed",
            Some("outer"),
            "own + cached + seed",
            "seed",
        ),
        ("outer", "inner", None, "    inner seed", "inner"),
        ("outer", "seed", Some("outer"), "    inner seed", "seed"),
        (
            "caller",
            "argument",
            Some("caller"),
            "moduleCarry argument",
            "argument",
        ),
    ] {
        assert_exact_lexical_reference(
            &fixture,
            &output,
            owner,
            target,
            target_owner,
            written,
            name,
        );
    }
    let document = output.document();
    let entity = |name: &str| {
        document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap()
    };
    assert_eq!(
        entity("inner").container,
        Some(ContainerRef::Entity(entity("outer").id))
    );
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].subject,
        RelationEndpoint::Entity(entity("outer").id)
    );
    assert_eq!(
        calls[0].object,
        RelationEndpoint::Entity(entity("inner").id)
    );
    let call_source = calls[0].evidence.source.as_ref().unwrap();
    assert_eq!(
        call_source.span(),
        lexical_site_span(&fixture, "    inner seed", "inner seed")
    );
    assert_eq!(call_source.repository(), fixture.source.repository());
    assert_eq!(call_source.generation(), fixture.source.generation());
    assert_eq!(call_source.content_hash(), fixture.source.content_hash());
    let module_read = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span()
                    == lexical_site_span(&fixture, "moduleCarry argument", "moduleCarry")
        })
        .collect::<Vec<_>>();
    assert_eq!(module_read.len(), 1);
    assert_eq!(module_read[0].enclosing, Some(entity("caller").id));
    assert_eq!(
        module_read[0].target,
        OccurrenceTarget::Unresolved {
            text_hash: content_hash(b"moduleCarry")
        }
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(module_read[0].id)
    }));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    assert!(
        !document
            .extensions
            .iter()
            .any(|envelope| { envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE })
    );
}

#[test]
fn recovery_missing_local_proof_keeps_own_formals_but_closes_captures_and_calls() {
    let fixture = Fixture::new(
        "fsharp",
        "module Recovery\n[<Measure>]\ntype rate = / duration /\nlet outer seed =\n    let cached = seed\n    let inner own = own + cached + seed\n    inner seed\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
    let full = fixture.analyze();
    let facts = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() != "fsharp.local_binding.expression")
        .cloned()
        .collect::<Vec<_>>();
    assert!(facts.len() < parsed.facts().len());
    let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
        provider().capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let clipped = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
    assert_eq!(
        clipped.report().coverage().status(),
        CoverageStatus::Unknown
    );
    assert_eq!(clipped.document().entities, full.document().entities);
    let definitions = |output: &AnalysisOutput| {
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(definitions(&clipped), definitions(&full));
    assert_exact_lexical_reference(
        &fixture,
        &clipped,
        "inner",
        "own",
        Some("inner"),
        "own + cached + seed",
        "own",
    );
    for name in ["cached", "seed"] {
        let reads = clipped
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span()
                        == lexical_site_span(&fixture, "own + cached + seed", name)
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0].target,
            OccurrenceTarget::Unresolved {
                text_hash: content_hash(name.as_bytes())
            }
        );
        assert!(!clipped.document().relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
        }));
    }
    assert!(!clipped.document().relations.iter().any(|edge| {
        matches!(
            edge.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )
    }));
    assert!(
        !clipped
            .document()
            .extensions
            .iter()
            .any(|envelope| { envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE })
    );
}

#[test]
fn recovery_extra_native_diagnostics_close_owned_binding_proofs() {
    use rootlight_adapter_sdk::{AdapterDiagnostic, CoverageReport, DiagnosticCode};
    use rootlight_ir::DiagnosticSeverity;

    let fixture = Fixture::new(
        "fsharp",
        "module Recovery\n[<Measure>]\ntype rate = / duration /\nlet outer seed =\n    let cached = seed\n    let inner own = own + cached + seed\n    inner seed\n",
    );
    let parsed = fixture.parse(4096, &deadline()).unwrap();
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].code().as_str(),
        "syntax-error-recovery"
    );
    let complete_partition = fixture.analyze();
    assert_exact_lexical_reference(
        &fixture,
        &complete_partition,
        "inner",
        "own",
        Some("inner"),
        "own + cached + seed",
        "own",
    );
    let definitions = |output: &AnalysisOutput| {
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .cloned()
            .collect::<Vec<_>>()
    };
    for code in [
        "syntax-extraction-limit",
        "syntax-node-limit",
        "syntax-depth-limit",
        "syntax-context-limit",
        "native-evidence-unavailable",
    ] {
        let mut diagnostics = parsed.diagnostics().to_vec();
        diagnostics.push(AdapterDiagnostic::new(
            DiagnosticCode::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            Some(fixture.source.clone()),
            CoverageStatus::Bounded,
        ));
        // Valid report accounting isolates the extra uncertainty from malformed SDK input.
        let coverage = CoverageReport::new(
            parsed.report().coverage().tier(),
            CoverageStatus::Unknown,
            fixture.text.len(),
            fixture.text.len(),
            diagnostics.len(),
            parsed.report().coverage().domains().to_vec(),
        )
        .unwrap();
        let parser = rootlight_adapter_sdk::testkit::MockParseProvider::new(
            provider().capabilities().clone(),
            parsed.facts().to_vec(),
            diagnostics,
            coverage,
        )
        .with_syntax_nodes(parsed.report().resources().syntax_nodes());
        let output = fixture.analyze_with_parser(Arc::new(parser), IrLimits::default());
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Unknown,
            "{code}"
        );
        assert_eq!(
            output.document().entities,
            complete_partition.document().entities,
            "{code}"
        );
        assert_eq!(
            definitions(&output),
            definitions(&complete_partition),
            "{code}"
        );
        let document = output.document();
        let inner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "inner")
            .unwrap();
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span()
                        == lexical_site_span(&fixture, "own + cached + seed", "own")
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "{code}");
        assert_eq!(reads[0].enclosing, Some(inner.id), "{code}");
        assert_eq!(
            reads[0].target,
            OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"own")
            },
            "{code}"
        );
        assert_eq!(reads[0].source.repository(), fixture.source.repository());
        assert_eq!(reads[0].source.generation(), fixture.source.generation());
        assert_eq!(
            reads[0].source.content_hash(),
            fixture.source.content_hash()
        );
        assert!(
            !document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
            }),
            "{code}"
        );
        assert!(
            !document.relations.iter().any(|edge| matches!(
                edge.predicate,
                RelationPredicate::Calls | RelationPredicate::DispatchCandidate
            )),
            "{code}"
        );
        assert!(
            !document
                .extensions
                .iter()
                .any(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE),
            "{code}"
        );
        assert!(
            document.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == code
                    && diagnostic.coverage_effect == CoverageStatus::Bounded
                    && diagnostic.source.as_ref() == Some(&fixture.source)
            }),
            "{code}"
        );
    }
}

#[test]
fn prepared_recovery_artifact_rebinds_clean_local_proofs_without_reparsing() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingParser {
        inner: Arc<TreeSitterProvider>,
        invocations: AtomicUsize,
    }

    impl ParseProvider for CountingParser {
        fn capabilities(&self) -> &rootlight_adapter_sdk::ParseCapabilities {
            self.inner.capabilities()
        }

        fn parse(
            &self,
            request: &ParseRequest<'_>,
            sink: &mut dyn rootlight_adapter_sdk::SyntaxFactSink,
            cancellation: &Cancellation,
        ) -> Result<rootlight_adapter_sdk::ParseReport, rootlight_adapter_sdk::AdapterError>
        {
            self.invocations.fetch_add(1, Ordering::Relaxed);
            self.inner.parse(request, sink, cancellation)
        }
    }

    let mut fixture = Fixture::new(
        "fsharp",
        "module Recovery\n[<Measure>]\ntype rate = / duration /\nlet outer seed =\n    let cached = seed\n    let inner own = own + cached + seed\n    inner seed\n",
    );
    let parser = Arc::new(CountingParser {
        inner: provider(),
        invocations: AtomicUsize::new(0),
    });
    let analyzer = TreeSitterAnalyzer::new(
        parser.clone(),
        rootlight_ir::ProducerIdentity::new(
            "fsharp-native-contract",
            "1.0",
            content_hash(b"fsharp-native-configuration"),
        )
        .unwrap(),
        fixture.language.clone(),
        "tree-sitter-fsharp-0.3.12",
        content_hash(b"fsharp-native-binary"),
    )
    .unwrap();
    let budget = limits(4096);
    let original_source = fixture.source.clone();
    let (initial, artifact) = {
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            fixture.language.clone(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"fsharp-native-build-context")),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let artifact = analyzer.prepare_artifact(&request, &deadline()).unwrap();
        assert!(artifact.syntax_partition_complete());
        let initial = analyzer
            .analyze_prepared_artifact(
                &request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(parser.invocations.load(Ordering::Relaxed), 1);
        assert_exact_lexical_reference(
            &fixture,
            &initial,
            "inner",
            "own",
            Some("inner"),
            "own + cached + seed",
            "own",
        );
        (initial, artifact)
    };
    fixture.source = SourceRef::new(
        original_source.repository(),
        GenerationId::from_bytes([12; 20]),
        original_source.span(),
        original_source.content_hash(),
        original_source.line_hint(),
    );
    assert_ne!(fixture.source.generation(), original_source.generation());
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        fixture.language.clone(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"fsharp-native-build-context")),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let reused = analyzer
        .analyze_prepared_artifact(
            &request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 1);
    let clean = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 2);
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
    assert_eq!(reused.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(
        initial
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<Vec<_>>(),
        reused
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<Vec<_>>()
    );
    for (owner, target, target_owner, written, name) in [
        ("cached", "seed", Some("outer"), "cached = seed", "seed"),
        ("inner", "own", Some("inner"), "own + cached + seed", "own"),
        ("inner", "cached", None, "own + cached + seed", "cached"),
        (
            "inner",
            "seed",
            Some("outer"),
            "own + cached + seed",
            "seed",
        ),
        ("outer", "inner", None, "    inner seed", "inner"),
        ("outer", "seed", Some("outer"), "    inner seed", "seed"),
    ] {
        assert_exact_lexical_reference(
            &fixture,
            &reused,
            owner,
            target,
            target_owner,
            written,
            name,
        );
    }
    let document = reused.document();
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    let entity = |name: &str| {
        document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap()
            .id
    };
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(entity("outer")));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(entity("inner")));
    assert_eq!(
        calls[0].evidence.source.as_ref().unwrap().span(),
        lexical_site_span(&fixture, "    inner seed", "inner seed")
    );
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    assert!(document.source_mappings.is_empty());
    for source in document
        .files
        .iter()
        .filter_map(|record| record.evidence.source.as_ref())
        .chain(
            document
                .entities
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
        .chain(document.occurrences.iter().map(|record| &record.source))
        .chain(
            document
                .occurrences
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
        .chain(
            document
                .relations
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
        .chain(
            document
                .coverage_records
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
        .chain(document.skipped_regions.iter().map(|record| &record.source))
        .chain(
            document
                .skipped_regions
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
        .chain(
            document
                .diagnostics
                .iter()
                .filter_map(|record| record.source.as_ref()),
        )
        .chain(
            document
                .diagnostics
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|record| record.evidence.source.as_ref()),
        )
    {
        assert_eq!(source.repository(), fixture.source.repository());
        assert_eq!(source.generation(), fixture.source.generation());
        assert_eq!(source.content_hash(), fixture.source.content_hash());
        assert_eq!(source.span().file(), fixture.source.span().file());
        assert!(source.span().end_byte() <= fixture.source.span().end_byte());
    }
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
}

#[test]
fn recovery_nested_module_keeps_boolean_pipeline_local_binding_proofs() {
    for (case, body, first_own, second_own) in [
        ("arithmetic", "own + cached + seed", "own + cached", None),
        (
            "boolean-pipeline",
            "own = seed || cached |> Set.contains own",
            "own = seed",
            Some("Set.contains own"),
        ),
    ] {
        let text = format!(
            "namespace Recovery\nmodule internal Operations =\n    [<Measure>]\n    type rate = / duration /\n    let outer seed =\n        let cached = seed\n        let inner own = {body}\n        inner seed\n"
        );
        let fixture = Fixture::new("fsharp", &text);
        let parsed = fixture.parse(4096, &deadline()).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Unknown,
            "{case}"
        );
        assert_eq!(
            parsed.report().coverage().covered_source_bytes(),
            text.len(),
            "{case}"
        );
        assert_eq!(
            parsed.diagnostics().len(),
            1,
            "{case}: {:?}",
            parsed.diagnostics()
        );
        assert_eq!(
            parsed.diagnostics()[0].code().as_str(),
            "syntax-error-recovery",
            "{case}"
        );
        let recovery_source = parsed.diagnostics()[0].source().unwrap();
        assert!(
            recovery_source.span().end_byte() <= fixture.span("outer seed").start_byte(),
            "{case}"
        );
        let output = fixture.analyze();
        let document = output.document();
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Unknown,
            "{case}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.reason == SkippedRegionReason::ParseError
                    && gap.detail == "syntax-error-recovery"
                    && gap.source == *recovery_source
            }),
            "{case}"
        );
        let owner = |name: &str| {
            let matches = document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "{case}: owner {name}");
            matches[0].id
        };
        let mut sites = vec![
            ("cached", "seed", Some("outer"), "cached = seed", "seed"),
            ("outer", "inner", None, "        inner seed", "inner"),
            ("outer", "seed", Some("outer"), "        inner seed", "seed"),
            ("inner", "own", Some("inner"), first_own, "own"),
            ("inner", "seed", Some("outer"), body, "seed"),
            ("inner", "cached", None, body, "cached"),
        ];
        if let Some(written) = second_own {
            sites.push(("inner", "own", Some("inner"), written, "own"));
        }
        for (enclosing, target_name, target_owner, written, name) in sites {
            let target_container = target_owner.map(owner);
            let targets = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.canonical_name == target_name
                        && target_container.map_or(entity.kind != EntityKind::Parameter, |parent| {
                            entity.kind == EntityKind::Parameter
                                && entity.container == Some(ContainerRef::Entity(parent))
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(
                targets.len(),
                1,
                "{case}: target {target_name} in {written}"
            );
            let span = lexical_site_span(&fixture, written, name);
            let reads = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1, "{case}: read {name} in {written}");
            let read = reads[0];
            assert_eq!(
                read.enclosing,
                Some(owner(enclosing)),
                "{case}: owner of {name} in {written}"
            );
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: targets[0].id
                },
                "{case}: target of {name} in {written}"
            );
            assert_eq!(
                read.source.repository(),
                fixture.source.repository(),
                "{case}: {written}"
            );
            assert_eq!(
                read.source.generation(),
                fixture.source.generation(),
                "{case}: {written}"
            );
            assert_eq!(
                read.source.content_hash(),
                fixture.source.content_hash(),
                "{case}: {written}"
            );
            let edges = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(read.id)
                })
                .collect::<Vec<_>>();
            assert_eq!(edges.len(), 1, "{case}: RefersTo for {name} in {written}");
            assert_eq!(
                edges[0].object,
                RelationEndpoint::Entity(targets[0].id),
                "{case}: RefersTo target for {name} in {written}"
            );
            assert_eq!(
                edges[0].evidence.source.as_ref(),
                Some(&read.source),
                "{case}: RefersTo source for {name} in {written}"
            );
        }
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == owner("inner"))
                .unwrap()
                .container,
            Some(ContainerRef::Entity(owner("outer"))),
            "{case}"
        );
        let calls = document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1, "{case}: saturated local call");
        assert_eq!(
            calls[0].subject,
            RelationEndpoint::Entity(owner("outer")),
            "{case}"
        );
        assert_eq!(
            calls[0].object,
            RelationEndpoint::Entity(owner("inner")),
            "{case}"
        );
        let source = calls[0].evidence.source.as_ref().unwrap();
        assert_eq!(
            source.span(),
            lexical_site_span(&fixture, "        inner seed", "inner seed"),
            "{case}"
        );
        assert_eq!(source.repository(), fixture.source.repository(), "{case}");
        assert_eq!(source.generation(), fixture.source.generation(), "{case}");
        assert_eq!(
            source.content_hash(),
            fixture.source.content_hash(),
            "{case}"
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate),
            "{case}"
        );
        assert!(
            !document
                .extensions
                .iter()
                .any(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE),
            "{case}: recovered module/import claims"
        );
    }
}
