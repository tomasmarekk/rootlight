//! Encoder invariants beyond end-to-end decoder interoperability.
//! These cases pin bounded scratch storage, maximal dictionary references and
//! the legal fallback for Huffman trees exceeding the format's depth limit.

use super::*;

#[test]
fn word_matches_equal_bytewise_matches_at_every_boundary() {
    let source: Vec<u8> = (0..600)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    for offset in 0..8 {
        for length in 0..=258 {
            let left = &source[offset..offset + length];
            for mismatch in 0..=length {
                let mut right = left.to_vec();
                if let Some(byte) = right.get_mut(mismatch) {
                    *byte ^= 128;
                }
                assert_eq!(matching_prefix(left, &right).unwrap(), mismatch);
                assert_eq!(matching_prefix(&right, left).unwrap(), mismatch);
            }
        }
    }
    for offset in 1..=32 {
        let repeated = vec![42; 300];
        let left = &repeated[..258];
        let right = &repeated[offset..offset + 258];
        assert_eq!(matching_prefix(left, right).unwrap(), 258);
        for length in 0..258 {
            assert_eq!(matching_prefix(&left[..length], right).unwrap(), length);
            assert_eq!(matching_prefix(left, &right[..length]).unwrap(), length);
        }
    }
}

#[test]
fn batched_dictionary_updates_preserve_every_hash_chain() {
    let mut input = Vec::new();
    let mut state = 193_u32;
    for _ in 0..BLOCK_BYTES {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        input.push(state.to_le_bytes()[0]);
    }
    for chunk in [1, 2, 3, 7, 258, 512, 32_768, BLOCK_BYTES] {
        let mut batched = Encoder::new().unwrap();
        batched.append(&input);
        let mut scalar = Encoder::new().unwrap();
        scalar.append(&input);
        for start in (0..input.len()).step_by(chunk) {
            let end = (start + chunk).min(input.len());
            batched.insert_range(start, end).unwrap();
            for position in start..end {
                scalar.insert(position).unwrap();
            }
        }
        assert_eq!(batched.heads, scalar.heads);
        assert_eq!(batched.previous, scalar.previous);
    }
    for a in 0_u8..=255 {
        for b in 0_u8..=255 {
            for c in [0, 1, 127, 255] {
                assert_eq!(
                    hash([a, b, c]),
                    ((usize::from(a) * 251 + usize::from(b)) * 251 + usize::from(c))
                        & (HASH_SLOTS - 1)
                );
            }
        }
    }
}

#[test]
fn dictionary_references_cross_blocks_at_the_maximum_distance() {
    let cancellation = Cancellation::new();
    let mut encoder = Encoder::new().unwrap();
    let mut state = 937_u32;
    let dictionary: Vec<u8> = (0..HISTORY_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect();
    encoder.append(&dictionary);
    encoder.block(false, &cancellation).unwrap();
    encoder.append(&dictionary);
    encoder.tokenize(&cancellation).unwrap();
    assert!(
        encoder.tokens.iter().any(|token| token.literal > 256
            && token.distance == 29
            && token.distance_extra == 8191)
    );
    assert!(encoder.tokens.iter().any(|token| token.literal == 285));
    let capacities = (
        encoder.input.capacity(),
        encoder.tokens.capacity(),
        encoder.bits.bytes.capacity(),
        encoder.previous.capacity(),
        encoder.heads.capacity(),
    );
    encoder.block(false, &cancellation).unwrap();
    let input = vec![b'x'; BLOCK_BYTES];
    for _ in 0..16 {
        assert_eq!(encoder.append(&input), BLOCK_BYTES);
        encoder.block(false, &cancellation).unwrap();
        assert_eq!(
            (
                encoder.input.capacity(),
                encoder.tokens.capacity(),
                encoder.bits.bytes.capacity(),
                encoder.previous.capacity(),
                encoder.heads.capacity()
            ),
            capacities
        );
    }
    let bytes = capacities.0
        + capacities.1 * size_of::<Token>()
        + capacities.2
        + (capacities.3 + capacities.4) * size_of::<u32>();
    assert!(bytes < 2 * 1024 * 1024);
}

#[test]
fn pathological_trees_fall_back_without_truncating_codes() {
    let mut frequencies = [0; 286];
    frequencies[0] = 1;
    frequencies[1] = 1;
    for symbol in 2..20 {
        frequencies[symbol] = frequencies[symbol - 1] + frequencies[symbol - 2];
    }
    assert!(frequencies.iter().sum::<u32>() < u32::try_from(BLOCK_BYTES).unwrap());
    assert!(dynamic_codes(&frequencies).unwrap().is_none());
    let complete = dynamic_codes(&[1; 286]).unwrap().unwrap();
    assert_eq!(
        complete
            .iter()
            .map(|code| 1_u32 << (15 - code.width))
            .sum::<u32>(),
        1 << 15
    );
    let single = dynamic_codes(&[0, 0, 1]).unwrap().unwrap();
    assert_eq!((single[2].bits, single[2].width), (0, 1));
}

#[test]
#[ignore = "requires a local gzip sample and an optimized build"]
fn profile_admitted_encoding_stages() {
    use std::{io::Read, time::Instant};
    let file = std::fs::File::open(std::env::var_os("ROOTLIGHT_GZIP_SAMPLE").unwrap()).unwrap();
    assert!(file.metadata().unwrap().len() <= 256 * 1024 * 1024);
    let mut source = Vec::new();
    flate2::read::GzDecoder::new(file.take(256 * 1024 * 1024 + 1))
        .take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut source)
        .unwrap();
    assert!(!source.is_empty() && source.len() <= 512 * 1024 * 1024);
    let cancellation = Cancellation::new();
    let mut observations = Vec::new();
    for _ in 0..3 {
        let mut encoder = Encoder::new().unwrap();
        let mut output = Vec::new();
        let mut tokenization = std::time::Duration::ZERO;
        let mut complete_blocks = std::time::Duration::ZERO;
        for (index, chunk) in source.chunks(BLOCK_BYTES).enumerate() {
            assert_eq!(encoder.append(chunk), chunk.len());
            // Re-tokenization rebuilds its dictionary from the same immutable
            // block. The second observation includes all normal encoding stages.
            let start = Instant::now();
            std::hint::black_box(encoder.tokenize(&cancellation).unwrap());
            tokenization += start.elapsed();
            let start = Instant::now();
            let bytes = encoder
                .block(
                    index + 1 == source.len().div_ceil(BLOCK_BYTES),
                    &cancellation,
                )
                .unwrap();
            complete_blocks += start.elapsed();
            output.extend_from_slice(bytes);
        }
        let mut decoded = Vec::new();
        flate2::read::DeflateDecoder::new(output.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, source);
        observations.push(serde_json::json!({
            "tokenization_micros": tokenization.as_micros(),
            "complete_blocks_micros": complete_blocks.as_micros(),
            "encoded_bytes": output.len(),
        }));
    }
    println!(
        "{}",
        serde_json::json!({ "decoded_bytes": source.len(), "stages": observations })
    );
}
