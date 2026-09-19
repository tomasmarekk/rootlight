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
    directory_with_code_page(modules, 1252)
}

fn directory_with_code_page(modules: &[(&str, u32)], code_page: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    scalar(&mut bytes, 1, 1);
    scalar(&mut bytes, 2, 0x409);
    scalar(&mut bytes, 20, 0x409);
    header(&mut bytes, 3, 2);
    word(&mut bytes, code_page);
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

fn renamed_entry(bytes: &mut [u8], id: usize, name: &str) {
    let start = 1024 + id * 128;
    bytes[start..start + 64].fill(0);
    let wide: Vec<_> = name.encode_utf16().collect();
    assert!(wide.len() <= 31);
    for (index, unit) in wide.iter().enumerate() {
        put16(bytes, start + index * 2, *unit);
    }
    put16(
        bytes,
        start + 64,
        u16::try_from((wide.len() + 1) * 2).unwrap(),
    );
}

fn ascii_case_variants(word: &str) -> Vec<String> {
    assert!(word.is_ascii() && word.len() < 8);
    (0..1usize << word.len())
        .map(|mask| {
            word.bytes()
                .enumerate()
                .map(|(index, byte)| {
                    char::from(if mask & (1 << index) == 0 {
                        byte.to_ascii_uppercase()
                    } else {
                        byte.to_ascii_lowercase()
                    })
                })
                .collect()
        })
        .collect()
}

fn rooted_fixture() -> Vec<u8> {
    fixture(
        &[("Source", 0)],
        vec![
            stream("Source", b"Attribute VB_Name = \"Source\"\r\n"),
            Stream {
                name: "PROJECT",
                bytes: b"Module=Source\r\n".to_vec(),
                inside: false,
            },
            Stream {
                name: "_VBA_PROJECT",
                bytes: vec![0xff; 7],
                inside: true,
            },
        ],
    )
}

#[test]
fn explicit_root_keeps_descriptor_evidence_and_ignores_version_values() {
    let bytes = rooted_fixture();
    let compound = parse(&bytes);
    let cancel = Cancellation::new();
    let rooted =
        root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel).unwrap();
    for (entry, id) in [
        (rooted.root(), 0),
        (rooted.storage(), 1),
        (rooted.properties(), 4),
        (rooted.version(), 5),
    ] {
        assert_eq!(entry.id(), id);
        assert_eq!(
            entry.directory_range(),
            1024 + usize::try_from(id).unwrap() * 128
                ..1024 + (usize::try_from(id).unwrap() + 1) * 128
        );
    }
    assert_eq!(rooted.version().byte_length(), 7);
    assert_eq!(
        rooted
            .project()
            .read_module(0, ModuleReadLimits::default(), &cancel)
            .unwrap()
            .bytes(),
        b"Attribute VB_Name = \"Source\"\r\n"
    );
    assert_eq!(
        rooted
            .project()
            .read_project_stream(64, &cancel)
            .unwrap()
            .bytes(),
        b"Module=Source\r\n"
    );
    assert!(!format!("{rooted:?}").contains("Module=Source"));
    for (id, name) in [(1, "vBa"), (4, "pRoJeCt"), (5, "_vBa_pRoJeCt")] {
        let mut changed = bytes.clone();
        renamed_entry(&mut changed, id, name);
        let compound = parse(&changed);
        assert!(
            root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel).is_ok()
        );
    }
}

