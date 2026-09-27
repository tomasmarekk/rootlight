//! Bounded first-party DEFLATE encoding for streaming recovery snapshots.
//! Blocks retain the preceding 32 KiB dictionary and select stored, fixed or
//! dynamic coding by bit cost; no input-sized allocation or external codec is used.

#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use rootlight_cancel::Cancellation;
use std::io;

use super::deflate::{DISTANCE_BASE, DISTANCE_EXTRA, LENGTH_BASE, LENGTH_EXTRA, LENGTH_ORDER};

#[cfg(test)]
mod tests;

pub(super) const BLOCK_BYTES: usize = 65_535;
const HISTORY_BYTES: usize = 32_768;
const INPUT_BYTES: usize = HISTORY_BYTES + BLOCK_BYTES;
const HASH_SLOTS: usize = 16_384;
const SEARCH_STEPS: usize = 8;
const NONE: u32 = u32::MAX;

fn invalid() -> io::Error {
    io::Error::other("invalid deflate encoder state")
}

fn reserved<T>(capacity: usize) -> io::Result<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| io::ErrorKind::OutOfMemory)?;
    Ok(values)
}

fn matching_prefix(left: &[u8], right: &[u8]) -> io::Result<usize> {
    let mut matched = 0;
    // Match discovery dominates block encoding. Word comparisons preserve the
    // bytewise result, including unaligned and overlapping dictionary slices.
    for (left, right) in left.as_chunks::<8>().0.iter().zip(right.as_chunks::<8>().0) {
        let difference = u64::from_le_bytes(*left) ^ u64::from_le_bytes(*right);
        if difference != 0 {
            return Ok(matched
                + usize::try_from(difference.trailing_zeros() / 8).map_err(|_| invalid())?);
        }
        matched += 8;
    }
    Ok(matched
        + left
            .get(matched..)
            .ok_or_else(invalid)?
            .iter()
            .zip(right.get(matched..).ok_or_else(invalid)?)
            .take_while(|(left, right)| left == right)
            .count())
}

fn hash([a, b, c]: [u8; 3]) -> usize {
    (usize::from(a) * 63_001 + usize::from(b) * 251 + usize::from(c)) & (HASH_SLOTS - 1)
}

#[derive(Clone, Copy, Default)]
struct Code {
    bits: u16,
    width: u8,
}

fn codes<const N: usize>(lengths: [u8; N]) -> io::Result<[Code; N]> {
    let mut counts = [0_u16; 16];
    for width in lengths.iter().copied().filter(|width| *width != 0) {
        *counts.get_mut(usize::from(width)).ok_or_else(invalid)? += 1;
    }
    let mut next = [0_u16; 16];
    let mut value = 0;
    for width in 1..16 {
        value = (value + counts.get(width - 1).ok_or_else(invalid)?) << 1;
        *next.get_mut(width).ok_or_else(invalid)? = value;
    }
    let mut result = [Code::default(); N];
    for (slot, width) in result.iter_mut().zip(lengths) {
        if width != 0 {
            let value = next.get_mut(usize::from(width)).ok_or_else(invalid)?;
            *slot = Code {
                bits: value.reverse_bits() >> (16 - width),
                width,
            };
            *value += 1;
        }
    }
    Ok(result)
}

fn dynamic_codes<const N: usize>(frequencies: &[u32; N]) -> io::Result<Option<[Code; N]>> {
    // The alphabet is bounded by RFC 1951. Two sorted queues build the tree
    // without per-block heap allocations or input-dependent recursion.
    let mut leaves = [0_usize; 288];
    let mut weights = [0_u32; 575];
    let mut parents = [usize::MAX; 575];
    let mut count = 0;
    for (symbol, &frequency) in frequencies.iter().enumerate() {
        if frequency != 0 {
            *leaves.get_mut(count).ok_or_else(invalid)? = symbol;
            count += 1;
        }
    }
    if count == 0 {
        let mut lengths = [0; N];
        *lengths.first_mut().ok_or_else(invalid)? = 1;
        return codes(lengths).map(Some);
    }
    let leaves = leaves.get_mut(..count).ok_or_else(invalid)?;
    leaves.sort_unstable_by_key(|symbol| (frequencies.get(*symbol).copied().unwrap_or(0), *symbol));
    for (slot, &symbol) in weights.iter_mut().zip(leaves.iter()) {
        *slot = *frequencies.get(symbol).ok_or_else(invalid)?;
    }
    let mut leaf = 0;
    let mut branch = count;
    let mut created = count;
    while created < count * 2 - 1 {
        let mut total = 0;
        for _ in 0..2 {
            let index = if leaf < count
                && (branch == created
                    || weights.get(leaf).ok_or_else(invalid)?
                        <= weights.get(branch).ok_or_else(invalid)?)
            {
                let index = leaf;
                leaf += 1;
                index
            } else {
                let index = branch;
                branch += 1;
                index
            };
            total += weights.get(index).ok_or_else(invalid)?;
            *parents.get_mut(index).ok_or_else(invalid)? = created;
        }
        *weights.get_mut(created).ok_or_else(invalid)? = total;
        created += 1;
    }
    let mut lengths = [0_u8; N];
    for (index, &symbol) in leaves.iter().enumerate() {
        let mut width = 0_u8;
        let mut parent = index;
        while let Some(&next) = parents.get(parent).filter(|next| **next != usize::MAX) {
            width += 1;
            if width > 15 {
                // Fixed/stored blocks remain valid when an exceptionally skewed
                // distribution needs a longer tree than the format permits.
                return Ok(None);
            }
            parent = next;
        }
        *lengths.get_mut(symbol).ok_or_else(invalid)? = width.max(1);
    }
    codes(lengths).map(Some)
}

