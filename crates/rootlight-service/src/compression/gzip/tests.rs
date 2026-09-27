//! Independent gzip encoders and authored headers validate recovery compatibility.
//! Fragmented reads exercise bounded history, final checksums and failed readers.

use super::*;
use flate2::{Compression, write::GzEncoder};
use rootlight_cancel::CancellationReason;
use std::io::{Read, Write};

fn encoded(source: &[u8], level: Compression) -> Vec<u8> {
    let mut writer = GzEncoder::new(Vec::new(), level);
    writer.write_all(source).unwrap();
    writer.finish().unwrap()
}

fn decoded(input: &[u8], expected: usize, chunk: usize) -> io::Result<Vec<u8>> {
    let cancellation = Cancellation::new();
    let mut reader = Reader::new(input, expected, &cancellation)?;
    assert_eq!(reader.read(&mut [])?, 0);
    let mut output = Vec::new();
    let mut buffer = vec![0; chunk];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(reader.read(&mut buffer)?, 0);
    Ok(output)
}

fn independent_crc(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 0 { 0 } else { 0xedb8_8320 };
        }
    }
    !crc
}

#[test]
fn fragmented_recovery_matches_independent_encoder_across_window_wraps() {
    let mut seed = 0x4ab7_65d3_u32;
    let random: Vec<_> = (0..180_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            u8::try_from(seed & 255).unwrap()
        })
        .collect();
    let sources = [
        Vec::new(),
        b"123456789".to_vec(),
        vec![b'x'; 180_000],
        random,
        (0..80_000_u32).flat_map(u32::to_le_bytes).collect(),
    ];
    for source in sources {
        for level in [
            Compression::none(),
            Compression::fast(),
            Compression::best(),
        ] {
            let input = encoded(&source, level);
            for chunk in [1, 7, 4096, 131_072] {
                assert_eq!(decoded(&input, source.len(), chunk).unwrap(), source);
            }
        }
    }
    assert_eq!(
        !update_crc(u32::MAX, b"123456789", &Cancellation::new()).unwrap(),
        0xcbf4_3926
    );
}

#[test]
fn word_crc_preserves_every_tail_alignment_and_incremental_boundary() {
    let source: Vec<_> = (0..1100_usize)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    let cancellation = Cancellation::new();
    for start in 0..8 {
        for length in 0..1000 {
            let bytes = &source[start..start + length];
            let expected = independent_crc(bytes);
            assert_eq!(
                !update_crc(u32::MAX, bytes, &cancellation).unwrap(),
                expected
            );
            let mut crc = u32::MAX;
            for chunk in bytes.chunks(13) {
                crc = update_crc(crc, chunk, &cancellation).unwrap();
            }
            assert_eq!(!crc, expected);
        }
    }
}

#[test]
fn optional_header_fields_and_header_crc_are_validated_without_path_use() {
    let source = b"immutable recovery document";
    let input = encoded(source, Compression::fast());
    let mut header = input[..10].to_vec();
    header[3] = 0x1f;
    header.extend_from_slice(&[3, 0, 12, 34, 56]);
    header.extend_from_slice(b"../unused-name\0unused comment\0");
    let checksum = independent_crc(&header).to_le_bytes();
    header.extend_from_slice(&checksum[..2]);
    let payload = header.len();
    header.extend_from_slice(&input[10..]);
    assert_eq!(decoded(&header, source.len(), 3).unwrap(), source);
    header[payload - 1] ^= 1;
    assert!(decoded(&header, source.len(), 3).is_err());
    for flags in [4, 8, 16, 2, 0xe0] {
        let mut incomplete = input[..10].to_vec();
        incomplete[3] = flags;
        incomplete.extend_from_slice(&input[input.len() - 8..]);
        assert!(decoded(&incomplete, source.len(), 3).is_err());
    }
}

