//! Project adoption of native own-class PHP method certificates.
//! A flat current-source class census authenticates names and physical owners;
//! inherited lookup, complex bodies and runtime dispatch remain unproved.

use super::*;

pub(super) struct Proof {
    pub(super) target: SymbolId,
    pub(super) owner_local: u64,
    pub(super) derivation: Vec<FactRef>,
}

pub(super) fn site(label: &str) -> bool {
    label == "php.this_method.reference"
}

pub(super) fn mandatory(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && site(fact.syntax_kind().as_str())
}

#[derive(Clone, Copy)]
struct Token<'a> {
    bytes: &'a [u8],
    start: usize,
    end: usize,
}

struct Method<'a> {
    name: Token<'a>,
    span: SourceSpan,
    body: SourceSpan,
    calls: Vec<Token<'a>>,
}

struct Class<'a> {
    name: Token<'a>,
    span: SourceSpan,
    body: SourceSpan,
    methods: Vec<Method<'a>>,
}

fn span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(start).map_err(|_| SinkError::AccountingOverflow)?,
        u64::try_from(end).map_err(|_| SinkError::AccountingOverflow)?,
    )
    .map_err(|_| SinkError::InvalidIr.into())
}

fn key(bytes: &[u8], cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in bytes.chunks(64) {
        cancel.check()?;
        let mut folded = [0; 64];
        for (target, byte) in folded.iter_mut().zip(chunk) {
            *target = byte.to_ascii_lowercase();
        }
        hash.update(&folded[..chunk.len()]);
    }
    Ok(content_hash(hash.finalize().as_bytes()))
}

