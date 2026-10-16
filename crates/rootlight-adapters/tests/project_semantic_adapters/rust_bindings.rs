//! Rust project bindings must preserve written owners and lexical visibility.
//! The native and project consumers share identity rules; these fixtures also
//! reject name-only fallback across initializer, namespace, and item boundaries.

use super::*;
use rootlight_ir::{ContainerRef, NormalizedIrDocument, OccurrenceRecord, RelationEndpoint};

fn direct_call_proof<'a>(
    document: &'a NormalizedIrDocument,
    call: &OccurrenceRecord,
) -> Option<(
    &'a rootlight_ir::ExtensionEnvelope,
    rootlight_ir::RustDirectCall,
)> {
    let proofs = document
        .extensions
        .iter()
        .filter(|envelope| {
            envelope.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE
                && envelope.evidence.source.as_ref() == Some(&call.source)
        })
        .map(|envelope| {
            (
                envelope,
                rootlight_ir::decode_rust_direct_call_envelope(envelope)
                    .expect("exact typed Rust call proof"),
            )
        })
        .collect::<Vec<_>>();
    assert!(proofs.len() <= 1, "one source-owned proof for one call");
    proofs.into_iter().next()
}

pub(super) fn assert_direct_call_proof(
    document: &NormalizedIrDocument,
    call: &OccurrenceRecord,
    target: rootlight_ids::SymbolId,
) {
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert_eq!(call.syntax_kind, "rust.direct_call.call");
    let (envelope, proof) =
        direct_call_proof(document, call).expect("source-backed function call proof");
    assert_eq!(proof.target, target);
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == target)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Function);
    assert!(!entity.flags.contains(&EntityFlag::Synthetic));
    assert_eq!(
        proof.canonical_name,
        content_hash(entity.canonical_name.as_bytes())
    );
    assert_eq!(envelope.provenance, entity.provenance);
    assert_eq!(envelope.provenance, call.provenance);
    let file = document
        .files
        .iter()
        .find(|file| file.id == call.file)
        .unwrap();
    assert_eq!(envelope.provenance, file.provenance);
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|definition| {
            definition.role == OccurrenceRole::Definition
                && definition.source.span() == proof.definition
                && definition.target == OccurrenceTarget::Resolved { symbol: target }
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].provenance, envelope.provenance);
    assert_eq!(
        definitions[0].source.content_hash(),
        call.source.content_hash()
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Occurrence(call.id))
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "rust-call-dispatch-target-unavailable"
                && gap.source == call.source)
    );
}

#[test]
fn rust_project_direct_calls_use_exact_source_proof_before_resolver_adoption() {
    let source = "fn /*definition*/helper() {} fn run() { /*call*/helper(); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let target = exact_symbol(marked_occurrence(
        document,
        source,
        "/*definition*/",
        OccurrenceRole::Definition,
    ));
    assert_direct_call_proof(
        document,
        marked_occurrence(document, source, "/*call*/", OccurrenceRole::CallSite),
        target,
    );
}

#[test]
fn rust_project_unsupported_calls_have_no_name_only_targets_or_claims() {
    for source in [
        "fn helper() {} fn run() { let helper = || 1; /*call*/helper(); }",
        "fn helper() {} fn run(helper: fn()) { /*call*/helper(); }",
        "fn helper() {} fn run() { { fn helper() {} /*call*/helper(); } }",
        "fn helper() {} fn run() { fn nested() { /*call*/helper(); } }",
        "fn helper() {} fn run() { let receiver = 1; /*call*/receiver.helper(); }",
        "fn helper() {} fn run() { /*call*/crate::helper(); }",
        "fn helper() {} use other::helper; fn run() { /*call*/helper(); }",
        "#[cfg(feature = \"conditional\")] fn helper() {} fn run() { /*call*/helper(); }",
        "fn helper() {} fn run() { opaque!(); /*call*/helper(); }",
    ] {
        let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let call = marked_occurrence(document, source, "/*call*/", OccurrenceRole::CallSite);
        assert_unproven_call(document, call);
    }
}

#[test]
fn rust_project_external_module_stub_vetoes_direct_call_proof() {
    let source = "mod opaque; fn target() {} fn owner(){target();}";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "target")
        .expect("written function is retained");
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "owner")
        .expect("written callable owner is retained");
    let start = u64::try_from(source.rfind("target").unwrap()).unwrap();
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 6
        })
        .collect();
    assert_eq!(calls.len(), 1, "exact authored callee is retained");
    let call = calls[0];
    assert_eq!(call.syntax_kind, "rust.direct_call.call");
    assert_eq!(call.enclosing, Some(owner.id));
    assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(call.syntactic_text_hash, content_hash(b"target"));
    let bytes = fixture.snapshots[0].content();
    assert_eq!(
        &bytes[usize::try_from(start).unwrap()..usize::try_from(start + 6).unwrap()],
        b"target"
    );
    assert_unproven_call(document, call);
    assert!(!document.extensions.iter().any(|envelope| {
        envelope.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE
            && rootlight_ir::decode_rust_direct_call_envelope(envelope)
                .is_ok_and(|proof| proof.target == target.id)
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source == call.source
            && gap.detail == "rust-call-dispatch-target-unavailable"
            && gap.provenance == call.provenance
    }));
}

