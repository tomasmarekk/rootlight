//! Candidate runtime controls; they require the complete Lean integration stage.
use super::*;
use rootlight_cancel::CancellationReason;

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/lean/conv_eval.lean");

fn parse(source: &[u8]) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_lean::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None);
    drop(parser);
    tree
}

#[test]
fn lean_original_and_next_scope_survive_a_counted_work_refusal() {
    let cancel = Cancellation::new();
    let scope = lean_scope::begin(&cancel, ORIGINAL, 8).unwrap();
    drop(parse(ORIGINAL));
    let mut witness = None;
    assert!(matches!(
        lean_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "lean-scanner-work-limit"
    ));
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::LeanExternalWork {
            consumed: 8,
            limit: 8
        })
    );
    let scope = lean_scope::begin(&cancel, ORIGINAL, usize::MAX).unwrap();
    let tree = parse(ORIGINAL).unwrap();
    let mut witness = None;
    lean_scope::finish(scope, &cancel, Some(&mut witness)).unwrap();
    assert!(witness.is_none());
    assert!(!tree.root_node().has_error());
    assert_eq!(tree.root_node().end_byte(), ORIGINAL.len());
}

#[test]
fn lean_complete_source_nul_and_final_cancellation_never_issue_soft_witnesses() {
    let cancel = Cancellation::new();
    assert!(matches!(
        lean_scope::begin(&cancel, b"def owned := 0\n\0", 0),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "lean-scanner-nul-unavailable"
    ));
    let scope = lean_scope::begin(&cancel, ORIGINAL, 8).unwrap();
    drop(parse(ORIGINAL));
    cancel.cancel(CancellationReason::ClientRequest);
    let mut witness = None;
    assert!(matches!(
        lean_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert!(witness.is_none());
    let fresh = Cancellation::new();
    let scope = lean_scope::begin(&fresh, ORIGINAL, usize::MAX).unwrap();
    lean_scope::finish(scope, &fresh, None).unwrap();
}

#[test]
fn lean_native_comment_capacity_has_no_fabricated_observed_depth() {
    let source = [b"/-".repeat(1025), b"-/".repeat(1025)].concat();
    let cancel = Cancellation::new();
    let scope = lean_scope::begin(&cancel, &source, usize::MAX).unwrap();
    drop(parse(&source));
    let mut witness = None;
    assert!(matches!(
        lean_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "lean-scanner-capacity"
    ));
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::LeanScannerCapacity)
    );
    let detail = witness.unwrap().diagnostic("refused");
    assert!(detail.contains("observed unknown"));
    assert!(!detail.contains("observed 1025"));
}

#[test]
fn lean_cache_route_requires_exact_scanner_status_and_callback_owner_identity() {
    let source = b"def owned (x : Nat) := x\n";
    let cancel = Cancellation::new();
    let scope = lean_scope::begin(&cancel, source, usize::MAX).unwrap();
    let tree = parse(source).unwrap();
    lean_scope::finish(scope, &cancel, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Lean;
    identity.grammar_version = "0.2.0";
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
        "lean-pristine-scanner",
        "lean-scanner-checked-state/1;status=stale",
        "lean-scanner-checked-state/1;wrapper-body=stale",
    ] {
        let mut changed = cached.clone_for_reuse();
        changed.lean_scanner_contract = predecessor;
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
