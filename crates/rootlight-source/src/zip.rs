//! Bounded ZIP directory and payload-range validation for embedded source containers.
//! Entries borrow untrusted names and encoded bytes; no extraction, decompression,
//! path resolution or Office ownership is implied. Layout follows PKWARE APPNOTE 6.3.10.

use std::{collections::BTreeMap, fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

const MAX_ENTRIES: usize = 65_536;

/// Independent archive preparation limits, without enlarging source-read budgets.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ZipLimits {
    /// Maximum input bytes, capped by the existing source snapshot ceiling.
    pub input_bytes: usize,
    /// Maximum central directory records, including directory entries.
    pub entries: usize,
}

impl Default for ZipLimits {
    fn default() -> Self {
        Self {
            input_bytes: 16 * 1024 * 1024,
            entries: 4096,
        }
    }
}

/// A source-free description of invalid, unsupported or over-budget ZIP data.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ZipError {
    /// Record framing, redundant metadata or physical ranges disagree.
    #[error("invalid zip record at byte {offset}")]
    Invalid {
        /// Byte offset in the archive or nested record being decoded.
        offset: usize,
    },
    /// Split/spanned archives cannot be resolved from one source snapshot.
    #[error("split zip archives are unsupported")]
    Split,
    /// A supplied resource ceiling is invalid.
    #[error("invalid zip limits")]
    Limits,
    /// A caller-selected resource ceiling was exceeded.
    #[error("zip {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource label, never source content.
        resource: &'static str,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Stored-byte retrieval cannot decode this method or encrypted payload.
    #[error("zip payload requires unsupported decoding")]
    Encoding,
    /// Expanded bytes disagree with the declared size or CRC32.
    #[error("zip payload integrity mismatch")]
    Integrity,
    /// The operation was cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// One original directory record and its non-overlapping physical payload.
pub struct ZipEntry<'a> {
    name: &'a [u8],
    encoded: &'a [u8],
    flags: u16,
    method: u16,
    expanded_bytes: u64,
    crc32: u32,
    central: Range<usize>,
    local: Range<usize>,
    payload: Range<usize>,
    descriptor: Range<usize>,
}

impl fmt::Debug for ZipEntry<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZipEntry")
            .field("method", &self.method)
            .field("flags", &self.flags)
            .field("expanded_bytes", &self.expanded_bytes)
            .field("payload", &self.payload)
            .finish_non_exhaustive()
    }
}

