//! Authored nested containers for generation lifecycle tests.
//! Reversed CFB allocation and relocated package parts make identity independent
//! of physical offsets, declaration order and conventional Office filenames.

const VBA_TYPE: &str = "application/vnd.ms-office.vbaProject";
const VBA_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const OFFICE_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const RELS_TYPE: &str = "application/vnd.openxmlformats-package.relationships+xml";

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

fn directory_values(
    modules: &[(&str, u32, bool)],
    code_page: u16,
    name: &str,
    version: (u32, u16),
) -> Vec<u8> {
    let mut bytes = Vec::new();
    scalar(&mut bytes, 1, 1);
    scalar(&mut bytes, 2, 0x409);
    scalar(&mut bytes, 20, 0x409);
    header(&mut bytes, 3, 2);
    word(&mut bytes, code_page);
    word(&mut bytes, 4);
    text(&mut bytes, name.as_bytes());
    pair(&mut bytes, 5, "");
    word(&mut bytes, 6);
    text(&mut bytes, b"");
    word(&mut bytes, 0);
    text(&mut bytes, b"");
    scalar(&mut bytes, 7, 0);
    scalar(&mut bytes, 8, 0);
    scalar(&mut bytes, 9, version.0);
    word(&mut bytes, version.1);
    header(&mut bytes, 15, 2);
    word(&mut bytes, u16::try_from(modules.len()).unwrap());
    header(&mut bytes, 19, 2);
    word(&mut bytes, 0);
    for (name, offset, class) in modules {
        word(&mut bytes, 25);
        text(&mut bytes, name.as_bytes());
        pair(&mut bytes, 26, name);
        pair(&mut bytes, 28, "");
        scalar(&mut bytes, 49, *offset);
        scalar(&mut bytes, 30, 0);
        header(&mut bytes, 44, 2);
        word(&mut bytes, 0);
        header(&mut bytes, if *class { 34 } else { 33 }, 0);
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

fn compress_module_source(bytes: &[u8]) -> Vec<u8> {
    assert!(!bytes.is_empty());
    let chunks = bytes.chunks_exact(4096);
    let tail = chunks.remainder();
    // Short raw chunks are invalid. Literal-only tails deliberately retain the
    // small fixture contract instead of encoding a payload beyond its header.
    assert!(
        tail.len() < 3500,
        "authored final source chunk must be below 3500 bytes"
    );
    if bytes.len() < 4096 {
        return compress_literals(bytes);
    }
    let mut result = vec![1];
    for chunk in chunks {
        word(&mut result, 0x3fff);
        result.extend_from_slice(chunk);
    }
    if !tail.is_empty() {
        result.extend_from_slice(&compress_literals(tail)[1..]);
    }
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

fn stream<'a>(name: &'a str, body: &[u8]) -> Stream<'a> {
    Stream {
        name,
        bytes: compress_module_source(body),
        inside: true,
    }
}
fn crc(bytes: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                (value >> 1) ^ 0xedb8_8320
            };
        }
    }
    !value
}
fn number(bytes: &mut [u8], offset: usize, width: usize, value: u64) {
    bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
}
fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, payload) in entries {
        let offset = bytes.len();
        let mut local = vec![0; 30];
        let mut record = vec![0; 46];
        let size = u64::try_from(payload.len()).unwrap();
        let name_len = u64::try_from(name.len()).unwrap();
        let checksum = u64::from(crc(payload));
        number(&mut local, 0, 4, 0x0403_4b50);
        number(&mut local, 4, 2, 20);
        number(&mut local, 14, 4, checksum);
        number(&mut local, 18, 4, size);
        number(&mut local, 22, 4, size);
        number(&mut local, 26, 2, name_len);
        bytes.extend_from_slice(&local);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(payload);
        number(&mut record, 0, 4, 0x0201_4b50);
        number(&mut record, 6, 2, 20);
        number(&mut record, 16, 4, checksum);
        number(&mut record, 20, 4, size);
        number(&mut record, 24, 4, size);
        number(&mut record, 28, 2, name_len);
        number(&mut record, 42, 4, u64::try_from(offset).unwrap());
        central.extend_from_slice(&record);
        central.extend_from_slice(name.as_bytes());
    }
    let start = bytes.len();
    bytes.extend_from_slice(&central);
    let mut end = vec![0; 22];
    number(&mut end, 0, 4, 0x0605_4b50);
    number(&mut end, 8, 2, u64::try_from(entries.len()).unwrap());
    number(&mut end, 10, 2, u64::try_from(entries.len()).unwrap());
    number(&mut end, 12, 4, u64::try_from(central.len()).unwrap());
    number(&mut end, 16, 4, u64::try_from(start).unwrap());
    bytes.extend_from_slice(&end);
    bytes
}
fn relationships(body: &str) -> Vec<u8> {
    format!("<Relationships xmlns='http://schemas.openxmlformats.org/package/2006/relationships'>{body}</Relationships>").into_bytes()
}
fn relation(kind: &str, target: &str, extra: &str) -> String {
    format!("<Relationship Id='r1' Type='{kind}' Target='{target}' {extra}/>")
}
fn package_entries() -> Vec<(String, Vec<u8>)> {
    vec![
        ("[Content_Types].xml".into(), format!("<Types xmlns='http://schemas.openxmlformats.org/package/2006/content-types'><Default Extension='xml' ContentType='application/xml'/><Default Extension='rels' ContentType='{RELS_TYPE}'/><Override PartName='/payload/project.dat' ContentType='{VBA_TYPE}'/></Types>").into_bytes()),
        ("_rels/.rels".into(), relationships(&relation(OFFICE_REL, "documents/main.xml", ""))),
        ("documents/main.xml".into(), b"main document bytes".to_vec()),
        ("documents/_rels/main.xml.rels".into(), relationships(&relation(VBA_REL, "../payload/project.dat", ""))),
        ("payload/project.dat".into(), b"project bytes for a subsequent compound reader".to_vec()),
    ]
}

