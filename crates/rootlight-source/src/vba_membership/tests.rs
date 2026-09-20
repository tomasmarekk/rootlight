//! Authored independent inventories exercise reordered, conflicting and missing
//! metadata without using module ordinals or names from the diagnostic corpus.

use super::*;
use rootlight_cancel::CancellationReason;

#[test]
fn source_name_check_preserves_pairing_issues_and_case_scope() {
    use crate::vba_header::{HeaderLimits, HeaderName};

    for (name, source, expected) in [
        (b"Source".as_slice(), "SOURCE", None),
        (
            b"Source".as_slice(),
            "Different",
            Some(MembershipIssue::SourceNameMismatch),
        ),
        (b"Caf\xe9".as_slice(), "Café", None),
        (
            b"Caf\xe9".as_slice(),
            "CAFÉ",
            Some(MembershipIssue::SourceNameMismatch),
        ),
    ] {
        let mut items = b"Class=".to_vec();
        items.extend(name);
        items.extend(b"\r\n");
        let (records, _) = run(
            1252,
            &[Module {
                name,
                wide: None,
                object: true,
            }],
            &items,
        );
        let input = format!("Attribute VB_Name = \"{source}\"\n");
        let cancel = Cancellation::new();
        let text =
            DecodedText::decode(input.as_bytes(), 65001, TextLimits::default(), &cancel).unwrap();
        let header =
            HeaderName::parse(&text, ModuleKind::Object, HeaderLimits::default(), &cancel).unwrap();
        let result = records[0].check_source_name(&header);
        if let Some(issue) = expected {
            assert_eq!(result, Err(issue));
        } else {
            assert_eq!(result, records[0].outcome());
            assert_eq!(
                result.unwrap().needs_non_ascii_case_validation(),
                !name.is_ascii()
            );
        }
        let wrong = HeaderName::parse(
            &text,
            ModuleKind::Procedural,
            HeaderLimits::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(
            records[0].check_source_name(&wrong),
            Err(MembershipIssue::SourceKindMismatch)
        );
        let (missing, _) = run(
            1252,
            &[Module {
                name,
                wide: None,
                object: true,
            }],
            b"",
        );
        assert_eq!(
            missing[0].check_source_name(&header),
            Err(MembershipIssue::Missing)
        );
    }
}

struct Module<'a> {
    name: &'a [u8],
    wide: Option<&'a str>,
    object: bool,
}

