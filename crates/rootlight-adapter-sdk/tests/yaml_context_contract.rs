//! Public document-local YAML scalar contracts, separate from native parsing.
//! Fixtures exercise tag evidence, type identity and bounded failure without
//! claiming application-specific constructors or whole-document semantics.

use rootlight_adapter_sdk::{YamlDocumentContext, structural_captured_name_for_language};

fn name(context: &YamlDocumentContext<'_>, text: &str, tag: Option<&str>) -> String {
    context.flow_scalar(text, tag).unwrap().into_name()
}

#[test]
fn explicit_core_tags_override_style_without_losing_exact_numeric_values() {
    let context = YamlDocumentContext::new(None, &[], 1024).unwrap();
    for (text, tag, expected) in [
        ("true", "!!str", "str:\"true\""),
        ("true", "!", "str:\"true\""),
        ("'TRUE'", "!!bool", "bool:true"),
        ("\"\\u0046ALSE\"", "!!bool", "bool:false"),
        ("'Null'", "!!null", "null:null"),
        ("\"~\"", "!!null", "null:null"),
        ("'0x10000000000000000'", "!!int", "int:18446744073709551616"),
        ("'-00011'", "!!int", "int:-11"),
        ("'10'", "!!float", "float:1e1"),
        ("'-0'", "!!float", "float:0"),
        (
            "'10e999999999999999999999'",
            "!!float",
            "float:1e1000000000000000000000",
        ),
        ("'.NaN'", "!!float", "float:nan"),
        ("'-.INF'", "!!float", "float:-inf"),
    ] {
        let scalar = context.flow_scalar(text, Some(tag)).unwrap();
        assert_eq!(scalar.name(), expected, "{tag} {text}");
        assert!(!scalar.has_unrecognized_tag());
        let verbatim = format!("!<tag:yaml.org,2002:{}>", tag.trim_start_matches('!'));
        if tag != "!" {
            assert_eq!(name(&context, text, Some(&verbatim)), expected);
        }
    }
    for (text, tag) in [
        ("yes", "!!bool"),
        ("1", "!!bool"),
        ("true", "!!null"),
        ("1.0", "!!int"),
        ("0o8", "!!int"),
        ("-0x1", "!!int"),
        ("0x10", "!!float"),
        ("0o7", "!!float"),
        ("1_000", "!!float"),
        ("plain", "!!seq"),
        ("plain", "!!map"),
    ] {
        assert!(
            context.flow_scalar(text, Some(tag)).is_none(),
            "{tag} {text}"
        );
    }
}

#[test]
fn empty_nodes_are_explicit_and_keep_null_distinct_from_empty_strings() {
    let context = YamlDocumentContext::new(None, &[], 128).unwrap();
    for (text, tag, expected) in [
        ("", None, "null:null"),
        ("", Some("!!null"), "null:null"),
        ("''", Some("!!null"), "null:null"),
        ("", Some("!!str"), "str:\"\""),
        ("", Some("!"), "str:\"\""),
        ("''", None, "str:\"\""),
    ] {
        assert_eq!(name(&context, text, tag), expected);
    }
    for tag in ["!!bool", "!!int", "!!float", "!!map", "!!seq"] {
        assert!(context.flow_scalar("", Some(tag)).is_none());
    }
    assert!(structural_captured_name_for_language("yaml", "", 128).is_none());
}

#[test]
fn directives_override_handles_only_inside_their_own_document() {
    let context = YamlDocumentContext::new(
        Some("1.2"),
        &[
            ("!", "tag:yaml.org,2002:"),
            ("!!", "!application/"),
            ("!core!", "tag:yaml.org,2002:"),
            ("!alias!", "tag:yaml.org,2002:"),
        ],
        256,
    )
    .unwrap();
    assert_eq!(name(&context, "12", Some("!str")), "str:\"12\"");
    assert_eq!(name(&context, "12", Some("!core!int")), "int:12");
    assert_eq!(name(&context, "12", Some("!alias!int")), "int:12");
    assert_eq!(name(&context, "12", Some("!")), "str:\"12\"");
    let opaque = context.flow_scalar("12", Some("!!int")).unwrap();
    assert!(opaque.has_unrecognized_tag());
    assert_eq!(opaque.name(), "tag:\"!application/int\":\"12\"");
    assert_eq!(
        name(&context, "12", Some("!<tag:yaml.org,2002:int>")),
        "int:12"
    );
    let next = YamlDocumentContext::new(None, &[], 256).unwrap();
    assert_eq!(name(&next, "12", Some("!!int")), "int:12");
    assert!(next.flow_scalar("12", Some("!core!int")).is_none());
    assert!(
        next.flow_scalar("12", Some("!str"))
            .unwrap()
            .has_unrecognized_tag()
    );
    assert_eq!(name(&context, "12", None), name(&next, "12", None));
}

