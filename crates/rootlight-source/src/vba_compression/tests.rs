//! Authored compression records exercise exact outputs and hostile boundaries.
//! Fixtures are independent of Office repositories and never execute module code.

use super::*;
use rootlight_cancel::CancellationReason;

enum Token {
    Literal(u8),
    Copy(u16),
}

fn compressed(tokens: &[Token]) -> Vec<u8> {
    let mut payload = Vec::new();
    for group in tokens.chunks(8) {
        let flags = group.iter().enumerate().fold(0u8, |f, (i, token)| {
            f | if matches!(token, Token::Copy(_)) {
                1 << i
            } else {
                0
            }
        });
        payload.push(flags);
        for token in group {
            match token {
                Token::Literal(b) => payload.push(*b),
                Token::Copy(word) => payload.extend(word.to_le_bytes()),
            }
        }
    }
    assert!((1..=4096).contains(&payload.len()));
    let mut result = (0xb000 | u16::try_from(payload.len() - 1).unwrap())
        .to_le_bytes()
        .to_vec();
    result.extend(payload);
    result
}

fn raw(bytes: &[u8]) -> Vec<u8> {
    assert_eq!(bytes.len(), 4096);
    let mut result = vec![0xff, 0x3f];
    result.extend_from_slice(bytes);
    result
}

fn container(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![1];
    for chunk in chunks {
        bytes.extend(chunk);
    }
    bytes
}

fn decode(input: &[u8]) -> Result<DecompressedContainer, DecompressionError> {
    decompress(input, DecompressionLimits::default(), &Cancellation::new())
}

#[test]
fn literals_overlapping_copies_and_chunk_provenance_are_exact() {
    let first = compressed(&[
        Token::Literal(b'a'),
        Token::Literal(b'b'),
        Token::Literal(b'c'),
        Token::Copy(0x2ffa),
    ]);
    let second = compressed(&[Token::Literal(b'z'), Token::Copy(0x0001)]);
    let input = container(&[first.clone(), second.clone()]);
    let result = decode(&input).unwrap();
    let mut expected: Vec<_> = (0..4096).map(|i| b"abc"[i % 3]).collect();
    expected.extend_from_slice(b"zzzzz");
    assert_eq!(result.bytes(), expected);
    assert_eq!(result.chunks().len(), 2);
    assert_eq!(result.chunks()[0].encoded_range(), 1..1 + first.len());
    assert_eq!(result.chunks()[0].decoded_range(), 0..4096);
    assert_eq!(
        result.chunks()[1].encoded_range(),
        1 + first.len()..input.len()
    );
    assert_eq!(result.chunks()[1].decoded_range(), 4096..4101);
    assert_eq!(result.chunks()[0].encoding(), ChunkEncoding::Tokens);
}

#[test]
fn raw_chunks_and_binary_literals_preserve_every_byte() {
    let bytes: Vec<_> = (0..4096).map(|i| u8::try_from(i % 256).unwrap()).collect();
    let tail = compressed(&[Token::Literal(0), Token::Literal(255), Token::Literal(128)]);
    let result = decode(&container(&[raw(&bytes), tail])).unwrap();
    let mut expected = bytes.clone();
    expected.extend([0, 255, 128]);
    assert_eq!(result.bytes(), expected);
    assert_eq!(result.chunks()[0].encoding(), ChunkEncoding::Raw);
    assert_eq!(result.chunks()[0].encoded_range(), 1..4099);
    assert_eq!(result.chunks()[1].decoded_range(), 4096..4099);
    let empty = decode(&[1]).unwrap();
    assert!(empty.bytes().is_empty());
    assert!(empty.chunks().is_empty());
}

#[test]
fn copy_token_bit_splits_follow_each_power_of_two_boundary() {
    for count in [
        1, 2, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513,
        1023, 1024, 1025, 2047, 2048, 2049,
    ] {
        let prefix: Vec<_> = (0..count).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let mut tokens: Vec<_> = prefix.iter().copied().map(Token::Literal).collect();
        let mut window = 16;
        let mut length_bits = 12;
        while window < count {
            window *= 2;
            length_bits -= 1;
        }
        tokens.push(Token::Copy(
            u16::try_from((count - 1) << length_bits).unwrap(),
        ));
        let result = decode(&container(&[compressed(&tokens)])).unwrap();
        let mut expected = prefix.clone();
        for i in 0..3 {
            expected.push(prefix[i % count]);
        }
        assert_eq!(result.bytes(), expected, "history length {count}");
    }
    let result = decode(&[1, 5, 0xb0, 6, b'x', 0xf9, 0x0f, 0xc0, 0xff]).unwrap();
    assert_eq!(result.bytes(), vec![b'x'; 4096]);
}

