//! Native Idris context does not itself prove SDK or resolver semantics.

use super::*;
use crate::source_view::NativeSourceView;
use rootlight_cancel::CancellationReason;

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/idris/stlc_small.idr");

fn parse(source: &[u8]) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_idris::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    tree
}

#[test]
fn idris_complete_original_and_encoded_views_retain_exact_native_witnesses() {
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), ORIGINAL].concat()
        } else {
            let mut raw = if encoding == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in std::str::from_utf8(ORIGINAL).unwrap().encode_utf16() {
                raw.extend(if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            raw
        };
        let cancel = Cancellation::new();
        let view = NativeSourceView::new(&raw, encoding, 16384, 65536, &cancel).unwrap();
        let scope = idris_scope::begin(&cancel, usize::MAX).unwrap();
        let tree = parse(view.as_bytes());
        idris_scope::finish(scope, &cancel, None).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "idris");
        assert!(!root.has_error() && !root.is_missing());
        assert_eq!(root.byte_range(), 3..2170);
        for (start, end, text, parent) in [
            (1215, 1218, "var", "funvar"),
            (1587, 1589, "v0", "funvar"),
            (1592, 1595, "var", "exp_name"),
            (1247, 1250, "var", "exp_name"),
        ] {
            let node = root.descendant_for_byte_range(start + 3, end + 3).unwrap();
            assert_eq!(node.kind(), "loname");
            assert_eq!(node.parent().unwrap().kind(), parent);
            assert_eq!(node.utf8_text(view.as_bytes()).unwrap(), text);
            let physical = view.to_physical_offset(start + 3).unwrap();
            let physical_end = view.to_physical_offset(end + 3).unwrap();
            assert_eq!(
                physical,
                if encoding == "utf-8" {
                    start + 3
                } else {
                    2 + start * 2
                }
            );
            assert_eq!(
                physical_end,
                if encoding == "utf-8" {
                    end + 3
                } else {
                    2 + end * 2
                }
            );
            assert_eq!(view.to_projected_offset(physical).unwrap(), start + 3);
            assert_eq!(view.to_projected_offset(physical_end).unwrap(), end + 3);
        }
        assert_eq!(
            view.to_physical_offset(view.as_bytes().len()).unwrap(),
            raw.len()
        );
        assert_eq!(
            view.as_bytes()
                .windows(2)
                .filter(|pair| *pair == b"\r\n")
                .count(),
            71
        );
        assert_eq!(
            crate::source_view::LineEndingPolicy::for_language("idris"),
            crate::source_view::LineEndingPolicy::Preserve
        );
    }
}

#[test]
fn idris_scanner_work_capacity_and_nul_have_distinct_admission() {
    let cancel = Cancellation::new();
    let scope = idris_scope::begin(&cancel, 16).unwrap();
    drop(parse(ORIGINAL));
    let mut witness = None;
    assert!(
        matches!(idris_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "idris-scanner-work-limit")
    );
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::IdrisExternalWork {
            consumed: 16,
            limit: 16
        })
    );
    let mut large = vec![b'a'; 1100];
    large.extend_from_slice(b" : Type\n");
    let scope = idris_scope::begin(&cancel, usize::MAX).unwrap();
    drop(parse(&large));
    witness = None;
    assert!(
        matches!(idris_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "idris-scanner-capacity")
    );
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::IdrisStateCapacity)
    );
    let scope = idris_scope::begin(&cancel, usize::MAX).unwrap();
    drop(parse(b"value : Type\nvalue = \0\n"));
    witness = None;
    assert!(
        matches!(idris_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "idris-scanner-nul-unavailable")
    );
    assert!(witness.is_none());
    let scope = idris_scope::begin(&cancel, usize::MAX).unwrap();
    let clean = parse(ORIGINAL);
    idris_scope::finish(scope, &cancel, None).unwrap();
    assert!(!clean.root_node().has_error());
}

#[test]
fn idris_cancellation_and_callback_panic_cannot_issue_a_fallback() {
    let early = Cancellation::new();
    early.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        idris_scope::begin(&early, 0),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let cancel = Cancellation::new();
    let scope = idris_scope::begin(&cancel, 16).unwrap();
    drop(parse(ORIGINAL));
    cancel.cancel(CancellationReason::ClientRequest);
    let mut witness = None;
    assert!(matches!(
        idris_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert!(witness.is_none());
    struct Panics;
    impl tree_sitter_idris::CancellationProbe for Panics {
        fn is_cancelled(&self) -> bool {
            panic!("owned Idris callback control")
        }
    }
    assert!(matches!(
        tree_sitter_idris::ScannerScope::begin(Panics, 0),
        Err(tree_sitter_idris::ScannerRefusal::CallbackPanicked)
    ));
    let clean = Cancellation::new();
    let scope = idris_scope::begin(&clean, usize::MAX).unwrap();
    let tree = parse(ORIGINAL);
    idris_scope::finish(scope, &clean, None).unwrap();
    assert!(!tree.root_node().has_error());
}

#[test]
fn idris_noop_cache_reuse_requires_scanner_policy_and_edits_stay_fresh() {
    let cancel = Cancellation::new();
    let scope = idris_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(ORIGINAL);
    idris_scope::finish(scope, &cancel, None).unwrap();
    let before = tree.root_node().to_sexp();
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Idris;
    identity.grammar_version = "0.0.1";
    identity.encoding = "utf-8".to_owned();
    identity.content_hash = rootlight_ids::content_hash(ORIGINAL);
    let mut cached = CachedParse::invalidation(ReuseInvalidation::FrontendRoute);
    cached.invalidation = None;
    cached.identity = identity.clone();
    cached.source = Arc::new(ORIGINAL.to_vec());
    cached.tree = Some(tree);
    let (tree, status) = prepare_reuse(
        Some(&cached),
        &identity,
        ORIGINAL,
        &[],
        ORIGINAL.len(),
        &cancel,
    )
    .unwrap();
    assert!(tree.is_some());
    assert_eq!(status, ReuseStatus::Reused { changed_ranges: 0 });
    for variant in 0..2 {
        let mut stale = cached.clone_for_reuse();
        if variant == 0 {
            stale.idris_scanner_contract = "pristine-state";
        } else {
            stale.idris_frontend_policy = "unknown-policy";
        }
        let (tree, status) = prepare_reuse(
            Some(&stale),
            &identity,
            ORIGINAL,
            &[],
            ORIGINAL.len(),
            &cancel,
        )
        .unwrap();
        assert!(tree.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
    let mut changed = ORIGINAL.to_vec();
    changed[1588] = b'1';
    let mut current = identity;
    current.content_hash = rootlight_ids::content_hash(&changed);
    let (tree, status) = prepare_reuse(
        Some(&cached),
        &current,
        &changed,
        &[SourceEdit::new(1588, 1589, "1").unwrap()],
        changed.len(),
        &cancel,
    )
    .unwrap();
    assert!(tree.is_none());
    assert_eq!(
        status,
        ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
    );
    assert_eq!(cached.tree.as_ref().unwrap().root_node().to_sexp(), before);
    assert_eq!(cached.source.as_slice(), ORIGINAL);
}