#[test]
fn opaque_tag_evidence_preserves_exact_escapes_and_decoded_content() {
    let context =
        YamlDocumentContext::new(None, &[("!app!", "tag:example.org,2000:")], 512).unwrap();
    let a = context.flow_scalar("'a''b'", Some("!app!type%21")).unwrap();
    let b = context
        .flow_scalar("\"a'b\"", Some("!<tag:example.org,2000:type%21>"))
        .unwrap();
    assert_eq!(a, b);
    assert!(a.has_unrecognized_tag());
    assert_eq!(a.name(), "tag:\"tag:example.org,2000:type%21\":\"a'b\"");
    for other in ["!app!type%2f", "!app!type%2F", "!app!type/", "!app!type%21"] {
        let identity = context.flow_scalar("value", Some(other)).unwrap();
        assert!(identity.has_unrecognized_tag());
        let resolved = format!(
            "tag:example.org,2000:{}",
            other.strip_prefix("!app!").unwrap()
        );
        assert_eq!(
            identity.name(),
            format!(
                "tag:{}:{}",
                serde_json::to_string(&resolved).unwrap(),
                serde_json::to_string("value").unwrap()
            )
        );
    }
    for (left, right) in [
        ("!app!type%2f", "!app!type%2F"),
        ("!app!type%2F", "!app!type/"),
        ("!app!Type", "!app!type"),
        ("!<tag:yaml.org,2002:%73tr>", "!!str"),
    ] {
        assert_ne!(
            name(&context, "true", Some(left)),
            name(&context, "true", Some(right))
        );
    }
    assert_ne!(
        name(&context, "a:b", Some("!x")),
        name(&context, "b", Some("!x:a"))
    );
    assert_eq!(name(&context, "'\\n'", Some("!x")), "tag:\"!x\":\"\\\\n\"");
    assert_eq!(
        name(&context, "\"\\n\"", Some("!x")),
        "tag:\"!x\":\"\\u000a\""
    );
    assert!(
        context
            .flow_scalar("abc", Some("!!timestamp"))
            .unwrap()
            .has_unrecognized_tag()
    );
}

#[test]
fn malformed_directives_and_tags_fail_without_guessing_a_type() {
    for directives in [
        vec![("!a!", "!x"), ("!a!", "!x")],
        vec![("!", "!x"), ("!", "!y")],
        vec![("!!", "!x"), ("!!", "!y")],
        vec![("!a_b!", "!x")],
        vec![("!é!", "!x")],
        vec![("!a", "!x")],
        vec![("!a!", "")],
        vec![("!a!", "[abc")],
        vec![("!a!", "!a b")],
        vec![("!a!", "!%G0")],
        vec![("!a!", "!%2")],
    ] {
        assert!(
            YamlDocumentContext::new(None, &directives, 128).is_none(),
            "{directives:?}"
        );
    }
    let context = YamlDocumentContext::new(None, &[("!a!", "!x")], 128).unwrap();
    for tag in [
        "",
        "?",
        "str",
        "!!",
        "!a!",
        "!missing!type",
        "!!!str",
        "!a!!x",
        "!<>",
        "!<x>",
        "!<!>",
        "!<$:?>",
        "!<tag:example.org:type",
        "!a b",
        "!a,",
        "!a[",
        "!a]",
        "!a{",
        "!a}",
        "!a%",
        "!a%2",
        "!a%xy",
        "!a🌍",
        "!<tag:example.org:type>junk",
        "!<tag:example.org:a\nb>",
    ] {
        assert!(context.flow_scalar("true", Some(tag)).is_none(), "{tag:?}");
    }
    for source in [
        "*alias",
        "&anchor value",
        "!!str true",
        "[key]",
        "{key: value}",
        "|+\n  value",
        ">-\n  value",
    ] {
        assert!(
            context.flow_scalar(source, Some("!!str")).is_none(),
            "{source:?}"
        );
    }
}

#[test]
fn version_policy_is_explicit_bounded_and_does_not_enable_legacy_types() {
    for (version, warning) in [
        (None, false),
        (Some("1.2"), false),
        (Some("01.002"), false),
        (Some("1.1"), true),
        (Some("1.3"), true),
        (Some("1.999999999999999999999999999999999"), true),
    ] {
        let context = YamlDocumentContext::new(version, &[], 128).unwrap();
        assert_eq!(context.has_version_warning(), warning);
        assert_eq!(name(&context, "yes", None), "str:\"yes\"");
        assert_eq!(name(&context, "1:20", None), "str:\"1:20\"");
        assert_eq!(name(&context, "true", None), "bool:true");
    }
    for version in [
        "", "1", "1.", ".2", "1.0", "0.2", "2.0", "1.2.2", "+1.2", "1. 2", "1.a",
    ] {
        assert!(
            YamlDocumentContext::new(Some(version), &[], 128).is_none(),
            "{version}"
        );
    }
    assert!(YamlDocumentContext::new(Some("1.2"), &[], 2).is_none());
}

