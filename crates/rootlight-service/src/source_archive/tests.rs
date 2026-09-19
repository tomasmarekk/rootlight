//! Authored ZIP fixtures exercise expansion admission, framing and retained provenance.
//! Invalid payloads retain valid envelopes so failures reach the production decoder.

use super::*;
use flate2::{Compression, write::DeflateEncoder};
use rootlight_cancel::CancellationReason;
use std::io::Write;

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

struct Entry<'a> {
    payload: &'a [u8],
    declared: u64,
    checksum: u32,
    method: u16,
    flags: u16,
}

fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let offset = bytes.len();
        let mut local = vec![0; 31];
        number(&mut local, 0, 4, 0x0403_4b50);
        number(&mut local, 4, 2, 20);
        number(&mut local, 6, 2, u64::from(entry.flags));
        number(&mut local, 8, 2, u64::from(entry.method));
        number(&mut local, 14, 4, u64::from(entry.checksum));
        number(
            &mut local,
            18,
            4,
            u64::try_from(entry.payload.len()).unwrap(),
        );
        number(&mut local, 22, 4, entry.declared);
        number(&mut local, 26, 2, 1);
        local[30] = b'a';
        bytes.extend_from_slice(&local);
        bytes.extend_from_slice(entry.payload);
        let mut record = vec![0; 47];
        number(&mut record, 0, 4, 0x0201_4b50);
        number(&mut record, 6, 2, 20);
        number(&mut record, 8, 2, u64::from(entry.flags));
        number(&mut record, 10, 2, u64::from(entry.method));
        number(&mut record, 16, 4, u64::from(entry.checksum));
        number(
            &mut record,
            20,
            4,
            u64::try_from(entry.payload.len()).unwrap(),
        );
        number(&mut record, 24, 4, entry.declared);
        number(&mut record, 28, 2, 1);
        number(&mut record, 42, 4, u64::try_from(offset).unwrap());
        record[46] = b'a';
        central.extend_from_slice(&record);
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

fn deflate(bytes: &[u8], level: Compression) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), level);
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn entry<'a>(payload: &'a [u8], source: &[u8], method: u16) -> Entry<'a> {
    Entry {
        payload,
        declared: u64::try_from(source.len()).unwrap(),
        checksum: crc(source),
        method,
        flags: 0,
    }
}

#[test]
fn stored_and_deflated_sources_retain_exact_archive_identity_and_ranges() {
    let source = b"123456789";
    // One authored final uncompressed Deflate block, independent of the encoder.
    let compressed = b"\x01\x09\x00\xf6\xff123456789";
    let bytes = archive(&[entry(source, source, 0), entry(compressed, source, 8)]);
    let mut reader =
        ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new()).unwrap();
    let first = reader.read_entry(0, &Cancellation::new()).unwrap();
    let second = reader.read_entry(1, &Cancellation::new()).unwrap();
    for (ordinal, output) in [&first, &second].into_iter().enumerate() {
        assert_eq!(output.bytes(), source);
        assert_eq!(output.archive_hash(), content_hash(&bytes));
        assert_eq!(output.ordinal(), ordinal);
        assert_eq!(
            output.central_range(),
            reader.entries()[ordinal].central_range()
        );
        assert_eq!(
            output.local_range(),
            reader.entries()[ordinal].local_range()
        );
        assert_eq!(
            output.encoded_range(),
            reader.entries()[ordinal].payload_range()
        );
        assert_eq!(
            output.descriptor_range(),
            reader.entries()[ordinal].descriptor_range()
        );
        assert!(!format!("{output:?}").contains("123456789"));
    }
    assert_eq!(&bytes[second.encoded_range()], compressed);
    assert_eq!(reader.read_attempts(), 2);
    assert_eq!(reader.expanded_bytes_reserved(), 18);
    assert_eq!(
        reader.encoded_bytes_reserved(),
        source.len() + compressed.len()
    );
    drop(reader);
    assert_eq!(second.into_bytes(), source);
}

#[test]
fn empty_large_and_multi_chunk_deflate_streams_require_exact_completion() {
    for source in [
        Vec::new(),
        vec![b'x'; 100_000],
        (0..200_000_u32).flat_map(u32::to_le_bytes).collect(),
    ] {
        for level in [
            Compression::none(),
            Compression::fast(),
            Compression::best(),
        ] {
            let compressed = deflate(&source, level);
            let bytes = archive(&[entry(&compressed, &source, 8)]);
            let mut reader =
                ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new())
                    .unwrap();
            assert_eq!(
                reader.read_entry(0, &Cancellation::new()).unwrap().bytes(),
                source
            );
        }
    }
}

#[test]
fn truncated_trailing_and_concatenated_streams_never_publish_partial_bytes() {
    let source = b"source body";
    let compressed = deflate(source, Compression::fast());
    for end in 0..compressed.len() {
        let bytes = archive(&[entry(&compressed[..end], source, 8)]);
        let mut reader =
            ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new())
                .unwrap();
        assert!(
            reader.read_entry(0, &Cancellation::new()).is_err(),
            "truncated at {end}"
        );
    }
    for suffix in [b"junk".as_slice(), compressed.as_slice()] {
        let mut encoded = compressed.clone();
        encoded.extend_from_slice(suffix);
        let bytes = archive(&[entry(&encoded, source, 8)]);
        let mut reader =
            ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new())
                .unwrap();
        assert!(matches!(
            reader.read_entry(0, &Cancellation::new()),
            Err(ZipExpansionError::Deflate { .. })
        ));
    }
}

