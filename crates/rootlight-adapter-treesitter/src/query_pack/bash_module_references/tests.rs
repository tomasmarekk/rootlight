//! Native helper controls keep partial input, work and cancellation explicit.
//! The complete wrapper fixture runs at its existing source-owned work bound.

use super::*;
use rootlight_cancel::CancellationReason;

fn parsed(source: &[u8]) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn use_node<'tree>(tree: &'tree tree_sitter::Tree, source: &[u8]) -> Node<'tree> {
    let start = source
        .windows(b"$OWNED".len())
        .rposition(|part| part == b"$OWNED")
        .unwrap()
        + 1;
    let node = tree
        .root_node()
        .descendant_for_byte_range(start, start + 5)
        .unwrap();
    assert_eq!(node.kind(), "variable_name");
    node
}

#[test]
fn complete_authority_precedes_zero_credit_census_and_cancellation_is_not_hidden() {
    let source = b"OWNED=value\nprintf '%s' \"$OWNED\"\n";
    let tree = parsed(source);
    let node = use_node(&tree, source);
    let mut partial = CaptureCache::new(source, 0, false);
    assert_eq!(
        partial
            .syntax(tree.root_node(), node, &Cancellation::new())
            .unwrap(),
        None
    );
    assert!(!partial.exhausted());
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        partial.syntax(tree.root_node(), node, &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn native_proof_work_has_zero_below_and_exact_admission_boundaries() {
    let source = b"OWNED=value\nif [[ -n \"$OWNED\" ]]; then printf '%s' \"$OWNED\" 1>&2; fi\n";
    let tree = parsed(source);
    let node = use_node(&tree, source);
    let succeeds = |maximum| {
        let mut cache = CaptureCache::new(source, maximum, true);
        let result = cache
            .syntax(tree.root_node(), node, &Cancellation::new())
            .unwrap();
        (
            !cache.exhausted() && result == Some("bash.module_reference"),
            cache.exhausted(),
        )
    };
    assert_eq!(succeeds(0), (false, true));
    let mut lower = 0usize;
    let mut upper = 16_384usize;
    assert!(succeeds(upper).0);
    while lower + 1 < upper {
        let middle = lower + (upper - lower) / 2;
        if succeeds(middle).0 {
            upper = middle
        } else {
            lower = middle
        }
    }
    assert_eq!(succeeds(upper - 1), (false, true));
    assert_eq!(succeeds(upper), (true, false));
}

#[test]
fn complete_nvm_variables_do_not_exhaust_the_existing_wrapper_work_budget() {
    let source = include_bytes!("../../../tests/fixtures/bash_written_calls/nvm.sh");
    assert_eq!(source.len(), 168666);
    let tree = parsed(source);
    let mut cache = CaptureCache::new(source, 1_048_576, true);
    let mut cursor = tree.root_node().walk();
    loop {
        let node = cursor.node();
        if node.kind() == "variable_name" {
            cache
                .syntax(tree.root_node(), node, &Cancellation::new())
                .unwrap();
            assert!(
                !cache.exhausted(),
                "new proof work must preserve existing whole-original admission"
            );
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

// Implicit arithmetic is a native syntax boundary, not a search for example text.
#[test]
fn implicit_arithmetic_and_readonly_names_never_gain_native_certificates() {
    for source in [
        "OWNED=1\nINDEX='OWNED++'\nARR=(first second)\n: \"${ARR[INDEX]}\"\nprintf '%s' \"$OWNED\"\n",
        "OWNED=1\nINDEX='OWNED++'\n: \"${ARR[INDEX]}\"\nprintf '%s' \"$OWNED\"\n",
        "OWNED=1\nOFFSET='OWNED++'\nTEXT=abc\n: \"${TEXT:OFFSET}\"\nprintf '%s' \"$OWNED\"\n",
        "OWNED=1\nSHIFT='++OWNED'\nTEXT=abc\n: \"${TEXT:1:SHIFT}\"\nprintf '%s' \"$OWNED\"\n",
        "OWNED=1\nINDEX='++OWNED'\nARR=([INDEX]=first)\nprintf '%s' \"$OWNED\"\n",
    ] {
        let tree = parsed(source.as_bytes());
        assert!(
            !tree.root_node().has_error(),
            "native control must be clean: {source}"
        );
        let node = use_node(&tree, source.as_bytes());
        let mut cache = CaptureCache::new(source.as_bytes(), 65_536, true);
        assert_eq!(
            cache
                .syntax(tree.root_node(), node, &Cancellation::new())
                .unwrap(),
            Some("bash.unproven_module_reference"),
            "{source}"
        );
        assert!(!cache.exhausted());
    }
    for name in ["UID", "EUID", "PPID", "BASH_VERSINFO", "BASH_MONOSECONDS"] {
        let source = format!("{name}=7\nprintf '%s' \"${name}\"\n");
        let tree = parsed(source.as_bytes());
        assert!(!tree.root_node().has_error());
        let start = source.rfind(name).unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + name.len())
            .unwrap();
        let mut cache = CaptureCache::new(source.as_bytes(), 65_536, true);
        assert_eq!(
            cache
                .syntax(tree.root_node(), node, &Cancellation::new())
                .unwrap(),
            Some("bash.unproven_module_reference"),
            "{source}"
        );
        assert!(!cache.exhausted());
    }
    let name = "BASH_CUSTOM";
    let source = format!("{name}=written\nprintf '%s' \"${name}\"\n");
    let tree = parsed(source.as_bytes());
    assert!(!tree.root_node().has_error());
    let start = source.rfind(name).unwrap();
    let node = tree
        .root_node()
        .descendant_for_byte_range(start, start + name.len())
        .unwrap();
    let mut cache = CaptureCache::new(source.as_bytes(), 65_536, true);
    assert_eq!(
        cache
            .syntax(tree.root_node(), node, &Cancellation::new())
            .unwrap(),
        Some("bash.module_reference")
    );
    assert!(!cache.exhausted());
}