#[derive(Clone, Copy)]
struct Token {
    literal: u16,
    length_extra: u16,
    distance: u8,
    distance_extra: u16,
}

struct Bits {
    bytes: Vec<u8>,
    value: u32,
    width: u8,
}

impl Bits {
    fn put(&mut self, value: u16, width: u8) -> io::Result<()> {
        if width > 16 || u32::from(value) >= 1_u32 << width {
            return Err(invalid());
        }
        self.value |= u32::from(value) << self.width;
        self.width += width;
        while self.width >= 8 {
            if self.bytes.len() >= BLOCK_BYTES + 16 {
                return Err(invalid());
            }
            let [byte, ..] = self.value.to_le_bytes();
            self.bytes.push(byte);
            self.value >>= 8;
            self.width -= 8;
        }
        Ok(())
    }

    fn align(&mut self) -> io::Result<()> {
        self.put(0, (8 - self.width) % 8)
    }

    fn symbol(&mut self, code: Code) -> io::Result<()> {
        if code.width == 0 {
            return Err(invalid());
        }
        self.put(code.bits, code.width)
    }
}

/// Reusable bounded block encoder; callers drain each emitted block before reuse.
pub(super) struct Encoder {
    input: Vec<u8>,
    history: usize,
    heads: Vec<u32>,
    previous: Vec<u32>,
    tokens: Vec<Token>,
    bits: Bits,
}

impl Encoder {
    /// Allocate fixed-size scratch storage, reporting allocation failure as IO.
    pub(super) fn new() -> io::Result<Self> {
        let mut heads = reserved(HASH_SLOTS)?;
        heads.resize(HASH_SLOTS, NONE);
        let mut previous = reserved(INPUT_BYTES)?;
        previous.resize(INPUT_BYTES, NONE);
        Ok(Self {
            input: reserved(INPUT_BYTES)?,
            history: 0,
            heads,
            previous,
            tokens: reserved(BLOCK_BYTES)?,
            bits: Bits {
                bytes: reserved(BLOCK_BYTES + 16)?,
                value: 0,
                width: 0,
            },
        })
    }

    /// Append at most one block's remaining space and return the accepted length.
    pub(super) fn append(&mut self, bytes: &[u8]) -> usize {
        let count = bytes.len().min(BLOCK_BYTES - self.pending());
        self.input.extend(bytes.iter().take(count));
        count
    }

    /// Return unencoded input bytes, excluding the retained dictionary.
    pub(super) fn pending(&self) -> usize {
        self.input.len() - self.history
    }

    fn insert(&mut self, position: usize) -> io::Result<u32> {
        let Some(&[a, b, c]) = self.input.get(position..position + 3) else {
            return Ok(NONE);
        };
        let head = self.heads.get_mut(hash([a, b, c])).ok_or_else(invalid)?;
        let old = *head;
        *self.previous.get_mut(position).ok_or_else(invalid)? = old;
        *head = u32::try_from(position).map_err(|_| invalid())?;
        Ok(old)
    }

