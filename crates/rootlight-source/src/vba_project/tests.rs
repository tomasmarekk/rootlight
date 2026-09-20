//! Authored PROJECT records exercise framing, subtype identity and provenance.
//! Opaque codepage bytes and ignored UI suffixes cannot silently become module
//! identities, and every failed prefix remains a failed complete inventory.

use super::*;
use rootlight_cancel::CancellationReason;

const ID: &str = "ID=\"{01234567-89AB-CDEF-0123-456789ABCDEF}\"\r\n";
const TAIL: &str =
    "Name=\"Project\"\r\nHelpContextID=\"0\"\r\nCMG=\"00\"\r\nDPB=\"00\"\r\nGC=\"00\"\r\n\r\n";

fn input(items: &str) -> Vec<u8> {
    format!("{ID}{items}{TAIL}").into_bytes()
}

fn parse(bytes: &[u8]) -> Result<ProjectProperties<'_>, ProjectError> {
    ProjectProperties::parse(bytes, ProjectLimits::default(), &Cancellation::new())
}

#[test]
fn module_kinds_and_original_ranges_survive_both_newline_pairs() {
    let fixture = input(
        "Module=Standard\r\nClass=ClassModule\r\nDocument=Doc/Part/&H89AbCDEF\r\nPackage={01234567-89AB-CDEF-0123-456789ABCDEF}\r\nBaseClass=Form\r\n",
    );
    for bytes in [
        fixture.clone(),
        String::from_utf8(fixture)
            .unwrap()
            .replace("\r\n", "\n\r")
            .into_bytes(),
    ] {
        let project = parse(&bytes).unwrap();
        assert_eq!(project.range(), 0..bytes.len());
        assert_eq!(project.remainder_range(), bytes.len()..bytes.len());
        assert_eq!(project.name_value(), b"\"Project\"");
        assert_eq!(&bytes[project.name_value_range()], project.name_value());
        assert_eq!(project.package_ranges().len(), 1);
        let expected = [
            (b"Standard".as_slice(), ProjectModuleKind::Procedural),
            (b"ClassModule".as_slice(), ProjectModuleKind::Class),
            (
                b"Doc/Part".as_slice(),
                ProjectModuleKind::Document {
                    automation_version: 0x89ab_cdef,
                },
            ),
            (b"Form".as_slice(), ProjectModuleKind::Designer),
        ];
        assert_eq!(project.modules().len(), expected.len());
        for (module, (name, kind)) in project.modules().iter().zip(expected) {
            assert_eq!(module.name(), name);
            assert_eq!(module.kind(), kind);
            assert_eq!(&bytes[module.name_range()], name);
            assert!(module.range().start < module.name_range().start);
            assert!(module.name_range().end < module.range().end);
        }
        assert!(!format!("{project:?} {:?}", project.modules()).contains("ClassModule"));
    }
}

#[test]
fn opaque_names_duplicates_and_suffixes_are_not_normalized_or_promoted() {
    let mut bytes = input("module=Alpha\r\nClass=ALPHA\r\n");
    let end = bytes.len();
    bytes.extend(b"[Host Extender Info]\r\n\r\n[Workspace]\r\nModule=Fake\r\n");
    let project = parse(&bytes).unwrap();
    assert_eq!(project.modules().len(), 2);
    assert_eq!(project.modules()[0].name(), b"Alpha");
    assert_eq!(project.modules()[1].name(), b"ALPHA");
    assert_eq!(project.range(), 0..end);
    assert_eq!(project.remainder_range(), end..bytes.len());
    let mut bytes = ID.as_bytes().to_vec();
    bytes.extend(b"Module=\x82\xa0\xe9/Item=Value\r\n");
    bytes.extend(TAIL.as_bytes());
    assert_eq!(
        parse(&bytes).unwrap().modules()[0].name(),
        b"\x82\xa0\xe9/Item=Value"
    );
    assert!(parse(&input("")).unwrap().modules().is_empty());
}

