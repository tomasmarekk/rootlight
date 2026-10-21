//! Recheck written unsigned-local overload selection against current source.
//! Native call/owner certificates cannot select a target without exact current
//! headers, local type, raw cardinality and source-owned entity witnesses.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
    source_for_span, span_contains,
};

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "chapel.typed_local_call.reference"
}

#[derive(Clone, Copy)]
struct Token<'a> {
    bytes: &'a [u8],
    start: usize,
    end: usize,
}

fn same(a: &[u8], b: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if a.len() != b.len() {
        return Ok(false);
    }
    for (a, b) in a.chunks(64).zip(b.chunks(64)) {
        cancel.check()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

fn text_hash(bytes: &[u8], cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut digest = blake3::Hasher::new();
    for chunk in bytes.chunks(64) {
        cancel.check()?;
        digest.update(chunk);
    }
    Ok(ContentHash::from_bytes(*digest.finalize().as_bytes()))
}

fn lex<'a>(
    source: &'a [u8],
    cancel: &Cancellation,
) -> Result<Option<Vec<Token<'a>>>, AdapterError> {
    let mut result = Vec::new();
    let mut at = usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3;
    while at < source.len() {
        cancel.check()?;
        let start = at;
        if source[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if source[at..].starts_with(b"//") {
            while at < source.len() && source[at] != b'\n' {
                cancel.check()?;
                at += 1
            }
            continue;
        }
        if source[at..].starts_with(b"/*") {
            at += 2;
            let mut depth = 1_usize;
            while depth != 0 {
                cancel.check()?;
                if source.get(at..).is_some_and(|s| s.starts_with(b"/*")) {
                    depth += 1;
                    at += 2;
                } else if source.get(at..).is_some_and(|s| s.starts_with(b"*/")) {
                    depth -= 1;
                    at += 2;
                } else if at < source.len() {
                    at += 1;
                } else {
                    return Ok(None);
                }
                if depth > 128 {
                    return Ok(None);
                }
            }
            continue;
        }
        if matches!(source[at], b'\'' | b'"') {
            let quote = source[at];
            at += 1;
            while at < source.len() && source[at] != quote {
                cancel.check()?;
                if matches!(source[at], b'\n' | b'\r' | b'\\') {
                    return Ok(None);
                }
                at += 1;
            }
            if at == source.len() {
                return Ok(None);
            }
            at += 1;
        } else if source[at].is_ascii_alphanumeric() || source[at] == b'_' {
            at += 1;
            while at < source.len()
                && (source[at].is_ascii_alphanumeric() || matches!(source[at], b'_' | b'$'))
            {
                cancel.check()?;
                at += 1;
            }
        } else if source[at].is_ascii_punctuation() {
            at += 1;
        } else {
            return Ok(None);
        }
        result
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        result.push(Token {
            bytes: &source[start..at],
            start,
            end: at,
        });
    }
    Ok(Some(result))
}

fn close(
    tokens: &[Token<'_>],
    start: usize,
    cancel: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let expected = match tokens[start].bytes {
        b"(" => b")".as_slice(),
        b"{" => b"}".as_slice(),
        _ => return Ok(None),
    };
    let mut stack = Vec::new();
    stack
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    stack.push(expected);
    for (index, token) in tokens.iter().enumerate().skip(start + 1) {
        cancel.check()?;
        match token.bytes {
            b"(" | b"{" => {
                stack
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                stack.push(if token.bytes == b"(" { b")" } else { b"}" });
            }
            b")" | b"}" => {
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

fn identifier(token: Token<'_>) -> bool {
    // The cancellable lexer has already validated the remainder of this token.
    token
        .bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
}

fn width(tokens: &[Token<'_>]) -> Option<u8> {
    let [name, open, value, end] = tokens else {
        return None;
    };
    if name.bytes != b"uint" || open.bytes != b"(" || end.bytes != b")" {
        return None;
    }
    match value.bytes {
        b"8" => Some(8),
        b"16" => Some(16),
        b"32" => Some(32),
        b"64" => Some(64),
        _ => None,
    }
}

struct Procedure {
    start: usize,
    name: usize,
    open: usize,
    args_end: usize,
    body: usize,
    end: usize,
    module: usize,
}
struct Module {
    start: usize,
    name: usize,
    body: usize,
    end: usize,
}

struct Current<'a> {
    tokens: Vec<Token<'a>>,
    procedures: Vec<Procedure>,
    modules: Vec<Module>,
}

impl<'a> Current<'a> {
    fn new(source: &'a [u8], cancel: &Cancellation) -> Result<Option<Self>, AdapterError> {
        let Some(tokens) = lex(source, cancel)? else {
            return Ok(None);
        };
        // Imports, type aliases, generic/intent declarations and dynamic bindings
        // are deliberately outside this finite closed unsigned-scalar proof.
        for token in &tokens {
            cancel.check()?;
            if matches!(
                token.bytes,
                b"use"
                    | b"import"
                    | b"type"
                    | b"ref"
                    | b"extern"
                    | b"where"
                    | b"config"
                    | b"param"
                    | b"class"
                    | b"record"
                    | b"iter"
                    | b"operator"
            ) {
                return Ok(None);
            }
        }
        let mut modules = Vec::<Module>::new();
        for (at, token) in tokens.iter().enumerate() {
            cancel.check()?;
            if token.bytes != b"module" {
                continue;
            }
            if !tokens.get(at + 1).copied().is_some_and(identifier)
                || tokens.get(at + 2).is_none_or(|t| t.bytes != b"{")
            {
                return Ok(None);
            }
            let Some(end) = close(&tokens, at + 2, cancel)? else {
                return Ok(None);
            };
            let start = if at > 0 && matches!(tokens[at - 1].bytes, b"public" | b"private") {
                at - 1
            } else {
                at
            };
            modules
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            modules.push(Module {
                start,
                name: at + 1,
                body: at + 2,
                end,
            });
        }
        let mut procedures = Vec::<Procedure>::new();
        for (at, token) in tokens.iter().enumerate() {
            cancel.check()?;
            if token.bytes != b"proc" {
                continue;
            }
            if !tokens.get(at + 1).copied().is_some_and(identifier)
                || tokens.get(at + 2).is_none_or(|t| t.bytes != b"(")
            {
                return Ok(None);
            }
            let Some(args_end) = close(&tokens, at + 2, cancel)? else {
                return Ok(None);
            };
            let mut body = args_end + 1;
            if tokens.get(body).is_some_and(|t| t.bytes == b":") {
                body += 1;
                while tokens.get(body).is_some_and(|t| t.bytes != b"{") {
                    cancel.check()?;
                    body += 1;
                }
            }
            if tokens.get(body).is_none_or(|t| t.bytes != b"{") {
                return Ok(None);
            }
            let Some(end) = close(&tokens, body, cancel)? else {
                return Ok(None);
            };
            let mut module = None;
            for (index, selected) in modules.iter().enumerate() {
                cancel.check()?;
                if selected.body < at
                    && end < selected.end
                    && module.is_none_or(|old: usize| modules[old].body < selected.body)
                {
                    module = Some(index);
                }
            }
            let Some(module) = module else {
                return Ok(None);
            };
            let mut start = at;
            if start > 0 && tokens[start - 1].bytes == b"inline" {
                start -= 1
            }
            if start > 0 && matches!(tokens[start - 1].bytes, b"public" | b"private") {
                start -= 1
            }
            procedures
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            procedures.push(Procedure {
                start,
                name: at + 1,
                open: at + 2,
                args_end,
                body,
                end,
                module,
            });
        }
        for a in &procedures {
            for b in &procedures {
                cancel.check()?;
                if a.start < b.start && b.end < a.end {
                    return Ok(None);
                }
            }
        }
        Ok(Some(Self {
            tokens,
            procedures,
            modules,
        }))
    }

    fn unsigned_formal(&self, procedure: &Procedure) -> Option<Option<u8>> {
        let args = &self.tokens[procedure.open + 1..procedure.args_end];
        if args.len() < 3 || !identifier(args[0]) || args[1].bytes != b":" {
            return None;
        }
        if let Some(width) = width(&args[2..]) {
            return Some(Some(width));
        }
        matches!(args.get(2..), Some([value]) if matches!(value.bytes, b"string" | b"c_string"))
            .then_some(None)
    }

    fn local_width(
        &self,
        owner: &Procedure,
        site: usize,
        name: &[u8],
        callee: &[u8],
        cancel: &Cancellation,
    ) -> Result<Option<u8>, AdapterError> {
        let mut depth = 0_usize;
        let mut result = None;
        let mut declarations = 0_usize;
        for at in owner.open + 1..owner.end {
            cancel.check()?;
            let token = self.tokens[at];
            if at < owner.body && same(token.bytes, callee, cancel)? {
                return Ok(None);
            }
            if token.bytes == b"{" {
                depth += 1
            }
            if token.bytes == b"}" {
                depth = depth.saturating_sub(1)
            }
            if same(token.bytes, callee, cancel)?
                && at > 0
                && matches!(self.tokens[at - 1].bytes, b"var" | b"const" | b"for" | b",")
            {
                return Ok(None);
            }
            if !same(token.bytes, name, cancel)? {
                continue;
            }
            if at < owner.body {
                return Ok(None);
            }
            if at > 0 && matches!(self.tokens[at - 1].bytes, b"for" | b",") {
                return Ok(None);
            }
            if at == 0 || !matches!(self.tokens[at - 1].bytes, b"var" | b"const") {
                continue;
            }
            declarations += 1;
            if depth != 1 || at >= site || self.tokens.get(at + 1).is_none_or(|t| t.bytes != b":") {
                continue;
            }
            let Some(width) = self.tokens.get(at + 2..at + 6).and_then(width) else {
                continue;
            };
            if self.tokens.get(at + 6).is_none_or(|t| t.bytes != b"=")
                || self.tokens.get(at + 8).is_none_or(|t| t.bytes != b";")
            {
                continue;
            }
            let value = self.tokens[at + 7].bytes;
            if value.is_empty() || value.len() > 20 || !value.iter().all(u8::is_ascii_digit) {
                continue;
            }
            let Some(value) = value.iter().try_fold(0_u64, |value, digit| {
                value.checked_mul(10)?.checked_add(u64::from(*digit - b'0'))
            }) else {
                continue;
            };
            if width < 64 && value >= 1_u64 << width {
                continue;
            }
            result = Some(width);
        }
        Ok((declarations == 1).then_some(result).flatten())
    }
}

struct Binding {
    owner: SymbolId,
    target: SymbolId,
    source: SourceRef,
}
pub(super) struct Targets {
    bindings: HashMap<u64, Binding>,
    source: SourceRef,
}

impl Targets {
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        projected_length: usize,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut bindings = HashMap::new();
        let empty = || Self {
            bindings: HashMap::new(),
            source: source.clone(),
        };
        let file = source.span().file();
        let full = SourceSpan::new(
            file,
            0,
            u64::try_from(projected_length).map_err(|_| SinkError::InvalidIr)?,
        )
        .map_err(|_| SinkError::InvalidIr)?;
        if physical(full)? != source.span() {
            return Ok(empty());
        }
        let full_text = text(full)?;
        let Some(current) = Current::new(full_text.as_bytes(), cancel)? else {
            return Ok(empty());
        };
        let mut ids = HashMap::<u64, Option<&SyntaxFact>>::new();
        let mut spans = HashMap::<SourceSpan, Vec<&SyntaxFact>>::new();
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        spans
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut roots = 0;
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != file
                || fact.span().end_byte() > full.end_byte()
                || fact.kind() == SyntaxFactKind::ErrorRecovery
                || fact.syntax_kind().as_str() == "chapel.unsupported_context.signature"
            {
                return Ok(empty());
            }
            ids.entry(fact.local_id())
                .and_modify(|value| *value = None)
                .or_insert(Some(fact));
            let rows = spans.entry(fact.span()).or_default();
            rows.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            rows.push(fact);
            if fact.kind() == SyntaxFactKind::Root {
                roots += 1;
                let prefix = SourceSpan::new(file, 0, fact.span().start_byte())
                    .map_err(|_| SinkError::InvalidIr)?;
                if fact.parent().is_some()
                    || fact.syntax_kind().as_str() != "chapel.source_file.root"
                    || fact.span().end_byte() != full.end_byte()
                    || !matches!(text(prefix)?, "" | "\u{feff}")
                {
                    return Ok(empty());
                }
            }
        }
        for value in ids.values() {
            cancel.check()?;
            if value.is_none() {
                return Ok(empty());
            }
        }
        if roots != 1 {
            return Ok(empty());
        }
        let one = |span: SourceSpan,
                   kind: SyntaxFactKind,
                   label: &str,
                   parent: Option<u64>|
         -> Option<&SyntaxFact> {
            let rows = spans.get(&span)?;
            if rows.len() > 4 {
                return None;
            }
            let mut matches = rows.iter().filter(|f| f.kind() == kind);
            let fact = *matches.next()?;
            if matches.next().is_some()
                || fact.syntax_kind().as_str() != label
                || fact.parent() != parent
            {
                return None;
            }
            Some(fact)
        };
        let raw_span = |start: usize, end: usize| {
            SourceSpan::new(
                file,
                u64::try_from(start).map_err(|_| SinkError::InvalidIr)?,
                u64::try_from(end).map_err(|_| SinkError::InvalidIr)?,
            )
            .map_err(|_| AdapterError::Sink(SinkError::InvalidIr))
        };
        let mut admitted = HashMap::<usize, (u64, &MaterializedEntity)>::new();
        admitted
            .try_reserve(current.procedures.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, procedure) in current.procedures.iter().enumerate() {
            cancel.check()?;
            let start = current.tokens[procedure.start].start;
            let end = current.tokens[procedure.end].end;
            let span = raw_span(start, end)?;
            let rows = spans.get(&span).filter(|rows| rows.len() <= 4);
            let Some(declaration) = rows.and_then(|rows| {
                let mut rows = rows
                    .iter()
                    .filter(|f| f.kind() == SyntaxFactKind::Declaration);
                let first = rows.next()?;
                rows.next().is_none().then_some(*first)
            }) else {
                continue;
            };
            if declaration.syntax_kind().as_str() != "chapel.function.declaration" {
                continue;
            }
            let name = current.tokens[procedure.name];
            let Some(definition) = one(
                raw_span(name.start, name.end)?,
                SyntaxFactKind::Occurrence,
                "chapel.name.definition",
                Some(declaration.local_id()),
            ) else {
                continue;
            };
            let Some(entity) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(evidence) = entity.record.evidence.source.as_ref() else {
                continue;
            };
            if entity.record.language != "chapel"
                || entity.record.kind != EntityKind::Function
                || entity.definition_local_id != Some(definition.local_id())
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || *evidence != source_for_span(source, physical(span)?)
                || !same(entity.record.canonical_name.as_bytes(), name.bytes, cancel)?
            {
                continue;
            }
            let header = raw_span(start, current.tokens[procedure.body].start)?;
            if one(
                header,
                SyntaxFactKind::Signature,
                "chapel.header.signature",
                Some(declaration.local_id()),
            )
            .is_none()
            {
                continue;
            }
            let module = &current.modules[procedure.module];
            let module_span = raw_span(
                current.tokens[module.start].start,
                current.tokens[module.end].end,
            )?;
            let Some(module_rows) = spans.get(&module_span).filter(|rows| rows.len() <= 5) else {
                continue;
            };
            // A written module may occupy the whole file. Its own Scope and
            // Declaration then coincide with the three source-frame roles.
            let coincident = module_span == full;
            let mut roles = [0_u8; 5];
            let mut invalid_frame = false;
            for row in module_rows {
                cancel.check()?;
                let slot = match (row.kind(), row.syntax_kind().as_str()) {
                    (SyntaxFactKind::Declaration, "chapel.module.declaration") => 0,
                    (SyntaxFactKind::Scope, "chapel.module.scope") => 1,
                    (SyntaxFactKind::Root, "chapel.source_file.root") if coincident => 2,
                    (SyntaxFactKind::Module, "chapel.file.module") if coincident => 3,
                    (SyntaxFactKind::Scope, "chapel.file.scope") if coincident => 4,
                    _ => {
                        invalid_frame = true;
                        break;
                    }
                };
                roles[slot] += 1;
            }
            let expected = if coincident {
                [1, 1, 1, 1, 1]
            } else {
                [1, 1, 0, 0, 0]
            };
            if invalid_frame || roles != expected {
                continue;
            }
            let mut declarations = module_rows
                .iter()
                .filter(|f| f.kind() == SyntaxFactKind::Declaration);
            let Some(module_decl) = declarations.next() else {
                continue;
            };
            if declarations.next().is_some()
                || module_decl.syntax_kind().as_str() != "chapel.module.declaration"
            {
                continue;
            }
            let module_name = current.tokens[module.name];
            if one(
                raw_span(module_name.start, module_name.end)?,
                SyntaxFactKind::Occurrence,
                "chapel.name.definition",
                Some(module_decl.local_id()),
            )
            .is_none()
            {
                continue;
            }
            let Some(module_entity) = entities.get(&module_decl.local_id()) else {
                continue;
            };
            if module_entity.record.language != "chapel"
                || module_entity.record.kind != EntityKind::Module
                || module_entity.record.repository != source.repository()
                || module_entity.record.generation != source.generation()
                || !same(
                    module_entity.record.canonical_name.as_bytes(),
                    module_name.bytes,
                    cancel,
                )?
                || module_entity.record.provenance != entity.record.provenance
                || module_entity.record.evidence.source.as_ref()
                    != Some(&source_for_span(source, physical(module_span)?))
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
            {
                continue;
            }
            let mut parent = declaration.parent();
            let mut steps = 0;
            while parent != Some(module_decl.local_id()) {
                cancel.check()?;
                steps += 1;
                if steps > facts.len() {
                    break;
                }
                let Some(scope) = parent.and_then(|id| ids.get(&id).copied().flatten()) else {
                    break;
                };
                if scope.kind() != SyntaxFactKind::Scope
                    || !span_contains(module_span, scope.span())
                {
                    break;
                }
                parent = scope.parent();
            }
            if parent == Some(module_decl.local_id()) {
                admitted.insert(index, (declaration.local_id(), entity));
            }
        }
        for fact in facts {
            cancel.check()?;
            if !required(fact) {
                continue;
            }
            let mut site = None;
            for (index, token) in current.tokens.iter().enumerate() {
                cancel.check()?;
                if u64::try_from(token.start).ok() == Some(fact.span().start_byte()) {
                    site = Some(index);
                    break;
                }
            }
            let Some(site) = site else { continue };
            let tokens = &current.tokens;
            let Some(call) = tokens.get(site..site + 4) else {
                continue;
            };
            let [callee, open, argument, end] = call else {
                continue;
            };
            if callee.end != usize::try_from(fact.span().end_byte()).unwrap_or(usize::MAX)
                || !identifier(*callee)
                || !identifier(*argument)
                || open.bytes != b"("
                || end.bytes != b")"
            {
                continue;
            }
            let Some(owner_index) = current
                .procedures
                .iter()
                .position(|p| p.body < site && site < p.end)
            else {
                continue;
            };
            let owner = &current.procedures[owner_index];
            let Some((owner_local, owner_entity)) = admitted.get(&owner_index).copied() else {
                continue;
            };
            let owner_span = raw_span(tokens[owner.start].start, tokens[owner.end].end)?;
            if one(
                owner_span,
                SyntaxFactKind::Signature,
                "chapel.typed_local_call_owner_context.signature",
                Some(owner_local),
            )
            .is_none()
                || fact.parent() != Some(owner_local)
            {
                continue;
            }
            let call_span = raw_span(callee.start, end.end)?;
            if one(
                call_span,
                SyntaxFactKind::Signature,
                "chapel.typed_local_call_context.signature",
                Some(owner_local),
            )
            .is_none()
                || one(
                    fact.span(),
                    SyntaxFactKind::Occurrence,
                    "chapel.typed_local_call.reference",
                    Some(owner_local),
                ) != Some(fact)
            {
                continue;
            }
            if site == 0
                || !matches!(tokens[site - 1].bytes, b"return" | b";" | b"}")
                || tokens.get(site + 4).is_none_or(|t| t.bytes != b";")
            {
                continue;
            }
            let mut depth = 0_usize;
            for token in &tokens[owner.body..site] {
                cancel.check()?;
                if token.bytes == b"{" {
                    depth += 1
                }
                if token.bytes == b"}" {
                    depth = depth.saturating_sub(1)
                }
            }
            if depth != 1 {
                continue;
            }
            let Some(width) =
                current.local_width(owner, site, argument.bytes, callee.bytes, cancel)?
            else {
                continue;
            };
            let mut target = None;
            let mut invalid = false;
            for (index, procedure) in current.procedures.iter().enumerate() {
                cancel.check()?;
                if procedure.module != owner.module
                    || !same(tokens[procedure.name].bytes, callee.bytes, cancel)?
                {
                    continue;
                }
                let Some(formal) = current.unsigned_formal(procedure) else {
                    invalid = true;
                    break;
                };
                if formal == Some(width) && target.replace(index).is_some() {
                    invalid = true;
                    break;
                }
            }
            // A module-level variable can shadow a procedure regardless of a
            // withheld native Definition; source census must retain that veto.
            for (at, token) in tokens.iter().enumerate() {
                cancel.check()?;
                if at > 0
                    && same(token.bytes, callee.bytes, cancel)?
                    && matches!(tokens[at - 1].bytes, b"var" | b"const" | b"for" | b",")
                {
                    invalid = true;
                    break;
                }
            }
            if invalid {
                continue;
            }
            let Some((_, target_entity)) = target.and_then(|index| admitted.get(&index)).copied()
            else {
                continue;
            };
            if target_entity.record.provenance != owner_entity.record.provenance {
                continue;
            }
            bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            bindings.insert(
                fact.local_id(),
                Binding {
                    owner: owner_entity.record.id,
                    target: target_entity.record.id,
                    source: source_for_span(source, physical(fact.span())?),
                },
            );
        }
        Ok(Self {
            bindings,
            source: source.clone(),
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        let Some(owner) = owner else { return Ok(None) };
        if !required(fact)
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || occurrence.role != OccurrenceRole::CallSite
            || occurrence.enclosing != Some(binding.owner)
            || owner.record.id != binding.owner
            || occurrence.provenance != owner.record.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source != binding.source
            || occurrence.syntactic_text_hash != text_hash(text.as_bytes(), cancel)?
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}

pub(super) fn relation(
    occurrence: &OccurrenceRecord,
    owner: &MaterializedEntity,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<RelationRecord, AdapterError> {
    cancel.check()?;
    let mut relation = lexical_reference_relation(occurrence, target)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Calls;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("chapel-written-call-identity"))?;
    Ok(relation)
}
