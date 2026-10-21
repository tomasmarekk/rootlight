//! Written Agda equations have physical definitions distinct from type signatures.
//! Complete licensed source remains bounded; application semantics are not inferred.

use std::{collections::BTreeSet, fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, RequestError, StreamLimits,
    execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::AgdaParseProvider;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationPredicate,
    SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

const ORIGINAL: &[u8] = include_bytes!("fixtures/agda_equations/stlc_small.agda");

fn analyze(
    raw: &[u8],
    encoding: &str,
    nodes: usize,
    cancel: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.agda"), raw).unwrap();
    let repository = derive_repository(b"agda-written-equation-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.agda")).unwrap(),
            65_536,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([52; 20]),
        SourceSpan::new(snapshot.file(), 0, raw.len() as u64).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(AgdaParseProvider::new(65_536, 16_384).unwrap());
    let analyzer = TreeSitterAnalyzer::new(
        provider,
        ProducerIdentity::new(
            "agda-written-equations",
            "1",
            content_hash(b"agda-source-contract"),
        )
        .unwrap(),
        LanguageId::new("agda").unwrap(),
        "agda-written-equations",
        AgdaParseProvider::binary_digest(),
    )
    .unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let budget = AnalysisLimits::new(
        65_536,
        nodes,
        128,
        1,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("agda").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"agda-written-build")),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &input,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancel,
    )?;
    validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files.len(), 1);
    assert_eq!(output.document().files[0].content_hash, content_hash(raw));
    assert_eq!(output.document().files[0].byte_length, raw.len() as u64);
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    for site in &output.document().occurrences {
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(site.source.repository(), repository);
        assert_eq!(site.source.generation(), source.generation());
        assert_eq!(site.source.span().file(), source.span().file());
    }
    Ok(output)
}

fn definition(document: &NormalizedIrDocument, name: &str, at: usize) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == at as u64
                && site.source.span().end_byte() == (at + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "one written clause Definition {name}@{at}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("clause definition")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Function);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.provenance, sites[0].provenance);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
            .count(),
        1
    );
    symbol
}

fn declaration(document: &NormalizedIrDocument, name: &str, at: usize, symbol: SymbolId) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Declaration
                && site.source.span().start_byte() == at as u64
                && site.source.span().end_byte() == (at + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "one signature declaration {name}@{at}");
    assert_eq!(sites[0].target, OccurrenceTarget::Resolved { symbol });
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span() == sites[0].source.span())
    );
}

