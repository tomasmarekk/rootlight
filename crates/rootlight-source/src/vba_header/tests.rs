//! Authored native headers distinguish effective module identity from body text.
//! Encoding and framing variations must preserve exact original-byte provenance.

use super::*;
use crate::vba_text::TextLimits;
use rootlight_cancel::CancellationReason;

fn decode(bytes: &[u8], page: u16) -> DecodedText<'_> {
    DecodedText::decode(bytes, page, TextLimits::default(), &Cancellation::new()).unwrap()
}

fn parse<'a>(text: &'a DecodedText<'_>, kind: ModuleKind) -> Result<HeaderName<'a>, HeaderError> {
    HeaderName::parse(text, kind, HeaderLimits::default(), &Cancellation::new())
}

#[test]
fn exposed_classes_keep_the_effective_literal_and_original_coordinates() {
    let input = b"Attribute VB_Name = \"Caf\xe9\"\r\nAttribute VB_Exposed = False\r\nAttribute vb_exposed _\r\n = tRuE\r\nSub Work()\r\nAttribute VB_Exposed = False\r\nEnd Sub\r\n";
    let text = decode(input, 1252);
    let header = parse(&text, ModuleKind::Object).unwrap();
    let exposed = header.exposed().unwrap();
    assert!(exposed.value());
    assert_eq!(&input[exposed.original_range()], b"tRuE");
    assert_eq!(&text.text()[exposed.utf8_range()], "tRuE");
    assert_eq!(
        exposed.utf8_range().start,
        exposed.original_range().start + 1
    );
    for (attributes, expected) in [
        ("", None),
        ("Attribute VB_Exposed = True\n", Some(true)),
        (
            "Attribute VB_Exposed = True\nAttribute VB_Exposed = False\n",
            Some(false),
        ),
        ("Attribute VB_Description = \"VB_Exposed = True\"\n", None),
    ] {
        let input = format!("Attribute VB_Name = \"Object\"\n{attributes}");
        let text = decode(input.as_bytes(), 65001);
        let header = parse(&text, ModuleKind::Object).unwrap();
        assert_eq!(header.exposed().map(HeaderBoolean::value), expected);
        assert!(
            parse(&text, ModuleKind::Procedural)
                .unwrap()
                .exposed()
                .is_none()
        );
    }
    for value in [
        "1",
        "-1",
        "\"True\"",
        "TrueX",
        "True:False",
        "True ' comment",
        "Tr _\nue",
        "",
    ] {
        let input = format!("Attribute VB_Name = \"Object\"\nAttribute VB_Exposed = {value}\n");
        let text = decode(input.as_bytes(), 65001);
        assert!(
            matches!(
                parse(&text, ModuleKind::Object),
                Err(HeaderError::Invalid { .. })
            ),
            "{value}"
        );
    }
}

#[test]
fn predeclared_id_uses_last_explicit_literal_and_exact_coordinates() {
    let input = b"Attribute VB_Name = \"Caf\xe9\"\r\nAttribute VB_PredeclaredId = False\r\nAttribute vb_predeclaredid _\r\n = _\r\n tRuE\r\nSub Work()\r\nAttribute VB_PredeclaredId = False\r\nEnd Sub\r\n";
    let text = decode(input, 1252);
    let header = parse(&text, ModuleKind::Object).unwrap();
    let value = header.predeclared_id().unwrap();
    assert!(value.value());
    assert_eq!(&input[value.original_range()], b"tRuE");
    assert_eq!(&text.text()[value.utf8_range()], "tRuE");
    assert_eq!(value.utf8_range().start, value.original_range().start + 1);
    for (attributes, expected) in [
        ("", None),
        ("Attribute VB_PredeclaredId = True\n", Some(true)),
        (
            "Attribute VB_PredeclaredId = True\nAttribute VB_PredeclaredId = False\n",
            Some(false),
        ),
        ("Attribute VB_GlobalNameSpace = True\n", None),
        (
            "Attribute VB_Description = \"VB_PredeclaredId = True\"\n",
            None,
        ),
    ] {
        let input = format!("Attribute VB_Name = \"Object\"\n{attributes}");
        let text = decode(input.as_bytes(), 65001);
        let header = parse(&text, ModuleKind::Object).unwrap();
        assert_eq!(header.predeclared_id().map(HeaderBoolean::value), expected);
    }
}

