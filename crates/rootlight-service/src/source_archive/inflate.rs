//! First-party raw DEFLATE decoding for admitted immutable ZIP payloads.
//! RFC 1951 framing, canonical codes and bounded LZ77 copies are independent
//! of ZIP ownership and CRC validation, which remain in the archive reader.

#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use rootlight_cancel::Cancellation;

use super::ZipExpansionError as Error;

#[cfg(test)]
mod tests;

const LENGTH_BASE: [usize; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [usize; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct Bits<'a> {
    input: &'a [u8],
    cursor: usize,
    buffer: u32,
    available: u8,
}

impl Bits<'_> {
    fn offset(&self) -> usize {
        // Huffman lookahead may fetch whole bytes belonging to the next block
        // or forbidden trailing input; those bytes are not consumed yet.
        self.cursor - usize::from(self.available / 8)
    }

    fn invalid(&self) -> Error {
        Error::Deflate {
            input_offset: self.offset(),
        }
    }

    fn fill(&mut self, width: u8) {
        while self.available < width {
            let Some(&byte) = self.input.get(self.cursor) else {
                break;
            };
            self.buffer |= u32::from(byte) << self.available;
            self.available += 8;
            self.cursor += 1;
        }
    }

    fn take(&mut self, width: u8) -> Result<u16, Error> {
        self.fill(width);
        if self.available < width {
            return Err(self.invalid());
        }
        let value = self.buffer & ((1_u32 << width) - 1);
        self.buffer >>= width;
        self.available -= width;
        u16::try_from(value).map_err(|_| self.invalid())
    }

    fn stored(&mut self) -> Result<&[u8], Error> {
        self.take(self.available % 8)?;
        let size = self.take(16)?;
        if self.take(16)? != !size {
            return Err(self.invalid());
        }
        let start = self.offset();
        let end = start
            .checked_add(usize::from(size))
            .ok_or_else(|| self.invalid())?;
        let data = self.input.get(start..end).ok_or_else(|| self.invalid())?;
        self.cursor = end;
        self.available = 0;
        self.buffer = 0;
        Ok(data)
    }
}

struct Huffman {
    counts: [u16; 16],
    first: [u32; 16],
    offsets: [usize; 16],
    symbols: [u16; 288],
    short: [(u16, u8); 512],
}

enum Alphabet {
    Complete,
    Literals,
    Distances,
}

impl Huffman {
    fn new(lengths: &[u8], alphabet: Alphabet, bits: &Bits<'_>) -> Result<Self, Error> {
        if lengths.len() > 288 {
            return Err(bits.invalid());
        }
        let mut tree = Self {
            counts: [0; 16],
            first: [0; 16],
            offsets: [0; 16],
            symbols: [0; 288],
            short: [(0, 0); 512],
        };
        for &length in lengths {
            let count = tree
                .counts
                .get_mut(usize::from(length))
                .ok_or_else(|| bits.invalid())?;
            if length != 0 {
                *count += 1;
            }
        }
        let mut slots = 1_u32;
        let mut total = 0;
        for width in 1..16 {
            let count = u32::from(*tree.counts.get(width).ok_or_else(|| bits.invalid())?);
            slots = (slots * 2)
                .checked_sub(count)
                .ok_or_else(|| bits.invalid())?;
            let previous = *tree.first.get(width - 1).ok_or_else(|| bits.invalid())?;
            let previous_count = *tree.counts.get(width - 1).ok_or_else(|| bits.invalid())?;
            *tree.first.get_mut(width).ok_or_else(|| bits.invalid())? =
                (previous + u32::from(previous_count)) * 2;
            *tree.offsets.get_mut(width).ok_or_else(|| bits.invalid())? = total;
            total += usize::try_from(count).map_err(|_| bits.invalid())?;
        }
        // A one-bit singleton has one intentionally unused code. An empty
        // distance alphabet is legal only until a length/distance pair is read.
        let permitted_incomplete = match alphabet {
            Alphabet::Complete => false,
            Alphabet::Literals => total == 1 && tree.counts.get(1) == Some(&1),
            Alphabet::Distances => total == 0 || (total == 1 && tree.counts.get(1) == Some(&1)),
        };
        if slots != 0 && !permitted_incomplete {
            return Err(bits.invalid());
        }
        let mut next = tree.offsets;
        for (symbol, &length) in lengths
            .iter()
            .enumerate()
            .filter(|(_, length)| **length != 0)
        {
            let width = usize::from(length);
            let slot = next.get_mut(width).ok_or_else(|| bits.invalid())?;
            let offset = *tree.offsets.get(width).ok_or_else(|| bits.invalid())?;
            let code = tree.first.get(width).ok_or_else(|| bits.invalid())?
                + u32::try_from(*slot - offset).map_err(|_| bits.invalid())?;
            let symbol = u16::try_from(symbol).map_err(|_| bits.invalid())?;
            *tree.symbols.get_mut(*slot).ok_or_else(|| bits.invalid())? = symbol;
            *slot += 1;
            if length <= 9 {
                let prefix = usize::try_from(code.reverse_bits() >> (32 - length))
                    .map_err(|_| bits.invalid())?;
                for index in (prefix..512).step_by(1 << length) {
                    *tree.short.get_mut(index).ok_or_else(|| bits.invalid())? = (symbol, length);
                }
            }
        }
        Ok(tree)
    }

