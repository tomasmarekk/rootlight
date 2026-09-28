//! Source-owned boundaries for statement calls through application results.
//! Syntax evidence is independent of native statement partitioning. It does not
//! establish target identity, argument compatibility, or return types.

use std::ops::Range;

use super::{group::quoted_end, name, trivia};

/// Syntax evidence within a complete, source-written call statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallChainRole {
    /// A lexical value read, including receiver and property-get contexts.
    ValueIdentifier,
    /// A lexical name used as an application target.
    InvokedValueIdentifier,
    /// A qualified or implicit member read, including its complete receiver.
    Member,
    /// A member used as an application target.
    InvokedMember,
    /// An application including its arguments; call versus index is unresolved.
    Application,
    /// A named argument label, not a lexical value read.
    ArgumentName,
}

/// One exact source span, relative to the supplied statement prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChainPart {
    span: Range<usize>,
    role: CallChainRole,
}

impl CallChainPart {
    /// Returns the original half-open byte range without rewriting source.
    #[must_use]
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }

    /// Returns syntax classification, not a resolved symbol or call edge.
    #[must_use]
    pub const fn role(&self) -> CallChainRole {
        self.role
    }
}

/// A statement containing a member access on an application result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChain {
    end: usize,
    parts: Vec<CallChainPart>,
}

impl CallChain {
    /// Exclusive end before a comment, statement separator, or bare newline.
    #[must_use]
    pub const fn end(&self) -> usize {
        self.end
    }

    /// Returns complete reference/application evidence for the admitted statement.
    #[must_use]
    pub fn parts(&self) -> &[CallChainPart] {
        &self.parts
    }
}

/// Distinguishes unsupported syntax from exhaustion of the caller's token budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallChainRead {
    /// Syntax outside this reader's admitted expression subset, or not a chain.
    Unsupported,
    /// The statement requires more tokens than the caller permits.
    TokenLimit,
    /// Complete statement and argument syntax, without semantic binding.
    Parsed(CallChain),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Name,
    Literal,
    Dot,
    Open(usize),
    Close,
    Comma,
    Named,
    Binary,
    Sign,
    Not,
    Call,
}

#[derive(Debug, Clone)]
struct Token {
    span: Range<usize>,
    kind: Kind,
    continued: bool,
}

#[derive(Debug, Clone, Copy)]
enum JobKind {
    Arguments,
    Expression,
}

#[derive(Debug)]
struct Job {
    range: Range<usize>,
    kind: JobKind,
}

struct Reader<'a> {
    tokens: &'a [Token],
    parts: Vec<CallChainPart>,
    jobs: Vec<Job>,
}

