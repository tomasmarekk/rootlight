//! Source-written C++ include and namespace bindings in the indexed snapshot.
//! This proof does not establish SDK, preprocessor, overload or configured compiler closure.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, EntityRecord, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef,
};

use super::{CapturedSource, CxxIndex, WrittenCall, cmake::WrittenIncludes};
use crate::{
    ResolutionError,
    engine::{CandidateIndex, ResolutionWorkBudget},
};
use rootlight_cancel::Cancellation;

pub(super) fn build(
    index: &CandidateIndex<'_>,
    occurrences: &[OccurrenceRecord],
    captured: &BTreeMap<FileId, CapturedSource<'_, '_>>,
    includes: &[WrittenIncludes],
    work: &mut ResolutionWorkBudget,
    cancellation: &Cancellation,
) -> Result<CxxIndex, ResolutionError> {
    let mut by_path = BTreeMap::new();
    let mut source_includes = BTreeMap::new();
    for file in index.files.values() {
        cancellation.check()?;
        work.consume()?;
        by_path.insert(file.path.as_str(), *file);
    }
    for source in captured.values() {
        cancellation.check()?;
        work.consume()?;
        if source.file.language == "cpp" {
            source_includes.insert(
                source.file.id,
                literal_includes(source.bytes, work, cancellation)?,
            );
        }
    }
    let mut declarations = BTreeMap::<(Vec<String>, String), Vec<&EntityRecord>>::new();
    let mut file_names = BTreeMap::<(FileId, String), BTreeSet<SymbolId>>::new();
    for entity in index
        .entities
        .values()
        .filter(|entity| entity.language == "cpp")
    {
        cancellation.check()?;
        work.consume()?;
        let Some(source) = &entity.evidence.source else {
            continue;
        };
        if entity.kind != EntityKind::Import {
            file_names
                .entry((source.span().file(), entity.canonical_name.clone()))
                .or_default()
                .insert(entity.id);
        }
        if let Some(namespace) = namespace(entity.container, source, index, work, cancellation)? {
            declarations
                .entry((namespace, entity.canonical_name.clone()))
                .or_default()
                .push(entity);
        }
    }
    let mut definitions = BTreeMap::new();
    for &ordinal in &index.cxx_definitions {
        cancellation.check()?;
        work.consume()?;
        let occurrence = occurrences
            .get(ordinal)
            .ok_or(ResolutionError::InvalidScore)?;
        if occurrence.role == OccurrenceRole::Definition
            && occurrence.syntax_kind == "cpp.identifier.definition"
            && let OccurrenceTarget::Resolved { symbol } = occurrence.target
        {
            definitions
                .entry(symbol)
                .or_insert_with(Vec::new)
                .push(occurrence);
        }
    }
    let mut result = CxxIndex::default();
    for &ordinal in &index.cxx_calls {
        cancellation.check()?;
        work.consume()?;
        let occurrence = occurrences
            .get(ordinal)
            .ok_or(ResolutionError::InvalidScore)?;
        if occurrence.role != OccurrenceRole::CallSite || occurrence.syntax_kind != "cpp.call.call"
        {
            continue;
        }
        let Some(caller) = captured
            .get(&occurrence.file)
            .filter(|source| source.file.language == "cpp")
        else {
            continue;
        };
        let Some(text) = bytes_for(caller.bytes, &occurrence.source)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            continue;
        };
        let Some((name, arguments)) = text.split_once('(') else {
            continue;
        };
        let name = name.trim_end();
        if !identifier(name)
            || !arguments.trim_end().ends_with(')')
            || content_hash(name.as_bytes()) != occurrence.syntactic_text_hash
        {
            continue;
        }
        let Some(owner) = occurrence
            .enclosing
            .and_then(|id| index.entities.get(&id))
            .filter(|owner| owner.kind == EntityKind::Function && owner.language == "cpp")
        else {
            continue;
        };
        let Some(owner_source) = owner
            .evidence
            .source
            .as_ref()
            .filter(|source| contains(source, &occurrence.source))
        else {
            continue;
        };
        let Some(scope) = namespace(owner.container, owner_source, index, work, cancellation)?
        else {
            continue;
        };
        let Some([target]) = declarations
            .get(&(scope, name.to_owned()))
            .map(Vec::as_slice)
        else {
            continue;
        };
        if target.kind != EntityKind::Function {
            continue;
        }
        let Some(target_source) = target.evidence.source.as_ref() else {
            continue;
        };
        let Some(target_file) = captured
            .get(&target_source.span().file())
            .filter(|source| source.file.language == "cpp")
        else {
            continue;
        };
        // Reopened namespace projections may retain a different physical extent.
        // An unclassified same-file declaration cannot disappear from ambiguity.
        if file_names
            .get(&(target_file.file.id, name.to_owned()))
            .is_some_and(|symbols| symbols.len() != 1 || !symbols.contains(&target.id))
        {
            continue;
        }
        let Some([definition]) = definitions.get(&target.id).map(Vec::as_slice) else {
            continue;
        };
        if bytes_for(target_file.bytes, &definition.source) != Some(name.as_bytes())
            || !contains(target_source, &definition.source)
            || file_names
                .get(&(caller.file.id, name.to_owned()))
                .is_some_and(|symbols| symbols.iter().any(|symbol| *symbol != target.id))
        {
            continue;
        }
        let Some(Some(imports)) = source_includes.get(&caller.file.id) else {
            continue;
        };
        if !matches!(source_includes.get(&target_file.file.id), Some(Some(_))) {
            continue;
        }
        let directory = caller
            .file
            .path
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        let authorities: Vec<_> = includes
            .iter()
            .filter(|authority| authority.directory == directory)
            .collect();
        if authorities.is_empty() {
            continue;
        }
        let mut evidence = Vec::new();
        let mut agreed = true;
        for authority in authorities {
            cancellation.check()?;
            work.consume()?;
            let mut found = false;
            for import in imports {
                for root in &authority.roots {
                    cancellation.check()?;
                    work.consume()?;
                    let Some(path) = root.as_ref().and_then(|root| super::join(root, import))
                    else {
                        break;
                    };
                    if let Some(source) = by_path.get(path.as_str()) {
                        found |= source.id == target_file.file.id;
                        break;
                    }
                }
            }
            agreed &= found;
            evidence.extend(authority.evidence.iter().cloned());
        }
        if !agreed {
            continue;
        }
        evidence.push(super::full_source(caller.file)?);
        evidence.push(super::full_source(target_file.file)?);
        evidence.sort_unstable();
        evidence.dedup();
        let proof = WrittenCall {
            call: occurrence.source.clone(),
            target: target.id,
            definition: definition.source.clone(),
            definition_id: definition.id,
            sources: evidence,
            owners: vec![owner.id, target.id],
        };
        result
            .calls
            .entry(occurrence.source.span())
            .and_modify(|proof| *proof = None)
            .or_insert(Some(proof));
    }
    Ok(result)
}

