//! Join const certificates to current code tokens, raw witnesses and exact variables.
//! This rejects shadowing or writes conservatively and proves no runtime value.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "typescript.const_value.reference"
}

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "typescript.const_clean_file.signature"
                | "typescript.const_target.signature"
                | "typescript.const_binding.signature"
                | "typescript.const_call_rhs.signature"
        )
}

fn digest(raw: &[u8], cancel: &Cancellation) -> Result<Option<ContentHash>, AdapterError> {
    cancel.check()?;
    if !raw
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
    {
        return Ok(None);
    }
    let mut hash = blake3::Hasher::new();
    for chunk in raw.chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            return Ok(None);
        }
        hash.update(chunk);
    }
    Ok(Some(ContentHash::from_bytes(*hash.finalize().as_bytes())))
}

fn equal(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
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

#[derive(Clone)]
struct Token {
    range: Range<usize>,
    scope: Option<usize>,
    name: bool,
}

struct Program<'source> {
    source: &'source [u8],
    tokens: Vec<Token>,
    braces: HashMap<usize, (Option<usize>, usize)>,
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn reserved(raw: &[u8]) -> bool {
    matches!(
        raw,
        b"await"
            | b"break"
            | b"case"
            | b"catch"
            | b"class"
            | b"const"
            | b"continue"
            | b"debugger"
            | b"default"
            | b"delete"
            | b"do"
            | b"else"
            | b"enum"
            | b"export"
            | b"extends"
            | b"false"
            | b"finally"
            | b"for"
            | b"function"
            | b"if"
            | b"import"
            | b"in"
            | b"instanceof"
            | b"let"
            | b"new"
            | b"null"
            | b"return"
            | b"super"
            | b"switch"
            | b"this"
            | b"throw"
            | b"true"
            | b"try"
            | b"typeof"
            | b"var"
            | b"void"
            | b"while"
            | b"with"
            | b"yield"
            | b"implements"
            | b"interface"
            | b"package"
            | b"private"
            | b"protected"
            | b"public"
            | b"static"
    )
}

impl<'source> Program<'source> {
    /// Scan once, polling every source byte. Opaque strings never supply code tokens.
    /// Regex literals and nontrivial template interpolation are intentionally refused.
    fn new(source: &'source [u8], cancel: &Cancellation) -> Result<Option<Self>, AdapterError> {
        let mut output = Self {
            source,
            tokens: Vec::new(),
            braces: HashMap::new(),
        };
        let mut stack = Vec::new();
        let mut scope = None;
        let mut at = usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3;
        if source.get(at..at + 2) == Some(b"#!") {
            while at < source.len() && source[at] != b'\n' {
                cancel.check()?;
                at += 1;
            }
        }
        while at < source.len() {
            cancel.check()?;
            let start = at;
            let byte = source[at];
            if byte.is_ascii_whitespace() {
                at += 1;
                continue;
            }
            if source.get(at..at + 2) == Some(b"//") {
                while at < source.len() && source[at] != b'\n' {
                    cancel.check()?;
                    at += 1;
                }
                continue;
            }
            if source.get(at..at + 2) == Some(b"/*") {
                at += 2;
                while at + 1 < source.len() && source.get(at..at + 2) != Some(b"*/") {
                    cancel.check()?;
                    at += 1;
                }
                if at + 1 >= source.len() {
                    return Ok(None);
                }
                at += 2;
                continue;
            }
            let mut name = false;
            if matches!(byte, b'\'' | b'"' | b'`') {
                let quote = byte;
                at += 1;
                let mut closed = false;
                while at < source.len() {
                    cancel.check()?;
                    if source[at] == b'\\' {
                        at = at.saturating_add(2);
                        continue;
                    }
                    if quote == b'`' && source.get(at..at + 2) == Some(b"${") {
                        at += 2;
                        let first = at;
                        while at < source.len() && source[at] != b'}' {
                            cancel.check()?;
                            if !(word(source[at]) || source[at] == b'.') {
                                return Ok(None);
                            }
                            at += 1;
                        }
                        if at == first || at >= source.len() {
                            return Ok(None);
                        }
                        at += 1;
                        continue;
                    }
                    if source[at] == quote {
                        at += 1;
                        closed = true;
                        break;
                    }
                    if quote != b'`' && matches!(source[at], b'\r' | b'\n') {
                        return Ok(None);
                    }
                    at += 1;
                }
                if !closed || at > source.len() {
                    return Ok(None);
                }
            } else if word(byte) {
                at += 1;
                while source.get(at).is_some_and(|byte| word(*byte)) {
                    cancel.check()?;
                    at += 1;
                }
                name = true;
                // These constructs can introduce binding/evaluation contexts outside
                // this first slice. Refusal is file-wide, not a runtime conclusion.
                if matches!(
                    &source[start..at],
                    b"function"
                        | b"class"
                        | b"interface"
                        | b"namespace"
                        | b"module"
                        | b"declare"
                        | b"with"
                        | b"eval"
                ) {
                    return Ok(None);
                }
            } else {
                if matches!(byte, b'/' | b'\\' | 0) || source.get(at..at + 2) == Some(b"=>") {
                    return Ok(None);
                }
                at += 1;
            }
            output
                .tokens
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            output.tokens.push(Token {
                range: start..at,
                scope,
                name,
            });
            match byte {
                b'{' | b'(' | b'[' if !name => {
                    stack
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    stack.push((byte, start, scope));
                    if byte == b'{' {
                        scope = Some(start);
                    }
                }
                b'}' | b')' | b']' if !name => {
                    let Some((open, begin, parent)) = stack.pop() else {
                        return Ok(None);
                    };
                    if !matches!((open, byte), (b'{', b'}') | (b'(', b')') | (b'[', b']')) {
                        return Ok(None);
                    }
                    if byte == b'}' {
                        output
                            .braces
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        output.braces.insert(begin, (parent, at));
                        scope = parent;
                    }
                }
                _ => {}
            }
        }
        Ok(stack.is_empty().then_some(output))
    }

    fn token(&self, range: &Range<usize>) -> Option<usize> {
        self.tokens
            .binary_search_by_key(&range.start, |token| token.range.start)
            .ok()
            .filter(|index| self.tokens[*index].range == *range)
    }

    fn raw(&self, index: usize) -> &[u8] {
        &self.source[self.tokens[index].range.clone()]
    }

    fn value_name(&self, index: usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(_) = self.tokens.get(index) else {
            return Ok(false);
        };
        let raw = self.raw(index);
        Ok(!reserved(raw) && digest(raw, cancel)?.is_some())
    }

    fn atom(&self, index: usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(_) = self.tokens.get(index) else {
            return Ok(false);
        };
        let raw = self.raw(index);
        if matches!(raw, b"true" | b"false" | b"null") {
            return Ok(true);
        }
        if raw.first().is_some_and(u8::is_ascii_digit) {
            if raw.len() > 1 && raw.first() == Some(&b'0') {
                return Ok(false);
            }
            for chunk in raw.chunks(64) {
                cancel.check()?;
                if !chunk.iter().all(u8::is_ascii_digit) {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if let Some(quote @ (b'\'' | b'"')) = raw.first().copied() {
            if raw.len() < 2 || raw.last() != Some(&quote) {
                return Ok(false);
            }
            for chunk in raw[1..raw.len() - 1].chunks(64) {
                cancel.check()?;
                if chunk
                    .iter()
                    .any(|byte| *byte == quote || matches!(byte, b'\\' | b'\r' | b'\n' | 0))
                {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        self.value_name(index, cancel)
    }

    fn initializer(
        &self,
        begin: usize,
        end: usize,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if begin >= end || end > self.tokens.len() {
            return Ok(false);
        }
        let mut at = begin;
        let awaited = self.raw(at) == b"await";
        at += usize::from(awaited);
        if at >= end {
            return Ok(false);
        }
        if !self.value_name(at, cancel)? {
            return Ok(!awaited && at + 1 == end && self.atom(at, cancel)?);
        }
        at += 1;
        let mut dotted = false;
        while at < end && self.raw(at) == b"." {
            cancel.check()?;
            dotted = true;
            at += 1;
            if at >= end || digest(self.raw(at), cancel)?.is_none() {
                return Ok(false);
            }
            at += 1;
        }
        if at == end {
            return Ok(!awaited && !dotted);
        }
        if self.raw(at) != b"(" {
            return Ok(false);
        }
        at += 1;
        if at >= end {
            return Ok(false);
        }
        if self.raw(at) != b")" {
            loop {
                cancel.check()?;
                if at >= end || !self.atom(at, cancel)? {
                    return Ok(false);
                }
                at += 1;
                if at >= end {
                    return Ok(false);
                }
                if self.raw(at) == b")" {
                    break;
                }
                if self.raw(at) != b"," {
                    return Ok(false);
                }
                at += 1;
            }
        }
        Ok(at + 1 == end)
    }

    fn constant(
        &self,
        declaration: &Range<usize>,
        definition: &Range<usize>,
        cancel: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        cancel.check()?;
        let Some(index) = self.token(definition) else {
            return Ok(None);
        };
        let Ok(last) = self
            .tokens
            .binary_search_by_key(&declaration.end, |token| token.range.end)
        else {
            return Ok(None);
        };
        if index == 0
            || !self.tokens[index].name
            || declaration.start != definition.start
            || self.raw(index - 1) != b"const"
            || self.tokens[index - 1].scope != self.tokens[index].scope
            || self
                .tokens
                .get(index + 1)
                .is_none_or(|_| self.raw(index + 1) != b"=")
            || self
                .tokens
                .get(index + 2)
                .is_none_or(|token| token.range.end > declaration.end)
            || !self.initializer(index + 2, last + 1, cancel)?
        {
            return Ok(None);
        }
        Ok(Some(index))
    }

    fn call_slot(
        &self,
        declaration: &Range<usize>,
        reference: &Range<usize>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(start) = self
            .tokens
            .binary_search_by_key(&declaration.start, |token| token.range.start)
            .ok()
        else {
            return Ok(false);
        };
        if self
            .constant(declaration, &self.tokens[start].range, cancel)?
            .is_none()
        {
            return Ok(false);
        }
        let mut at = start + 2;
        if self.tokens.get(at).is_none_or(|token| !token.name)
            || digest(self.raw(at), cancel)?.is_none()
        {
            return Ok(false);
        }
        let mut selected = self.tokens[at].range == *reference;
        at += 1;
        if self.tokens.get(at).is_none_or(|_| self.raw(at) != b"(") {
            return Ok(false);
        }
        at += 1;
        if self.tokens.get(at).is_none() {
            return Ok(false);
        }
        if self.raw(at) != b")" {
            loop {
                cancel.check()?;
                if self.tokens.get(at).is_none_or(|token| !token.name)
                    || digest(self.raw(at), cancel)?.is_none()
                {
                    return Ok(false);
                }
                selected |= self.tokens[at].range == *reference;
                at += 1;
                if self.tokens.get(at).is_none() {
                    return Ok(false);
                }
                if self.raw(at) == b")" {
                    break;
                }
                if self.raw(at) != b"," {
                    return Ok(false);
                }
                at += 1;
            }
        }
        Ok(selected && self.tokens[at].range.end == declaration.end)
    }

    fn visible(
        &self,
        target: &Range<usize>,
        reference: &Range<usize>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let (Some(target), Some(reference)) = (self.token(target), self.token(reference)) else {
            return Ok(false);
        };
        let target = self.tokens[target].scope;
        let mut current = self.tokens[reference].scope;
        loop {
            cancel.check()?;
            if current == target {
                return Ok(true);
            }
            let Some((parent, _)) = current.and_then(|start| self.braces.get(&start)) else {
                return Ok(false);
            };
            current = *parent;
        }
    }
}

struct Binding {
    target: SymbolId,
    span: SourceSpan,
    provenance: FactId,
    owner: SymbolId,
    spelling: ContentHash,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn range(span: SourceSpan) -> Option<Range<usize>> {
    Some(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
}

fn unique<'a>(
    facts: &'a [&'a SyntaxFact],
    kind: SyntaxFactKind,
    syntax: &str,
) -> Option<&'a SyntaxFact> {
    let mut selected = None;
    for fact in facts {
        // All span buckets have a strict finite size before this helper is called.
        if fact.kind() == kind && fact.syntax_kind().as_str() == syntax {
            if selected.is_some() {
                return None;
            }
            selected = Some(*fact);
        }
    }
    selected
}

fn chain(
    fact: &SyntaxFact,
    module: &SyntaxFact,
    ids: &HashMap<u64, &SyntaxFact>,
    source: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut child = fact;
    for _ in 0..ids.len() {
        cancel.check()?;
        let Some(parent) = child.parent().and_then(|id| ids.get(&id).copied()) else {
            return Ok(false);
        };
        if parent.span().file() != child.span().file()
            || parent.span().start_byte() > child.span().start_byte()
            || parent.span().end_byte() < child.span().end_byte()
        {
            return Ok(false);
        }
        if parent.local_id() == module.local_id() {
            return Ok(true);
        }
        let Some(raw) = range(parent.span()).and_then(|range| source.get(range)) else {
            return Ok(false);
        };
        if parent.kind() != SyntaxFactKind::Scope
            || !match parent.syntax_kind().as_str() {
                "typescript.block.scope" => raw.first() == Some(&b'{') && raw.last() == Some(&b'}'),
                "typescript.for.scope" => raw.starts_with(b"for") && raw.last() == Some(&b'}'),
                _ => false,
            }
        {
            return Ok(false);
        }
        child = parent;
    }
    Ok(false)
}

impl Targets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let mut ids = HashMap::new();
        let mut spans: HashMap<SourceSpan, Vec<&SyntaxFact>> = HashMap::new();
        let mut windows: HashMap<u64, Option<&SyntaxFact>> = HashMap::new();
        let mut definitions: HashMap<u64, Option<&SyntaxFact>> = HashMap::new();
        let mut root = None;
        let mut module = None;
        let mut clean = None;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::ErrorRecovery {
                return Ok(result);
            }
            ids.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if ids.insert(fact.local_id(), fact).is_some() {
                return Ok(result);
            }
            spans
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            let rows = spans.entry(fact.span()).or_default();
            if rows.len() >= 12 {
                return Ok(result);
            }
            rows.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            rows.push(fact);
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "typescript.const_target.signature"
            {
                windows
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                windows
                    .entry(fact.span().end_byte())
                    .and_modify(|old| *old = None)
                    .or_insert(Some(fact));
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
            {
                definitions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                definitions
                    .entry(fact.span().start_byte())
                    .and_modify(|old| *old = None)
                    .or_insert(Some(fact));
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, "typescript.program.root") => {
                    if root.replace(fact).is_some() {
                        return Ok(result);
                    }
                }
                (SyntaxFactKind::Module, "typescript.file.module") => {
                    if module.replace(fact).is_some() {
                        return Ok(result);
                    }
                }
                (SyntaxFactKind::Signature, "typescript.const_clean_file.signature") => {
                    if clean.replace(fact).is_some() {
                        return Ok(result);
                    }
                }
                (SyntaxFactKind::Root | SyntaxFactKind::Module, _) => return Ok(result),
                _ => {}
            }
        }
        let (Some(root), Some(module), Some(clean)) = (root, module, clean) else {
            return Ok(result);
        };
        if physical(root.span())? != source.span()
            || root.parent().is_some()
            || module.parent() != Some(root.local_id())
            || module.span() != root.span()
            || clean.span() != root.span()
            || clean.parent() != Some(module.local_id())
        {
            return Ok(result);
        }
        if spans.get(&root.span()).is_none_or(|rows| {
            rows.iter().any(|row| {
                row.kind() == SyntaxFactKind::Signature
                    && row.syntax_kind().as_str() != "typescript.const_clean_file.signature"
            })
        }) {
            return Ok(result);
        }
        let full = text(root.span())?;
        let Some(program) = Program::new(full.as_bytes(), cancel)? else {
            return Ok(result);
        };
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.kind == EntityKind::Module && entity.record.language == "typescript"
        }) else {
            return Ok(result);
        };
        let mut references: HashMap<ContentHash, HashSet<Range<usize>>> = HashMap::new();
        let mut certified_definitions: HashMap<ContentHash, Range<usize>> = HashMap::new();
        let mut rejected = HashSet::new();
        let mut candidates = Vec::new();
        for fact in facts {
            cancel.check()?;
            if !required(fact) {
                continue;
            }
            let Some(reference_range) = range(fact.span()) else {
                continue;
            };
            let Some(key) = digest(text(fact.span())?.as_bytes(), cancel)? else {
                continue;
            };
            let Some(rows) = spans.get(&fact.span()) else {
                continue;
            };
            if unique(
                rows,
                SyntaxFactKind::Occurrence,
                "typescript.const_value.reference",
            )
            .map(SyntaxFact::local_id)
                != Some(fact.local_id())
                || rows
                    .iter()
                    .filter(|row| row.kind() == SyntaxFactKind::Occurrence)
                    .any(|row| {
                        !matches!(
                            row.syntax_kind().as_str(),
                            "typescript.const_value.reference" | "typescript.call.call"
                        )
                    })
                || rows
                    .iter()
                    .filter(|row| {
                        row.kind() == SyntaxFactKind::Occurrence
                            && row.syntax_kind().as_str() == "typescript.call.call"
                    })
                    .count()
                    > 1
            {
                continue;
            }
            let Some(owner) = fact
                .parent()
                .and_then(|id| ids.get(&id).copied())
                .filter(|owner| {
                    owner.kind() == SyntaxFactKind::Declaration
                        && owner.syntax_kind().as_str() == "typescript.variable.declaration"
                })
            else {
                continue;
            };
            let Some(owner_range) = range(owner.span()) else {
                continue;
            };
            if !program.call_slot(&owner_range, &reference_range, cancel)?
                || !chain(owner, module, &ids, full.as_bytes(), cancel)?
            {
                continue;
            }
            let Some(owner_rows) = spans.get(&owner.span()) else {
                continue;
            };
            if unique(
                owner_rows,
                SyntaxFactKind::Declaration,
                "typescript.variable.declaration",
            )
            .map(SyntaxFact::local_id)
                != Some(owner.local_id())
                || owner_rows
                    .iter()
                    .filter(|row| row.kind() == SyntaxFactKind::Declaration)
                    .count()
                    != 1
                || owner_rows.iter().any(|row| {
                    row.kind() == SyntaxFactKind::Signature
                        && !matches!(
                            row.syntax_kind().as_str(),
                            "typescript.const_binding.signature"
                                | "typescript.const_call_rhs.signature"
                        )
                })
                || unique(
                    owner_rows,
                    SyntaxFactKind::Signature,
                    "typescript.const_call_rhs.signature",
                )
                .is_none_or(|marker| marker.parent() != Some(owner.local_id()))
            {
                continue;
            }
            let Some(window) = windows.get(&fact.span().end_byte()).copied().flatten() else {
                continue;
            };
            let Some(window_rows) = spans.get(&window.span()) else {
                continue;
            };
            if window_rows.iter().any(|row| {
                row.kind() == SyntaxFactKind::Signature
                    && row.syntax_kind().as_str() != "typescript.const_target.signature"
            }) || unique(
                window_rows,
                SyntaxFactKind::Signature,
                "typescript.const_target.signature",
            )
            .map(SyntaxFact::local_id)
                != Some(window.local_id())
            {
                continue;
            }
            let Some(definition) = definitions
                .get(&window.span().start_byte())
                .copied()
                .flatten()
                .filter(|definition| {
                    definition.syntax_kind().as_str() == "typescript.identifier.definition"
                })
            else {
                continue;
            };
            let Some(target) = definition
                .parent()
                .and_then(|id| ids.get(&id).copied())
                .filter(|target| {
                    target.kind() == SyntaxFactKind::Declaration
                        && target.syntax_kind().as_str() == "typescript.variable.declaration"
                })
            else {
                continue;
            };
            let Some(target_range) = range(target.span()) else {
                continue;
            };
            let Some(definition_range) = range(definition.span()) else {
                continue;
            };
            if target.span().end_byte() > owner.span().start_byte()
                || program
                    .constant(&target_range, &definition_range, cancel)?
                    .is_none()
                || !program.visible(&definition_range, &reference_range, cancel)?
                || window.parent() != target.parent()
                || !chain(target, module, &ids, full.as_bytes(), cancel)?
                || !equal(
                    text(definition.span())?.as_bytes(),
                    text(fact.span())?.as_bytes(),
                    cancel,
                )?
            {
                continue;
            }
            let Some(target_rows) = spans.get(&target.span()) else {
                continue;
            };
            let Some(definition_rows) = spans.get(&definition.span()) else {
                continue;
            };
            if target_rows
                .iter()
                .filter(|row| row.kind() == SyntaxFactKind::Declaration)
                .count()
                != 1
                || target_rows.iter().any(|row| {
                    row.kind() == SyntaxFactKind::Signature
                        && !matches!(
                            row.syntax_kind().as_str(),
                            "typescript.const_binding.signature"
                                | "typescript.const_call_rhs.signature"
                        )
                })
                || unique(
                    target_rows,
                    SyntaxFactKind::Declaration,
                    "typescript.variable.declaration",
                )
                .map(SyntaxFact::local_id)
                    != Some(target.local_id())
                || unique(
                    target_rows,
                    SyntaxFactKind::Signature,
                    "typescript.const_binding.signature",
                )
                .is_none_or(|marker| marker.parent() != Some(target.local_id()))
                || definition_rows.len() != 1
            {
                continue;
            }
            let (Some(variable), Some(owner_entity)) = (
                entities.get(&target.local_id()),
                entities.get(&owner.local_id()),
            ) else {
                continue;
            };
            let Some(evidence) = variable.record.evidence.source.as_ref() else {
                continue;
            };
            if variable.record.kind != EntityKind::Variable
                || owner_entity.record.kind != EntityKind::Variable
                || variable.record.language != "typescript"
                || owner_entity.record.language != "typescript"
                || variable.definition_local_id != Some(definition.local_id())
                || variable.record.repository != source.repository()
                || variable.record.generation != source.generation()
                || *evidence != super::source_for_span(source, physical(target.span())?)
                || variable.record.provenance != owner_entity.record.provenance
                || owner_entity.record.repository != source.repository()
                || owner_entity.record.generation != source.generation()
                || variable.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || owner_entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || owner_entity.record.evidence.source.as_ref()
                    != Some(&super::source_for_span(source, physical(owner.span())?))
                || !equal(
                    variable.record.canonical_name.as_bytes(),
                    text(definition.span())?.as_bytes(),
                    cancel,
                )?
            {
                continue;
            }
            references
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if let Some(existing) = certified_definitions.get(&key) {
                if *existing != definition_range {
                    // Separate valid-looking chains cannot jointly authorize a
                    // spelling whose native certificate requires one binding.
                    rejected
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    rejected.insert(key);
                }
            } else {
                certified_definitions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                certified_definitions.insert(key, definition_range.clone());
            }
            let positions = references.entry(key).or_default();
            positions
                .try_reserve(2)
                .map_err(|_| SinkError::AllocationFailed)?;
            positions.insert(definition_range);
            positions.insert(reference_range);
            candidates
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            candidates.push((
                key,
                fact,
                variable.record.id,
                owner_entity.record.id,
                variable.record.provenance,
            ));
        }
        for token in &program.tokens {
            cancel.check()?;
            if !token.name {
                continue;
            }
            let Some(key) = digest(&program.source[token.range.clone()], cancel)? else {
                continue;
            };
            if references
                .get(&key)
                .is_some_and(|positions| !positions.contains(&token.range))
            {
                rejected
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                rejected.insert(key);
            }
        }
        result
            .bindings
            .try_reserve(candidates.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (key, fact, target, owner, provenance) in candidates {
            cancel.check()?;
            if rejected.contains(&key) {
                continue;
            }
            result.bindings.insert(
                fact.local_id(),
                Binding {
                    target,
                    owner,
                    provenance,
                    span: physical(fact.span())?,
                    spelling: key,
                },
            );
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok((required(fact) && binding.span == physical && occurrence.source == super::source_for_span(&self.source, physical)
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation() && occurrence.file == physical.file()
            && occurrence.role == OccurrenceRole::Reference && occurrence.syntax_kind == "typescript.const_value.reference"
            && occurrence.syntactic_text_hash == binding.spelling && occurrence.enclosing == Some(binding.owner)
            && occurrence.provenance == binding.provenance && occurrence.evidence.source.as_ref() == Some(&occurrence.source)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling)).then_some(binding.target))
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::super::{
        Lowering, NativeSourceView, TreeSitterAnalyzer, exclude_ambiguous_entities,
        materialize_entity,
    };
    use super::*;
    use rootlight_adapter_sdk::{
        AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
        LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis, execute_parse,
    };
    use rootlight_ids::{GenerationId, RepositoryId, content_hash, derive_repository};
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, ProducerIdentity,
    };
    use rootlight_vfs::{RelativePath, RepositoryRoot};
    use std::{fs, path::Path, sync::Arc};

    const ORIGINAL: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rootlight-service/tests/fixtures/typescript_value_uses/annotation-tests.ts"
    ));

    fn native_provider() -> Arc<crate::TreeSitterProvider> {
        Arc::new(
            crate::TreeSitterProvider::new(
                crate::RuntimeConfig::new(
                    1024 * 1024,
                    16_384,
                    128,
                    16,
                    64,
                    1,
                    16 * 1024 * 1024,
                    crate::ParserSettings::new(256).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        )
    }

    fn with_raw_entities(
        raw: &[u8],
        operation: impl FnOnce(
            &Lowering<'_, '_>,
            &HashMap<u64, MaterializedEntity>,
            &rootlight_ir::NormalizedIrDocument,
        ),
    ) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("written.ts"), raw).unwrap();
        let repository = derive_repository(b"typescript-const-consumer-qualification").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("written.ts")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([7; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = native_provider();
        let analyzer = TreeSitterAnalyzer::new(
            provider.clone(),
            ProducerIdentity::new(
                "typescript-const-consumer-qualification",
                "1.0",
                content_hash(b"typescript-const-config"),
            )
            .unwrap(),
            LanguageId::new("typescript").unwrap(),
            "typescript-const-consumer-qualification",
            content_hash(b"typescript-const-binary"),
        )
        .unwrap();
        let stream = || {
            StreamLimits::new(
                128,
                4096,
                16 * 1024 * 1024,
                128,
                128 * 1024,
                1024 * 1024,
                BatchThresholds::new(1, 1024 * 1024, 32, 16 * 1024).unwrap(),
            )
            .unwrap()
        };
        let limits = AnalysisLimits::new(
            1024 * 1024,
            16_384,
            128,
            16,
            16 * 1024 * 1024,
            stream(),
            stream(),
            IrLimits::default(),
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("typescript").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"typescript-const-build")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancel = Cancellation::new();
        let parsed = execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            rootlight_ir::CoverageStatus::Complete
        );
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        let view = NativeSourceView::new(
            raw,
            "utf-8",
            limits.max_source_bytes(),
            limits.max_reported_memory_bytes(),
            &cancel,
        )
        .unwrap();
        let lowering = Lowering::new(&analyzer, &request, &parsed, &view, &cancel).unwrap();
        let mut strings = 0;
        let plan = lowering.entity_drafts(&cancel, &mut strings).unwrap();
        let provenance = lowering.provenance().unwrap().id;
        let mut entities = HashMap::new();
        for draft in &plan.drafts {
            let entity = materialize_entity(
                draft,
                &source,
                &lowering.coordinates,
                draft
                    .authored_display_span
                    .map(|span| lowering.text_for_span(span))
                    .transpose()
                    .unwrap(),
                request.build_context(),
                provenance,
                analyzer.descriptor.tier(),
                limits.ir().max_string_bytes,
                &entities,
            )
            .unwrap();
            entities.insert(draft.local_id, entity);
        }
        let _excluded = exclude_ambiguous_entities(&mut entities);
        operation(&lowering, &entities, output.document());
    }

    fn with_native(
        raw: &[u8],
        operation: impl FnOnce(
            &Lowering<'_, '_>,
            &HashMap<u64, MaterializedEntity>,
            &SyntaxFact,
            &OccurrenceRecord,
            SymbolId,
            u64,
        ),
    ) {
        with_raw_entities(raw, |lowering, entities, document| {
            assert_eq!(
                lowering
                    .coordinates
                    .facts()
                    .iter()
                    .filter(|fact| {
                        required(fact)
                            && fact.span().start_byte() == 538
                            && fact.span().end_byte() == 555
                    })
                    .count(),
                1,
                "the required carrier and optional capture must share one raw reference"
            );
            let read = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "typescript.const_value.reference"
                    && site.source.span().start_byte() == 538
                    && site.source.span().end_byte() == 555
            })
            .expect(
                "the actual original must supply its native certified reference before tampering",
            );
            let OccurrenceTarget::Resolved { symbol } = read.target else {
                let witnesses: Vec<_> = lowering
                    .coordinates
                    .facts()
                    .iter()
                    .filter(|fact| {
                        metadata(fact)
                            || matches!(fact.span().start_byte(), 0 | 160 | 490 | 529 | 538 | 556)
                    })
                    .collect();
                panic!(
                    "the baseline actual original must bind before consumer tampering: {witnesses:?}"
                );
            };
            let (&local, variable) = entities
                .iter()
                .find(|(_, entity)| entity.record.id == symbol)
                .unwrap();
            assert_eq!(variable.record.kind, EntityKind::Variable);
            let definition = variable.definition_local_id.unwrap();
            assert!(
                lowering
                    .coordinates
                    .facts()
                    .iter()
                    .any(|fact| fact.local_id() == definition
                        && fact.span().start_byte() == 160
                        && fact.span().end_byte() == 177)
            );
            let fact = lowering
                .coordinates
                .facts()
                .iter()
                .find(|fact| {
                    required(fact)
                        && fact.span().start_byte() == 538
                        && fact.span().end_byte() == 555
                })
                .unwrap();
            let mut unresolved = read.clone();
            unresolved.target = OccurrenceTarget::Unresolved {
                text_hash: read.syntactic_text_hash,
            };
            operation(lowering, entities, fact, &unresolved, symbol, local);
        });
    }

    fn join(
        lowering: &Lowering<'_, '_>,
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        cancel: &Cancellation,
    ) -> Result<Targets, AdapterError> {
        Targets::new(
            facts,
            entities,
            source,
            |span| lowering.text_for_span(span),
            |span| lowering.coordinates.physical(span),
            cancel,
        )
    }

    fn duplicate(facts: &mut Vec<SyntaxFact>, original: &SyntaxFact, syntax: Option<&str>) {
        let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
        facts.push(SyntaxFact::new(
            id,
            original.parent(),
            original.kind(),
            original.span(),
            original.depth(),
            syntax
                .map(|syntax| rootlight_adapter_sdk::SyntaxKindLabel::new(syntax).unwrap())
                .unwrap_or_else(|| original.syntax_kind().clone()),
        ));
    }

    fn marker(facts: &mut Vec<SyntaxFact>, parent: &SyntaxFact, span: SourceSpan, label: &str) {
        let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
        facts.push(SyntaxFact::new(
            id,
            Some(parent.local_id()),
            SyntaxFactKind::Signature,
            span,
            parent.depth() + 1,
            rootlight_adapter_sdk::SyntaxKindLabel::new(label).unwrap(),
        ));
    }

    #[test]
    fn complete_raw_chains_cannot_whitelist_two_definitions_of_one_spelling() {
        for (source, names, admitted) in [
            (
                "const value=1; const a=consume(value); { const other=2; const b=consume(other); }",
                ["value", "other"],
                true,
            ),
            (
                "const value=1; const a=consume(value); { const value=2; const b=consume(value); }",
                ["value", "value"],
                false,
            ),
        ] {
            with_raw_entities(source.as_bytes(), |lowering, entities, document| {
                let cancel = Cancellation::new();
                let mut facts: Vec<_> = lowering
                    .coordinates
                    .facts()
                    .iter()
                    .filter(|fact| !metadata(fact))
                    .cloned()
                    .collect();
                let module = facts
                    .iter()
                    .find(|fact| fact.kind() == SyntaxFactKind::Module)
                    .unwrap()
                    .clone();
                marker(
                    &mut facts,
                    &module,
                    module.span(),
                    "typescript.const_clean_file.signature",
                );
                let mut probes = Vec::new();
                for (index, name) in names.into_iter().enumerate() {
                    let target_start = source.find(&format!("{name}={}", index + 1)).unwrap();
                    let owner_start = source
                        .find(if index == 0 { "a=consume" } else { "b=consume" })
                        .unwrap();
                    let call = format!("consume({name})");
                    let use_start = if index == 0 {
                        source.find(&call)
                    } else {
                        source.rfind(&call)
                    }
                    .unwrap()
                        + "consume(".len();
                    let target = facts
                        .iter()
                        .find(|fact| {
                            fact.kind() == SyntaxFactKind::Declaration
                                && fact.span().start_byte() == u64::try_from(target_start).unwrap()
                        })
                        .unwrap()
                        .clone();
                    let owner = facts
                        .iter()
                        .find(|fact| {
                            fact.kind() == SyntaxFactKind::Declaration
                                && fact.span().start_byte() == u64::try_from(owner_start).unwrap()
                        })
                        .unwrap()
                        .clone();
                    let reference_index = facts
                        .iter()
                        .position(|fact| {
                            fact.kind() == SyntaxFactKind::Occurrence
                                && fact.span().start_byte() == u64::try_from(use_start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(use_start + name.len()).unwrap()
                        })
                        .unwrap();
                    let reference = &facts[reference_index];
                    let reference = SyntaxFact::new(
                        reference.local_id(),
                        Some(owner.local_id()),
                        SyntaxFactKind::Occurrence,
                        reference.span(),
                        reference.depth(),
                        rootlight_adapter_sdk::SyntaxKindLabel::new(
                            "typescript.const_value.reference",
                        )
                        .unwrap(),
                    );
                    facts[reference_index] = reference.clone();
                    let target_parent = facts
                        .iter()
                        .find(|fact| Some(fact.local_id()) == target.parent())
                        .unwrap()
                        .clone();
                    marker(
                        &mut facts,
                        &target,
                        target.span(),
                        "typescript.const_binding.signature",
                    );
                    marker(
                        &mut facts,
                        &owner,
                        owner.span(),
                        "typescript.const_call_rhs.signature",
                    );
                    marker(
                        &mut facts,
                        &target_parent,
                        SourceSpan::new(
                            target.span().file(),
                            target.span().start_byte(),
                            reference.span().end_byte(),
                        )
                        .unwrap(),
                        "typescript.const_target.signature",
                    );
                    let variable = &entities[&target.local_id()];
                    let owner_entity = &entities[&owner.local_id()];
                    let mut occurrence = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.role == OccurrenceRole::Reference
                                && site.source.span() == reference.span()
                        })
                        .unwrap()
                        .clone();
                    occurrence.syntax_kind = "typescript.const_value.reference".to_owned();
                    occurrence.enclosing = Some(owner_entity.record.id);
                    occurrence.target = OccurrenceTarget::Unresolved {
                        text_hash: occurrence.syntactic_text_hash,
                    };
                    occurrence.id = rootlight_ir::derive_occurrence_record_id(&occurrence).unwrap();
                    probes.push((reference, occurrence, variable.record.id));
                }
                let targets =
                    join(lowering, &facts, entities, lowering.full_source, &cancel).unwrap();
                assert_eq!(probes.len(), 2);
                for (reference, occurrence, symbol) in probes {
                    assert_eq!(
                        targets
                            .resolve(&reference, occurrence.source.span(), &occurrence, &cancel)
                            .unwrap(),
                        admitted.then_some(symbol),
                        "{source}"
                    );
                }
            });
        }
    }

    #[test]
    fn complete_original_raw_join_refuses_missing_duplicate_and_foreign_same_span_witnesses() {
        with_native(
            ORIGINAL,
            |lowering, entities, read, occurrence, symbol, local| {
                let cancel = Cancellation::new();
                let facts = lowering.coordinates.facts();
                let source = lowering.full_source;
                let baseline = join(lowering, facts, entities, source, &cancel).unwrap();
                assert_eq!(
                    baseline
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    Some(symbol)
                );
                let definition = entities[&local].definition_local_id.unwrap();
                let witnesses: Vec<_> = facts
                    .iter()
                    .filter(|fact| {
                        fact.local_id() == read.local_id()
                            || fact.local_id() == definition
                            || fact.local_id() == local
                            || metadata(fact)
                                && (fact.syntax_kind().as_str()
                                    == "typescript.const_clean_file.signature"
                                    || fact.syntax_kind().as_str()
                                        == "typescript.const_binding.signature"
                                        && fact.span().start_byte() == 160
                                    || fact.syntax_kind().as_str()
                                        == "typescript.const_target.signature"
                                        && fact.span().end_byte() == 555
                                    || fact.syntax_kind().as_str()
                                        == "typescript.const_call_rhs.signature"
                                        && fact.parent() == read.parent())
                    })
                    .collect();
                assert_eq!(witnesses.len(), 7);
                for witness in witnesses {
                    let kept: Vec<_> = facts
                        .iter()
                        .filter(|fact| fact.local_id() != witness.local_id())
                        .cloned()
                        .collect();
                    let missing = join(lowering, &kept, entities, source, &cancel).unwrap();
                    assert_eq!(
                        missing
                            .resolve(read, occurrence.source.span(), occurrence, &cancel)
                            .unwrap(),
                        None,
                        "missing {}",
                        witness.syntax_kind()
                    );
                    let mut doubled = facts.to_vec();
                    duplicate(&mut doubled, witness, None);
                    let rejected = join(lowering, &doubled, entities, source, &cancel).unwrap();
                    assert_eq!(
                        rejected
                            .resolve(read, occurrence.source.span(), occurrence, &cancel)
                            .unwrap(),
                        None,
                        "duplicate {}",
                        witness.syntax_kind()
                    );
                    if matches!(
                        witness.kind(),
                        SyntaxFactKind::Signature
                            | SyntaxFactKind::Declaration
                            | SyntaxFactKind::Occurrence
                    ) {
                        let label = match witness.kind() {
                            SyntaxFactKind::Signature => "foreign.context.signature",
                            SyntaxFactKind::Declaration => "foreign.variable.declaration",
                            _ => "foreign.name.definition",
                        };
                        let mut foreign = facts.to_vec();
                        duplicate(&mut foreign, witness, Some(label));
                        let rejected = join(lowering, &foreign, entities, source, &cancel).unwrap();
                        assert_eq!(
                            rejected
                                .resolve(read, occurrence.source.span(), occurrence, &cancel)
                                .unwrap(),
                            None,
                            "foreign {}",
                            witness.syntax_kind()
                        );
                    }
                }
            },
        );
    }

    #[test]
    fn complete_original_join_refuses_parent_current_entity_source_and_occurrence_tampering() {
        with_native(
            ORIGINAL,
            |lowering, entities, read, occurrence, symbol, local| {
                let cancel = Cancellation::new();
                let source = lowering.full_source;
                let facts = lowering.coordinates.facts();
                assert_eq!(
                    join(lowering, facts, entities, source, &cancel)
                        .unwrap()
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    Some(symbol)
                );
                for mode in 0..4 {
                    let mut changed = entities.clone();
                    let target = changed.get_mut(&local).unwrap();
                    match mode {
                        0 => target.record.repository = RepositoryId::from_bytes([99; 16]),
                        1 => target.record.generation = GenerationId::from_bytes([99; 20]),
                        2 => target.direct_parent = ContainerRef::File(source.span().file()),
                        _ => {
                            target.record.evidence.source = Some(SourceRef::new(
                                source.repository(),
                                source.generation(),
                                target.record.evidence.source.as_ref().unwrap().span(),
                                content_hash(b"stale"),
                                None,
                            ))
                        }
                    }
                    assert_eq!(
                        join(lowering, facts, &changed, source, &cancel)
                            .unwrap()
                            .resolve(read, occurrence.source.span(), occurrence, &cancel)
                            .unwrap(),
                        None
                    );
                }
                let root = facts
                    .iter()
                    .find(|fact| fact.kind() == SyntaxFactKind::Root)
                    .unwrap();
                let changed: Vec<_> = facts
                    .iter()
                    .map(|fact| {
                        if fact.local_id() == read.local_id() {
                            SyntaxFact::new(
                                fact.local_id(),
                                Some(root.local_id()),
                                fact.kind(),
                                fact.span(),
                                1,
                                fact.syntax_kind().clone(),
                            )
                        } else {
                            fact.clone()
                        }
                    })
                    .collect();
                assert_eq!(
                    join(lowering, &changed, entities, source, &cancel)
                        .unwrap()
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    None
                );
                let baseline = join(lowering, facts, entities, source, &cancel).unwrap();
                for mode in 0..4 {
                    let mut changed = occurrence.clone();
                    match mode {
                        0 => changed.enclosing = None,
                        1 => changed.generation = GenerationId::from_bytes([99; 20]),
                        2 => changed.syntactic_text_hash = content_hash(b"foreign"),
                        _ => changed.role = OccurrenceRole::CallSite,
                    }
                    assert_eq!(
                        baseline
                            .resolve(read, changed.source.span(), &changed, &cancel)
                            .unwrap(),
                        None
                    );
                }
                let cancelled = Cancellation::new();
                cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                assert!(matches!(
                    join(lowering, facts, entities, source, &cancelled),
                    Err(AdapterError::Cancelled { .. })
                ));
            },
        );
    }

    #[test]
    fn code_census_keeps_comments_strings_and_simple_template_paths_opaque() {
        let cancel = Cancellation::new();
        let source = b"// const hidden = 0;\nconst value = 'fake(value)'; const text = `${entry.name}`; const result = consume(value);";
        let program = Program::new(source, &cancel).unwrap().unwrap();
        assert_eq!(
            program
                .tokens
                .iter()
                .filter(|token| program.source[token.range.clone()] == *b"value")
                .count(),
            2
        );
        assert!(
            !program
                .tokens
                .iter()
                .any(|token| program.source[token.range.clone()] == *b"hidden")
        );
        for source in [
            b"const value = /fake/;".as_slice(),
            b"const value = `${call()}`;",
            b"const val\\u0075e = 1;",
            b"const value = (() => missing);",
        ] {
            assert!(Program::new(source, &cancel).unwrap().is_none());
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            Program::new(b"/* long checkpoint */ const value = 1;", &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn original_certificate_cannot_resolve_against_changed_current_code() {
        with_native(ORIGINAL, |lowering, entities, read, occurrence, _, _| {
            let cancel = Cancellation::new();
            for mode in 0..2 {
                let mut current = std::str::from_utf8(ORIGINAL).unwrap().to_owned();
                if mode == 0 {
                    let declaration = current.find("const validateTestSuite").unwrap();
                    current.replace_range(declaration..declaration + 5, "let  ");
                } else {
                    let initializer = current.find("await validate").unwrap();
                    current.replace_range(initializer..initializer + 5, ".....");
                }
                assert_eq!(current.len(), ORIGINAL.len());
                let targets = Targets::new(
                    lowering.coordinates.facts(),
                    entities,
                    lowering.full_source,
                    |span| Ok(current.get(range(span).unwrap()).unwrap()),
                    |span| lowering.coordinates.physical(span),
                    &cancel,
                )
                .unwrap();
                assert_eq!(
                    targets
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    None,
                    "current code mode {mode}"
                );
            }
        });
    }

    #[test]
    fn target_initializer_tokens_refuse_malformed_and_unmodeled_source_shapes() {
        let cancel = Cancellation::new();
        for (initializer, admitted) in [
            ("1", true),
            ("'plain'", true),
            ("factory", true),
            ("true", true),
            ("null", true),
            ("await validate('plain')", true),
            ("JSON.parse(json)", true),
            (".", false),
            ("..... validate('plain')", false),
            ("[1]", false),
            ("({item: 1})", false),
            ("1 + 2", false),
            ("1.5", false),
            ("-1", false),
            ("factory.member", false),
            ("factory?.(input)", false),
            ("factory<Type>(input)", false),
            ("factory(other())", false),
            (r"'escaped\n'", false),
        ] {
            let source = format!("const value = {initializer};");
            let program = Program::new(source.as_bytes(), &cancel).unwrap().unwrap();
            let definition = 6..11;
            let declaration = 6..source.len() - 1;
            assert_eq!(
                program
                    .constant(&declaration, &definition, &cancel)
                    .unwrap()
                    .is_some(),
                admitted,
                "{initializer}"
            );
        }
    }

    fn selected_limits(ir: IrLimits, records: usize) -> AnalysisLimits {
        let syntax = StreamLimits::new(
            128,
            records,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(1, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap();
        let stream = StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(1, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap();
        AnalysisLimits::new(
            1024 * 1024,
            16_384,
            128,
            16,
            16 * 1024 * 1024,
            syntax,
            stream,
            ir,
        )
        .unwrap()
    }

    #[test]
    fn original_required_artifact_and_exact_one_less_relation_and_occurrence_reservations_match() {
        with_native(ORIGINAL, |lowering, _, _, _, _, _| {
            let cancel = Cancellation::new();
            let analyzer = lowering.analyzer;
            let request = lowering.request;
            let required = native_provider()
                .required_syntax_fact_count(&request.to_parse_request(), &cancel)
                .unwrap();
            let artifact = analyzer.prepare_artifact(request, &cancel).unwrap();
            assert_eq!(
                artifact.required_syntax_fact_count(&cancel).unwrap(),
                required
            );
            for exact in [false, true] {
                let budget = selected_limits(IrLimits::default(), required - usize::from(!exact));
                let selected = AnalysisRequest::new_with_parse_context(
                    request.source().clone(),
                    LanguageId::new("typescript").unwrap(),
                    EncodingId::utf8(),
                    Vec::new(),
                    request.tier(),
                    request.build_context(),
                    &budget,
                )
                .unwrap()
                .with_generated_status(false);
                if !exact {
                    assert!(
                        matches!(analyzer.prepare_artifact(&selected, &cancel), Err(AdapterError::Sink(SinkError::StreamLimit { resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts, observed, limit })) if observed == required && limit + 1 == required)
                    );
                    continue;
                }
                let selected_artifact = analyzer.prepare_artifact(&selected, &cancel).unwrap();
                assert_eq!(
                    selected_artifact
                        .required_syntax_fact_count(&cancel)
                        .unwrap(),
                    required
                );
                assert_eq!(
                    selected_artifact.parse_output.report().coverage().status(),
                    rootlight_ir::CoverageStatus::Bounded
                );
                let facts = selected_artifact.parse_output.facts();
                let references = facts.iter().filter(|fact| super::required(fact)).count();
                assert_eq!(references, 2);
                let entities = facts
                    .iter()
                    .filter(|fact| super::super::structural_entity_kind(fact).is_some())
                    .count();
                let occurrences = entities
                    + facts
                        .iter()
                        .filter(|fact| super::super::occurrence_role(fact).is_some())
                        .count()
                    + references;
                let relations = entities + references * 2;
                for relation_limit in [true, false] {
                    for exact in [false, true] {
                        let mut ir = IrLimits::default();
                        let observed = if relation_limit {
                            relations
                        } else {
                            occurrences
                        };
                        let limit = observed - usize::from(!exact);
                        if relation_limit {
                            ir.max_relations = limit;
                        } else {
                            ir.max_occurrences = limit;
                        }
                        let budget = selected_limits(ir, required);
                        let bounded = AnalysisRequest::new_with_parse_context(
                            request.source().clone(),
                            LanguageId::new("typescript").unwrap(),
                            EncodingId::utf8(),
                            Vec::new(),
                            request.tier(),
                            request.build_context(),
                            &budget,
                        )
                        .unwrap()
                        .with_generated_status(false);
                        let admission = super::super::preflight_lowering_limits(
                            &bounded,
                            &selected_artifact.parse_output,
                            analyzer.descriptor.identity(),
                            &analyzer.frontend_version,
                            &[],
                            &cancel,
                        );
                        if !exact {
                            let error = match admission {
                                Ok(_) => {
                                    panic!("preflight must reserve both Reference and Read facets")
                                }
                                Err(error) => error,
                            };
                            assert_eq!(
                                error,
                                AdapterError::Sink(SinkError::StreamLimit {
                                    resource: rootlight_adapter_sdk::ResourceKind::Records,
                                    observed,
                                    limit
                                })
                            );
                            continue;
                        }
                        admission.unwrap();
                        let prepared = super::super::PreparsedTreeSitterAnalyzer {
                            analyzer,
                            parse_output: &selected_artifact.parse_output,
                        };
                        let result = execute_analysis(
                            &prepared,
                            &bounded,
                            ExtensionSupport::default(),
                            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                            &cancel,
                        )
                        .unwrap();
                        assert_eq!(
                            result
                                .document()
                                .occurrences
                                .iter()
                                .filter(|site| site.role == OccurrenceRole::Read
                                    && site.syntax_kind == "typescript.const_value_read.read")
                                .count(),
                            2
                        );
                        assert_eq!(
                            result
                                .document()
                                .occurrences
                                .iter()
                                .filter(|site| site.role == OccurrenceRole::Reference
                                    && site.syntax_kind == "typescript.const_value.reference")
                                .count(),
                            2
                        );
                    }
                }
                let rejected = analyzer.analyze_prepared_artifact(
                    &selected,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                );
                assert!(
                    matches!(rejected, Err(AdapterError::ProviderFailed { code })
                    if code.as_str() == "treesitter-structural-artifact-mismatch")
                );
                let replay = analyzer
                    .analyze_prepared_artifact(
                        &selected,
                        &selected_artifact,
                        ExtensionSupport::default(),
                        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                        &cancel,
                    )
                    .unwrap();
                assert_eq!(
                    replay
                        .document()
                        .occurrences
                        .iter()
                        .filter(|site| site.role == OccurrenceRole::Read
                            && site.syntax_kind == "typescript.const_value_read.read")
                        .count(),
                    2
                );
            }
        });
    }
}