    fn symbol(&self, bits: &mut Bits<'_>) -> Result<u16, Error> {
        bits.fill(9);
        let index = usize::try_from(bits.buffer & 511).map_err(|_| bits.invalid())?;
        let &(symbol, length) = self.short.get(index).ok_or_else(|| bits.invalid())?;
        if length != 0 && length <= bits.available {
            bits.take(length)?;
            return Ok(symbol);
        }
        let mut code = 0_u32;
        for ((&first, &count), &offset) in self
            .first
            .iter()
            .zip(&self.counts)
            .zip(&self.offsets)
            .skip(1)
        {
            code = code * 2 + u32::from(bits.take(1)?);
            if let Some(index) = code.checked_sub(first)
                && index < u32::from(count)
            {
                let index = offset + usize::try_from(index).map_err(|_| bits.invalid())?;
                return self
                    .symbols
                    .get(index)
                    .copied()
                    .ok_or_else(|| bits.invalid());
            }
        }
        Err(bits.invalid())
    }
}

fn dynamic(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), Error> {
    let literals = usize::from(bits.take(5)?) + 257;
    let distances = usize::from(bits.take(5)?) + 1;
    let codes = usize::from(bits.take(4)?) + 4;
    if literals > 286 {
        return Err(bits.invalid());
    }
    let mut lengths = [0_u8; 19];
    for &index in LENGTH_ORDER.iter().take(codes) {
        *lengths.get_mut(index).ok_or_else(|| bits.invalid())? =
            u8::try_from(bits.take(3)?).map_err(|_| bits.invalid())?;
    }
    let code_tree = Huffman::new(&lengths, Alphabet::Complete, bits)?;
    let mut lengths = [0_u8; 318];
    let total = literals + distances;
    let mut used = 0;
    while used < total {
        let symbol = code_tree.symbol(bits)?;
        let (value, count) = match symbol {
            0..=15 => (u8::try_from(symbol).map_err(|_| bits.invalid())?, 1),
            16 => (
                *lengths
                    .get(used.checked_sub(1).ok_or_else(|| bits.invalid())?)
                    .ok_or_else(|| bits.invalid())?,
                usize::from(bits.take(2)?) + 3,
            ),
            17 => (0, usize::from(bits.take(3)?) + 3),
            18 => (0, usize::from(bits.take(7)?) + 11),
            _ => return Err(bits.invalid()),
        };
        let end = used + count;
        if end > total {
            return Err(bits.invalid());
        }
        lengths
            .get_mut(used..end)
            .ok_or_else(|| bits.invalid())?
            .fill(value);
        used = end;
    }
    if lengths.get(256) == Some(&0) {
        return Err(bits.invalid());
    }
    let lengths = lengths.get(..total).ok_or_else(|| bits.invalid())?;
    let (literal_lengths, distance_lengths) = lengths
        .split_at_checked(literals)
        .ok_or_else(|| bits.invalid())?;
    Ok((
        Huffman::new(literal_lengths, Alphabet::Literals, bits)?,
        Huffman::new(distance_lengths, Alphabet::Distances, bits)?,
    ))
}

