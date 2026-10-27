//! Native scope controls for Liquid loop endpoint certification.
//! These tests use the linked bounded scanner and do not infer runtime values.

use super::*;

struct Probe(Cancellation);
impl tree_sitter_liquid::CancellationProbe for Probe {
    fn is_cancelled(&self) -> bool {
        self.0.check().is_err()
    }
}

fn parse(source: &[u8], included: Option<tree_sitter::Range>) -> tree_sitter::Tree {
    let scope =
        tree_sitter_liquid::ScannerScope::begin(Probe(Cancellation::new()), 2_000_000).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_liquid::LANGUAGE.into())
        .unwrap();
    if let Some(range) = included {
        parser.set_included_ranges(&[range]).unwrap();
    }
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    scope.finish().unwrap();
    tree
}

fn clean(source: &[u8]) -> Census {
    let tree = parse(source, None);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    census(
        tree.root_node(),
        source,
        100_000,
        true,
        &Cancellation::new(),
    )
    .unwrap()
    .unwrap()
}

fn at(source: &[u8], needle: &[u8]) -> Range {
    let start = source
        .windows(needle.len())
        .position(|part| part == needle)
        .unwrap();
    (start, start + needle.len())
}

#[test]
fn original_complete_native_template_certifies_both_items_and_exact_value_receivers() {
    let source = include_bytes!("../../tests/fixtures/liquid/blog-sidebar.liquid");
    assert_eq!(source.len(), 888);
    let result = clean(source);
    assert_eq!(
        result
            .definitions
            .iter()
            .map(|item| item.name)
            .collect::<Vec<_>>(),
        [(191, 198), (673, 676)]
    );
    assert!(result.reads.contains(&Read {
        name: (270, 277),
        definition: (191, 198)
    }));
    assert!(result.reads.contains(&Read {
        name: (753, 756),
        definition: (673, 676)
    }));
    assert!(
        result
            .reads
            .iter()
            .all(|read| read.definition == (191, 198) || read.definition == (673, 676))
    );
}

#[test]
fn same_name_nested_items_choose_the_innermost_body_and_restore_outer_item_afterwards() {
    let source = b"{% for item in outer %}{{ item }}{% for item in item.children %}{{ item }}{% endfor %}{{ item }}{% endfor %}";
    let result = clean(source);
    assert_eq!(result.definitions.len(), 2);
    assert_eq!(result.reads.len(), 4);
    let outer = result.definitions[0].name;
    let inner = result.definitions[1].name;
    assert_eq!(
        result
            .reads
            .iter()
            .map(|read| read.definition)
            .collect::<Vec<_>>(),
        [outer, outer, inner, outer]
    );
}

#[test]
fn sibling_alternative_and_outside_uses_do_not_reuse_a_finished_loop_item() {
    let source = b"{{ item }}{% for item in source %}{{ item }}{% else %}{{ item }}{% endfor %}{{ item }}{% for other in source %}{{ item }}{% endfor %}";
    let result = clean(source);
    assert_eq!(result.definitions.len(), 2);
    assert_eq!(result.reads.len(), 1);
    assert_eq!(result.reads[0].definition, result.definitions[0].name);
}

#[test]
fn a_same_name_inner_alternative_uses_only_the_enclosing_item() {
    let source = b"{% for item in outer %}{% for item in nested %}{{ item }}{% else %}{{ item }}{% endfor %}{% endfor %}";
    let result = clean(source);
    assert_eq!(result.reads.len(), 2);
    assert_eq!(result.reads[0].definition, result.definitions[1].name);
    assert_eq!(result.reads[1].definition, result.definitions[0].name);
}

#[test]
fn assign_capture_and_counter_mutation_withhold_ambiguous_same_item_reads() {
    for statement in [
        "{% assign item = other %}",
        "{% capture item %}text{% endcapture %}",
        "{% increment item %}",
        "{% decrement item %}",
    ] {
        let source = format!(
            "{{% for item in source %}}{{{{ item }}}}{statement}{{{{ item }}}}{{% endfor %}}"
        );
        let result = clean(source.as_bytes());
        assert_eq!(result.definitions.len(), 1);
        assert!(result.reads.is_empty(), "{statement}");
    }
}

#[test]
fn mutation_of_an_unrelated_name_does_not_erase_an_ordinary_item_read() {
    let source = b"{% for item in source %}{% assign other = 1 %}{{ item }}{% endfor %}";
    let result = clean(source);
    assert_eq!(result.reads.len(), 1);
}