    fn insert_range(&mut self, start: usize, end: usize) -> io::Result<()> {
        let input = self
            .input
            .get(start..end.saturating_add(2).min(self.input.len()))
            .ok_or_else(invalid)?;
        let previous = self.previous.get_mut(start..end).ok_or_else(invalid)?;
        let heads: &mut [u32; HASH_SLOTS] = self
            .heads
            .as_mut_slice()
            .try_into()
            .map_err(|_| invalid())?;
        // Preserve insertion order while checking contiguous dictionary bounds
        // once per range, rather than for every byte in a copied match.
        for ((bytes, previous), position) in input.windows(3).zip(previous).zip(start..end) {
            let &[a, b, c] = bytes else {
                return Err(invalid());
            };
            let head = heads.get_mut(hash([a, b, c])).ok_or_else(invalid)?;
            *previous = *head;
            *head = u32::try_from(position).map_err(|_| invalid())?;
        }
        Ok(())
    }

    fn tokenize(&mut self, cancellation: &Cancellation) -> io::Result<([u32; 286], [u32; 30])> {
        self.heads.fill(NONE);
        self.tokens.clear();
        for position in (0..self.history).step_by(512) {
            cancellation.check().map_err(io::Error::other)?;
            self.insert_range(position, (position + 512).min(self.history))?;
        }
        let mut literals = [0_u32; 286];
        let mut distances = [0_u32; 30];
        let mut position = self.history;
        while position < self.input.len() {
            if self.tokens.len().is_multiple_of(256) {
                cancellation.check().map_err(io::Error::other)?;
            }
            let mut candidate = self.insert(position)?;
            let mut length = 2;
            let mut distance = 0;
            let maximum = (self.input.len() - position).min(258);
            let current = self
                .input
                .get(position..position + maximum)
                .ok_or_else(invalid)?;
            for _ in 0..SEARCH_STEPS {
                if candidate == NONE || maximum < 3 {
                    break;
                }
                let prior = usize::try_from(candidate).map_err(|_| invalid())?;
                if prior >= position || position - prior > HISTORY_BYTES {
                    break;
                }
                let matching = self.input.get(prior..prior + maximum).ok_or_else(invalid)?;
                if matching.get(length) == current.get(length) {
                    let found = matching_prefix(matching, current)?;
                    if found > length {
                        length = found;
                        distance = position - prior;
                        if found == maximum {
                            break;
                        }
                    }
                }
                candidate = *self.previous.get(prior).ok_or_else(invalid)?;
            }
            let token = if distance != 0 {
                let length_code = LENGTH_BASE.partition_point(|base| *base <= length) - 1;
                let distance_code = DISTANCE_BASE.partition_point(|base| *base <= distance) - 1;
                *distances.get_mut(distance_code).ok_or_else(invalid)? += 1;
                self.insert_range(position + 1, position + length)?;
                position += length;
                Token {
                    literal: u16::try_from(257 + length_code).map_err(|_| invalid())?,
                    length_extra: u16::try_from(
                        length - LENGTH_BASE.get(length_code).ok_or_else(invalid)?,
                    )
                    .map_err(|_| invalid())?,
                    distance: u8::try_from(distance_code).map_err(|_| invalid())?,
                    distance_extra: u16::try_from(
                        distance - DISTANCE_BASE.get(distance_code).ok_or_else(invalid)?,
                    )
                    .map_err(|_| invalid())?,
                }
            } else {
                let byte = *self.input.get(position).ok_or_else(invalid)?;
                position += 1;
                Token {
                    literal: u16::from(byte),
                    length_extra: 0,
                    distance: 0,
                    distance_extra: 0,
                }
            };
            *literals
                .get_mut(usize::from(token.literal))
                .ok_or_else(invalid)? += 1;
            self.tokens.push(token);
        }
        *literals.get_mut(256).ok_or_else(invalid)? = 1;
        Ok((literals, distances))
    }

    fn emit(
        &mut self,
        literals: &[Code],
        distances: &[Code],
        cancellation: &Cancellation,
    ) -> io::Result<()> {
        for (index, token) in self.tokens.iter().enumerate() {
            if index.is_multiple_of(512) {
                cancellation.check().map_err(io::Error::other)?;
            }
            self.bits.symbol(
                *literals
                    .get(usize::from(token.literal))
                    .ok_or_else(invalid)?,
            )?;
            if token.literal > 256 {
                self.bits.put(
                    token.length_extra,
                    *LENGTH_EXTRA
                        .get(usize::from(token.literal - 257))
                        .ok_or_else(invalid)?,
                )?;
                self.bits.symbol(
                    *distances
                        .get(usize::from(token.distance))
                        .ok_or_else(invalid)?,
                )?;
                self.bits.put(
                    token.distance_extra,
                    *DISTANCE_EXTRA
                        .get(usize::from(token.distance))
                        .ok_or_else(invalid)?,
                )?;
            }
        }
        self.bits.symbol(*literals.get(256).ok_or_else(invalid)?)
    }

