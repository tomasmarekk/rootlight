//! Exercise the SDK cancellation bridge without granting a partial native tree.
//! Reuse additionally binds the generated tables, checked scanner and Rc owner.

use super::*;
use rootlight_cancel::CancellationReason;

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/wolfram/SocketUtilities.wl");

fn parse(source: &[u8]) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_wolfram::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None);
    drop(parser);
    tree
}

#[test]
fn wolfram_full_original_and_next_scope_survive_a_measured_work_refusal() {
    assert_eq!(ORIGINAL.len(), 3944);
    let cancel = Cancellation::new();
    let scope = wolfram_scope::begin(&cancel, 8).unwrap();
    drop(parse(ORIGINAL));
    let mut witness = None;
    assert!(
        matches!(wolfram_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "wolfram-scanner-work-limit")
    );
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::WolframExternalWork {
            consumed: 8,
            limit: 8
        })
    );
    assert!(
        witness.unwrap().diagnostic("refused").contains(
            "consumed 8; demanded unknown; remaining-demand unknown; next charge declined"
        )
    );
    let scope = wolfram_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(ORIGINAL).unwrap();
    let mut witness = None;
    wolfram_scope::finish(scope, &cancel, Some(&mut witness)).unwrap();
    assert!(witness.is_none());
    // Real recovery remains query input; scanner success does not prove clean syntax.
    assert_eq!(tree.root_node().end_byte(), ORIGINAL.len());
}

#[test]
fn wolfram_cancellation_before_zero_credit_and_after_work_decline_never_certifies_source() {
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        wolfram_scope::begin(&cancelled, 0),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let cancel = Cancellation::new();
    let scope = wolfram_scope::begin(&cancel, 8).unwrap();
    drop(parse(ORIGINAL));
    cancel.cancel(CancellationReason::ClientRequest);
    let mut witness = None;
    assert!(matches!(
        wolfram_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert!(witness.is_none());
    let clean = Cancellation::new();
    let scope = wolfram_scope::begin(&clean, usize::MAX).unwrap();
    wolfram_scope::finish(scope, &clean, None).unwrap();
}

#[test]
fn wolfram_cache_reuse_requires_the_exact_active_scanner_and_callback_owner_contract() {
    let source = b"owned[value_String] := Module[{}, external[value]];\n";
    let cancel = Cancellation::new();
    let scope = wolfram_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(source).unwrap();
    wolfram_scope::finish(scope, &cancel, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Wolfram;
    identity.grammar_version = "0.0.1";
    identity.encoding = "utf-8".to_owned();
    identity.content_hash = rootlight_ids::content_hash(source);
    let mut cached = CachedParse::invalidation(ReuseInvalidation::FrontendRoute);
    cached.invalidation = None;
    cached.identity = identity.clone();
    cached.source = Arc::new(source.to_vec());
    cached.tree = Some(tree);
    let (tree, status) =
        prepare_reuse(Some(&cached), &identity, source, &[], source.len(), &cancel).unwrap();
    assert!(tree.is_some());
    assert_eq!(status, ReuseStatus::Reused { changed_ranges: 0 });
    for predecessor in [
        "wolfram-pristine-scanner",
        "wolfram-observation-scanner/1",
        "wolfram-scanner-checked-state/1;owner=stale",
    ] {
        let mut changed = cached.clone_for_reuse();
        changed.wolfram_scanner_contract = predecessor;
        let (tree, status) = prepare_reuse(
            Some(&changed),
            &identity,
            source,
            &[],
            source.len(),
            &cancel,
        )
        .unwrap();
        assert!(tree.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
}

#[test]
fn wolfram_comment_depth_has_a_genuine_soft_witness_but_nul_remains_strict() {
    let cancel = Cancellation::new();
    let mut depth = "(*".repeat(1025).into_bytes();
    depth.extend_from_slice("*)".repeat(1025).as_bytes());
    let scope = wolfram_scope::begin(&cancel, usize::MAX).unwrap();
    drop(parse(&depth));
    let mut witness = None;
    assert!(
        matches!(wolfram_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed {code}) if code.as_str() == "wolfram-scanner-capacity")
    );
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::WolframCommentDepth)
    );
    assert!(
        witness
            .unwrap()
            .diagnostic("refused")
            .contains("limit 1024; observed unknown")
    );
    let scope = wolfram_scope::begin(&cancel, usize::MAX).unwrap();
    drop(parse(b"(*\0*)"));
    let mut witness = None;
    assert!(
        matches!(wolfram_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed {code}) if code.as_str() == "wolfram-scanner-nul-unavailable")
    );
    assert!(witness.is_none());
    let scope = wolfram_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(ORIGINAL).unwrap();
    wolfram_scope::finish(scope, &cancel, None).unwrap();
    assert!(!tree.root_node().has_error());
}
