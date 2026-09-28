//! Inventories source-declared default procedures within verified class owners.
//! Conflicting or malformed dispatch attributes remain class-wide barriers.

use super::*;

pub(super) struct DefaultMember {
    pub(super) name: String,
    pub(super) sources: Vec<SourceRef>,
}

pub(super) fn inventory(
    entities: &BTreeMap<SymbolId, &EntityRecord>,
    classes: &BTreeSet<SymbolId>,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<BTreeMap<SymbolId, Option<DefaultMember>>, ResolutionError> {
    let mut result: BTreeMap<SymbolId, Option<DefaultMember>> = BTreeMap::new();
    let mut observed = BTreeSet::new();
    for entity in entities.values() {
        cancellation.check()?;
        work.consume()?;
        let Some(ContainerRef::Entity(class)) = entity.container else {
            continue;
        };
        if !classes.contains(&class)
            || !matches!(
                entity.kind,
                EntityKind::Method | EntityKind::Function | EntityKind::Property
            )
        {
            continue;
        }
        let Some(source) = entity.evidence.source.as_ref() else {
            result.insert(class, None);
            continue;
        };
        let Some(bytes) = sources.get(&source.span().file()).and_then(|bytes| {
            bytes.get(
                usize::try_from(source.span().start_byte()).ok()?
                    ..usize::try_from(source.span().end_byte()).ok()?,
            )
        }) else {
            result.insert(class, None);
            continue;
        };
        let attribute = rootlight_vba::member_dispatch_attribute(bytes, &mut || {
            cancellation.check()?;
            work.consume()
        })?;
        if let rootlight_vba::MemberDispatchAttribute::Value { span, .. } = &attribute {
            let start = source
                .span()
                .start_byte()
                .checked_add(u64::try_from(span.start).map_err(|_| ResolutionError::CountOverflow)?)
                .ok_or(ResolutionError::CountOverflow)?;
            let end = source
                .span()
                .start_byte()
                .checked_add(u64::try_from(span.end).map_err(|_| ResolutionError::CountOverflow)?)
                .ok_or(ResolutionError::CountOverflow)?;
            observed.insert((source.span().file(), start, end));
        }
        match attribute {
            rootlight_vba::MemberDispatchAttribute::Absent
            | rootlight_vba::MemberDispatchAttribute::Value {
                id: 1.. | ..=-1, ..
            } => {}
            rootlight_vba::MemberDispatchAttribute::Invalid => {
                result.insert(class, None);
            }
            rootlight_vba::MemberDispatchAttribute::Value { id: 0, span } => {
                let name = entity.canonical_name.to_ascii_lowercase();
                let start = source
                    .span()
                    .start_byte()
                    .checked_add(
                        u64::try_from(span.start).map_err(|_| ResolutionError::CountOverflow)?,
                    )
                    .ok_or(ResolutionError::CountOverflow)?;
                let end = source
                    .span()
                    .start_byte()
                    .checked_add(
                        u64::try_from(span.end).map_err(|_| ResolutionError::CountOverflow)?,
                    )
                    .ok_or(ResolutionError::CountOverflow)?;
                let attribute = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    rootlight_ir::SourceSpan::new(source.span().file(), start, end)
                        .map_err(ResolutionError::InvalidProducer)?,
                    source.content_hash(),
                    None,
                );
                match result.entry(class) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(Some(DefaultMember {
                            name,
                            sources: vec![attribute],
                        }));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        if let Some(prior) = entry.get_mut() {
                            if prior.name == name {
                                prior.sources.push(attribute);
                            } else {
                                entry.insert(None);
                            }
                        }
                    }
                }
            }
        }
    }
    // An unmodeled declaration can compete for the same dispatch ID. In
    // particular, variable attributes and excluded conditional procedures
    // must not disappear from the uniqueness proof for a recognized method.
    for class in classes.iter().filter_map(|id| entities.get(id)) {
        cancellation.check()?;
        work.consume()?;
        let Some(file) = class
            .evidence
            .source
            .as_ref()
            .map(|source| source.span().file())
        else {
            result.insert(class.id, None);
            continue;
        };
        let Some(bytes) = sources.get(&file) else {
            result.insert(class.id, None);
            continue;
        };
        let mut offset = 0_usize;
        for physical in bytes.split_inclusive(|byte| *byte == b'\n') {
            cancellation.check()?;
            work.consume_many(physical.len().div_ceil(64).max(1))?;
            let line = physical.strip_suffix(b"\n").unwrap_or(physical);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let trimmed = line.trim_ascii();
            if trimmed
                .get(..9)
                .is_some_and(|word| word.eq_ignore_ascii_case(b"Attribute"))
                && trimmed.get(9).is_some_and(u8::is_ascii_whitespace)
            {
                let left = trimmed
                    .split(|byte| *byte == b'=')
                    .next()
                    .unwrap_or_default()
                    .trim_ascii();
                let attribute = left.rsplit(|byte| *byte == b'.').next().unwrap_or_default();
                if (attribute.eq_ignore_ascii_case(b"VB_UserMemId")
                    || attribute.eq_ignore_ascii_case(b"VB_VarUserMemId"))
                    && !observed.contains(&(
                        file,
                        u64::try_from(offset).map_err(|_| ResolutionError::CountOverflow)?,
                        u64::try_from(offset + line.len())
                            .map_err(|_| ResolutionError::CountOverflow)?,
                    ))
                {
                    result.insert(class.id, None);
                }
            }
            offset += physical.len();
        }
    }
    Ok(result)
}
