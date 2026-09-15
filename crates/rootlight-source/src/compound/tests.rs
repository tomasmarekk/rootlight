//! Authored compound-file fixtures separate physical allocation from logical data.
//! They exercise public extraction and original-byte mappings, including hostile
//! allocation graphs, without Office applications or third-party fixture files.

use super::*;
use rootlight_cancel::CancellationReason;

struct Fixture {
    bytes: Vec<u8>,
    small: Vec<u8>,
    large: Vec<u8>,
    directory: usize,
    mini_fat: usize,
    first_large: usize,
    difat: Option<usize>,
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn entry(bytes: &mut [u8], offset: usize, name: &str, kind: u8, start: u32, length: usize) {
    let mut units: Vec<_> = name.encode_utf16().collect();
    units.push(0);
    for (i, unit) in units.iter().enumerate() {
        put16(bytes, offset + i * 2, *unit);
    }
    put16(bytes, offset + 64, u16::try_from(units.len() * 2).unwrap());
    bytes[offset + 66] = kind;
    bytes[offset + 67] = 1;
    for at in [68, 72, 76] {
        put32(bytes, offset + at, FREE);
    }
    put32(bytes, offset + 116, start);
    put32(bytes, offset + 120, u32::try_from(length).unwrap());
}

fn fixture(version: u16, extended_difat: bool) -> Fixture {
    let size = if version == 3 { 512 } else { 4096 };
    let fat_count = if extended_difat { 110 } else { 1 };
    let difat_id = extended_difat.then_some(fat_count);
    let directory_id = fat_count + usize::from(extended_difat);
    let mini_id = directory_id + 1;
    let mini_fat_id = directory_id + 2;
    let first_large = directory_id + 3;
    let small: Vec<_> = (0..70)
        .map(|i| b'a' + u8::try_from(i % 26).unwrap())
        .collect();
    let large: Vec<_> = (0..4113).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let permutation = if version == 3 {
        vec![0, 2, 1, 4, 3, 6, 5, 8, 7]
    } else {
        vec![0, 2]
    };
    let ids: Vec<_> = permutation.iter().map(|n| first_large + n).collect();
    let sectors = ids.iter().max().unwrap() + 1;
    let mut bytes = vec![0; (sectors + 1) * size];
    bytes[..8].copy_from_slice(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]);
    for (offset, value) in [
        (24, 0x3e),
        (26, version),
        (28, 0xfffe),
        (30, if version == 3 { 9 } else { 12 }),
        (32, 6),
    ] {
        put16(&mut bytes, offset, value);
    }
    for (offset, value) in [
        (40, usize::from(version == 4)),
        (44, fat_count),
        (48, directory_id),
        (56, 4096),
        (60, mini_fat_id),
        (64, 1),
        (72, usize::from(extended_difat)),
    ] {
        put32(&mut bytes, offset, u32::try_from(value).unwrap());
    }
    put32(
        &mut bytes,
        68,
        difat_id.map_or(END, |id| u32::try_from(id).unwrap()),
    );
    for i in 0..109 {
        put32(
            &mut bytes,
            76 + i * 4,
            if i < fat_count {
                u32::try_from(i).unwrap()
            } else {
                FREE
            },
        );
    }
    if let Some(id) = difat_id {
        let base = (id + 1) * size;
        for i in (0..size).step_by(4) {
            put32(&mut bytes, base + i, FREE);
        }
        put32(&mut bytes, base, 109);
        put32(&mut bytes, base + size - 4, END);
    }
    let mut fat = vec![FREE; fat_count * size / 4];
    fat[..fat_count].fill(FAT);
    if let Some(id) = difat_id {
        fat[id] = DIFAT;
    }
    for id in [directory_id, mini_id, mini_fat_id] {
        fat[id] = END;
    }
    for (i, id) in ids.iter().enumerate() {
        fat[*id] = ids.get(i + 1).map_or(END, |id| u32::try_from(*id).unwrap());
    }
    for (i, value) in fat.iter().enumerate() {
        put32(
            &mut bytes,
            (i / (size / 4) + 1) * size + (i % (size / 4)) * 4,
            *value,
        );
    }
    let directory = (directory_id + 1) * size;
    entry(
        &mut bytes,
        directory,
        "Root Entry",
        5,
        u32::try_from(mini_id).unwrap(),
        192,
    );
    entry(&mut bytes, directory + 128, "Nested", 1, 0, 0);
    entry(&mut bytes, directory + 256, "Small", 2, 2, small.len());
    entry(
        &mut bytes,
        directory + 384,
        "Large",
        2,
        u32::try_from(first_large).unwrap(),
        large.len(),
    );
    put32(&mut bytes, directory + 76, 1);
    put32(&mut bytes, directory + 128 + 76, 2);
    put32(&mut bytes, directory + 256 + 72, 3);
    let mini_fat = (mini_fat_id + 1) * size;
    for i in (0..size).step_by(4) {
        put32(&mut bytes, mini_fat + i, FREE);
    }
    put32(&mut bytes, mini_fat, END);
    put32(&mut bytes, mini_fat + 8, 0);
    let mini_base = (mini_id + 1) * size;
    bytes[mini_base + 128..mini_base + 192].copy_from_slice(&small[..64]);
    bytes[mini_base..mini_base + 6].copy_from_slice(&small[64..]);
    for (chunk, id) in large.chunks(size).zip(ids) {
        bytes[(id + 1) * size..(id + 1) * size + chunk.len()].copy_from_slice(chunk);
    }
    Fixture {
        bytes,
        small,
        large,
        directory,
        mini_fat,
        first_large,
        difat: difat_id.map(|id| (id + 1) * size),
    }
}

