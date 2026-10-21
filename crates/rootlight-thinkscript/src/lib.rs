//! Finite written ThinkScript script/input syntax with physical source positions.
//! Both production and consumption re-census source through this internal model;
//! expressions establish ordinary value positions, never study evaluation or Calls.

#![forbid(unsafe_code)]

use rootlight_cancel::{Cancellation, Cancelled};
use std::{collections::HashMap, fmt};

/// Failure of the bounded source model, before any partial plan is admitted.
#[derive(Debug)]
pub enum Error {
    /// Cooperative cancellation won.
    Cancelled(Cancelled),
    /// A caller-owned token, fact or nesting ceiling was reached.
    Limit,
    /// Fallible host allocation failed.
    Allocation,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<Cancelled> for Error {
    fn from(value: Cancelled) -> Self {
        Self::Cancelled(value)
    }
}

/// Closed syntax roles emitted by this finite source model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Complete physical source root.
    Root,
    /// Full-source lexical environment witness.
    Environment,
    /// Written top-level script declaration.
    Script,
    /// Exact script declaration name.
    ScriptName,
    /// Exact script header and opening brace.
    Header,
    /// Direct written script input declaration.
    Input,
    /// Exact input declaration name.
    InputName,
    /// Proven ordinary use of a unique own input.
    Read,
    /// Paired current-source value-position witness.
    ReadContext,
}

impl Role {
    /// Returns this closed role's source label for SDK emission and consumption.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Root => "thinkscript.source_file",
            Self::Environment => "thinkscript.environment.signature",
            Self::Script => "thinkscript.script.declaration",
            Self::ScriptName => "thinkscript.script.definition",
            Self::Header => "thinkscript.script.signature",
            Self::Input => "thinkscript.input.declaration",
            Self::InputName => "thinkscript.input.definition",
            Self::Read => "thinkscript.input_value.read",
            Self::ReadContext => "thinkscript.input_value.signature",
        }
    }
}

/// One source-positioned node; IDs identify only this complete plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Local syntax identity.
    pub id: u64,
    /// Exact direct owner identity.
    pub parent: Option<u64>,
    /// Inclusive physical start byte.
    pub start: usize,
    /// Exclusive physical end byte.
    pub end: usize,
    /// Direct source ownership depth.
    pub depth: usize,
    /// Closed source role.
    pub role: Role,
}

/// A proven value use of one unique own-script input declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// Read occurrence local identity.
    pub read: u64,
    /// Script declaration identity.
    pub owner: u64,
    /// Input declaration identity.
    pub target: u64,
}

/// Complete finite syntax plan. Unsupported constructs stay outside this subset.
#[derive(Debug)]
pub struct Plan {
    /// Exact required records, in parent-before-child order.
    pub records: Vec<Record>,
    /// Independently reconstructible own-input value dependencies.
    pub bindings: Vec<Binding>,
    /// Whether the entire physical source had closed lexical delimiters.
    pub lexical_complete: bool,
    /// Actual lexical token nodes observed before emitting the required role plan.
    pub syntax_nodes: usize,
    /// Maximum observed delimiter or required-plan ownership depth.
    pub max_depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Name,
    Number,
    String,
    Punct(u8),
}
#[derive(Clone, Copy, Debug)]
struct Token {
    start: usize,
    end: usize,
    kind: Kind,
    mate: Option<usize>,
}

fn push<T>(items: &mut Vec<T>, value: T, ceiling: usize) -> Result<(), Error> {
    if items.len() >= ceiling {
        return Err(Error::Limit);
    }
    items.try_reserve(1).map_err(|_| Error::Allocation)?;
    items.push(value);
    Ok(())
}

fn checkpoint(at: usize, cancel: &Cancellation) -> Result<(), Error> {
    if at.is_multiple_of(64) {
        cancel.check()?;
    }
    Ok(())
}

