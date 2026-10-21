//! Prove direct RHS names in a closed source-owned Agda declaration environment.
//! The raw statement census precedes ItemCollector's name merge; no elaboration,
//! local binding, application Calls, or imported namespace is inferred here.

use std::collections::BTreeMap;

use super::{Item, ItemKind, WrittenStatement};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;

#[derive(Clone, Copy)]
pub(super) struct Reference {
    pub(super) owner: usize,
    pub(super) target: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

#[derive(Default)]
struct Census {
    signature: Option<(usize, usize)>,
    equation: Option<(usize, usize)>,
    signatures: usize,
    equations: usize,
}

fn key(bytes: &[u8], cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in bytes.chunks(64) {
        cancel.check()?;
        hash.update(chunk);
    }
    Ok(ContentHash::from_bytes(*hash.finalize().as_bytes()))
}

fn equal(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
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

fn space(
    code: &[u8],
    mut at: usize,
    end: usize,
    cancel: &Cancellation,
) -> Result<usize, AdapterError> {
    while at < end && code[at].is_ascii_whitespace() {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        at += 1;
    }
    Ok(at)
}

fn word(
    code: &[u8],
    mut at: usize,
    end: usize,
    cancel: &Cancellation,
) -> Result<usize, AdapterError> {
    while at < end && !code[at].is_ascii_whitespace() {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        at += 1;
    }
    Ok(at)
}

fn ordinary_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'%' | b'\'')
}

