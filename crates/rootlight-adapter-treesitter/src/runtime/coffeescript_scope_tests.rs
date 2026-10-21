//! Actual native work closure must report accepted credits and discard syntax.
//! Scope release leaves the next ordinary request independently admissible.

use super::{coffeescript_scope, scanner_refusal::NativeScannerRefusalDetails};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::{Cancellation, CancellationReason};

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/coffeescript/line.coffee");

#[test]
fn accepted_scanner_work_is_measured_before_refusal_and_next_request_is_clean() {
    let cancellation = Cancellation::new();
    let scope = coffeescript_scope::begin(&cancellation, 8).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_coffeescript::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None);
    drop(tree);
    drop(parser);
    let mut native = None;
    assert!(
        matches!(coffeescript_scope::finish(scope, &cancellation, Some(&mut native)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "coffeescript-scanner-work-limit")
    );
    assert_eq!(
        native,
        Some(NativeScannerRefusalDetails::CoffeescriptExternalWork {
            consumed: 8,
            limit: 8
        })
    );
    let detail = native.unwrap().diagnostic("refused");
    assert!(
        detail.contains(
            "consumed 8; demanded unknown; remaining-demand unknown; next charge declined"
        )
    );
    let scope = coffeescript_scope::begin(&cancellation, usize::MAX).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_coffeescript::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    assert!(!tree.root_node().has_error());
    drop(parser);
    let mut healthy = None;
    coffeescript_scope::finish(scope, &cancellation, Some(&mut healthy)).unwrap();
    assert!(healthy.is_none());
}

#[test]
fn caller_cancellation_never_issues_scanner_source_preservation() {
    let cancellation = Cancellation::new();
    let scope = coffeescript_scope::begin(&cancellation, usize::MAX).unwrap();
    cancellation.cancel(CancellationReason::ClientRequest);
    let mut native = None;
    assert!(matches!(
        coffeescript_scope::finish(scope, &cancellation, Some(&mut native)),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert!(native.is_none());
    assert!(matches!(
        coffeescript_scope::begin(&cancellation, usize::MAX),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let clean = Cancellation::new();
    let scope = coffeescript_scope::begin(&clean, usize::MAX).unwrap();
    coffeescript_scope::finish(scope, &clean, None).unwrap();
}