pub(super) fn assert_unproven_call(document: &NormalizedIrDocument, call: &OccurrenceRecord) {
    let expected_gap = if call.syntax_kind == "rust.generic_call.call" {
        "rust-generic-instantiation-and-typecheck-unverified"
    } else if call.syntax_kind == "rust.scoped_call.scoped_call"
        || document.occurrences.iter().any(|occurrence| {
            occurrence.file == call.file && occurrence.role == OccurrenceRole::ImportUse
        })
    {
        "rust-written-module-call-proof-unavailable"
    } else {
        "rust-call-dispatch-target-unavailable"
    };
    assert_unproven_call_with_gap(document, call, expected_gap);
}

pub(super) fn assert_unproven_call_with_gap(
    document: &NormalizedIrDocument,
    call: &OccurrenceRecord,
    expected_gap: &str,
) {
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    assert!(direct_call_proof(document, call).is_none());
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == expected_gap
            && gap.source == call.source
            && gap.domain == FactDomain::Relations
            && gap.provenance == call.provenance
    }));
    assert!(!document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.subject == RelationEndpoint::Occurrence(call.id)
    }));
}

#[test]
fn rust_project_direct_calls_observe_whole_let_activation() {
    let source = "fn /*definition*/helper() {} fn run() { let helper = /*initializer*/helper(); /*after*/helper(); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let target = exact_symbol(marked_occurrence(
        document,
        source,
        "/*definition*/",
        OccurrenceRole::Definition,
    ));
    assert_direct_call_proof(
        document,
        marked_occurrence(
            document,
            source,
            "/*initializer*/",
            OccurrenceRole::CallSite,
        ),
        target,
    );
    assert_unproven_call(
        document,
        marked_occurrence(document, source, "/*after*/", OccurrenceRole::CallSite),
    );
}

#[test]
fn rust_project_direct_calls_retain_closure_creation_environment() {
    let source = "fn /*definition*/helper() {} fn run() { let before = || /*before*/helper(); let helper = || 1; let after = || /*after*/helper(); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let target = exact_symbol(marked_occurrence(
        document,
        source,
        "/*definition*/",
        OccurrenceRole::Definition,
    ));
    let before = marked_occurrence(document, source, "/*before*/", OccurrenceRole::CallSite);
    assert_direct_call_proof(document, before, target);
    let owner = document
        .entities
        .iter()
        .find(|entity| Some(entity.id) == before.enclosing)
        .unwrap();
    assert!(owner.flags.contains(&EntityFlag::Synthetic));
    assert_eq!(owner.kind, EntityKind::Function);
    assert_unproven_call(
        document,
        marked_occurrence(document, source, "/*after*/", OccurrenceRole::CallSite),
    );
}

#[test]
fn rust_project_trimming_keeps_direct_call_namespace_barriers() {
    for barrier in [
        "use other::helper;",
        "#[cfg(feature = \"conditional\")]",
        "macro_rules! opaque { () => {} }",
    ] {
        let source = format!(
            "fn helper() {{}} {barrier} fn run() {{ {} /*call*/helper(); }}",
            "unknown(); ".repeat(400)
        );
        let fixture = ProjectFixture::new(["src/lib.rs"], [&source], SemanticProjectLanguage::Rust);
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC),
            "{barrier}: optional syntax is bounded"
        );
        let call = marked_occurrence(document, &source, "/*call*/", OccurrenceRole::CallSite);
        let expected_gap = if barrier.starts_with("use ") {
            "rust-written-module-call-proof-unavailable"
        } else {
            "rust-call-dispatch-target-unavailable"
        };
        assert_unproven_call_with_gap(document, call, expected_gap);
        assert!(
            !document
                .extensions
                .iter()
                .any(|envelope| envelope.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE),
            "{barrier}: retained barrier vetoes all call proofs"
        );
    }
}