#[test]
fn root_selection_never_falls_back_to_other_subtrees() {
    let mut bytes = rooted_fixture();
    // Reparent the existing VBA subtree under an explicitly selected host storage.
    let host = 6;
    entry(&mut bytes, host, "Host", 1, 0, 0);
    put32(&mut bytes, 1024 + 76, u32::try_from(host).unwrap());
    put32(&mut bytes, 1024 + host * 128 + 76, 1);
    let compound = parse(&bytes);
    let cancel = Cancellation::new();
    assert!(matches!(
        root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel),
        Err(root::RootError::Storage)
    ));
    let rooted =
        root::RootedProject::open(&compound, 6, ProjectReadLimits::default(), &cancel).unwrap();
    assert_eq!(rooted.root().id(), 6);
    assert_eq!(rooted.storage().parent(), Some(6));
    assert_eq!(rooted.properties().parent(), Some(6));
    for id in [3, 99] {
        assert!(matches!(
            root::RootedProject::open(&compound, id, ProjectReadLimits::default(), &cancel),
            Err(root::RootError::Root)
        ));
    }
}

#[test]
fn root_children_cannot_be_missing_ambiguous_or_wrong_kind() {
    let cancel = Cancellation::new();
    for id in [1, 4, 5] {
        let mut bytes = rooted_fixture();
        renamed_entry(&mut bytes, id, "Unrelated");
        let compound = parse(&bytes);
        assert!(
            root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel).is_err()
        );
    }
    for (name, inside) in [("vBa", false), ("project", false), ("_vba_project", true)] {
        let bytes = fixture(
            &[],
            vec![
                Stream {
                    name: "PROJECT",
                    bytes: Vec::new(),
                    inside: false,
                },
                Stream {
                    name: "_VBA_PROJECT",
                    bytes: vec![0; 7],
                    inside: true,
                },
                Stream {
                    name,
                    bytes: Vec::new(),
                    inside,
                },
            ],
        );
        let compound = parse(&bytes);
        assert!(
            root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel).is_err()
        );
    }
    for id in [4, 5] {
        let mut bytes = rooted_fixture();
        entry(
            &mut bytes,
            id,
            if id == 4 { "PROJECT" } else { "_VBA_PROJECT" },
            1,
            0,
            0,
        );
        let compound = parse(&bytes);
        assert!(
            root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel).is_err()
        );
    }
}

#[test]
fn version_presence_and_directory_limits_do_not_activate_cached_code() {
    let cancel = Cancellation::new();
    for length in [0, 1, 6, 7, 8, 4096] {
        let bytes = fixture(
            &[],
            vec![
                Stream {
                    name: "PROJECT",
                    bytes: Vec::new(),
                    inside: false,
                },
                Stream {
                    name: "_VBA_PROJECT",
                    bytes: vec![0xa5; length],
                    inside: true,
                },
            ],
        );
        let compound = parse(&bytes);
        let result = root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel);
        assert_eq!(result.is_ok(), length >= 7);
        if length < 7 {
            assert!(matches!(result, Err(root::RootError::Version)));
        }
    }
    let bytes = rooted_fixture();
    let compound = parse(&bytes);
    let mut limits = ProjectReadLimits::default();
    limits.directory.input_bytes = 1;
    assert!(matches!(
        root::RootedProject::open(&compound, 0, limits, &cancel),
        Err(root::RootError::Source(_))
    ));
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel),
        Err(root::RootError::Cancelled(_))
    ));
}

#[test]
fn version_cache_cannot_be_claimed_as_module_source() {
    let bytes = fixture(
        &[("_VBA_PROJECT", 0), ("Source", 0)],
        vec![
            stream("_VBA_PROJECT", b"cached bytes are not module code"),
            stream("Source", b"independent readable body"),
            Stream {
                name: "PROJECT",
                bytes: Vec::new(),
                inside: false,
            },
        ],
    );
    let compound = parse(&bytes);
    let cancel = Cancellation::new();
    let rooted =
        root::RootedProject::open(&compound, 0, ProjectReadLimits::default(), &cancel).unwrap();
    assert_eq!(
        rooted.project().modules()[0].stream_id(),
        Err(BindingError::Metadata)
    );
    assert!(matches!(
        rooted
            .project()
            .read_module(0, ModuleReadLimits::default(), &cancel),
        Err(SourceError::Binding {
            source: BindingError::Metadata,
            ..
        })
    ));
    assert_eq!(
        rooted
            .project()
            .read_module(1, ModuleReadLimits::default(), &cancel)
            .unwrap()
            .bytes(),
        b"independent readable body"
    );
}

