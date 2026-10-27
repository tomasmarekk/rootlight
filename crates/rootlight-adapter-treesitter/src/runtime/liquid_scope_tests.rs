//! Exercise the SDK cancellation bridge without granting a partial native tree.
//! Reuse additionally binds the generated tables, checked scanner and Rc owner.

use super::*;
use rootlight_cancel::CancellationReason;

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/liquid/blog-sidebar.liquid");

fn parse(source: &[u8]) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_liquid::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None);
    drop(parser);
    tree
}

#[test]
fn liquid_full_original_and_next_scope_survive_a_measured_work_refusal() {
    assert_eq!(ORIGINAL.len(), 888);
    let cancel = Cancellation::new();
    let scope = liquid_scope::begin(&cancel, 8).unwrap();
    drop(parse(ORIGINAL));
    let mut witness = None;
    assert!(
        matches!(liquid_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "liquid-scanner-work-limit")
    );
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::LiquidExternalWork {
            consumed: 8,
            limit: 8
        })
    );
    assert!(
        witness
            .unwrap()
            .diagnostic("refused")
            .contains("consumed 8; demanded unknown; next charge declined")
    );
    let scope = liquid_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(ORIGINAL).unwrap();
    let mut witness = None;
    liquid_scope::finish(scope, &cancel, Some(&mut witness)).unwrap();
    assert!(witness.is_none());
    // Real recovery remains query input; scanner success does not prove clean syntax.
    assert_eq!(tree.root_node().end_byte(), ORIGINAL.len());
}

#[test]
fn liquid_cancellation_before_zero_credit_and_after_work_decline_never_certifies_source() {
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        liquid_scope::begin(&cancelled, 0),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let cancel = Cancellation::new();
    let scope = liquid_scope::begin(&cancel, 8).unwrap();
    drop(parse(ORIGINAL));
    cancel.cancel(CancellationReason::ClientRequest);
    let mut witness = None;
    assert!(matches!(
        liquid_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert!(witness.is_none());
    let clean = Cancellation::new();
    let scope = liquid_scope::begin(&clean, usize::MAX).unwrap();
    liquid_scope::finish(scope, &clean, None).unwrap();
}

#[test]
fn liquid_cache_reuse_requires_the_exact_active_scanner_and_callback_owner_contract() {
    let source = b"{% for item in source %}{{ item }}{% endfor %}";
    let cancel = Cancellation::new();
    let scope = liquid_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(source).unwrap();
    liquid_scope::finish(scope, &cancel, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Liquid;
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
        "liquid-pristine-scanner",
        "liquid-observation-scanner/1",
        "liquid-scanner-checked-state/1;owner=stale",
    ] {
        let mut changed = cached.clone_for_reuse();
        changed.liquid_scanner_contract = predecessor;
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
fn liquid_raw_comment_and_frontmatter_scans_spend_finite_external_credit() {
    for (open, close) in [
        ("{% raw %}", "{% endraw %}"),
        ("{% comment %}", "{% endcomment %}"),
        ("---\n", "\n---\n"),
    ] {
        let source = format!("{open}{}{close}", "x".repeat(1024));
        let cancel = Cancellation::new();
        let scope = liquid_scope::begin(&cancel, 64).unwrap();
        drop(parse(source.as_bytes()));
        let mut witness = None;
        assert!(
            matches!(liquid_scope::finish(scope,&cancel,Some(&mut witness)),Err(AdapterError::ProviderFailed { code }) if code.as_str()=="liquid-scanner-work-limit")
        );
        assert_eq!(
            witness,
            Some(NativeScannerRefusalDetails::LiquidExternalWork {
                consumed: 64,
                limit: 64
            })
        );
        let scope = liquid_scope::begin(&cancel, usize::MAX).unwrap();
        let tree = parse(source.as_bytes()).unwrap();
        liquid_scope::finish(scope, &cancel, None).unwrap();
        assert!(!tree.root_node().has_error());
    }
}

#[test]
fn liquid_embedded_nul_is_a_strict_scanner_refusal_without_a_work_witness() {
    let cancel = Cancellation::new();
    let scope = liquid_scope::begin(&cancel, usize::MAX).unwrap();
    drop(parse(b"{% raw %}text\0still raw{% endraw %}"));
    let mut witness = None;
    assert!(
        matches!(liquid_scope::finish(scope,&cancel,Some(&mut witness)),Err(AdapterError::ProviderFailed { code }) if code.as_str()=="liquid-scanner-nul-unavailable")
    );
    assert!(witness.is_none());
}
