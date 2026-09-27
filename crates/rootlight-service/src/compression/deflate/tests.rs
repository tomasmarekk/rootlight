//! Authored framing/codebook cases and differential checks use an independent
//! test-only codec; production decoding never delegates to that implementation.

use super::*;
use flate2::{Compression, Decompress, FlushDecompress, Status, write::DeflateEncoder};
use std::io::Write;

#[derive(Default)]
struct Stream {
    bytes: Vec<u8>,
    used: usize,
}

impl Stream {
    fn bits(&mut self, value: u16, width: usize) {
        for index in 0..width {
            if self.used.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let last = self.bytes.last_mut().unwrap();
            *last |= u8::try_from((value >> index) & 1).unwrap() << (self.used % 8);
            self.used += 1;
        }
    }

    fn code(&mut self, value: u16, width: usize) {
        self.bits(value.reverse_bits() >> (16 - width), width);
    }

    fn fixed(&mut self, symbol: u16) {
        let (code, width) = match symbol {
            0..=143 => (48 + symbol, 8),
            144..=255 => (400 + symbol - 144, 9),
            256..=279 => (symbol - 256, 7),
            280..=287 => (192 + symbol - 280, 8),
            _ => panic!("invalid fixture symbol"),
        };
        self.code(code, width);
    }

    fn stored(&mut self, source: &[u8], last: bool) {
        self.bits(u16::from(last), 3);
        while !self.used.is_multiple_of(8) {
            self.bits(1, 1);
        }
        let length = u16::try_from(source.len()).unwrap();
        self.bits(length, 16);
        self.bits(!length, 16);
        for byte in source {
            self.bits(u16::from(*byte), 8);
        }
    }

    fn dynamic(&mut self, literals: &[u8], distances: &[u8]) {
        self.bits(5, 3);
        self.bits(u16::try_from(literals.len() - 257).unwrap(), 5);
        self.bits(u16::try_from(distances.len() - 1).unwrap(), 5);
        self.bits(14, 4);
        // Code-length alphabet: 0 -> 0, 1 -> 10, 2 -> 11. The wire order is
        // written explicitly so a production permutation error cannot mask it.
        for width in [0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2] {
            self.bits(width, 3);
        }
        for length in literals.iter().chain(distances) {
            match length {
                0 => self.code(0, 1),
                1 => self.code(2, 2),
                2 => self.code(3, 2),
                _ => panic!("invalid fixture code length"),
            }
        }
    }
}

fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let cancellation = Cancellation::new();
    let decoded = inflate(input, expected, &cancellation);
    let streamed = (|| {
        let mut reader = Reader::new(input, expected, &cancellation)?;
        let mut output = Vec::new();
        let mut buffer = [0; 97];
        loop {
            let count = reader.read(&mut buffer)?;
            assert!(reader.window.capacity() <= 65_536 + 258);
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
        }
        Ok::<_, Error>(output)
    })();
    assert_eq!(decoded.as_ref().ok(), streamed.as_ref().ok());
    decoded
}

fn reference(input: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut codec = Decompress::new(false);
    let mut output = vec![0; expected + 1];
    let status = codec
        .decompress(input, &mut output, FlushDecompress::Finish)
        .ok()?;
    if status != Status::StreamEnd
        || codec.total_in() != u64::try_from(input.len()).unwrap()
        || codec.total_out() != u64::try_from(expected).unwrap()
    {
        return None;
    }
    output.truncate(expected);
    Some(output)
}

fn encoded(source: &[u8], level: Compression) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), level);
    encoder.write_all(source).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn fixed_overlap_and_maximum_distance_span_block_boundaries() {
    let prefix: Vec<_> = (0..32_768_usize)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    let mut stream = Stream::default();
    stream.stored(&prefix, false);
    stream.bits(2, 3);
    stream.fixed(257);
    stream.code(29, 5);
    stream.bits(8191, 13);
    stream.fixed(65);
    stream.fixed(285);
    stream.code(0, 5);
    stream.fixed(256);
    // The preceding end code and short-code lookahead can prefetch this block.
    stream.stored(b"end", true);
    let mut expected = prefix.clone();
    expected.extend_from_slice(&prefix[..3]);
    expected.extend_from_slice(&[65; 259]);
    expected.extend_from_slice(b"end");
    assert_eq!(reference(&stream.bytes, expected.len()).unwrap(), expected);
    assert_eq!(decode(&stream.bytes, expected.len()).unwrap(), expected);
}

#[test]
fn dynamic_singleton_and_absent_distance_alphabets_are_contextual() {
    let mut lengths = vec![0; 257];
    lengths[256] = 1;
    let mut empty = Stream::default();
    empty.dynamic(&lengths, &[0]);
    empty.code(0, 1);
    assert_eq!(reference(&empty.bytes, 0), Some(Vec::new()));
    assert_eq!(decode(&empty.bytes, 0).unwrap(), b"");

    lengths[65] = 1;
    lengths[256] = 2;
    lengths.push(2);
    for distances in [&[0][..], &[1][..]] {
        let mut stream = Stream::default();
        stream.dynamic(&lengths, distances);
        stream.code(0, 1);
        stream.code(3, 2);
        stream.code(0, 1);
        stream.code(2, 2);
        if distances[0] == 0 {
            assert!(decode(&stream.bytes, 4).is_err());
            assert!(reference(&stream.bytes, 4).is_none());
        } else {
            assert_eq!(decode(&stream.bytes, 4).unwrap(), b"AAAA");
            assert_eq!(reference(&stream.bytes, 4).unwrap(), b"AAAA");
        }
    }
    let mut missing_end = Stream::default();
    missing_end.dynamic(&[0; 257], &[0]);
    assert!(decode(&missing_end.bytes, 0).is_err());
}