#[test]
fn reserved_storage_and_stream_names_ignore_ascii_case() {
    let cancel = Cancellation::new();
    let base = fixture(
        &[("Source", 0)],
        vec![
            stream("Source", b"body"),
            Stream {
                name: "PROJECT",
                bytes: b"properties".to_vec(),
                inside: false,
            },
        ],
    );
    for (id, name) in [(1, "VBA"), (2, "dir"), (4, "PROJECT")] {
        for variant in ascii_case_variants(name) {
            let mut input = base.clone();
            renamed_entry(&mut input, id, &variant);
            let file = parse(&input);
            let project =
                VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel).unwrap();
            assert_eq!(project.directory_id(), 2);
            assert_eq!(
                project.read_project_stream(64, &cancel).unwrap().bytes(),
                b"properties"
            );
            assert_eq!(
                project
                    .read_module(0, ModuleReadLimits::default(), &cancel)
                    .unwrap()
                    .bytes(),
                b"body"
            );
        }
    }
    for invalid in ["VBAx", " VBA", "ＶBA", "VBΑ"] {
        let mut input = base.clone();
        renamed_entry(&mut input, 1, invalid);
        let file = parse(&input);
        assert!(matches!(
            VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel),
            Err(SourceError::Storage)
        ));
    }
}

#[test]
fn reserved_directory_aliases_are_ambiguous_but_storage_confined() {
    let cancel = Cancellation::new();
    for names in [["dir", "DIR"], ["DIR", "dir"]] {
        let input = compound(&[
            Stream {
                name: names[0],
                bytes: compress_literals(&directory(&[])),
                inside: true,
            },
            Stream {
                name: names[1],
                bytes: compress_literals(&directory(&[])),
                inside: true,
            },
        ]);
        let file = parse(&input);
        assert!(matches!(
            VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel),
            Err(SourceError::Metadata)
        ));
    }
    let input = compound(&[
        Stream {
            name: "DIR",
            bytes: compress_literals(&directory(&[])),
            inside: true,
        },
        Stream {
            name: "dir",
            bytes: b"outside storage".to_vec(),
            inside: false,
        },
    ]);
    let file = parse(&input);
    let project = VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel).unwrap();
    assert_eq!(project.directory_id(), 2);
    assert!(project.modules().is_empty());
}

#[test]
fn reserved_project_aliases_cannot_hide_or_cross_module_storage() {
    let cancel = Cancellation::new();
    for names in [["PROJECT", "project"], ["project", "PROJECT"]] {
        let input = fixture(
            &[("Source", 0)],
            vec![
                stream("Source", b"body"),
                Stream {
                    name: names[0],
                    bytes: b"first".to_vec(),
                    inside: false,
                },
                Stream {
                    name: names[1],
                    bytes: b"second".to_vec(),
                    inside: false,
                },
            ],
        );
        let file = parse(&input);
        let project = VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel).unwrap();
        assert!(matches!(
            project.read_project_stream(64, &cancel),
            Err(SourceError::ProjectMetadata)
        ));
        assert_eq!(
            project
                .read_module(0, ModuleReadLimits::default(), &cancel)
                .unwrap()
                .bytes(),
            b"body"
        );
    }
    for outside in ["pRoJeCt", "PROJECT.extra", "ＰROJECT"] {
        let input = fixture(
            &[("Source", 0)],
            vec![
                stream("Source", b"body"),
                Stream {
                    name: "PROJECT",
                    bytes: b"inside".to_vec(),
                    inside: true,
                },
                Stream {
                    name: outside,
                    bytes: b"outside".to_vec(),
                    inside: false,
                },
            ],
        );
        let file = parse(&input);
        let project = VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel).unwrap();
        if outside == "pRoJeCt" {
            assert_eq!(
                project.read_project_stream(64, &cancel).unwrap().bytes(),
                b"outside"
            );
        } else {
            assert!(matches!(
                project.read_project_stream(64, &cancel),
                Err(SourceError::ProjectMetadata)
            ));
        }
    }
}