fn word(out: &mut Vec<u8>, value: u16) {
    out.extend(value.to_le_bytes());
}
fn dword(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
fn header(out: &mut Vec<u8>, id: u16, size: u32) {
    word(out, id);
    dword(out, size);
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    dword(out, u32::try_from(value.len()).unwrap());
    out.extend(value);
}
fn wide(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn scalar(out: &mut Vec<u8>, id: u16, value: u32) {
    header(out, id, 4);
    dword(out, value);
}
fn pair(out: &mut Vec<u8>, id: u16, value: &str) {
    word(out, id);
    bytes(out, value.as_bytes());
    word(out, 0);
    bytes(out, &wide(value));
}

fn directory(page: u16, modules: &[Module<'_>]) -> Vec<u8> {
    named_directory(page, b"Project", modules)
}

fn named_directory(page: u16, name: &[u8], modules: &[Module<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    scalar(&mut out, 1, 1);
    scalar(&mut out, 2, 0x409);
    scalar(&mut out, 20, 0x409);
    header(&mut out, 3, 2);
    word(&mut out, page);
    word(&mut out, 4);
    bytes(&mut out, name);
    pair(&mut out, 5, "");
    pair(&mut out, 6, "");
    scalar(&mut out, 7, 0);
    scalar(&mut out, 8, 0);
    scalar(&mut out, 9, 1);
    word(&mut out, 0);
    header(&mut out, 15, 2);
    word(&mut out, u16::try_from(modules.len()).unwrap());
    header(&mut out, 19, 2);
    word(&mut out, 0);
    for module in modules {
        word(&mut out, 25);
        bytes(&mut out, module.name);
        if let Some(name) = module.wide {
            word(&mut out, 71);
            bytes(&mut out, &wide(name));
        }
        pair(&mut out, 26, "Stream");
        pair(&mut out, 28, "");
        scalar(&mut out, 49, 0);
        scalar(&mut out, 30, 0);
        header(&mut out, 44, 2);
        word(&mut out, 0);
        header(&mut out, if module.object { 34 } else { 33 }, 0);
        header(&mut out, 43, 0);
    }
    header(&mut out, 16, 0);
    out
}

fn properties(items: &[u8]) -> Vec<u8> {
    named_properties(items, b"\"Project\"")
}

fn named_properties(items: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = b"ID=\"{01234567-89AB-CDEF-0123-456789ABCDEF}\"\r\n".to_vec();
    out.extend(items);
    out.extend(b"Name=");
    out.extend(value);
    out.extend(b"\r\nHelpContextID=\"0\"\r\nCMG=\"00\"\r\nDPB=\"00\"\r\nGC=\"00\"\r\n\r\n");
    out
}

#[test]
fn project_identity_reconciles_exact_decoded_names_and_original_ranges() {
    for (page, name, value, expected) in [
        (
            1252,
            b"Project".as_slice(),
            b"\"Project\"".as_slice(),
            "Project",
        ),
        (1252, b"Caf\xe9", b"\"Caf\xe9\"", "Café"),
        (65001, "Δelta".as_bytes(), "\"Δelta\"".as_bytes(), "Δelta"),
        (1252, b"With\"Quote", b"\"With\"\"Quote\"", "With\"Quote"),
        (1252, b"\"", b"\"\"\"\"", "\""),
        (
            1252,
            b" not-an-identifier\t",
            b"\" not-an-identifier\t\"",
            " not-an-identifier\t",
        ),
    ] {
        let d = named_directory(page, name, &[]);
        let mut p = named_properties(b"", value);
        p.extend(b"[Workspace]\r\nName=\"Injected\"\r\n");
        let cancel = Cancellation::new();
        let directory = ProjectDirectory::parse(&d, DirectoryLimits::default(), &cancel).unwrap();
        let properties = ProjectProperties::parse(&p, ProjectLimits::default(), &cancel).unwrap();
        let identity = project_identity(&directory, &properties, &cancel).unwrap();
        assert_eq!(identity.name(), Ok(expected));
        assert_eq!(&d[identity.directory_name_range()], name);
        assert_eq!(&p[identity.property_value_range()], value);
        assert!(!format!("{identity:?}").contains(&format!("{expected:?}")));
    }
}

#[test]
fn project_identity_failures_do_not_erase_module_membership() {
    for (page, name, value, expected) in [
        (
            1252,
            b"Project".as_slice(),
            b"\"PROJECT\"".as_slice(),
            ProjectIdentityIssue::Mismatch,
        ),
        (1252, b"Project", b"Project", ProjectIdentityIssue::Quoting),
        (1252, b"Project", b"\"\"", ProjectIdentityIssue::Name),
        (
            1252,
            b"Project",
            b"\"Bad\"Quote\"",
            ProjectIdentityIssue::Quoting,
        ),
        (
            1252,
            b"Project",
            b"\"Bad\x01\"",
            ProjectIdentityIssue::Quoting,
        ),
        (
            1252,
            b"Project",
            b"\"\x81\"",
            ProjectIdentityIssue::InvalidEncoding,
        ),
        (
            65001,
            b"\xff",
            b"\"Project\"",
            ProjectIdentityIssue::InvalidEncoding,
        ),
        (
            932,
            b"Project",
            b"\"Project\"",
            ProjectIdentityIssue::UnsupportedCodePage,
        ),
        (
            65001,
            "é".as_bytes(),
            "\"e\u{301}\"".as_bytes(),
            ProjectIdentityIssue::Mismatch,
        ),
    ] {
        let d = named_directory(
            page,
            name,
            &[Module {
                name: b"Source",
                wide: None,
                object: false,
            }],
        );
        let p = named_properties(b"Module=Source\r\n", value);
        let cancel = Cancellation::new();
        let directory = ProjectDirectory::parse(&d, DirectoryLimits::default(), &cancel).unwrap();
        let properties = ProjectProperties::parse(&p, ProjectLimits::default(), &cancel).unwrap();
        let identity = project_identity(&directory, &properties, &cancel).unwrap();
        assert_eq!(identity.name(), Err(expected));
        let (left, right) = reconcile(&directory, &properties, &cancel).unwrap();
        assert_eq!((left.len(), right.len()), (1, 1));
        if page != 932 {
            assert!(left[0].outcome().is_ok());
            assert!(right[0].outcome().is_ok());
        }
        assert_eq!(&d[identity.directory_name_range()], name);
        assert_eq!(&p[identity.property_value_range()], value);
    }
}

#[test]
fn project_identity_bounds_quoting_expansion_and_observes_cancellation() {
    let cancel = Cancellation::new();
    for character in [b'A', b'"', 0xe9] {
        let name = vec![character; 128];
        let mut value = vec![b'"'];
        for byte in &name {
            value.push(*byte);
            if *byte == b'"' {
                value.push(*byte);
            }
        }
        value.push(b'"');
        let d = named_directory(1252, &name, &[]);
        let p = named_properties(b"", &value);
        let directory = ProjectDirectory::parse(&d, DirectoryLimits::default(), &cancel).unwrap();
        let properties = ProjectProperties::parse(&p, ProjectLimits::default(), &cancel).unwrap();
        let identity = project_identity(&directory, &properties, &cancel).unwrap();
        assert_eq!(identity.name().unwrap().chars().count(), 128);
        let mut excess = vec![b'"'];
        excess.extend([b'A'; 129]);
        excess.push(b'"');
        let p = named_properties(b"", &excess);
        let properties = ProjectProperties::parse(&p, ProjectLimits::default(), &cancel).unwrap();
        assert_eq!(
            project_identity(&directory, &properties, &cancel)
                .unwrap()
                .name(),
            Err(ProjectIdentityIssue::Name)
        );
        let stopped = Cancellation::new();
        stopped.cancel(CancellationReason::ClientRequest);
        assert!(project_identity(&directory, &properties, &stopped).is_err());
    }
}

fn run(
    page: u16,
    modules: &[Module<'_>],
    items: &[u8],
) -> (Vec<MembershipRecord>, Vec<MembershipRecord>) {
    let d = directory(page, modules);
    let p = properties(items);
    let cancel = Cancellation::new();
    let dir = ProjectDirectory::parse(&d, DirectoryLimits::default(), &cancel).unwrap();
    let project = ProjectProperties::parse(&p, ProjectLimits::default(), &cancel).unwrap();
    let result = reconcile(&dir, &project, &cancel).unwrap();
    assert_eq!(result.0.len(), modules.len());
    assert_eq!(result.1.len(), project.modules().len());
    for (index, record) in result.0.iter().enumerate() {
        assert_eq!(&d[record.name_range()], modules[index].name);
        assert_eq!(record.range(), dir.modules()[index].range());
    }
    for (index, record) in result.1.iter().enumerate() {
        assert_eq!(&p[record.name_range()], project.modules()[index].name());
        assert_eq!(record.range(), project.modules()[index].range());
    }
    result
}

#[test]
fn reordered_subtypes_pair_by_name_with_original_ranges() {
    let (left, right) = run(
        1252,
        &[
            Module {
                name: b"Standard",
                wide: Some("Standard"),
                object: false,
            },
            Module {
                name: b"Object",
                wide: None,
                object: true,
            },
            Module {
                name: b"Doc/Part",
                wide: Some("Doc/Part"),
                object: true,
            },
            Module {
                name: b"View",
                wide: Some("View"),
                object: true,
            },
        ],
        b"Document=Doc/Part/&H1234ABCD\r\nBaseClass=View\r\nModule=STANDARD\r\nClass=object\r\n",
    );
    let expected = [
        (2, ProjectModuleKind::Procedural),
        (3, ProjectModuleKind::Class),
        (
            0,
            ProjectModuleKind::Document {
                automation_version: 0x1234abcd,
            },
        ),
        (1, ProjectModuleKind::Designer),
    ];
    for (ordinal, (counterpart, kind)) in expected.into_iter().enumerate() {
        let matched = left[ordinal].outcome().unwrap();
        assert_eq!((matched.counterpart(), matched.kind()), (counterpart, kind));
        assert!(!matched.needs_non_ascii_case_validation());
        assert_eq!(right[counterpart].outcome().unwrap().counterpart(), ordinal);
    }
    assert_eq!(left[0].name(), Some("Standard"));
    assert_eq!(right[2].name(), Some("STANDARD"));
}

#[test]
fn duplicate_claimants_never_pick_a_first_or_last_winner() {
    for (names, items) in [
        (
            vec!["Same", "same", "Other"],
            "Module=SAME\r\nModule=Other\r\n",
        ),
        (
            vec!["Same", "Other"],
            "Module=Same\r\nClass=SAME\r\nModule=Other\r\n",
        ),
        (
            vec!["Same", "same", "Other"],
            "Module=Same\r\nModule=SAME\r\nModule=Other\r\n",
        ),
    ] {
        let modules: Vec<_> = names
            .iter()
            .map(|name| Module {
                name: name.as_bytes(),
                wide: None,
                object: false,
            })
            .collect();
        let (left, right) = run(1252, &modules, items.as_bytes());
        for record in left.iter().chain(&right) {
            if record.name() == Some("Other") {
                assert!(record.outcome().is_ok());
            } else {
                assert_eq!(record.outcome(), Err(MembershipIssue::Ambiguous));
            }
        }
    }
}

#[test]
fn missing_invalid_unicode_and_kind_conflicts_preserve_siblings() {
    let (left, right) = run(
        1252,
        &[
            Module {
                name: b"Absent",
                wide: None,
                object: false,
            },
            Module {
                name: b"WrongType",
                wide: None,
                object: true,
            },
            Module {
                name: b"Spelling",
                wide: Some("SPELLING"),
                object: false,
            },
            Module {
                name: b"Good",
                wide: Some("Good"),
                object: false,
            },
        ],
        b"Module=Extra\r\nModule=WrongType\r\nModule=Spelling\r\nModule=Good\r\n",
    );
    assert_eq!(left[0].outcome(), Err(MembershipIssue::Missing));
    assert_eq!(right[0].outcome(), Err(MembershipIssue::Missing));
    assert_eq!(left[1].outcome(), Err(MembershipIssue::KindMismatch));
    assert_eq!(right[1].outcome(), Err(MembershipIssue::KindMismatch));
    assert_eq!(left[2].outcome(), Err(MembershipIssue::UnicodeNameMismatch));
    assert_eq!(right[2].outcome(), Err(MembershipIssue::CounterpartInvalid));
    assert!(left[3].outcome().is_ok() && right[3].outcome().is_ok());
}

#[test]
fn non_ascii_spelling_is_lossless_but_not_locale_equivalence() {
    let (left, right) = run(
        1252,
        &[
            Module {
                name: b"caf\xe9",
                wide: Some("café"),
                object: false,
            },
            Module {
                name: b"\xe9",
                wide: Some("é"),
                object: false,
            },
        ],
        b"Module=CAF\xe9\r\nModule=\xc9\r\n",
    );
    assert_eq!(left[0].name(), Some("café"));
    assert!(left[0].outcome().unwrap().needs_non_ascii_case_validation());
    assert!(
        right[0]
            .outcome()
            .unwrap()
            .needs_non_ascii_case_validation()
    );
    assert_eq!(left[1].outcome(), Err(MembershipIssue::Missing));
    assert_eq!(right[1].outcome(), Err(MembershipIssue::Missing));
    assert!(!format!("{:?}", left[0]).contains("café"));
}

#[test]
fn name_limits_count_scalars_and_encoding_failures_are_explicit() {
    for length in [31, 32] {
        let name = "🦀".repeat(length);
        let items = format!("Module={name}\r\n");
        let (left, right) = run(
            65001,
            &[Module {
                name: name.as_bytes(),
                wide: Some(&name),
                object: false,
            }],
            items.as_bytes(),
        );
        if length == 31 {
            assert!(left[0].outcome().unwrap().needs_non_ascii_case_validation());
        } else {
            assert_eq!(left[0].outcome(), Err(MembershipIssue::Name));
            assert_eq!(right[0].outcome(), Err(MembershipIssue::Name));
        }
    }
    for (page, name, expected) in [
        (1252, b"\x81".as_slice(), MembershipIssue::InvalidEncoding),
        (65001, b"\xff", MembershipIssue::InvalidEncoding),
        (0, b"Plain", MembershipIssue::UnsupportedCodePage),
    ] {
        let mut items = b"Module=".to_vec();
        items.extend(name);
        items.extend(b"\r\n");
        let (left, right) = run(
            page,
            &[Module {
                name,
                wide: None,
                object: false,
            }],
            &items,
        );
        assert_eq!(left[0].outcome(), Err(expected));
        assert_eq!(right[0].outcome(), Err(expected));
    }
}

#[test]
fn cancelled_empty_inventory_does_not_return_success() {
    let d = directory(1252, &[]);
    let p = properties(b"");
    let cancel = Cancellation::new();
    let dir = ProjectDirectory::parse(&d, DirectoryLimits::default(), &cancel).unwrap();
    let project = ProjectProperties::parse(&p, ProjectLimits::default(), &cancel).unwrap();
    assert!(reconcile(&dir, &project, &cancel).is_ok());
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(reconcile(&dir, &project, &cancel).is_err());
}