#[test]
fn rust_project_direct_call_proofs_preserve_raw_unicode_and_encoded_source() {
    let source = "fn /*raw_definition*/r#type() {} fn /*unicode_definition*/é() {} fn run() { /*raw_call*/r#type(); /*unicode_call*/e\u{301}(); }";
    let mut identities = Vec::new();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut fixture =
            ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
        let bytes: Vec<_> = match encoding {
            "utf-8" => source.as_bytes().to_vec(),
            "utf-16le" => [0xff, 0xfe]
                .into_iter()
                .chain(source.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
            "utf-16be" => [0xfe, 0xff]
                .into_iter()
                .chain(source.encode_utf16().flat_map(u16::to_be_bytes))
                .collect(),
            _ => unreachable!(),
        };
        fs::write(fixture._temporary.path().join("src/lib.rs"), &bytes).unwrap();
        let repository = RepositoryRoot::open(repository_id(), fixture._temporary.path()).unwrap();
        fixture.snapshots = vec![
            repository
                .snapshot(
                    &RelativePath::parse(Path::new("src/lib.rs")).unwrap(),
                    64 * 1024,
                )
                .unwrap(),
        ];
        fixture.sources = vec![SourceRef::new(
            repository_id(),
            generation_id(),
            span(fixture.snapshots[0].file(), 0, bytes.len()),
            content_hash(&bytes),
            None,
        )];
        let limits = real_parser_limits();
        let input = ProjectSourceInput::new(
            GenerationBoundSnapshot::new(&fixture.snapshots[0], &fixture.sources[0]).unwrap(),
            LanguageId::new("rust").unwrap(),
            EncodingId::new(encoding).unwrap(),
            false,
            Vec::new(),
        );
        let manifest = b"{\"target\":\"fixture\"}";
        let request = ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.project").unwrap(),
            BuildTargetId::new("//fixture:project").unwrap(),
            fixture.build_context,
            content_hash(manifest),
            manifest,
            vec![input],
            AnalysisTier::TierB,
            &limits,
        )
        .unwrap();
        let parser: Arc<dyn ParseProvider> = Arc::new(real_parser());
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            parser,
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
        let at = |marker: &str, role| {
            let index = source.find(marker).unwrap() + marker.len();
            let start = if encoding == "utf-8" {
                index
            } else {
                2 + source[..index].encode_utf16().count() * 2
            };
            let matches: Vec<_> = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == role
                        && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .collect();
            assert_eq!(matches.len(), 1, "{encoding}: {marker}");
            matches[0]
        };
        let mut targets = Vec::new();
        for (definition_marker, call_marker, definition_spelling, call_spelling, canonical) in [
            (
                "/*raw_definition*/",
                "/*raw_call*/",
                "r#type",
                "r#type",
                "type",
            ),
            (
                "/*unicode_definition*/",
                "/*unicode_call*/",
                "é",
                "e\u{301}",
                "é",
            ),
        ] {
            let definition = at(definition_marker, OccurrenceRole::Definition);
            let call = at(call_marker, OccurrenceRole::CallSite);
            let target = exact_symbol(definition);
            assert_direct_call_proof(document, call, target);
            let (_, proof) = direct_call_proof(document, call).unwrap();
            assert_eq!(proof.canonical_name, content_hash(canonical.as_bytes()));
            for (occurrence, spelling) in [(definition, definition_spelling), (call, call_spelling)]
            {
                assert_eq!(occurrence.source.content_hash(), content_hash(&bytes));
                assert_eq!(
                    occurrence.syntactic_text_hash,
                    content_hash(spelling.as_bytes())
                );
                let start = usize::try_from(occurrence.source.span().start_byte()).unwrap();
                let end = usize::try_from(occurrence.source.span().end_byte()).unwrap();
                let expected: Vec<_> = match encoding {
                    "utf-8" => spelling.as_bytes().to_vec(),
                    "utf-16le" => spelling.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                    "utf-16be" => spelling.encode_utf16().flat_map(u16::to_be_bytes).collect(),
                    _ => unreachable!(),
                };
                assert_eq!(
                    &bytes[start..end],
                    expected,
                    "{encoding}: physical source retrieval"
                );
            }
            targets.push(target);
        }
        identities.push(targets);
    }
    assert!(identities.windows(2).all(|pair| pair[0] == pair[1]));
}

fn marked_occurrence<'a>(
    document: &'a NormalizedIrDocument,
    source: &str,
    marker: &str,
    role: OccurrenceRole,
) -> &'a OccurrenceRecord {
    let start = u64::try_from(source.find(marker).expect("marker exists") + marker.len()).unwrap();
    let occurrences = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == role && occurrence.source.span().start_byte() == start
        })
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 1, "{marker}: {role:?}");
    let occurrence = occurrences[0];
    assert_eq!(
        occurrence.source.content_hash(),
        content_hash(source.as_bytes())
    );
    occurrence
}

fn exact_symbol(occurrence: &OccurrenceRecord) -> rootlight_ids::SymbolId {
    let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
        panic!("expected exact written binding: {occurrence:?}")
    };
    symbol
}

fn assert_unexpanded_macro_token(
    document: &NormalizedIrDocument,
    source: &str,
    marker: &str,
    name: &str,
) {
    let start = u64::try_from(source.find(marker).unwrap() + marker.len()).unwrap();
    let end = start + u64::try_from(name.len()).unwrap();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.source.span().start_byte() <= start
                && occurrence.source.span().end_byte() >= end
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { .. } | OccurrenceTarget::Candidates { .. }
                ))
    );
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "rust-macro-expansion-bindings-unavailable"
        && gap.source.content_hash() == content_hash(source.as_bytes())
        && gap.source.span().start_byte() <= start
        && gap.source.span().end_byte() >= end));
    assert_eq!(
        &source.as_bytes()[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
        name.as_bytes()
    );
}

