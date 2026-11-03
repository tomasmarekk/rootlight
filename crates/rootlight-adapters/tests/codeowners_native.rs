//! Public native CODEOWNERS analysis authenticates source-owned owner intent.
//! Mandatory certificates retain valid and invalid lines without inventing
//! software declarations, repository assignments or external account authority.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::{CodeownersMatcher, CodeownersParseProvider};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CODEOWNERS_PROGRAM_NAMESPACE, CodeownersOwnerKind,
    CoverageStatus, ExtensionSupport, IrLimits, ProducerIdentity, SourceRef, SourceSpan,
    decode_codeowners_program_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

fn analyze(raw: &[u8], nodes: usize, extensions: usize) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("CODEOWNERS"), raw).unwrap();
    let repository = derive_repository(b"codeowners-native-program-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("CODEOWNERS")).unwrap(),
            65536,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([53; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(CodeownersParseProvider::new(65536, 16384).unwrap()),
        ProducerIdentity::new(
            "codeowners-native-contract",
            "1",
            content_hash(b"codeowners-test"),
        )
        .unwrap(),
        LanguageId::new("codeowners").unwrap(),
        "codeowners-native-1",
        CodeownersParseProvider::binary_digest(),
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
    let mut ir = IrLimits::default();
    ir.max_extensions = extensions;
    let limits = AnalysisLimits::new(
        65536,
        nodes,
        128,
        1,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("codeowners").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"codeowners-test-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    Ok(output)
}

#[test]
fn public_native_program_keeps_order_owner_spans_clearing_and_scoped_invalid_lines() {
    let raw = b"\xef\xbb\xbf# owners\r\n*.rs @first @org/team dev@example.com # note\r\n/apps/ @apps\r\n/apps/clear\r\n!bad @ignored\r\n*.rs @last\r\n";
    let output = analyze(raw, 7, 2).unwrap();
    let document = output.document();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].content_hash, content_hash(raw));
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert!(document.relations.is_empty());
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    let envelope = document
        .extensions
        .iter()
        .find(|extension| extension.namespace == CODEOWNERS_PROGRAM_NAMESPACE)
        .unwrap();
    let program = decode_codeowners_program_envelope(envelope).unwrap();
    assert_eq!(program.text().as_bytes(), raw);
    assert_eq!(program.rules().len(), 4);
    assert_eq!(program.invalid_lines().len(), 1);
    let first = &program.rules()[0];
    assert_eq!(
        (first.pattern.start_byte(), first.pattern.end_byte()),
        (13, 17)
    );
    assert_eq!(
        first
            .owners
            .iter()
            .map(|owner| (owner.span.start_byte(), owner.span.end_byte(), owner.kind))
            .collect::<Vec<_>>(),
        [
            (18, 24, CodeownersOwnerKind::User),
            (25, 34, CodeownersOwnerKind::Team),
            (35, 50, CodeownersOwnerKind::Email),
        ]
    );
    assert!(program.rules()[2].owners.is_empty());
    assert_eq!(
        program
            .rules()
            .iter()
            .map(|rule| rule.line_number)
            .collect::<Vec<_>>(),
        [2, 3, 4, 6]
    );
    let invalid = program.invalid_lines()[0];
    assert!(document.diagnostics.iter().any(|diagnostic| diagnostic.code
        == invalid.reason.diagnostic_code()
        && diagnostic.evidence.source.as_ref().unwrap().span() == invalid.line));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == rootlight_ir::FactDomain::Extensions
                && gap.detail == invalid.reason.diagnostic_code()
                && gap.source.span() == invalid.line)
    );
    let matcher = CodeownersMatcher::from_program(program, &Cancellation::new()).unwrap();
    assert_eq!(
        matcher
            .decision("nested/file.rs", false, &Cancellation::new())
            .unwrap()
            .unwrap()
            .rule_index,
        3
    );
    assert_eq!(
        matcher
            .decision("apps/clear/nested/file.txt", false, &Cancellation::new())
            .unwrap()
            .unwrap()
            .rule_index,
        2
    );
    assert!(analyze(raw, 6, 2).is_err());
    assert!(analyze(raw, 7, 1).is_err());
}

#[test]
fn unsupported_matching_retains_typed_rules_and_pattern_scoped_uncertainty() {
    let output = analyze(b"a{b,c} @owner\n", 3, 2).unwrap();
    let document = output.document();
    let envelope = document
        .extensions
        .iter()
        .find(|extension| extension.namespace == CODEOWNERS_PROGRAM_NAMESPACE)
        .unwrap();
    let program = decode_codeowners_program_envelope(envelope).unwrap();
    assert_eq!(program.rules().len(), 1);
    assert!(program.invalid_lines().is_empty());
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "codeowners-pattern-semantics-unavailable"
        && gap.source.span() == program.rules()[0].pattern));
    assert!(CodeownersMatcher::from_program(program, &Cancellation::new()).is_err());
}

#[test]
fn entirely_invalid_and_comment_only_files_have_complete_native_inventories() {
    for (raw, valid, invalid) in [
        (b"# only comments\r\n\r\n".as_slice(), 0, 0),
        (b"[abc] @range\nfoo @valid broken-owner\n".as_slice(), 0, 2),
    ] {
        let output = analyze(raw, 2 + valid + invalid, 2).unwrap();
        let program = output
            .document()
            .extensions
            .iter()
            .find(|extension| extension.namespace == CODEOWNERS_PROGRAM_NAMESPACE)
            .map(decode_codeowners_program_envelope)
            .unwrap()
            .unwrap();
        assert_eq!(program.rules().len(), valid);
        assert_eq!(program.invalid_lines().len(), invalid);
    }
    let output = analyze(b"* @owner\n", 3, 2).unwrap();
    let program = output
        .document()
        .extensions
        .iter()
        .find(|extension| extension.namespace == CODEOWNERS_PROGRAM_NAMESPACE)
        .map(decode_codeowners_program_envelope)
        .unwrap()
        .unwrap();
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(CodeownersMatcher::from_program(program, &cancellation).is_err());
}
