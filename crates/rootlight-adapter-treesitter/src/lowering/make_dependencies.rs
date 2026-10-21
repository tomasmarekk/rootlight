//! Derive only a directly written normal Make prerequisite between local targets.
//! Source checks distinguish the header operand from recipe and order-only uses;
//! the relation makes no claim about evaluated makefiles or executed recipes.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, EntityRecord, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
    derive_relation_record_id,
};

use super::MaterializedEntity;

fn same(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.chunks(64).zip(right.chunks(64)) {
        cancel.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn ordinary_name(name: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if !name
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.'))
    {
        return Ok(false);
    }
    let mut path = false;
    for chunk in name.chunks(64) {
        cancel.check()?;
        for byte in chunk {
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')) {
                return Ok(false);
            }
            path |= *byte == b'/';
        }
    }
    // Dot-prefixed goal names can be Make directives. Only explicit path names
    // enter this slice; special goals and ordinary dotfiles remain unqualified.
    Ok(name.first() != Some(&b'.') || path)
}

fn normal_header(
    header: &[u8],
    owner: &[u8],
    cancel: &Cancellation,
) -> Result<Option<(usize, usize)>, AdapterError> {
    cancel.check()?;
    if !ordinary_name(owner, cancel)? {
        return Ok(None);
    }
    let Some(prefix) = header.get(..owner.len()) else {
        return Ok(None);
    };
    if !same(prefix, owner, cancel)? {
        return Ok(None);
    }
    let mut at = owner.len();
    while header
        .get(at)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        cancel.check()?;
        at += 1;
    }
    if header.get(at) != Some(&b':') || header.get(at + 1) == Some(&b':') {
        return Ok(None);
    }
    at += 1;
    let normal_start = at;
    loop {
        cancel.check()?;
        while header
            .get(at)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            cancel.check()?;
            at += 1;
        }
        match header.get(at) {
            None | Some(b'\n' | b'#' | b';' | b'|') => return Ok(Some((normal_start, at))),
            Some(b'\r') => {
                return Ok(
                    matches!(header.get(at + 1), None | Some(b'\n')).then_some((normal_start, at))
                );
            }
            _ => {}
        }
        let token_start = at;
        while header
            .get(at)
            .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'#' | b';' | b'|'))
        {
            cancel.check()?;
            at += 1;
        }
        let Some(token) = header.get(token_start..at) else {
            return Ok(None);
        };
        if !ordinary_name(token, cancel)? {
            return Ok(None);
        }
    }
}

fn exact_operand(
    header: &[u8],
    target: &[u8],
    start: usize,
    end: usize,
    normal: (usize, usize),
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    cancel.check()?;
    if start >= end || start < normal.0 || end > normal.1 || !ordinary_name(target, cancel)? {
        return Ok(false);
    }
    let before = start == normal.0
        || start
            .checked_sub(1)
            .and_then(|at| header.get(at))
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'));
    let after = end == normal.1
        || header
            .get(end)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'));
    let Some(token) = header.get(start..end) else {
        return Ok(false);
    };
    Ok(before && after && same(token, target, cancel)?)
}

/// Retain physical starts only under the default, ordinary Make line policy.
/// Unmodeled definition blocks or recipe-prefix changes refuse this new proof;
/// existing definitions and occurrence RefersTo remain the parser's responsibility.
fn source_line_starts(
    source: &[u8],
    cancel: &Cancellation,
) -> Result<Option<HashSet<usize>>, AdapterError> {
    let mut starts = HashSet::new();
    let mut at = 0;
    let mut continued = false;
    while at < source.len() {
        cancel.check()?;
        let line_start = at;
        while at < source.len() && source[at] != b'\n' {
            if at.is_multiple_of(64) {
                cancel.check()?;
            }
            at += 1;
        }
        let mut end = at;
        at += usize::from(at < source.len());
        if end > line_start && source[end - 1] == b'\r' {
            end -= 1;
        }
        while end > line_start && matches!(source[end - 1], b' ' | b'\t') {
            cancel.check()?;
            end -= 1;
        }
        let mut begin = line_start;
        while begin < end && source[begin] == b' ' {
            cancel.check()?;
            begin += 1;
        }
        if begin == end {
            continue;
        }
        let next_continued = source[end - 1] == b'\\';
        let blocked = continued || next_continued;
        continued = next_continued;
        if source[begin] == b'#' {
            continue;
        }
        if source.get(line_start) == Some(&b'\t') {
            continue;
        }
        let mut word = begin;
        while word < end && !matches!(source[word], b' ' | b'\t' | b':' | b'=') {
            if word.is_multiple_of(64) {
                cancel.check()?;
            }
            word += 1;
        }
        let keyword = &source[begin..word];
        let keyword = if next_continued && word == end {
            keyword.strip_suffix(b"\\").unwrap_or(keyword)
        } else {
            keyword
        };
        if keyword == b".RECIPEPREFIX"
            || (matches!(
                keyword,
                b"define" | b"endef" | b"override" | b"export" | b"unexport" | b"private"
            ) && !source
                .get(word)
                .is_some_and(|byte| matches!(byte, b':' | b'=')))
        {
            return Ok(None);
        }
        if blocked {
            continue;
        }
        starts
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        starts.insert(begin);
    }
    Ok(Some(starts))
}

