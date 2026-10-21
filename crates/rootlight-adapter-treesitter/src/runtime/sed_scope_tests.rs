//! Keep physical evidence separate from the declared static Sed file-text view.
//! Native context checks do not certify definitions or branch resolution.

use super::*;
use crate::source_view::{LineEndingPolicy, NativeSourceView};
use rootlight_cancel::CancellationReason;

const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/sed/mario.sed");

fn view<'a>(raw: &'a [u8], encoding: &str, cancel: &Cancellation) -> NativeSourceView<'a> {
    NativeSourceView::new_with_policy(
        raw,
        encoding,
        LineEndingPolicy::CrlfFileText,
        1024 * 1024,
        1024 * 1024,
        cancel,
    )
    .unwrap()
}

fn parse(source: &[u8]) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_gsed::LANGUAGE_BRE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    tree
}

fn issues(tree: &Tree) -> usize {
    let mut cursor = tree.walk();
    let mut count = 0;
    loop {
        count += usize::from(cursor.node().kind() == "syntax_issue");
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return count;
            }
        }
    }
}

#[test]
fn sed_full_original_maps_six_native_command_witnesses_to_physical_bytes() {
    assert_eq!(ORIGINAL.len(), 8893);
    let cancel = Cancellation::new();
    let source = view(ORIGINAL, "utf-8", &cancel);
    assert_eq!(source.as_bytes().len(), 8509);
    assert!(!source.is_identity());
    let scope = sed_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(source.as_bytes());
    sed_scope::finish(scope, &cancel, None).unwrap();
    let root = tree.root_node();
    assert_eq!(root.kind(), "script");
    assert_eq!(root.byte_range(), 0..8509);
    assert!(!root.has_error() && !root.is_missing());
    assert_eq!(issues(&tree), 0);
    for (start, end, raw_start, raw_end, name, owner) in [
        (938, 942, 975, 979, "none", "label_command"),
        (951, 955, 990, 994, "left", "label_command"),
        (977, 982, 1020, 1025, "right", "label_command"),
        (5202, 5205, 5403, 5406, "end", "label_command"),
        (946, 949, 984, 987, "end", "branch_command"),
        (972, 975, 1014, 1017, "end", "branch_command"),
    ] {
        let node = root.descendant_for_byte_range(start, end).unwrap();
        assert_eq!(node.kind(), "label_text");
        assert_eq!(node.byte_range(), start..end);
        assert_eq!(node.utf8_text(source.as_bytes()).unwrap(), name);
        let label = node.parent().unwrap();
        assert_eq!(label.kind(), "label");
        assert_eq!(label.parent().unwrap().kind(), owner);
        assert_eq!(source.to_physical_offset(start).unwrap(), raw_start);
        assert_eq!(source.to_physical_offset(end).unwrap(), raw_end);
        assert_eq!(&ORIGINAL[raw_start..raw_end], name.as_bytes());
        assert_eq!(source.to_projected_offset(raw_start).unwrap(), start);
        assert_eq!(source.to_projected_offset(raw_end).unwrap(), end);
    }
}

