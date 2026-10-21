use super::*;
use rootlight_cancel::CancellationReason;

struct Probe;
impl tree_sitter_lean::CancellationProbe for Probe {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn parse(source: &[u8]) -> tree_sitter::Tree {
    let scope = tree_sitter_lean::ScannerScope::begin(Probe, 2_000_000).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_lean::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    scope.finish().unwrap();
    tree
}

#[test]
fn complete_original_uses_unique_named_fields_and_preserves_its_leading_crlf() {
    let source = include_bytes!("../../../tests/fixtures/lean/conv_eval.lean");
    let tree = parse(source);
    assert!(!tree.root_node().has_error());
    assert_eq!(tree.root_node().start_byte(), 2);
    assert_eq!(tree.root_node().end_byte(), source.len());
    let cancel = Cancellation::new();
    let mut cache = CaptureCache::new(source, tree.root_node(), 100_000, true);
    cache.initialize(&cancel).unwrap();
    assert!(cache.complete && cache.closed && !cache.exhausted());
    let captured: Vec<_> = cache
        .declarations
        .values()
        .map(|item| (item.label, item.name.clone()))
        .collect();
    for expected in [
        ("lean.function", 254..257),
        ("lean.parameter", 259..260),
        ("lean.parameter", 272..273),
    ] {
        assert!(captured.contains(&expected));
    }
    assert_eq!(cache.reads.len(), 1);
    assert!(cache.reads.values().any(|range| *range == (271..283)));
    assert_eq!(cache.contexts.len(), 1);
    assert!(cache.contexts.values().any(|range| *range == (250..283)));
    let root = candidate(tree.root_node(), StructuralRole::Root, &cancel, &mut cache)
        .unwrap()
        .unwrap();
    assert_eq!(
        (root.start, root.end, root.syntax),
        (0, source.len(), "lean.file")
    );
}

#[test]
fn renaming_keeps_direct_formal_proof_while_nested_qualified_grouped_and_duplicate_forms_decline() {
    for source in [
        "def owned (value : Nat) := value\n",
        "def renamed (argument : Nat) := argument\n",
    ] {
        let tree = parse(source.as_bytes());
        assert!(!tree.root_node().has_error());
        let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 100_000, true);
        cache.initialize(&Cancellation::new()).unwrap();
        assert_eq!(cache.reads.len(), 1, "{source}");
    }
    for source in [
        "def owned (value value : Nat) := value\n",
        "def owned (value : Nat) := let value := 0; value\n",
        "def owned (value : Nat) := (fun value => value) value\n",
        "def owned (value : Nat) := Other.value\n",
        "def owned (value : Nat) := «value»\n",
        "def owned (value : Nat) := value\ndef owned (value : Nat) := value\n",
        "namespace Outer\ndef owned (value : Nat) := value\nend Outer\n",
    ] {
        let tree = parse(source.as_bytes());
        let mut cache = CaptureCache::new(source.as_bytes(), tree.root_node(), 100_000, true);
        cache.initialize(&Cancellation::new()).unwrap();
        assert!(cache.reads.is_empty(), "{source}");
        assert!(cache.contexts.is_empty(), "{source}");
    }
}

#[test]
fn incomplete_recovered_cancelled_or_exhausted_input_never_returns_binding_certificates() {
    let source = include_bytes!("../../../tests/fixtures/lean/conv_eval.lean");
    let tree = parse(source);
    let cancel = Cancellation::new();
    for credit in [0, 1, 8] {
        let mut cache = CaptureCache::new(source, tree.root_node(), credit, true);
        cache.initialize(&cancel).unwrap();
        assert!(cache.exhausted());
        assert!(cache.reads.is_empty());
    }
    let mut incomplete = CaptureCache::new(source, tree.root_node(), 100_000, false);
    incomplete.initialize(&cancel).unwrap();
    assert!(incomplete.declarations.is_empty() && incomplete.reads.is_empty());
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    let mut stopped = CaptureCache::new(source, tree.root_node(), 100_000, true);
    assert!(matches!(
        stopped.initialize(&cancelled),
        Err(AdapterError::Cancelled { .. })
    ));
    let source = b"def owned (value : Nat) := (\n";
    let tree = parse(source);
    assert!(tree.root_node().has_error());
    let mut recovered = CaptureCache::new(source, tree.root_node(), 100_000, true);
    recovered.initialize(&cancel).unwrap();
    assert!(recovered.declarations.is_empty() && recovered.reads.is_empty());
}