fn lex(
    source: &[u8],
    limit: usize,
    cancel: &Cancellation,
) -> Result<(Vec<Token>, bool, usize), Error> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut maximum_depth = 0;
    let mut at = usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3;
    while at < source.len() {
        checkpoint(at, cancel)?;
        let start = at;
        let byte = source[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte == b'#' {
            while at < source.len() && !matches!(source[at], b'\r' | b'\n') {
                checkpoint(at, cancel)?;
                if source[at] == 0 {
                    return Ok((tokens, false, maximum_depth));
                }
                at += 1;
            }
            continue;
        }
        let kind = if byte == b'"' {
            at += 1;
            let mut closed = false;
            while at < source.len() {
                cancel.check()?;
                match source[at] {
                    0 | b'\r' | b'\n' => return Ok((tokens, false, maximum_depth)),
                    b'\\' => {
                        at += 1;
                        if at >= source.len() || matches!(source[at], 0 | b'\r' | b'\n') {
                            return Ok((tokens, false, maximum_depth));
                        }
                        at += 1;
                    }
                    b'"' => {
                        at += 1;
                        closed = true;
                        break;
                    }
                    _ => at += 1,
                }
            }
            if !closed {
                return Ok((tokens, false, maximum_depth));
            }
            Kind::String
        } else if byte.is_ascii_alphabetic() || byte == b'_' {
            at += 1;
            while at < source.len() && (source[at].is_ascii_alphanumeric() || source[at] == b'_') {
                checkpoint(at, cancel)?;
                at += 1;
            }
            Kind::Name
        } else if byte.is_ascii_digit() {
            at += 1;
            while at < source.len() && source[at].is_ascii_digit() {
                checkpoint(at, cancel)?;
                at += 1;
            }
            if source.get(at) == Some(&b'.') && source.get(at + 1).is_some_and(u8::is_ascii_digit) {
                at += 1;
                while at < source.len() && source[at].is_ascii_digit() {
                    checkpoint(at, cancel)?;
                    at += 1;
                }
            }
            Kind::Number
        } else if byte.is_ascii() && byte != 0 {
            at += 1;
            Kind::Punct(byte)
        } else {
            return Ok((tokens, false, maximum_depth));
        };
        let index = tokens.len();
        push(
            &mut tokens,
            Token {
                start,
                end: at,
                kind,
                mate: None,
            },
            limit,
        )?;
        match kind {
            Kind::Punct(b'(' | b'[' | b'{') => {
                push(&mut stack, index, 128)?;
                maximum_depth = maximum_depth.max(stack.len());
            }
            Kind::Punct(close @ (b')' | b']' | b'}')) => {
                let Some(open) = stack.pop() else {
                    return Ok((tokens, false, maximum_depth));
                };
                let expected = match close {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if tokens[open].kind != Kind::Punct(expected) {
                    return Ok((tokens, false, maximum_depth));
                }
                tokens[open].mate = Some(index);
                tokens[index].mate = Some(open);
            }
            _ => {}
        }
    }
    Ok((tokens, stack.is_empty(), maximum_depth))
}

fn valid_utf8(source: &[u8], cancel: &Cancellation) -> Result<bool, Error> {
    let mut at = 0;
    while at < source.len() {
        cancel.check()?;
        let width = match source[at] {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Ok(false),
        };
        let Some(end) = at.checked_add(width).filter(|end| *end <= source.len()) else {
            return Ok(false);
        };
        if std::str::from_utf8(&source[at..end]).is_err() {
            return Ok(false);
        }
        at = end;
    }
    Ok(true)
}

fn word(source: &[u8], token: Token, expected: &[u8]) -> bool {
    token.kind == Kind::Name && &source[token.start..token.end] == expected
}

fn ordinary_name(source: &[u8], token: Token) -> bool {
    token.kind == Kind::Name
        && ![
            "above",
            "ago",
            "and",
            "bar",
            "bars",
            "below",
            "between",
            "case",
            "crosses",
            "declare",
            "def",
            "default",
            "do",
            "else",
            "equal",
            "equals",
            "false",
            "fold",
            "from",
            "greater",
            "if",
            "input",
            "is",
            "less",
            "no",
            "not",
            "or",
            "plot",
            "profile",
            "rec",
            "reference",
            "script",
            "switch",
            "than",
            "then",
            "to",
            "true",
            "while",
            "with",
            "within",
            "yes",
        ]
        .iter()
        .any(|word| source[token.start..token.end].eq_ignore_ascii_case(word.as_bytes()))
}

fn name_key(source: &[u8], token: Token, cancel: &Cancellation) -> Result<[u8; 32], Error> {
    let mut hash = blake3::Hasher::new();
    for chunk in source[token.start..token.end].chunks(64) {
        cancel.check()?;
        let mut folded = [0_u8; 64];
        for (to, from) in folded.iter_mut().zip(chunk) {
            *to = from.to_ascii_lowercase();
        }
        hash.update(&folded[..chunk.len()]);
    }
    Ok(*hash.finalize().as_bytes())
}

fn exact_name(
    source: &[u8],
    left: Token,
    right: Token,
    cancel: &Cancellation,
) -> Result<bool, Error> {
    let left = &source[left.start..left.end];
    let right = &source[right.start..right.end];
    if left.len() != right.len() {
        return Ok(false);
    }
    for (a, b) in left.chunks(64).zip(right.chunks(64)) {
        cancel.check()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Expression<'a> {
    tokens: &'a [Token],
    source: &'a [u8],
    at: usize,
    reads: Vec<Token>,
    limit: usize,
    cancel: &'a Cancellation,
}

impl Expression<'_> {
    fn expression(&mut self, minimum: u8, depth: usize) -> Result<bool, Error> {
        self.cancel.check()?;
        if depth >= 128 {
            return Err(Error::Limit);
        }
        if !self.primary(depth + 1)? {
            return Ok(false);
        }
        loop {
            self.cancel.check()?;
            let precedence = match self.tokens.get(self.at).map(|t| t.kind) {
                Some(Kind::Punct(b'+' | b'-')) => 1,
                Some(Kind::Punct(b'*' | b'/' | b'%')) => 2,
                _ => break,
            };
            if precedence < minimum {
                break;
            }
            self.at += 1;
            if !self.expression(precedence + 1, depth + 1)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn primary(&mut self, depth: usize) -> Result<bool, Error> {
        self.cancel.check()?;
        let Some(token) = self.tokens.get(self.at).copied() else {
            return Ok(false);
        };
        if matches!(token.kind, Kind::Punct(b'+' | b'-')) {
            self.at += 1;
            return self.expression(3, depth + 1);
        }
        self.at += 1;
        match token.kind {
            Kind::Number | Kind::String => {}
            Kind::Name => {
                if [
                    b"if".as_slice(),
                    b"then",
                    b"else",
                    b"fold",
                    b"switch",
                    b"script",
                    b"input",
                    b"def",
                    b"plot",
                    b"rec",
                ]
                .iter()
                .any(|name| word(self.source, token, name))
                {
                    return Ok(false);
                }
                if self
                    .tokens
                    .get(self.at)
                    .is_some_and(|next| next.kind == Kind::Punct(b'('))
                {
                    self.at += 1;
                    if !self
                        .tokens
                        .get(self.at)
                        .is_some_and(|next| next.kind == Kind::Punct(b')'))
                    {
                        loop {
                            if !self.expression(0, depth + 1)? {
                                return Ok(false);
                            }
                            if self
                                .tokens
                                .get(self.at)
                                .is_some_and(|next| next.kind == Kind::Punct(b','))
                            {
                                self.at += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    if !self
                        .tokens
                        .get(self.at)
                        .is_some_and(|next| next.kind == Kind::Punct(b')'))
                    {
                        return Ok(false);
                    }
                    self.at += 1;
                } else {
                    push(&mut self.reads, token, self.limit)?;
                }
            }
            Kind::Punct(b'(') => {
                if !self.expression(0, depth + 1)?
                    || !self
                        .tokens
                        .get(self.at)
                        .is_some_and(|next| next.kind == Kind::Punct(b')'))
                {
                    return Ok(false);
                }
                self.at += 1;
            }
            _ => return Ok(false),
        }
        while self
            .tokens
            .get(self.at)
            .is_some_and(|next| next.kind == Kind::Punct(b'.'))
        {
            self.cancel.check()?;
            if !self
                .tokens
                .get(self.at + 1)
                .is_some_and(|next| next.kind == Kind::Name)
            {
                return Ok(false);
            }
            self.at += 2;
        }
        Ok(true)
    }
}

#[derive(Clone, Copy)]
struct Input {
    start: usize,
    end: usize,
    name: Token,
}
struct Script {
    start: usize,
    end: usize,
    name: Token,
    header_end: usize,
    inputs: Vec<Input>,
    reads: Vec<(Token, usize)>,
    closed: bool,
}

fn script(
    source: &[u8],
    tokens: &[Token],
    start: usize,
    end: usize,
    limit: usize,
    cancel: &Cancellation,
) -> Result<Script, Error> {
    let mut out = Script {
        start: tokens[start].start,
        end: tokens[end].end,
        name: tokens[start + 1],
        header_end: tokens[start + 2].end,
        inputs: Vec::new(),
        reads: Vec::new(),
        closed: true,
    };
    let mut names: HashMap<[u8; 32], Option<usize>> = HashMap::new();
    names
        .try_reserve(end - start)
        .map_err(|_| Error::Allocation)?;
    let mut uses = Vec::new();
    let mut at = start + 3;
    while at < end {
        cancel.check()?;
        let begin = at;
        let input = word(source, tokens[at], b"input");
        let ordinary =
            input || word(source, tokens[at], b"def") || word(source, tokens[at], b"plot");
        if !ordinary
            || at + 3 >= end
            || !ordinary_name(source, tokens[at + 1])
            || tokens[at + 2].kind != Kind::Punct(b'=')
        {
            out.closed = false;
            break;
        }
        let name = tokens[at + 1];
        at += 3;
        let expression_start = at;
        while at < end && tokens[at].kind != Kind::Punct(b';') {
            cancel.check()?;
            if tokens[at].kind == Kind::Punct(b'{') {
                out.closed = false;
                break;
            }
            at += 1;
        }
        if !out.closed || at == end {
            out.closed = false;
            break;
        }
        let mut expression = Expression {
            tokens: &tokens[expression_start..at],
            source,
            at: 0,
            reads: Vec::new(),
            limit,
            cancel,
        };
        if !expression.expression(0, 0)? || expression.at != expression.tokens.len() {
            out.closed = false;
            break;
        }
        let key = name_key(source, name, cancel)?;
        let candidate = if input { Some(out.inputs.len()) } else { None };
        names
            .entry(key)
            .and_modify(|slot| *slot = None)
            .or_insert(candidate);
        if input {
            push(
                &mut out.inputs,
                Input {
                    start: tokens[begin].start,
                    end: tokens[at].end,
                    name,
                },
                limit,
            )?;
        } else {
            for token in expression.reads {
                push(&mut uses, (token, tokens[begin].start), limit)?;
            }
        }
        at += 1;
    }
    if out.closed {
        for (token, statement_start) in uses {
            cancel.check()?;
            if let Some(index) = names
                .get(&name_key(source, token, cancel)?)
                .copied()
                .flatten()
                && out.inputs[index].end <= statement_start
                && exact_name(source, token, out.inputs[index].name, cancel)?
            {
                push(&mut out.reads, (token, index), limit)?;
            }
        }
    }
    Ok(out)
}

/// Reconstructs a finite plan from complete source under caller-owned ceilings.
///
/// # Errors
/// Returns cancellation, allocation or a token/record/depth ceiling refusal.
pub fn scan(
    source: &[u8],
    max_tokens: usize,
    max_records: usize,
    cancel: &Cancellation,
) -> Result<Plan, Error> {
    cancel.check()?;
    let (tokens, lexical_complete, maximum_depth) = if valid_utf8(source, cancel)? {
        lex(source, max_tokens, cancel)?
    } else {
        (Vec::new(), false, 0)
    };
    let mut plan = Plan {
        records: Vec::new(),
        bindings: Vec::new(),
        lexical_complete,
        syntax_nodes: tokens.len(),
        max_depth: maximum_depth,
    };
    let add = |plan: &mut Plan, role, parent, start, end, depth| -> Result<u64, Error> {
        let id = u64::try_from(plan.records.len())
            .map_err(|_| Error::Limit)?
            .checked_add(1)
            .ok_or(Error::Limit)?;
        push(
            &mut plan.records,
            Record {
                id,
                parent,
                start,
                end,
                depth,
                role,
            },
            max_records,
        )?;
        plan.max_depth = plan.max_depth.max(depth);
        Ok(id)
    };
    add(&mut plan, Role::Root, None, 0, source.len(), 0)?;
    if !lexical_complete {
        return Ok(plan);
    }
    add(&mut plan, Role::Environment, Some(1), 0, source.len(), 1)?;
    let mut at = 0;
    let mut statement_start = true;
    while at < tokens.len() {
        cancel.check()?;
        if statement_start
            && word(source, tokens[at], b"script")
            && tokens
                .get(at + 1)
                .is_some_and(|token| ordinary_name(source, *token))
            && tokens
                .get(at + 2)
                .is_some_and(|token| token.kind == Kind::Punct(b'{'))
        {
            let Some(end) = tokens[at + 2].mate else {
                return Ok(plan);
            };
            let script = script(source, &tokens, at, end, max_records, cancel)?;
            let owner = add(
                &mut plan,
                Role::Script,
                Some(1),
                script.start,
                script.end,
                1,
            )?;
            add(
                &mut plan,
                Role::ScriptName,
                Some(owner),
                script.name.start,
                script.name.end,
                2,
            )?;
            add(
                &mut plan,
                Role::Header,
                Some(owner),
                script.start,
                script.header_end,
                2,
            )?;
            let mut targets = Vec::new();
            targets
                .try_reserve_exact(script.inputs.len())
                .map_err(|_| Error::Allocation)?;
            for input in script.inputs {
                cancel.check()?;
                let target = add(
                    &mut plan,
                    Role::Input,
                    Some(owner),
                    input.start,
                    input.end,
                    2,
                )?;
                add(
                    &mut plan,
                    Role::InputName,
                    Some(target),
                    input.name.start,
                    input.name.end,
                    3,
                )?;
                targets.push(target);
            }
            for (token, target) in script.reads {
                cancel.check()?;
                let read = add(
                    &mut plan,
                    Role::Read,
                    Some(owner),
                    token.start,
                    token.end,
                    2,
                )?;
                add(
                    &mut plan,
                    Role::ReadContext,
                    Some(owner),
                    token.start,
                    token.end,
                    2,
                )?;
                push(
                    &mut plan.bindings,
                    Binding {
                        read,
                        owner,
                        target: targets[target],
                    },
                    max_records,
                )?;
            }
            at = end + 1;
            statement_start = true;
        } else {
            let token = tokens[at];
            if matches!(token.kind, Kind::Punct(b'{' | b'(' | b'[')) {
                at = token.mate.ok_or(Error::Limit)? + 1;
                statement_start = false;
            } else {
                statement_start = token.kind == Kind::Punct(b';');
                at += 1;
            }
        }
    }
    Ok(plan)
}

/// Returns only a distinctive complete written script/input routing hint.
///
/// This is finite source recognition, not a general `.txt` file association.
#[must_use]
pub fn has_content_hint(source: &[u8]) -> bool {
    let ceiling = source.len().saturating_add(1);
    scan(source, ceiling, ceiling, &Cancellation::new()).is_ok_and(|plan| {
        plan.lexical_complete && plan.records.iter().any(|record| record.role == Role::Input)
    })
}

/// Authenticates the complete shared source model in provider cache identities.
#[must_use]
pub fn source_digest() -> [u8; 32] {
    *blake3::hash(include_bytes!("lib.rs")).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    const ORIGINAL: &[u8] =
        include_bytes!("../../rootlight-adapters/tests/fixtures/thinkscript/VIX_FGMR.txt");

    #[test]
    fn complete_original_positions_and_required_value_contexts_keep_physical_offsets() {
        assert_eq!(ORIGINAL.len(), 2565);
        let plan = scan(ORIGINAL, 8192, 8192, &Cancellation::new()).unwrap();
        assert!(plan.lexical_complete);
        for (role, start, end) in [
            (Role::ScriptName, 316, 339),
            (Role::InputName, 380, 385),
            (Role::InputName, 402, 407),
            (Role::Read, 578, 583),
            (Role::ReadContext, 578, 583),
        ] {
            assert_eq!(
                plan.records
                    .iter()
                    .filter(|record| record.role == role
                        && record.start == start
                        && record.end == end)
                    .count(),
                1
            );
        }
        let read = plan
            .records
            .iter()
            .find(|record| record.role == Role::Read && record.start == 578)
            .unwrap();
        let binding = plan
            .bindings
            .iter()
            .find(|binding| binding.read == read.id)
            .unwrap();
        let definition = &plan.records[usize::try_from(binding.target).unwrap()];
        assert_eq!(definition.role, Role::InputName);
        assert_eq!((definition.start, definition.end), (380, 385));
        assert_eq!(read.parent, Some(binding.owner));
        assert_eq!(
            ORIGINAL
                .windows(2)
                .filter(|bytes| *bytes == b"\r\n")
                .count(),
            91
        );
        let mut doubled_cr = Vec::new();
        for &byte in ORIGINAL {
            if byte == b'\n' {
                doubled_cr.push(b'\r');
            }
            doubled_cr.push(byte);
        }
        let shift = ORIGINAL[..578]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        let expanded = scan(&doubled_cr, 8192, 8192, &Cancellation::new()).unwrap();
        assert!(expanded.lexical_complete);
        assert!(
            expanded
                .records
                .iter()
                .any(|record| record.role == Role::Read
                    && record.start == 578 + shift
                    && record.end == 583 + shift)
        );
    }

    #[test]
    fn renamed_scripts_keep_scopes_comments_strings_and_selectors_distinct() {
        let source = b"# script Fake { input ghost=1; }\r\nscript Owned { input value=2; input note=\"# script Fake { input ghost=1; }\"; def local=External(value).value; plot result=local+value; }";
        let plan = scan(source, 4096, 4096, &Cancellation::new()).unwrap();
        let definitions: Vec<_> = plan
            .records
            .iter()
            .filter(|record| matches!(record.role, Role::ScriptName | Role::InputName))
            .map(|record| &source[record.start..record.end])
            .collect();
        assert_eq!(definitions, [b"Owned".as_slice(), b"value", b"note"]);
        let reads: Vec<_> = plan
            .records
            .iter()
            .filter(|record| record.role == Role::Read)
            .collect();
        assert_eq!(reads.len(), 2);
        assert!(
            reads
                .iter()
                .all(|record| &source[record.start..record.end] == b"value")
        );
        assert!(has_content_hint(source));
        assert!(!has_content_hint(
            b"documentation: script Owned { input value=2; plot p=value; }"
        ));
        assert!(!has_content_hint(
            b"\"script Owned { input value=2; plot p=value; }\""
        ));
    }

    #[test]
    fn duplicate_shadow_case_collision_write_and_unknown_blocks_never_bind_inputs() {
        for source in [
            b"script Owned { input value=2; input value=3; plot p=value; }".as_slice(),
            b"script Owned { input value=2; input VALUE=3; plot p=value; }",
            b"script Owned { input value=2; def value=3; plot p=value; }",
            b"script Owned { input value=2; value=3; plot p=value; }",
            b"script Owned { input value=2; switch(value) { default: plot p=value; } }",
            b"script Owned { input value=2; plot p=value value; }",
            b"script Owned { input value=2; plot p=External(value=3); }",
            b"script Owned { input value=2; plot p=if value then 1 else 0; }",
        ] {
            let plan = scan(source, 4096, 4096, &Cancellation::new()).unwrap();
            assert!(plan.lexical_complete);
            assert!(
                plan.bindings.is_empty(),
                "no guessed input binding for {source:?}"
            );
        }
        let sibling =
            b"script Left { input value=2; plot p=value; } script Right { plot p=value; }";
        let plan = scan(sibling, 4096, 4096, &Cancellation::new()).unwrap();
        assert_eq!(plan.bindings.len(), 1);
        let call = b"script Owned { input value=2; plot p=value(3); }";
        assert!(
            scan(call, 4096, 4096, &Cancellation::new())
                .unwrap()
                .bindings
                .is_empty()
        );
    }

    #[test]
    fn incomplete_invalid_and_encoded_inputs_cannot_issue_a_partial_plan() {
        for source in [
            b"script Owned { input value=2; plot p=value;".as_slice(),
            b"script Owned { input value=2; plot p=\"unterminated; }",
            b"script Owned { input value=2; plot p=\"\xff\"; }",
            b"script Owned { input value=2;\0 plot p=value; }",
            b"\xff\xfes\0c\0r\0i\0p\0t\0",
        ] {
            let plan = scan(source, 4096, 4096, &Cancellation::new()).unwrap();
            assert!(!plan.lexical_complete);
            assert_eq!(plan.records.len(), 1);
            assert!(plan.bindings.is_empty());
            assert!(!has_content_hint(source));
        }
        let mut bom = b"\xef\xbb\xbf".to_vec();
        bom.extend_from_slice(ORIGINAL);
        let plan = scan(&bom, 8192, 8192, &Cancellation::new()).unwrap();
        assert!(
            plan.records.iter().any(|record| record.role == Role::Read
                && record.start == 581
                && record.end == 586)
        );
    }

    #[test]
    fn source_token_record_and_depth_limits_and_cancellation_are_strict() {
        let plan = scan(ORIGINAL, 8192, 8192, &Cancellation::new()).unwrap();
        let exact = scan(ORIGINAL, 8192, plan.records.len(), &Cancellation::new()).unwrap();
        assert_eq!(plan.records, exact.records);
        assert!(matches!(
            scan(ORIGINAL, 8192, plan.records.len() - 1, &Cancellation::new()),
            Err(Error::Limit)
        ));
        assert!(matches!(
            scan(ORIGINAL, 1, 8192, &Cancellation::new()),
            Err(Error::Limit)
        ));
        let nested = format!(
            "script Owned {{ input x=2; plot p={}x{}; }}",
            "(".repeat(129),
            ")".repeat(129)
        );
        assert!(matches!(
            scan(nested.as_bytes(), 4096, 4096, &Cancellation::new()),
            Err(Error::Limit)
        ));
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            scan(ORIGINAL, 8192, 8192, &cancel),
            Err(Error::Cancelled(_))
        ));
    }
}
