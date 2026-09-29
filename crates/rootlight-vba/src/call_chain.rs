//! Source-owned boundaries for call statements and value applications.
//! Syntax evidence is independent of native statement partitioning. It does not
//! establish target identity, argument compatibility, or return types.

use std::ops::Range;

use super::{group::quoted_end, name, trivia};

/// Syntax evidence within a complete, source-written statement or value expression.
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
    /// A member application evaluated as a value, allowing property-get selection.
    InvokedValueMember,
    /// An application including its arguments; call versus index is unresolved.
    Application,
    /// A named argument label, not a lexical value read.
    ArgumentName,
    /// The unbracketed instance keyword, distinct from an ordinary identifier.
    Instance,
    /// A direct instance member, excluding the `Me.` prefix from its span.
    SelfMember,
    /// A direct instance member used as a value application target.
    InvokedSelfMember,
}

/// One exact source span, relative to the supplied source prefix.
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

/// Complete syntax evidence for an admitted statement or value expression.
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

    /// Returns complete reference/application evidence for the admitted source.
    #[must_use]
    pub fn parts(&self) -> &[CallChainPart] {
        &self.parts
    }
}

/// Distinguishes unsupported syntax from exhaustion of the caller's token budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallChainRead {
    /// Syntax outside the selected reader's admitted expression subset.
    Unsupported,
    /// The statement requires more tokens than the caller permits.
    TokenLimit,
    /// Complete statement and argument syntax, without semantic binding.
    Parsed(CallChain),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Name,
    Instance,
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

