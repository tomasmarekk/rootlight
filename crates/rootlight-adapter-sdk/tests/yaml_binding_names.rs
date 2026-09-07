//! YAML serialization names are literal tokens, not Core-schema scalar keys.

use rootlight_adapter_sdk::{
    SyntaxFact, SyntaxFactKind, SyntaxKindLabel, structural_captured_name_for_fact,
};
use rootlight_ids::FileId;
use rootlight_ir::SourceSpan;

fn fact(label: &str, length: usize) -> SyntaxFact {
    SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Occurrence,
        SourceSpan::new(
            FileId::from_bytes([1; 20]),
            0,
            u64::try_from(length).unwrap(),
        )
        .unwrap(),
        0,
        SyntaxKindLabel::new(label).unwrap(),
    )
}

#[test]
fn binding_roles_preserve_raw_unicode_punctuation_and_scalar_looking_names() {
    for name in [
        "true",
        "null",
        "01",
        "'quoted'",
        r"a\u0062",
        "a:b#c&d*e",
        "🌍",
        "a\u{85}b",
        "a\u{a0}b",
        "a\u{2028}b",
        "a\u{2029}b",
    ] {
        for role in ["yaml.anchor.definition", "yaml.alias.reference"] {
            let fact = fact(role, name.len());
            let decoded =
                structural_captured_name_for_fact("yaml", &fact, name, name.len()).unwrap();
            assert_eq!(decoded, name);
            assert!(matches!(decoded, std::borrow::Cow::Borrowed(_)));
            assert!(
                structural_captured_name_for_fact("yaml", &fact, name, name.len() - 1).is_none()
            );
        }
    }
}

#[test]
fn binding_roles_reject_forbidden_characters_without_trimming() {
    for name in [
        "",
        " x",
        "x ",
        "a\tb",
        "a\nb",
        "a\rb",
        "a[b",
        "a]b",
        "a{b",
        "a}b",
        "a,b",
        "a\0b",
        "a\u{7f}b",
        "a\u{feff}b",
        "a\u{fffe}b",
    ] {
        for role in ["yaml.anchor.definition", "yaml.alias.reference"] {
            assert!(
                structural_captured_name_for_fact("yaml", &fact(role, name.len()), name, 1024)
                    .is_none(),
                "{role}: {name:?}"
            );
        }
    }
}

#[test]
fn ordinary_yaml_keys_keep_core_schema_canonicalization() {
    assert_eq!(
        structural_captured_name_for_fact("yaml", &fact("yaml.key.definition", 4), "true", 1024)
            .unwrap(),
        "bool:true"
    );
    assert_eq!(
        structural_captured_name_for_fact("lua", &fact("lua.local.definition", 4), "true", 1024)
            .unwrap(),
        "true"
    );
}
