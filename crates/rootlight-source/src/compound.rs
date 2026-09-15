//! Bounded, non-executing reads of MS-CFB storage and stream objects.
//! Directory identities and physical byte ranges are retained independently of
//! Office project semantics, decompression, code pages and repository indexing.
//! The wire layout follows Microsoft's MS-CFB specification, sections 2.2–2.6.

use std::{fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

const FREE: u32 = 0xffff_ffff;
const END: u32 = 0xffff_fffe;
const FAT: u32 = 0xffff_fffd;
const DIFAT: u32 = 0xffff_fffc;
const MAX_ENTRIES: usize = 65_536;
const MAX_WORK: usize = 16 * 1024 * 1024;

/// Per-container preparation limits; these do not enlarge source-read budgets.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompoundLimits {
    /// Maximum retained input bytes, capped by the source snapshot ceiling.
    pub input_bytes: usize,
    /// Maximum directory slots, including unused slots in directory sectors.
    pub directory_entries: usize,
    /// Maximum allocation cells, directory links and stream sectors inspected.
    pub work_units: usize,
}

impl Default for CompoundLimits {
    fn default() -> Self {
        Self {
            input_bytes: 16 * 1024 * 1024,
            directory_entries: 4096,
            work_units: 2 * 1024 * 1024,
        }
    }
}

/// A preparation or extraction failure, without repository source payloads.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompoundError {
    /// The input is not a supported compound-file header.
    #[error("invalid compound file header")]
    Header,
    /// An allocation chain is truncated, cyclic, aliased or out of bounds.
    #[error("invalid compound file allocation")]
    Allocation,
    /// Directory identity, containment or a stream descriptor is invalid.
    #[error("invalid compound file directory")]
    Directory,
    /// The supplied limit exceeds a hard ceiling or admits no preparation work.
    #[error("invalid compound file limits")]
    Limits,
    /// The operation exceeded a caller-selected resource limit.
    #[error("compound file {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource label, never untrusted source text.
        resource: &'static str,
        /// Configured resource ceiling.
        maximum: usize,
    },
    /// The selected directory ID is absent or is not a stream object.
    #[error("compound file entry is not a stream")]
    NotStream,
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// Kind of an allocated CFB directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundEntryKind {
    /// Root storage, which also describes the mini-stream allocation.
    Root,
    /// A nested storage containing directory children.
    Storage,
    /// A readable byte stream.
    Stream,
}

/// One allocated entry with a file-local directory identity.
#[derive(Debug)]
pub struct CompoundEntry {
    id: u32,
    name: String,
    kind: CompoundEntryKind,
    parent: Option<u32>,
    directory_offset: usize,
    start: u32,
    length: usize,
    left: u32,
    right: u32,
    child: u32,
}

impl CompoundEntry {
    /// Returns the directory ID in this compound file, not a repository file ID.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }
    /// Returns the exact decoded UTF-16 name; callers must treat it as untrusted.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the entry's storage or stream role.
    #[must_use]
    pub const fn kind(&self) -> CompoundEntryKind {
        self.kind
    }
    /// Returns the containing storage ID, or `None` for the root.
    #[must_use]
    pub const fn parent(&self) -> Option<u32> {
        self.parent
    }
    /// Returns the original 128-byte directory descriptor range.
    #[must_use]
    pub fn directory_range(&self) -> Range<usize> {
        self.directory_offset..self.directory_offset + 128
    }
    /// Returns the declared byte length; storage objects report zero.
    #[must_use]
    pub const fn byte_length(&self) -> usize {
        self.length
    }
}

/// One exact mapping from extracted bytes to the retained container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompoundSourcePart {
    container: Range<usize>,
    stream: Range<usize>,
}

impl CompoundSourcePart {
    /// Returns the physical range in the original compound-file input.
    #[must_use]
    pub fn container_range(&self) -> Range<usize> {
        self.container.clone()
    }
    /// Returns the matching range in the extracted stream.
    #[must_use]
    pub fn stream_range(&self) -> Range<usize> {
        self.stream.clone()
    }
}

/// Exact stream bytes and a complete, ordered physical-source mapping.
pub struct CompoundStream {
    bytes: Vec<u8>,
    parts: Vec<CompoundSourcePart>,
}

