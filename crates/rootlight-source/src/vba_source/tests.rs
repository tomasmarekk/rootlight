//! Authored Office stream fixtures test storage-confined lazy source extraction.
//! Reversed physical allocation separates byte identity from directory order;
//! malformed modules coexist with readable siblings rather than hiding them.

use super::*;
use crate::compound::CompoundLimits;
use rootlight_cancel::CancellationReason;

const FREE: u32 = u32::MAX;
const END: u32 = u32::MAX - 1;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn word(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend(value.to_le_bytes());
}
fn dword(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend(value.to_le_bytes());
}
fn header(bytes: &mut Vec<u8>, id: u16, size: u32) {
    word(bytes, id);
    dword(bytes, size);
}
fn text(bytes: &mut Vec<u8>, text: &[u8]) {
    dword(bytes, u32::try_from(text.len()).unwrap());
    bytes.extend(text);
}
fn wide(name: &str) -> Vec<u8> {
    name.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn pair(bytes: &mut Vec<u8>, id: u16, name: &str) {
    word(bytes, id);
    text(bytes, name.as_bytes());
    word(bytes, 0);
    text(bytes, &wide(name));
}
fn scalar(bytes: &mut Vec<u8>, id: u16, value: u32) {
    header(bytes, id, 4);
    dword(bytes, value);
}

fn directory(modules: &[(&str, u32)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    scalar(&mut bytes, 1, 1);
    scalar(&mut bytes, 2, 0x409);
    scalar(&mut bytes, 20, 0x409);
    header(&mut bytes, 3, 2);
    word(&mut bytes, 1252);
    word(&mut bytes, 4);
    text(&mut bytes, b"Project");
    pair(&mut bytes, 5, "");
    word(&mut bytes, 6);
    text(&mut bytes, b"");
    word(&mut bytes, 0);
    text(&mut bytes, b"");
    scalar(&mut bytes, 7, 0);
    scalar(&mut bytes, 8, 0);
    scalar(&mut bytes, 9, 1);
    word(&mut bytes, 0);
    header(&mut bytes, 15, 2);
    word(&mut bytes, u16::try_from(modules.len()).unwrap());
    header(&mut bytes, 19, 2);
    word(&mut bytes, 0);
    for (name, offset) in modules {
        word(&mut bytes, 25);
        text(&mut bytes, name.as_bytes());
        pair(&mut bytes, 26, name);
        pair(&mut bytes, 28, "");
        scalar(&mut bytes, 49, *offset);
        scalar(&mut bytes, 30, 0);
        header(&mut bytes, 44, 2);
        word(&mut bytes, 0);
        header(&mut bytes, 33, 0);
        header(&mut bytes, 43, 0);
    }
    header(&mut bytes, 16, 0);
    bytes
}

fn compress_literals(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() < 3500 && !bytes.is_empty());
    let mut payload = Vec::new();
    for group in bytes.chunks(8) {
        payload.push(0);
        payload.extend(group);
    }
    let mut result = vec![1];
    word(
        &mut result,
        0xb000 | u16::try_from(payload.len() - 1).unwrap(),
    );
    result.extend(payload);
    result
}

struct Stream<'a> {
    name: &'a str,
    bytes: Vec<u8>,
    inside: bool,
}

fn entry(bytes: &mut [u8], id: usize, name: &str, kind: u8, start: u32, length: usize) {
    let at = 1024 + id * 128;
    for (i, unit) in name.encode_utf16().chain([0]).enumerate() {
        put16(bytes, at + i * 2, unit);
    }
    put16(
        bytes,
        at + 64,
        u16::try_from((name.encode_utf16().count() + 1) * 2).unwrap(),
    );
    bytes[at + 66] = kind;
    bytes[at + 67] = 1;
    for offset in [68, 72, 76] {
        put32(bytes, at + offset, FREE);
    }
    put32(bytes, at + 116, start);
    put32(bytes, at + 120, u32::try_from(length).unwrap());
}