#[test]
fn source_directive_expansion_and_output_budgets_fail_at_exact_boundaries() {
    let version = Some("1.2");
    let directives = [("!a!", "!kind"), ("!b!", "!value")];
    let bytes = 3 + 3 + 5 + 3 + 6;
    assert!(YamlDocumentContext::new(version, &directives, bytes).is_some());
    assert!(YamlDocumentContext::new(version, &directives, bytes - 1).is_none());
    for (text, tag) in [
        ("true", None),
        ("'0xFFFFFFFF'", Some("!!int")),
        ("'true'", Some("!!bool")),
        ("\"\\0\"", Some("!x")),
        ("", Some("!!str")),
    ] {
        let full = YamlDocumentContext::new(None, &[], 256)
            .unwrap()
            .flow_scalar(text, tag)
            .unwrap();
        let expanded = match tag {
            Some("!!int") => "tag:yaml.org,2002:int".len(),
            Some("!!bool") => "tag:yaml.org,2002:bool".len(),
            Some("!!str") => "tag:yaml.org,2002:str".len(),
            _ => 0,
        };
        let required = full
            .name()
            .len()
            .max(text.len() + tag.map_or(0, str::len))
            .max(expanded);
        assert_eq!(
            YamlDocumentContext::new(None, &[], required)
                .unwrap()
                .flow_scalar(text, tag),
            Some(full)
        );
        assert!(
            YamlDocumentContext::new(None, &[], required - 1)
                .unwrap()
                .flow_scalar(text, tag)
                .is_none()
        );
    }
    let context = YamlDocumentContext::new(None, &[("!a!", "tag:example.org:")], 19).unwrap();
    assert!(context.flow_scalar("x", Some("!a!suffix")).is_none());
    let context = YamlDocumentContext::new(None, &[], usize::MAX).unwrap();
    assert_eq!(name(&context, "12", None), "int:12");
}

#[test]
fn document_untagged_flow_path_preserves_the_existing_shared_contract() {
    let context = YamlDocumentContext::new(None, &[], 512).unwrap();
    for source in [
        "null",
        "true",
        "false",
        "name",
        "'true'",
        "\"\\x61\"",
        "'a\n  b'",
        "0xFF",
        "0o77",
        "-0012",
        "2.50e-1",
        ".inf",
        ".nan",
        "'🌍'",
        "''",
        "1_000",
    ] {
        let legacy = structural_captured_name_for_language("yaml", source, 512).unwrap();
        let current = context.flow_scalar(source, None).unwrap();
        assert_eq!(current.name(), legacy);
        assert!(!current.has_unrecognized_tag());
    }
}

#[test]
fn tag_character_table_and_many_directives_preserve_exact_document_lookup() {
    let plain = YamlDocumentContext::new(None, &[], 256).unwrap();
    for byte in 0u8..=127 {
        let tag = format!("!kind{}tail", char::from(byte));
        let accepted = byte.is_ascii_alphanumeric() || b"-#;/?:@&=+$_.~*'()".contains(&byte);
        assert_eq!(
            plain.flow_scalar("value", Some(&tag)).is_some(),
            accepted,
            "{tag:?}"
        );
    }
    for byte in 0u8..=255 {
        let tag = format!("!kind%{byte:02X}");
        let scalar = plain.flow_scalar("value", Some(&tag)).unwrap();
        assert_eq!(scalar.name(), format!("tag:\"{tag}\":\"value\""));
        assert!(scalar.has_unrecognized_tag());
    }
    let handles: Vec<_> = (0..512).map(|i| format!("!handle-{i}!")).collect();
    let prefixes: Vec<_> = (0..512).map(|i| format!("!type-{i}/")).collect();
    let directives: Vec<_> = handles
        .iter()
        .zip(&prefixes)
        .map(|(h, p)| (h.as_str(), p.as_str()))
        .collect();
    let context = YamlDocumentContext::new(None, &directives, 32768).unwrap();
    for (handle, prefix) in &directives {
        let scalar = context
            .flow_scalar("12", Some(&format!("{handle}scalar")))
            .unwrap();
        assert_eq!(scalar.name(), format!("tag:\"{prefix}scalar\":\"12\""));
        assert!(scalar.has_unrecognized_tag());
    }
    let reversed: Vec<_> = directives.iter().copied().rev().collect();
    let reordered = YamlDocumentContext::new(None, &reversed, 32768).unwrap();
    for handle in &handles {
        let tag = format!("{handle}scalar");
        assert_eq!(
            context.flow_scalar("12", Some(&tag)),
            reordered.flow_scalar("12", Some(&tag))
        );
    }
}
