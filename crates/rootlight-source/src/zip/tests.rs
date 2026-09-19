//! Authored wire fixtures exercise redundant ZIP metadata and physical provenance.
//! Payload integrity uses published CRC vectors, independent of archive construction.

use super::*;
use rootlight_cancel::CancellationReason;

fn field(bytes: &mut [u8], start: usize, width: usize, value: u64) {
    bytes[start..start + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

fn fixture(names: &[&[u8]], zip64: bool, descriptor: Option<bool>, method: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut directory = Vec::new();
    for name in names {
        let offset = bytes.len();
        let mut local = vec![0; 30];
        let flags = if descriptor.is_some() { 8 } else { 0 };
        field(&mut local, 0, 4, 0x0403_4b50);
        field(&mut local, 4, 2, if zip64 { 45 } else { 20 });
        field(&mut local, 6, 2, flags);
        field(&mut local, 8, 2, u64::from(method));
        if descriptor.is_none() {
            field(&mut local, 14, 4, 0xcbf4_3926);
        }
        for size_field in [18, 22] {
            field(
                &mut local,
                size_field,
                4,
                if zip64 {
                    0xffff_ffff
                } else if descriptor.is_some() {
                    0
                } else {
                    9
                },
            );
        }
        field(&mut local, 26, 2, u64::try_from(name.len()).unwrap());
        field(&mut local, 28, 2, if zip64 { 20 } else { 0 });
        bytes.extend_from_slice(&local);
        bytes.extend_from_slice(name);
        if zip64 {
            bytes.extend_from_slice(&[1, 0, 16, 0]);
            bytes.extend_from_slice(&9_u64.to_le_bytes());
            bytes.extend_from_slice(&9_u64.to_le_bytes());
        }
        bytes.extend_from_slice(b"123456789");
        if let Some(signature) = descriptor {
            if signature {
                bytes.extend_from_slice(&0x0807_4b50_u32.to_le_bytes());
            }
            bytes.extend_from_slice(&0xcbf4_3926_u32.to_le_bytes());
            for _ in 0..2 {
                if zip64 {
                    bytes.extend_from_slice(&9_u64.to_le_bytes());
                } else {
                    bytes.extend_from_slice(&9_u32.to_le_bytes());
                }
            }
        }
        let mut header = vec![0; 46];
        field(&mut header, 0, 4, 0x0201_4b50);
        field(&mut header, 6, 2, if zip64 { 45 } else { 20 });
        field(&mut header, 8, 2, flags);
        field(&mut header, 10, 2, u64::from(method));
        field(&mut header, 16, 4, 0xcbf4_3926);
        for size_field in [20, 24] {
            field(
                &mut header,
                size_field,
                4,
                if zip64 { 0xffff_ffff } else { 9 },
            );
        }
        field(&mut header, 28, 2, u64::try_from(name.len()).unwrap());
        field(&mut header, 30, 2, if zip64 { 28 } else { 0 });
        field(
            &mut header,
            42,
            4,
            if zip64 {
                0xffff_ffff
            } else {
                u64::try_from(offset).unwrap()
            },
        );
        directory.extend_from_slice(&header);
        directory.extend_from_slice(name);
        if zip64 {
            directory.extend_from_slice(&[1, 0, 24, 0]);
            directory.extend_from_slice(&9_u64.to_le_bytes());
            directory.extend_from_slice(&9_u64.to_le_bytes());
            directory.extend_from_slice(&u64::try_from(offset).unwrap().to_le_bytes());
        }
    }
    let central = bytes.len();
    bytes.extend_from_slice(&directory);
    if zip64 {
        let location = bytes.len();
        let mut end = vec![0; 56];
        field(&mut end, 0, 4, 0x0606_4b50);
        field(&mut end, 4, 8, 44);
        field(&mut end, 14, 2, 45);
        for count_field in [24, 32] {
            field(
                &mut end,
                count_field,
                8,
                u64::try_from(names.len()).unwrap(),
            );
        }
        field(&mut end, 40, 8, u64::try_from(directory.len()).unwrap());
        field(&mut end, 48, 8, u64::try_from(central).unwrap());
        bytes.extend_from_slice(&end);
        let mut locator = vec![0; 20];
        field(&mut locator, 0, 4, 0x0706_4b50);
        field(&mut locator, 8, 8, u64::try_from(location).unwrap());
        field(&mut locator, 16, 4, 1);
        bytes.extend_from_slice(&locator);
    }
    let mut end = vec![0; 22];
    field(&mut end, 0, 4, 0x0605_4b50);
    for count_field in [8, 10] {
        field(
            &mut end,
            count_field,
            2,
            if zip64 {
                0xffff
            } else {
                u64::try_from(names.len()).unwrap()
            },
        );
    }
    field(
        &mut end,
        12,
        4,
        if zip64 {
            0xffff_ffff
        } else {
            u64::try_from(directory.len()).unwrap()
        },
    );
    field(
        &mut end,
        16,
        4,
        if zip64 {
            0xffff_ffff
        } else {
            u64::try_from(central).unwrap()
        },
    );
    bytes.extend_from_slice(&end);
    bytes
}

fn open(bytes: &[u8]) -> Result<ZipArchive<'_>, ZipError> {
    ZipArchive::open(bytes, ZipLimits::default(), &Cancellation::new())
}

#[test]
fn stored_zip_and_zip64_preserve_physical_ranges_and_descriptors() {
    for zip64 in [false, true] {
        for descriptor in [None, Some(false), Some(true)] {
            let bytes = fixture(&[b"nested/module.bas", b"peer"], zip64, descriptor, 0);
            let archive = open(&bytes).unwrap();
            assert_eq!(archive.entries().len(), 2);
            let mut end = 0;
            for entry in archive.entries() {
                assert_eq!(
                    entry.stored_bytes(9, &Cancellation::new()).unwrap(),
                    b"123456789"
                );
                assert_eq!(&bytes[entry.payload_range()], entry.encoded_bytes());
                assert_eq!(entry.local_range().end, entry.payload_range().start);
                assert_eq!(entry.payload_range().end, entry.descriptor_range().start);
                assert_eq!(entry.local_range().start, end);
                end = entry.descriptor_range().end;
                assert_eq!(entry.descriptor_range().is_empty(), descriptor.is_none());
            }
            assert_eq!(end, archive.central_range().start);
        }
    }
    assert!(
        open(&fixture(&[], false, None, 0))
            .unwrap()
            .entries()
            .is_empty()
    );
}

#[test]
fn opaque_names_duplicates_and_unknown_methods_do_not_imply_paths_or_decoding() {
    let bytes = fixture(&[b"../outside", b"../outside", b"\xff\0"], false, None, 99);
    let archive = open(&bytes).unwrap();
    assert_eq!(archive.entries()[0].name(), b"../outside");
    assert_eq!(archive.entries()[1].name(), b"../outside");
    assert_eq!(archive.entries()[2].name(), b"\xff\0");
    for entry in archive.entries() {
        assert!(matches!(
            entry.stored_bytes(9, &Cancellation::new()),
            Err(ZipError::Encoding)
        ));
    }
    assert!(!format!("{archive:?}").contains("outside"));
    assert!(!format!("{archive:?}").contains("123456789"));
}

#[test]
fn mismatched_local_metadata_and_overlapping_records_are_rejected() {
    let original = fixture(&[b"a", b"a"], false, None, 0);
    let ranges: Vec<_> = open(&original)
        .unwrap()
        .entries()
        .iter()
        .map(ZipEntry::central_range)
        .collect();
    for (offset, width, value) in [
        (6, 2, 1),
        (8, 2, 8),
        (14, 4, 0),
        (18, 4, 8),
        (22, 4, 8),
        (30, 1, u64::from(b'b')),
    ] {
        let mut bytes = original.clone();
        field(&mut bytes, offset, width, value);
        assert!(open(&bytes).is_err(), "mutation at {offset}");
    }
    let mut bytes = original.clone();
    field(&mut bytes, ranges[1].start + 42, 4, 0);
    assert!(open(&bytes).is_err());
    let mut bytes = original.clone();
    field(&mut bytes, ranges[0].start + 20, 4, 0xffff_ffff);
    assert!(open(&bytes).is_err());
}

#[test]
fn descriptors_and_zip64_redundancy_are_verified() {
    for zip64 in [false, true] {
        let original = fixture(&[b"module"], zip64, Some(true), 0);
        let archive = open(&original).unwrap();
        let entry = &archive.entries()[0];
        for offset in [
            entry.descriptor_range().start + 4,
            entry.descriptor_range().start + 8,
        ] {
            let mut bytes = original.clone();
            bytes[offset] ^= 1;
            assert!(open(&bytes).is_err());
        }
        let mut bytes = original.clone();
        let len = bytes.len();
        field(&mut bytes, len - 22 + 8, 2, 2);
        assert!(open(&bytes).is_err());
        if zip64 {
            let mut bytes = original.clone();
            field(&mut bytes, len - 42 + 8, 8, u64::MAX);
            assert!(open(&bytes).is_err());
            let mut bytes = original.clone();
            field(&mut bytes, len - 98 + 40, 8, u64::MAX);
            assert!(open(&bytes).is_err());
        }
    }
}

#[test]
fn integrity_checks_detect_changed_payloads_and_enforce_output_budgets() {
    let mut bytes = fixture(&[b"module"], false, None, 0);
    let offset = open(&bytes).unwrap().entries()[0].payload_range().start;
    bytes[offset] ^= 1;
    let archive = open(&bytes).unwrap();
    let entry = &archive.entries()[0];
    assert!(matches!(
        entry.stored_bytes(9, &Cancellation::new()),
        Err(ZipError::Integrity)
    ));
    assert!(matches!(
        entry.verify_expanded(b"123456789", 8, &Cancellation::new()),
        Err(ZipError::Limit { .. })
    ));
    assert!(matches!(
        entry.verify_expanded(b"12345678", 9, &Cancellation::new()),
        Err(ZipError::Integrity)
    ));
    entry
        .verify_expanded(b"123456789", 9, &Cancellation::new())
        .unwrap();
    assert_eq!(crc32(b"", &Cancellation::new()).unwrap(), 0);
    assert_eq!(
        crc32(b"123456789", &Cancellation::new()).unwrap(),
        0xcbf4_3926
    );
}

#[test]
fn input_entry_limits_cancellation_and_all_truncations_are_total() {
    let bytes = fixture(&[b"a", b"b"], true, Some(true), 8);
    let limits = ZipLimits {
        input_bytes: bytes.len(),
        entries: 2,
    };
    ZipArchive::open(&bytes, limits, &Cancellation::new()).unwrap();
    for limits in [
        ZipLimits {
            input_bytes: bytes.len() - 1,
            ..limits
        },
        ZipLimits {
            entries: 1,
            ..limits
        },
    ] {
        assert!(matches!(
            ZipArchive::open(&bytes, limits, &Cancellation::new()),
            Err(ZipError::Limit { .. })
        ));
    }
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        ZipArchive::open(&bytes, limits, &cancellation),
        Err(ZipError::Cancelled(_))
    ));
    for end in 0..bytes.len() {
        assert!(open(&bytes[..end]).is_err(), "truncated at {end}");
    }
    for offset in 0..bytes.len() {
        let mut mutation = bytes.clone();
        mutation[offset] ^= 0xff;
        let _ = open(&mutation);
    }
}