fn compressed(
    bits: &mut Bits<'_>,
    literal: &Huffman,
    distance: &Huffman,
    output: &mut Vec<u8>,
    expected: usize,
    cancellation: &Cancellation,
) -> Result<(), Error> {
    let mut work = 0_u8;
    loop {
        if work == 0 {
            cancellation.check()?;
        }
        work = (work + 1) % 64;
        match literal.symbol(bits)? {
            byte @ 0..=255 => {
                if output.len() == expected {
                    return Err(Error::OutputSize);
                }
                output.push(u8::try_from(byte).map_err(|_| bits.invalid())?);
            }
            256 => return Ok(()),
            symbol @ 257..=285 => {
                let index = usize::from(symbol - 257);
                let base = *LENGTH_BASE.get(index).ok_or_else(|| bits.invalid())?;
                let width = *LENGTH_EXTRA.get(index).ok_or_else(|| bits.invalid())?;
                let length = base + usize::from(bits.take(width)?);
                let index = usize::from(distance.symbol(bits)?);
                let base = *DISTANCE_BASE.get(index).ok_or_else(|| bits.invalid())?;
                let width = *DISTANCE_EXTRA.get(index).ok_or_else(|| bits.invalid())?;
                let distance = base + usize::from(bits.take(width)?);
                let start = output
                    .len()
                    .checked_sub(distance)
                    .ok_or_else(|| bits.invalid())?;
                let end = output
                    .len()
                    .checked_add(length)
                    .filter(|end| *end <= expected)
                    .ok_or(Error::OutputSize)?;
                // Each copy uses only already-produced bytes. Growing the source
                // prefix preserves overlapping LZ77 matches without unsafe writes.
                while output.len() < end {
                    let count = (end - output.len()).min(output.len() - start);
                    output.extend_from_within(start..start + count);
                }
            }
            _ => return Err(bits.invalid()),
        }
    }
}

/// Decode one complete raw stream within the archive reader's admitted size.
/// No partial bytes escape on malformed framing, size mismatch or cancellation.
pub(super) fn inflate(
    input: &[u8],
    expected: usize,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, Error> {
    cancellation.check()?;
    let mut bits = Bits {
        input,
        cursor: 0,
        buffer: 0,
        available: 0,
    };
    let mut output = Vec::new();
    output
        .try_reserve_exact(expected)
        .map_err(|_| Error::Allocation)?;
    let mut fixed = None;
    loop {
        cancellation.check()?;
        let last = bits.take(1)? != 0;
        match bits.take(2)? {
            0 => {
                let source = bits.stored()?;
                if output
                    .len()
                    .checked_add(source.len())
                    .is_none_or(|end| end > expected)
                {
                    return Err(Error::OutputSize);
                }
                for chunk in source.chunks(super::DECODE_CHUNK_BYTES) {
                    cancellation.check()?;
                    output.extend_from_slice(chunk);
                }
            }
            kind @ (1 | 2) => {
                let dynamic;
                let (literal, distance) = if kind == 1 {
                    if fixed.is_none() {
                        let mut lengths = [8; 288];
                        lengths
                            .get_mut(144..256)
                            .ok_or_else(|| bits.invalid())?
                            .fill(9);
                        lengths
                            .get_mut(256..280)
                            .ok_or_else(|| bits.invalid())?
                            .fill(7);
                        fixed = Some((
                            Huffman::new(&lengths, Alphabet::Complete, &bits)?,
                            Huffman::new(&[5; 32], Alphabet::Complete, &bits)?,
                        ));
                    }
                    fixed.as_ref().ok_or_else(|| bits.invalid())?
                } else {
                    dynamic = self::dynamic(&mut bits)?;
                    &dynamic
                };
                compressed(
                    &mut bits,
                    literal,
                    distance,
                    &mut output,
                    expected,
                    cancellation,
                )?;
            }
            _ => return Err(bits.invalid()),
        }
        if last {
            if bits.offset() != input.len() {
                return Err(bits.invalid());
            }
            if output.len() != expected {
                return Err(Error::OutputSize);
            }
            cancellation.check()?;
            return Ok(output);
        }
    }
}