#[test]
fn unsupported_nested_owners_never_borrow_the_surrounding_loop_scope() {
    for statement in [
        "{% tablerow item in source %}{{ item }}{% endtablerow %}",
        "{% capture other %}{{ item }}{% endcapture %}",
        "{% form 'product' %}{{ item }}{% endform %}",
    ] {
        let source = format!("{{% for item in source %}}{statement}{{% endfor %}}");
        assert!(clean(source.as_bytes()).reads.is_empty(), "{statement}");
    }
}

#[test]
fn dot_properties_filter_names_and_quoted_lookalikes_are_not_local_value_receivers() {
    let source =
        b"{% for item in source %}{{ external.item }}{{ item.name | item: 'item' }}{% endfor %}";
    let result = clean(source);
    assert_eq!(result.reads.len(), 1);
    assert_eq!(
        &source[result.reads[0].name.0..result.reads[0].name.1],
        b"item"
    );
    assert_eq!(result.reads[0].name.0, at(source, b"item.name").0);
}

#[test]
fn dynamic_bracket_keys_are_value_reads_while_static_properties_keep_no_target() {
    let source = b"{% for item in source %}{{ external[item] }}{{ external[item.key] }}{{ item[item] }}{% endfor %}";
    assert_eq!(clean(source).reads.len(), 4);
}

#[test]
fn raw_comment_and_string_contents_do_not_create_loop_or_value_certificates() {
    let source = b"{% raw %}{% for item in source %}{{ item }}{% endfor %}{% endraw %}{% comment %}{% for item in source %}{{ item }}{% endfor %}{% endcomment %}{{ 'item' }}";
    let result = clean(source);
    assert!(result.definitions.is_empty());
    assert!(result.reads.is_empty());
}

#[test]
fn missing_endfor_and_malformed_brackets_never_supply_a_partial_clean_census() {
    for source in [
        b"{% for item in source %}{{ item }}".as_slice(),
        b"{% for item in source[broken %}{{ item }}{% endfor %}",
    ] {
        let tree = parse(source, None);
        assert!(tree.root_node().has_error());
        assert!(
            census(
                tree.root_node(),
                source,
                100_000,
                true,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
    }
}

#[test]
fn selected_clean_ranges_cannot_certify_whole_physical_source_owners() {
    let source = b"{% for item in source %}{{ item }}{% endfor %} rejected";
    let end = source.len() - b" rejected".len();
    let selected = tree_sitter::Range {
        start_byte: 0,
        end_byte: end,
        start_point: tree_sitter::Point::new(0, 0),
        end_point: tree_sitter::Point::new(0, end),
    };
    let tree = parse(source, Some(selected));
    assert!(!tree.root_node().has_error());
    assert!(
        census(
            tree.root_node(),
            source,
            100_000,
            false,
            &Cancellation::new()
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn positive_work_exhaustion_is_distinct_from_unsupported_clean_context() {
    let source = b"{% for item in source %}{{ item }}{% endfor %}";
    let tree = parse(source, None);
    let (proof, exhausted) =
        census_with_status(tree.root_node(), source, 1, true, &Cancellation::new()).unwrap();
    assert!(proof.is_none());
    assert!(exhausted);
    let (proof, exhausted) = census_with_status(
        tree.root_node(),
        source,
        100_000,
        false,
        &Cancellation::new(),
    )
    .unwrap();
    assert!(proof.is_none());
    assert!(!exhausted);
}

#[test]
fn cancellation_precedes_clean_context_and_zero_work_credit() {
    let source = b"{% for item in source %}{{ item }}{% endfor %}";
    let tree = parse(source, None);
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        census(tree.root_node(), source, 0, false, &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn utf8_bom_non_ascii_template_text_and_crlf_keep_exact_byte_endpoints() {
    let source = "\u{feff}é🧭\r\n{% for item in source %}{{ item }}{% endfor %}".as_bytes();
    let result = clean(source);
    assert_eq!(result.definitions[0].name.0, at(source, b"item in").0);
    assert_eq!(result.reads[0].name.0, at(source, b"item }}").0);
}

#[test]
fn opaque_import_or_custom_runtime_statements_withhold_uncertified_loop_values() {
    for statement in [
        "{% include 'snippet' %}",
        "{% render 'snippet' %}",
        "{% custom_runtime item %}",
    ] {
        let source = format!(
            "{{% for item in source %}}{{{{ item }}}}{statement}{{{{ item }}}}{{% endfor %}}"
        );
        let result = clean(source.as_bytes());
        assert_eq!(result.definitions.len(), 1, "{statement}");
        assert!(result.reads.is_empty(), "{statement}");
    }
}