/// Reads one chain call from its first token, independently of parser nodes.
///
/// The main callee must contain an application followed by a dot member.
/// Arguments admit names, dot/application chains, grouped expressions, decimal
/// integers, strings, dates, Boolean/null literals and ordinary unary/binary
/// operators. Named and omitted arguments retain their authored boundaries.
/// Assignment, bang dispatch, type suffixes, instance/type/pointer keywords,
/// malformed syntax and other expressions return `Unsupported` atomically.
/// No partial reference list is returned. Quoted names preserve their brackets.
///
/// Tokenization and expression traversal are nonrecursive and linear in the
/// statement length. Allocation is bounded by `maximum_tokens` and input length;
/// at most two syntax parts are emitted per token. Callers must charge emitted
/// parts to their normal fact budget and independently resolve every target.
/// The callback runs throughout scanning and traversal for cancellation/work.
///
/// # Errors
/// Propagates the callback's error unchanged.
pub fn call_chain_statement<E>(
    source: &[u8],
    maximum_tokens: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<CallChainRead, E> {
    check()?;
    let tokens = match tokenize(source, maximum_tokens, check)? {
        Ok(tokens) => tokens,
        Err(result) => return Ok(result),
    };
    let Some(last) = tokens.last() else {
        return Ok(CallChainRead::Unsupported);
    };
    if tokens.first().is_none_or(|token| token.span.start != 0) {
        return Ok(CallChainRead::Unsupported);
    }
    let end = last.span.end;
    let mut reader = Reader {
        tokens: &tokens,
        parts: Vec::new(),
        jobs: Vec::new(),
    };
    let explicit = tokens.first().is_some_and(|t| t.kind == Kind::Call);
    let start = usize::from(explicit);
    if !tokens
        .get(start)
        .is_some_and(|t| matches!(t.kind, Kind::Name | Kind::Dot))
    {
        return Ok(CallChainRead::Unsupported);
    }
    let Some((cursor, final_part, chained)) = reader.atom(start, tokens.len(), check)? else {
        return Ok(CallChainRead::Unsupported);
    };
    if !chained {
        return Ok(CallChainRead::Unsupported);
    }
    if cursor < tokens.len() {
        // Bare statement arguments need separation from the complete callee.
        // Operators at this boundary are assignments/ambiguous continuations,
        // not evidence of an additional invoked member.
        let previous_end = tokens.get(cursor - 1).map_or(0, |t| t.span.end);
        if explicit
            || tokens.get(cursor).is_none_or(|t| {
                t.span.start == previous_end || matches!(t.kind, Kind::Binary | Kind::Sign)
            })
        {
            return Ok(CallChainRead::Unsupported);
        }
        reader.jobs.push(Job {
            range: cursor..tokens.len(),
            kind: JobKind::Arguments,
        });
    }
    reader.invoke(final_part);
    // An explicit Call prefix belongs to the whole application, but never to
    // the receiver/member spans needed for source-identity joins.
    if let Some(application) = reader.parts.iter_mut().find(|part| {
        part.role == CallChainRole::Application
            && part.span.start == tokens.get(start).map_or(0, |token| token.span.start)
            && part.span.end == end
    }) {
        application.span.start = 0;
    } else {
        reader.parts.push(CallChainPart {
            span: 0..end,
            role: CallChainRole::Application,
        });
    }
    while let Some(job) = reader.jobs.pop() {
        check()?;
        let valid = match job.kind {
            JobKind::Arguments => reader.arguments(job.range, check)?,
            JobKind::Expression => reader.expression(job.range, check)?,
        };
        if !valid {
            return Ok(CallChainRead::Unsupported);
        }
    }
    Ok(CallChainRead::Parsed(CallChain {
        end,
        parts: reader.parts,
    }))
}

impl Reader<'_> {
    fn invoke(&mut self, part: Option<usize>) {
        if let Some(part) = part.and_then(|index| self.parts.get_mut(index)) {
            part.role = match part.role {
                CallChainRole::ValueIdentifier => CallChainRole::InvokedValueIdentifier,
                CallChainRole::Member => CallChainRole::InvokedMember,
                unchanged @ (CallChainRole::InvokedValueIdentifier
                | CallChainRole::InvokedMember
                | CallChainRole::Application
                | CallChainRole::ArgumentName) => unchanged,
            };
        }
    }

    fn atom<E>(
        &mut self,
        start: usize,
        end: usize,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<(usize, Option<usize>, bool)>, E> {
        let Some(first) = self.tokens.get(start).filter(|_| start < end) else {
            return Ok(None);
        };
        let source_start = first.span.start;
        let mut cursor = start + 1;
        let mut part = None;
        let mut applied = false;
        let mut chained = false;
        match first.kind {
            Kind::Name => {
                part = Some(self.parts.len());
                self.parts.push(CallChainPart {
                    span: first.span.clone(),
                    // Both arguments and member receivers evaluate a value.
                    // This context also permits authored enum/module roots and
                    // property getters; a plain lexical name loses that proof.
                    role: CallChainRole::ValueIdentifier,
                });
            }
            Kind::Dot => {
                let Some(member) = self
                    .tokens
                    .get(cursor)
                    .filter(|t| cursor < end && t.kind == Kind::Name)
                else {
                    return Ok(None);
                };
                part = Some(self.parts.len());
                self.parts.push(CallChainPart {
                    span: source_start..member.span.end,
                    role: CallChainRole::Member,
                });
                cursor += 1;
            }
            Kind::Open(close) if close < end => {
                self.jobs.push(Job {
                    range: cursor..close,
                    kind: JobKind::Expression,
                });
                cursor = close + 1;
            }
            Kind::Literal => return Ok(Some((cursor, None, false))),
            Kind::Open(_)
            | Kind::Close
            | Kind::Comma
            | Kind::Named
            | Kind::Binary
            | Kind::Sign
            | Kind::Not
            | Kind::Call => return Ok(None),
        }
        while cursor < end {
            check()?;
            let Some(token) = self.tokens.get(cursor) else {
                return Ok(None);
            };
            match token.kind {
                Kind::Dot => {
                    let previous_end = self.tokens.get(cursor - 1).map_or(0, |t| t.span.end);
                    if token.span.start != previous_end && !token.continued {
                        return Ok(None);
                    }
                    let Some(member) = self
                        .tokens
                        .get(cursor + 1)
                        .filter(|t| cursor + 1 < end && t.kind == Kind::Name)
                    else {
                        return Ok(None);
                    };
                    chained |= applied;
                    part = Some(self.parts.len());
                    self.parts.push(CallChainPart {
                        span: source_start..member.span.end,
                        role: CallChainRole::Member,
                    });
                    cursor += 2;
                }
                Kind::Open(close) if close < end => {
                    self.invoke(part);
                    self.jobs.push(Job {
                        range: cursor + 1..close,
                        kind: JobKind::Arguments,
                    });
                    let Some(last) = self.tokens.get(close) else {
                        return Ok(None);
                    };
                    self.parts.push(CallChainPart {
                        span: source_start..last.span.end,
                        role: CallChainRole::Application,
                    });
                    part = None;
                    cursor = close + 1;
                    applied = true;
                }
                Kind::Name
                | Kind::Literal
                | Kind::Open(_)
                | Kind::Close
                | Kind::Comma
                | Kind::Named
                | Kind::Binary
                | Kind::Sign
                | Kind::Not
                | Kind::Call => break,
            }
        }
        Ok(Some((cursor, part, chained)))
    }

    fn expression<E>(
        &mut self,
        range: Range<usize>,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut cursor = range.start;
        loop {
            check()?;
            while cursor < range.end
                && self
                    .tokens
                    .get(cursor)
                    .is_some_and(|t| matches!(t.kind, Kind::Sign | Kind::Not))
            {
                check()?;
                cursor += 1;
            }
            let Some((next, _, _)) = self.atom(cursor, range.end, check)? else {
                return Ok(false);
            };
            cursor = next;
            if cursor == range.end {
                return Ok(true);
            }
            if !self
                .tokens
                .get(cursor)
                .is_some_and(|t| matches!(t.kind, Kind::Binary | Kind::Sign))
            {
                return Ok(false);
            }
            cursor += 1;
        }
    }

    fn arguments<E>(
        &mut self,
        range: Range<usize>,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        if range.is_empty() {
            return Ok(true);
        }
        let mut start = range.start;
        let mut cursor = start;
        let mut named = false;
        loop {
            check()?;
            if cursor == range.end
                || self
                    .tokens
                    .get(cursor)
                    .is_some_and(|t| t.kind == Kind::Comma)
            {
                let argument = start..cursor;
                if argument.is_empty() {
                    if named || cursor == range.end {
                        return Ok(false);
                    }
                } else {
                    let is_named = self.tokens.get(start).is_some_and(|t| t.kind == Kind::Name)
                        && start + 1 < cursor
                        && self
                            .tokens
                            .get(start + 1)
                            .is_some_and(|t| t.kind == Kind::Named);
                    if is_named {
                        named = true;
                        if let Some(token) = self.tokens.get(start) {
                            self.parts.push(CallChainPart {
                                span: token.span.clone(),
                                role: CallChainRole::ArgumentName,
                            });
                        }
                    } else if named {
                        return Ok(false);
                    }
                    self.jobs.push(Job {
                        range: start + if is_named { 2 } else { 0 }..cursor,
                        kind: JobKind::Expression,
                    });
                }
                if cursor == range.end {
                    return Ok(true);
                }
                cursor += 1;
                start = cursor;
            } else if let Some(Token {
                kind: Kind::Open(close),
                ..
            }) = self.tokens.get(cursor)
            {
                cursor = close + 1;
            } else {
                cursor += 1;
            }
        }
    }
}

fn tokenize<E>(
    source: &[u8],
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Result<Vec<Token>, CallChainRead>, E> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut opens = Vec::new();
    let mut cursor = 0;
    loop {
        check()?;
        let continued = trivia(source, &mut cursor, check)?;
        let start = cursor;
        let Some(&byte) = source.get(cursor) else {
            break;
        };
        if matches!(byte, b'\r' | b'\n' | b'\'')
            || (byte == b':' && source.get(cursor + 1) != Some(&b'='))
        {
            break;
        }
        if tokens.len() >= maximum {
            return Ok(Err(CallChainRead::TokenLimit));
        }
        let kind = match byte {
            b'[' | b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let Some(word) = name(source, &mut cursor, check)? else {
                    return Ok(Err(CallChainRead::Unsupported));
                };
                if byte == b'[' {
                    Kind::Name
                } else if word.eq_ignore_ascii_case(b"Call") {
                    Kind::Call
                } else if word.eq_ignore_ascii_case(b"Not") {
                    Kind::Not
                } else if [
                    b"And".as_slice(),
                    b"Or",
                    b"Xor",
                    b"Eqv",
                    b"Imp",
                    b"Mod",
                    b"Is",
                    b"Like",
                ]
                .iter()
                .any(|op| word.eq_ignore_ascii_case(op))
                {
                    Kind::Binary
                } else if [b"True".as_slice(), b"False", b"Nothing", b"Null", b"Empty"]
                    .iter()
                    .any(|literal| word.eq_ignore_ascii_case(literal))
                {
                    Kind::Literal
                } else if [b"New".as_slice(), b"Me", b"AddressOf", b"TypeOf", b"ByVal"]
                    .iter()
                    .any(|keyword| word.eq_ignore_ascii_case(keyword))
                {
                    return Ok(Err(CallChainRead::Unsupported));
                } else {
                    Kind::Name
                }
            }
            b'"' => {
                let Some(end) = quoted_end(source, cursor, byte, check)? else {
                    return Ok(Err(CallChainRead::Unsupported));
                };
                cursor = end;
                Kind::Literal
            }
            b'#' => {
                cursor += 1;
                while source
                    .get(cursor)
                    .is_some_and(|b| !matches!(b, b'#' | b'\r' | b'\n'))
                {
                    if cursor.is_multiple_of(512) {
                        check()?
                    }
                    cursor += 1;
                }
                if source.get(cursor) != Some(&b'#') {
                    return Ok(Err(CallChainRead::Unsupported));
                }
                cursor += 1;
                Kind::Literal
            }
            b'0'..=b'9' => {
                while source.get(cursor).is_some_and(u8::is_ascii_digit) {
                    if cursor.is_multiple_of(512) {
                        check()?
                    }
                    cursor += 1;
                }
                Kind::Literal
            }
            b'(' => {
                opens.push(tokens.len());
                cursor += 1;
                Kind::Open(0)
            }
            b')' => {
                let Some(open) = opens.pop() else {
                    return Ok(Err(CallChainRead::Unsupported));
                };
                let close = tokens.len();
                if let Some(token) = tokens.get_mut(open) {
                    token.kind = Kind::Open(close)
                }
                cursor += 1;
                Kind::Close
            }
            b':' if source.get(cursor + 1) == Some(&b'=') => {
                cursor += 2;
                Kind::Named
            }
            b'.' => {
                cursor += 1;
                Kind::Dot
            }
            b',' => {
                cursor += 1;
                Kind::Comma
            }
            b'+' | b'-' => {
                cursor += 1;
                Kind::Sign
            }
            b'=' | b'<' | b'>' | b'*' | b'/' | b'\\' | b'^' | b'&' => {
                cursor += 1;
                if (byte == b'<' && source.get(cursor).is_some_and(|b| matches!(b, b'=' | b'>')))
                    || (byte == b'>' && source.get(cursor) == Some(&b'='))
                {
                    cursor += 1
                }
                Kind::Binary
            }
            _ => return Ok(Err(CallChainRead::Unsupported)),
        };
        tokens.push(Token {
            span: start..cursor,
            kind,
            continued,
        });
    }
    Ok(if opens.is_empty() {
        Ok(tokens)
    } else {
        Err(CallChainRead::Unsupported)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    fn read(source: &[u8]) -> CallChainRead {
        call_chain_statement(source, 4096, &mut || Ok::<(), Infallible>(())).unwrap()
    }

    #[test]
    fn chains_keep_nested_arguments_and_exact_source() {
        let source = b".Create(Codec.Trim(text)).Store limit:=Low(1) + High(2), note:=\"a,\"\"b\" ' ignored\r\nOther";
        let CallChainRead::Parsed(chain) = read(source) else {
            panic!("complete chain")
        };
        let parts = chain
            .parts()
            .iter()
            .map(|p| (&source[p.span()], p.role()))
            .collect::<Vec<_>>();
        for expected in [
            (b".Create".as_slice(), CallChainRole::InvokedMember),
            (
                b".Create(Codec.Trim(text))".as_slice(),
                CallChainRole::Application,
            ),
            (
                b".Create(Codec.Trim(text)).Store".as_slice(),
                CallChainRole::InvokedMember,
            ),
            (b"Codec.Trim".as_slice(), CallChainRole::InvokedMember),
            (b"Codec.Trim(text)".as_slice(), CallChainRole::Application),
            (b"text".as_slice(), CallChainRole::ValueIdentifier),
            (b"Low(1)".as_slice(), CallChainRole::Application),
            (b"High(2)".as_slice(), CallChainRole::Application),
            (b"limit".as_slice(), CallChainRole::ArgumentName),
        ] {
            assert!(parts.contains(&expected), "{expected:?}: {parts:?}")
        }
        assert!(!parts.iter().any(|(text, _)| *text == b".Store"));
        assert_eq!(
            &source[..chain.end()],
            b".Create(Codec.Trim(text)).Store limit:=Low(1) + High(2), note:=\"a,\"\"b\""
        );
    }

    #[test]
    fn continuations_groups_and_statement_boundaries_are_preserved() {
        for source in [
            b"Call maker.Create().Store((Left(1) + Right(2)))".as_slice(),
            b"maker.Create() _\r\n .Store value:=#1/2/2000#, skip:=False:Other",
            b"[maker].[Create]().[Store] , Count(2)",
        ] {
            assert!(
                matches!(read(source), CallChainRead::Parsed(_)),
                "{source:?}"
            )
        }
        for source in [
            b"maker.Create() .Store value".as_slice(),
            b"maker.Create().Store = value",
            b"maker.Create().Store New Product",
            b"maker.Create().Store AddressOf Handler",
            b"maker.Create().Store Me.Value",
            b"maker.Create().Store amount:=1, 2",
            b"maker.Create().Store 1,",
            b"maker.Create().Store (unclosed",
            b"maker.Create().Store x +",
            b"maker.Create().Store value$",
        ] {
            assert_eq!(read(source), CallChainRead::Unsupported, "{source:?}")
        }
    }

    #[test]
    fn token_limit_and_cancellation_never_return_partial_evidence() {
        assert_eq!(
            call_chain_statement(b"a.Make().Visit 1", 3, &mut || Ok::<(), Infallible>(())).unwrap(),
            CallChainRead::TokenLimit
        );
        let source = format!(
            "a.Make().Visit {}value{}",
            "(".repeat(1500),
            ")".repeat(1500)
        );
        assert!(matches!(read(source.as_bytes()), CallChainRead::Parsed(_)));
        let mut callbacks = 0;
        let result = call_chain_statement(source.as_bytes(), 4096, &mut || {
            callbacks += 1;
            if callbacks == 20 {
                Err("cancelled")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("cancelled"));
        let literal = format!("a.Make().Visit \"{}\"", "x".repeat(4096));
        let mut callbacks = 0;
        assert_eq!(
            call_chain_statement(literal.as_bytes(), 4096, &mut || {
                callbacks += 1;
                if callbacks == 20 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            }),
            Err("cancelled")
        );
    }

    #[test]
    fn arbitrary_bytes_and_prefixes_keep_bounded_unique_source_spans() {
        let base = b"[factory].Make(value:=Helper(1)).Save [limit]:=First(2) + Second(3)";
        let verify = |source: &[u8]| {
            if let CallChainRead::Parsed(chain) = read(source) {
                assert!(chain.end() <= source.len());
                assert!(chain.parts().len() <= 8192);
                for (index, part) in chain.parts().iter().enumerate() {
                    assert!(part.span().start < part.span().end);
                    assert!(part.span().end <= chain.end());
                    assert!(!chain.parts()[..index].contains(part));
                }
            }
        };
        for end in 0..=base.len() {
            verify(&base[..end]);
        }
        for byte in 0..=u8::MAX {
            for at in 0..base.len() {
                let mut source = base.to_vec();
                source[at] = byte;
                verify(&source);
            }
        }
    }
}
