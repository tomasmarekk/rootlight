//! Document-local collection tag contracts used by structural key construction.
//! Tag identity and kind validation stay separate from graph hashing and parsing.

use rootlight_adapter_sdk::{YamlCollectionKind, YamlDocumentContext};

#[test]
fn collection_tags_share_document_handles_and_validate_known_node_kinds() {
    let context = YamlDocumentContext::new(None, &[("!e!", "tag:yaml.org,2002:")], 256).unwrap();
    for (kind, suffix) in [
        (YamlCollectionKind::Mapping, "map"),
        (YamlCollectionKind::Sequence, "seq"),
    ] {
        for tag in [
            None,
            Some("!"),
            Some(format!("!!{suffix}")).as_deref(),
            Some(format!("!e!{suffix}")).as_deref(),
            Some(format!("!<tag:yaml.org,2002:{suffix}>")).as_deref(),
        ] {
            let tag = context.collection_tag(kind, tag).unwrap();
            assert_eq!(tag.name(), format!("tag:yaml.org,2002:{suffix}"));
            assert!(!tag.has_unrecognized_tag());
        }
        for tag in [
            "!!str",
            "!!int",
            "!!bool",
            "!!float",
            "!!null",
            if suffix == "map" { "!!seq" } else { "!!map" },
        ] {
            assert!(context.collection_tag(kind, Some(tag)).is_none());
        }
    }
}

#[test]
fn collection_application_tags_remain_exact_opaque_and_document_local() {
    let first = YamlDocumentContext::new(None, &[("!e!", "tag:example.org,")], 256).unwrap();
    let second = YamlDocumentContext::new(None, &[], 256).unwrap();
    let encoded = first
        .collection_tag(YamlCollectionKind::Sequence, Some("!e!%41"))
        .unwrap();
    let literal = first
        .collection_tag(YamlCollectionKind::Sequence, Some("!e!A"))
        .unwrap();
    assert_ne!(encoded.name(), literal.name());
    assert!(encoded.has_unrecognized_tag());
    assert!(literal.has_unrecognized_tag());
    assert!(
        second
            .collection_tag(YamlCollectionKind::Sequence, Some("!e!A"))
            .is_none()
    );
}

#[test]
fn collection_tag_source_and_expansion_obey_existing_byte_budgets() {
    for maximum in 0..40 {
        let context = YamlDocumentContext::new(None, &[], maximum).unwrap();
        assert_eq!(
            context
                .collection_tag(YamlCollectionKind::Mapping, None)
                .is_some(),
            maximum >= "tag:yaml.org,2002:map".len()
        );
        let verbatim = "!<tag:yaml.org,2002:map>";
        assert_eq!(
            context
                .collection_tag(YamlCollectionKind::Mapping, Some(verbatim))
                .is_some(),
            maximum >= verbatim.len()
        );
    }
    let context = YamlDocumentContext::new(None, &[], 256).unwrap();
    for tag in ["", "!!", "!<>", "!<missing", "!bad%GG", "!bad space"] {
        assert!(
            context
                .collection_tag(YamlCollectionKind::Mapping, Some(tag))
                .is_none(),
            "{tag:?}"
        );
    }
}
