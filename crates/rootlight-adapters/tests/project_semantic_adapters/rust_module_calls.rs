//! Behavioral checks for authored Rust module and explicit import call paths.
//! Native syntax and immutable source evidence must agree before a project
//! call receives a foreign free-function target; uncertain namespaces stay bounded.

use super::*;
use rootlight_ir::FactRef;

#[test]
fn written_module_calls_authenticate_each_file_and_definition() {
    for call in [
        "crate::dep::ping",
        "dep::ping",
        "selected",
        "self::dep::ping",
    ] {
        let source = format!(
            "mod dep; use crate::dep::ping as selected; fn run() {{ let label = \"extern\"; {call}(); }}"
        );
        let fixture = ProjectFixture::new(
            ["src/main.rs", "src/dep.rs"],
            [&source, "pub fn ping() {}"],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let call = document
            .occurrences
            .iter()
            .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "ping" && entity.kind == EntityKind::Function)
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{source}"
        );
        assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
        assert!(
            call.evidence
                .derivation
                .contains(&FactRef::Entity(target.id))
        );
        for file in &document.files {
            assert!(call.evidence.derivation.contains(&FactRef::File(file.id)));
            assert!(
                call.evidence
                    .derivation
                    .contains(&FactRef::Fact(file.provenance))
            );
        }
        let relation = document
            .relations
            .iter()
            .find(|relation| relation.predicate == RelationPredicate::Calls)
            .unwrap();
        assert_eq!(relation.evidence, call.evidence);
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.source == call.source
                && gap.detail == "rust-call-typecheck-and-instantiation-unverified"
        }));
        assert!(
            !document
                .extensions
                .iter()
                .any(
                    |extension| extension.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE
                        && extension.evidence.source.as_ref() == Some(&call.source)
                )
        );
    }
}

#[test]
fn inline_and_directory_modules_resolve_relative_explicit_imports() {
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/worker/mod.rs", "src/worker/leaf.rs"],
        [
            "pub mod worker; pub mod gateway { use super::worker::ping as invoke; pub fn run() { invoke(); } }",
            "pub mod leaf; pub fn ping() { self::leaf::done(); }",
            "pub fn done() {}",
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let names: BTreeSet<_> = document
        .occurrences
        .iter()
        .filter(|call| call.role == OccurrenceRole::CallSite)
        .map(|call| match call.target {
            OccurrenceTarget::Resolved { symbol } => document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap()
                .canonical_name
                .as_str(),
            _ => panic!("authored module call must resolve: {call:?}"),
        })
        .collect();
    assert_eq!(names, BTreeSet::from(["ping", "done"]));
}

#[test]
fn unrelated_same_name_provider_never_enters_the_call_proof() {
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/dep.rs", "src/noise.rs"],
        [
            "mod dep; use crate::dep::ping as selected; fn run() { selected(); }",
            "pub fn ping() {}",
            "pub fn ping() {}",
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let call = document
        .occurrences
        .iter()
        .find(|call| call.role == OccurrenceRole::CallSite)
        .unwrap();
    let target = document
        .entities
        .iter()
        .find(|entity| {
            entity.canonical_name == "ping"
                && entity.kind == EntityKind::Function
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == fixture.snapshots[1].file())
        })
        .unwrap();
    assert_eq!(
        call.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(
        call.evidence
            .derivation
            .contains(&FactRef::File(fixture.snapshots[1].file()))
    );
    assert!(
        !call
            .evidence
            .derivation
            .contains(&FactRef::File(fixture.snapshots[2].file()))
    );
    assert_eq!(call.evidence.derivation.len(), 5);
}

#[test]
fn generic_call_sites_keep_the_full_callee_without_inventing_a_target() {
    for callee in [
        "ping::<u8>",
        "crate::dep::ping::<u8>",
        "receiver.ping::<u8>",
    ] {
        let source = format!("mod dep; use crate::dep::ping; fn run() {{ {callee}(); }}");
        let fixture = ProjectFixture::new(
            ["src/lib.rs", "src/dep.rs"],
            [&source, "pub fn ping<T>() {}"],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|call| call.role == OccurrenceRole::CallSite)
            .collect();
        let [call] = calls.as_slice() else {
            panic!("one authored generic call is retained: {calls:?}")
        };
        assert_eq!(call.syntax_kind, "rust.generic_call.call");
        let start = source.find(callee).unwrap();
        assert_eq!(
            call.source.span(),
            span(fixture.snapshots[0].file(), start, start + callee.len())
        );
        assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(call.syntactic_text_hash, content_hash(callee.as_bytes()));
        assert_eq!(
            call.target,
            OccurrenceTarget::Unresolved {
                text_hash: content_hash(callee.as_bytes())
            }
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == call.source
                    && gap.domain == FactDomain::Relations
                    && gap.detail == "rust-generic-instantiation-and-typecheck-unverified")
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "rust-generic-call-target-unavailable")
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
        assert!(
            !document
                .extensions
                .iter()
                .any(
                    |extension| extension.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE
                        && extension.evidence.source.as_ref() == Some(&call.source)
                )
        );
    }
}

#[test]
fn complete_parse_reports_cannot_authenticate_an_omitted_non_bom_prefix() {
    for prefix in ["// withheld source context\n", "#![cfg(any())]\n"] {
        let source = format!("{prefix}mod dep; use crate::dep::ping; fn run() {{ ping(); }}");
        let fixture = ProjectFixture::new(
            ["src/lib.rs", "src/dep.rs"],
            [&source, "pub fn ping() {}"],
            SemanticProjectLanguage::Rust,
        );
        let parser: Arc<dyn ParseProvider> = Arc::new(PrefixOmittingParser {
            inner: real_parser(),
            prefix,
        });
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            parser,
            producer_identity(),
            content_hash(b"prefix-control-parser"),
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
        let document = output.document();
        let call = document
            .occurrences
            .iter()
            .find(|call| call.role == OccurrenceRole::CallSite)
            .unwrap();
        assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
        assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == call.source
                    && gap.detail == "rust-written-module-call-proof-unavailable")
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
    }
}