#[test]
fn all_optional_properties_are_ordered_and_payloads_remain_opaque() {
    let bytes = format!(
        "{ID}HelpFile=\"x\"\r\nExeName32=\"ignored\"\r\nName=\"Project\"\r\nHelpContextID=\"0\"\r\nDescription=\"Module=NotADeclaration\"\r\nVersionCompatible32=\"1\"\r\nCMG=\"opaque\"\r\nDPB=\"opaque\"\r\nGC=\"opaque\"\r\n\r\n"
    );
    assert!(parse(bytes.as_bytes()).unwrap().modules().is_empty());
    for bad in [
        bytes.replace("HelpFile=\"x\"\r\n", "Unknown=\"x\"\r\n"),
        bytes.replace("Name=\"Project\"", "Class=Injected"),
        bytes.replace("HelpContextID=\"0\"\r\n", ""),
        bytes.replace("CMG=\"opaque\"", "DPB=\"opaque\""),
        bytes.replace(
            "GC=\"opaque\"\r\n\r\n",
            "GC=\"opaque\"\r\nModule=Late\r\n\r\n",
        ),
    ] {
        assert!(parse(bad.as_bytes()).is_err());
    }
}

#[test]
fn truncations_invalid_declarations_and_framing_fail_without_partial_success() {
    let bytes = input("Module=First\r\nClass=Second\r\n");
    for end in 0..bytes.len() {
        assert!(parse(&bytes[..end]).is_err(), "prefix {end}");
    }
    for item in [
        "Module=\r\n",
        "Document=Doc/&H0\r\n",
        "Document=Doc/&H0000000G\r\n",
        "Document=Doc/0x00000000\r\n",
        "Package={bad}\r\n",
        "Module=A\0B\r\n",
        "Module=A\n",
        "Module=A\rX\r\n",
    ] {
        assert!(parse(&input(item)).is_err(), "{item:?}");
    }
    assert!(parse(&bytes[1..]).is_err());
    let mut invalid = bytes;
    invalid[4] = b'!';
    assert!(parse(&invalid).is_err());
}

#[test]
fn input_line_item_limits_and_cancellation_are_independent() {
    let bytes = input("Module=One\r\nPackage={01234567-89AB-CDEF-0123-456789ABCDEF}\r\n");
    for limits in [
        ProjectLimits {
            input_bytes: bytes.len() - 1,
            line_bytes: 50,
            ..ProjectLimits::default()
        },
        ProjectLimits {
            line_bytes: 1,
            ..ProjectLimits::default()
        },
        ProjectLimits {
            items: 1,
            ..ProjectLimits::default()
        },
    ] {
        assert!(matches!(
            ProjectProperties::parse(&bytes, limits, &Cancellation::new()),
            Err(ProjectError::Limit { .. })
        ));
    }
    let limits = ProjectLimits {
        input_bytes: bytes.len(),
        line_bytes: 50,
        items: 2,
    };
    assert_eq!(
        ProjectProperties::parse(&bytes, limits, &Cancellation::new())
            .unwrap()
            .modules()
            .len(),
        1
    );
    for limits in [
        ProjectLimits {
            items: 65536,
            ..ProjectLimits::default()
        },
        ProjectLimits {
            input_bytes: usize::MAX,
            ..ProjectLimits::default()
        },
        ProjectLimits {
            line_bytes: usize::MAX,
            ..ProjectLimits::default()
        },
    ] {
        assert!(matches!(
            ProjectProperties::parse(&bytes, limits, &Cancellation::new()),
            Err(ProjectError::Limits)
        ));
    }
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        ProjectProperties::parse(&bytes, ProjectLimits::default(), &cancel),
        Err(ProjectError::Cancelled(_))
    ));
}

#[test]
fn deterministic_mutations_preserve_bounded_original_ranges() {
    let original = input("Module=Standard\r\nDocument=Doc/&H00000000\r\n");
    for index in 0..original.len() {
        for replacement in [0, b'=', b'\r', b'\n', 255] {
            let mut bytes = original.clone();
            bytes[index] = replacement;
            if let Ok(project) = parse(&bytes) {
                assert!(project.range().end <= bytes.len());
                assert_eq!(&bytes[project.name_value_range()], project.name_value());
                assert!(project.name_value_range().end <= project.range().end);
                for module in project.modules() {
                    assert_eq!(&bytes[module.name_range()], module.name());
                    assert!(module.range().end <= project.range().end);
                }
            }
        }
    }
}