impl<'a> ZipEntry<'a> {
    /// Returns exact untrusted name bytes, not a validated path or package URI.
    #[must_use]
    pub const fn name(&self) -> &'a [u8] {
        self.name
    }
    /// Returns the original flags, including filename encoding and encryption.
    #[must_use]
    pub const fn flags(&self) -> u16 {
        self.flags
    }
    /// Returns the wire compression method; zero is stored and eight is Deflate.
    #[must_use]
    pub const fn method(&self) -> u16 {
        self.method
    }
    /// Returns the declared expanded size, not permission to allocate that amount.
    #[must_use]
    pub const fn expanded_bytes(&self) -> u64 {
        self.expanded_bytes
    }
    /// Returns exact encoded bytes; callers must check flags and bound decoding.
    #[must_use]
    pub const fn encoded_bytes(&self) -> &'a [u8] {
        self.encoded
    }
    /// Returns the central record range, including its name, extras and comment.
    #[must_use]
    pub fn central_range(&self) -> Range<usize> {
        self.central.clone()
    }
    /// Returns the local header range, including its name and extras.
    #[must_use]
    pub fn local_range(&self) -> Range<usize> {
        self.local.clone()
    }
    /// Returns the original encoded payload range, not expanded byte offsets.
    #[must_use]
    pub fn payload_range(&self) -> Range<usize> {
        self.payload.clone()
    }
    /// Returns the trailing descriptor range, empty when no descriptor is present.
    #[must_use]
    pub fn descriptor_range(&self) -> Range<usize> {
        self.descriptor.clone()
    }

    /// Checks caller-decoded bytes against size and CRC32 within a bounded scan.
    ///
    /// CRC32 detects corruption; it is not authentication or proof of Office ownership.
    /// The caller must already have enforced the output limit while decoding.
    ///
    /// # Errors
    /// Returns a limit, integrity or cancellation error; never allocates or decodes.
    pub fn verify_expanded(
        &self,
        bytes: &[u8],
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<(), ZipError> {
        cancellation.check()?;
        if u64::try_from(maximum).map_err(|_| ZipError::Limits)? > crate::HARD_MAX_SNAPSHOT_BYTES {
            return Err(ZipError::Limits);
        }
        if self.expanded_bytes > u64::try_from(maximum).map_err(|_| ZipError::Limits)?
            || bytes.len() > maximum
        {
            return Err(ZipError::Limit {
                resource: "expanded bytes",
                maximum,
            });
        }
        if u64::try_from(bytes.len()).map_err(|_| ZipError::Integrity)? != self.expanded_bytes
            || crc32(bytes, cancellation)? != self.crc32
        {
            return Err(ZipError::Integrity);
        }
        Ok(())
    }

    /// Returns integrity-checked stored bytes without copying or activating content.
    ///
    /// # Errors
    /// Rejects compression/encryption, mismatched integrity, exceeded limits or cancellation.
    pub fn stored_bytes(
        &self,
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<&'a [u8], ZipError> {
        if self.method != 0 || self.flags & !(8 | 0x0800) != 0 {
            return Err(ZipError::Encoding);
        }
        self.verify_expanded(self.encoded, maximum, cancellation)?;
        Ok(self.encoded)
    }
}

/// Validated single-disk directory with bounded allocation and borrowed payloads.
#[derive(Debug)]
pub struct ZipArchive<'a> {
    entries: Vec<ZipEntry<'a>>,
    central: Range<usize>,
}