fn ordinary(bytes: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    if bytes.first().is_none_or(|byte| !byte.is_ascii_alphabetic()) {
        return Ok(false);
    }
    for chunk in bytes.chunks(64) {
        cancel.check()?;
        if !chunk.iter().copied().all(ordinary_byte) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn options_only(source: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    let mut at = 0;
    while at < source.len() {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        if source.get(at..at + 3) != Some(b"{-#") {
            at += 1;
            continue;
        }
        at = space(source, at + 3, source.len(), cancel)?;
        let end = word(source, at, source.len(), cancel)?;
        if source.get(at..end) != Some(b"OPTIONS") {
            return Ok(false);
        }
        at = end;
        loop {
            if at.is_multiple_of(64) {
                cancel.check()?;
            }
            if source.get(at..at + 3) == Some(b"#-}") {
                at += 3;
                break;
            }
            if at == source.len() {
                return Ok(false);
            }
            at += 1;
        }
    }
    Ok(true)
}

// A possible signature binder with this spelling is enough to withhold proof.
// This deliberately does not guess which dependent type names are implicit.
fn signature_mentions(
    code: &[u8],
    start: usize,
    end: usize,
    name: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut at = start;
    while at < end {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        if !ordinary_byte(code[at]) {
            at += 1;
            continue;
        }
        let first = at;
        while at < end && ordinary_byte(code[at]) {
            if at.is_multiple_of(64) {
                cancel.check()?;
            }
            at += 1;
        }
        if equal(&code[first..at], name, cancel)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn body_head(
    code: &[u8],
    source: &[u8],
    item: &Item,
    cancel: &Cancellation,
) -> Result<Option<(usize, usize)>, AdapterError> {
    let Some((_, equation_end)) = item.equation else {
        return Ok(None);
    };
    let equals = space(code, equation_end, item.declaration_end, cancel)?;
    if code.get(equals) != Some(&b'=')
        || code
            .get(equals + 1)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        return Ok(None);
    }
    let start = space(code, equals + 1, item.declaration_end, cancel)?;
    for (offset, (&masked, &raw)) in code[equation_end..start]
        .iter()
        .zip(&source[equation_end..start])
        .enumerate()
    {
        if offset.is_multiple_of(64) {
            cancel.check()?;
        }
        if masked != raw && !raw.is_ascii_whitespace() {
            return Ok(None);
        }
    }
    if code
        .get(start)
        .is_none_or(|byte| !byte.is_ascii_alphabetic())
    {
        return Ok(None);
    }
    let mut end = start;
    while end < item.declaration_end && ordinary_byte(code[end]) {
        if end.is_multiple_of(64) {
            cancel.check()?;
        }
        end += 1;
    }
    if !ordinary(&code[start..end], cancel)? {
        return Ok(None);
    }
    let mut at = end;
    let mut depth = 0_usize;
    let mut argument = false;
    let mut needs_atom = false;
    while at < item.declaration_end {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        if code[at] != source[at] && !source[at].is_ascii_whitespace() {
            return Ok(None);
        }
        match code[at] {
            byte if byte.is_ascii_whitespace() => at += 1,
            b'(' => {
                depth += 1;
                needs_atom = true;
                at += 1;
            }
            b')' if depth > 0 && !needs_atom => {
                depth -= 1;
                at += 1;
            }
            byte if byte.is_ascii_alphabetic() => {
                let begin = at;
                while at < item.declaration_end && ordinary_byte(code[at]) {
                    if at.is_multiple_of(64) {
                        cancel.check()?;
                    }
                    at += 1;
                }
                if matches!(
                    &code[begin..at],
                    b"let"
                        | b"where"
                        | b"do"
                        | b"with"
                        | b"rewrite"
                        | b"in"
                        | b"syntax"
                        | b"open"
                        | b"module"
                ) {
                    return Ok(None);
                }
                argument = true;
                needs_atom = false;
            }
            _ => return Ok(None),
        }
    }
    Ok((argument && depth == 0 && !needs_atom).then_some((start, end)))
}

pub(super) fn collect(
    source: &[u8],
    code: &[u8],
    statements: &[WrittenStatement],
    items: &[Item],
    incomplete: bool,
    cancel: &Cancellation,
) -> Result<Vec<Reference>, AdapterError> {
    let mut references = Vec::new();
    if incomplete || !options_only(source, cancel)? || items.is_empty() {
        return Ok(references);
    }
    for item in items {
        cancel.check()?;
        if item.kind != ItemKind::Function || item.parent_item.is_some() {
            return Ok(references);
        }
    }
    let mut base = None;
    for statement in statements {
        cancel.check()?;
        base = Some(base.map_or(statement.indent, |old: usize| old.min(statement.indent)));
    }
    let mut census = BTreeMap::<ContentHash, Census>::new();
    for statement in statements {
        cancel.check()?;
        if Some(statement.indent) != base {
            continue;
        }
        let first = statement.content;
        let end = word(code, first, statement.end, cancel)?;
        let next = space(code, end, statement.end, cancel)?;
        let next_end = word(code, next, statement.end, cancel)?;
        let entry = census.entry(key(&code[first..end], cancel)?).or_default();
        if code.get(next..next_end) == Some(b":") {
            entry.signatures += 1;
            entry.signature = Some((first, end));
        } else {
            let mut at = next;
            let mut equals = false;
            while at < statement.end {
                let tail = word(code, at, statement.end, cancel)?;
                equals |= code.get(at..tail) == Some(b"=");
                at = space(code, tail, statement.end, cancel)?;
            }
            if !equals {
                return Ok(Vec::new());
            }
            entry.equations += 1;
            entry.equation = Some((first, end));
        }
    }
    let mut names = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        cancel.check()?;
        let name = &code[item.name_start..item.name_end];
        let digest = key(name, cancel)?;
        let Some(raw) = census.get(&digest) else {
            return Ok(Vec::new());
        };
        if !item.has_signature
            || raw.signatures != 1
            || raw.equations != 1
            || raw.signature != Some((item.name_start, item.name_end))
            || raw.equation != item.equation
            || names.insert(digest, index).is_some()
        {
            return Ok(Vec::new());
        }
    }
    if census.len() != items.len() {
        return Ok(Vec::new());
    }
    for (owner, item) in items.iter().enumerate() {
        cancel.check()?;
        let Some((start, end)) = body_head(code, source, item, cancel)? else {
            continue;
        };
        let name = &code[start..end];
        let Some(&target) = names.get(&key(name, cancel)?) else {
            continue;
        };
        let declaration = &items[target];
        let Some((definition_start, _)) = declaration.equation else {
            continue;
        };
        if definition_start >= item.line_start
            || declaration.declaration_end > item.line_start
            || !equal(
                name,
                &code[declaration.name_start..declaration.name_end],
                cancel,
            )?
            || signature_mentions(code, item.name_end, item.line_end, name, cancel)?
        {
            continue;
        }
        references
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        references.push(Reference {
            owner,
            target,
            start,
            end,
        });
    }
    cancel.check()?;
    Ok(references)
}
