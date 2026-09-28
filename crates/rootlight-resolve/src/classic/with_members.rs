//! Supplies exact receiver spans for explicit and With-bound member expressions.
//! A source-position sweep selects the innermost body without treating a nested
//! header as part of its own block or substituting invented receiver text.

use super::member_access_parts;
use crate::{ResolutionError, engine::ResolutionWorkBudget};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, FileId};
use rootlight_ir::{OccurrenceRecord, SourceRef, SourceSpan};
use std::collections::BTreeMap;

pub(super) struct Member<'s> {
    pub(super) receiver: &'s str,
    pub(super) name: &'s str,
    pub(super) span: SourceSpan,
    pub(super) boundaries: Vec<SourceRef>,
    pub(super) implicit_root: bool,
}

pub(super) fn inventory<'s>(
    occurrences: &[OccurrenceRecord],
    sources: &BTreeMap<FileId, &'s [u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<BTreeMap<FactId, Member<'s>>, ResolutionError> {
    let mut sites: BTreeMap<FileId, Vec<&OccurrenceRecord>> = BTreeMap::new();
    for site in occurrences {
        cancellation.check()?;
        work.consume()?;
        if super::is_member(&site.syntax_kind) {
            sites.entry(site.file).or_default().push(site);
        }
    }
    let mut result = BTreeMap::new();
    for (file, mut sites) in sites {
        let Some(&source) = sources.get(&file) else {
            continue;
        };
        sites.sort_unstable_by_key(|site| site.source.span());
        let implicit = sites.iter().any(|site| {
            usize::try_from(site.source.span().start_byte())
                .ok()
                .and_then(|i| source.get(i))
                == Some(&b'.')
        });
        let blocks = if implicit {
            rootlight_vba::with_blocks(source, crate::MAX_RESOLUTION_WORK_LIMIT, &mut || {
                cancellation.check()?;
                work.consume()
            })?
            .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut next = 0;
        let mut active: Vec<&rootlight_vba::WithBlock> = Vec::new();
        for site in sites {
            cancellation.check()?;
            work.consume()?;
            let span = site.source.span();
            let (Ok(start), Ok(end)) = (
                usize::try_from(span.start_byte()),
                usize::try_from(span.end_byte()),
            ) else {
                continue;
            };
            let Some(text) = source.get(start..end) else {
                continue;
            };
            while let Some(block) = blocks.get(next).filter(|block| block.body().start <= start) {
                work.consume()?;
                while active
                    .last()
                    .is_some_and(|parent| parent.body().end <= block.body().start)
                {
                    active.pop();
                }
                active.push(block);
                next += 1;
            }
            while active.last().is_some_and(|block| block.body().end <= start) {
                active.pop();
            }
            let mut boundaries = Vec::new();
            let (receiver, name, range, implicit_root) = if text.first() == Some(&b'.') {
                let Some(block) = active.last().filter(|block| end <= block.body().end) else {
                    continue;
                };
                let tail = text.get(1..).unwrap_or_default();
                let mut check = || {
                    cancellation.check()?;
                    work.consume()
                };
                let offset = 1 + rootlight_vba::logical_whitespace_len(tail, &mut check)?;
                let tail = text.get(offset..).unwrap_or_default();
                if let Some((receiver, name)) = member_access_parts(tail, cancellation, work)? {
                    let range = start..start + offset + receiver.len();
                    let Some(receiver) = source
                        .get(range.clone())
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    else {
                        continue;
                    };
                    (receiver, name, range, false)
                } else {
                    let Some(path) = rootlight_vba::procedure_pointer_path(tail, &mut || {
                        cancellation.check()?;
                        work.consume()
                    })?
                    else {
                        continue;
                    };
                    if path.components() != 1 || path.end() != tail.len() {
                        continue;
                    }
                    let range = block.receiver();
                    let Some(receiver) = source
                        .get(range.clone())
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    else {
                        continue;
                    };
                    let Ok(name) = std::str::from_utf8(path.last()) else {
                        continue;
                    };
                    for range in [block.header(), block.terminator()] {
                        boundaries.push(source_ref(&site.source, range)?);
                    }
                    (receiver, name, range, true)
                }
            } else {
                let Some((receiver, name)) = member_access_parts(text, cancellation, work)? else {
                    continue;
                };
                (receiver, name, start..start + receiver.len(), false)
            };
            result.insert(
                site.id,
                Member {
                    receiver,
                    name,
                    span: source_ref(&site.source, range)?.span(),
                    boundaries,
                    implicit_root,
                },
            );
        }
    }
    Ok(result)
}

fn source_ref(
    source: &SourceRef,
    range: std::ops::Range<usize>,
) -> Result<SourceRef, ResolutionError> {
    let span = SourceSpan::new(
        source.span().file(),
        u64::try_from(range.start).map_err(|_| ResolutionError::CountOverflow)?,
        u64::try_from(range.end).map_err(|_| ResolutionError::CountOverflow)?,
    )
    .map_err(ResolutionError::InvalidProducer)?;
    Ok(SourceRef::new(
        source.repository(),
        source.generation(),
        span,
        source.content_hash(),
        None,
    ))
}