#[test]
fn rust_project_local_leaves_preserve_definitions_and_structural_identity() {
    let source = "fn run(/*parameter*/seed: i32) { let (/*left*/left, /*right*/right) = (seed, 2); let ref /*borrow*/borrowed = left; let _ = right; }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "run")
        .unwrap();
    for (marker, name, kind) in [
        ("/*parameter*/", "seed", EntityKind::Parameter),
        ("/*left*/", "left", EntityKind::Variable),
        ("/*right*/", "right", EntityKind::Variable),
        ("/*borrow*/", "borrowed", EntityKind::Variable),
    ] {
        let definition = marked_occurrence(document, source, marker, OccurrenceRole::Definition);
        let symbol = exact_symbol(definition);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!((entity.kind, entity.canonical_name.as_str()), (kind, name));
        assert_eq!(entity.container, Some(ContainerRef::Entity(owner.id)));
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(symbol)
            })
            .count();
        assert_eq!(contains, 1, "{name} has exactly one written owner");
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "_")
    );
    assert_real_parser_symbol_identity(
        &fixture,
        "rust",
        &[
            (EntityKind::Parameter, "seed"),
            (EntityKind::Variable, "left"),
            (EntityKind::Variable, "right"),
            (EntityKind::Variable, "borrowed"),
        ],
    );
}

#[test]
fn rust_project_shadowing_uses_the_prior_binding_during_initialization() {
    let source = "fn run() { let /*first*/value = 1; let /*second*/value = /*initializer*/value + 1; { let /*inner*/value = 3; consume(/*inside*/value); } consume(/*after*/value); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let first = exact_symbol(marked_occurrence(
        document,
        source,
        "/*first*/",
        OccurrenceRole::Definition,
    ));
    let second = exact_symbol(marked_occurrence(
        document,
        source,
        "/*second*/",
        OccurrenceRole::Definition,
    ));
    let inner = exact_symbol(marked_occurrence(
        document,
        source,
        "/*inner*/",
        OccurrenceRole::Definition,
    ));
    assert_eq!(BTreeSet::from([first, second, inner]).len(), 3);
    for (marker, expected) in [
        ("/*initializer*/", first),
        ("/*inside*/", inner),
        ("/*after*/", second),
    ] {
        assert_eq!(
            exact_symbol(marked_occurrence(
                document,
                source,
                marker,
                OccurrenceRole::Reference
            )),
            expected
        );
    }
}

#[test]
fn rust_project_local_targets_do_not_cross_source_or_namespace_boundaries() {
    for source in [
        "fn run() { consume(/*use*/value); let value = 1; }",
        "fn run() { let value = /*use*/value; }",
        "fn run() { { let value = 1; } { consume(/*use*/value); } }",
        "fn first() { let value = 1; } fn second() { consume(/*use*/value); }",
        "fn run() { let value = 1; fn nested() { consume(/*use*/value); } }",
    ] {
        let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
        let output = analyze_with_real_parser(&fixture);
        let reference = marked_occurrence(
            output.document(),
            source,
            "/*use*/",
            OccurrenceRole::Reference,
        );
        assert!(
            matches!(reference.target, OccurrenceTarget::Unresolved { .. }),
            "{source}: {:?}",
            reference.target
        );
    }
    let source = "fn run() { let value = 1; invoke!(/*use*/value); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    assert_unexpanded_macro_token(output.document(), source, "/*use*/", "value");
    let source = "fn run() { let Value = 1; let other: /*type*/Value; }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let reference = marked_occurrence(
        output.document(),
        source,
        "/*type*/",
        OccurrenceRole::TypeUse,
    );
    assert!(matches!(
        reference.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn rust_project_closure_bindings_keep_the_anonymous_owner() {
    let source = "fn run() { let outer = 1; let callback = |/*parameter*/arg| { let /*local*/inside = /*use*/arg + outer; inside }; }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let parameter = exact_symbol(marked_occurrence(
        document,
        source,
        "/*parameter*/",
        OccurrenceRole::Definition,
    ));
    let local = exact_symbol(marked_occurrence(
        document,
        source,
        "/*local*/",
        OccurrenceRole::Definition,
    ));
    assert_eq!(
        exact_symbol(marked_occurrence(
            document,
            source,
            "/*use*/",
            OccurrenceRole::Reference
        )),
        parameter
    );
    let parameter_entity = document
        .entities
        .iter()
        .find(|entity| entity.id == parameter)
        .unwrap();
    let local_entity = document
        .entities
        .iter()
        .find(|entity| entity.id == local)
        .unwrap();
    assert_eq!(parameter_entity.container, local_entity.container);
    let Some(ContainerRef::Entity(owner)) = parameter_entity.container else {
        panic!("closure owner")
    };
    let closure = document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap();
    assert_eq!(closure.kind, EntityKind::Function);
    assert!(closure.flags.contains(&EntityFlag::Synthetic));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: owner })
    );
    for child in [parameter, local] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(child))
                .count(),
            1
        );
    }
}