#[test]
fn sed_measured_work_refusal_withholds_the_tree_and_next_scope_is_clean() {
    let cancel = Cancellation::new();
    let source = view(ORIGINAL, "utf-8", &cancel);
    let scope = sed_scope::begin(&cancel, 16).unwrap();
    drop(parse(source.as_bytes()));
    let mut witness = None;
    assert!(
        matches!(sed_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "sed-scanner-work-limit")
    );
    assert_eq!(
        witness,
        Some(NativeScannerRefusalDetails::SedExternalWork {
            consumed: 16,
            limit: 16,
        })
    );
    assert!(
        witness
            .unwrap()
            .diagnostic("refused")
            .contains("demanded unknown")
    );
    let scope = sed_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(b":entry\nb entry\n");
    sed_scope::finish(scope, &cancel, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert_eq!(issues(&tree), 0);
}

#[test]
fn sed_early_and_late_cancellation_never_issue_source_fallback() {
    let early = Cancellation::new();
    early.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        sed_scope::begin(&early, 0),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest,
        })
    ));
    let cancel = Cancellation::new();
    let source = view(ORIGINAL, "utf-8", &cancel);
    let scope = sed_scope::begin(&cancel, 16).unwrap();
    drop(parse(source.as_bytes()));
    cancel.cancel(CancellationReason::ClientRequest);
    let mut witness = None;
    assert!(matches!(
        sed_scope::finish(scope, &cancel, Some(&mut witness)),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert!(witness.is_none());
    let clean = Cancellation::new();
    let scope = sed_scope::begin(&clean, usize::MAX).unwrap();
    sed_scope::finish(scope, &clean, None).unwrap();
}

#[test]
fn sed_reuse_binds_scanner_view_and_physical_edit_policy() {
    let raw = b":entry\r\nb entry\r\n";
    let cancel = Cancellation::new();
    let source = view(raw, "utf-8", &cancel);
    let scope = sed_scope::begin(&cancel, usize::MAX).unwrap();
    let tree = parse(source.as_bytes());
    sed_scope::finish(scope, &cancel, None).unwrap();
    let retained_geometry = tree.root_node().to_sexp();
    let mut identity = invalid_identity();
    identity.family = GrammarFamily::Sed;
    identity.grammar_version = "0.4.0";
    identity.encoding = "utf-8".to_owned();
    identity.line_endings = LineEndingPolicy::CrlfFileText;
    identity.content_hash = rootlight_ids::content_hash(raw);
    let mut cached = CachedParse::invalidation(ReuseInvalidation::FrontendRoute);
    cached.invalidation = None;
    cached.identity = identity.clone();
    cached.source = Arc::new(raw.to_vec());
    cached.tree = Some(tree);
    let (tree, status) =
        prepare_reuse(Some(&cached), &identity, raw, &[], raw.len(), &cancel).unwrap();
    assert!(tree.is_some());
    assert_eq!(status, ReuseStatus::Reused { changed_ranges: 0 });
    for variant in 0..3 {
        let mut stale = cached.clone_for_reuse();
        match variant {
            0 => stale.sed_scanner_contract = "sed-pristine-scanner",
            1 => stale.sed_frontend_policy = "sed-raw-program/0",
            _ => stale.identity.line_endings = LineEndingPolicy::Preserve,
        }
        let (tree, status) =
            prepare_reuse(Some(&stale), &identity, raw, &[], raw.len(), &cancel).unwrap();
        assert!(tree.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
    let changed = b":other\r\nb entry\r\n";
    let mut current = identity;
    current.content_hash = rootlight_ids::content_hash(changed);
    let (tree, status) = prepare_reuse(
        Some(&cached),
        &current,
        changed,
        &[SourceEdit::new(1, 6, "other").unwrap()],
        changed.len(),
        &cancel,
    )
    .unwrap();
    assert!(tree.is_none());
    assert_eq!(
        status,
        ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
    );
    assert_eq!(
        cached.tree.as_ref().unwrap().root_node().to_sexp(),
        retained_geometry
    );
    assert_eq!(cached.source.as_slice(), raw);
}

#[test]
fn sed_bom_observation_keeps_the_bom_and_exact_encoded_boundary() {
    let text = "\u{feff}# marker λ😀\r\n:entry\r\nb entry\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw = match encoding {
            "utf-8" => text.as_bytes().to_vec(),
            _ => text
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect(),
        };
        let cancel = Cancellation::new();
        let source = view(&raw, encoding, &cancel);
        assert!(source.as_bytes().starts_with(b"\xef\xbb\xbf"));
        assert_eq!(
            source.to_physical_offset(3).unwrap(),
            if encoding == "utf-8" { 3 } else { 2 }
        );
        assert_eq!(
            source.to_physical_offset(source.as_bytes().len()).unwrap(),
            raw.len()
        );
        let scope = sed_scope::begin(&cancel, usize::MAX).unwrap();
        let tree = parse(source.as_bytes());
        sed_scope::finish(scope, &cancel, None).unwrap();
        println!(
            "sed-bom-observation encoding={encoding} raw={} projected={} root={:?} errors={} syntax_issues={} cst={}",
            raw.len(),
            source.as_bytes().len(),
            tree.root_node().byte_range(),
            tree.root_node().has_error(),
            issues(&tree),
            tree.root_node().to_sexp()
        );
        // This is a geometry observation. Clean encoded admission remains a
        // separate mandatory matrix assertion, including any real parser gap.
        assert!(tree.root_node().end_byte() <= source.as_bytes().len());
    }
}
