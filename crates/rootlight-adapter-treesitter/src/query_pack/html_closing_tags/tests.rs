//! Producer controls separate explicit current-tree names from source text.
//! All positions refer to the native parser's logical UTF-8 source bytes.

use super::*;

type Site = ((usize, usize), &'static str);
type CaptureResult = Result<(Vec<Site>, bool), AdapterError>;

fn capture_sites(
    source: &[u8],
    maximum: usize,
    complete: bool,
    cancel: &Cancellation,
) -> CaptureResult {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let mut cache = CaptureCache::new(source, tree.root_node(), maximum, complete);
    let mut sites = Vec::new();
    let mut cursor = tree.root_node().walk();
    loop {
        let node = cursor.node();
        if node.kind() == "tag_name"
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "end_tag")
        {
            for role in [StructuralRole::Reference, StructuralRole::Signature] {
                let mut candidate = QueryCandidate {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    role,
                    syntax: "html.tag_name",
                    required: false,
                    native_depth: 0,
                };
                if cache.refine(node, role, &mut candidate, cancel)? {
                    assert!(candidate.required);
                    assert_eq!(
                        (candidate.start, candidate.end),
                        (node.start_byte(), node.end_byte())
                    );
                    sites.push(((candidate.start, candidate.end), candidate.syntax));
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok((sites, cache.exhausted()));
            }
        }
    }
}

#[test]
fn real_closing_names_receive_exact_required_reference_and_context_carriers() {
    let source = b"<head><meta charset='utf-8'><title>x</title></HeAd>";
    let (sites, exhausted) = capture_sites(source, 4096, true, &Cancellation::new()).unwrap();
    assert!(!exhausted);
    assert_eq!(sites.len(), 4);
    let head = source
        .windows(b"HeAd".len())
        .position(|part| part == b"HeAd")
        .unwrap();
    assert!(sites.contains(&((head, head + 4), "html.closing_tag")));
    assert!(sites.contains(&((head, head + 4), "html.closing_tag_context")));
}

#[test]
fn comments_attributes_and_script_text_do_not_receive_closing_carriers() {
    let source = b"<!-- <head></head> --><main title='</head>'><script>const text='</head>';</script></main>";
    let (sites, exhausted) = capture_sites(source, 4096, true, &Cancellation::new()).unwrap();
    assert!(!exhausted);
    assert_eq!(sites.len(), 4);
    assert!(
        sites
            .iter()
            .all(|((start, end), _)| matches!(source.get(*start..*end), Some(b"script" | b"main")))
    );
}

#[test]
fn unsupported_or_incomplete_source_does_not_spend_a_false_limit() {
    for source in [
        "<main><p>implicit</main>",
        "<svg><path/></svg>",
        "<main></main></wrong>",
    ] {
        let (sites, exhausted) =
            capture_sites(source.as_bytes(), 4096, true, &Cancellation::new()).unwrap();
        assert!(sites.is_empty(), "{source}");
        assert!(!exhausted, "{source}");
    }
    let source = b"<main></main>";
    let (sites, exhausted) = capture_sites(source, 0, false, &Cancellation::new()).unwrap();
    assert!(sites.is_empty() && !exhausted);
    let (sites, exhausted) = capture_sites(source, 0, true, &Cancellation::new()).unwrap();
    assert!(sites.is_empty() && exhausted);
}

#[test]
fn older_definition_capture_and_typed_cancellation_keep_their_contracts() {
    let source = b"<main></main>";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let element = root.named_child(0).unwrap();
    let opening = element.named_child(0).unwrap().named_child(0).unwrap();
    let mut cache = CaptureCache::new(source, root, 0, false);
    let mut candidate = QueryCandidate {
        start: opening.start_byte(),
        end: opening.end_byte(),
        role: StructuralRole::Definition,
        syntax: "html.tag_name",
        required: false,
        native_depth: 0,
    };
    assert!(
        cache
            .refine(
                opening,
                StructuralRole::Definition,
                &mut candidate,
                &Cancellation::new()
            )
            .unwrap()
    );
    assert_eq!(candidate.syntax, "html.tag_name");
    assert!(!candidate.required && !cache.exhausted());
    let registry = crate::query_pack::QueryPackRegistry::audited().unwrap();
    let family = crate::registry::GrammarFamily::Html;
    let pack = registry.get(family).unwrap();
    let maximum = root.descendant_count();
    let bounded = pack
        .extract(family, &tree, source, maximum, 4096, &Cancellation::new())
        .unwrap();
    assert_eq!(
        bounded.limit,
        Some(crate::query_pack::QueryLimit::HtmlClosingTags)
    );
    for role in [
        StructuralRole::Root,
        StructuralRole::Module,
        StructuralRole::Declaration,
        StructuralRole::Definition,
    ] {
        assert!(
            bounded
                .candidates
                .iter()
                .any(|candidate| candidate.role == role)
        );
    }
    assert!(bounded.candidates.iter().all(|candidate| !matches!(
        candidate.syntax,
        "html.closing_tag" | "html.closing_tag_context"
    )));
    assert!(matches!(
        pack.extract_identity(family, &tree, source, maximum, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "query-identity-html-closing-tag-limit"
    ));
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        capture_sites(source, 4096, true, &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
}