impl fmt::Debug for CompoundStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompoundStream")
            .field("bytes", &self.bytes.len())
            .field("parts", &self.parts.len())
            .finish()
    }
}

impl CompoundStream {
    /// Returns exact bytes, without decoding, decompression or execution.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Returns mappings that partition the stream bytes without gaps.
    #[must_use]
    pub fn source_parts(&self) -> &[CompoundSourcePart] {
        &self.parts
    }
}

/// A borrowed CFB input with validated allocation and directory containment.
///
/// Preparation rejects cycles, shared sectors and unreachable allocated entries.
/// It does not validate writer-only metadata, red-black balancing or name-order
/// conformance. Entry IDs avoid guessing the format's Unicode name comparison;
/// higher-level consumers must establish unique project/stream identities.
pub struct CompoundFile<'a> {
    bytes: &'a [u8],
    sector_size: usize,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_stream: Vec<u32>,
    entries: Vec<Option<CompoundEntry>>,
}

impl fmt::Debug for CompoundFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompoundFile")
            .field("bytes", &self.bytes.len())
            .field("directory_slots", &self.entries.len())
            .finish()
    }
}

struct Work<'a> {
    left: usize,
    maximum: usize,
    cancellation: &'a Cancellation,
}

impl Work<'_> {
    fn step(&mut self) -> Result<(), CompoundError> {
        self.cancellation.check()?;
        self.left = self.left.checked_sub(1).ok_or(CompoundError::Limit {
            resource: "work",
            maximum: self.maximum,
        })?;
        Ok(())
    }
}

fn word(bytes: &[u8], offset: usize) -> Result<u32, CompoundError> {
    let end = offset.checked_add(4).ok_or(CompoundError::Allocation)?;
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or(CompoundError::Allocation)?
            .try_into()
            .map_err(|_| CompoundError::Allocation)?,
    ))
}

fn short(bytes: &[u8], offset: usize) -> Result<u16, CompoundError> {
    let end = offset.checked_add(2).ok_or(CompoundError::Header)?;
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or(CompoundError::Header)?
            .try_into()
            .map_err(|_| CompoundError::Header)?,
    ))
}

fn index(id: u32) -> Result<usize, CompoundError> {
    usize::try_from(id).map_err(|_| CompoundError::Allocation)
}

fn sector_start(id: u32, size: usize) -> Result<usize, CompoundError> {
    index(id)?
        .checked_add(1)
        .and_then(|n| n.checked_mul(size))
        .ok_or(CompoundError::Allocation)
}

fn sector(bytes: &[u8], id: u32, size: usize) -> Result<&[u8], CompoundError> {
    let start = sector_start(id, size)?;
    bytes
        .get(start..start.checked_add(size).ok_or(CompoundError::Allocation)?)
        .ok_or(CompoundError::Allocation)
}

fn claim(id: u32, used: &mut [bool]) -> Result<(), CompoundError> {
    let slot = used.get_mut(index(id)?).ok_or(CompoundError::Allocation)?;
    if *slot {
        return Err(CompoundError::Allocation);
    }
    *slot = true;
    Ok(())
}

fn chain(
    start: u32,
    table: &[u32],
    used: &mut [bool],
    expected: Option<usize>,
    maximum: usize,
    work: &mut Work<'_>,
) -> Result<Vec<u32>, CompoundError> {
    let mut result = Vec::new();
    let mut current = start;
    while current != END {
        work.step()?;
        if result.len() >= maximum || expected.is_some_and(|n| result.len() >= n) {
            return Err(CompoundError::Allocation);
        }
        claim(current, used)?;
        result.push(current);
        current = *table
            .get(index(current)?)
            .ok_or(CompoundError::Allocation)?;
    }
    if expected.is_some_and(|n| n != result.len()) {
        return Err(CompoundError::Allocation);
    }
    Ok(result)
}