impl<'a> ZipArchive<'a> {
    /// Validates ZIP/ZIP64 records, local bindings, descriptors and physical separation.
    ///
    /// Duplicate names and unknown methods remain distinct entries, not silently selected
    /// or discarded. Extra-field framing is checked; unknown values remain opaque.
    /// Payload CRC checks require [`ZipEntry::verify_expanded`] or stored-byte retrieval.
    ///
    /// # Errors
    /// Returns malformed/split archive, resource or cancellation errors. No I/O occurs.
    pub fn open(
        input: &'a [u8],
        limits: ZipLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, ZipError> {
        cancellation.check()?;
        let hard = usize::try_from(crate::HARD_MAX_SNAPSHOT_BYTES).map_err(|_| ZipError::Limits)?;
        if limits.input_bytes == 0
            || limits.input_bytes > hard
            || limits.entries == 0
            || limits.entries > MAX_ENTRIES
        {
            return Err(ZipError::Limits);
        }
        if input.len() > limits.input_bytes {
            return Err(ZipError::Limit {
                resource: "input bytes",
                maximum: limits.input_bytes,
            });
        }
        let (central, count) = directory(input, cancellation)?;
        if count > limits.entries {
            return Err(ZipError::Limit {
                resource: "entries",
                maximum: limits.entries,
            });
        }
        let mut cursor = central.start;
        let mut entries = Vec::new();
        let mut occupied = BTreeMap::new();
        for _ in 0..count {
            cancellation.check()?;
            let entry = entry(input, cursor, &central, cancellation)?;
            let start = entry.local.start;
            let end = entry.descriptor.end;
            if occupied
                .range(..=start)
                .next_back()
                .is_some_and(|(_, previous_end)| *previous_end > start)
                || occupied
                    .range(start..)
                    .next()
                    .is_some_and(|(next_start, _)| *next_start < end)
            {
                return Err(ZipError::Invalid { offset: start });
            }
            occupied.insert(start, end);
            cursor = entry.central.end;
            entries.push(entry);
        }
        // A central signature is retained as opaque framing, never as verified trust.
        if cursor < central.end && number(input, cursor, 4)? == 0x0505_4b50 {
            cursor = add(cursor, add(6, size(number(input, add(cursor, 4)?, 2)?)?)?)?;
        }
        if cursor != central.end {
            return Err(ZipError::Invalid { offset: cursor });
        }
        cancellation.check()?;
        Ok(Self { entries, central })
    }
    /// Returns all records in original central directory order, including duplicates.
    #[must_use]
    pub fn entries(&self) -> &[ZipEntry<'a>] {
        &self.entries
    }
    /// Returns the full central directory range in the supplied source.
    #[must_use]
    pub fn central_range(&self) -> Range<usize> {
        self.central.clone()
    }
}

fn add(left: usize, right: usize) -> Result<usize, ZipError> {
    left.checked_add(right)
        .ok_or(ZipError::Invalid { offset: left })
}
fn size(value: u64) -> Result<usize, ZipError> {
    usize::try_from(value).map_err(|_| ZipError::Invalid { offset: 0 })
}
fn slice(input: &[u8], start: usize, length: usize) -> Result<&[u8], ZipError> {
    input
        .get(start..add(start, length)?)
        .ok_or(ZipError::Invalid { offset: start })
}
fn number(input: &[u8], start: usize, length: usize) -> Result<u64, ZipError> {
    let mut result = 0;
    for (index, byte) in slice(input, start, length)?.iter().enumerate() {
        result |= u64::from(*byte) << (index * 8);
    }
    Ok(result)
}

fn directory(input: &[u8], cancellation: &Cancellation) -> Result<(Range<usize>, usize), ZipError> {
    let last = input
        .len()
        .checked_sub(22)
        .ok_or(ZipError::Invalid { offset: 0 })?;
    let mut eocd = None;
    for offset in (last.saturating_sub(65_535)..=last).rev() {
        if offset % 256 == 0 {
            cancellation.check()?;
        }
        if number(input, offset, 4)? == 0x0605_4b50
            && add(add(offset, 22)?, size(number(input, offset + 20, 2)?)?)? == input.len()
            && eocd.replace(offset).is_some()
        {
            return Err(ZipError::Invalid { offset });
        }
    }
    let end = eocd.ok_or(ZipError::Invalid { offset: last })?;
    let read = |field, length| number(input, end + field, length);
    if read(4, 2)? != 0 || read(6, 2)? != 0 {
        return Err(ZipError::Split);
    }
    let count = read(10, 2)?;
    let length = read(12, 4)?;
    let start = read(16, 4)?;
    let locator = end
        .checked_sub(20)
        .filter(|offset| number(input, *offset, 4).ok() == Some(0x0706_4b50));
    let (start, length, count, boundary) = if let Some(locator) = locator {
        if number(input, locator + 4, 4)? != 0 || number(input, locator + 16, 4)? != 1 {
            return Err(ZipError::Split);
        }
        let zip64 = size(number(input, locator + 8, 8)?)?;
        if number(input, zip64, 4)? != 0x0606_4b50 {
            return Err(ZipError::Invalid { offset: zip64 });
        }
        let record_size = size(number(input, add(zip64, 4)?, 8)?)?;
        if record_size < 44 || add(zip64, add(12, record_size)?)? != locator {
            return Err(ZipError::Invalid { offset: zip64 });
        }
        if number(input, zip64 + 16, 4)? != 0 || number(input, zip64 + 20, 4)? != 0 {
            return Err(ZipError::Split);
        }
        let full_count = number(input, zip64 + 32, 8)?;
        let full_length = number(input, zip64 + 40, 8)?;
        let full_start = number(input, zip64 + 48, 8)?;
        if number(input, zip64 + 24, 8)? != full_count {
            return Err(ZipError::Split);
        }
        for (short, full, sentinel) in [
            (count, full_count, 0xffff),
            (read(8, 2)?, full_count, 0xffff),
            (length, full_length, 0xffff_ffff),
            (start, full_start, 0xffff_ffff),
        ] {
            if short != sentinel && short != full {
                return Err(ZipError::Invalid { offset: end });
            }
        }
        (full_start, full_length, full_count, zip64)
    } else {
        if count == 0xffff || length == 0xffff_ffff || start == 0xffff_ffff {
            return Err(ZipError::Invalid { offset: end });
        }
        if read(8, 2)? != count {
            return Err(ZipError::Split);
        }
        (start, length, count, end)
    };
    let start = size(start)?;
    let stop = add(start, size(length)?)?;
    if stop != boundary || stop > input.len() {
        return Err(ZipError::Invalid { offset: start });
    }
    Ok((start..stop, size(count)?))
}

fn extra_zip64<'a>(
    extra: &'a [u8],
    cancellation: &Cancellation,
) -> Result<Option<&'a [u8]>, ZipError> {
    let mut cursor = 0;
    let mut zip64 = None;
    while cursor < extra.len() {
        cancellation.check()?;
        let tag = number(extra, cursor, 2)?;
        let length = size(number(extra, add(cursor, 2)?, 2)?)?;
        let value = slice(extra, add(cursor, 4)?, length)?;
        if tag == 1 && zip64.replace(value).is_some() {
            return Err(ZipError::Invalid { offset: cursor });
        }
        cursor = add(cursor, add(4, length)?)?;
    }
    Ok(zip64)
}