#[test]
fn complete_original_semicolon_clauses_define_the_same_symbols_as_their_signatures() {
    assert_eq!(ORIGINAL.len(), 2275);
    for (a, b, word) in [
        (1247, 1251, b"var%".as_slice()),
        (1291, 1295, b"var%"),
        (1648, 1651, b"v0%"),
        (1687, 1690, b"v0%"),
        (1695, 1699, b"var%"),
    ] {
        assert_eq!(&ORIGINAL[a..b], word);
    }
    let output = analyze(ORIGINAL, "utf-8", 16_384, &Cancellation::new()).unwrap();
    let first = definition(output.document(), "var%", 1291);
    let second = definition(output.document(), "v0%", 1687);
    declaration(output.document(), "var%", 1247, first);
    declaration(output.document(), "v0%", 1648, second);
    assert_ne!(first, second);
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        19
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
fn renamed_inline_and_next_line_equations_keep_signature_and_clause_spans() {
    let text = "module Written where\nplain% : Set; plain%\n = Set\nnext% : Set; next% = plain%\n";
    let output = analyze(text.as_bytes(), "utf-8", 1024, &Cancellation::new()).unwrap();
    for name in ["plain%", "next%"] {
        let first = text.find(name).unwrap();
        let second = text[first + name.len()..].find(name).unwrap() + first + name.len();
        let target = definition(output.document(), name, second);
        declaration(output.document(), name, first, target);
    }
}

#[test]
fn comments_strings_and_parenthesized_semicolons_never_forge_statement_heads() {
    let text = "module Written where\n-- ghost% : Set; ghost% = Set\n{- hidden% : Set; hidden% = Set -}\nquote% : Set\nquote% = \"; phantom% = Set\"\nkept% : (A : Set) → Set; kept% A = A\n";
    let output = analyze(text.as_bytes(), "utf-8", 1024, &Cancellation::new()).unwrap();
    let names: BTreeSet<_> = output
        .document()
        .entities
        .iter()
        .map(|e| e.canonical_name.as_str())
        .collect();
    for absent in ["ghost%", "hidden%", "phantom%"] {
        assert!(!names.contains(absent));
    }
    definition(output.document(), "kept%", text.rfind("kept%").unwrap());
}

#[test]
fn body_edits_preserve_identity_and_utf8_bom_keeps_physical_definition_bytes() {
    let before = "module Written where\nplain% : Set; plain%\n = Set\n";
    let changed = before.replace(" = Set", " = Set₁");
    let first = analyze(before.as_bytes(), "utf-8", 1024, &Cancellation::new()).unwrap();
    let next = analyze(changed.as_bytes(), "utf-8", 1024, &Cancellation::new()).unwrap();
    let site = before.rfind("plain%").unwrap();
    assert_eq!(
        definition(first.document(), "plain%", site),
        definition(next.document(), "plain%", site)
    );
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(before.as_bytes());
    let encoded = analyze(&bom, "utf-8", 1024, &Cancellation::new()).unwrap();
    definition(encoded.document(), "plain%", site + 3);
}

#[test]
fn unqualified_utf16_and_cancelled_or_tiny_requests_refuse_without_facts() {
    let raw = b"module Written where\nplain% : Set; plain% = Set\n";
    let mut utf16 = vec![0xff, 0xfe];
    for unit in std::str::from_utf8(raw).unwrap().encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    assert!(matches!(
        analyze(&utf16, "utf-16le", 1024, &Cancellation::new()),
        Err(AdapterError::RejectedRequest(
            RequestError::UnsupportedEncoding
        ))
    ));
    let stop = Cancellation::new();
    stop.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze(raw, "utf-8", 1024, &stop),
        Err(AdapterError::Cancelled { .. })
    ));
    assert!(analyze(raw, "utf-8", 1, &Cancellation::new()).is_err());
}
fn with_equation_request<R>(
    raw: &[u8],
    records: usize,
    action: impl FnOnce(&AnalysisRequest<'_>) -> R,
) -> R {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.agda"), raw).unwrap();
    let repository = derive_repository(b"agda-equation-witness-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.agda")).unwrap(),
            65_536,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([53; 20]),
        SourceSpan::new(snapshot.file(), 0, raw.len() as u64).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let syntax = StreamLimits::new(
        128,
        records,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(records.min(64), 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let ir = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        65_536,
        16_384,
        128,
        1,
        16 * 1024 * 1024,
        syntax,
        ir,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("agda").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"agda-equation-witness-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    action(&request)
}

fn equation_analyzer(
    provider: Arc<dyn rootlight_adapter_sdk::ParseProvider>,
) -> TreeSitterAnalyzer {
    TreeSitterAnalyzer::new(
        provider,
        ProducerIdentity::new(
            "agda-equation-witness",
            "1",
            content_hash(b"agda-equation-witness"),
        )
        .unwrap(),
        LanguageId::new("agda").unwrap(),
        "agda-written-equations",
        AgdaParseProvider::binary_digest(),
    )
    .unwrap()
}

#[test]
fn actual_provider_artifact_count_and_exact_record_admission_keep_signature_declarations() {
    use rootlight_adapter_sdk::{ResourceKind, SinkError, execute_parse};
    let native = Arc::new(AgdaParseProvider::new(65_536, 16_384).unwrap());
    let analyzer = equation_analyzer(native.clone());
    let required = with_equation_request(ORIGINAL, 4096, |request| {
        native
            .required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())
            .unwrap()
    });
    assert_eq!(required, 19 * 4 + 1 + 1 + 2 * 6);
    with_equation_request(ORIGINAL, required, |request| {
        let parsed = execute_parse(
            native.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(parsed.facts().len(), required);
        let (output, artifact) = analyzer
            .analyze_and_capture(
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            artifact
                .required_syntax_fact_count(&Cancellation::new())
                .unwrap(),
            required
        );
        let first = definition(output.document(), "var%", 1291);
        declaration(output.document(), "var%", 1247, first);
        let replay = analyzer
            .analyze_from_artifact(
                request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(replay.document(), output.document());
    });
    with_equation_request(ORIGINAL, required - 1, |request| {
        assert_eq!(
            native
                .required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())
                .unwrap(),
            required
        );
        for error in [
            execute_parse(
                native.as_ref(),
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap_err(),
            analyzer
                .prepare_artifact(request, &Cancellation::new())
                .unwrap_err(),
        ] {
            assert!(
                matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::Records, observed, limit })
                if observed == required && limit == required - 1),
                "{error:?}"
            );
        }
    });
}