#[test]
fn comments_cannot_redirect_the_end_record_and_split_archives_fail() {
    let mut bytes = fixture(&[b"a"], false, None, 0);
    let end = bytes.len() - 22;
    field(&mut bytes, end + 20, 2, 4);
    bytes.extend_from_slice(b"note");
    open(&bytes).unwrap();
    field(&mut bytes, end + 4, 2, 1);
    assert!(matches!(open(&bytes), Err(ZipError::Split)));
    let mut bytes = fixture(&[], false, None, 0);
    field(&mut bytes, 20, 2, 22);
    bytes.extend_from_slice(&fixture(&[], false, None, 0));
    assert!(open(&bytes).is_err());
}

#[test]
fn central_order_is_independent_of_physical_order() {
    let mut bytes = fixture(&[b"a", b"b"], false, None, 0);
    let archive = open(&bytes).unwrap();
    let first = archive.entries()[0].central_range();
    let second = archive.entries()[1].central_range();
    let saved_first = bytes[first.clone()].to_vec();
    let saved_second = bytes[second.clone()].to_vec();
    bytes[first].copy_from_slice(&saved_second);
    bytes[second].copy_from_slice(&saved_first);
    let archive = open(&bytes).unwrap();
    assert_eq!(archive.entries()[0].name(), b"b");
    assert_eq!(archive.entries()[1].name(), b"a");
    assert!(
        archive.entries()[0].payload_range().start > archive.entries()[1].payload_range().start
    );
}

#[test]
fn unknown_payload_flags_and_malformed_extra_fields_are_not_silently_decoded() {
    let original = fixture(&[b"a"], false, None, 0);
    let central = open(&original).unwrap().central_range().start;
    for flags in [1, 0x20, 0x40, 0x2000, 0x8000] {
        let mut bytes = original.clone();
        field(&mut bytes, 6, 2, flags);
        field(&mut bytes, central + 8, 2, flags);
        let archive = open(&bytes).unwrap();
        assert_eq!(u64::from(archive.entries()[0].flags()), flags);
        assert!(matches!(
            archive.entries()[0].stored_bytes(9, &Cancellation::new()),
            Err(ZipError::Encoding)
        ));
    }
    let original = fixture(&[b"a"], true, None, 0);
    for offset in [31, 33] {
        let mut bytes = original.clone();
        field(&mut bytes, offset, 2, 0xffff);
        assert!(open(&bytes).is_err());
    }
    assert!(matches!(
        extra_zip64(&[1, 0, 0, 0, 1, 0, 0, 0], &Cancellation::new()),
        Err(ZipError::Invalid { .. })
    ));
}