#[test]
fn truncation_extra_members_size_and_crc_errors_never_complete() {
    let source = b"exact bounded recovery payload";
    let input = encoded(source, Compression::best());
    for length in 0..input.len() {
        assert!(
            decoded(&input[..length], source.len(), 5).is_err(),
            "prefix {length}"
        );
    }
    for expected in [0, source.len() - 1, source.len() + 1] {
        assert!(decoded(&input, expected, 5).is_err());
    }
    for offset in [0, 1, 2, input.len() - 8, input.len() - 4] {
        let mut corrupt = input.clone();
        corrupt[offset] ^= 0xff;
        assert!(decoded(&corrupt, source.len(), 5).is_err());
    }
    for suffix in [vec![0], encoded(b"", Compression::fast()), input.clone()] {
        let mut trailing = input.clone();
        trailing.extend_from_slice(&suffix);
        assert!(decoded(&trailing, source.len(), 5).is_err());
    }
    // Repeating an otherwise exact member keeps the final trailer's size valid;
    // only strict raw-stream completion rejects the second member.
    let mut twice = input.clone();
    twice.extend_from_slice(&input);
    assert!(decoded(&twice, source.len(), 5).is_err());
}

#[test]
fn checksum_failure_is_sticky_and_cancellation_interrupts_buffered_reads() {
    assert_eq!(
        codec_error(Error::Allocation).kind(),
        io::ErrorKind::OutOfMemory
    );
    let source = vec![65; 200_000];
    let mut input = encoded(&source, Compression::fast());
    let crc_offset = input.len() - 8;
    input[crc_offset] ^= 1;
    let cancellation = Cancellation::new();
    let mut reader = Reader::new(&input, source.len(), &cancellation).unwrap();
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
    assert!(reader.read(&mut [0; 1]).is_err());
    let input = encoded(&source, Compression::fast());
    let mut reader = Reader::new(&input, source.len(), &cancellation).unwrap();
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(reader.read(&mut [0; 1]).is_err());
    assert!(Reader::new(&input, source.len(), &cancellation).is_err());
}

#[test]
#[ignore = "requires a local gzip sample and an optimized build"]
fn measure_admitted_gzip_snapshot() {
    fn consume(mut reader: impl Read) -> (u64, blake3::Hash) {
        let mut hash = blake3::Hasher::new();
        let mut count = 0_u64;
        let mut buffer = [0; 65_536];
        loop {
            let length = reader.read(&mut buffer).unwrap();
            if length == 0 {
                break;
            }
            hash.update(&buffer[..length]);
            count += u64::try_from(length).unwrap();
        }
        (count, hash.finalize())
    }
    let path = std::env::var_os("ROOTLIGHT_GZIP_SAMPLE").expect("provide a local gzip sample");
    let file = std::fs::File::open(path).unwrap();
    assert!(file.metadata().unwrap().len() <= 256 * 1024 * 1024);
    let mut input = Vec::new();
    file.take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut input)
        .unwrap();
    assert!(input.len() <= 256 * 1024 * 1024);
    let expected = usize::try_from(u32::from_le_bytes(
        input[input.len() - 4..].try_into().unwrap(),
    ))
    .unwrap();
    assert!(expected <= 512 * 1024 * 1024);
    let cancellation = Cancellation::new();
    let mut reference = Vec::new();
    let mut first_party = Vec::new();
    let mut identity = None;
    // Alternate order so warmup and cache effects are not assigned only to one codec.
    for round in 0..6 {
        for own in [round % 2 == 0, round % 2 != 0] {
            let start = std::time::Instant::now();
            let observed = if own {
                consume(Reader::new(std::hint::black_box(&input), expected, &cancellation).unwrap())
            } else {
                consume(flate2::read::GzDecoder::new(std::hint::black_box(
                    input.as_slice(),
                )))
            };
            let elapsed = start.elapsed().as_micros();
            assert_eq!(observed.0, u64::try_from(expected).unwrap());
            if let Some(identity) = identity {
                assert_eq!(observed, identity);
            } else {
                identity = Some(observed);
            }
            if own {
                first_party.push(elapsed);
            } else {
                reference.push(elapsed);
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "encoded_bytes": input.len(), "decoded_bytes": expected,
            "encoded_blake3": blake3::hash(&input).to_hex().to_string(),
            "decoded_blake3": identity.unwrap().1.to_hex().to_string(),
            "first_party_micros": first_party, "reference_micros": reference,
        })
    );
}
