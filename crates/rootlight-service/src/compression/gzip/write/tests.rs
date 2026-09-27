//! Independent decoder checks for first-party recovery encoding.
//! Fragmentation, streaming flush, destination failures and cancellation verify
//! that only completely finished members can pass recovery validation.

use super::*;
use crate::compression::gzip::Reader;
use rootlight_cancel::CancellationReason;
use std::io::Read;

fn random(length: usize, mut seed: u32) -> Vec<u8> {
    (0..length)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed.to_le_bytes()[0]
        })
        .collect()
}

fn verify(source: &[u8], encoded: &[u8]) {
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(encoded)
        .read_to_end(&mut decoded)
        .unwrap();
    assert_eq!(decoded, source);
    let mut decoded = Vec::new();
    Reader::new(encoded, source.len(), &Cancellation::new())
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
    assert_eq!(decoded, source);
}

#[test]
fn fragmented_writes_produce_identical_members_and_bounded_expansion() {
    let mut sources = vec![
        vec![],
        b"123456789".to_vec(),
        vec![b'a'; 200_000],
        random(200_000, 73),
    ];
    sources.push((0_u32..80_000).flat_map(u32::to_le_bytes).collect());
    let dictionary = random(32_768, 123);
    sources.push(dictionary.repeat(6));
    for source in sources {
        let mut identity = None;
        for chunk in [1, 17, 65_535, 262_144] {
            let cancellation = Cancellation::new();
            let mut writer = Writer::new(Vec::new(), &cancellation).unwrap();
            for bytes in source.chunks(chunk) {
                writer.write_all(bytes).unwrap();
            }
            writer.try_finish().unwrap();
            let once = writer.get_ref().len();
            writer.try_finish().unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.get_ref().len(), once);
            assert_eq!(
                writer.write(b"x").unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
            let encoded = writer.finish().unwrap();
            verify(&source, &encoded);
            assert!(encoded.len() <= source.len() + 18 + (source.len() / 65_535 + 1) * 6);
            if let Some(prior) = &identity {
                assert_eq!(&encoded, prior);
            } else {
                identity = Some(encoded);
            }
        }
    }
}

#[test]
fn generated_distributions_roundtrip_all_block_forms() {
    let mut kinds = std::collections::BTreeSet::new();
    for seed in 1..=64 {
        let length = usize::try_from(seed * 2137 % 90_001).unwrap();
        let mut source = random(length, seed);
        if seed % 3 != 0 {
            for value in &mut source {
                *value %= u8::try_from(seed % 29 + 1).unwrap();
            }
        }
        let cancellation = Cancellation::new();
        let mut writer = Writer::new(Vec::new(), &cancellation).unwrap();
        writer.write_all(&source).unwrap();
        let encoded = writer.finish().unwrap();
        kinds.insert((encoded[10] >> 1) & 3);
        verify(&source, &encoded);
    }
    let cancellation = Cancellation::new();
    let encoded = Writer::new(Vec::new(), &cancellation)
        .unwrap()
        .finish()
        .unwrap();
    kinds.insert((encoded[10] >> 1) & 3);
    assert_eq!(kinds, [0, 1, 2].into_iter().collect());
}

#[test]
fn sync_flush_exposes_all_input_and_preserves_dictionary() {
    let cancellation = Cancellation::new();
    let mut writer = Writer::new(Vec::new(), &cancellation).unwrap();
    let mut source = Vec::new();
    for length in [1, 2, 3, 257, 32_768, 65_535, 100_000] {
        let bytes = random(length, 811);
        source.extend_from_slice(&bytes);
        writer.write_all(&bytes).unwrap();
        writer.flush().unwrap();
        let mut partial = Vec::new();
        let error = flate2::read::GzDecoder::new(writer.get_ref().as_slice())
            .read_to_end(&mut partial)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(partial, source);
        let once = writer.get_ref().len();
        writer.flush().unwrap();
        assert_eq!(writer.get_ref().len(), once);
    }
    verify(&source, &writer.finish().unwrap());
}

struct ShortWriter {
    bytes: Vec<u8>,
    maximum: usize,
    flush_error: bool,
}

impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes
            .len()
            .min(7)
            .min(self.maximum.saturating_sub(self.bytes.len()));
        if count == 0 {
            return Err(io::ErrorKind::StorageFull.into());
        }
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.flush_error {
            Err(io::ErrorKind::StorageFull.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn destination_failures_are_terminal_without_drop_retries() {
    let source = random(131_080, 47);
    let cancellation = Cancellation::new();
    let mut destination = ShortWriter {
        bytes: Vec::new(),
        maximum: usize::MAX,
        flush_error: false,
    };
    let mut writer = Writer::new(&mut destination, &cancellation).unwrap();
    writer.write_all(&source).unwrap();
    writer.finish().unwrap();
    verify(&source, &destination.bytes);
    let length = destination.bytes.len();
    for maximum in [0, 3, 10, 17, 65_550, length - 8, length - 4, length - 1] {
        let mut destination = ShortWriter {
            bytes: Vec::new(),
            maximum,
            flush_error: false,
        };
        let mut writer = Writer::new(&mut destination, &cancellation).unwrap();
        let error = writer
            .write_all(&source)
            .and_then(|()| writer.try_finish())
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        let count = writer.get_ref().bytes.len();
        assert!(writer.write(b"x").is_err());
        assert!(writer.flush().is_err());
        assert!(writer.try_finish().is_err());
        drop(writer);
        assert_eq!(destination.bytes.len(), count);
    }
    let mut destination = ShortWriter {
        bytes: Vec::new(),
        maximum: usize::MAX,
        flush_error: true,
    };
    let mut writer = Writer::new(&mut destination, &cancellation).unwrap();
    writer.write_all(b"sample").unwrap();
    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert!(writer.try_finish().is_err());
}

#[test]
fn cancellation_prevents_finalization_even_with_buffered_input() {
    let cancellation = Cancellation::new();
    let mut output = Vec::new();
    let mut writer = Writer::new(&mut output, &cancellation).unwrap();
    writer.write_all(b"pending input").unwrap();
    let length = writer.get_ref().len();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(writer.write(b"x").is_err());
    assert!(writer.try_finish().is_err());
    drop(writer);
    assert_eq!(output.len(), length);
    assert!(Writer::new(Vec::new(), &cancellation).is_err());
}

#[test]
#[ignore = "requires a local gzip sample and an optimized build"]
fn measure_admitted_snapshot_encoding() {
    let path = std::env::var_os("ROOTLIGHT_GZIP_SAMPLE").expect("provide a local gzip sample");
    let file = std::fs::File::open(path).unwrap();
    assert!(file.metadata().unwrap().len() <= 256 * 1024 * 1024);
    let mut encoded = Vec::new();
    file.take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut encoded)
        .unwrap();
    assert!(encoded.len() <= 256 * 1024 * 1024);
    let mut source = Vec::new();
    flate2::read::GzDecoder::new(encoded.as_slice())
        .take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut source)
        .unwrap();
    assert!(source.len() <= 512 * 1024 * 1024);
    verify(&source, &encoded);
    let cancellation = Cancellation::new();
    let mut first_party = Vec::new();
    let mut reference = Vec::new();
    let mut first_party_bytes = 0;
    let mut reference_bytes = 0;
    let mut first_party_hash = None;
    for round in 0..6 {
        for own in [round % 2 == 0, round % 2 != 0] {
            let start = std::time::Instant::now();
            let output = if own {
                let mut writer = Writer::new(Vec::new(), &cancellation).unwrap();
                for chunk in source.chunks(65_536) {
                    writer.write_all(std::hint::black_box(chunk)).unwrap();
                }
                writer.flush().unwrap();
                writer.finish().unwrap()
            } else {
                let mut writer =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                for chunk in source.chunks(65_536) {
                    writer.write_all(std::hint::black_box(chunk)).unwrap();
                }
                writer.flush().unwrap();
                writer.finish().unwrap()
            };
            let micros = start.elapsed().as_micros();
            verify(&source, &output);
            if own {
                first_party.push(micros);
                first_party_bytes = output.len();
                let hash = blake3::hash(&output);
                if let Some(previous) = first_party_hash {
                    assert_eq!(previous, hash);
                }
                first_party_hash = Some(hash);
            } else {
                reference.push(micros);
                reference_bytes = output.len();
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "decoded_bytes": source.len(), "decoded_blake3": blake3::hash(&source).to_hex().to_string(),
            "input_blake3": blake3::hash(&encoded).to_hex().to_string(),
            "first_party_bytes": first_party_bytes, "reference_bytes": reference_bytes,
            "first_party_blake3": first_party_hash.unwrap().to_hex().to_string(),
            "first_party_micros": first_party, "reference_micros": reference,
        })
    );
}