#[test]
fn module_text_uses_its_own_declared_page_and_preserves_raw_source() {
    use crate::vba_text::{TextError, TextLimits};

    for page in [1252, 65001, 932] {
        let body = b"' caf\xe9 \x80\r\n";
        let input = compound(&[
            Stream {
                name: "dir",
                bytes: compress_literals(&directory_with_code_page(&[("Source", 0)], page)),
                inside: true,
            },
            stream("Source", body),
        ]);
        let file = parse(&input);
        let cancel = Cancellation::new();
        let project = VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel).unwrap();
        let module = project
            .read_module(0, ModuleReadLimits::default(), &cancel)
            .unwrap();
        match page {
            1252 => {
                let text = module.decode_text(TextLimits::default(), &cancel).unwrap();
                assert_eq!(text.text(), "' café €\r\n");
                assert_eq!(text.original_range(8..11), Some(7..8));
                assert_eq!(text.code_page(), page);
            }
            65001 => assert!(matches!(
                module.decode_text(TextLimits::default(), &cancel),
                Err(TextError::Invalid { offset: 5, .. })
            )),
            _ => assert!(matches!(
                module.decode_text(TextLimits::default(), &cancel),
                Err(TextError::Unsupported { code_page: 932 })
            )),
        }
        assert_eq!(module.bytes(), body);
    }
}

#[test]
fn membership_reads_own_project_and_preserves_lazy_source_access() {
    use crate::vba_membership::{
        MembershipError, MembershipIssue, MembershipLimits, ProjectMembership,
    };

    let properties = b"ID=\"{01234567-89AB-CDEF-0123-456789ABCDEF}\"\r\nModule=SECOND\r\nClass=First\r\nName=\"Project\"\r\nHelpContextID=\"0\"\r\nCMG=\"00\"\r\nDPB=\"00\"\r\nGC=\"00\"\r\n\r\n";
    let input = fixture(
        &[("First", 0), ("Second", 0)],
        vec![
            stream("First", b"first body"),
            stream("Second", b"second body"),
            Stream {
                name: "PROJECT",
                bytes: properties.to_vec(),
                inside: false,
            },
        ],
    );
    let file = parse(&input);
    let cancel = Cancellation::new();
    let project = VbaProject::open(&file, 1, ProjectReadLimits::default(), &cancel).unwrap();
    let membership =
        ProjectMembership::read(&project, MembershipLimits::default(), &cancel).unwrap();
    assert_eq!(
        membership.directory_records()[0].outcome(),
        Err(MembershipIssue::KindMismatch)
    );
    assert_eq!(
        membership.directory_records()[1]
            .outcome()
            .unwrap()
            .counterpart(),
        0
    );
    assert_eq!(membership.project_records().len(), 2);
    assert_eq!(membership.project_stream().bytes(), properties);
    for part in membership.project_stream().source_parts() {
        assert_eq!(
            &input[part.container_range()],
            &properties[part.stream_range()]
        );
    }
    assert_eq!(
        project
            .read_module(0, ModuleReadLimits::default(), &cancel)
            .unwrap()
            .bytes(),
        b"first body"
    );
    let mut limits = MembershipLimits::default();
    limits.project.input_bytes = properties.len() - 1;
    assert!(ProjectMembership::read(&project, limits, &cancel).is_err());
    let mut limits = MembershipLimits::default();
    limits.directory.modules = 1;
    assert!(matches!(
        ProjectMembership::read(&project, limits, &cancel),
        Err(MembershipError::Directory(_))
    ));
}