pub(super) fn book(modules: &[(&str, &[u8])]) -> Vec<u8> {
    typed_book(
        &modules
            .iter()
            .map(|(name, body)| (*name, *body, false))
            .collect::<Vec<_>>(),
    )
}

pub(super) fn typed_book(modules: &[(&str, &[u8], bool)]) -> Vec<u8> {
    book_with_project_value(modules, "\"Project\"")
}

pub(super) fn book_with_project_value(modules: &[(&str, &[u8], bool)], value: &str) -> Vec<u8> {
    use rootlight_source::vba_project::ProjectModuleKind;
    native_book(
        &modules
            .iter()
            .map(|(name, body, class)| {
                (
                    *name,
                    *body,
                    if *class {
                        ProjectModuleKind::Class
                    } else {
                        ProjectModuleKind::Procedural
                    },
                )
            })
            .collect::<Vec<_>>(),
        value,
    )
}

// These authored source fixtures deliberately omit host/type-library and
// designer-generated definitions. Tests must require explicit coverage gaps.
pub(super) fn native_book(
    modules: &[(
        &str,
        &[u8],
        rootlight_source::vba_project::ProjectModuleKind,
    )],
    value: &str,
) -> Vec<u8> {
    native_book_with_references(modules, value, &[])
}

pub(super) fn native_book_with_references(
    modules: &[(
        &str,
        &[u8],
        rootlight_source::vba_project::ProjectModuleKind,
    )],
    value: &str,
    references: &[u8],
) -> Vec<u8> {
    native_book_with_directory_values(modules, value, "Project", (1, 0), references)
}