fn compound(streams: &[Stream<'_>]) -> Vec<u8> {
    let directory_sectors = (streams.len() + 2).div_ceil(4);
    let mini_cells: usize = streams
        .iter()
        .filter(|s| s.bytes.len() < 4096)
        .map(|s| s.bytes.len().div_ceil(64))
        .sum();
    assert!(mini_cells <= 128);
    let mini_sectors = mini_cells.div_ceil(8);
    let mini_fat = 1 + directory_sectors;
    let mini_start = mini_fat + 1;
    let regular_start = mini_start + mini_sectors;
    let regular_sectors: usize = streams
        .iter()
        .filter(|s| s.bytes.len() >= 4096)
        .map(|s| s.bytes.len().div_ceil(512))
        .sum();
    let sectors = regular_start + regular_sectors;
    assert!(sectors <= 128);
    let mut bytes = vec![0; (sectors + 1) * 512];
    bytes[..8].copy_from_slice(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]);
    for (at, value) in [(24, 0x3e), (26, 3), (28, 0xfffe), (30, 9), (32, 6)] {
        put16(&mut bytes, at, value);
    }
    for (at, value) in [
        (44, 1),
        (48, 1),
        (56, 4096),
        (60, u32::try_from(mini_fat).unwrap()),
        (64, 1),
        (68, END),
    ] {
        put32(&mut bytes, at, value);
    }
    for i in 0..109 {
        put32(&mut bytes, 76 + i * 4, if i == 0 { 0 } else { FREE });
    }
    for i in 0..128 {
        put32(&mut bytes, 512 + i * 4, FREE);
        put32(&mut bytes, (mini_fat + 1) * 512 + i * 4, FREE);
    }
    put32(&mut bytes, 512, FREE - 2);
    for sector in 1..=directory_sectors {
        put32(
            &mut bytes,
            512 + sector * 4,
            if sector == directory_sectors {
                END
            } else {
                u32::try_from(sector + 1).unwrap()
            },
        );
    }
    put32(&mut bytes, 512 + mini_fat * 4, END);
    for sector in mini_start..regular_start {
        put32(
            &mut bytes,
            512 + sector * 4,
            if sector + 1 == regular_start {
                END
            } else {
                u32::try_from(sector + 1).unwrap()
            },
        );
    }
    entry(
        &mut bytes,
        0,
        "Root Entry",
        5,
        if mini_cells == 0 {
            END
        } else {
            u32::try_from(mini_start).unwrap()
        },
        mini_cells * 64,
    );
    entry(&mut bytes, 1, "VBA", 1, 0, 0);
    put32(&mut bytes, 1024 + 76, 1);
    let mut mini_used = 0;
    let mut regular_used = 0;
    for (index, stream) in streams.iter().enumerate() {
        let small = stream.bytes.len() < 4096;
        let block = if small { 64 } else { 512 };
        let count = stream.bytes.len().div_ceil(block);
        let ids: Vec<_> = (0..count)
            .map(|i| {
                if small {
                    mini_cells - 1 - mini_used - i
                } else {
                    regular_start + regular_used + count - 1 - i
                }
            })
            .collect();
        let first = ids.first().map_or(END, |id| u32::try_from(*id).unwrap());
        entry(
            &mut bytes,
            index + 2,
            stream.name,
            2,
            first,
            stream.bytes.len(),
        );
        for (i, (id, chunk)) in ids.iter().zip(stream.bytes.chunks(block)).enumerate() {
            let next = ids.get(i + 1).map_or(END, |id| u32::try_from(*id).unwrap());
            let (table, physical) = if small {
                (
                    (mini_fat + 1) * 512 + id * 4,
                    (mini_start + 1) * 512 + id * 64,
                )
            } else {
                (512 + id * 4, (id + 1) * 512)
            };
            put32(&mut bytes, table, next);
            bytes[physical..physical + chunk.len()].copy_from_slice(chunk);
        }
        if small {
            mini_used += count;
        } else {
            regular_used += count;
        }
    }
    for inside in [false, true] {
        let ids: Vec<_> = streams
            .iter()
            .enumerate()
            .filter(|(_, s)| s.inside == inside)
            .map(|(i, _)| i + 2)
            .collect();
        if let Some(first) = ids.first() {
            put32(
                &mut bytes,
                1024 + 128 + if inside { 76 } else { 72 },
                u32::try_from(*first).unwrap(),
            );
        }
        for pair in ids.windows(2) {
            put32(
                &mut bytes,
                1024 + pair[0] * 128 + 72,
                u32::try_from(pair[1]).unwrap(),
            );
        }
    }
    bytes
}

fn fixture(modules: &[(&str, u32)], streams: Vec<Stream<'_>>) -> Vec<u8> {
    let mut all = vec![Stream {
        name: "dir",
        bytes: compress_literals(&directory(modules)),
        inside: true,
    }];
    all.extend(streams);
    compound(&all)
}
fn stream<'a>(name: &'a str, body: &[u8]) -> Stream<'a> {
    Stream {
        name,
        bytes: compress_literals(body),
        inside: true,
    }
}
fn parse(bytes: &[u8]) -> CompoundFile<'_> {
    CompoundFile::parse(bytes, CompoundLimits::default(), &Cancellation::new()).unwrap()
}