#[test]
fn actual_materialization_refuses_missing_duplicate_and_shifted_signature_witnesses() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, execute_parse, testkit::MockParseProvider,
    };
    let raw = b"module Written where\nplain : plain; plain = Set\n";
    with_equation_request(raw, 4096, |request| {
        let native = AgdaParseProvider::new(65_536, 16_384).unwrap();
        let parsed = execute_parse(
            &native,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        let replay = |facts| {
            let mock = MockParseProvider::new(
                native.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            execute_analysis(
                &equation_analyzer(Arc::new(mock)),
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap()
        };
        let first = raw.windows(5).position(|bytes| bytes == b"plain").unwrap();
        let clause = raw.windows(5).rposition(|bytes| bytes == b"plain").unwrap();
        let baseline = replay(parsed.facts().to_vec());
        let symbol = definition(baseline.document(), "plain", clause);
        declaration(baseline.document(), "plain", first, symbol);
        for (label, mode) in [
            ("agda.function.signature", 0),
            ("agda.function.definition", 1),
            ("agda.function.signature_name.declaration", 1),
            ("agda.function.signature_name.declaration", 2),
            ("agda.function.signature_name.declaration", 3),
        ] {
            let mut facts = parsed.facts().to_vec();
            let selected: Vec<_> = facts
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == label)
                .cloned()
                .collect();
            assert_eq!(selected.len(), 1);
            let fact = &selected[0];
            if mode == 0 {
                facts.retain(|current| current.local_id() != fact.local_id());
            } else if mode == 1 {
                let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                facts.push(SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            } else if mode == 2 {
                let index = facts
                    .iter()
                    .position(|current| current.local_id() == fact.local_id())
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    SourceSpan::new(
                        fact.span().file(),
                        fact.span().start_byte() + 1,
                        fact.span().end_byte(),
                    )
                    .unwrap(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                );
            } else {
                let type_use = raw
                    .windows(5)
                    .enumerate()
                    .filter(|(_, bytes)| *bytes == b"plain")
                    .nth(1)
                    .unwrap()
                    .0;
                assert!(type_use > first && type_use < clause);
                let index = facts
                    .iter()
                    .position(|current| current.local_id() == fact.local_id())
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    SourceSpan::new(fact.span().file(), type_use as u64, (type_use + 5) as u64)
                        .unwrap(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                );
            }
            let output = replay(facts);
            assert!(
                !output
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.role == OccurrenceRole::Declaration
                        && site.source.span().start_byte() >= first as u64
                        && site.source.span().end_byte() < clause as u64)
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == rootlight_ir::FactDomain::Occurrences
                        && gap.source.content_hash() == content_hash(raw)
                        && gap.source.repository() == request.source().source_ref().repository()
                        && gap.source.generation() == request.source().source_ref().generation()
                        && gap.detail == "agda-signature-equation-identity-unavailable")
            );
            assert!(
                !output
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls)
            );
        }
    });
    let grouped = "module Written where\nleft right : Set\nleft = Set\nright = Set\n";
    let output = analyze(grouped.as_bytes(), "utf-8", 1024, &Cancellation::new()).unwrap();
    for name in ["left", "right"] {
        let symbol = definition(output.document(), name, grouped.rfind(name).unwrap());
        declaration(output.document(), name, grouped.find(name).unwrap(), symbol);
    }
}