struct PrefixOmittingParser {
    inner: TreeSitterProvider,
    prefix: &'static str,
}

impl ParseProvider for PrefixOmittingParser {
    fn capabilities(&self) -> &ParseCapabilities {
        self.inner.capabilities()
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let output = rootlight_adapter_sdk::execute_parse(
            &self.inner,
            request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        let omit = request.source().bytes().starts_with(self.prefix.as_bytes());
        let start = u64::try_from(self.prefix.len()).unwrap();
        let facts: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| !omit || fact.span().end_byte() > start)
            .map(|fact| {
                if omit
                    && matches!(
                        fact.syntax_kind().as_str(),
                        "rust.file.root" | "rust.proof_context.signature"
                    )
                {
                    SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        SourceSpan::new(fact.span().file(), start, fact.span().end_byte()).unwrap(),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    )
                } else {
                    fact.clone()
                }
            })
            .collect();
        for chunk in facts.chunks(256) {
            sink.push(rootlight_adapter_sdk::SyntaxFactBatch::new(
                sink.next_sequence(),
                chunk.to_vec(),
                Vec::new(),
            ))?;
        }
        // This deliberately overclaims completeness to test the consumer's
        // independent physical-source authentication, rather than parser honesty.
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
fn uncertain_module_membership_and_dispatch_never_guess_targets() {
    let cases = [
        (
            "use crate::dep::ping; fn run() { ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod absent; fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        ("mod dep; fn run() { crate::dep::ping(); }", "fn ping() {}"),
        (
            "mod dep; fn run() { crate::dep::ping(); }",
            "pub fn ping<T>() {}",
        ),
        (
            "mod dep; fn run() { crate::dep::ping::<u8>(); }",
            "pub fn ping<T>() {}",
        ),
        (
            "mod dep; use crate::dep::ping; fn run(ping: fn()) { ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; use crate::dep::ping; fn run() { let ping = || {}; ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; use crate::dep::ping; fn run() { fn ping() {} ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; use crate::dep::ping; use crate::dep::ping; fn run() { ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; use crate::dep::*; fn run() { ping(); }",
            "pub fn ping() {}",
        ),
        (
            "#[cfg(feature = \"conditional\")] mod dep; fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "#[path = \"dep.rs\"] mod alternate; fn run() { crate::alternate::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; fn run() { opaque!(); crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; fn run() { let value = 0; value.ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; fn run() { crate::dep::ping(); }",
            "pub fn ping() {} pub fn ping() {}",
        ),
        (
            "mod dep; fn run() { crate::dep::ping(); }",
            "#[cfg(feature = \"conditional\")] pub fn ping() {}",
        ),
        (
            "mod dep; fn run() { let dep = 0; dep::ping(); }",
            "pub fn ping() {}",
        ),
        ("fn run() { crate::dep::ping(); }", "pub fn ping() {}"),
        (
            "mod dep; fn run<dep>() { dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "pub extern crate other as dep; mod dep; fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; use crate::dep::ping; extern \"C\" { fn ping(); } fn run() { ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; struct dep; fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; type dep = i32; fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; union dep { value: i32 } fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "unsafe mod dep; fn run() { crate::dep::ping(); }",
            "pub fn ping() {}",
        ),
        (
            "mod dep; use crate::dep::ping; fn run() { create_shadow!(); ping(); }",
            "pub fn ping() {}",
        ),
    ];
    for (source, provider) in cases {
        let fixture = ProjectFixture::new(
            ["src/lib.rs", "src/dep.rs"],
            [source, provider],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|call| call.role == OccurrenceRole::CallSite)
            .collect();
        if source.contains("::<") {
            let start = u64::try_from(source.find("crate::dep::ping::<").unwrap()).unwrap();
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(
                        |gap| gap.source.content_hash() == content_hash(source.as_bytes())
                            && gap.source.span().start_byte() <= start
                            && gap.source.span().end_byte() > start
                            && gap.detail == "rust-generic-instantiation-and-typecheck-unverified"
                    ),
                "{source}"
            );
        }
        assert!(!calls.is_empty(), "{source}");
        for call in calls {
            assert!(
                matches!(call.target, OccurrenceTarget::Unresolved { .. }),
                "{source}: {call:?}"
            );
            assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
        }
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(
                    |gap| gap.source.content_hash() == content_hash(source.as_bytes())
                        && gap.detail.starts_with("rust-")
                ),
            "{source}"
        );
    }
}

