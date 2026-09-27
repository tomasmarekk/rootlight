//! Authored identifiers distinguish declared type-library identity from labels.
//! Non-ASCII payloads exercise original-byte provenance instead of UTF-8 offsets.

use super::*;
use rootlight_cancel::CancellationReason;

const GUID: &str = "{01234567-89AB-cdef-1032-547698badcfe}";

fn parse(bytes: &[u8]) -> Result<ReferenceIdentifier<'_>, ReferenceError> {
    ReferenceIdentifier::parse(
        bytes,
        0..bytes.len(),
        1252,
        TextLimits::default(),
        &Cancellation::new(),
    )
}

#[test]
fn library_fields_are_hexadecimal_and_do_not_conflate_names_or_paths() {
    for (tag, platform) in [
        ("G", ReferencePlatform::Windows),
        ("H", ReferencePlatform::Macintosh),
    ] {
        let text = format!("*\\{tag}{GUID}#aBcd.Ff#1234aBcd#../untrusted/Library#Display#Suffix");
        let mut owner = b"prefix:".to_vec();
        owner.extend(text.as_bytes());
        let end = owner.len();
        owner.extend(b":suffix");
        let parsed = ReferenceIdentifier::parse(
            &owner,
            7..end,
            1252,
            TextLimits::default(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            parsed.identity(),
            ReferenceIdentity::Library {
                platform,
                guid: [
                    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x32, 0x54, 0x76, 0x98,
                    0xba, 0xdc, 0xfe
                ],
                major: 0xabcd,
                minor: 0xff,
                lcid: 0x1234_abcd,
            }
        );
        assert_eq!(parsed.source_range(), 7..end);
        assert_eq!(parsed.original_bytes(), text.as_bytes());
        assert_eq!(parsed.path(), "../untrusted/Library");
        assert_eq!(&owner[parsed.path_range()], parsed.path().as_bytes());
        assert_eq!(parsed.display_name(), Some("Display#Suffix"));
        assert_eq!(
            &owner[parsed.display_name_range().unwrap()],
            b"Display#Suffix"
        );
        assert!(!format!("{parsed:?}").contains("untrusted"));
    }
}

#[test]
fn all_project_forms_keep_paths_literal_and_empty_paths_explicit() {
    for (tag, platform, kind) in [
        (
            "A",
            ReferencePlatform::Windows,
            ProjectReferenceKind::Standalone,
        ),
        (
            "B",
            ReferencePlatform::Macintosh,
            ProjectReferenceKind::Standalone,
        ),
        (
            "C",
            ReferencePlatform::Windows,
            ProjectReferenceKind::Embedded,
        ),
        (
            "D",
            ReferencePlatform::Macintosh,
            ProjectReferenceKind::Embedded,
        ),
    ] {
        for path in [
            "",
            "../project#name",
            "C:\\outside\\project",
            "Volume:Folder:Project",
        ] {
            let text = format!("*\\{tag}{path}");
            let parsed = parse(text.as_bytes()).unwrap();
            assert_eq!(
                parsed.identity(),
                ReferenceIdentity::Project { platform, kind }
            );
            assert_eq!(parsed.path(), path);
            assert_eq!(parsed.path_range(), 3..text.len());
            assert!(parsed.display_name().is_none());
            assert!(parsed.display_name_range().is_none());
        }
    }
    let placeholder = b"*\\G{00000000-0000-0000-0000-000000000000}#0.0#0##";
    let parsed = parse(placeholder).unwrap();
    assert!(parsed.path().is_empty());
    assert_eq!(parsed.display_name(), Some(""));
    assert!(parsed.path_range().is_empty());
    assert!(parsed.display_name_range().unwrap().is_empty());
}

#[test]
fn library_identity_does_not_follow_display_names_or_path_aliases() {
    let first = format!("*\\G{GUID}#1.2#409#path#SharedName");
    let identity = parse(first.as_bytes()).unwrap().identity();
    let renamed = format!("*\\G{GUID}#0001.0002#00000409#another-path#OtherName");
    assert_eq!(identity, parse(renamed.as_bytes()).unwrap().identity());
    for (guid, version, lcid) in [
        (GUID, "2.2", "409"),
        (GUID, "1.3", "409"),
        (GUID, "1.2", "407"),
        ("{01234567-89AB-CDEF-1032-547698BADCF0}", "1.2", "409"),
    ] {
        let changed = format!("*\\G{guid}#{version}#{lcid}#path#SharedName");
        assert_ne!(identity, parse(changed.as_bytes()).unwrap().identity());
    }
}

#[test]
fn transcoded_fields_map_to_original_bytes_without_normalization() {
    let prefix = format!("*\\G{GUID}#1.2#409#");
    let mut bytes = prefix.as_bytes().to_vec();
    bytes.extend(b"C:\\caf\xe9\\lib#Display\x80");
    let parsed = parse(&bytes).unwrap();
    assert_eq!(parsed.path(), "C:\\café\\lib");
    assert_eq!(&bytes[parsed.path_range()], b"C:\\caf\xe9\\lib");
    assert_eq!(parsed.display_name(), Some("Display€"));
    assert_eq!(&bytes[parsed.display_name_range().unwrap()], b"Display\x80");
    let utf8 = format!("{prefix}./Δ𝄞#e\u{301}");
    let parsed = ReferenceIdentifier::parse(
        utf8.as_bytes(),
        0..utf8.len(),
        65001,
        TextLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    assert_eq!(&utf8.as_bytes()[parsed.path_range()], "./Δ𝄞".as_bytes());
    assert_eq!(parsed.display_name(), Some("e\u{301}"));
}

#[test]
fn malformed_identifiers_do_not_supply_partial_identity_or_disclose_payloads() {
    for text in [
        format!("*\\g{GUID}#1.0#0##"),
        format!("*\\E{GUID}#1.0#0##"),
        format!("*\\G{GUID}#10000.0#0##"),
        format!("*\\G{GUID}#.0#0##"),
        format!("*\\G{GUID}#1.0.1#0##"),
        format!("*\\G{GUID}#1.+0#0##"),
        format!("*\\G{GUID}#1.0#100000000##"),
        format!("*\\G{GUID}#1.0###"),
        format!("*\\G{GUID}#1.0#0#path"),
        format!("*\\G{GUID}#1.0#0#path#null\0name"),
        "*\\Cpath\0suffix".to_owned(),
        "*\\G{01234567_89ab-cdef-1032-547698badcfe}#1.0#0##".to_owned(),
        "*\\G{01234567-89ab-cdef-1032-547698badcfz}#1.0#0##".to_owned(),
    ] {
        let error = parse(text.as_bytes()).unwrap_err();
        assert!(
            matches!(error, ReferenceError::Invalid { offset: 0 }),
            "{text}"
        );
        assert!(!format!("{error} {error:?}").contains("suffix"));
    }
    let text = format!("*\\G{GUID}#1.0#0#path#");
    for end in 0..text.len() {
        assert!(parse(&text.as_bytes()[..end]).is_err(), "prefix {end}");
    }
}

#[test]
fn display_names_are_bounded_in_original_encoding_and_limits_remain_independent() {
    let prefix = format!("*\\G{GUID}#1.0#0##");
    for count in [255, 256] {
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.extend(std::iter::repeat_n(0xe9, count));
        assert_eq!(parse(&bytes).is_ok(), count == 255);
    }
    let bytes = b"*\\Cproject";
    let limits = TextLimits {
        input_bytes: bytes.len() - 1,
        ..TextLimits::default()
    };
    assert!(matches!(
        ReferenceIdentifier::parse(bytes, 0..bytes.len(), 1252, limits, &Cancellation::new()),
        Err(ReferenceError::Text(TextError::Limit { .. }))
    ));
    let limits = TextLimits {
        output_bytes: 1,
        ..TextLimits::default()
    };
    assert!(matches!(
        ReferenceIdentifier::parse(bytes, 0..bytes.len(), 1252, limits, &Cancellation::new()),
        Err(ReferenceError::Text(TextError::Limit { .. }))
    ));
    assert!(matches!(
        ReferenceIdentifier::parse(
            bytes,
            0..bytes.len(),
            932,
            TextLimits::default(),
            &Cancellation::new()
        ),
        Err(ReferenceError::Text(TextError::Unsupported {
            code_page: 932
        }))
    ));
    for range in [bytes.len()..0, 0..usize::MAX] {
        assert!(matches!(
            ReferenceIdentifier::parse(
                bytes,
                range,
                1252,
                TextLimits::default(),
                &Cancellation::new()
            ),
            Err(ReferenceError::Range)
        ));
    }
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        ReferenceIdentifier::parse(
            bytes,
            0..bytes.len(),
            1252,
            TextLimits::default(),
            &cancellation
        ),
        Err(ReferenceError::Text(TextError::Cancelled(_)))
    ));
}

#[test]
fn deterministic_mutations_keep_all_successful_fields_inside_the_source_window() {
    let text = format!("*\\G{GUID}#ffff.FFFF#FFFFFFFF#path#name");
    for index in 0..text.len() {
        for replacement in [0, b'#', b'.', b'0', b'F', 0xff] {
            let mut bytes = text.as_bytes().to_vec();
            bytes[index] = replacement;
            if let Ok(parsed) = parse(&bytes) {
                for range in [Some(parsed.path_range()), parsed.display_name_range()]
                    .into_iter()
                    .flatten()
                {
                    assert!(range.start <= range.end && range.end <= bytes.len());
                    let decoded = DecodedText::decode(
                        &bytes[range.clone()],
                        1252,
                        TextLimits::default(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                    if range == parsed.path_range() {
                        assert_eq!(decoded.text(), parsed.path());
                    } else {
                        assert_eq!(Some(decoded.text()), parsed.display_name());
                    }
                }
            }
        }
    }
}