#[test]
fn implicit_rhs_semicolons_do_not_create_file_owned_local_definitions() {
    let source = b"module Written where\nowned : Set; owned = let local : Set; local = Set in local\nother = let nested : Set; nested = Set in nested\n";
    let output = analyze(source, "utf-8", 1024, &Cancellation::new()).unwrap();
    for absent in ["local", "nested"] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == absent)
        );
    }
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        2
    );
    let at = source
        .windows(5)
        .rposition(|bytes| bytes == b"owned")
        .unwrap();
    definition(output.document(), "owned", at);
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

fn assert_rhs_reference(
    document: &NormalizedIrDocument,
    raw: &[u8],
    at: usize,
    name: &str,
    target: SymbolId,
    owner: SymbolId,
) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == at as u64
                && site.source.span().end_byte() == (at + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact written RHS Reference");
    let site = sites[0];
    assert_eq!(&raw[at..at + name.len()], name.as_bytes());
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
    assert_eq!(site.syntax_kind, "agda.rhs.reference");
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].object,
        rootlight_ir::RelationEndpoint::Entity(target)
    );
    assert_eq!(edges[0].provenance, site.provenance);
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
}

#[test]
fn complete_original_rhs_reference_targets_the_unique_earlier_written_equation() {
    assert_eq!(ORIGINAL.len(), 2275);
    assert_eq!(&ORIGINAL[1695..1699], b"var%");
    let output = analyze(ORIGINAL, "utf-8", 16_384, &Cancellation::new()).unwrap();
    let document = output.document();
    let target = definition(document, "var%", 1291);
    let owner = definition(document, "v0%", 1687);
    declaration(document, "var%", 1247, target);
    declaration(document, "v0%", 1648, owner);
    assert_rhs_reference(document, ORIGINAL, 1695, "var%", target, owner);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "agda-layout-and-expression-semantics-partial" })
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
}