#[test]
fn malformed_predeclared_values_never_establish_instance_identity() {
    for value in [
        "1",
        "-1",
        "\"True\"",
        "TrueX",
        "True:False",
        "True ' comment",
        "Tr _\nue",
        "",
    ] {
        let input =
            format!("Attribute VB_Name = \"Object\"\nAttribute VB_PredeclaredId = {value}\n");
        let text = decode(input.as_bytes(), 65001);
        assert!(
            matches!(
                parse(&text, ModuleKind::Object),
                Err(HeaderError::Invalid { .. })
            ),
            "{value}"
        );
    }
    let text = decode(
        b"Attribute VB_Name = \"Module\"\nAttribute VB_PredeclaredId = True\n",
        65001,
    );
    assert!(matches!(
        parse(&text, ModuleKind::Procedural),
        Err(HeaderError::Invalid { .. })
    ));
}

#[test]
fn predeclared_attributes_obey_header_limits_and_truncation() {
    let input = "Attribute VB_Name = \"Object\"\nAttribute VB_PredeclaredId = True\n";
    for end in 0..=input.len() {
        let text = decode(&input.as_bytes()[..end], 65001);
        for maximum in [0, end.saturating_sub(1), end] {
            if let Ok(header) = HeaderName::parse(
                &text,
                ModuleKind::Object,
                HeaderLimits {
                    bytes: maximum,
                    attributes: 2,
                },
                &Cancellation::new(),
            ) && let Some(attribute) = header.predeclared_id()
            {
                assert!(attribute.value());
                assert_eq!(&text.text()[attribute.utf8_range()], "True");
                assert!(attribute.utf8_range().end <= maximum);
            }
        }
    }
    let text = decode(input.as_bytes(), 65001);
    assert!(matches!(
        HeaderName::parse(
            &text,
            ModuleKind::Object,
            HeaderLimits {
                bytes: input.len(),
                attributes: 1
            },
            &Cancellation::new()
        ),
        Err(HeaderError::Limit)
    ));
}

#[test]
fn effective_name_retains_original_codepage_bytes() {
    let bytes = b"Attribute VB_Description = \"\x80\"\r\nAttribute VB_Name = \"Caf\xe9\"\r\nOption Explicit\r\n";
    let text = decode(bytes, 1252);
    let name = parse(&text, ModuleKind::Procedural).unwrap();
    assert_eq!(name.name(), "Café");
    assert_eq!(&text.text()[name.utf8_range()], name.name());
    assert_eq!(&bytes[name.original_range()], b"Caf\xe9");
    assert_eq!(name.utf8_range().start, name.original_range().start + 2);
    assert_eq!(name.declarations(), 1);
    assert_eq!(name.kind(), ModuleKind::Procedural);
    assert!(!format!("{name:?}").contains(name.name()));
}

#[test]
fn physical_lines_and_token_continuations_preserve_identity() {
    for newline in ["\r\n", "\r", "\n", "\u{2028}", "\u{2029}"] {
        let input = format!(
            "\tATTRIBUTE _{newline} VB_Name _{newline} = _{newline} \"Source\" _{newline} {newline}Sub Work(){newline}End Sub"
        );
        let text = decode(input.as_bytes(), 65001);
        let name = parse(&text, ModuleKind::Procedural).unwrap();
        assert_eq!(name.name(), "Source");
        assert_eq!(&input.as_bytes()[name.original_range()], b"Source");
    }
    for suffix in ["", "\t", "\u{19}", "\u{1680}", "\u{2000}", "\u{3000}"] {
        let input = format!("Attribute vb_name = \"Source\"{suffix}");
        let text = decode(input.as_bytes(), 65001);
        assert_eq!(
            parse(&text, ModuleKind::Procedural).unwrap().name(),
            "Source"
        );
    }
}

#[test]
fn class_uses_last_header_name_but_procedural_duplicates_fail() {
    let input = b"Attribute VB_Name = \"Before\"\nAttribute VB_GlobalNameSpace = False\nAttribute vb_name = \"After\"\nSub Work()\nAttribute VB_Name = \"Body\"\nEnd Sub\n";
    let text = decode(input, 1252);
    let name = parse(&text, ModuleKind::Object).unwrap();
    assert_eq!(name.name(), "After");
    assert_eq!(name.declarations(), 2);
    assert_eq!(&input[name.original_range()], b"After");
    assert!(matches!(
        parse(&text, ModuleKind::Procedural),
        Err(HeaderError::Duplicate)
    ));
}

#[test]
fn comments_preambles_and_body_attributes_cannot_supply_a_name() {
    for prefix in [
        "' comment\n",
        "Rem comment\n",
        "\n",
        "Option Explicit\n",
        "Sub Work()\n",
        "VERSION 1.0 CLASS\n",
        "BEGIN\n",
        "\"payload\"\n",
        "AttributeVB_Name = \"False\"\n",
    ] {
        let input = format!("{prefix}Attribute VB_Name = \"False\"\n");
        let text = decode(input.as_bytes(), 65001);
        assert!(
            matches!(parse(&text, ModuleKind::Object), Err(HeaderError::Missing)),
            "{prefix:?}"
        );
    }
    let input =
        b"Attribute VB_Description = \"VB_Name\"\nSub Work()\nAttribute VB_Name = \"False\"\n";
    assert!(matches!(
        parse(&decode(input, 1252), ModuleKind::Object),
        Err(HeaderError::Missing)
    ));
}

