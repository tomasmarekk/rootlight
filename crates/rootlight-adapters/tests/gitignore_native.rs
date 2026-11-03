//! Public native Gitignore analysis retains ordered semantics and physical source.
//! Required parser certificates cannot be removed by optional query quotas, and
//! native rules do not become inferred software declarations or file targets.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::{GitignoreMatcher, GitignoreParseProvider};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, ExtensionSupport,
    GITIGNORE_PROGRAM_NAMESPACE, GitignorePolarity, IrLimits, ProducerIdentity, SourceRef,
    SourceSpan, decode_gitignore_program_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

fn analyze(raw: &[u8], nodes: usize, extensions: usize) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("rules.gitignore"), raw).unwrap();
    let repository = derive_repository(b"gitignore-native-program-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("rules.gitignore")).unwrap(),
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
        Arc::new(GitignoreParseProvider::new(65536, 16384).unwrap()),
        ProducerIdentity::new(
            "gitignore-native-contract",
            "1",
            content_hash(b"gitignore-test"),
        )
        .unwrap(),
        LanguageId::new("gitignore").unwrap(),
        "gitignore-native-1",
        GitignoreParseProvider::binary_digest(),
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
        LanguageId::new("gitignore").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"gitignore-test-build")),
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
fn original_native_rules_have_public_typed_readback_and_real_matching() {
    for raw in [
        include_bytes!("fixtures/gitignore/Rust.gitignore").as_slice(),
        include_bytes!("fixtures/gitignore/Vim.gitignore").as_slice(),
    ] {
        let output = analyze(raw, 16384, 1024).unwrap();
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        let document = output.document();
        assert_eq!(document.files.len(), 1);
        assert_eq!(document.files[0].content_hash, content_hash(raw));
        assert!(document.entities.is_empty());
        assert!(document.occurrences.is_empty());
        assert!(document.relations.is_empty());
        let envelope = document
            .extensions
            .iter()
            .find(|extension| extension.namespace == GITIGNORE_PROGRAM_NAMESPACE)
            .unwrap();
        let program = decode_gitignore_program_envelope(envelope).unwrap();
        assert_eq!(program.text().as_bytes(), raw);
        assert!(!program.rules().is_empty());
        let matcher = GitignoreMatcher::from_program(program, &Cancellation::new()).unwrap();
        let decision = matcher
            .decision("debug", true, &Cancellation::new())
            .unwrap();
        if raw == include_bytes!("fixtures/gitignore/Rust.gitignore") {
            assert_eq!(decision.unwrap().polarity, GitignorePolarity::Exclude);
        } else {
            assert_eq!(
                matcher
                    .decision("design.svg", false, &Cancellation::new())
                    .unwrap()
                    .unwrap()
                    .polarity,
                GitignorePolarity::Reinclude
            );
        }
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "gitignore-repository-context-not-analyzed")
        );
    }
}

#[test]
fn native_program_requires_all_rule_certificates_and_atomic_extension_budget() {
    let raw = b"\xef\xbb\xbf# comment\r\ncache/\r\n!cache/keep\r\ncache/\r\n";
    let output = analyze(raw, 5, 2).unwrap();
    let program = output
        .document()
        .extensions
        .iter()
        .find(|extension| extension.namespace == GITIGNORE_PROGRAM_NAMESPACE)
        .map(decode_gitignore_program_envelope)
        .unwrap()
        .unwrap();
    assert_eq!(program.rules().len(), 3);
    assert_eq!(program.rules()[0].line_number, 2);
    assert_eq!(program.rules()[2].line_number, 4);
    assert!(analyze(raw, 4, 2).is_err());
    assert!(analyze(raw, 5, 1).is_err());
}

#[test]
fn native_compiler_refuses_invalid_range_but_accepts_literal_unclosed_class() {
    let refused = analyze(b"kept/\n[z-a]\n!keep\n", 1, 1).unwrap();
    assert_eq!(
        refused.report().coverage().status(),
        CoverageStatus::Unknown
    );
    assert!(
        !refused
            .document()
            .extensions
            .iter()
            .any(|extension| extension.namespace == GITIGNORE_PROGRAM_NAMESPACE)
    );
    assert!(
        refused
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "gitignore-pattern-parse-error")
    );
    assert!(analyze(b"[abc\n", 16384, 1024).is_ok());
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    let output = analyze(b"*.log\n", 16384, 1024).unwrap();
    let program = output
        .document()
        .extensions
        .iter()
        .find(|extension| extension.namespace == GITIGNORE_PROGRAM_NAMESPACE)
        .map(decode_gitignore_program_envelope)
        .unwrap()
        .unwrap();
    assert!(GitignoreMatcher::from_program(program, &cancellation).is_err());
}

#[test]
fn comment_only_source_has_an_authenticated_empty_native_program() {
    let source = b"\xef\xbb\xbf# authored comment\r\n\r\n   \r\n";
    let output = analyze(source, 2, 2).unwrap();
    let program = output
        .document()
        .extensions
        .iter()
        .find(|extension| extension.namespace == GITIGNORE_PROGRAM_NAMESPACE)
        .map(decode_gitignore_program_envelope)
        .unwrap()
        .unwrap();
    assert!(program.rules().is_empty());
    assert_eq!(program.text().as_bytes(), source);
}