#[test]
fn raw_external_and_normalized_inline_names_preserve_physical_spelling() {
    for (path, source, provider, expected) in [
        (
            "src/type.rs",
            "mod r#type; fn run() { crate::r#type::ping(); }",
            "pub fn ping() {}",
            "ping",
        ),
        (
            "src/unused.rs",
            "mod λ { pub fn café() {} } fn run() { crate::λ::cafe\u{301}(); }",
            "",
            "café",
        ),
    ] {
        let fixture = ProjectFixture::new(
            ["src/lib.rs", path],
            [source, provider],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let call = document
            .occurrences
            .iter()
            .find(|call| call.role == OccurrenceRole::CallSite)
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == expected && entity.kind == EntityKind::Function)
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{source}"
        );
        assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
        let bytes = &source.as_bytes()[usize::try_from(call.source.span().start_byte()).unwrap()
            ..usize::try_from(call.source.span().end_byte()).unwrap()];
        assert_eq!(call.syntactic_text_hash, content_hash(bytes));
    }
}

#[test]
fn external_non_ascii_names_and_other_crate_declarations_never_supply_membership() {
    for fixture in [
        ProjectFixture::new_owned(
            vec!["src/lib.rs".into(), "src/λ.rs".into()],
            vec![
                "mod λ; fn run() { crate::λ::ping(); }".into(),
                "pub fn ping() {}".into(),
            ],
            SemanticProjectLanguage::Rust,
        ),
        ProjectFixture::new_owned(
            vec![
                "src/lib.rs".into(),
                "src/main.rs".into(),
                "src/dep.rs".into(),
            ],
            vec![
                "mod dep;".into(),
                "fn run() { crate::dep::ping(); }".into(),
                "pub fn ping() {}".into(),
            ],
            SemanticProjectLanguage::Rust,
        ),
    ] {
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let call = document
            .occurrences
            .iter()
            .find(|call| call.role == OccurrenceRole::CallSite)
            .unwrap();
        assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == call.source
                    && gap.detail == "rust-written-module-call-proof-unavailable")
        );
    }
}