#[test]
fn renamed_rhs_heads_bind_only_to_their_own_earlier_written_equations() {
    for (target, owner, argument) in [
        ("chosen", "owned", "argument"),
        ("pick%", "written%", "value%"),
    ] {
        let raw = format!(
            "{{-# OPTIONS --type-in-type #-}}\n{target} : Set → Set; {target} = λ x → x\n{argument} : Set; {argument} = Set\n{owner} : Set; {owner} = {target} ({argument})\n"
        );
        let bytes = raw.as_bytes();
        let output = analyze(bytes, "utf-8", 16_384, &Cancellation::new()).unwrap();
        let target_at = raw.find(&format!("{target} =")).unwrap();
        let owner_at = raw.find(&format!("{owner} =")).unwrap();
        let read_at = raw.rfind(target).unwrap();
        let symbol = definition(output.document(), target, target_at);
        let enclosing = definition(output.document(), owner, owner_at);
        assert_rhs_reference(output.document(), bytes, read_at, target, symbol, enclosing);
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn rhs_scope_census_refuses_shadow_blocks_duplicates_imports_and_incomplete_terms() {
    let prefix = "chosen : Set; chosen = Set\nargument : Set; argument = Set\n";
    for tail in [
        "owned : Set; owned chosen = chosen argument\n",
        "owned : Set; owned = λ chosen → chosen argument\n",
        "owned : Set; owned = let chosen = argument in chosen argument\n",
        "owned : Set; owned = chosen argument where\n  chosen : Set\n  chosen = Set\n",
        "chosen : Set\nowned : Set; owned = chosen argument\n",
        "chosen = Set\nowned : Set; owned = chosen argument\n",
        "open Other\nowned : Set; owned = chosen argument\n",
        "import Other\nowned : Set; owned = chosen argument\n",
        "module Local where\n  owned : Set; owned = chosen argument\n",
        "owned : (chosen : Set) → Set; owned = chosen argument\n",
        "owned : Set; owned = Other.chosen argument\n",
        "owned : Set; owned = chosen (argument\n",
        "owned : Set; owned = chosen argument; hidden = chosen argument\n",
        "{-# BUILTIN NATURAL chosen #-}\nowned : Set; owned = chosen argument\n",
    ] {
        let raw = format!("{prefix}{tail}");
        let output = analyze(raw.as_bytes(), "utf-8", 16_384, &Cancellation::new()).unwrap();
        assert!(
            !output.document().occurrences.iter().any(|site| {
                site.syntax_kind == "agda.rhs.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{tail}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert!(!output.document().skipped_regions.is_empty());
    }
    let later = b"owned : Set; owned = chosen argument\nchosen : Set; chosen = Set\n";
    let output = analyze(later, "utf-8", 16_384, &Cancellation::new()).unwrap();
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "agda.rhs.reference")
    );
}

#[test]
fn original_rhs_utf8_bom_and_crlf_keep_physical_source_and_target_positions() {
    for raw in [
        [b"\xef\xbb\xbf".as_slice(), ORIGINAL].concat(),
        String::from_utf8(ORIGINAL.to_vec())
            .unwrap()
            .replace('\n', "\r\n")
            .into_bytes(),
    ] {
        let original = std::str::from_utf8(ORIGINAL).unwrap();
        let text = std::str::from_utf8(&raw).unwrap();
        let shift = |at: usize| {
            if raw.starts_with(b"\xef\xbb\xbf") {
                at + 3
            } else {
                original[..at].replace('\n', "\r\n").len()
            }
        };
        let output = analyze(&raw, "utf-8", 16_384, &Cancellation::new()).unwrap();
        let target = definition(output.document(), "var%", shift(1291));
        let owner = definition(output.document(), "v0%", shift(1687));
        assert_eq!(&text.as_bytes()[shift(1695)..shift(1699)], b"var%");
        assert_rhs_reference(output.document(), &raw, shift(1695), "var%", target, owner);
    }
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze(ORIGINAL, "utf-8", 16_384, &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn original_rhs_provider_artifact_required_boundary_keeps_each_certified_reference() {
    use rootlight_adapter_sdk::execute_parse;
    let provider = Arc::new(AgdaParseProvider::new(65_536, 16_384).unwrap());
    let analyzer = equation_analyzer(provider.clone());
    let count = with_equation_request(ORIGINAL, 4096, |request| {
        provider
            .required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())
            .unwrap()
    });
    with_equation_request(ORIGINAL, count, |request| {
        let parsed = execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        let spans: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "agda.rhs.reference")
            .map(|fact| (fact.span().start_byte(), fact.span().end_byte()))
            .collect();
        assert_eq!(
            spans,
            [
                (1695, 1699),
                (1766, 1770),
                (1855, 1859),
                (1962, 1966),
                (2087, 2091),
                (2192, 2196)
            ]
        );
        assert_eq!(count, 19 * 4 + 1 + 1 + 2 * spans.len());
        let (output, artifact) = analyzer
            .analyze_and_capture(
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            artifact
                .required_syntax_fact_count(&Cancellation::new())
                .unwrap(),
            count
        );
        let target = definition(output.document(), "var%", 1291);
        let owner = definition(output.document(), "v0%", 1687);
        assert_rhs_reference(output.document(), ORIGINAL, 1695, "var%", target, owner);
        let replay = analyzer
            .analyze_from_artifact(
                request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.document(), replay.document());
    });
    with_equation_request(ORIGINAL, count - 1, |request| {
        assert!(
            analyzer
                .prepare_artifact(request, &Cancellation::new())
                .is_err()
        );
    });
}

#[test]
fn actual_rhs_materialization_requires_environment_target_and_owner_raw_witnesses() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, SyntaxKindLabel, execute_parse, testkit::MockParseProvider,
    };
    let raw = b"chosen : Set; chosen = Set\nargument : Set; argument = Set\nowned : Set; owned = chosen argument\n";
    with_equation_request(raw, 4096, |request| {
        let provider = AgdaParseProvider::new(65_536, 16_384).unwrap();
        let parsed = execute_parse(
            &provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        let replay = |facts| {
            let mock = MockParseProvider::new(
                provider.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            execute_analysis(
                &equation_analyzer(Arc::new(mock)),
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
        };
        let chosen = raw
            .windows(8)
            .position(|bytes| bytes == b"chosen =")
            .unwrap();
        let owned = raw
            .windows(7)
            .position(|bytes| bytes == b"owned =")
            .unwrap();
        let read = raw
            .windows(6)
            .rposition(|bytes| bytes == b"chosen")
            .unwrap();
        let baseline = replay(parsed.facts().to_vec()).unwrap();
        let target = definition(baseline.document(), "chosen", chosen);
        let owner = definition(baseline.document(), "owned", owned);
        assert_rhs_reference(baseline.document(), raw, read, "chosen", target, owner);
        let earlier_owner_id = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "agda.function.definition"
                    && fact.span().start_byte() == chosen as u64
            })
            .unwrap()
            .parent()
            .unwrap();
        let earlier_owner = parsed
            .facts()
            .iter()
            .find(|fact| fact.local_id() == earlier_owner_id)
            .unwrap();
        assert_eq!(
            earlier_owner.syntax_kind().as_str(),
            "agda.function.declaration"
        );
        assert!(earlier_owner.span().end_byte() < read as u64);
        for (label, mode) in [
            ("agda.rhs.environment.signature", 0),
            ("agda.rhs.environment.signature", 1),
            ("agda.rhs.environment.signature", 2),
            ("agda.rhs.target.signature", 0),
            ("agda.rhs.target.signature", 1),
            ("agda.rhs.reference", 3),
            ("agda.function.definition", 4),
            ("agda.rhs.reference", 5),
        ] {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label)
                .unwrap();
            let fact = facts[index].clone();
            if mode == 0 {
                facts.remove(index);
            } else if mode == 1 || mode == 4 {
                let local = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                facts.push(SyntaxFact::new(
                    local,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    if mode == 4 {
                        SyntaxKindLabel::new("agda.foreign.reference").unwrap()
                    } else {
                        fact.syntax_kind().clone()
                    },
                ));
            } else {
                facts[index] = SyntaxFact::new(
                    fact.local_id(),
                    if mode == 3 {
                        Some(1)
                    } else if mode == 5 {
                        assert_eq!(fact.depth(), earlier_owner.depth() + 1);
                        Some(earlier_owner.local_id())
                    } else {
                        fact.parent()
                    },
                    fact.kind(),
                    fact.span(),
                    if mode == 3 { 1 } else { fact.depth() },
                    if mode == 2 {
                        SyntaxKindLabel::new("agda.foreign.signature").unwrap()
                    } else {
                        fact.syntax_kind().clone()
                    },
                );
            }
            let replayed = replay(facts);
            if mode == 5 {
                let Err(error) = replayed else {
                    panic!("out-of-parent reference unexpectedly materialized: {label}/{mode}");
                };
                assert!(
                    matches!(&error, rootlight_adapter_sdk::AdapterError::ProviderFailed { code }
                        if code.as_str() == "treesitter-lowering-parent"),
                    "out-of-parent reference must fail raw graph validation: {label}/{mode}: {error:?}"
                );
                continue;
            }
            let output = replayed.unwrap();
            let sites: Vec<_> = output
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.source.span().start_byte() == read as u64
                })
                .collect();
            assert_eq!(
                sites.len(),
                1,
                "retained exact unresolved reference: {label}/{mode}"
            );
            assert!(
                matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
                "{label}/{mode}"
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail
                        == "agda-written-rhs-binding-and-application-semantics-partial")
            );
        }
    });
}