#[test]
fn dynamic_repeats_cross_the_literal_distance_boundary() {
    let mut stream = Stream::default();
    stream.bits(5, 3);
    stream.bits(1, 5);
    stream.bits(3, 5);
    stream.bits(14, 4);
    // Six complete code-length symbols: 0/1 use two bits; 2/16/17/18
    // use three. Literal 65 has length 1; 256/257 and all distances use 2.
    for width in [3, 3, 3, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 2] {
        stream.bits(width, 3);
    }
    stream.code(7, 3);
    stream.bits(54, 7);
    stream.code(1, 2);
    stream.code(7, 3);
    stream.bits(127, 7);
    stream.code(7, 3);
    stream.bits(38, 7);
    stream.code(6, 3);
    stream.bits(0, 3);
    stream.code(4, 3);
    stream.code(5, 3);
    stream.bits(2, 2);
    stream.code(0, 1);
    stream.code(3, 2);
    stream.code(0, 2);
    stream.code(2, 2);
    assert_eq!(reference(&stream.bytes, 4).unwrap(), b"AAAA");
    assert_eq!(decode(&stream.bytes, 4).unwrap(), b"AAAA");
}

#[test]
fn canonical_trees_reject_invalid_shapes_and_decode_fifteen_bit_codes() {
    let mut bits = Bits {
        input: &[],
        cursor: 0,
        buffer: 0,
        available: 0,
    };
    for lengths in [&[1, 1, 1][..], &[2, 2][..], &[16][..], &[][..]] {
        assert!(Huffman::new(lengths, Alphabet::Complete, &bits).is_err());
    }
    assert!(Huffman::new(&[2], Alphabet::Literals, &bits).is_err());
    assert!(Huffman::new(&[1], Alphabet::Literals, &bits).is_ok());
    assert!(Huffman::new(&[0], Alphabet::Distances, &bits).is_ok());
    let lengths: Vec<_> = (1..=15).chain([15]).collect();
    let tree = Huffman::new(&lengths, Alphabet::Complete, &bits).unwrap();
    let mut stream = Stream::default();
    for width in 1..=15 {
        stream.code((1 << width) - 2, width);
    }
    stream.code(32767, 15);
    bits.input = &stream.bytes;
    for symbol in 0..16 {
        assert_eq!(tree.symbol(&mut bits).unwrap(), symbol);
    }
}

#[test]
fn malformed_blocks_reserved_codes_and_impossible_distances_are_rejected() {
    for bytes in [&[7][..], &[1, 1, 0, 0, 0, 65][..], &[5, 31, 0][..]] {
        assert!(decode(bytes, 1).is_err());
    }
    for symbol in [286, 287] {
        let mut stream = Stream::default();
        stream.bits(3, 3);
        stream.fixed(symbol);
        assert!(decode(&stream.bytes, 3).is_err());
    }
    for distance in [0, 30, 31] {
        let mut stream = Stream::default();
        stream.bits(3, 3);
        stream.fixed(257);
        stream.code(distance, 5);
        stream.fixed(256);
        assert!(decode(&stream.bytes, 3).is_err());
    }
    for repeated in [16, 18] {
        let mut stream = Stream::default();
        stream.bits(5, 3);
        stream.bits(0, 5);
        stream.bits(0, 5);
        stream.bits(0, 4);
        for symbol in [16, 17, 18, 0] {
            stream.bits(u16::from(symbol == 0 || symbol == repeated), 3);
        }
        stream.code(1, 1);
        stream.bits(127, 7);
        stream.code(1, 1);
        stream.bits(127, 7);
        assert!(decode(&stream.bytes, 0).is_err());
    }
}

#[test]
fn deterministic_generated_streams_match_independent_codec() {
    let mut random = 0x3d70_e54a_u32;
    for case in 0..96 {
        let length = match case % 6 {
            0 => case,
            1 => 257 + case,
            2 => 32_768 + case,
            3 => 65_536 + case,
            4 => 129 * case,
            _ => 4096 + case,
        };
        let source: Vec<_> = (0..length)
            .map(|index| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                match case % 4 {
                    0 => u8::try_from(random & 255).unwrap(),
                    1 => u8::try_from(random & 7).unwrap(),
                    2 => u8::try_from(index % 251).unwrap(),
                    _ => u8::try_from(index / 258 % 2).unwrap(),
                }
            })
            .collect();
        for level in [
            Compression::none(),
            Compression::fast(),
            Compression::best(),
        ] {
            let input = encoded(&source, level);
            assert_eq!(
                decode(&input, length).unwrap(),
                source,
                "case {case}, {level:?}"
            );
        }
    }
}

#[test]
fn mutated_streams_never_publish_different_bytes_from_reference() {
    let source = b"bounded canonical codes, repeated repeated repeated source bytes";
    for level in [
        Compression::none(),
        Compression::fast(),
        Compression::best(),
    ] {
        let input = encoded(source, level);
        for offset in 0..input.len() {
            for bit in 0..8 {
                let mut mutated = input.clone();
                mutated[offset] ^= 1 << bit;
                assert_eq!(
                    decode(&mutated, source.len()).ok(),
                    reference(&mutated, source.len()),
                    "offset {offset}, bit {bit}, {level:?}"
                );
            }
        }
        for length in 0..input.len() {
            assert!(decode(&input[..length], source.len()).is_err());
        }
    }
}