pub(super) fn native_book_with_directory_values(
    modules: &[(
        &str,
        &[u8],
        rootlight_source::vba_project::ProjectModuleKind,
    )],
    value: &str,
    directory_name: &str,
    version: (u32, u16),
    references: &[u8],
) -> Vec<u8> {
    use rootlight_source::vba_project::ProjectModuleKind;
    let declarations = modules
        .iter()
        .map(|(name, _, kind)| (*name, 0, *kind != ProjectModuleKind::Procedural))
        .collect::<Vec<_>>();
    let mut streams = modules
        .iter()
        .map(|(name, body, _)| stream(name, body))
        .collect::<Vec<_>>();
    streams.push(Stream {
        name: "PROJECT",
        bytes: format!("ID=\"{{01234567-89AB-CDEF-0123-456789ABCDEF}}\"\r\n{}Name={value}\r\nHelpContextID=\"0\"\r\nCMG=\"00\"\r\nDPB=\"00\"\r\nGC=\"00\"\r\n\r\n", modules
            .iter()
            .map(|(name, _, kind)| match kind {
                ProjectModuleKind::Class => format!("Class={name}\r\n"),
                ProjectModuleKind::Procedural => format!("Module={name}\r\n"),
                ProjectModuleKind::Document { automation_version } => format!("Document={name}/&H{automation_version:08X}\r\n"),
                ProjectModuleKind::Designer => format!("BaseClass={name}\r\n"),
            })
            .collect::<String>())
            .into_bytes(),
        inside: false,
    });
    streams.push(Stream {
        name: "_VBA_PROJECT",
        bytes: vec![0; 7],
        inside: true,
    });
    let mut entries = package_entries();
    let mut metadata = directory_values(&declarations, 1252, directory_name, version);
    let insertion = rootlight_source::vba_dir::ProjectDirectory::parse(
        &metadata,
        rootlight_source::vba_dir::DirectoryLimits::default(),
        &rootlight_cancel::Cancellation::new(),
    )
    .unwrap()
    .information_range()
    .end;
    metadata.splice(insertion..insertion, references.iter().copied());
    let mut all = vec![Stream {
        name: "dir",
        bytes: compress_literals(&metadata),
        inside: true,
    }];
    all.extend(streams);
    entries[4].1 = compound(&all);
    zip(&entries)
}

#[test]
fn module_source_fixture_preserves_small_literal_encodings() {
    for length in [1, 7, 8, 9, 1024, 3499] {
        let bytes: Vec<_> = (0..length)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        assert_eq!(compress_module_source(&bytes), compress_literals(&bytes));
    }
}

#[test]
fn module_source_fixture_roundtrips_raw_chunks_and_bounded_literal_tails() {
    use rootlight_source::vba_compression::{ChunkEncoding, DecompressionLimits, decompress};

    for full_chunks in [1, 2, 9] {
        for tail_bytes in [0, 1, 7, 8, 9, 1024, 3499] {
            let length = full_chunks * 4096 + tail_bytes;
            let bytes: Vec<_> = (0..length)
                .map(|index| u8::try_from(index % 251).unwrap())
                .collect();
            let encoded = compress_module_source(&bytes);
            let decoded = decompress(
                &encoded,
                DecompressionLimits::default(),
                &rootlight_cancel::Cancellation::new(),
            )
            .unwrap();
            assert_eq!(decoded.bytes(), bytes);
            assert_eq!(
                decoded.chunks().len(),
                full_chunks + usize::from(tail_bytes != 0)
            );
            for (index, chunk) in decoded.chunks().iter().take(full_chunks).enumerate() {
                assert_eq!(chunk.encoding(), ChunkEncoding::Raw);
                assert_eq!(chunk.decoded_range(), index * 4096..(index + 1) * 4096);
                assert_eq!(
                    chunk.encoded_range(),
                    1 + index * 4098..1 + (index + 1) * 4098
                );
                let start = chunk.encoded_range().start;
                assert_eq!(&encoded[start..start + 2], &0x3fffu16.to_le_bytes());
            }
            if tail_bytes != 0 {
                let tail = decoded.chunks().last().unwrap();
                assert_eq!(tail.encoding(), ChunkEncoding::Tokens);
                assert_eq!(tail.decoded_range(), full_chunks * 4096..length);
                assert_eq!(tail.encoded_range().start, 1 + full_chunks * 4098);
                assert_eq!(tail.encoded_range().end, encoded.len());
            }
        }
    }
}

#[test]
fn module_source_fixture_rejects_empty_or_oversized_literal_tails() {
    assert!(std::panic::catch_unwind(|| compress_module_source(&[])).is_err());
    for tail_bytes in [3500, 3584, 4095] {
        for full_chunks in [0, 1] {
            let bytes = vec![b'x'; full_chunks * 4096 + tail_bytes];
            assert!(std::panic::catch_unwind(|| compress_module_source(&bytes)).is_err());
        }
    }
}
