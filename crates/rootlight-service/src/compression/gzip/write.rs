//! Streaming single-member GZIP output for transactional recovery publication.
//! Finishing is explicit: errors poison the writer, and dropping it never
//! finalizes the member. The caller retains responsibility for its destination.

#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use rootlight_cancel::Cancellation;
use std::io::{self, Write};

use super::{super::encode, update_crc};

#[cfg(test)]
mod tests;

/// A bounded GZIP writer with explicit completion and terminal IO failures.
pub(crate) struct Writer<'a, W> {
    inner: W,
    encoder: encode::Encoder,
    cancellation: &'a Cancellation,
    crc: u32,
    size: u32,
    header: bool,
    dirty: bool,
    finished: bool,
    failed: bool,
}

impl<'a, W: Write> Writer<'a, W> {
    /// Reserve fixed scratch space without writing to the destination.
    /// Cancellation and allocation failures are returned as IO errors.
    pub(crate) fn new(inner: W, cancellation: &'a Cancellation) -> io::Result<Self> {
        cancellation.check().map_err(io::Error::other)?;
        Ok(Self {
            inner,
            encoder: encode::Encoder::new()?,
            cancellation,
            crc: u32::MAX,
            size: 0,
            header: false,
            dirty: false,
            finished: false,
            failed: false,
        })
    }

    /// Inspect the destination's error/accounting state without bypassing encoding.
    pub(crate) fn get_ref(&self) -> &W {
        &self.inner
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        if let Err(cancelled) = self.cancellation.check() {
            self.failed = true;
            return Err(io::Error::other(cancelled));
        }
        if self.failed {
            return Err(io::Error::other("gzip writer previously failed"));
        }
        Ok(())
    }

    fn header(&mut self) -> io::Result<()> {
        if !self.header {
            // No timestamp, filename or host identifier enters a reproducible snapshot.
            self.inner
                .write_all(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255])?;
            self.header = true;
        }
        Ok(())
    }

    /// Emit the final block and CRC/size once. Failed output cannot be retried;
    /// the caller must discard its staging artifact. Does not sync the destination.
    pub(crate) fn try_finish(&mut self) -> io::Result<()> {
        self.checkpoint()?;
        if self.finished {
            return Ok(());
        }
        self.failed = true;
        self.header()?;
        self.inner
            .write_all(self.encoder.block(true, self.cancellation)?)?;
        self.inner.write_all(&(!self.crc).to_le_bytes())?;
        self.inner.write_all(&self.size.to_le_bytes())?;
        self.cancellation.check().map_err(io::Error::other)?;
        self.finished = true;
        self.failed = false;
        Ok(())
    }

    /// Finish the member and return its destination, or preserve the IO failure.
    pub(crate) fn finish(mut self) -> io::Result<W> {
        self.try_finish()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for Writer<'_, W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.checkpoint()?;
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "gzip writer is finished",
            ));
        }
        if input.is_empty() {
            return Ok(0);
        }
        self.failed = true;
        self.header()?;
        if self.encoder.pending() == encode::BLOCK_BYTES {
            self.inner
                .write_all(self.encoder.block(false, self.cancellation)?)?;
        }
        let count = input
            .len()
            .min(encode::BLOCK_BYTES - self.encoder.pending());
        let bytes = input
            .get(..count)
            .ok_or_else(|| io::Error::other("invalid gzip input length"))?;
        self.crc = update_crc(self.crc, bytes, self.cancellation)?;
        let count = self.encoder.append(bytes);
        // RFC 1952 ISIZE is the uncompressed byte count modulo 2^32.
        self.size = self
            .size
            .wrapping_add(u32::try_from(count).map_err(|_| io::ErrorKind::InvalidInput)?);
        self.dirty = true;
        self.failed = false;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.checkpoint()?;
        self.failed = true;
        self.header()?;
        if !self.finished && self.dirty {
            self.inner
                .write_all(self.encoder.block(false, self.cancellation)?)?;
            self.inner.write_all(self.encoder.sync()?)?;
            self.dirty = false;
        }
        self.inner.flush()?;
        self.failed = false;
        Ok(())
    }
}