/// Admit only singular authored target headers and their exact raw name leaves.
/// Count competing headers before entity normalization, even without a Definition.
pub(super) fn written_targets<'source>(
    facts: &[SyntaxFact],
    materialized: &HashMap<u64, MaterializedEntity>,
    full: &SourceRef,
    mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
    mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
    cancel: &Cancellation,
) -> Result<HashSet<SymbolId>, AdapterError> {
    cancel.check()?;
    let mut ids = HashMap::new();
    let mut definitions = HashMap::<u64, Option<&SyntaxFact>>::new();
    let mut spans = HashMap::<SourceSpan, usize>::new();
    let mut root = None;
    let mut roots = 0;
    ids.try_reserve(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    spans
        .try_reserve(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for fact in facts {
        cancel.check()?;
        ids.entry(fact.local_id())
            .and_modify(|row| *row = None)
            .or_insert(Some(fact));
        let count = spans.entry(fact.span()).or_default();
        *count = count.saturating_add(1).min(3);
        if fact.kind() == SyntaxFactKind::Root {
            roots += 1;
            if fact.syntax_kind().as_str() == "make.source_file"
                && fact.parent().is_none()
                && fact.depth() == 0
                && physical(fact.span())? == full.span()
            {
                root = Some(fact);
            }
        }
        if fact.kind() == SyntaxFactKind::Occurrence
            && fact.syntax_kind().as_str().ends_with(".definition")
            && let Some(parent) = fact.parent()
        {
            definitions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            definitions
                .entry(parent)
                .and_modify(|row| *row = None)
                .or_insert(Some(fact));
        }
    }
    let Some(root) =
        root.filter(|root| roots == 1 && ids.get(&root.local_id()) == Some(&Some(*root)))
    else {
        return Ok(HashSet::new());
    };
    let Some(starts) = source_line_starts(text(root.span())?.as_bytes(), cancel)? else {
        return Ok(HashSet::new());
    };
    let mut names = HashMap::<ContentHash, usize>::new();
    let mut candidates = HashMap::<u64, (ContentHash, SourceSpan, u64)>::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Declaration
            || fact.syntax_kind().as_str() != "make.target.declaration"
            || fact.parent() != Some(root.local_id())
            || fact.depth() != 1
            || usize::try_from(fact.span().start_byte())
                .ok()
                .is_none_or(|start| !starts.contains(&start))
            || ids.get(&fact.local_id()) != Some(&Some(fact))
        {
            continue;
        }
        let header = text(fact.span())?;
        let mut end = 0;
        while header
            .as_bytes()
            .get(end)
            .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b':' | b'\r' | b'\n'))
        {
            cancel.check()?;
            end += 1;
        }
        let name = &header.as_bytes()[..end];
        if normal_header(header.as_bytes(), name, cancel)?.is_none() {
            continue;
        }
        let mut hash = blake3::Hasher::new();
        for chunk in name.chunks(64) {
            cancel.check()?;
            hash.update(chunk);
        }
        let key = ContentHash::from_bytes(*hash.finalize().as_bytes());
        names
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        let count = names.entry(key).or_default();
        *count = count.saturating_add(1).min(2);
        let Some(definition) = definitions.get(&fact.local_id()).copied().flatten() else {
            continue;
        };
        let name_end = fact
            .span()
            .start_byte()
            .checked_add(u64::try_from(end).map_err(|_| SinkError::InvalidIr)?)
            .ok_or(SinkError::InvalidIr)?;
        let name_span = SourceSpan::new(fact.span().file(), fact.span().start_byte(), name_end)
            .map_err(|_| SinkError::InvalidIr)?;
        if definition.syntax_kind().as_str() != "make.name.definition"
            || definition.span() != name_span
            || definition.depth() != 2
            || ids.get(&definition.local_id()) != Some(&Some(definition))
            || spans.get(&name_span) != Some(&1)
            || spans.get(&fact.span()) != Some(&(1 + usize::from(fact.span() == root.span())))
        {
            continue;
        }
        candidates
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        candidates.insert(fact.local_id(), (key, name_span, definition.local_id()));
    }
    let mut admitted = HashSet::new();
    for (local, entity) in materialized {
        cancel.check()?;
        let Some((key, name_span, definition)) = candidates.get(local) else {
            continue;
        };
        let Some(fact) = ids.get(local).copied().flatten() else {
            continue;
        };
        let Some(source) = entity.record.evidence.source.as_ref() else {
            continue;
        };
        if names.get(key) != Some(&1)
            || entity.record.language != "make"
            || entity.record.kind != EntityKind::BuildTarget
            || entity.direct_parent != ContainerRef::File(full.span().file())
            || entity.record.container != Some(entity.direct_parent)
            || entity.definition_local_id != Some(*definition)
            || *source != super::source_for_span(full, physical(fact.span())?)
            || !same(
                text(*name_span)?.as_bytes(),
                entity.record.canonical_name.as_bytes(),
                cancel,
            )?
        {
            continue;
        }
        admitted
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        admitted.insert(entity.record.id);
    }
    Ok(admitted)
}