fn namespace(
    mut container: Option<ContainerRef>,
    child: &SourceRef,
    index: &CandidateIndex<'_>,
    work: &mut ResolutionWorkBudget,
    cancellation: &Cancellation,
) -> Result<Option<Vec<String>>, ResolutionError> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(current) = container {
        cancellation.check()?;
        work.consume()?;
        match current {
            ContainerRef::File(file) if file == child.span().file() => {
                result.reverse();
                return Ok(Some(result));
            }
            ContainerRef::Entity(id) if seen.insert(id) => {
                let Some(entity) = index.entities.get(&id).filter(|entity| {
                    entity.language == "cpp"
                        && matches!(entity.kind, EntityKind::Module | EntityKind::Namespace)
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| contains(source, child))
                }) else {
                    return Ok(None);
                };
                let parts: Vec<_> = entity.canonical_name.split("::").collect();
                if !parts.iter().all(|part| identifier(part)) {
                    return Ok(None);
                }
                result.extend(parts.into_iter().rev().map(str::to_owned));
                container = entity.container;
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn contains(parent: &SourceRef, child: &SourceRef) -> bool {
    parent.repository() == child.repository()
        && parent.generation() == child.generation()
        && parent.content_hash() == child.content_hash()
        && parent.span().file() == child.span().file()
        && parent.span().start_byte() <= child.span().start_byte()
        && child.span().end_byte() <= parent.span().end_byte()
}

fn bytes_for<'a>(bytes: &'a [u8], source: &SourceRef) -> Option<&'a [u8]> {
    bytes.get(
        usize::try_from(source.span().start_byte()).ok()?
            ..usize::try_from(source.span().end_byte()).ok()?,
    )
}

fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn literal_includes(
    bytes: &[u8],
    work: &mut ResolutionWorkBudget,
    cancellation: &Cancellation,
) -> Result<Option<Vec<String>>, ResolutionError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(None);
    };
    if bytes.len() > 1024 * 1024
        || ["\\\n", "\\\r\n", "R\""]
            .iter()
            .any(|pattern| text.contains(pattern))
    {
        return Ok(None);
    }
    for _ in bytes.chunks(4096) {
        cancellation.check()?;
        work.consume()?;
    }
    let mut cleaned = bytes.to_vec();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes.get(offset..offset + 2) == Some(b"//") {
            while offset < bytes.len() && bytes[offset] != b'\n' {
                cleaned[offset] = b' ';
                offset += 1;
            }
        } else if bytes.get(offset..offset + 2) == Some(b"/*") {
            let start = offset;
            offset += 2;
            while offset < bytes.len() && bytes.get(offset..offset + 2) != Some(b"*/") {
                offset += 1;
            }
            if offset == bytes.len() {
                return Ok(None);
            }
            offset += 2;
            for byte in &mut cleaned[start..offset] {
                if *byte != b'\n' && *byte != b'\r' {
                    *byte = b' ';
                }
            }
        } else if matches!(bytes[offset], b'\'' | b'"') {
            let quote = bytes[offset];
            offset += 1;
            while offset < bytes.len() && bytes[offset] != quote {
                if bytes[offset] == b'\\' {
                    offset += 1;
                }
                offset += 1;
            }
            if offset == bytes.len() {
                return Ok(None);
            }
            offset += 1;
        } else {
            offset += 1;
        }
    }
    let Ok(cleaned) = std::str::from_utf8(&cleaned) else {
        return Ok(None);
    };
    // These constructs can alter ordinary name lookup or introduce dependent calls.
    if cleaned
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| matches!(word, "using" | "template"))
    {
        return Ok(None);
    }
    let mut includes = Vec::new();
    for line in cleaned.lines() {
        cancellation.check()?;
        work.consume()?;
        let Some(directive) = line.trim_start().strip_prefix('#') else {
            continue;
        };
        let directive = directive.trim_start();
        if directive == "pragma once" {
            continue;
        }
        let Some(path) = directive
            .strip_prefix("include")
            .map(str::trim)
            .and_then(|path| path.strip_prefix('<'))
            .and_then(|path| path.strip_suffix('>'))
        else {
            return Ok(None);
        };
        if path.is_empty()
            || path.bytes().any(|byte| {
                byte.is_ascii_whitespace() || matches!(byte, b'<' | b'>' | b'"' | b'\\' | b'$')
            })
        {
            return Ok(None);
        }
        includes.push(path.to_owned());
    }
    Ok(Some(includes))
}
