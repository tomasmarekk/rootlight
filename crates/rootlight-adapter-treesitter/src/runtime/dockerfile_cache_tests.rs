//! Reuse needs the exact scanner/status/generated-table contract.
//! All other source, grammar and encoding inputs remain identical in this control.

use super::*;

#[test]
fn dockerfile_cached_tree_requires_exact_scanner_status_and_parser_contract() {
    let source = b"FROM scratch AS written\n";
    let cancellation = Cancellation::new();
    let scope = dockerfile_scope::begin(&cancellation).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_dockerfile::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    dockerfile_scope::finish(scope, &cancellation, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Dockerfile;
    identity.grammar_version = "0.2.0";
    identity.encoding = "utf-8".to_owned();
    identity.content_hash = rootlight_ids::content_hash(source);
    let mut cached = CachedParse::invalidation(ReuseInvalidation::FrontendRoute);
    cached.invalidation = None;
    cached.identity = identity.clone();
    cached.source = Arc::new(source.to_vec());
    cached.tree = Some(tree);
    let (old, status) = prepare_reuse(
        Some(&cached),
        &identity,
        source,
        &[],
        source.len(),
        &cancellation,
    )
    .unwrap();
    assert!(old.is_some());
    assert_eq!(status, ReuseStatus::Reused { changed_ranges: 0 });
    for predecessor in [
        "dockerfile-pristine-scanner",
        "dockerfile-scanner-checked-state/1;scanner=stale",
        "dockerfile-scanner-checked-state/1;status=stale",
        "dockerfile-scanner-checked-state/1;parser=stale",
    ] {
        let mut changed = cached.clone_for_reuse();
        changed.dockerfile_scanner_contract = predecessor;
        let (old, status) = prepare_reuse(
            Some(&changed),
            &identity,
            source,
            &[],
            source.len(),
            &cancellation,
        )
        .unwrap();
        assert!(old.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
}