fn parse(bytes: &[u8]) -> Result<CompoundFile<'_>, CompoundError> {
    CompoundFile::parse(bytes, CompoundLimits::default(), &Cancellation::new())
}

fn assert_mapping(container: &[u8], stream: &CompoundStream) {
    let mut position = 0;
    for part in stream.source_parts() {
        assert_eq!(part.stream_range().start, position);
        assert_eq!(
            &container[part.container_range()],
            &stream.bytes()[part.stream_range()]
        );
        position = part.stream_range().end;
    }
    assert_eq!(position, stream.bytes().len());
}

#[test]
fn reads_fragmented_regular_and_mini_streams_with_exact_original_ranges() {
    for version in [3, 4] {
        for extended in [false, true] {
            let f = fixture(version, extended);
            let file = parse(&f.bytes).unwrap();
            let entries: Vec<_> = file.entries().collect();
            assert_eq!(entries.len(), 4);
            assert_eq!(entries[0].kind(), CompoundEntryKind::Root);
            assert_eq!(entries[0].parent(), None);
            assert_eq!(entries[1].parent(), Some(0));
            for (id, expected) in [(2, &f.small), (3, &f.large)] {
                let entry = entries.iter().find(|e| e.id() == id).unwrap();
                assert_eq!(entry.parent(), Some(1));
                assert_eq!(entry.byte_length(), expected.len());
                assert_eq!(entry.directory_range().len(), 128);
                let stream = file
                    .read_stream(id, expected.len(), &Cancellation::new())
                    .unwrap();
                assert_eq!(stream.bytes(), expected);
                assert_mapping(&f.bytes, &stream);
            }
            assert_eq!(entries[2].name(), "Small");
            assert!(matches!(
                file.read_stream(1, 100, &Cancellation::new()),
                Err(CompoundError::NotStream)
            ));
            assert!(matches!(
                file.read_stream(50_000, 100, &Cancellation::new()),
                Err(CompoundError::NotStream)
            ));
        }
    }
}

#[test]
fn accepts_legacy_v3_size_padding_but_not_oversized_v4_streams() {
    let mut f = fixture(3, false);
    for slot in [0, 2, 3] {
        put32(&mut f.bytes, f.directory + slot * 128 + 124, 0xdead_beef);
    }
    assert_eq!(
        parse(&f.bytes)
            .unwrap()
            .read_stream(3, f.large.len(), &Cancellation::new())
            .unwrap()
            .bytes(),
        f.large
    );
    let mut f = fixture(4, false);
    put32(&mut f.bytes, f.directory + 384 + 124, 1);
    assert!(matches!(parse(&f.bytes), Err(CompoundError::Allocation)));
}

#[test]
fn rejects_cycles_aliases_metadata_overlap_and_truncated_chains() {
    for case in 0..8 {
        let mut f = fixture(3, false);
        match case {
            0 => put32(
                &mut f.bytes,
                512 + f.first_large * 4,
                u32::try_from(f.first_large).unwrap(),
            ),
            1 => put32(&mut f.bytes, f.directory + 384 + 116, 1),
            2 => put32(&mut f.bytes, f.mini_fat + 8, 2),
            3 => {
                put32(&mut f.bytes, f.directory + 384 + 116, 2);
                put32(&mut f.bytes, f.directory + 384 + 120, 70);
            }
            4 => put32(&mut f.bytes, 512 + f.first_large * 4, END),
            5 => put32(&mut f.bytes, f.mini_fat + 8, FREE),
            6 => put32(&mut f.bytes, f.directory + 256 + 116, 50_000),
            7 => put32(&mut f.bytes, f.directory + 384 + 120, u32::MAX),
            _ => unreachable!(),
        }
        assert!(
            matches!(parse(&f.bytes), Err(CompoundError::Allocation)),
            "case {case}"
        );
    }
    let mut f = fixture(3, true);
    put32(&mut f.bytes, 72, 2);
    put32(&mut f.bytes, f.difat.unwrap() + 508, 110);
    assert!(matches!(parse(&f.bytes), Err(CompoundError::Allocation)));
}

