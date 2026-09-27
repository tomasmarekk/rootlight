//! Authored MS-OVBA directory records exercising framing and provenance.
//! Deliberately noncanonical reader-ignored words distinguish layout parsing
//! from generic TLV skipping or scans for apparent module markers in payloads.

use super::*;
use rootlight_cancel::CancellationReason;

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
fn sized(out: &mut Vec<u8>, bytes: &[u8]) {
    dword(out, u32::try_from(bytes.len()).unwrap());
    out.extend(bytes);
}
fn scalar(out: &mut Vec<u8>, id: u16, value: u32) {
    header(out, id, 4);
    dword(out, value);
}
fn utf16(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn pair(out: &mut Vec<u8>, id: u16, bytes: &[u8], unicode: &str) {
    word(out, id);
    sized(out, bytes);
    word(out, 0xeffe);
    sized(out, &utf16(unicode));
}
fn marker(out: &mut Vec<u8>, id: u16) {
    header(out, id, 0xffab_feab);
}

fn information(out: &mut Vec<u8>, optional: bool, page: u16) {
    scalar(out, 1, 3);
    if optional {
        scalar(out, 0x004a, 0xabcdef12);
    }
    scalar(out, 2, 0x0409);
    scalar(out, 0x0014, 0x0409);
    header(out, 3, 2);
    word(out, page);
    word(out, 4);
    sized(out, b"Project");
    pair(out, 5, &[0x0f, 1, 0x19, 2, 0x31, 3], "description");
    word(out, 6);
    sized(out, b"help.chm");
    word(out, 0xabcd);
    sized(out, b"help.chm");
    scalar(out, 7, 7);
    scalar(out, 8, 0);
    marker(out, 9);
    dword(out, 123);
    word(out, 45);
    if optional {
        pair(out, 0x000c, b"Build = 7", "Build = 7");
    }
}

fn control(out: &mut Vec<u8>, extended: bool) {
    marker(out, 0x002f);
    sized(out, b"twiddled");
    dword(out, 1);
    word(out, 2);
    if extended {
        pair(out, 0x0016, b"Library", "Library");
    }
    word(out, 0xfefa);
    dword(out, u32::MAX);
    sized(out, b"extended");
    out.extend([0xff; 26]);
}

fn references(out: &mut Vec<u8>) -> Vec<(Range<usize>, ReferenceKind)> {
    let mut result = Vec::new();
    let start = out.len();
    pair(out, 0x0016, b"Library", "Library");
    marker(out, 0x000d);
    sized(out, b"registered");
    dword(out, 9);
    word(out, 7);
    result.push((start..out.len(), ReferenceKind::Registered));
    let start = out.len();
    marker(out, 0x000e);
    sized(out, b"/external/project");
    sized(out, b"../project");
    dword(out, 2);
    word(out, 3);
    result.push((start..out.len(), ReferenceKind::Project));
    let start = out.len();
    control(out, false);
    result.push((start..out.len(), ReferenceKind::Control));
    let start = out.len();
    word(out, 0x0033);
    sized(out, b"original");
    control(out, true);
    result.push((start..out.len(), ReferenceKind::OriginalControl));
    result
}

fn module(out: &mut Vec<u8>, optional: bool, kind: u16, offset: u32) -> Range<usize> {
    let start = out.len();
    word(out, 0x0019);
    sized(out, b"Module");
    if optional {
        word(out, 0x0047);
        sized(out, &utf16("Module"));
    }
    pair(out, 0x001a, b"ModuleStream", "ModuleStream");
    pair(out, 0x001c, b"description", "description");
    scalar(out, 0x0031, offset);
    scalar(out, 0x001e, 9);
    header(out, 0x002c, 2);
    word(out, 0x1234);
    marker(out, kind);
    if optional {
        marker(out, 0x0025);
        marker(out, 0x0028);
    }
    marker(out, 0x002b);
    start..out.len()
}

fn module_header(out: &mut Vec<u8>, count: u16) {
    header(out, 0x000f, 2);
    word(out, count);
    header(out, 0x0013, 2);
    word(out, 7);
}

fn parse(bytes: &[u8]) -> Result<ProjectDirectory<'_>, DirectoryError> {
    ProjectDirectory::parse(bytes, DirectoryLimits::default(), &Cancellation::new())
}

