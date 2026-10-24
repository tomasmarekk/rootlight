//! CUDA reuse and finite scanner guards follow actual native parser identity.
//! A declared Cpp header shares this native state contract without becoming CUDA source.

use super::*;

#[test]
fn cuda_cached_native_tree_requires_current_contract_and_fresh_physical_edits() {
    let source = b"__global__ void kernel(int value) {}\n";
    let cancel = Cancellation::new();
    let scope = cuda_scope::begin(&cancel, 100_000).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language_for(GrammarFamily::Cuda))
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    cuda_scope::finish(scope, &cancel, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Cuda;
    identity.grammar_version = "0.21.2";
    identity.encoding = "utf-8".to_owned();
    identity.content_hash = rootlight_ids::content_hash(source);
    let mut cached = CachedParse::invalidation(ReuseInvalidation::FrontendRoute);
    cached.invalidation = None;
    cached.identity = identity.clone();
    cached.source = Arc::new(source.to_vec());
    cached.tree = Some(tree);
    let (old, status) =
        prepare_reuse(Some(&cached), &identity, source, &[], source.len(), &cancel).unwrap();
    assert!(old.is_some());
    assert_eq!(status, ReuseStatus::Reused { changed_ranges: 0 });
    for variant in 0..3 {
        let mut changed = cached.clone_for_reuse();
        let edits = if variant == 2 {
            vec![SourceEdit::new(0, 0, " ").unwrap()]
        } else {
            Vec::new()
        };
        if variant == 0 {
            changed.cuda_scanner_contract = "stale-native-scanner";
        }
        if variant == 1 {
            changed.cuda_frontend_policy = "stale-native-route";
        }
        let (old, status) = prepare_reuse(
            Some(&changed),
            &identity,
            source,
            &edits,
            source.len(),
            &cancel,
        )
        .unwrap();
        assert!(old.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
    assert_eq!(
        native_family_for_source(GrammarFamily::Cpp, "source.cuh"),
        GrammarFamily::Cuda
    );
    assert_eq!(
        native_family_for_source(GrammarFamily::Cpp, "source.cpp"),
        GrammarFamily::Cpp
    );
}

#[test]
fn cuda_refused_scanner_scope_does_not_admit_the_next_parse_state() {
    let cancel = Cancellation::new();
    assert!(matches!(
        cuda_scope::begin(&cancel, 0),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "cuda-scanner-work-limit"
    ));
    let scope = cuda_scope::begin(&cancel, 1).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language_for(GrammarFamily::Cuda))
        .unwrap();
    drop(parser.parse(b"const char *value = R\"tag(body)tag\";", None));
    let mut refusal = None;
    assert!(matches!(
        cuda_scope::finish(scope, &cancel, Some(&mut refusal)),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "cuda-scanner-work-limit"
    ));
    assert_eq!(
        refusal,
        Some(NativeScannerRefusalDetails::CudaExternalWork {
            consumed: 1,
            limit: 1,
        })
    );
    drop(parser);
    let scope = cuda_scope::begin(&cancel, 100_000).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language_for(GrammarFamily::Cuda))
        .unwrap();
    let clean = parser
        .parse(
            b"__device__ int owned(int value) { return value * 2; }",
            None,
        )
        .unwrap();
    cuda_scope::finish(scope, &cancel, None).unwrap();
    assert!(!clean.root_node().has_error());
}