#[test]
fn malformed_attributes_never_return_a_partial_identity() {
    for tail in [
        "VB_Name = \"\"",
        "VB_Name = \"Unclosed",
        "VB_Name = Source",
        "VB_Name = \"Has Space\"",
        "VB_Name = \"A\tB\"",
        "VB_Name = \"A\0B\"",
        "VB_Name = \"A _\nB\"",
        "VB_Name = \"Source\" ' comment",
        "VB_Name = \"Source\" : End",
        "VB_Name = \"Source\"\"",
        "VB_Name",
        "= \"Source\"",
        "VB_Description =",
        "VB_Name = \"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef\"",
    ] {
        let input = format!("Attribute VB_Name = \"Before\"\nAttribute {tail}\n");
        let text = decode(input.as_bytes(), 65001);
        assert!(
            matches!(
                parse(&text, ModuleKind::Object),
                Err(HeaderError::Invalid { .. })
            ),
            "{tail:?}"
        );
    }
    let input = b"Attribute VB_Name = \"ABCDEFGHIJKLMNOPQRSTUVWXYZabcde\"";
    assert_eq!(
        parse(&decode(input, 1252), ModuleKind::Procedural)
            .unwrap()
            .name()
            .len(),
        31
    );
}

#[test]
fn header_limits_count_unknown_attributes_and_exact_bytes() {
    let first = "Attribute VB_Name = \"Source\"\r\n";
    let input = format!("{first}Sub Work()\n{}", "body".repeat(100));
    let text = decode(input.as_bytes(), 65001);
    let cancel = Cancellation::new();
    let limits = HeaderLimits {
        bytes: first.len(),
        attributes: 1,
    };
    assert!(HeaderName::parse(&text, ModuleKind::Procedural, limits, &cancel).is_ok());
    assert!(matches!(
        HeaderName::parse(
            &text,
            ModuleKind::Procedural,
            HeaderLimits {
                bytes: first.len() - 1,
                ..limits
            },
            &cancel
        ),
        Err(HeaderError::Limit)
    ));
    assert!(matches!(
        HeaderName::parse(
            &text,
            ModuleKind::Procedural,
            HeaderLimits {
                attributes: 0,
                ..limits
            },
            &cancel
        ),
        Err(HeaderError::Limit)
    ));
    let input = format!("{first}Attribute Custom = False\n");
    let text = decode(input.as_bytes(), 65001);
    assert!(matches!(
        HeaderName::parse(
            &text,
            ModuleKind::Object,
            HeaderLimits {
                bytes: input.len(),
                attributes: 1
            },
            &cancel
        ),
        Err(HeaderError::Limit)
    ));
    assert!(
        HeaderName::parse(
            &text,
            ModuleKind::Object,
            HeaderLimits {
                bytes: input.len(),
                attributes: 2
            },
            &cancel
        )
        .is_ok()
    );
    for limits in [
        HeaderLimits {
            bytes: usize::MAX,
            ..HeaderLimits::default()
        },
        HeaderLimits {
            attributes: usize::from(u16::MAX) + 1,
            ..HeaderLimits::default()
        },
    ] {
        assert!(matches!(
            HeaderName::parse(&text, ModuleKind::Object, limits, &cancel),
            Err(HeaderError::Limits)
        ));
    }
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        HeaderName::parse(&text, ModuleKind::Object, HeaderLimits::default(), &cancel),
        Err(HeaderError::Cancelled(_))
    ));
}

#[test]
fn all_unicode_truncations_and_small_budgets_remain_total() {
    let input =
        "\u{3000}Attribute _\u{2028} VB_Name = \"Méthode\"\u{2029}Attribute VB_Name = \"Final\"";
    for end in input.char_indices().map(|(at, _)| at).chain([input.len()]) {
        let text = decode(&input.as_bytes()[..end], 65001);
        for bytes in 0..=input.len() {
            if let Ok(name) = HeaderName::parse(
                &text,
                ModuleKind::Object,
                HeaderLimits {
                    bytes,
                    attributes: 3,
                },
                &Cancellation::new(),
            ) {
                assert_eq!(&input[name.original_range()], name.name());
                assert!(name.original_range().end <= bytes);
            }
        }
    }
}