#[test]
fn rejects_invalid_signatures_truncation_and_chunk_sizes() {
    for input in [&[][..], &[0], &[1, 0, 0], &[1, 0, 0x70]] {
        assert!(matches!(
            decode(input),
            Err(DecompressionError::Signature { .. })
        ));
    }
    let input = container(&[raw(&vec![0xa5; 4096])]);
    for end in 2..input.len() {
        assert!(
            matches!(
                decode(&input[..end]),
                Err(DecompressionError::Truncated { .. })
            ),
            "end {end}"
        );
    }
    assert!(matches!(
        decode(&[1, 0]),
        Err(DecompressionError::Truncated { offset: 1 })
    ));
    for input in [
        vec![1, 0, 0x30, 0],
        vec![1, 0, 0xb0, 0],
        vec![1, 1, 0xb0, 1, 0],
    ] {
        assert!(decode(&input).is_err());
    }
    let short = compressed(&[Token::Literal(b'x')]);
    assert!(matches!(
        decode(&container(&[short.clone(), short])),
        Err(DecompressionError::Invalid { offset: 1 })
    ));
}

#[test]
fn copy_tokens_cannot_cross_history_or_overflow_a_chunk() {
    let invalid = [
        compressed(&[Token::Copy(0)]),
        compressed(&[Token::Literal(b'x'), Token::Copy(0x1000)]),
        compressed(&[Token::Literal(b'x'), Token::Copy(0x0fff)]),
        compressed(&[
            Token::Literal(b'x'),
            Token::Copy(0x0ffc),
            Token::Literal(b'y'),
        ]),
    ];
    for chunk in invalid {
        assert!(matches!(
            decode(&container(&[chunk])),
            Err(DecompressionError::Invalid { .. })
        ));
    }
    let reset = container(&[raw(&vec![7; 4096]), compressed(&[Token::Copy(0)])]);
    assert!(matches!(
        decode(&reset),
        Err(DecompressionError::Invalid { .. })
    ));
}

#[test]
fn input_expansion_chunk_and_cancellation_limits_are_independent() {
    let chunk = compressed(&[Token::Literal(b'x'), Token::Copy(0x0ffc)]);
    let input = container(&[chunk.clone(), chunk]);
    for limits in [
        DecompressionLimits {
            input_bytes: input.len() - 1,
            ..DecompressionLimits::default()
        },
        DecompressionLimits {
            output_bytes: 8191,
            ..DecompressionLimits::default()
        },
        DecompressionLimits {
            chunks: 1,
            ..DecompressionLimits::default()
        },
    ] {
        assert!(matches!(
            decompress(&input, limits, &Cancellation::new()),
            Err(DecompressionError::Limit { .. })
        ));
    }
    let exact = DecompressionLimits {
        input_bytes: input.len(),
        output_bytes: 8192,
        chunks: 2,
    };
    assert_eq!(
        decompress(&input, exact, &Cancellation::new())
            .unwrap()
            .bytes()
            .len(),
        8192
    );
    for limits in [
        DecompressionLimits {
            input_bytes: usize::MAX,
            ..exact
        },
        DecompressionLimits {
            output_bytes: usize::MAX,
            ..exact
        },
        DecompressionLimits {
            chunks: usize::MAX,
            ..exact
        },
    ] {
        assert!(matches!(
            decompress(&input, limits, &Cancellation::new()),
            Err(DecompressionError::Limits)
        ));
    }
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        decompress(&input, exact, &cancel),
        Err(DecompressionError::Cancelled(_))
    ));
}

#[test]
fn terminal_flag_without_tokens_is_ignored_after_complete_groups() {
    let tokens: Vec<_> = b"abcdefgh".iter().copied().map(Token::Literal).collect();
    for flags in 0..=255 {
        let mut chunk = compressed(&tokens);
        chunk.push(flags);
        let header = 0xb000 | u16::try_from(chunk.len() - 3).unwrap();
        chunk[..2].copy_from_slice(&header.to_le_bytes());
        let input = container(&[chunk]);
        let result = decode(&input).unwrap();
        assert_eq!(result.bytes(), b"abcdefgh");
        assert_eq!(result.chunks()[0].encoded_range(), 1..input.len());
        assert!(matches!(
            decode(&[1, 0, 0xb0, flags]),
            Err(DecompressionError::Invalid { .. })
        ));
    }
}

#[test]
fn deterministic_mutations_preserve_bounded_complete_chunk_partitions() {
    let input = container(&[compressed(&[Token::Literal(b'x'), Token::Copy(0x0ffc)])]);
    for at in 0..input.len() {
        for replacement in 0..=255 {
            let mut mutated = input.clone();
            mutated[at] = replacement;
            if let Ok(result) = decode(&mutated) {
                let mut encoded = 1;
                let mut decoded = 0;
                for chunk in result.chunks() {
                    assert_eq!(chunk.encoded_range().start, encoded);
                    assert_eq!(chunk.decoded_range().start, decoded);
                    assert!((1..=4096).contains(&chunk.decoded_range().len()));
                    encoded = chunk.encoded_range().end;
                    decoded = chunk.decoded_range().end;
                }
                assert_eq!(encoded, mutated.len());
                assert_eq!(decoded, result.bytes().len());
            }
        }
    }
}