/// One invocation owns one immutable source and unique written target census.
/// Cache the header interval once, avoiding a full rescan for each prerequisite.
#[derive(Default)]
pub(super) struct HeaderCache {
    ranges: HashMap<SymbolId, Option<(usize, usize)>>,
}

impl HeaderCache {
    /// Preserve the existing occurrence edge and add only current-source ownership.
    /// The caller independently requires one written definition per endpoint symbol.
    ///
    /// # Errors
    /// Returns cancellation, allocation or identity errors without a partial edge.
    pub(super) fn relation(
        &mut self,
        owner: &MaterializedEntity,
        target: &EntityRecord,
        occurrence: &OccurrenceRecord,
        spans: (SourceSpan, SourceSpan),
        header: &str,
        cancel: &Cancellation,
    ) -> Result<Option<RelationRecord>, AdapterError> {
        cancel.check()?;
        let (owner_span, reference_span) = spans;
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let Some(target_source) = target.evidence.source.as_ref() else {
            return Ok(None);
        };
        let source = &occurrence.source;
        if owner.record.language != "make"
            || target.language != "make"
            || owner.record.kind != EntityKind::BuildTarget
            || target.kind != EntityKind::BuildTarget
            || owner.record.id == target.id
            || owner.record.repository != source.repository()
            || target.repository != source.repository()
            || owner.record.generation != source.generation()
            || target.generation != source.generation()
            || owner.record.provenance != occurrence.provenance
            || target.provenance != occurrence.provenance
            || owner.direct_parent != ContainerRef::File(source.span().file())
            || owner.record.container != Some(owner.direct_parent)
            || target.container != Some(owner.direct_parent)
            || !super::span_contains(owner_source.span(), source.span())
            || *owner_source != super::source_for_span(source, owner_source.span())
            || *target_source != super::source_for_span(source, target_source.span())
            || occurrence.enclosing != Some(owner.record.id)
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.syntax_kind != "make.target.reference"
            || occurrence.target != (OccurrenceTarget::Resolved { symbol: target.id })
            || occurrence.evidence.source.as_ref() != Some(source)
            || !super::span_contains(owner_span, reference_span)
        {
            return Ok(None);
        }
        let Some((start, end)) = reference_span
            .start_byte()
            .checked_sub(owner_span.start_byte())
            .zip(
                reference_span
                    .end_byte()
                    .checked_sub(owner_span.start_byte()),
            )
            .and_then(|(start, end)| {
                Some((usize::try_from(start).ok()?, usize::try_from(end).ok()?))
            })
        else {
            return Ok(None);
        };
        let normal = if let Some(normal) = self.ranges.get(&owner.record.id) {
            *normal
        } else {
            let normal = normal_header(
                header.as_bytes(),
                owner.record.canonical_name.as_bytes(),
                cancel,
            )?;
            self.ranges
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.ranges.insert(owner.record.id, normal);
            normal
        };
        let Some(normal) = normal else {
            return Ok(None);
        };
        if !exact_operand(
            header.as_bytes(),
            target.canonical_name.as_bytes(),
            start,
            end,
            normal,
            cancel,
        )? {
            return Ok(None);
        }
        let mut relation = super::lexical_reference_relation(occurrence, target.id)?;
        relation.subject = RelationEndpoint::Entity(owner.record.id);
        relation.predicate = RelationPredicate::DependsOn;
        relation
            .evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        relation.evidence.derivation.extend([
            FactRef::Entity(owner.record.id),
            FactRef::Fact(occurrence.id),
            FactRef::Entity(target.id),
        ]);
        relation.evidence.derivation.sort_unstable();
        relation.id = derive_relation_record_id(&relation)
            .map_err(|_| super::provider_failure("make-prerequisite-relation-identity"))?;
        Ok(Some(relation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal_operand(
        header: &[u8],
        owner: &[u8],
        target: &[u8],
        start: usize,
        end: usize,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(normal) = normal_header(header, owner, cancel)? else {
            return Ok(false);
        };
        exact_operand(header, target, start, end, normal, cancel)
    }

    #[test]
    fn only_exact_normal_header_tokens_prove_written_dependencies() {
        for source in [
            "all: check",
            "all : check # note",
            "all: check | cache",
            "all: check; echo ready",
            "all: check\r\n\techo ready",
        ] {
            let at = source.find("check").unwrap();
            assert!(
                normal_operand(
                    source.as_bytes(),
                    b"all",
                    b"check",
                    at,
                    at + 5,
                    &Cancellation::new()
                )
                .unwrap(),
                "{source}"
            );
        }
        for source in [
            "all: | check",
            "all:: check",
            "all other: check",
            "all: X = check",
            "all: $(TARGET) check",
            "all: check \\\n more",
            "all:\n\techo check",
            "all: # check",
            "all: checker",
            "all: check$(TAIL)",
            "all: check\rASSIGN = value",
            ".PHONY: check",
        ] {
            let at = source.find("check").unwrap();
            let owner = if source.starts_with('.') {
                b".PHONY".as_slice()
            } else {
                b"all"
            };
            assert!(
                !normal_operand(
                    source.as_bytes(),
                    owner,
                    b"check",
                    at,
                    at + 5,
                    &Cancellation::new()
                )
                .unwrap(),
                "{source}"
            );
        }
        assert!(
            !normal_operand(
                b"all: checker",
                b"all",
                b"check",
                5,
                10,
                &Cancellation::new()
            )
            .unwrap()
        );
        assert!(
            !normal_operand(b"all: check", b"all", b"check", 6, 10, &Cancellation::new()).unwrap()
        );
    }

    #[test]
    fn cancelled_header_proof_never_admits_an_edge() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            normal_operand(b"all: check", b"all", b"check", 5, 10, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}

#[cfg(test)]
mod source_context_tests {
    use super::*;

    #[test]
    fn physical_headers_do_not_enter_recipe_define_or_continued_contexts() {
        let source = b"package: verify\r\nverify:\r\n\tmasked:\r\nvalue := text \\\ncontinued:\n";
        let starts = source_line_starts(source, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert!(starts.contains(&0));
        assert!(starts.contains(&17));
        for needle in [b"masked:".as_slice(), b"continued:".as_slice()] {
            let at = source
                .windows(needle.len())
                .position(|value| value == needle)
                .unwrap();
            assert!(!starts.contains(&at));
        }
        for source in [
            b"define TEMPLATE\nverify:\nendef\n".as_slice(),
            b"define\\\nTEMPLATE\nverify:\nendef\n".as_slice(),
            b"define TEMPLATE \\\nmore\nverify:\nendef\n".as_slice(),
            b"private override export define TEMPLATE\nverify:\nendef\n".as_slice(),
            b"override define TEMPLATE\nverify:\nendef\n".as_slice(),
            b".RECIPEPREFIX := >\nverify:\n".as_slice(),
            b"export .RECIPEPREFIX := >\nverify:\n".as_slice(),
        ] {
            assert!(
                source_line_starts(source, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
        let commented = b"package: verify\n# masked \\\nverify:\n";
        let starts = source_line_starts(commented, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert!(starts.contains(&0));
        let hidden = commented
            .windows(7)
            .rposition(|word| word == b"verify:")
            .unwrap();
        assert!(!starts.contains(&hidden));
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            source_line_starts(source, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
