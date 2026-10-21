//! Exercise native admission without assigning label or branch semantic credit.

use super::*;
use rootlight_adapter_treesitter::{GrammarFamily, NativeScannerRefusalDetails};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/sed/mario.sed");
const MEMORY: MemoryAdmissionPolicy = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

#[test]
fn sed_native_original_preserves_physical_root_and_noop_then_rebuilds_physical_edits() {
    assert_eq!(ORIGINAL.len(), 8893);
    let fixture = Fixture::new("written.sed", ORIGINAL);
    let limits = limits(MAX_SOURCE_BYTES, 32_768, 256);
    let provider = provider(MAX_SOURCE_BYTES, 32_768, 256, 4 * 1024 * 1024);
    let settings = ParserSettings::new(1024).unwrap();
    let cancel = Cancellation::new();
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "sed",
        Vec::new(),
    );
    let first = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancel)
        .unwrap();
    assert_eq!(
        first.output().report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(first.reuse_key().native_family(), GrammarFamily::Sed);
    assert_eq!(
        first.reuse_key().current_content_hash(),
        content_hash(ORIGINAL)
    );
    assert!(first.output().facts().iter().any(|fact|
        fact.kind() == SyntaxFactKind::Root && fact.span() == fixture.source.span()));
    assert!(
        first
            .output()
            .facts()
            .iter()
            .all(|fact| usize::try_from(fact.span().end_byte()).unwrap() <= ORIGINAL.len())
    );
    let previous = first.previous().unwrap();
    let noop = provider
        .execute_with_previous(&input, Some(previous), &[], settings, MEMORY, &cancel)
        .unwrap();
    assert_eq!(
        noop.reuse_status(),
        ReuseStatus::Reused { changed_ranges: 0 }
    );
    assert_eq!(noop.output().facts(), first.output().facts());
    assert_eq!(noop.output().report(), first.output().report());
    let mut changed = ORIGINAL.to_vec();
    changed[975..979].copy_from_slice(b"next");
    let current = fixture.rewrite(&changed);
    let input = request(
        &current.snapshot,
        &current.source,
        &limits,
        "sed",
        Vec::new(),
    );
    let edited = provider
        .execute_with_previous(
            &input,
            Some(previous),
            &[SourceEdit::new(975, 979, "next").unwrap()],
            settings,
            MEMORY,
            &cancel,
        )
        .unwrap();
    assert_eq!(
        edited.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
    );
    let fresh = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancel)
        .unwrap();
    assert_eq!(edited.output().facts(), fresh.output().facts());
    assert_eq!(edited.output().report(), fresh.output().report());
    assert_eq!(
        edited.reuse_key().current_content_hash(),
        content_hash(&changed)
    );
    assert_eq!(fixture.snapshot.content(), ORIGINAL);
}

#[test]
fn sed_only_measured_external_work_can_certify_exact_source_and_quotas() {
    let mut raw = vec![b'#'];
    raw.extend(std::iter::repeat_n(b'a', 16_384));
    raw.extend_from_slice(b"\r\n:entry\r\nb entry\r\n");
    let fixture = Fixture::new("work.sed", &raw);
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "sed",
        Vec::new(),
    );
    let proof = provider
        .certify_native_scanner_refusal(&input, MEMORY, &Cancellation::new())
        .unwrap()
        .expect("the real comment scanner must exceed its 4096-step credit");
    assert_eq!(
        proof.details(),
        NativeScannerRefusalDetails::SedExternalWork {
            consumed: 4096,
            limit: 4096
        }
    );
    assert_eq!(proof.source(), &fixture.source);
    assert!(proof.matches_request(&input));
    assert!(proof.matches_source(&fixture.source, "sed", "utf-8"));
    assert!(!proof.matches_source(&fixture.source, "sed", "utf-16le"));
    assert!(proof.matches_error(&proof.adapter_error()));
    assert!(proof.was_issued_by(&provider));
    let other_provider = super::provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    assert!(!proof.was_issued_by(&other_provider));
    let other_limits = super::limits(MAX_SOURCE_BYTES, 2048, 64);
    let other_input = request(
        &fixture.snapshot,
        &fixture.source,
        &other_limits,
        "sed",
        Vec::new(),
    );
    assert!(!proof.matches_request(&other_input));
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        provider.certify_native_scanner_refusal(&input, MEMORY, &cancelled),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(fixture.snapshot.content(), raw.as_slice());
    assert_eq!(fixture.snapshot.content_hash(), content_hash(&raw));
}

#[test]
fn sed_ranges_and_path_spelling_cannot_change_the_declared_bre_frontend() {
    let raw = b":entry\r\nb entry\r\n";
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    for path in ["program.sed", "program.ere", "program.txt"] {
        let fixture = Fixture::new(path, raw);
        let input = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "sed",
            Vec::new(),
        );
        let output = provider
            .execute_with_previous(
                &input,
                None,
                &[],
                ParserSettings::new(64).unwrap(),
                MEMORY,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(output.reuse_key().native_family(), GrammarFamily::Sed);
        assert_eq!(output.reuse_key().grammar_version(), "0.4.0");
        let included = IncludedRange::new(fixture.source.span(), LanguageId::new("sed").unwrap());
        let partial = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "sed",
            vec![included],
        );
        assert!(
            provider
                .certify_native_scanner_refusal(&partial, MEMORY, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(
            provider
                .execute_with_previous(
                    &partial,
                    None,
                    &[],
                    ParserSettings::new(64).unwrap(),
                    MEMORY,
                    &Cancellation::new()
                )
                .is_err()
        );
    }
}

#[test]
fn sed_named_syntax_issues_are_unknown_coverage_without_a_source_refusal_certificate() {
    let fixture = Fixture::new("invalid.sed", b"?\r\n");
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "sed",
        Vec::new(),
    );
    let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!output.diagnostics().is_empty());
    assert!(
        provider
            .certify_native_scanner_refusal(&input, MEMORY, &Cancellation::new())
            .unwrap()
            .is_none()
    );
}