impl<'a> CompoundFile<'a> {
    /// Prepares directory and allocation identities without copying stream bytes.
    ///
    /// # Errors
    /// Returns a typed error for malformed input, exhausted limits or cancellation.
    /// No filesystem, network, COM activation or macro execution occurs.
    ///
    /// # Examples
    /// ```
    /// use rootlight_cancel::Cancellation;
    /// use rootlight_source::compound::{CompoundError, CompoundFile, CompoundLimits};
    ///
    /// let cancellation = Cancellation::new();
    /// let result = CompoundFile::parse(b"not a container", CompoundLimits::default(), &cancellation);
    /// assert!(matches!(result, Err(CompoundError::Header)));
    /// ```
    pub fn parse(
        bytes: &'a [u8],
        limits: CompoundLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, CompoundError> {
        cancellation.check()?;
        if u64::try_from(limits.input_bytes).map_err(|_| CompoundError::Limits)?
            > crate::HARD_MAX_SNAPSHOT_BYTES
            || limits.directory_entries > MAX_ENTRIES
            || limits.work_units > MAX_WORK
            || limits.directory_entries == 0
            || limits.work_units == 0
        {
            return Err(CompoundError::Limits);
        }
        if bytes.len() > limits.input_bytes {
            return Err(CompoundError::Limit {
                resource: "input bytes",
                maximum: limits.input_bytes,
            });
        }
        if bytes.get(..8) != Some(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1])
            || bytes.len() < 512
        {
            return Err(CompoundError::Header);
        }
        let version = short(bytes, 26)?;
        let size = match (version, short(bytes, 30)?) {
            (3, 9) => 512,
            (4, 12) => 4096,
            _ => return Err(CompoundError::Header),
        };
        if short(bytes, 28)? != 0xfffe
            || short(bytes, 32)? != 6
            || word(bytes, 56)? != 4096
            || bytes.get(8..24).is_none_or(|s| s.iter().any(|b| *b != 0))
            || bytes.get(34..40).is_none_or(|s| s.iter().any(|b| *b != 0))
            || !bytes.len().is_multiple_of(size)
            || bytes.len() < size
            || (version == 3 && word(bytes, 40)? != 0)
        {
            return Err(CompoundError::Header);
        }
        let sectors = bytes.len() / size - 1;
        let fat_count = index(word(bytes, 44)?)?;
        let difat_count = index(word(bytes, 72)?)?;
        if fat_count == 0 || fat_count > sectors || difat_count > sectors {
            return Err(CompoundError::Allocation);
        }
        let mut work = Work {
            left: limits.work_units,
            maximum: limits.work_units,
            cancellation,
        };
        let mut used = vec![false; sectors];
        let mut fat_ids = Vec::new();
        let mut difat_ids = Vec::new();
        for offset in (76..512).step_by(4) {
            work.step()?;
            let id = word(bytes, offset)?;
            if fat_ids.len() < fat_count {
                claim(id, &mut used)?;
                fat_ids.push(id);
            } else if id != FREE {
                return Err(CompoundError::Allocation);
            }
        }
        let mut next_difat = word(bytes, 68)?;
        for _ in 0..difat_count {
            work.step()?;
            claim(next_difat, &mut used)?;
            difat_ids.push(next_difat);
            let data = sector(bytes, next_difat, size)?;
            for offset in (0..size - 4).step_by(4) {
                work.step()?;
                let id = word(data, offset)?;
                if fat_ids.len() < fat_count {
                    claim(id, &mut used)?;
                    fat_ids.push(id);
                } else if id != FREE {
                    return Err(CompoundError::Allocation);
                }
            }
            next_difat = word(data, size - 4)?;
        }
        if next_difat != END || fat_ids.len() != fat_count {
            return Err(CompoundError::Allocation);
        }
        let mut fat = Vec::new();
        for id in &fat_ids {
            let data = sector(bytes, *id, size)?;
            for offset in (0..size).step_by(4) {
                work.step()?;
                let value = word(data, offset)?;
                if fat.len() < sectors {
                    fat.push(value);
                } else if value != FREE {
                    return Err(CompoundError::Allocation);
                }
            }
        }
        if fat.len() != sectors {
            return Err(CompoundError::Allocation);
        }
        for (ids, marker) in [(&fat_ids, FAT), (&difat_ids, DIFAT)] {
            for id in ids {
                work.step()?;
                if fat.get(index(*id)?) != Some(&marker) {
                    return Err(CompoundError::Allocation);
                }
            }
        }
        let directory = chain(word(bytes, 48)?, &fat, &mut used, None, sectors, &mut work)?;
        if directory.is_empty() || (version == 4 && directory.len() != index(word(bytes, 40)?)?) {
            return Err(CompoundError::Directory);
        }
        if directory.len() > limits.directory_entries / (size / 128) {
            return Err(CompoundError::Limit {
                resource: "directory entries",
                maximum: limits.directory_entries,
            });
        }
        let mut entries = Vec::new();
        for id in directory {
            let data = sector(bytes, id, size)?;
            for offset in (0..size).step_by(128) {
                work.step()?;
                let record = data
                    .get(offset..offset + 128)
                    .ok_or(CompoundError::Directory)?;
                let entry = parse_entry(
                    record,
                    u32::try_from(entries.len()).map_err(|_| CompoundError::Directory)?,
                    sector_start(id, size)? + offset,
                    version,
                )?;
                entries.push(entry);
            }
        }
        let root = entries
            .first()
            .and_then(Option::as_ref)
            .ok_or(CompoundError::Directory)?;
        if root.kind != CompoundEntryKind::Root
            || root.name != "Root Entry"
            || root.left != FREE
            || root.right != FREE
            || !root.length.is_multiple_of(64)
        {
            return Err(CompoundError::Directory);
        }
        if root.length > bytes.len() {
            return Err(CompoundError::Allocation);
        }
        let mini_stream = chain(
            root.start,
            &fat,
            &mut used,
            Some(root.length.div_ceil(size)),
            sectors,
            &mut work,
        )?;
        let mini_slots = root.length / 64;
        let mini_count = index(word(bytes, 64)?)?;
        let mini_sectors = chain(
            word(bytes, 60)?,
            &fat,
            &mut used,
            Some(mini_count),
            sectors,
            &mut work,
        )?;
        let mut mini_fat = Vec::new();
        for id in mini_sectors {
            let data = sector(bytes, id, size)?;
            for offset in (0..size).step_by(4) {
                work.step()?;
                let value = word(data, offset)?;
                if mini_fat.len() < mini_slots {
                    mini_fat.push(value);
                } else if value != FREE {
                    return Err(CompoundError::Allocation);
                }
            }
        }
        validate_tree(&mut entries, &mut work)?;
        let mut mini_used = vec![false; mini_slots];
        for entry in entries
            .iter()
            .flatten()
            .filter(|e| e.kind == CompoundEntryKind::Stream)
        {
            work.step()?;
            if entry.length > bytes.len() {
                return Err(CompoundError::Allocation);
            }
            if entry.length < 4096 {
                chain(
                    entry.start,
                    &mini_fat,
                    &mut mini_used,
                    Some(entry.length.div_ceil(64)),
                    mini_slots,
                    &mut work,
                )?;
            } else {
                chain(
                    entry.start,
                    &fat,
                    &mut used,
                    Some(entry.length.div_ceil(size)),
                    sectors,
                    &mut work,
                )?;
            }
        }
        Ok(Self {
            bytes,
            sector_size: size,
            fat,
            mini_fat,
            mini_stream,
            entries,
        })
    }