fn expand(
    value: u64,
    sentinel: u64,
    extra: Option<&[u8]>,
    cursor: &mut usize,
    width: usize,
) -> Result<u64, ZipError> {
    if value != sentinel {
        return Ok(value);
    }
    let result = number(
        extra.ok_or(ZipError::Invalid { offset: *cursor })?,
        *cursor,
        width,
    )?;
    *cursor = add(*cursor, width)?;
    Ok(result)
}

fn entry<'a>(
    input: &'a [u8],
    start: usize,
    central: &Range<usize>,
    cancellation: &Cancellation,
) -> Result<ZipEntry<'a>, ZipError> {
    let directory = input.get(..central.end).ok_or(ZipError::Invalid {
        offset: central.end,
    })?;
    let fixed = slice(directory, start, 46)?;
    let read = |field, length| number(fixed, field, length);
    if read(0, 4)? != 0x0201_4b50 {
        return Err(ZipError::Invalid { offset: start });
    }
    let name_start = add(start, 46)?;
    let name = slice(directory, name_start, size(read(28, 2)?)?)?;
    let extra_start = add(name_start, name.len())?;
    let extra = slice(directory, extra_start, size(read(30, 2)?)?)?;
    let end = add(add(extra_start, extra.len())?, size(read(32, 2)?)?)?;
    slice(directory, start, end - start)?;
    let zip64 = extra_zip64(extra, cancellation)?;
    let mut position = 0;
    let expanded_bytes = expand(read(24, 4)?, 0xffff_ffff, zip64, &mut position, 8)?;
    let compressed = expand(read(20, 4)?, 0xffff_ffff, zip64, &mut position, 8)?;
    let local = size(expand(read(42, 4)?, 0xffff_ffff, zip64, &mut position, 8)?)?;
    if expand(read(34, 2)?, 0xffff, zip64, &mut position, 4)? != 0 {
        return Err(ZipError::Split);
    }
    let body = input.get(..central.start).ok_or(ZipError::Invalid {
        offset: central.start,
    })?;
    let local_fixed = slice(body, local, 30)?;
    let local_read = |field, length| number(local_fixed, field, length);
    if local_read(0, 4)? != 0x0403_4b50
        || local_read(6, 2)? != read(8, 2)?
        || local_read(8, 2)? != read(10, 2)?
        || local_read(4, 2)? != read(6, 2)?
    {
        return Err(ZipError::Invalid { offset: local });
    }
    let local_name = slice(body, add(local, 30)?, size(local_read(26, 2)?)?)?;
    if local_name != name {
        return Err(ZipError::Invalid { offset: local });
    }
    let local_extra_start = add(add(local, 30)?, local_name.len())?;
    let local_extra = slice(body, local_extra_start, size(local_read(28, 2)?)?)?;
    let local_zip64 = extra_zip64(local_extra, cancellation)?;
    let mut position = 0;
    let local_expanded = expand(
        local_read(22, 4)?,
        0xffff_ffff,
        local_zip64,
        &mut position,
        8,
    )?;
    let local_compressed = expand(
        local_read(18, 4)?,
        0xffff_ffff,
        local_zip64,
        &mut position,
        8,
    )?;
    let flags = u16::try_from(read(8, 2)?).map_err(|_| ZipError::Invalid { offset: start })?;
    let crc = read(16, 4)?;
    let descriptor_present = flags & 8 != 0;
    for (local_value, declared) in [
        (local_expanded, expanded_bytes),
        (local_compressed, compressed),
        (local_read(14, 4)?, crc),
    ] {
        if local_value != declared && !(descriptor_present && local_value == 0) {
            return Err(ZipError::Invalid { offset: local });
        }
    }
    let data_start = add(local_extra_start, local_extra.len())?;
    let data_end = add(data_start, size(compressed)?)?;
    let encoded = slice(body, data_start, size(compressed)?)?;
    let descriptor_end = if descriptor_present {
        descriptor(
            body,
            data_end,
            crc,
            compressed,
            expanded_bytes,
            zip64.is_some() || local_zip64.is_some(),
        )?
    } else {
        data_end
    };
    Ok(ZipEntry {
        name,
        encoded,
        flags,
        method: u16::try_from(read(10, 2)?).map_err(|_| ZipError::Invalid { offset: start })?,
        expanded_bytes,
        crc32: u32::try_from(crc).map_err(|_| ZipError::Invalid { offset: start })?,
        central: start..end,
        local: local..data_start,
        payload: data_start..data_end,
        descriptor: data_end..descriptor_end,
    })
}

