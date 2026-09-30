//! Reuses bytes-relative lexical inventories within one verified source build.
//! Hashes only select candidates: charged exact-byte comparison protects against
//! collisions, and file ownership and semantic reconciliation remain external.

use std::{borrow::Cow, collections::BTreeMap};

use rootlight_ids::ContentHash;

#[cfg(test)]
mod tests;

const MAX_CANDIDATE_KEYS: usize = 1024;
const MAX_CACHE_ENTRIES: usize = 256;
const MAX_RETAINED_RESULT_BYTES: usize = 1024 * 1024;
const COMPARISON_CHUNK_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    hash: ContentHash,
    length: usize,
}

#[derive(Debug, Default)]
pub(super) struct RepeatedSources {
    keys: BTreeMap<SourceKey, bool>,
}

impl RepeatedSources {
    pub(super) fn build<'a, E>(
        sources: impl IntoIterator<Item = (ContentHash, &'a [u8])>,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        let mut result = Self::default();
        for (hash, bytes) in sources {
            check()?;
            let key = SourceKey {
                hash,
                length: bytes.len(),
            };
            if let Some(repeated) = result.keys.get_mut(&key) {
                *repeated = true;
            } else if result.keys.len() < MAX_CANDIDATE_KEYS {
                result.keys.insert(key, false);
            }
        }
        check()?;
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    source: SourceKey,
    read_limits: Option<(usize, usize)>,
}

#[derive(Debug)]
struct Entry<'a, T> {
    bytes: &'a [u8],
    value: T,
}

#[derive(Debug)]
pub(super) struct LexicalCache<'keys, 'bytes, T> {
    repeated: &'keys RepeatedSources,
    entries: BTreeMap<CacheKey, Entry<'bytes, T>>,
    retained_result_bytes: usize,
    maximum_entries: usize,
    maximum_result_bytes: usize,
}

impl<'keys, 'bytes, T: Clone> LexicalCache<'keys, 'bytes, T> {
    pub(super) fn new(repeated: &'keys RepeatedSources) -> Self {
        Self {
            repeated,
            entries: BTreeMap::new(),
            retained_result_bytes: 0,
            maximum_entries: MAX_CACHE_ENTRIES,
            maximum_result_bytes: MAX_RETAINED_RESULT_BYTES,
        }
    }

    pub(super) fn get_or_parse<E, C: FnMut() -> Result<(), E>>(
        &mut self,
        hash: ContentHash,
        bytes: &'bytes [u8],
        read_limits: Option<(usize, usize)>,
        check: &mut C,
        parse: impl FnOnce(&mut C) -> Result<T, E>,
        result_bytes: impl FnOnce(&T) -> usize,
    ) -> Result<Cow<'_, T>, E> {
        check()?;
        let key = CacheKey {
            source: SourceKey {
                hash,
                length: bytes.len(),
            },
            read_limits,
        };
        if self.repeated.keys.get(&key.source) != Some(&true) {
            return parse(check).map(Cow::Owned);
        }
        let hit = if let Some(entry) = self.entries.get(&key) {
            exact_bytes(entry.bytes, bytes, check)?
        } else {
            false
        };
        if hit {
            check()?;
            return Ok(Cow::Borrowed(
                &self
                    .entries
                    .get(&key)
                    .expect("a matched cache entry remains present")
                    .value,
            ));
        }
        let value = parse(check)?;
        let retained = self.retained_result_bytes.checked_add(result_bytes(&value));
        // A collision must not replace its representative or make a different
        // module's bytes-relative ranges reusable for that representative.
        if self.entries.contains_key(&key)
            || self.entries.len() >= self.maximum_entries
            || retained.is_none_or(|bytes| bytes > self.maximum_result_bytes)
        {
            return Ok(Cow::Owned(value));
        }
        check()?;
        self.retained_result_bytes = retained.expect("retained size was checked against the cap");
        self.entries.insert(key, Entry { bytes, value });
        Ok(Cow::Borrowed(
            &self
                .entries
                .get(&key)
                .expect("a newly inserted cache entry remains present")
                .value,
        ))
    }
}

fn exact_bytes<E>(
    left: &[u8],
    right: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .chunks(COMPARISON_CHUNK_BYTES)
        .zip(right.chunks(COMPARISON_CHUNK_BYTES))
    {
        check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn initializer_bytes(_: &rootlight_vba::ClassInitializer) -> usize {
    std::mem::size_of::<rootlight_vba::ClassInitializer>()
}

pub(super) fn value_read_bytes(reads: &Option<Vec<rootlight_vba::ValueRead>>) -> usize {
    std::mem::size_of_val(reads).saturating_add(reads.as_ref().map_or(0, |reads| {
        reads
            .capacity()
            .saturating_mul(std::mem::size_of::<rootlight_vba::ValueRead>())
    }))
}