struct Atom {
    next: usize,
    member: Option<usize>,
    invocation: Option<usize>,
    chained: bool,
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
    let tokens = match tokenize(source, maximum_tokens, false, check)? {
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
    let Some(atom) = reader.atom(start, tokens.len(), check)? else {
        return Ok(CallChainRead::Unsupported);
    };
    if !atom.chained {
        return Ok(CallChainRead::Unsupported);
    }
    let cursor = atom.next;
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
    reader.invoke(atom.member);
    if let Some(part) = atom
        .member
        .or(atom.invocation)
        .and_then(|index| reader.parts.get_mut(index))
        && part.role == CallChainRole::InvokedValueMember
    {
        part.role = CallChainRole::InvokedMember;
    }
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

/// Reads an application-containing value expression through its logical line end.
///
/// Uses the expression subset and nonrecursive, two-parts-per-token bounds of
/// [`call_chain_statement`], additionally admitting `Me` and direct instance
/// members without requiring a terminal member call. Names and
/// argument labels keep their exact spans; neither target identity nor call/index
/// semantics are inferred. Expressions without an application, unsupported syntax
/// and malformed input return `Unsupported` with no partial evidence. A bare
/// newline, comment or statement separator ends the input; continuations do not.
/// Callers must charge emitted parts to their fact budget.
///
/// # Errors
/// Propagates cancellation/work callback errors unchanged.
pub fn value_application_expression<E>(
    source: &[u8],
    maximum_tokens: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<CallChainRead, E> {
    value_expression(source, maximum_tokens, true, check)
}

/// Reads an assignment's `=` and complete value through its logical-line boundary.
///
/// Input begins immediately after the left-hand expression. Only logical trivia
/// may precede `=` or the value. The admitted value subset is the same as
/// [`value_application_expression`], but an application is not required. The
/// returned end excludes trailing trivia and comments. Unsupported, incomplete
/// or token-limited input returns `None`; a bare newline is never crossed.
/// This establishes syntax only, not assignability or value-type compatibility.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn assigned_value_end<E>(
    source: &[u8],
    maximum_tokens: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    check()?;
    let mut cursor = 0;
    trivia(source, &mut cursor, check)?;
    if source.get(cursor) != Some(&b'=') {
        return Ok(None);
    }
    cursor += 1;
    trivia(source, &mut cursor, check)?;
    let CallChainRead::Parsed(value) = value_expression(
        source.get(cursor..).unwrap_or_default(),
        maximum_tokens,
        false,
        check,
    )?
    else {
        return Ok(None);
    };
    Ok(cursor.checked_add(value.end()))
}

/// Finds the argument-list end in a complete possible indexed assignment tail.
///
/// Input begins immediately after the receiver and must contain `(indices) = value`,
/// with only logical trivia after the value. The returned end includes the closing
/// parenthesis and any leading trivia. RHS syntax is bounded by `maximum_tokens`.
/// `None` means incomplete, unsupported, or token-limited input. This does not
/// distinguish assignment from a call receiving a comparison expression: callers
/// must prove a receiver whose declared type has the corresponding setter.
/// Index expressions are only delimited here; their argument contract must also
/// be validated by the caller.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn indexed_assignment_arguments_end<E>(
    source: &[u8],
    maximum_tokens: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    let leading = super::logical_whitespace_len(source, check)?;
    let Some(group_end) =
        super::parenthesized_expression_end(source.get(leading..).unwrap_or_default(), check)?
    else {
        return Ok(None);
    };
    let Some(arguments_end) = leading.checked_add(group_end) else {
        return Ok(None);
    };
    let Some(value_end) = assigned_value_end(
        source.get(arguments_end..).unwrap_or_default(),
        maximum_tokens,
        check,
    )?
    else {
        return Ok(None);
    };
    let Some(end) = arguments_end.checked_add(value_end) else {
        return Ok(None);
    };
    let Some(rest) = source.get(end..) else {
        return Ok(None);
    };
    Ok((super::logical_whitespace_len(rest, check)? == rest.len()).then_some(arguments_end))
}

fn value_expression<E>(
    source: &[u8],
    maximum_tokens: usize,
    require_application: bool,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<CallChainRead, E> {
    check()?;
    let tokens = match tokenize(source, maximum_tokens, true, check)? {
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
    reader.jobs.push(Job {
        range: 0..tokens.len(),
        kind: JobKind::Expression,
    });
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
    if require_application
        && !reader
            .parts
            .iter()
            .any(|part| part.role == CallChainRole::Application)
    {
        return Ok(CallChainRead::Unsupported);
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
                CallChainRole::Member => CallChainRole::InvokedValueMember,
                CallChainRole::SelfMember => CallChainRole::InvokedSelfMember,
                unchanged @ (CallChainRole::InvokedValueIdentifier
                | CallChainRole::InvokedMember
                | CallChainRole::InvokedValueMember
                | CallChainRole::Application
                | CallChainRole::ArgumentName
                | CallChainRole::Instance
                | CallChainRole::InvokedSelfMember) => unchanged,
            };
        }
    }

    fn atom<E>(
        &mut self,
        start: usize,
        end: usize,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<Atom>, E> {
        let Some(first) = self.tokens.get(start).filter(|_| start < end) else {
            return Ok(None);
        };
        let source_start = first.span.start;
        let mut cursor = start + 1;
        let mut part = None;
        let mut invocation = None;
        let mut applied = false;
        let mut chained = false;
        match first.kind {
            Kind::Instance => {
                self.parts.push(CallChainPart {
                    span: first.span.clone(),
                    role: CallChainRole::Instance,
                });
            }
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
            Kind::Literal => {
                return Ok(Some(Atom {
                    next: cursor,
                    member: None,
                    invocation: None,
                    chained: false,
                }));
            }
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
                    invocation = None;
                    let direct_self = first.kind == Kind::Instance && cursor == start + 1;
                    self.parts.push(CallChainPart {
                        span: if direct_self {
                            member.span.clone()
                        } else {
                            source_start..member.span.end
                        },
                        role: if direct_self {
                            CallChainRole::SelfMember
                        } else {
                            CallChainRole::Member
                        },
                    });
                    cursor += 2;
                }
                Kind::Open(close) if close < end => {
                    if first.kind == Kind::Instance && cursor == start + 1 {
                        return Ok(None);
                    }
                    self.invoke(part);
                    invocation = part;
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
                | Kind::Instance
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
        Ok(Some(Atom {
            next: cursor,
            member: part,
            invocation,
            chained,
        }))
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
            let Some(atom) = self.atom(cursor, range.end, check)? else {
                return Ok(false);
            };
            cursor = atom.next;
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
    allow_instance: bool,
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
                } else if allow_instance && word.eq_ignore_ascii_case(b"Me") {
                    Kind::Instance
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
    fn indexed_assignment_tails_preserve_indices_and_require_a_complete_value() {
        for (source, indices) in [
            ("(2) = 3", "(2)"),
            (" (key:=Pick(1)) = value", " (key:=Pick(1))"),
            (" _\r\n (\"=\") _\r\n = Fetch(2) + 3 ", " _\r\n (\"=\")"),
            ("(1, 2) = left = right", "(1, 2)"),
            ("() = Nothing", "()"),
        ] {
            let end = indexed_assignment_arguments_end(source.as_bytes(), 128, &mut || {
                Ok::<(), Infallible>(())
            })
            .unwrap()
            .unwrap();
            assert_eq!(&source[..end], indices);
        }
        for source in [
            "(1)",
            "(1) =",
            "(1) == 2",
            "(1) = 2 +",
            "(1) = New Item",
            "(1)(2) = 3",
            "(1)\n=2",
            "(1) =\n2",
            "(1) = 2\nNext",
            "(1) = 2: Next",
            "(1) + 2",
            "(1) = 2, 3",
        ] {
            assert_eq!(
                indexed_assignment_arguments_end(source.as_bytes(), 128, &mut || {
                    Ok::<(), Infallible>(())
                })
                .unwrap(),
                None,
                "{source}"
            );
        }
        assert_eq!(
            indexed_assignment_arguments_end(
                b"(1) = Fetch(2)",
                1,
                &mut || Ok::<(), Infallible>(())
            )
            .unwrap(),
            None
        );
        assert_eq!(
            indexed_assignment_arguments_end(b"(1) = 2", 128, &mut || Err("cancelled")),
            Err("cancelled")
        );
    }

    #[test]
    fn indexed_assignment_scans_bound_arbitrary_bytes_and_cancel_inside_groups() {
        for byte in 0..=u8::MAX {
            let source = [b'(', byte, b')', b'=', byte];
            for end in 0..=source.len() {
                if let Some(offset) =
                    indexed_assignment_arguments_end(&source[..end], 64, &mut || {
                        Ok::<(), Infallible>(())
                    })
                    .unwrap()
                {
                    assert!(offset <= end);
                }
            }
        }
        let source = format!("({}) = 1", "key,".repeat(2048));
        let mut checks = 0;
        assert_eq!(
            indexed_assignment_arguments_end(source.as_bytes(), source.len(), &mut || {
                checks += 1;
                if checks == 12 {
                    Err("work exhausted")
                } else {
                    Ok(())
                }
            }),
            Err("work exhausted")
        );
    }

    #[test]
    fn assignment_values_require_complete_bounded_expressions() {
        for (source, prefix) in [
            (" = 7\nNextLine", " = 7"),
            (" = Nothing ' note", " = Nothing"),
            (
                " _\r\n = _\r\n Me.Entry(offset:=1) + 2: next",
                " _\r\n = _\r\n Me.Entry(offset:=1) + 2",
            ),
            ("=\"a=b\"", "=\"a=b\""),
            ("=value = other", "=value = other"),
        ] {
            let end = assigned_value_end(source.as_bytes(), 128, &mut || Ok::<(), Infallible>(()))
                .unwrap()
                .unwrap();
            assert_eq!(&source[..end], prefix);
        }
        for source in [
            "",
            " =",
            "=\nvalue",
            "\n=value",
            "=value +",
            "=Call Run()",
            "==1",
            "=one two",
            "=f(1",
            "=other!Item",
            "=New Item",
            "=Me()",
        ] {
            assert_eq!(
                assigned_value_end(source.as_bytes(), 128, &mut || Ok::<(), Infallible>(()))
                    .unwrap(),
                None,
                "{source}"
            );
        }
        assert_eq!(
            assigned_value_end(b"=value", 0, &mut || Ok::<(), Infallible>(())).unwrap(),
            None
        );
        assert_eq!(
            assigned_value_end(b"=value", 128, &mut || Err("cancelled")),
            Err("cancelled")
        );
        let deep = format!("={}1{}", "(".repeat(1024), ")".repeat(1024));
        assert_eq!(
            assigned_value_end(
                deep.as_bytes(),
                deep.len(),
                &mut || Ok::<(), Infallible>(())
            )
            .unwrap(),
            Some(deep.len())
        );
        let mut checks = 0;
        assert_eq!(
            assigned_value_end(deep.as_bytes(), deep.len(), &mut || {
                checks += 1;
                if checks == 12 {
                    Err("work exhausted")
                } else {
                    Ok(())
                }
            }),
            Err("work exhausted")
        );
        for byte in 0..=u8::MAX {
            let source = [b'=', byte, b'(', b'1', b')'];
            for end in 0..=source.len() {
                assigned_value_end(&source[..end], 64, &mut || Ok::<(), Infallible>(())).unwrap();
            }
        }
    }

    #[test]
    fn value_applications_preserve_instance_and_argument_roles() {
        let source = b"Me.[Item](index:=Other(1)) + values(2) ' note\nNextLine(3)";
        let CallChainRead::Parsed(expression) =
            value_application_expression(source, 128, &mut || Ok::<(), Infallible>(())).unwrap()
        else {
            panic!("complete expression")
        };
        assert_eq!(
            source.get(..expression.end()),
            Some(b"Me.[Item](index:=Other(1)) + values(2)".as_slice())
        );
        for (text, role) in [
            (b"Me".as_slice(), CallChainRole::Instance),
            (b"[Item]".as_slice(), CallChainRole::InvokedSelfMember),
            (
                b"Me.[Item](index:=Other(1))".as_slice(),
                CallChainRole::Application,
            ),
            (b"index".as_slice(), CallChainRole::ArgumentName),
            (b"Other".as_slice(), CallChainRole::InvokedValueIdentifier),
            (b"Other(1)".as_slice(), CallChainRole::Application),
            (b"values".as_slice(), CallChainRole::InvokedValueIdentifier),
            (b"values(2)".as_slice(), CallChainRole::Application),
        ] {
            assert_eq!(
                expression
                    .parts()
                    .iter()
                    .filter(|part| part.role() == role && source.get(part.span()) == Some(text))
                    .count(),
                1,
                "{text:?}"
            );
        }
        for source in [
            b"Pick( _\r\n 1)".as_slice(),
            b"Me.Factory().Name",
            b"Pick(Me)",
            b"[Me](1)",
        ] {
            assert!(
                matches!(
                    value_application_expression(source, 128, &mut || Ok::<(), Infallible>(()))
                        .unwrap(),
                    CallChainRead::Parsed(_)
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn value_applications_reject_partial_syntax_and_obey_limits() {
        for source in [
            b"Pick\n(1)".as_slice(),
            b"Pick: (1)",
            b"Pick(1) Other(2)",
            b"Pick(1",
            b"Me(1)",
            b"Pick(AddressOf Handler)",
            b"Pick!Key(1)",
            b"Pick(1) +",
        ] {
            assert_eq!(
                value_application_expression(source, 128, &mut || Ok::<(), Infallible>(()))
                    .unwrap(),
                CallChainRead::Unsupported,
                "{source:?}"
            );
        }
        assert_eq!(
            value_application_expression(b"Pick(1)", 2, &mut || Ok::<(), Infallible>(())).unwrap(),
            CallChainRead::TokenLimit
        );
        let mut work = 0;
        assert_eq!(
            value_application_expression(b"Pick(1) + Other(2)", 128, &mut || {
                work += 1;
                if work == 5 { Err("cancelled") } else { Ok(()) }
            }),
            Err("cancelled")
        );
        for byte in 0..=u8::MAX {
            let source = [b'F', b'(', byte, b')'];
            for end in 0..=source.len() {
                if let CallChainRead::Parsed(expression) = value_application_expression(
                    &source[..end],
                    8,
                    &mut || Ok::<(), Infallible>(()),
                )
                .unwrap()
                {
                    assert!(expression.parts().len() <= 16);
                    assert!(expression.end() <= end);
                    assert!(
                        expression
                            .parts()
                            .iter()
                            .all(|part| part.span().start < part.span().end
                                && part.span().end <= expression.end())
                    );
                }
            }
        }
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
            (b".Create".as_slice(), CallChainRole::InvokedValueMember),
            (
                b".Create(Codec.Trim(text))".as_slice(),
                CallChainRole::Application,
            ),
            (
                b".Create(Codec.Trim(text)).Store".as_slice(),
                CallChainRole::InvokedMember,
            ),
            (b"Codec.Trim".as_slice(), CallChainRole::InvokedValueMember),
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
    fn terminal_calls_do_not_change_receiver_or_argument_value_roles() {
        for (source, terminal) in [
            ("maker.Create().Store Codec.Read(1)", "maker.Create().Store"),
            (
                "Call maker.Create().Store(Codec.Read(1))",
                "maker.Create().Store",
            ),
            ("maker.Create().Store()(Codec.Read(1))", ""),
        ] {
            let CallChainRead::Parsed(chain) = read(source.as_bytes()) else {
                panic!("complete chain: {source}");
            };
            for text in ["maker.Create", "maker.Create().Store", "Codec.Read"] {
                let parts: Vec<_> = chain
                    .parts()
                    .iter()
                    .filter(|part| source.get(part.span()) == Some(text))
                    .collect();
                assert_eq!(parts.len(), 1, "{source}: {text}");
                assert_eq!(
                    parts[0].role(),
                    if text == terminal {
                        CallChainRole::InvokedMember
                    } else {
                        CallChainRole::InvokedValueMember
                    },
                    "{source}: {text}"
                );
            }
        }
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