#[test]
fn rust_project_destructuring_emits_only_written_binding_leaves() {
    let source = "struct Pair { left: i32, right: i32 } fn run(/*input*/input: Pair) { let (mut /*first*/first, (_, ref /*second*/second)) = (1, (2, 3)); let [/*head*/head, /*tail*/tail @ ..] = [1, 2]; let Pair { /*left*/left, right: /*renamed*/renamed } = input; let Some(/*inner*/inner) = Some(1) else { return }; let &/*borrowed*/borrowed = &1; let /*kept*/_kept: i32; let ref mut /*changed*/changed = 1; let /*discard*/_ = 0; }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "run")
        .unwrap()
        .id;
    let mut symbols = BTreeSet::new();
    for (marker, name) in [
        ("/*first*/", "first"),
        ("/*second*/", "second"),
        ("/*head*/", "head"),
        ("/*tail*/", "tail"),
        ("/*left*/", "left"),
        ("/*renamed*/", "renamed"),
        ("/*inner*/", "inner"),
        ("/*borrowed*/", "borrowed"),
        ("/*kept*/", "_kept"),
        ("/*changed*/", "changed"),
    ] {
        let symbol = exact_symbol(marked_occurrence(
            document,
            source,
            marker,
            OccurrenceRole::Definition,
        ));
        assert!(
            symbols.insert(symbol),
            "distinct written leaves have distinct symbols"
        );
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(
            (entity.kind, entity.canonical_name.as_str()),
            (EntityKind::Variable, name)
        );
        assert_eq!(entity.container, Some(ContainerRef::Entity(owner)));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(symbol))
                .count(),
            1
        );
    }
    {
        let marker = "/*discard*/";
        let start = u64::try_from(source.find(marker).unwrap() + marker.len()).unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start)
        );
    }
    assert_real_parser_symbol_identity(
        &fixture,
        "rust",
        &[
            (EntityKind::Variable, "first"),
            (EntityKind::Variable, "second"),
            (EntityKind::Variable, "left"),
            (EntityKind::Variable, "renamed"),
            (EntityKind::Variable, "inner"),
        ],
    );
}

#[test]
fn rust_project_let_else_retains_the_prior_environment_until_the_semicolon() {
    let source = "fn run() { let /*prior*/value = 7; let Some(/*next*/value) = produce(/*initializer*/value) else { inspect(/*else*/value); return }; inspect(/*after*/value); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let prior = exact_symbol(marked_occurrence(
        document,
        source,
        "/*prior*/",
        OccurrenceRole::Definition,
    ));
    let next = exact_symbol(marked_occurrence(
        document,
        source,
        "/*next*/",
        OccurrenceRole::Definition,
    ));
    assert_ne!(prior, next);
    for (marker, symbol) in [
        ("/*initializer*/", prior),
        ("/*else*/", prior),
        ("/*after*/", next),
    ] {
        assert_eq!(
            exact_symbol(marked_occurrence(
                document,
                source,
                marker,
                OccurrenceRole::Reference
            )),
            symbol,
            "{marker}"
        );
    }
}

#[test]
fn rust_project_closure_capture_keeps_the_creation_environment() {
    let source = "fn run(/*outer_parameter*/parameter: i32) { let /*prior*/value = 1; let capture = || { inspect(/*capture*/value); }; let /*next*/value = 2; let shadow = |/*inner_parameter*/parameter| { inspect(/*inner_use*/parameter); }; inspect(/*outer_use*/parameter); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    for (definition, use_site) in [
        ("/*prior*/", "/*capture*/"),
        ("/*inner_parameter*/", "/*inner_use*/"),
        ("/*outer_parameter*/", "/*outer_use*/"),
    ] {
        let symbol = exact_symbol(marked_occurrence(
            document,
            source,
            definition,
            OccurrenceRole::Definition,
        ));
        assert_eq!(
            exact_symbol(marked_occurrence(
                document,
                source,
                use_site,
                OccurrenceRole::Reference
            )),
            symbol
        );
    }
    let closures: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.flags.contains(&EntityFlag::Synthetic) && entity.kind == EntityKind::Function
        })
        .collect();
    assert_eq!(closures.len(), 2);
    assert_ne!(closures[0].id, closures[1].id);
}