fn same(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.chunks(64).zip(right.chunks(64)) {
        cancel.check()?;
        if !left.eq_ignore_ascii_case(right) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn exact(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
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

fn word(token: Token<'_>) -> bool {
    token
        .bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
}

fn lex<'a>(bytes: &'a [u8], cancel: &Cancellation) -> Result<Option<Vec<Token<'a>>>, AdapterError> {
    let mut offset = usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3;
    if bytes.get(offset..offset + 5) != Some(b"<?php") {
        return Ok(None);
    }
    offset += 5;
    let mut tokens = Vec::new();
    while offset < bytes.len() {
        cancel.check()?;
        if bytes[offset].is_ascii_whitespace() {
            offset += 1;
            continue;
        }
        if bytes.get(offset..offset + 2) == Some(b"//") {
            while offset < bytes.len() && bytes[offset] != b'\n' {
                check_periodically(offset, cancel)?;
                offset += 1;
            }
            continue;
        }
        if bytes.get(offset..offset + 2) == Some(b"/*") {
            offset += 2;
            while offset < bytes.len() && bytes.get(offset..offset + 2) != Some(b"*/") {
                check_periodically(offset, cancel)?;
                offset += 1;
            }
            if offset == bytes.len() {
                return Ok(None);
            }
            offset += 2;
            continue;
        }
        let start = offset;
        let first = bytes[offset];
        if first.is_ascii_alphabetic() || matches!(first, b'_' | b'$') || first.is_ascii_digit() {
            offset += 1;
            while offset < bytes.len()
                && (bytes[offset].is_ascii_alphanumeric() || bytes[offset] == b'_')
            {
                check_periodically(offset, cancel)?;
                if first.is_ascii_digit() && !bytes[offset].is_ascii_digit() {
                    return Ok(None);
                }
                offset += 1;
            }
            if first == b'$'
                && (offset == start + 1
                    || !bytes[start + 1].is_ascii_alphabetic() && bytes[start + 1] != b'_')
            {
                return Ok(None);
            }
        } else if bytes.get(offset..offset + 2) == Some(b"->") {
            offset += 2;
        } else if bytes.get(offset..offset + 3) == Some(b"...") {
            offset += 3;
        } else if matches!(first, b'{' | b'}' | b'(' | b')' | b';' | b',') {
            offset += 1;
        } else {
            return Ok(None);
        }
        tokens
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        tokens.push(Token {
            bytes: &bytes[start..offset],
            start,
            end: offset,
        });
    }
    Ok(Some(tokens))
}

fn closed(
    tokens: &[Token<'_>],
    start: usize,
    cancel: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let closing = match tokens[start].bytes {
        b"{" => b"}".as_slice(),
        b"(" => b")",
        _ => return Ok(None),
    };
    let mut stack = vec![closing];
    for (index, token) in tokens.iter().enumerate().skip(start + 1) {
        cancel.check()?;
        match token.bytes {
            b"{" => stack.push(b"}"),
            b"(" => stack.push(b")"),
            b"}" | b")" => {
                if stack.pop() != Some(token.bytes) {
                    return Ok(None);
                }
                if stack.is_empty() {
                    return Ok(Some(index));
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

fn flat_calls<'a>(
    tokens: &[Token<'a>],
    cancel: &Cancellation,
) -> Result<Option<Vec<Token<'a>>>, AdapterError> {
    let mut calls = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        cancel.check()?;
        let returning = tokens[at].bytes == b"return";
        at += usize::from(returning);
        if returning && tokens.get(at).is_some_and(|token| token.bytes == b";") {
            at += 1;
            continue;
        }
        if tokens.get(at).is_some_and(|token| token.bytes == b"$this") {
            if tokens.get(at + 1).is_none_or(|token| token.bytes != b"->")
                || tokens.get(at + 2).is_none_or(|token| !word(*token))
                || tokens.get(at + 3).is_none_or(|token| token.bytes != b"(")
            {
                return Ok(None);
            }
            let name = tokens[at + 2];
            at += 4;
            while tokens.get(at).is_some_and(|token| token.bytes != b")") {
                cancel.check()?;
                if tokens[at].bytes == b"..." {
                    at += 1;
                }
                let Some(value) = tokens.get(at) else {
                    return Ok(None);
                };
                if !value
                    .bytes
                    .first()
                    .is_some_and(|byte| *byte == b'$' || byte.is_ascii_digit())
                {
                    return Ok(None);
                }
                at += 1;
                if tokens.get(at).is_some_and(|token| token.bytes == b",") {
                    at += 1;
                } else if tokens.get(at).is_none_or(|token| token.bytes != b")") {
                    return Ok(None);
                }
            }
            if tokens.get(at).is_none_or(|token| token.bytes != b")") {
                return Ok(None);
            }
            at += 1;
            calls
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            calls.push(name);
        } else if returning
            && tokens.get(at).is_some_and(|token| {
                token
                    .bytes
                    .first()
                    .is_some_and(|byte| *byte == b'$' || byte.is_ascii_digit())
            })
        {
            at += 1;
        } else {
            return Ok(None);
        }
        if tokens.get(at).is_none_or(|token| token.bytes != b";") {
            return Ok(None);
        }
        at += 1;
    }
    Ok(Some(calls))
}

fn layout<'a>(
    tokens: &[Token<'a>],
    file: FileId,
    cancel: &Cancellation,
) -> Result<Option<Vec<Class<'a>>>, AdapterError> {
    let mut classes = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        cancel.check()?;
        let start = at;
        if tokens[at].bytes == b"final" {
            at += 1;
        }
        if tokens.get(at).is_none_or(|token| token.bytes != b"class")
            || tokens.get(at + 1).is_none_or(|token| !word(*token))
            || tokens.get(at + 2).is_none_or(|token| token.bytes != b"{")
        {
            return Ok(None);
        }
        let name = tokens[at + 1];
        let opening = at + 2;
        let Some(end) = closed(tokens, opening, cancel)? else {
            return Ok(None);
        };
        at = opening + 1;
        let mut methods = Vec::new();
        while at < end {
            cancel.check()?;
            let method_start = at;
            let mut modifiers = BTreeSet::new();
            while matches!(
                tokens[at].bytes,
                b"public" | b"private" | b"protected" | b"final"
            ) {
                if !modifiers.insert(tokens[at].bytes) {
                    return Ok(None);
                }
                at += 1;
                if at >= end {
                    return Ok(None);
                }
            }
            if tokens[at].bytes != b"function"
                || tokens.get(at + 1).is_none_or(|token| !word(*token))
                || tokens.get(at + 2).is_none_or(|token| token.bytes != b"(")
            {
                return Ok(None);
            }
            let method_name = tokens[at + 1];
            at += 3;
            while at < end && tokens[at].bytes != b")" {
                cancel.check()?;
                if tokens[at].bytes.first() != Some(&b'$') || tokens[at].bytes == b"$this" {
                    return Ok(None);
                }
                at += 1;
                if tokens.get(at).is_some_and(|token| token.bytes == b",") {
                    at += 1;
                } else if tokens.get(at).is_none_or(|token| token.bytes != b")") {
                    return Ok(None);
                }
            }
            if at + 1 >= end || tokens[at].bytes != b")" || tokens[at + 1].bytes != b"{" {
                return Ok(None);
            }
            let body = at + 1;
            let Some(method_end) = closed(tokens, body, cancel)? else {
                return Ok(None);
            };
            if method_end >= end {
                return Ok(None);
            }
            let calls = flat_calls(&tokens[body + 1..method_end], cancel)?.unwrap_or_default();
            methods.push(Method {
                name: method_name,
                span: span(file, tokens[method_start].start, tokens[method_end].end)?,
                body: span(file, tokens[body].start, tokens[method_end].end)?,
                calls,
            });
            at = method_end + 1;
        }
        classes.push(Class {
            name,
            span: span(file, tokens[start].start, tokens[end].end)?,
            body: span(file, tokens[opening].start, tokens[end].end)?,
            methods,
        });
        at = end + 1;
    }
    Ok(Some(classes))
}

struct Raw<'a> {
    ids: BTreeMap<u64, &'a SyntaxFact>,
    spans: BTreeMap<SourceSpan, Vec<&'a SyntaxFact>>,
}

impl<'a> Raw<'a> {
    fn fact(
        &self,
        span: SourceSpan,
        kind: SyntaxFactKind,
        label: &str,
        count: usize,
    ) -> Option<&'a SyntaxFact> {
        let rows = self.spans.get(&span)?;
        if rows.len() != count {
            return None;
        }
        let mut matches = rows
            .iter()
            .filter(|fact| fact.kind() == kind && fact.syntax_kind().as_str() == label);
        let found = *matches.next()?;
        matches.next().is_none().then_some(found)
    }
}

fn entity(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    raw: &Raw<'_>,
    source: &SourceRef,
    name: Token<'_>,
    owner_span: SourceSpan,
    class: Option<SymbolId>,
) -> Result<Option<(u64, SymbolId)>, AdapterError> {
    let kind = if class.is_none() {
        EntityKind::Class
    } else if name.bytes.eq_ignore_ascii_case(b"__construct") {
        EntityKind::Constructor
    } else {
        EntityKind::Method
    };
    let label = match kind {
        EntityKind::Class => "php.class.declaration",
        EntityKind::Constructor => "php.constructor.declaration",
        _ => "php.method.declaration",
    };
    let Some(declaration) = raw.fact(owner_span, SyntaxFactKind::Declaration, label, 2) else {
        return Ok(None);
    };
    let context_label = if class.is_none() {
        "php.this_class_context.signature"
    } else {
        "php.this_method_context.signature"
    };
    let Some(context) = raw.fact(owner_span, SyntaxFactKind::Signature, context_label, 2) else {
        return Ok(None);
    };
    let Some(definition) = raw.fact(
        span(owner_span.file(), name.start, name.end)?,
        SyntaxFactKind::Occurrence,
        "php.name.definition",
        1,
    ) else {
        return Ok(None);
    };
    if context.parent() != Some(declaration.local_id())
        || definition.parent() != Some(declaration.local_id())
    {
        return Ok(None);
    }
    let Some(symbol) = builder
        .symbol_by_declaration
        .get(&(owner_span.file(), declaration.local_id()))
        .copied()
    else {
        return Ok(None);
    };
    let mut count = 0;
    for record in &builder.records {
        builder.cancellation.check()?;
        if let IrRecord::Entity(record) = record
            && record.id == symbol
        {
            if record.kind != kind
                || record.language != "php"
                || !exact(
                    record.canonical_name.as_bytes(),
                    name.bytes,
                    builder.cancellation,
                )?
                || record.repository != source.repository()
                || record.generation != source.generation()
                || record.provenance != builder.provenance_for_file(owner_span.file())?
                || record.container
                    != Some(
                        class.map_or(ContainerRef::File(owner_span.file()), ContainerRef::Entity),
                    )
                || record.evidence.source
                    != Some(SourceRef::new(
                        source.repository(),
                        source.generation(),
                        owner_span,
                        source.content_hash(),
                        None,
                    ))
            {
                return Ok(None);
            }
            count += 1;
        }
    }
    Ok((count == 1).then_some((declaration.local_id(), symbol)))
}

pub(super) fn resolve(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<BTreeMap<SourceSpan, Proof>, AdapterError> {
    let cancel = builder.cancellation;
    let mut result = BTreeMap::new();
    for parsed in &builder.parsed {
        cancel.check()?;
        if !matches!(
            parsed.parse_status,
            CoverageStatus::Complete | CoverageStatus::Bounded
        ) || parsed.input.encoding().as_str() != "utf-8"
        {
            continue;
        }
        let source = parsed.input.source().source_ref();
        let bytes = parsed.input.source().bytes();
        let Some(tokens) = lex(bytes, cancel)? else {
            continue;
        };
        let Some(classes) = layout(&tokens, source.span().file(), cancel)? else {
            continue;
        };
        let mut raw = Raw {
            ids: BTreeMap::new(),
            spans: BTreeMap::new(),
        };
        let mut invalid = false;
        let mut roots = Vec::new();
        for fact in &parsed.facts {
            cancel.check()?;
            invalid |= raw.ids.insert(fact.local_id(), fact).is_some()
                || !contains_span(source.span(), fact.span())
                || fact.kind() == SyntaxFactKind::ErrorRecovery;
            raw.spans.entry(fact.span()).or_default().push(fact);
            if fact.kind() == SyntaxFactKind::Root {
                roots.push(fact);
            }
        }
        let [root] = roots.as_slice() else { continue };
        if invalid
            || root.syntax_kind().as_str() != "php.program.root"
            || root.parent().is_some()
            || root.depth() != 0
            || root.span().end_byte() != source.span().end_byte()
            || !matches!(
                bytes.get(..usize::try_from(root.span().start_byte()).unwrap_or(usize::MAX)),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            continue;
        }
        let mut names = BTreeMap::<ContentHash, usize>::new();
        for class in &classes {
            *names.entry(key(class.name.bytes, cancel)?).or_default() += 1;
        }
        for class in &classes {
            cancel.check()?;
            if names[&key(class.name.bytes, cancel)?] != 1 {
                continue;
            }
            let Some((class_local, class_symbol)) =
                entity(builder, &raw, source, class.name, class.span, None)?
            else {
                continue;
            };
            if raw.ids[&class_local].parent() != Some(root.local_id()) {
                continue;
            }
            let Some(class_body) = raw
                .fact(
                    class.body,
                    SyntaxFactKind::Scope,
                    "php.declaration_list.scope",
                    1,
                )
                .filter(|scope| scope.parent() == Some(class_local))
            else {
                continue;
            };
            let mut methods = BTreeMap::new();
            for method in &class.methods {
                cancel.check()?;
                let name = key(method.name.bytes, cancel)?;
                let entry = methods.entry(name).or_insert_with(Vec::new);
                entry.push(method);
            }
            for owner in &class.methods {
                cancel.check()?;
                let Some((owner_local, owner_symbol)) = entity(
                    builder,
                    &raw,
                    source,
                    owner.name,
                    owner.span,
                    Some(class_symbol),
                )?
                else {
                    continue;
                };
                if raw.ids[&owner_local].parent() != Some(class_body.local_id()) {
                    continue;
                }
                let Some(body) = raw
                    .fact(owner.body, SyntaxFactKind::Scope, "php.block.scope", 1)
                    .filter(|scope| scope.parent() == Some(owner_local))
                else {
                    continue;
                };
                for call in &owner.calls {
                    cancel.check()?;
                    let call_span = span(source.span().file(), call.start, call.end)?;
                    let Some(fact) = raw
                        .fact(
                            call_span,
                            SyntaxFactKind::Occurrence,
                            "php.this_method.reference",
                            1,
                        )
                        .filter(|fact| fact.parent() == Some(body.local_id()))
                    else {
                        continue;
                    };
                    let Some(targets) = methods.get(&key(call.bytes, cancel)?) else {
                        continue;
                    };
                    let [target] = targets.as_slice() else {
                        continue;
                    };
                    if !same(call.bytes, target.name.bytes, cancel)?
                        || target.name.bytes.eq_ignore_ascii_case(b"__construct")
                    {
                        continue;
                    }
                    let Some((target_local, target_symbol)) = entity(
                        builder,
                        &raw,
                        source,
                        target.name,
                        target.span,
                        Some(class_symbol),
                    )?
                    else {
                        continue;
                    };
                    if raw.ids[&target_local].parent() != Some(class_body.local_id()) {
                        continue;
                    }
                    let mut site = None;
                    let mut duplicate = false;
                    for candidate in &builder.occurrences {
                        cancel.check()?;
                        if candidate.source.span() == call_span {
                            duplicate |= site.replace(candidate).is_some();
                        }
                    }
                    let Some(site) = site.filter(|_| !duplicate) else {
                        continue;
                    };
                    if site.role != OccurrenceRole::Reference
                        || !self::site(&site.syntax_kind)
                        || !exact(site.name.as_bytes(), call.bytes, cancel)?
                        || site.enclosing_declaration != Some(owner_local)
                        || site.source != source_for_span(parsed.input, fact.span())
                    {
                        continue;
                    }
                    let derivation = BTreeSet::from([
                        FactRef::File(source.span().file()),
                        FactRef::Fact(builder.provenance_for_file(source.span().file())?),
                        FactRef::Entity(class_symbol),
                        FactRef::Entity(owner_symbol),
                        FactRef::Entity(target_symbol),
                    ]);
                    if derivation.len() > builder.request.limits().ir().max_nested_items_per_record
                    {
                        continue;
                    }
                    result.insert(
                        call_span,
                        Proof {
                            target: target_symbol,
                            owner_local,
                            derivation: derivation.into_iter().collect(),
                        },
                    );
                }
            }
        }
    }
    Ok(result)
}