#[test]
fn module_call_witnesses_keep_original_encoded_coordinates_and_hashes() {
    let source = "// λ 😀\nmod dep; fn run() { crate::dep::ping(); }";
    let provider = "// 😀\npub fn ping() {}";
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let mut fixture = ProjectFixture::new(
            ["src/lib.rs", "src/dep.rs"],
            [source, provider],
            SemanticProjectLanguage::Rust,
        );
        let encode = |text: &str| -> Vec<u8> {
            match encoding {
                "utf-8" => text.as_bytes().to_vec(),
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
        let bytes = [encode(source), encode(provider)];
        for (path, bytes) in ["src/lib.rs", "src/dep.rs"].into_iter().zip(&bytes) {
            fs::write(fixture._temporary.path().join(path), bytes).unwrap();
        }
        let repository = RepositoryRoot::open(repository_id(), fixture._temporary.path()).unwrap();
        fixture.snapshots = ["src/lib.rs", "src/dep.rs"]
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
                .as_str()
                .cmp(right.source().path().as_str())
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
        let call = document
            .occurrences
            .iter()
            .find(|call| call.role == OccurrenceRole::CallSite)
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "ping" && entity.kind == EntityKind::Function)
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{encoding}"
        );
        let prefix = source
            .get(..source.find("crate::dep::ping").unwrap())
            .unwrap();
        let start = if matches!(encoding, "utf-8" | "utf-8-bom") {
            prefix.len() + if encoding == "utf-8-bom" { 3 } else { 0 }
        } else {
            2 + prefix.encode_utf16().count() * 2
        };
        let length = if matches!(encoding, "utf-8" | "utf-8-bom") {
            "crate::dep::ping".len()
        } else {
            "crate::dep::ping".encode_utf16().count() * 2
        };
        assert_eq!(
            call.source.span(),
            span(fixture.snapshots[0].file(), start, start + length)
        );
        assert_eq!(call.source.content_hash(), content_hash(&bytes[0]));
        assert_eq!(
            target.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(&bytes[1])
        );
        assert!(
            call.evidence
                .derivation
                .contains(&FactRef::File(fixture.snapshots[1].file()))
        );
    }
}

#[test]
fn duplicate_layouts_crate_roots_and_private_sibling_items_stay_unresolved() {
    for fixture in [
        ProjectFixture::new_owned(vec!["src/lib.rs".into(), "src/dep.rs".into(), "src/dep/mod.rs".into()],
            vec!["mod dep; fn run() { crate::dep::ping(); }".into(), "pub fn ping() {}".into(), "pub fn ping() {}".into()], SemanticProjectLanguage::Rust),
        ProjectFixture::new_owned(vec!["src/lib.rs".into(), "src/main.rs".into(), "src/dep.rs".into()],
            vec!["mod dep;".into(), "mod dep;".into(), "pub fn ping() {} fn run() { crate::dep::ping(); }".into()], SemanticProjectLanguage::Rust),
        ProjectFixture::new_owned(vec!["src/lib.rs".into()],
            vec!["mod worker { mod hidden { pub fn ping() {} } } mod gateway { fn run() { crate::worker::hidden::ping(); } }".into()], SemanticProjectLanguage::Rust),
        ProjectFixture::new_owned(vec!["src/lib.rs".into()],
            vec!["mod dep; mod dep { pub fn ping() {} } fn run() { crate::dep::ping(); }".into()], SemanticProjectLanguage::Rust),
        ProjectFixture::new_owned(vec!["src/lib.rs".into()],
            vec!["mod dep {} mod dep { fn run() { crate::other::ping(); } } mod other { pub fn ping() {} }".into()], SemanticProjectLanguage::Rust),
        ProjectFixture::new_owned(vec!["src/lib.rs".into(), "src/dep.rs".into(), "src/other.rs".into()],
            vec!["mod dep; mod other; use crate::other as dep; fn run() { crate::dep::ping(); }".into(), "pub fn ping() {}".into(), "pub fn ping() {}".into()], SemanticProjectLanguage::Rust),
    ] {
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        assert!(document.occurrences.iter().filter(|call| call.role == OccurrenceRole::CallSite)
            .all(|call| matches!(call.target, OccurrenceTarget::Unresolved { .. })));
        assert!(!document.relations.iter().any(|relation| relation.predicate == RelationPredicate::Calls));
    }
}
