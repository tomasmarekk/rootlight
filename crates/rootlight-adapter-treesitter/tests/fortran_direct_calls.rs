//! Original written Fortran calls require exact native module ownership.
//! These controls distinguish direct subroutines from imports, ambiguous names
//! and local procedure bindings without changing authored entity kinds.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, IncludedRange, LanguageId, MemoryAdmissionPolicy, ParseRequest,
    StreamLimits, execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, EntityRecord,
    ExtensionSupport, FactDomain, IrLimits, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, SkippedRegionReason, SourceRef, SourceSpan,
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
    encoding: EncodingId,
}

impl Fixture {
    fn new(text: &str, suffix: &str) -> Self {
        Self::new_at(text, suffix, "Sample")
    }

    fn new_at(text: &str, suffix: &str, stem: &str) -> Self {
        Self::new_physical(text, suffix, stem, EncodingId::utf8(), text.as_bytes())
    }

    fn new_physical(
        text: &str,
        suffix: &str,
        stem: &str,
        encoding: EncodingId,
        raw: &[u8],
    ) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let path = format!("{stem}{suffix}");
        fs::write(directory.path().join(&path), raw).unwrap();
        let repository = derive_repository(b"fortran-direct-call-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(&path)).unwrap(), 8192)
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([11; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _directory: directory,
            text: text.to_owned(),
            snapshot,
            source,
            encoding,
        }
    }

    fn offset(&self, projected: usize) -> u64 {
        let physical = if self.encoding.as_str() == "utf-8" {
            projected
        } else {
            self.text[..projected].encode_utf16().count() * 2
        };
        u64::try_from(physical).unwrap()
    }

    fn span(&self, written: &str, occurrence: usize) -> SourceSpan {
        let start = self.text.match_indices(written).nth(occurrence).unwrap().0;
        SourceSpan::new(
            self.snapshot.file(),
            self.offset(start),
            self.offset(start + written.len()),
        )
        .unwrap()
    }

    fn analyze(&self, clean_parse: bool) -> AnalysisOutput {
        self.analyze_with_ranges(clean_parse, Vec::new())
    }

    fn analyze_with_ranges(&self, clean_parse: bool, ranges: Vec<IncludedRange>) -> AnalysisOutput {
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
            self.encoding.clone(),
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
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            self.encoding.clone(),
            ranges,
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
                    if self.encoding.as_str() == "utf-8" {
                        content_hash(&self.snapshot.content()[range])
                    } else {
                        let decoded: String = char::decode_utf16(
                            self.snapshot.content()[range]
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|unit| {
                                    if self.encoding.as_str() == "utf-16le" {
                                        u16::from_le_bytes(*unit)
                                    } else {
                                        u16::from_be_bytes(*unit)
                                    }
                                }),
                        )
                        .collect::<Result<_, _>>()
                        .unwrap();
                        content_hash(decoded.as_bytes())
                    }
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

fn edge(
    document: &NormalizedIrDocument,
    predicate: RelationPredicate,
    from: &EntityRecord,
    to: &EntityRecord,
    witness: SourceSpan,
) {
    let found: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == predicate
                && relation.subject == RelationEndpoint::Entity(from.id)
                && relation.object == RelationEndpoint::Entity(to.id)
                && relation.evidence.source.as_ref().is_some_and(|source| {
                    source.span().file() == witness.file()
                        && source.span().start_byte() <= witness.start_byte()
                        && source.span().end_byte() >= witness.end_byte()
                })
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{predicate:?}: {} -> {}",
        from.canonical_name,
        to.canonical_name
    );
    let source = found[0].evidence.source.as_ref().unwrap();
    assert_eq!(source.repository(), from.repository);
    assert_eq!(source.generation(), from.generation);
    assert_eq!(
        source.content_hash(),
        from.evidence.source.as_ref().unwrap().content_hash()
    );
    assert_eq!(
        source.content_hash(),
        to.evidence.source.as_ref().unwrap().content_hash()
    );
}

fn assert_unresolved_calls(fixture: &Fixture, clean: bool) {
    let output = fixture.analyze(clean);
    let document = output.document();
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert!(
        !calls.is_empty(),
        "negative must retain a real written CALL"
    );
    for call in calls {
        assert!(
            matches!(call.target, OccurrenceTarget::Unresolved { .. }),
            "{call:?}"
        );
        assert!(document.skipped_regions.iter().any(|region| {
            region.source.span() == call.source.span()
                && region.domain == FactDomain::Relations
                && region.reason == SkippedRegionReason::UnsupportedConstruct
                && region.detail == "fortran-call-target-unavailable"
        }));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn original_direct_calls_bind_exact_written_subroutine_in_same_module() {
    const ORIGINAL: &str = include_str!("fixtures/fortran-direct-call/stdlib_array.f90");
    let fixture = Fixture::new(ORIGINAL, ".f90");
    assert_eq!(ORIGINAL.len(), 1981);
    assert_eq!(fixture.snapshot.content(), ORIGINAL.as_bytes());
    let output = fixture.analyze(true);
    let document = output.document();
    let module = definition(
        document,
        SourceSpan::new(fixture.snapshot.file(), 189, 201).unwrap(),
    );
    let first = definition(
        document,
        SourceSpan::new(fixture.snapshot.file(), 449, 456).unwrap(),
    );
    let second = definition(
        document,
        SourceSpan::new(fixture.snapshot.file(), 949, 957).unwrap(),
    );
    let target = definition(
        document,
        SourceSpan::new(fixture.snapshot.file(), 1362, 1372).unwrap(),
    );
    for (entity, written, kind, start, end) in [
        (module, "stdlib_array", EntityKind::Module, 189, 201),
        (first, "trueloc", EntityKind::Function, 449, 456),
        (second, "falseloc", EntityKind::Function, 949, 957),
        (target, "logicalloc", EntityKind::Function, 1362, 1372),
    ] {
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, written);
        assert_eq!(entity.display_name, written);
        let span = SourceSpan::new(fixture.snapshot.file(), start, end).unwrap();
        assert_eq!(definition(document, span).id, entity.id);
        if kind != EntityKind::Module {
            assert_eq!(entity.container, Some(ContainerRef::Entity(module.id)));
            edge(document, RelationPredicate::Contains, module, entity, span);
        }
    }
    for (caller, start, end) in [(first, 710, 720), (second, 1217, 1227)] {
        let witness = SourceSpan::new(fixture.snapshot.file(), start, end).unwrap();
        assert_eq!(
            &ORIGINAL.as_bytes()[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            b"logicalloc"
        );
        assert!(
            !document.occurrences.iter().any(
                |site| site.role == OccurrenceRole::Definition && site.source.span() == witness
            )
        );
        edge(document, RelationPredicate::Calls, caller, target, witness);
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.enclosing == Some(caller.id)
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(sites[0].syntactic_text_hash, content_hash(b"logicalloc"));
    }
    let literals: Vec<_> = ORIGINAL
        .match_indices("logicalloc")
        .map(|(start, _)| (start, start + 10))
        .collect();
    assert_eq!(
        literals,
        [(710, 720), (1217, 1227), (1362, 1372), (1942, 1952)]
    );
    assert_eq!(
        ORIGINAL
            .match_indices("logicalloc__absent_source_control__")
            .count(),
        0
    );
}

#[test]
fn case_insensitive_lookup_preserves_written_identity_and_module_partition() {
    let fixture = Fixture::new(
        "module First\nimplicit none\ncontains\nsubroutine Run()\ncall wOrK()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module First\nmodule Second\nimplicit none\ncontains\nsubroutine Other()\ncall WORK()\nend subroutine Other\nsubroutine work()\nend subroutine work\nend module Second\n",
        ".f90",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    let first = definition(document, fixture.span("Run", 0));
    let first_target = definition(document, fixture.span("Work", 0));
    let second = definition(document, fixture.span("Other", 0));
    let second_target = definition(document, fixture.span("work", 0));
    assert_eq!(first_target.canonical_name, "Work");
    assert_eq!(second_target.canonical_name, "work");
    edge(
        document,
        RelationPredicate::Calls,
        first,
        first_target,
        fixture.span("wOrK", 0),
    );
    edge(
        document,
        RelationPredicate::Calls,
        second,
        second_target,
        fixture.span("WORK", 0),
    );
    assert_ne!(first_target.id, second_target.id);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .count(),
        2
    );
}

#[test]
fn another_module_or_external_file_scope_does_not_supply_the_target() {
    for text in [
        "module First\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nend module First\nmodule Second\nimplicit none\ncontains\nsubroutine Work()\nend subroutine Work\nend module Second\n",
        "module First\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nend module First\nsubroutine Work()\nend subroutine Work\n",
    ] {
        assert_unresolved_calls(&Fixture::new(text, ".f90"), true);
    }
    let only_caller = Fixture::new(
        "module First\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nend module First\n",
        ".f90",
    );
    let only_target = Fixture::new_at(
        "module First\nimplicit none\ncontains\nsubroutine Work()\nend subroutine Work\nend module First\n",
        ".f90",
        "Target",
    );
    let target = only_target.analyze(true);
    assert_eq!(
        definition(target.document(), only_target.span("Work", 0)).kind,
        EntityKind::Function
    );
    assert_ne!(only_caller.snapshot.file(), only_target.snapshot.file());
    assert_unresolved_calls(&only_caller, true);
}

#[test]
fn ambiguous_case_folded_declarations_and_duplicate_modules_withhold_proof() {
    for text in [
        "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nsubroutine WORK()\nend subroutine WORK\nend module Sample\n",
        "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\nmodule SAMPLE\nimplicit none\nend module SAMPLE\n",
    ] {
        assert_unresolved_calls(&Fixture::new(text, ".f90"), true);
    }
}

#[test]
fn formal_result_local_and_statement_function_names_block_host_binding() {
    for caller in [
        "subroutine Run(Work)\ncall Work()\nend subroutine Run",
        "subroutine Run()\ninteger :: work\ncall Work()\nend subroutine Run",
        "function Run() result(Work)\ninteger :: Work\ncall Work()\nend function Run",
        "subroutine Run()\ninteger :: Work, X\nWork(X) = X\ncall Work()\nend subroutine Run",
    ] {
        let text = format!(
            "module Sample\nimplicit none\ncontains\n{caller}\nsubroutine Work()\nend subroutine Work\nend module Sample\n"
        );
        assert_unresolved_calls(&Fixture::new(&text, ".f90"), true);
    }
}

#[test]
fn use_interface_external_and_procedure_pointer_are_binding_barriers() {
    for declaration in [
        "use Foreign, only: Work",
        "external :: Work",
        "procedure(), pointer :: Work",
        "interface\nsubroutine Work()\nend subroutine Work\nend interface",
        "integer, external :: Work",
    ] {
        let text = format!(
            "module Sample\nimplicit none\ncontains\nsubroutine Run()\n{declaration}\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n"
        );
        assert_unresolved_calls(&Fixture::new(&text, ".f90"), true);
    }
    let imported_module = "module Sample\nuse Foreign\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n";
    assert_unresolved_calls(&Fixture::new(imported_module, ".f90"), true);
}

#[test]
fn type_bound_and_nested_or_block_scopes_do_not_reuse_outer_proof() {
    for caller in [
        "subroutine Run(Object)\nclass(*) :: Object\ncall Object%Work()\nend subroutine Run",
        "subroutine Run()\nblock\ncall Work()\nend block\nend subroutine Run",
        "subroutine Run()\ncall Work()\ncontains\nsubroutine Work()\nend subroutine Work\nend subroutine Run",
    ] {
        let text = format!(
            "module Sample\nimplicit none\ncontains\n{caller}\nsubroutine Work()\nend subroutine Work\nend module Sample\n"
        );
        let fixture = Fixture::new(&text, ".f90");
        let output = fixture.analyze(true);
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
    }
}

#[test]
fn recovered_preprocessed_and_mismatched_end_names_cannot_supply_a_certificate() {
    let base = "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n";
    for (text, clean) in [
        (base.replace("end module Sample", "end module Other"), true),
        (format!("#define SOMETHING 1\n{base}"), true),
        (
            format!("!$omp declare variant (Other) match(construct={{parallel}})\n{base}"),
            true,
        ),
        (
            base.replace(
                "end subroutine Work",
                "end subroutine Work\n!#: template remains physical",
            ),
            true,
        ),
        (
            base.replace("end subroutine Work", "end subroutine Work("),
            false,
        ),
    ] {
        assert_unresolved_calls(&Fixture::new(&text, ".f90"), clean);
    }
}

#[test]
fn crlf_continued_calls_keep_original_byte_witnesses() {
    let fixture = Fixture::new(
        "module Sample\r\nimplicit none\r\ncontains\r\nsubroutine Run()\r\ncall &\r\n & wOrK()\r\nend subroutine Run\r\nsubroutine Work()\r\nend subroutine Work\r\nend module Sample\r\n",
        ".f90",
    );
    let output = fixture.analyze(true);
    let document = output.document();
    edge(
        document,
        RelationPredicate::Calls,
        definition(document, fixture.span("Run", 0)),
        definition(document, fixture.span("Work", 0)),
        fixture.span("wOrK", 0),
    );
}

#[test]
fn explicit_full_file_partition_still_withholds_cross_partition_binding() {
    let fixture = Fixture::new(
        "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n",
        ".f90",
    );
    let output = fixture.analyze_with_ranges(
        true,
        vec![IncludedRange::new(
            fixture.source.span(),
            LanguageId::new("fortran").unwrap(),
        )],
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn functions_self_calls_and_target_binding_barriers_remain_unresolved() {
    for text in [
        "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\ninteger function Work()\nend function Work\nend module Sample\n",
        "module Sample\nimplicit none\ncontains\nrecursive subroutine Work()\ncall Work()\nend subroutine Work\nend module Sample\n",
        "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nuse Foreign\nend subroutine Work\nend module Sample\n",
        "module Sample\ninteger :: Work\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n",
    ] {
        assert_unresolved_calls(&Fixture::new(text, ".f90"), true);
    }
}

#[test]
fn stale_content_or_file_identity_is_rejected_before_native_call_analysis() {
    let fixture = Fixture::new("module Sample\nimplicit none\nend module Sample\n", ".f90");
    let other = Fixture::new_at(
        "module Other\nimplicit none\nend module Other\n",
        ".f90",
        "Other",
    );
    for source in [
        SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            fixture.source.span(),
            content_hash(b"stale source"),
            None,
        ),
        SourceRef::new(
            fixture.source.repository(),
            fixture.source.generation(),
            SourceSpan::new(
                other.snapshot.file(),
                0,
                u64::try_from(fixture.text.len()).unwrap(),
            )
            .unwrap(),
            fixture.source.content_hash(),
            None,
        ),
    ] {
        assert!(GenerationBoundSnapshot::new(&fixture.snapshot, &source).is_err());
    }
}

#[test]
fn utf16_call_bindings_keep_exact_physical_spans_and_original_source_hash() {
    let text = "\u{feff}! λ😀\nmodule Sample\nimplicit none\ncontains\nsubroutine Run()\ncall wOrK()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n";
    for encoding in ["utf-16le", "utf-16be"] {
        let raw: Vec<_> = text
            .encode_utf16()
            .flat_map(|unit| {
                if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        let fixture = Fixture::new_physical(
            text,
            ".f90",
            "Encoded",
            EncodingId::new(encoding).unwrap(),
            &raw,
        );
        let output = fixture.analyze(true);
        let document = output.document();
        let target = definition(document, fixture.span("Work", 0));
        let witness = fixture.span("wOrK", 0);
        edge(
            document,
            RelationPredicate::Calls,
            definition(document, fixture.span("Run", 0)),
            target,
            witness,
        );
        assert_eq!(fixture.snapshot.content(), raw);
        assert_eq!(fixture.source.content_hash(), content_hash(&raw));
        let start = usize::try_from(witness.start_byte()).unwrap();
        let end = usize::try_from(witness.end_byte()).unwrap();
        let expected: Vec<_> = "wOrK"
            .encode_utf16()
            .flat_map(|unit| {
                if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        assert_eq!(&raw[start..end], expected);
    }
}
