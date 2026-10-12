//! Authored OPC documents pin metadata semantics, original ranges and rejection.
//! Fixtures are independent of archive paths and do not claim package ownership.

use super::*;
use rootlight_cancel::CancellationReason;

fn types(body: &str) -> String {
    format!("<Types xmlns='{TYPES_NS}'>{body}</Types>")
}
fn rels(body: &str) -> String {
    format!("<Relationships xmlns='{RELS_NS}'>{body}</Relationships>")
}
fn parse_types(text: &str) -> Result<ContentTypes, MetadataError> {
    ContentTypes::parse(
        text.as_bytes(),
        MetadataLimits::default(),
        &Cancellation::new(),
    )
}
fn parse_rels(text: &str) -> Result<Relationships, MetadataError> {
    Relationships::parse(
        text.as_bytes(),
        MetadataLimits::default(),
        &Cancellation::new(),
    )
}

#[test]
fn content_type_precedence_identity_and_original_ranges_are_preserved() {
    let text = types(
        "<Default Extension='xml' ContentType='application/xml'/><Override PartName='/α/MAIN.xml' ContentType='text/plain; charset=&quot;utf-8&quot;'/>",
    );
    let parsed = parse_types(&text).unwrap();
    assert_eq!(parsed.source_hash(), content_hash(text.as_bytes()));
    assert_eq!(parsed.declarations().len(), 2);
    let declaration = parsed.for_part("/α/main.XML").unwrap();
    assert_eq!(
        declaration.media_type().value(),
        "text/plain; charset=\"utf-8\""
    );
    assert_eq!(
        &text[declaration.media_type().original_range()],
        "text/plain; charset=&quot;utf-8&quot;"
    );
    assert!(text[declaration.original_range()].starts_with("<Override "));
    assert_eq!(
        parsed
            .for_part("/unrelated.XML")
            .unwrap()
            .media_type()
            .value(),
        "application/xml"
    );
    assert!(parsed.for_part("/unrelated.bin").is_none());
    assert!(parsed.for_part("../α/main.xml").is_none());
    assert!(parsed.for_part("/α//main.xml").is_none());
    assert!(!format!("{parsed:?}").contains("charset"));
}

#[test]
fn prefixes_shadowing_comments_instructions_and_explicit_closing_tags_work() {
    let text = format!(
        "<?xml version='1.0' encoding='UTF-8' standalone='yes'?><!--root--><?inspect ignored?><p:Types xmlns:p='{TYPES_NS}' xmlns:q='urn:unrelated'><q:Default xmlns:q='{TYPES_NS}' Extension='bin' ContentType='application/octet-stream'> <!--empty--> </q:Default></p:Types><?done?>"
    );
    let parsed = parse_types(&text).unwrap();
    assert_eq!(parsed.declarations().len(), 1);
    let record = &parsed.declarations()[0];
    assert!(text[record.original_range()].ends_with("</q:Default>"));
    let wrong = text.replace(&format!("xmlns:q='{TYPES_NS}'"), "xmlns:q='urn:wrong'");
    assert!(matches!(
        parse_types(&wrong),
        Err(MetadataError::Unsupported { .. })
    ));
    assert!(parse_types(&types("&#32;<![CDATA[ \n ]]>")).is_ok());
    assert!(parse_types(&types("<![CDATA[not whitespace]]>")).is_err());
}

#[test]
fn relationships_keep_external_targets_without_resolving_them() {
    let text = rels(
        "<Relationship Id='  δ1  ' Type='urn:kind' Target='../parts/a&amp;b.bin'/><Relationship Id='δ2' Type='urn:Kind' Target='https://invalid.example/x?q=1&amp;y=2' TargetMode='External'>ignored <![CDATA[string content]]></Relationship>",
    );
    let parsed = parse_rels(&text).unwrap();
    assert_eq!(parsed.source_hash(), content_hash(text.as_bytes()));
    let first = &parsed.declarations()[0];
    assert_eq!(first.id().value(), "δ1");
    assert_eq!(first.mode(), TargetMode::Internal);
    assert!(first.mode_source().is_none());
    assert_eq!(first.target().value(), "../parts/a&b.bin");
    let second = &parsed.declarations()[1];
    assert_eq!(second.mode(), TargetMode::External);
    assert_eq!(second.mode_source().unwrap().value(), "External");
    assert_eq!(second.target().value(), "https://invalid.example/x?q=1&y=2");
    assert_ne!(first.kind(), second.kind());
    assert_eq!(&text[first.id().original_range()], "  δ1  ");
}