#[test]
fn lazy_sources_preserve_bytes_and_composed_physical_dependencies() {
    let body = b"Attribute VB_Name = \"Module\"\r\nSub Run()\r\nEnd Sub\r\n";
    let mut encoded = vec![0x81; 71];
    encoded.extend(compress_literals(body));
    let input = fixture(
        &[("SourceΔ", 71)],
        vec![Stream {
            name: "SourceΔ",
            bytes: encoded.clone(),
            inside: true,
        }],
    );
    let compound = parse(&input);
    let cancel = Cancellation::new();
    let project = VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel).unwrap();
    assert_eq!(project.modules().len(), 1);
    assert_eq!(project.code_page(), 1252);
    assert_eq!(project.storage_id(), 1);
    assert_eq!(project.directory_id(), 2);
    assert_eq!(project.modules()[0].stream_id().unwrap(), 3);
    let source = project
        .read_module(0, ModuleReadLimits::default(), &cancel)
        .unwrap();
    assert_eq!(source.bytes(), body);
    assert_eq!(source.encoded_stream().bytes(), encoded);
    assert_eq!(source.source_offset(), 71);
    assert_eq!(source.stream_id(), 3);
    assert_eq!(source.descriptor_range(), 1024 + 3 * 128..1024 + 4 * 128);
    for stream in [project.encoded_directory(), source.encoded_stream()] {
        for part in stream.source_parts() {
            assert_eq!(
                &input[part.container_range()],
                &stream.bytes()[part.stream_range()]
            );
        }
    }
    for chunk in source.decoded().chunks() {
        let range = chunk.encoded_range();
        let stream_range = range.start + source.source_offset()..range.end + source.source_offset();
        let mut reconstructed = Vec::new();
        for part in source.encoded_stream().source_parts() {
            let mapped = part.stream_range();
            let start = mapped.start.max(stream_range.start);
            let end = mapped.end.min(stream_range.end);
            if start < end {
                let physical = part.container_range().start + start - mapped.start;
                reconstructed.extend_from_slice(&input[physical..physical + end - start]);
            }
        }
        assert_eq!(reconstructed, encoded[stream_range]);
    }
    assert!(!format!("{source:?} {project:?}").contains("VB_Name"));
}

#[test]
fn bad_binding_or_body_does_not_hide_readable_sibling_modules() {
    let input = fixture(
        &[
            ("Missing", 0),
            ("Broken", 0),
            ("Good", 0),
            ("Good", 0),
            ("Readable", 0),
        ],
        vec![
            Stream {
                name: "Broken",
                bytes: vec![255; 5],
                inside: true,
            },
            stream("Good", b"duplicate"),
            stream("Readable", b"readable"),
        ],
    );
    let compound = parse(&input);
    let cancel = Cancellation::new();
    let project = VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel).unwrap();
    assert_eq!(project.modules().len(), 5);
    assert_eq!(project.modules()[0].stream_id(), Err(BindingError::Missing));
    for ordinal in [2, 3] {
        assert_eq!(
            project.modules()[ordinal].stream_id(),
            Err(BindingError::Reused)
        );
    }
    assert!(matches!(
        project.read_module(0, ModuleReadLimits::default(), &cancel),
        Err(SourceError::Binding { .. })
    ));
    assert!(matches!(
        project.read_module(1, ModuleReadLimits::default(), &cancel),
        Err(SourceError::Compression(_))
    ));
    assert_eq!(
        project
            .read_module(4, ModuleReadLimits::default(), &cancel)
            .unwrap()
            .bytes(),
        b"readable"
    );
}