#[test]
fn ordered_metadata_retains_all_reference_variants_and_exact_module_ranges() {
    let mut input = Vec::new();
    information(&mut input, true, 932);
    let info_end = input.len();
    let expected_refs = references(&mut input);
    module_header(&mut input, 2);
    let first = module(&mut input, true, 0x0021, 1234);
    let second = module(&mut input, false, 0x0022, u32::MAX);
    marker(&mut input, 0x0010);
    let directory = parse(&input).unwrap();
    assert_eq!(directory.bytes(), input);
    assert_eq!(directory.information_range(), 0..info_end);
    assert_eq!(directory.code_page(), 932);
    assert_eq!(directory.name().bytes(), b"Project");
    assert_eq!(&input[directory.name().range()], b"Project");
    assert_eq!(
        directory
            .references()
            .iter()
            .map(|r| (r.range(), r.kind()))
            .collect::<Vec<_>>(),
        expected_refs
    );
    for (index, reference) in directory.references().iter().enumerate() {
        let field = |range: &Range<usize>| {
            assert!(range.start >= reference.range().start);
            assert!(range.end <= reference.range().end);
            &input[range.clone()]
        };
        if let Some(name) = reference.name() {
            assert_eq!(index, 0);
            assert_eq!(field(&name.name_range()), b"Library");
            assert_eq!(field(&name.unicode_name_range()), utf16("Library"));
        } else {
            assert_ne!(index, 0);
        }
        match reference.payload() {
            ReferencePayload::Registered { libid } => {
                assert_eq!(field(libid), b"registered");
            }
            ReferencePayload::Project {
                absolute_libid,
                relative_libid,
                major_version,
                minor_version,
                version_range,
            } => {
                assert_eq!(field(absolute_libid), b"/external/project");
                assert_eq!(field(relative_libid), b"../project");
                assert_eq!((*major_version, *minor_version), (2, 3));
                assert_eq!(field(version_range), [2, 0, 0, 0, 3, 0]);
            }
            ReferencePayload::Control {
                original_libid,
                twiddled_libid,
                extended_name,
                extended_libid,
                original_type_library,
                cookie,
                cookie_range,
            } => {
                assert_eq!(
                    original_libid.as_ref().map(field),
                    (index == 3).then_some(b"original".as_slice())
                );
                assert_eq!(field(twiddled_libid), b"twiddled");
                assert_eq!(field(extended_libid), b"extended");
                assert_eq!(field(original_type_library), [0xff; 16]);
                assert_eq!(*cookie, u32::MAX);
                assert_eq!(field(cookie_range), cookie.to_le_bytes());
                assert_eq!(extended_name.is_some(), index == 3);
                if let Some(name) = extended_name {
                    assert_eq!(field(&name.name_range()), b"Library");
                    assert_eq!(field(&name.unicode_name_range()), utf16("Library"));
                }
            }
        }
        assert!(!format!("{reference:?}").contains("Library"));
    }
    let modules = directory.modules();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].range(), first);
    assert_eq!(modules[1].range(), second);
    assert_eq!(modules[0].name().bytes(), b"Module");
    assert_eq!(modules[0].unicode_name().unwrap().bytes(), utf16("Module"));
    assert_eq!(modules[0].kind(), ModuleKind::Procedural);
    assert!(modules[0].is_private() && modules[0].is_read_only());
    assert_eq!(modules[1].kind(), ModuleKind::Object);
    assert!(!modules[1].is_private() && !modules[1].is_read_only());
    assert!(modules[1].unicode_name().is_none());
    assert_eq!(modules[0].text_offset(), 1234);
    assert_eq!(modules[1].text_offset(), u32::MAX);
    for module in modules {
        for text in [
            module.name(),
            module.stream_name(),
            module.unicode_stream_name(),
        ] {
            assert_eq!(input.get(text.range()).unwrap(), text.bytes());
            assert!(
                text.range().start >= module.range().start
                    && text.range().end <= module.range().end
            );
        }
        assert_eq!(
            input.get(module.text_offset_range()).unwrap(),
            module.text_offset().to_le_bytes()
        );
    }
    assert!(!format!("{directory:?}").contains("ModuleStream"));
    assert!(!format!("{:?}", modules[0]).contains("description"));
}