fn descriptor(
    input: &[u8],
    start: usize,
    crc: u64,
    compressed: u64,
    expanded: u64,
    zip64: bool,
) -> Result<usize, ZipError> {
    let width = if zip64 { 8 } else { 4 };
    let mut end = None;
    for prefix in [0, 4] {
        if prefix == 4 && number(input, start, 4).ok() != Some(0x0807_4b50) {
            continue;
        }
        let data = add(start, prefix)?;
        if number(input, data, 4).ok() == Some(crc)
            && number(input, add(data, 4)?, width).ok() == Some(compressed)
            && number(input, add(data, 4 + width)?, width).ok() == Some(expanded)
        {
            let candidate = add(data, 4 + 2 * width)?;
            if end.replace(candidate).is_some() {
                return Err(ZipError::Invalid { offset: start });
            }
        }
    }
    end.ok_or(ZipError::Invalid { offset: start })
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut index = 0;
    let mut seed = 0_u32;
    while index < 256 {
        let mut value = seed;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                (value >> 1) ^ 0xedb8_8320
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
        seed += 1;
    }
    table
};

fn crc32(bytes: &[u8], cancellation: &Cancellation) -> Result<u32, ZipError> {
    let mut crc = u32::MAX;
    for chunk in bytes.chunks(4096) {
        cancellation.check()?;
        for byte in chunk {
            let index = usize::from(crc.to_le_bytes()[0] ^ byte);
            crc = (crc >> 8) ^ CRC_TABLE[index];
        }
    }
    Ok(!crc)
}

#[cfg(test)]
mod tests;