#[test]
fn rejects_directory_cycles_aliases_orphans_and_invalid_names() {
    for case in 0..7 {
        let mut f = fixture(3, false);
        match case {
            0 => put32(&mut f.bytes, f.directory + 256 + 72, 2),
            1 => put32(&mut f.bytes, f.directory + 128 + 68, 2),
            2 => put32(&mut f.bytes, f.directory + 256 + 72, FREE),
            3 => put16(&mut f.bytes, f.directory + 256 + 64, 63),
            4 => put16(&mut f.bytes, f.directory + 256, 0xd800),
            5 => put16(&mut f.bytes, f.directory + 256, u16::from(b'/')),
            6 => put32(&mut f.bytes, f.directory + 256 + 76, 3),
            _ => unreachable!(),
        }
        assert!(
            matches!(parse(&f.bytes), Err(CompoundError::Directory)),
            "case {case}"
        );
    }
}

#[test]
fn caller_limits_and_cancellation_are_enforced_before_output_allocation() {
    let f = fixture(3, false);
    let limits = CompoundLimits {
        input_bytes: f.bytes.len() - 1,
        ..CompoundLimits::default()
    };
    assert!(matches!(
        CompoundFile::parse(&f.bytes, limits, &Cancellation::new()),
        Err(CompoundError::Limit {
            resource: "input bytes",
            ..
        })
    ));
    let limits = CompoundLimits {
        directory_entries: 3,
        ..CompoundLimits::default()
    };
    assert!(matches!(
        CompoundFile::parse(&f.bytes, limits, &Cancellation::new()),
        Err(CompoundError::Limit {
            resource: "directory entries",
            ..
        })
    ));
    let limits = CompoundLimits {
        work_units: 1,
        ..CompoundLimits::default()
    };
    assert!(matches!(
        CompoundFile::parse(&f.bytes, limits, &Cancellation::new()),
        Err(CompoundError::Limit {
            resource: "work",
            ..
        })
    ));
    let file = parse(&f.bytes).unwrap();
    assert!(matches!(
        file.read_stream(2, 69, &Cancellation::new()),
        Err(CompoundError::Limit {
            resource: "stream bytes",
            maximum: 69
        })
    ));
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        CompoundFile::parse(&f.bytes, CompoundLimits::default(), &cancel),
        Err(CompoundError::Cancelled(_))
    ));
    assert!(matches!(
        file.read_stream(2, 70, &cancel),
        Err(CompoundError::Cancelled(_))
    ));
}

#[test]
fn empty_streams_and_root_storage_need_no_data_sectors() {
    for version in [3, 4] {
        let mut f = fixture(version, false);
        for id in [0, 2, 3] {
            let at = f.directory + id * 128;
            put32(&mut f.bytes, at + 116, END);
            put32(&mut f.bytes, at + 120, 0);
        }
        put32(&mut f.bytes, 60, END);
        put32(&mut f.bytes, 64, 0);
        let file = parse(&f.bytes).unwrap();
        for id in [2, 3] {
            let stream = file.read_stream(id, 0, &Cancellation::new()).unwrap();
            assert!(stream.bytes().is_empty());
            assert!(stream.source_parts().is_empty());
        }
        for id in 1..=3 {
            f.bytes[f.directory + id * 128 + 66] = 0;
        }
        put32(&mut f.bytes, f.directory + 76, FREE);
        assert_eq!(parse(&f.bytes).unwrap().entries().count(), 1);
    }
}

#[test]
fn every_truncation_and_deterministic_metadata_mutation_is_bounded() {
    let f = fixture(3, false);
    for end in 0..f.bytes.len() {
        assert!(parse(&f.bytes[..end]).is_err(), "truncation {end}");
    }
    for position in 0..f.directory + 512 {
        let mut bytes = f.bytes.clone();
        bytes[position] ^= 0xff;
        if let Ok(file) = parse(&bytes) {
            for entry in file
                .entries()
                .filter(|e| e.kind() == CompoundEntryKind::Stream)
            {
                let stream = file
                    .read_stream(entry.id(), bytes.len(), &Cancellation::new())
                    .unwrap();
                assert_mapping(&bytes, &stream);
            }
        }
    }
}