#[test]
fn reference_names_and_binary_identity_remain_distinct_and_source_bound() {
    let mut input = Vec::new();
    information(&mut input, false, 932);
    pair(&mut input, 0x0016, &[0x81, 0x40], "Δ𝄞");
    let original_start = input.len();
    word(&mut input, 0x0033);
    sized(&mut input, &[0x0f, 1, 0x19, 2]);
    control(&mut input, true);
    let control_end = input.len();
    let guid: Vec<u8> = (0..16).collect();
    input[control_end - 20..control_end - 4].copy_from_slice(&guid);
    input[control_end - 4..control_end].copy_from_slice(&0x1234_abcd_u32.to_le_bytes());
    pair(&mut input, 0x0016, b"", "");
    marker(&mut input, 0x000d);
    sized(&mut input, b"");
    input.extend([0; 6]);
    module_header(&mut input, 0);
    marker(&mut input, 0x0010);

    let directory = parse(&input).unwrap();
    let first = &directory.references()[0];
    let name = first.name().unwrap();
    assert_eq!(&input[name.name_range()], [0x81, 0x40]);
    assert_eq!(&input[name.unicode_name_range()], utf16("Δ𝄞"));
    let ReferencePayload::Control {
        original_libid: Some(original),
        extended_name: Some(extended_name),
        original_type_library,
        cookie,
        cookie_range,
        ..
    } = first.payload()
    else {
        panic!("expected original control metadata")
    };
    assert_eq!(*original, original_start + 6..original_start + 10);
    assert_eq!(&input[original.clone()], [0x0f, 1, 0x19, 2]);
    assert_eq!(&input[extended_name.name_range()], b"Library");
    assert_eq!(&input[original_type_library.clone()], guid);
    assert_eq!(*cookie, 0x1234_abcd);
    assert_eq!(&input[cookie_range.clone()], cookie.to_le_bytes());
    let empty = &directory.references()[1];
    assert!(empty.name().unwrap().name_range().is_empty());
    assert!(empty.name().unwrap().unicode_name_range().is_empty());
    let ReferencePayload::Registered { libid } = empty.payload() else {
        panic!("expected registered reference")
    };
    assert!(libid.is_empty());
    assert!(libid.start >= empty.range().start && libid.end <= empty.range().end);
}

#[test]
fn optional_records_and_empty_project_do_not_require_a_default_code_page() {
    for optional in [false, true] {
        for page in [0, 1252, 65001, u16::MAX] {
            let mut input = Vec::new();
            information(&mut input, optional, page);
            module_header(&mut input, 0);
            marker(&mut input, 0x0010);
            let result = parse(&input).unwrap();
            assert_eq!(result.code_page(), page);
            assert!(result.modules().is_empty() && result.references().is_empty());
        }
    }
}

#[test]
fn unicode_is_lossless_but_does_not_claim_mbcs_equivalence_or_stream_identity() {
    let mut input = Vec::new();
    information(&mut input, false, 932);
    module_header(&mut input, 1);
    word(&mut input, 0x0019);
    sized(&mut input, &[0x81, 0x40]);
    word(&mut input, 0x0047);
    sized(&mut input, &utf16("Δ𝄞"));
    pair(&mut input, 0x001a, &[0x81, 0x41], "StreamΔ𝄞");
    pair(&mut input, 0x001c, b"", "");
    scalar(&mut input, 0x0031, 0);
    scalar(&mut input, 0x001e, 0);
    header(&mut input, 0x002c, 2);
    word(&mut input, 0);
    marker(&mut input, 0x0022);
    marker(&mut input, 0x002b);
    marker(&mut input, 0x0010);
    let result = parse(&input).unwrap();
    let module = &result.modules()[0];
    assert_eq!(module.name().bytes(), [0x81, 0x40]);
    assert_eq!(module.unicode_name().unwrap().bytes(), utf16("Δ𝄞"));
    assert_eq!(module.unicode_stream_name().bytes(), utf16("StreamΔ𝄞"));
}

fn fixture() -> Vec<u8> {
    let mut input = Vec::new();
    information(&mut input, true, 1252);
    references(&mut input);
    module_header(&mut input, 2);
    module(&mut input, true, 0x0021, 64);
    module(&mut input, false, 0x0022, 128);
    marker(&mut input, 0x0010);
    input
}

#[test]
fn all_truncations_trailing_bytes_and_unknown_records_fail_closed() {
    let input = fixture();
    for end in 0..input.len() {
        assert!(parse(&input[..end]).is_err(), "prefix {end}");
    }
    let mut trailing = input.clone();
    trailing.push(0);
    assert!(matches!(
        parse(&trailing),
        Err(DirectoryError::Invalid { .. })
    ));
    let mut unknown = input.clone();
    unknown[..2].copy_from_slice(&0x7654u16.to_le_bytes());
    assert!(matches!(
        parse(&unknown),
        Err(DirectoryError::Record {
            offset: 0,
            id: 0x7654
        })
    ));
    let mut info = Vec::new();
    information(&mut info, false, 1252);
    let reference_start = info.len();
    marker(&mut info, 0x0019);
    module_header(&mut info, 0);
    marker(&mut info, 0x0010);
    assert!(
        matches!(parse(&info), Err(DirectoryError::Record { offset, id: 0x0019 }) if offset == reference_start)
    );
}