#[test]
fn bindings_are_exact_direct_children_and_never_cross_storage_boundaries() {
    let input = fixture(
        &[("Outside", 0), ("Duplicate", 0), ("dir", 0), ("absent", 0)],
        vec![
            Stream {
                name: "Outside",
                bytes: compress_literals(b"outside"),
                inside: false,
            },
            stream("Duplicate", b"first"),
            stream("Duplicate", b"second"),
        ],
    );
    let compound = parse(&input);
    let project = VbaProject::open(
        &compound,
        1,
        ProjectReadLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    let expected = [
        BindingError::Missing,
        BindingError::Ambiguous,
        BindingError::Metadata,
        BindingError::Missing,
    ];
    for (module, error) in project.modules().iter().zip(expected) {
        assert_eq!(module.stream_id(), Err(error));
    }
    let mut input = fixture(&[("Module", 0)], vec![stream("Module", b"source")]);
    input[1024 + 3 * 128 + 66] = 1;
    put32(&mut input, 1024 + 3 * 128 + 120, 0);
    put32(&mut input, 1024 + 3 * 128 + 116, 0);
    let compound = parse(&input);
    let project = VbaProject::open(
        &compound,
        1,
        ProjectReadLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    assert_eq!(
        project.modules()[0].stream_id(),
        Err(BindingError::NotStream)
    );
}

#[test]
fn preparation_and_lazy_reads_enforce_independent_limits_and_cancellation() {
    let body = b"source";
    let compressed = compress_literals(body);
    let input = fixture(&[("Source", 0)], vec![stream("Source", body)]);
    let compound = parse(&input);
    let cancel = Cancellation::new();
    let mut preparation = ProjectReadLimits::default();
    preparation.directory.modules = 0;
    assert!(matches!(
        VbaProject::open(&compound, 1, preparation, &cancel),
        Err(SourceError::Directory(DirectoryError::Limit { .. }))
    ));
    preparation = ProjectReadLimits::default();
    preparation.compression.output_bytes = 1;
    assert!(matches!(
        VbaProject::open(&compound, 1, preparation, &cancel),
        Err(SourceError::Compression(DecompressionError::Limit { .. }))
    ));
    let project = VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel).unwrap();
    let limits = ModuleReadLimits {
        stream_bytes: compressed.len() - 1,
        ..ModuleReadLimits::default()
    };
    assert!(matches!(
        project.read_module(0, limits, &cancel),
        Err(SourceError::Compound(CompoundError::Limit { .. }))
    ));
    for resource in ["input", "output", "chunks"] {
        let mut limits = ModuleReadLimits::default();
        match resource {
            "input" => limits.compression.input_bytes = compressed.len() - 1,
            "output" => limits.compression.output_bytes = body.len() - 1,
            _ => limits.compression.chunks = 0,
        }
        assert!(matches!(
            project.read_module(0, limits, &cancel),
            Err(SourceError::Compression(DecompressionError::Limit { .. }))
        ));
    }
    let invalid = ModuleReadLimits {
        stream_bytes: usize::MAX,
        ..ModuleReadLimits::default()
    };
    assert!(matches!(
        project.read_module(0, invalid, &cancel),
        Err(SourceError::Limits)
    ));
    assert!(matches!(
        project.read_module(1, ModuleReadLimits::default(), &cancel),
        Err(SourceError::Ordinal { ordinal: 1 })
    ));
    for id in [0, 2, 99] {
        assert!(matches!(
            VbaProject::open(&compound, id, ProjectReadLimits::default(), &cancel),
            Err(SourceError::Storage)
        ));
    }
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel),
        Err(SourceError::Cancelled(_))
    ));
    assert!(matches!(
        project.read_module(0, ModuleReadLimits::default(), &cancel),
        Err(SourceError::Cancelled(_))
    ));
}

#[test]
fn offsets_are_checked_before_read_and_regular_streams_remain_exact() {
    for offset in [100, u32::MAX] {
        let input = fixture(&[("Source", offset)], vec![stream("Source", b"source")]);
        let compound = parse(&input);
        let project = VbaProject::open(
            &compound,
            1,
            ProjectReadLimits::default(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(matches!(
            project.read_module(0, ModuleReadLimits::default(), &Cancellation::new()),
            Err(SourceError::Offset { ordinal: 0 })
        ));
    }
    let body: Vec<u8> = (0..4096).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let mut encoded = vec![1, 255, 0x3f];
    encoded.extend(&body);
    let input = fixture(
        &[("Source", 0)],
        vec![Stream {
            name: "Source",
            bytes: encoded,
            inside: true,
        }],
    );
    let compound = parse(&input);
    let project = VbaProject::open(
        &compound,
        1,
        ProjectReadLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    let source = project
        .read_module(0, ModuleReadLimits::default(), &Cancellation::new())
        .unwrap();
    assert_eq!(source.bytes(), body);
    for part in source.encoded_stream().source_parts() {
        assert_eq!(
            &input[part.container_range()],
            &source.encoded_stream().bytes()[part.stream_range()]
        );
    }
}

#[test]
fn missing_or_ambiguous_metadata_cannot_create_a_partial_inventory() {
    for streams in [
        vec![stream("other", b"x")],
        vec![stream("dir", b"x"), stream("dir", b"y")],
    ] {
        let input = compound(&streams);
        let compound = parse(&input);
        assert!(matches!(
            VbaProject::open(
                &compound,
                1,
                ProjectReadLimits::default(),
                &Cancellation::new()
            ),
            Err(SourceError::Metadata)
        ));
    }
    for name in ["", "name_longer_than_thirty_one_utf16_units"] {
        let input = fixture(&[(name, 0)], vec![]);
        let compound = parse(&input);
        let project = VbaProject::open(
            &compound,
            1,
            ProjectReadLimits::default(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(project.modules()[0].stream_id(), Err(BindingError::Name));
    }
}