#[test]
fn utf16_endianness_surrogates_and_attribute_normalization_keep_raw_offsets() {
    let document = format!(
        "<?xml version='1.0' encoding='UTF-16'?><Types xmlns='{TYPES_NS}'><Override PartName='/𐀀/a.bin' ContentType='text/plain; value=&quot;a\r\nb\tc&#x9;d&quot;'/></Types>"
    );
    for little in [false, true] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in document.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let parsed =
            ContentTypes::parse(&bytes, MetadataLimits::default(), &Cancellation::new()).unwrap();
        assert_eq!(parsed.source_hash(), content_hash(&bytes));
        let record = parsed.for_part("/𐀀/a.bin").unwrap();
        assert_eq!(
            record.media_type().value(),
            "text/plain; value=\"a b c\td\""
        );
        let raw = &bytes[record.media_type().original_range()];
        let units: Vec<_> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                if little {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "text/plain; value=&quot;a\r\nb\tc&#x9;d&quot;"
        );
        let raw = &bytes[record.original_range()];
        assert_eq!(
            raw.len(),
            document[document.find("<Override").unwrap()..document.find("</Types>").unwrap()]
                .encode_utf16()
                .count()
                * 2
        );
    }
    let utf8 = document.replace("UTF-16", "UTF-8");
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(utf8.as_bytes());
    let parsed =
        ContentTypes::parse(&bytes, MetadataLimits::default(), &Cancellation::new()).unwrap();
    assert_eq!(
        parsed.declarations()[0].original_range().start,
        3 + utf8.find("<Override").unwrap()
    );
}

#[test]
fn duplicates_conflicting_namespaces_and_schema_extras_are_rejected() {
    for body in [
        "<Default Extension='bin' ContentType='text/plain'/><Default Extension='BIN' ContentType='text/plain'/>",
        "<Override PartName='/A/x' ContentType='text/plain'/><Override PartName='/a/X' ContentType='text/plain'/>",
    ] {
        assert!(matches!(
            parse_types(&types(body)),
            Err(MetadataError::Duplicate { .. })
        ));
    }
    let duplicate = rels(
        "<Relationship Id='r1' Type='urn:a' Target='x'/><Relationship Id=' r1 ' Type='urn:b' Target='y'/>",
    );
    assert!(matches!(
        parse_rels(&duplicate),
        Err(MetadataError::Duplicate { .. })
    ));
    for body in [
        "<Default Extension='x' Extension='y' ContentType='text/plain'/>",
        "<Default Extension='x' ContentType='text/plain' Extra='y'/>",
        "<Default Extension='x' ContentType='text/plain' xmlns:xml='urn:wrong'/>",
        "<Default Extension='x' ContentType='text/plain' xmlns:a='http://www.w3.org/XML/1998/namespace'/>",
        "<Default Extension='x' ContentType='text/plain' xmlns:xmlns='urn:a'/>",
        "<Default Extension='x' ContentType='text/plain' xmlns:p=''/>",
        "<Default Extension='x' ContentType='text/plain' xml:base='https://invalid.example/'/>",
        "<Default Extension='x'/>",
        "<Unknown/>",
    ] {
        assert!(parse_types(&types(body)).is_err(), "{body}");
    }
    assert!(parse_types(&format!("<Types xmlns='{TYPES_NS}' Extra='x'/>")).is_err());
}

#[test]
fn malformed_xml_dtds_entities_and_document_smuggling_are_rejected() {
    let valid = types("<Default Extension='bin' ContentType='text/plain'/>");
    for prefix in [
        "<!DOCTYPE Types>",
        "<!DOCTYPE Types [<!ENTITY x SYSTEM 'file:///never-read'>]>",
        "<?xml version='1.1'?>",
        "<!-- -- -->",
        "<?XML?>",
    ] {
        assert!(
            parse_types(&format!("{prefix}{valid}")).is_err(),
            "{prefix}"
        );
    }
    for suffix in [
        "<Types/>",
        "trailing",
        "<!--",
        "<?xml version='1.0'?>",
        "\0",
    ] {
        assert!(parse_types(&format!("{valid}{suffix}")).is_err());
    }
    for value in [
        "&unknown;",
        "&#0;",
        "&#xD800;",
        "&#x110000;",
        "&#-1;",
        "&#x+41;",
        "&#X41;",
        "a<b",
        "&amp",
    ] {
        assert!(
            parse_types(&types(&format!(
                "<Default Extension='{value}' ContentType='text/plain'/>"
            )))
            .is_err(),
            "{value}"
        );
    }
    for bad in [
        valid.replace("/>", ">text</Default>"),
        valid.replace("/>", "><Nested/></Default>"),
        valid.replace("</Types>", "</Other>"),
        valid.replace(" ContentType", "ContentType"),
    ] {
        assert!(parse_types(&bad).is_err());
    }
    let nested = format!(
        "<Relationships xmlns='{RELS_NS}'><Relationship Id='r' Type='x' Target='y'><Relationship/></Relationship></Relationships>"
    );
    assert!(parse_rels(&nested).is_err());
}