#[test]
fn source_header_binds_metadata_without_hiding_conflicting_bodies() {
    use crate::{
        vba_header::{HeaderLimits, HeaderName},
        vba_membership::{MembershipIssue, MembershipLimits, ProjectMembership},
        vba_text::TextLimits,
    };

    let properties = b"ID=\"{01234567-89AB-CDEF-0123-456789ABCDEF}\"\r\nModule=Source\r\nName=\"Project\"\r\nHelpContextID=\"0\"\r\nCMG=\"00\"\r\nDPB=\"00\"\r\nGC=\"00\"\r\n\r\n";
    for name in ["SOURCE", "Unrelated"] {
        let body = format!("Attribute VB_Name = \"{name}\"\r\nSub Work()\r\nEnd Sub\r\n");
        let input = fixture(
            &[("Source", 0)],
            vec![
                stream("Source", body.as_bytes()),
                Stream {
                    name: "PROJECT",
                    bytes: properties.to_vec(),
                    inside: false,
                },
            ],
        );
        let compound = parse(&input);
        let cancel = Cancellation::new();
        let project =
            VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel).unwrap();
        let membership =
            ProjectMembership::read(&project, MembershipLimits::default(), &cancel).unwrap();
        let source = project
            .read_module(0, ModuleReadLimits::default(), &cancel)
            .unwrap();
        let text = source.decode_text(TextLimits::default(), &cancel).unwrap();
        let header = HeaderName::parse(
            &text,
            crate::vba_dir::ModuleKind::Procedural,
            HeaderLimits::default(),
            &cancel,
        )
        .unwrap();
        let record = &membership.directory_records()[0];
        let outcome = record.check_source_name(&header);
        if name == "SOURCE" {
            assert_eq!(outcome, record.outcome());
        } else {
            assert_eq!(outcome, Err(MembershipIssue::SourceNameMismatch));
        }
        assert_eq!(source.bytes(), body.as_bytes());
        assert_eq!(&source.bytes()[header.original_range()], name.as_bytes());
        assert_eq!(membership.directory_records().len(), 1);
        assert_eq!(membership.project_records().len(), 1);
    }
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

#[test]
fn project_properties_are_siblings_and_cannot_hide_readable_modules() {
    let cancel = Cancellation::new();
    let body = b"readable";
    for properties in [
        vec![],
        vec![Stream {
            name: "PROJECT",
            bytes: b"child".to_vec(),
            inside: true,
        }],
        vec![
            Stream {
                name: "PROJECT",
                bytes: b"first".to_vec(),
                inside: false,
            },
            Stream {
                name: "PROJECT",
                bytes: b"second".to_vec(),
                inside: false,
            },
        ],
    ] {
        let mut streams = vec![stream("Source", body)];
        streams.extend(properties);
        let input = fixture(&[("Source", 0)], streams);
        let compound = parse(&input);
        let project =
            VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel).unwrap();
        assert!(matches!(
            project.read_project_stream(4096, &cancel),
            Err(SourceError::ProjectMetadata)
        ));
        assert_eq!(
            project
                .read_module(0, ModuleReadLimits::default(), &cancel)
                .unwrap()
                .bytes(),
            body
        );
    }
    let properties = b"unparsed original project bytes";
    let input = fixture(
        &[("Source", 0)],
        vec![
            stream("Source", body),
            Stream {
                name: "PROJECT",
                bytes: properties.to_vec(),
                inside: false,
            },
        ],
    );
    let compound = parse(&input);
    let project = VbaProject::open(&compound, 1, ProjectReadLimits::default(), &cancel).unwrap();
    let source = project
        .read_project_stream(properties.len(), &cancel)
        .unwrap();
    assert_eq!(source.bytes(), properties);
    for part in source.source_parts() {
        assert_eq!(
            &input[part.container_range()],
            &source.bytes()[part.stream_range()]
        );
    }
    assert!(matches!(
        project.read_project_stream(properties.len() - 1, &cancel),
        Err(SourceError::Compound(CompoundError::Limit { .. }))
    ));
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        project.read_project_stream(4096, &cancel),
        Err(SourceError::Cancelled(_))
    ));
}
