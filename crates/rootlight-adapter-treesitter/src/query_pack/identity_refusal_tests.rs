//! Native helper refusals must name their real bounded operation.
//! The private scan allowance makes the original reporting defect small and
//! deterministic; service tests separately use unchanged production limits.

use super::*;

#[test]
fn c_formal_work_refusal_is_not_reported_as_capture_records() {
    let source = b"void forward(int value) { sink(value); }\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language_for(GrammarFamily::C))
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let registry = QueryPackRegistry::audited().unwrap();
    let pack = registry.get(GrammarFamily::C).unwrap();
    let error = pack
        .extract_identity(GrammarFamily::C, &tree, source, 8, &Cancellation::new())
        .unwrap_err();
    assert!(matches!(error, AdapterError::ProviderFailed { code }
        if code.as_str() == "query-identity-c-formal-reads-limit"));
}

#[test]
fn native_capture_allowance_accepts_exact_max_and_refuses_the_next_capture() {
    let source = b"int value;\n";
    let language = language_for(GrammarFamily::C);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let registry = QueryPackRegistry::audited().unwrap();
    let pack = registry.get(GrammarFamily::C).unwrap();
    assert_eq!(pack.role_for_capture(0), Some(StructuralRole::Root));
    for (query, expected) in [
        ("(translation_unit) @root", None),
        (
            "(translation_unit) @root @module",
            Some(QueryLimit::Capture),
        ),
    ] {
        let query = Query::new(&language, query).unwrap();
        let mut retained = 0;
        let limit = pack
            .scan_query(
                &query,
                QueryInput {
                    family: GrammarFamily::C,
                    tree: &tree,
                    source,
                    cancellation: &Cancellation::new(),
                    normalize_calls: false,
                },
                QueryScanLimits {
                    matches: 10,
                    captures: 1,
                    body_nodes: 10,
                },
                |_| {
                    retained += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(limit, expected);
        assert_eq!(retained, 1);
    }
}

#[test]
fn real_c_formal_refusal_measures_helper_work_separately_from_native_captures() {
    let source = b"void forward(int value) { sink(value); }\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language_for(GrammarFamily::C))
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let registry = QueryPackRegistry::audited().unwrap();
    let pack = registry.get(GrammarFamily::C).unwrap();
    let mut witness = None;
    let limit = pack
        .scan_query_with_refusal(
            &pack.identity_query,
            QueryInput {
                family: GrammarFamily::C,
                tree: &tree,
                source,
                cancellation: &Cancellation::new(),
                normalize_calls: false,
            },
            QueryScanLimits {
                matches: 128,
                captures: 128,
                body_nodes: 8,
            },
            |_| Ok(()),
            Some(&mut witness),
        )
        .unwrap();
    assert_eq!(limit, Some(QueryLimit::Capture));
    let details = witness.unwrap().details();
    assert_eq!(details.origin(), "query-identity-c-formal-reads-limit");
    assert_eq!((details.observed(), details.limit()), (Some(9), Some(8)));
    assert!(details.progress().0.unwrap() < 128);
    assert!(details.progress().1.unwrap() < 128);
    assert_eq!(details.progress().2, Some(0));
}

#[test]
fn real_query_capture_refusal_retains_the_exact_native_progress_snapshot() {
    let source = b"int value;\n";
    let language = language_for(GrammarFamily::C);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let registry = QueryPackRegistry::audited().unwrap();
    let pack = registry.get(GrammarFamily::C).unwrap();
    let query = Query::new(&language, "(translation_unit) @root @module").unwrap();
    let mut witness = None;
    let limit = pack
        .scan_query_with_refusal(
            &query,
            QueryInput {
                family: GrammarFamily::C,
                tree: &tree,
                source,
                cancellation: &Cancellation::new(),
                normalize_calls: false,
            },
            QueryScanLimits {
                matches: 10,
                captures: 1,
                body_nodes: 10,
            },
            |_| Ok(()),
            Some(&mut witness),
        )
        .unwrap();
    assert_eq!(limit, Some(QueryLimit::Capture));
    let details = witness.unwrap().details();
    assert_eq!(details.origin(), "query-identity-capture-slots-limit");
    assert_eq!((details.observed(), details.limit()), (Some(2), Some(1)));
    assert_eq!(details.progress(), (Some(1), Some(1), Some(0)));
}

#[test]
fn real_cpp_helper_refusal_keeps_unexposed_work_counts_unknown() {
    let source = b"int value;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language_for(GrammarFamily::Cpp))
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let registry = QueryPackRegistry::audited().unwrap();
    let pack = registry.get(GrammarFamily::Cpp).unwrap();
    let mut witness = None;
    let limit = pack
        .scan_query_with_refusal(
            &pack.identity_query,
            QueryInput {
                family: GrammarFamily::Cpp,
                tree: &tree,
                source,
                cancellation: &Cancellation::new(),
                normalize_calls: false,
            },
            QueryScanLimits {
                matches: 128,
                captures: 128,
                body_nodes: 0,
            },
            |_| Ok(()),
            Some(&mut witness),
        )
        .unwrap();
    assert_eq!(limit, Some(QueryLimit::Capture));
    let details = witness.unwrap().details();
    assert_eq!(details.origin(), "query-identity-cpp-const-formals-limit");
    assert_eq!((details.observed(), details.limit()), (None, None));
    assert_eq!(details.unit(), "helper-allowance");
    assert!(details.progress().0.is_some());
    assert!(details.progress().1.is_some());
    assert!(
        details
            .diagnostic("source retained")
            .contains("observed=unknown limit=unknown")
    );
}
