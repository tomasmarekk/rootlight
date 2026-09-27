//! Encoder invariants beyond end-to-end decoder interoperability.
//! These cases pin bounded scratch storage, maximal dictionary references and
//! the legal fallback for Huffman trees exceeding the format's depth limit.

use super::*;

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