    /// Iterates allocated entries in directory-ID order, retaining parent IDs.
    pub fn entries(&self) -> impl Iterator<Item = &CompoundEntry> {
        self.entries.iter().flatten()
    }

    /// Copies one validated stream and maps every returned byte to the container.
    ///
    /// # Errors
    /// Returns [`CompoundError::NotStream`] for invalid IDs or storage objects,
    /// [`CompoundError::Limit`] before allocating an oversized output, or a
    /// cancellation error. The output ceiling cannot exceed the input length.
    pub fn read_stream(
        &self,
        id: u32,
        maximum_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<CompoundStream, CompoundError> {
        cancellation.check()?;
        let entry = self
            .entries
            .get(index(id)?)
            .and_then(Option::as_ref)
            .filter(|e| e.kind == CompoundEntryKind::Stream)
            .ok_or(CompoundError::NotStream)?;
        if entry.length > maximum_bytes {
            return Err(CompoundError::Limit {
                resource: "stream bytes",
                maximum: maximum_bytes,
            });
        }
        let mut result = CompoundStream {
            bytes: Vec::with_capacity(entry.length),
            parts: Vec::new(),
        };
        let mini = entry.length < 4096;
        let block = if mini { 64 } else { self.sector_size };
        let table = if mini { &self.mini_fat } else { &self.fat };
        let mut next = entry.start;
        while result.bytes.len() < entry.length {
            cancellation.check()?;
            let offset = if mini {
                let logical = index(next)?
                    .checked_mul(64)
                    .ok_or(CompoundError::Allocation)?;
                let physical = *self
                    .mini_stream
                    .get(logical / self.sector_size)
                    .ok_or(CompoundError::Allocation)?;
                sector_start(physical, self.sector_size)?
                    .checked_add(logical % self.sector_size)
                    .ok_or(CompoundError::Allocation)?
            } else {
                sector_start(next, self.sector_size)?
            };
            let amount = block.min(entry.length - result.bytes.len());
            let end = offset
                .checked_add(amount)
                .ok_or(CompoundError::Allocation)?;
            let data = self
                .bytes
                .get(offset..end)
                .ok_or(CompoundError::Allocation)?;
            let from = result.bytes.len();
            result.bytes.extend_from_slice(data);
            result.parts.push(CompoundSourcePart {
                container: offset..end,
                stream: from..result.bytes.len(),
            });
            next = *table.get(index(next)?).ok_or(CompoundError::Allocation)?;
        }
        if next != END {
            return Err(CompoundError::Allocation);
        }
        Ok(result)
    }
}

fn parse_entry(
    data: &[u8],
    id: u32,
    offset: usize,
    version: u16,
) -> Result<Option<CompoundEntry>, CompoundError> {
    let kind = match data.get(66) {
        Some(0) => return Ok(None),
        Some(1) => CompoundEntryKind::Storage,
        Some(2) => CompoundEntryKind::Stream,
        Some(5) => CompoundEntryKind::Root,
        _ => return Err(CompoundError::Directory),
    };
    let length = usize::from(short(data, 64)?);
    if !(2..=64).contains(&length)
        || !length.is_multiple_of(2)
        || data.get(67).is_none_or(|c| *c > 1)
    {
        return Err(CompoundError::Directory);
    }
    let units: Vec<_> = data
        .get(..length)
        .ok_or(CompoundError::Directory)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    let (last, name_units) = units.split_last().ok_or(CompoundError::Directory)?;
    if *last != 0 || name_units.contains(&0) {
        return Err(CompoundError::Directory);
    }
    let name = String::from_utf16(name_units).map_err(|_| CompoundError::Directory)?;
    if name.contains(['/', '\\', ':', '!']) {
        return Err(CompoundError::Directory);
    }
    // MS-CFB readers must ignore legacy uninitialized high DWORDs in v3 sizes.
    let byte_length = u64::from(word(data, 120)?)
        | if version == 4 {
            u64::from(word(data, 124)?) << 32
        } else {
            0
        };
    let child = word(data, 76)?;
    if kind == CompoundEntryKind::Stream && child != FREE {
        return Err(CompoundError::Directory);
    }
    Ok(Some(CompoundEntry {
        id,
        name,
        kind,
        parent: None,
        directory_offset: offset,
        start: word(data, 116)?,
        length: if kind == CompoundEntryKind::Storage {
            0
        } else {
            usize::try_from(byte_length).map_err(|_| CompoundError::Allocation)?
        },
        left: word(data, 68)?,
        right: word(data, 72)?,
        child,
    }))
}

fn validate_tree(
    entries: &mut [Option<CompoundEntry>],
    work: &mut Work<'_>,
) -> Result<(), CompoundError> {
    let child = entries
        .first()
        .and_then(Option::as_ref)
        .ok_or(CompoundError::Directory)?
        .child;
    let mut seen = vec![false; entries.len()];
    *seen.first_mut().ok_or(CompoundError::Directory)? = true;
    let mut pending = vec![(child, 0)];
    while let Some((id, parent)) = pending.pop() {
        work.step()?;
        if id == FREE {
            continue;
        }
        let slot = seen.get_mut(index(id)?).ok_or(CompoundError::Directory)?;
        if *slot {
            return Err(CompoundError::Directory);
        }
        *slot = true;
        let entry = entries
            .get_mut(index(id)?)
            .and_then(Option::as_mut)
            .ok_or(CompoundError::Directory)?;
        if entry.kind == CompoundEntryKind::Root {
            return Err(CompoundError::Directory);
        }
        entry.parent = Some(parent);
        pending.push((entry.left, parent));
        pending.push((entry.right, parent));
        if entry.kind == CompoundEntryKind::Storage {
            pending.push((entry.child, id));
        }
    }
    if entries
        .iter()
        .zip(seen)
        .any(|(entry, visited)| entry.is_some() && !visited)
    {
        return Err(CompoundError::Directory);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