#[test]
fn rust_project_const_patterns_and_local_calls_remain_conservative() {
    let source = "const LIMIT: i32 = 1; struct Unit; type Name = i32; fn callable() {} fn run(input: i32) { let /*constant*/LIMIT = input else { return }; let /*constructor*/Unit = Unit; let /*value*/Name = 1; let typed: /*type*/Name = 1; let callable = || 1; /*call*/callable(); inspect(/*reference*/Name); opaque!(/*macro*/Name); }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    for marker in ["/*constant*/", "/*constructor*/"] {
        let start = u64::try_from(source.find(marker).unwrap() + marker.len()).unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start)
        );
    }
    let value = exact_symbol(marked_occurrence(
        document,
        source,
        "/*value*/",
        OccurrenceRole::Definition,
    ));
    assert_eq!(
        exact_symbol(marked_occurrence(
            document,
            source,
            "/*reference*/",
            OccurrenceRole::Reference
        )),
        value
    );
    assert_ne!(
        marked_occurrence(document, source, "/*type*/", OccurrenceRole::TypeUse).target,
        OccurrenceTarget::Resolved { symbol: value }
    );
    assert!(matches!(
        marked_occurrence(document, source, "/*call*/", OccurrenceRole::CallSite).target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_unexpanded_macro_token(document, source, "/*macro*/", "Name");
    for detail in [
        "rust-pattern-binding-namespace-unavailable",
        "rust-call-dispatch-target-unavailable",
        "rust-macro-expansion-bindings-unavailable",
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "{detail}"
        );
    }
}

#[test]
fn rust_project_local_identities_survive_initializer_body_and_trivia_edits() {
    let source = "fn run() { let /*first*/value = 1; { let /*inner*/value = 2; inspect(value); } let /*last*/value = 3; inspect(value); }";
    let modified = source
        .replace("fn run", "// source trivia\nfn run")
        .replace("= 2", "= 20")
        .replace("inspect(value)", "inspect( value )");
    let identities = |source: &str| {
        let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
        let output = analyze_with_real_parser(&fixture);
        ["/*first*/", "/*inner*/", "/*last*/"].map(|marker| {
            exact_symbol(marked_occurrence(
                output.document(),
                source,
                marker,
                OccurrenceRole::Definition,
            ))
        })
    };
    let original = identities(source);
    assert_eq!(original.into_iter().collect::<BTreeSet<_>>().len(), 3);
    assert_eq!(original, identities(&modified));
}