#[test]
fn false_sizes_crc_and_repeated_failures_consume_bounded_reservations() {
    let source = b"source body";
    let compressed = deflate(source, Compression::fast());
    for declared in [1, 100] {
        let bytes = archive(&[Entry {
            declared,
            ..entry(&compressed, source, 8)
        }]);
        let mut reader =
            ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new())
                .unwrap();
        assert!(matches!(
            reader.read_entry(0, &Cancellation::new()),
            Err(ZipExpansionError::OutputSize)
        ));
        assert_eq!(
            reader.expanded_bytes_reserved(),
            usize::try_from(declared).unwrap()
        );
    }
    let bytes = archive(&[
        Entry {
            checksum: 1,
            ..entry(&compressed, source, 8)
        },
        entry(source, source, 0),
    ]);
    let limits = ZipSourceLimits {
        expanded_bytes: source.len() * 2,
        ..ZipSourceLimits::default()
    };
    let mut reader = ZipSourceArchive::open(&bytes, limits, &Cancellation::new()).unwrap();
    assert!(matches!(
        reader.read_entry(0, &Cancellation::new()),
        Err(ZipExpansionError::Archive(ZipError::Integrity))
    ));
    assert_eq!(
        reader.read_entry(1, &Cancellation::new()).unwrap().bytes(),
        source
    );
    assert!(matches!(
        reader.read_entry(1, &Cancellation::new()),
        Err(ZipExpansionError::Limit {
            resource: "expanded bytes",
            ..
        })
    ));
    assert_eq!(reader.expanded_bytes_reserved(), source.len() * 2);
}

#[test]
fn entry_aggregate_input_and_attempt_limits_are_independent() {
    let source = b"123456789";
    let bytes = archive(&[entry(source, source, 0)]);
    for (limits, resource) in [
        (
            ZipSourceLimits {
                entry_bytes: 8,
                ..ZipSourceLimits::default()
            },
            "entry bytes",
        ),
        (
            ZipSourceLimits {
                expanded_bytes: 8,
                ..ZipSourceLimits::default()
            },
            "expanded bytes",
        ),
        (
            ZipSourceLimits {
                encoded_bytes: 8,
                ..ZipSourceLimits::default()
            },
            "encoded bytes",
        ),
    ] {
        let mut reader = ZipSourceArchive::open(&bytes, limits, &Cancellation::new()).unwrap();
        assert!(
            matches!(reader.read_entry(0, &Cancellation::new()), Err(ZipExpansionError::Limit { resource: actual, .. }) if actual == resource)
        );
        assert_eq!(reader.expanded_bytes_reserved(), 0);
        assert_eq!(reader.encoded_bytes_reserved(), 0);
    }
    let limits = ZipSourceLimits {
        reads: 1,
        entry_bytes: 9,
        expanded_bytes: 9,
        encoded_bytes: 9,
        ..ZipSourceLimits::default()
    };
    let mut reader = ZipSourceArchive::open(&bytes, limits, &Cancellation::new()).unwrap();
    reader.read_entry(0, &Cancellation::new()).unwrap();
    assert!(matches!(
        reader.read_entry(0, &Cancellation::new()),
        Err(ZipExpansionError::Limit {
            resource: "reads",
            ..
        })
    ));
    let mut reader = ZipSourceArchive::open(&bytes, limits, &Cancellation::new()).unwrap();
    assert!(matches!(
        reader.read_entry(2, &Cancellation::new()),
        Err(ZipExpansionError::MissingEntry)
    ));
    assert!(matches!(
        reader.read_entry(0, &Cancellation::new()),
        Err(ZipExpansionError::Limit {
            resource: "reads",
            ..
        })
    ));
}

#[test]
fn unsupported_flags_and_methods_are_not_decoded_or_budgeted_as_sources() {
    for method in [0, 8, 99] {
        for flags in [1, 0x20, 0x40, 0x2000] {
            let bytes = archive(&[Entry {
                flags,
                ..entry(b"", b"", method)
            }]);
            let mut reader =
                ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new())
                    .unwrap();
            assert!(matches!(
                reader.read_entry(0, &Cancellation::new()),
                Err(ZipExpansionError::Encoding { .. })
            ));
            assert_eq!(reader.expanded_bytes_reserved(), 0);
            assert_eq!(reader.read_attempts(), 1);
        }
    }
    let compressed = deflate(b"", Compression::fast());
    for flags in [0, 2, 4, 6, 0x0800] {
        let bytes = archive(&[Entry {
            flags,
            ..entry(&compressed, b"", 8)
        }]);
        let mut reader =
            ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new())
                .unwrap();
        assert!(
            reader
                .read_entry(0, &Cancellation::new())
                .unwrap()
                .bytes()
                .is_empty()
        );
    }
}

#[test]
fn cancellation_and_hard_bounds_do_not_change_existing_admission() {
    let bytes = archive(&[entry(b"source", b"source", 0)]);
    let mut reader =
        ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &Cancellation::new()).unwrap();
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        reader.read_entry(0, &cancellation),
        Err(ZipExpansionError::Cancelled(_))
    ));
    assert_eq!(reader.read_attempts(), 0);
    assert!(matches!(
        ZipSourceArchive::open(&bytes, ZipSourceLimits::default(), &cancellation),
        Err(ZipExpansionError::Cancelled(_))
    ));
    for limits in [
        ZipSourceLimits {
            reads: 0,
            ..ZipSourceLimits::default()
        },
        ZipSourceLimits {
            entry_bytes: usize::MAX,
            ..ZipSourceLimits::default()
        },
        ZipSourceLimits {
            expanded_bytes: usize::MAX,
            ..ZipSourceLimits::default()
        },
        ZipSourceLimits {
            encoded_bytes: usize::MAX,
            ..ZipSourceLimits::default()
        },
    ] {
        assert!(matches!(
            ZipSourceArchive::open(&bytes, limits, &Cancellation::new()),
            Err(ZipExpansionError::Limits)
        ));
    }
}