#[test]
fn lengths_counts_record_order_and_text_encodings_are_checked() {
    let mut input = fixture();
    let parsed = parse(&input).unwrap();
    let module_start = parsed.modules()[0].range().start;
    let stream = parsed.modules()[0].unicode_stream_name().range();
    let name = parsed.modules()[0].name().range();
    let second_start = parsed.modules()[1].range().start;
    let first_end = parsed.modules()[0].range().end;
    let mut changes: Vec<(usize, Vec<u8>)> = vec![
        (2, u32::MAX.to_le_bytes().to_vec()),
        (module_start + 2, u32::MAX.to_le_bytes().to_vec()),
        (stream.start - 4, 3u32.to_le_bytes().to_vec()),
        (stream.start, 0xd800u16.to_le_bytes().to_vec()),
        (stream.start, 0u16.to_le_bytes().to_vec()),
        (name.start, vec![0]),
        (second_start, 0x0010u16.to_le_bytes().to_vec()),
        (first_end - 6, 0x0025u16.to_le_bytes().to_vec()),
    ];
    let modules_header = module_start - 16;
    changes.push((modules_header + 6, 1u16.to_le_bytes().to_vec()));
    changes.push((modules_header + 6, 3u16.to_le_bytes().to_vec()));
    for (offset, bytes) in changes {
        let original = input[offset..offset + bytes.len()].to_vec();
        input[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(parse(&input).is_err(), "corruption at {offset}");
        input[offset..offset + bytes.len()].copy_from_slice(&original);
    }
}

#[test]
fn input_reference_module_and_cancellation_budgets_are_independent() {
    let input = fixture();
    let cancellation = Cancellation::new();
    for (limits, resource) in [
        (
            DirectoryLimits {
                input_bytes: input.len() - 1,
                ..DirectoryLimits::default()
            },
            "input bytes",
        ),
        (
            DirectoryLimits {
                modules: 1,
                ..DirectoryLimits::default()
            },
            "modules",
        ),
        (
            DirectoryLimits {
                references: 3,
                ..DirectoryLimits::default()
            },
            "references",
        ),
    ] {
        assert!(
            matches!(ProjectDirectory::parse(&input, limits, &cancellation),
            Err(DirectoryError::Limit { resource: actual, .. }) if actual == resource)
        );
    }
    for limits in [
        DirectoryLimits {
            input_bytes: usize::MAX,
            ..DirectoryLimits::default()
        },
        DirectoryLimits {
            modules: 65536,
            ..DirectoryLimits::default()
        },
        DirectoryLimits {
            references: 65536,
            ..DirectoryLimits::default()
        },
    ] {
        assert!(matches!(
            ProjectDirectory::parse(&input, limits, &cancellation),
            Err(DirectoryError::Limits)
        ));
    }
    let exact = DirectoryLimits {
        input_bytes: input.len(),
        modules: 2,
        references: 4,
    };
    assert!(ProjectDirectory::parse(&input, exact, &cancellation).is_ok());
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        ProjectDirectory::parse(&input, exact, &cancellation),
        Err(DirectoryError::Cancelled(_))
    ));
}

#[test]
fn deterministic_mutations_never_escape_input_or_record_budgets() {
    let input = fixture();
    for position in 0..input.len() {
        let mut changed = input.clone();
        changed[position] ^= 0xa5;
        if let Ok(parsed) = parse(&changed) {
            assert_eq!(parsed.bytes(), changed);
            assert_eq!(&changed[parsed.name().range()], parsed.name().bytes());
            assert!(parsed.name().range().end <= parsed.information_range().end);
            assert!(parsed.modules().len() <= 4096 && parsed.references().len() <= 4096);
            let mut last = parsed.information_range().end;
            for range in parsed
                .references()
                .iter()
                .map(ReferenceRecord::range)
                .chain(parsed.modules().iter().map(ModuleRecord::range))
            {
                assert!(range.start >= last && range.end <= input.len() && range.start < range.end);
                last = range.end;
            }
        }
    }
}