#[test]
fn rust_project_generic_fallback_cannot_import_another_files_local_binding() {
    let source = "fn run() { inspect(/*value*/value); let item: /*type*/Name; }";
    let other = "fn other(value: i32) { let Name = 1; }";
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/other.rs"],
        [source, other],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    for (marker, role) in [
        ("/*value*/", OccurrenceRole::Reference),
        ("/*type*/", OccurrenceRole::TypeUse),
    ] {
        assert!(matches!(
            marked_occurrence(output.document(), source, marker, role).target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn rust_project_static_items_keep_their_value_type_and_call_namespaces() {
    let source = "const /*constant*/LIMIT: i32 = 1; static /*static*/VALUE: i32 = 2; type /*alias*/Name = i32; fn /*function*/helper() {} fn run() { inspect(/*constant_use*/LIMIT); inspect(/*static_use*/VALUE); /*call*/helper(); let typed: /*type*/Name = 1; inspect(/*wrong_value*/Name); let wrong: /*wrong_type*/helper; }";
    let fixture = ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    for (definition, use_site, role) in [
        (
            "/*constant*/",
            "/*constant_use*/",
            OccurrenceRole::Reference,
        ),
        ("/*static*/", "/*static_use*/", OccurrenceRole::Reference),
        ("/*alias*/", "/*type*/", OccurrenceRole::TypeUse),
        ("/*function*/", "/*call*/", OccurrenceRole::CallSite),
    ] {
        let symbol = exact_symbol(marked_occurrence(
            document,
            source,
            definition,
            OccurrenceRole::Definition,
        ));
        let occurrence = marked_occurrence(document, source, use_site, role);
        if role == OccurrenceRole::CallSite {
            assert_unproven_call(document, occurrence);
            assert!(!document.extensions.iter().any(|envelope| {
                envelope.namespace == rootlight_ir::RUST_DIRECT_CALL_NAMESPACE
                    && rootlight_ir::decode_rust_direct_call_envelope(envelope)
                        .is_ok_and(|proof| proof.target == symbol)
            }));
        } else {
            assert_eq!(exact_symbol(occurrence), symbol, "{use_site}");
        }
    }
    for (marker, role) in [
        ("/*wrong_value*/", OccurrenceRole::Reference),
        ("/*wrong_type*/", OccurrenceRole::TypeUse),
    ] {
        assert!(matches!(
            marked_occurrence(document, source, marker, role).target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn rust_project_qualified_calls_do_not_use_the_unqualified_local_environment() {
    let source = "mod tools; fn run() { let helper = || 1; /*qualified*/tools::helper(); }";
    let provider = "pub fn /*definition*/helper() {}";
    let fixture = ProjectFixture::new(
        ["src/lib.rs", "src/tools.rs"],
        [source, provider],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let expected = exact_symbol(marked_occurrence(
        document,
        provider,
        "/*definition*/",
        OccurrenceRole::Definition,
    ));
    let call = marked_occurrence(document, source, "/*qualified*/", OccurrenceRole::CallSite);
    assert_eq!(call.target, OccurrenceTarget::Resolved { symbol: expected });
    assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(call.syntactic_text_hash, content_hash(b"tools::helper"));
    assert!(direct_call_proof(document, call).is_none());
    let target_file = document
        .files
        .iter()
        .find(|file| file.id == fixture.snapshots[1].file())
        .unwrap();
    assert_eq!(target_file.content_hash, content_hash(provider.as_bytes()));
    assert!(
        call.evidence
            .derivation
            .contains(&rootlight_ir::FactRef::File(target_file.id))
    );
    assert!(
        call.evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Fact(target_file.provenance))
    );
    assert!(
        call.evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Entity(expected))
    );
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "rust-call-typecheck-and-instantiation-unverified"
        && gap.source == call.source
        && gap.provenance == call.provenance));
    assert!(document.relations.iter().any(|relation| relation.predicate
        == RelationPredicate::Calls
        && relation.subject == RelationEndpoint::Occurrence(call.id)
        && relation.object == RelationEndpoint::Entity(expected)
        && relation.evidence == call.evidence));
}

#[test]
fn rust_project_unavailable_inner_patterns_do_not_rebind_the_outer_local() {
    for construct in [
        "for value in produce(/*initializer*/value) { inspect(/*inside*/value); inspect(/*unaffected*/other); } inspect(/*after*/value);",
        "if let Some(value) = produce(/*initializer*/value) { inspect(/*inside*/value); inspect(/*unaffected*/other); } else { inspect(/*outside*/value); } inspect(/*after*/value);",
        "while let Some(value) = produce(/*initializer*/value) { inspect(/*inside*/value); inspect(/*unaffected*/other); } inspect(/*after*/value);",
        "match produce(/*initializer*/value) { Some(value) => { inspect(/*inside*/value); inspect(/*unaffected*/other); }, _ => inspect(/*outside*/value) }; inspect(/*after*/value);",
        "{ let (Some(value) | None) = produce(/*initializer*/value) else { return }; inspect(/*inside*/value); inspect(/*unaffected*/other); } inspect(/*after*/value);",
    ] {
        let source =
            format!("fn run() {{ let /*outer*/value = 1; let /*other*/other = 2; {construct} }}");
        let fixture = ProjectFixture::new(["src/lib.rs"], [&source], SemanticProjectLanguage::Rust);
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let outer = exact_symbol(marked_occurrence(
            document,
            &source,
            "/*outer*/",
            OccurrenceRole::Definition,
        ));
        let inside = marked_occurrence(document, &source, "/*inside*/", OccurrenceRole::Reference);
        assert!(
            matches!(inside.target, OccurrenceTarget::Unresolved { .. }),
            "{construct}: {:?}",
            inside.target
        );
        for marker in ["/*initializer*/", "/*after*/", "/*outside*/"] {
            if source.contains(marker) {
                assert_eq!(
                    exact_symbol(marked_occurrence(
                        document,
                        &source,
                        marker,
                        OccurrenceRole::Reference
                    )),
                    outer,
                    "{construct}: {marker}"
                );
            }
        }
        let other = exact_symbol(marked_occurrence(
            document,
            &source,
            "/*other*/",
            OccurrenceRole::Definition,
        ));
        assert_eq!(
            exact_symbol(marked_occurrence(
                document,
                &source,
                "/*unaffected*/",
                OccurrenceRole::Reference
            )),
            other,
            "{construct}: unrelated written outer binding"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| gap.detail
                == "rust-pattern-binding-namespace-unavailable"
                && gap.source.content_hash() == content_hash(source.as_bytes())),
            "{construct}"
        );
    }
}

#[test]
fn rust_project_raw_and_unicode_bindings_preserve_encoded_source_coordinates() {
    let source = "fn run(/*parameter*/r#type: i32) { let /*local*/λ = /*raw_use*/r#type; inspect(/*unicode_use*/λ); }";
    let mut encoding_identities = Vec::new();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut fixture =
            ProjectFixture::new(["src/lib.rs"], [source], SemanticProjectLanguage::Rust);
        let bytes = match encoding {
            "utf-8" => source.as_bytes().to_vec(),
            "utf-16le" => [0xff, 0xfe]
                .into_iter()
                .chain(source.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
            "utf-16be" => [0xfe, 0xff]
                .into_iter()
                .chain(source.encode_utf16().flat_map(u16::to_be_bytes))
                .collect(),
            _ => unreachable!(),
        };
        fs::write(fixture._temporary.path().join("src/lib.rs"), &bytes).unwrap();
        let repository = RepositoryRoot::open(repository_id(), fixture._temporary.path()).unwrap();
        fixture.snapshots = vec![
            repository
                .snapshot(
                    &RelativePath::parse(Path::new("src/lib.rs")).unwrap(),
                    64 * 1024,
                )
                .unwrap(),
        ];
        fixture.sources = vec![SourceRef::new(
            repository_id(),
            generation_id(),
            span(fixture.snapshots[0].file(), 0, bytes.len()),
            content_hash(&bytes),
            None,
        )];
        let limits = real_parser_limits();
        let input = ProjectSourceInput::new(
            GenerationBoundSnapshot::new(&fixture.snapshots[0], &fixture.sources[0]).unwrap(),
            LanguageId::new("rust").unwrap(),
            EncodingId::new(encoding).unwrap(),
            false,
            Vec::new(),
        );
        let manifest = b"{\"target\":\"fixture\"}";
        let request = ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.project").unwrap(),
            BuildTargetId::new("//fixture:project").unwrap(),
            fixture.build_context,
            content_hash(manifest),
            manifest,
            vec![input],
            AnalysisTier::TierB,
            &limits,
        )
        .unwrap();
        let provider = Arc::new(real_parser());
        let parser: Arc<dyn ParseProvider> = provider.clone();
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            parser,
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
        assert!(
            document.diagnostics.is_empty(),
            "{encoding}: {:?}",
            document.diagnostics
        );
        let physical_site = |marker: &str| {
            let index = source.find(marker).unwrap() + marker.len();
            if encoding == "utf-8" {
                u64::try_from(index).unwrap()
            } else {
                u64::try_from(2 + source[..index].encode_utf16().count() * 2).unwrap()
            }
        };
        let at = |marker: &str, role| {
            let matches: Vec<_> = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == role
                        && occurrence.source.span().start_byte() == physical_site(marker)
                })
                .collect();
            assert_eq!(matches.len(), 1, "{encoding}: {marker} {role:?}");
            matches[0]
        };
        let parameter = exact_symbol(at("/*parameter*/", OccurrenceRole::Definition));
        let local = exact_symbol(at("/*local*/", OccurrenceRole::Definition));
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == parameter)
                .unwrap()
                .canonical_name,
            "type"
        );
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == local)
                .unwrap()
                .canonical_name,
            "λ"
        );
        assert_eq!(
            exact_symbol(at("/*raw_use*/", OccurrenceRole::Reference)),
            parameter
        );
        assert_eq!(
            exact_symbol(at("/*unicode_use*/", OccurrenceRole::Reference)),
            local
        );
        let original_text = rootlight_vfs::SourceText::parse(&bytes, &Cancellation::new())
            .unwrap()
            .unwrap();
        for occurrence in &document.occurrences {
            let start = usize::try_from(occurrence.source.span().start_byte()).unwrap();
            let end = usize::try_from(occurrence.source.span().end_byte()).unwrap();
            assert_eq!(occurrence.source.content_hash(), content_hash(&bytes));
            let mut physical_end = start;
            let spelling: String = original_text
                .chars_from_byte(start)
                .unwrap()
                .take_while(|character| {
                    let next = physical_end + original_text.encoding().character_bytes(*character);
                    if next <= end {
                        physical_end = next;
                        true
                    } else {
                        false
                    }
                })
                .collect();
            assert_eq!(physical_end, end, "{encoding}: whole scalar occurrence");
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(spelling.as_bytes()),
                "{encoding}: {:?}",
                occurrence.source.span()
            );
            let written: Vec<u8> = if encoding == "utf-8" {
                spelling.as_bytes().to_vec()
            } else if encoding == "utf-16le" {
                spelling.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else {
                spelling.encode_utf16().flat_map(u16::to_be_bytes).collect()
            };
            assert_eq!(
                &bytes[start..end],
                written.as_slice(),
                "exact physical occurrence bytes"
            );
        }
        let structural = TreeSitterAnalyzer::new(
            provider,
            producer_identity(),
            LanguageId::new("rust").unwrap(),
            "rust",
            content_hash(b"real-parser-binary"),
        )
        .unwrap();
        let structural_request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshots[0], &fixture.sources[0]).unwrap(),
            LanguageId::new("rust").unwrap(),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            fixture.build_context,
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let native = execute_analysis(
            &structural,
            &structural_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        for (kind, name, expected) in [
            (EntityKind::Parameter, "type", parameter),
            (EntityKind::Variable, "λ", local),
        ] {
            assert_eq!(
                native
                    .document()
                    .entities
                    .iter()
                    .find(|entity| entity.kind == kind && entity.canonical_name == name)
                    .unwrap()
                    .id,
                expected,
                "{encoding}: {name}"
            );
        }
        encoding_identities.push([parameter, local]);
    }
    assert!(
        encoding_identities
            .windows(2)
            .all(|pair| pair[0] == pair[1])
    );
}