    /// Encode pending bytes, optionally ending the stream. Returned bytes stay
    /// valid until the next call. Cancellation can leave the encoder unusable.
    pub(super) fn block(
        &mut self,
        final_block: bool,
        cancellation: &Cancellation,
    ) -> io::Result<&[u8]> {
        cancellation.check().map_err(io::Error::other)?;
        self.bits.bytes.clear();
        let (literal_counts, distance_counts) = self.tokenize(cancellation)?;
        let fixed_literals = codes(std::array::from_fn::<_, 288, _>(|symbol| match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        }))?;
        let fixed_distances = codes([5; 32])?;
        let dynamic = dynamic_codes(&literal_counts)?.zip(dynamic_codes(&distance_counts)?);
        let cost = |literals: &[Code], distances: &[Code]| -> io::Result<usize> {
            let mut bits = 0_usize;
            for (count, code) in literal_counts
                .iter()
                .zip(literals)
                .chain(distance_counts.iter().zip(distances))
            {
                bits += usize::try_from(*count).map_err(|_| invalid())? * usize::from(code.width);
            }
            for (count, width) in literal_counts
                .iter()
                .skip(257)
                .zip(LENGTH_EXTRA)
                .chain(distance_counts.iter().zip(DISTANCE_EXTRA))
            {
                bits += usize::try_from(*count).map_err(|_| invalid())? * usize::from(width);
            }
            Ok(bits)
        };
        let fixed_cost = 3 + cost(&fixed_literals, &fixed_distances)?;
        let stored_cost =
            3 + (8 - (usize::from(self.bits.width) + 3) % 8) % 8 + 32 + self.pending() * 8;
        let mut dynamic_cost = usize::MAX;
        let mut literal_count = 257;
        let mut distance_count = 1;
        if let Some((literals, distances)) = &dynamic {
            literal_count = literals
                .iter()
                .rposition(|code| code.width != 0)
                .map_or(257, |index| (index + 1).max(257));
            distance_count = distances
                .iter()
                .rposition(|code| code.width != 0)
                .map_or(1, |index| index + 1);
            dynamic_cost =
                3 + 14 + 19 * 3 + (literal_count + distance_count) * 4 + cost(literals, distances)?;
        }
        if stored_cost <= fixed_cost.min(dynamic_cost) {
            self.bits.put(u16::from(final_block), 3)?;
            self.bits.align()?;
            let count = u16::try_from(self.pending()).map_err(|_| invalid())?;
            self.bits.put(count, 16)?;
            self.bits.put(!count, 16)?;
            self.bits
                .bytes
                .extend_from_slice(self.input.get(self.history..).ok_or_else(invalid)?);
        } else if dynamic_cost < fixed_cost {
            let (literals, distances) = dynamic.ok_or_else(invalid)?;
            self.bits.put(u16::from(final_block) | 4, 3)?;
            self.bits.put(
                u16::try_from(literal_count - 257).map_err(|_| invalid())?,
                5,
            )?;
            self.bits
                .put(u16::try_from(distance_count - 1).map_err(|_| invalid())?, 5)?;
            // A complete four-bit code-length alphabet avoids recursive tree
            // construction; its bounded header is included in block selection.
            self.bits.put(15, 4)?;
            for symbol in LENGTH_ORDER {
                self.bits.put(if symbol < 16 { 4 } else { 0 }, 3)?;
            }
            for code in literals
                .iter()
                .take(literal_count)
                .chain(distances.iter().take(distance_count))
            {
                self.bits
                    .put(u16::from(code.width).reverse_bits() >> 12, 4)?;
            }
            self.emit(&literals, &distances, cancellation)?;
        } else {
            self.bits.put(u16::from(final_block) | 2, 3)?;
            self.emit(&fixed_literals, &fixed_distances, cancellation)?;
        }
        if final_block {
            self.bits.align()?;
        }
        self.history = self.input.len().min(HISTORY_BYTES);
        let start = self.input.len() - self.history;
        self.input.copy_within(start.., 0);
        self.input.truncate(self.history);
        Ok(&self.bits.bytes)
    }

    /// Emit a byte-aligned empty stored block for an explicit streaming flush.
    pub(super) fn sync(&mut self) -> io::Result<&[u8]> {
        self.bits.bytes.clear();
        self.bits.put(0, 3)?;
        self.bits.align()?;
        self.bits.put(0, 16)?;
        self.bits.put(u16::MAX, 16)?;
        Ok(&self.bits.bytes)
    }
}
