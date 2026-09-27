//! Single-member GZIP recovery IO over the shared first-party DEFLATE core.
//! Optional header fields are validated but never interpreted as filesystem paths.
//! EOF proves the exact admitted size, CRC and absence of trailing members/data.

#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use rootlight_cancel::Cancellation;
use std::{io, sync::LazyLock};

use super::{Error, deflate};

mod write;
pub(crate) use write::Writer;

#[cfg(test)]
mod tests;

static CRC_TABLES: LazyLock<[[u32; 256]; 8]> = LazyLock::new(|| {
    std::array::from_fn(|level| {
        let mut seed = 0_u32;
        std::array::from_fn(|_| {
            let mut value = seed;
            seed += 1;
            for _ in 0..8 * (level + 1) {
                value = (value >> 1) ^ if value & 1 == 0 { 0 } else { 0xedb8_8320 };
            }
            value
        })
    })
});

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid gzip recovery stream")
}

fn codec_error(error: Error) -> io::Error {
    match error {
        Error::Allocation => io::ErrorKind::OutOfMemory.into(),
        Error::Cancelled(cancelled) => io::Error::other(cancelled),
        error => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

fn update_crc(mut crc: u32, bytes: &[u8], cancellation: &Cancellation) -> io::Result<u32> {
    let tables = &*CRC_TABLES;
    for chunk in bytes.chunks(4096) {
        cancellation.check().map_err(io::Error::other)?;
        let mut words = chunk.chunks_exact(8);
        for word in &mut words {
            let &[a, b, c, d, e, f, g, h] = word else {
                return Err(invalid());
            };
            let [a, b, c, d] = (u32::from_le_bytes([a, b, c, d]) ^ crc).to_le_bytes();
            // Independent word contributions avoid eight serial CRC dependency steps.
            crc = 0;
            for (table, byte) in tables.iter().zip([h, g, f, e, d, c, b, a]) {
                crc ^= table.get(usize::from(byte)).ok_or_else(invalid)?;
            }
        }
        for byte in words.remainder() {
            let [low, ..] = crc.to_le_bytes();
            crc = (crc >> 8)
                ^ tables
                    .first()
                    .ok_or_else(invalid)?
                    .get(usize::from(low ^ byte))
                    .ok_or_else(invalid)?;
        }
    }
    Ok(crc)
}

fn terminated(bytes: &[u8], cursor: &mut usize, cancellation: &Cancellation) -> io::Result<()> {
    for chunk in bytes.get(*cursor..).ok_or_else(invalid)?.chunks(4096) {
        cancellation.check().map_err(io::Error::other)?;
        if let Some(end) = chunk.iter().position(|byte| *byte == 0) {
            *cursor += end + 1;
            return Ok(());
        }
        *cursor += chunk.len();
    }
    Err(invalid())
}

pub(crate) struct Reader<'a> {
    deflate: deflate::Reader<'a>,
    cancellation: &'a Cancellation,
    crc: u32,
    expected_crc: u32,
    finished: bool,
    failed: bool,
}

impl<'a> Reader<'a> {
    /// Open one admitted member without allocating its expanded payload.
    /// Malformed headers, trailer sizes and cancellation are returned as IO errors.
    pub(crate) fn new(
        input: &'a [u8],
        expected: usize,
        cancellation: &'a Cancellation,
    ) -> io::Result<Self> {
        cancellation.check().map_err(io::Error::other)?;
        let &[0x1f, 0x8b, 8, flags, _, _, _, _, _, _] = input.get(..10).ok_or_else(invalid)? else {
            return Err(invalid());
        };
        if flags & 0xe0 != 0 {
            return Err(invalid());
        }
        let end = input.len().checked_sub(8).ok_or_else(invalid)?;
        let (header_payload, trailer) = input.split_at_checked(end).ok_or_else(invalid)?;
        let expected_crc = u32::from_le_bytes(
            trailer
                .get(..4)
                .ok_or_else(invalid)?
                .try_into()
                .map_err(|_| invalid())?,
        );
        let size = u32::from_le_bytes(
            trailer
                .get(4..)
                .ok_or_else(invalid)?
                .try_into()
                .map_err(|_| invalid())?,
        );
        if u64::from(size) != u64::try_from(expected).map_err(|_| invalid())? % (1_u64 << 32) {
            return Err(invalid());
        }
        let mut cursor = 10_usize;
        if flags & 4 != 0 {
            let length = u16::from_le_bytes(
                header_payload
                    .get(cursor..cursor + 2)
                    .ok_or_else(invalid)?
                    .try_into()
                    .map_err(|_| invalid())?,
            );
            cursor = cursor
                .checked_add(2 + usize::from(length))
                .ok_or_else(invalid)?;
            header_payload.get(..cursor).ok_or_else(invalid)?;
        }
        if flags & 8 != 0 {
            terminated(header_payload, &mut cursor, cancellation)?;
        }
        if flags & 16 != 0 {
            terminated(header_payload, &mut cursor, cancellation)?;
        }
        if flags & 2 != 0 {
            let actual = !update_crc(
                u32::MAX,
                header_payload.get(..cursor).ok_or_else(invalid)?,
                cancellation,
            )?;
            let recorded = u16::from_le_bytes(
                header_payload
                    .get(cursor..cursor + 2)
                    .ok_or_else(invalid)?
                    .try_into()
                    .map_err(|_| invalid())?,
            );
            if u32::from(recorded) != actual & 0xffff {
                return Err(invalid());
            }
            cursor += 2;
        }
        let deflate = deflate::Reader::new(
            header_payload.get(cursor..).ok_or_else(invalid)?,
            expected,
            cancellation,
        )
        .map_err(codec_error)?;
        Ok(Self {
            deflate,
            cancellation,
            crc: u32::MAX,
            expected_crc,
            finished: false,
            failed: false,
        })
    }
}

impl io::Read for Reader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.cancellation.check().map_err(io::Error::other)?;
        if self.failed {
            return Err(invalid());
        }
        if output.is_empty() || self.finished {
            return Ok(0);
        }
        // Failed readers stay failed, including failures after writing caller
        // scratch space. Those bytes are never reported as a successful read.
        self.failed = true;
        let count = self.deflate.read(output).map_err(codec_error)?;
        self.crc = update_crc(
            self.crc,
            output.get(..count).ok_or_else(invalid)?,
            self.cancellation,
        )?;
        if count == 0 {
            if !self.crc != self.expected_crc {
                return Err(invalid());
            }
            self.finished = true;
        }
        self.failed = false;
        Ok(count)
    }
}