#[test]
fn limits_cancellation_and_all_truncations_are_total() {
    let text = types("<Default Extension='bin' ContentType='text/plain'/>");
    for end in 0..text.len() {
        assert!(
            ContentTypes::parse(
                &text.as_bytes()[..end],
                MetadataLimits::default(),
                &Cancellation::new()
            )
            .is_err(),
            "{end}"
        );
    }
    for limits in [
        MetadataLimits {
            bytes: text.len() - 1,
            ..MetadataLimits::default()
        },
        MetadataLimits {
            records: 0,
            ..MetadataLimits::default()
        },
        MetadataLimits {
            attributes: 1,
            ..MetadataLimits::default()
        },
        MetadataLimits {
            value_bytes: 2,
            ..MetadataLimits::default()
        },
    ] {
        assert!(matches!(
            ContentTypes::parse(text.as_bytes(), limits, &Cancellation::new()),
            Err(MetadataError::Limit { .. })
        ));
    }
    for limits in [
        MetadataLimits {
            bytes: usize::MAX,
            ..MetadataLimits::default()
        },
        MetadataLimits {
            records: usize::MAX,
            ..MetadataLimits::default()
        },
        MetadataLimits {
            attributes: usize::MAX,
            ..MetadataLimits::default()
        },
        MetadataLimits {
            value_bytes: usize::MAX,
            ..MetadataLimits::default()
        },
    ] {
        assert!(matches!(
            ContentTypes::parse(text.as_bytes(), limits, &Cancellation::new()),
            Err(MetadataError::Limits)
        ));
    }
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        ContentTypes::parse(text.as_bytes(), MetadataLimits::default(), &cancel),
        Err(MetadataError::Cancelled(_))
    ));
    let limits = MetadataLimits {
        bytes: text.len(),
        records: 1,
        attributes: 2,
        ..MetadataLimits::default()
    };
    assert!(ContentTypes::parse(text.as_bytes(), limits, &Cancellation::new()).is_ok());
}

#[test]
fn malformed_encodings_and_contradictory_declarations_fail() {
    for bytes in [
        vec![0xff],
        vec![0xff, 0xfe, 0],
        vec![0xfe, 0xff, 0xd8, 0],
        vec![0xff, 0xfe, 0, 0xdc],
        b"<?xml version='1.0' encoding='UTF-16'?><Types/>".to_vec(),
        b"<?xml version='1.0' encoding='latin1'?><Types/>".to_vec(),
    ] {
        assert!(matches!(
            ContentTypes::parse(&bytes, MetadataLimits::default(), &Cancellation::new()),
            Err(MetadataError::Encoding)
        ));
    }
}

#[test]
fn part_selectors_extensions_and_media_types_reject_ambiguous_forms() {
    for name in [
        "a/b", "/", "/a//b", "/a/../b", "/a/b.", "/a?b", "/a#b", "/a\\b", "/a/%2f", "/a/%5C",
        "/a/%61", "/a/%", "/a/%gg",
    ] {
        assert!(!valid_part_name(name), "{name}");
    }
    for name in ["/a/b", "/α/𐀀", "/a/%20b", "/a/a:b@c", "/a/a%3Fb"] {
        assert!(valid_part_name(name), "{name}");
    }
    for extension in ["", "a.b", "a;b", "a/b", "a?b"] {
        assert!(!valid_extension(extension));
    }
    for mime in [
        "",
        "text",
        "/plain",
        "text/",
        "text/plain;",
        "text/plain; a=",
        "text/plain; a=\"unterminated",
        "text/plain; a=\"bad\nvalue\"",
        "text/plain junk",
    ] {
        assert!(!valid_media_type(mime), "{mime}");
    }
    for mime in [
        "text/plain",
        "TEXT/PLAIN; charset=utf-8",
        "application/octet-stream; x=\"a\\\"b\"",
        "text/plain; a=\"\"",
    ] {
        assert!(valid_media_type(mime), "{mime}");
    }
    for body in [
        "<Relationship Id='1bad' Type='x' Target='y'/>",
        "<Relationship Id='a:b' Type='x' Target='y'/>",
        "<Relationship Id='r' Type='x' Target='y' TargetMode='external'/>",
    ] {
        assert!(parse_rels(&rels(body)).is_err());
    }
}
