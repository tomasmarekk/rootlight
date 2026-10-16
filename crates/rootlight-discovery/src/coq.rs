//! Bounded Coq/Rocq command evidence for the shared `.v` suffix.
//!
//! Complete written sentences establish attribution only. Comments, strings,
//! dynamic notations, proof checking and command validity require separate care.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a [u8]),
    Punctuation(u8),
    Literal,
    Newline,
    SentenceEnd,
}

#[derive(Clone, Copy)]
enum StringSyntax {
    Rocq,
    BackslashEscaped,
}

struct Lexeme<'a> {
    offset: usize,
    token: Token<'a>,
}

fn string_end(source: &[u8], mut cursor: usize, syntax: StringSyntax) -> Option<usize> {
    while let Some(&byte) = source.get(cursor) {
        cursor += 1;
        if byte == b'\\' && matches!(syntax, StringSyntax::BackslashEscaped) {
            cursor = (cursor + 1).min(source.len());
            continue;
        }
        if byte == b'"' {
            if matches!(syntax, StringSyntax::Rocq) && source.get(cursor) == Some(&b'"') {
                cursor += 1;
            } else {
                return Some(cursor);
            }
        }
    }
    None
}

fn comment_end(source: &[u8], mut cursor: usize, open: &[u8], close: &[u8]) -> usize {
    let mut depth = 1usize;
    while cursor < source.len() {
        let rest = &source[cursor..];
        if rest.starts_with(open) {
            depth += 1;
            cursor += open.len();
        } else if rest.starts_with(close) {
            depth -= 1;
            cursor += close.len();
            if depth == 0 {
                return cursor;
            }
        } else if rest.starts_with(b"\"") {
            // Rocq also requires properly closed strings inside nested comments.
            cursor = string_end(source, cursor + 1, StringSyntax::Rocq).unwrap_or(source.len());
        } else {
            cursor += 1;
        }
    }
    source.len()
}

fn tokens(source: &[u8], truncated: bool, syntax: StringSyntax) -> Vec<Lexeme<'_>> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(&byte) = source.get(cursor) {
        let rest = &source[cursor..];
        let offset = cursor;
        let mut push = |token| result.push(Lexeme { offset, token });
        if byte == b'\n' {
            push(Token::Newline);
            cursor += 1;
        } else if byte.is_ascii_whitespace() {
            cursor += 1;
        } else if rest.starts_with(b"(*") || rest.starts_with(b"/*") {
            let (open, close) = if rest.starts_with(b"(*") {
                (b"(*".as_slice(), b"*)".as_slice())
            } else {
                (b"/*".as_slice(), b"*/".as_slice())
            };
            let end = comment_end(source, cursor + 2, open, close);
            if source[cursor..end].contains(&b'\n') {
                push(Token::Newline);
            }
            cursor = end;
        } else if rest.starts_with(b"//") || rest.starts_with(b"#!") {
            cursor += rest
                .iter()
                .position(|&value| value == b'\n')
                .unwrap_or(rest.len());
        } else if byte == b'"' {
            cursor = string_end(source, cursor + 1, syntax).unwrap_or(source.len());
            push(Token::Literal);
        } else if byte == b'`' {
            cursor += 1;
            cursor += source[cursor..]
                .iter()
                .position(|&value| value == b'`')
                .map_or(source.len() - cursor, |length| length + 1);
            push(Token::Literal);
        } else if byte == b'\''
            && rest[1..]
                .iter()
                .take_while(|&&value| value != b'\n')
                .position(|&value| value == b'\'')
                .is_some()
        {
            cursor += 1;
            while let Some(&next) = source.get(cursor) {
                cursor += 1;
                if next == b'\\' {
                    cursor = (cursor + 1).min(source.len());
                } else if next == b'\'' {
                    break;
                }
            }
            push(Token::Literal);
        } else if byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 {
            let length = rest
                .iter()
                .take_while(|&&value| {
                    value.is_ascii_alphanumeric() || matches!(value, b'_' | b'\'') || value >= 0x80
                })
                .count();
            push(Token::Name(&rest[..length]));
            cursor += length;
        } else {
            let after = source.get(cursor + 1..).unwrap_or_default();
            let sentence_end = byte == b'.'
                && (after.first().is_some_and(u8::is_ascii_whitespace)
                    || after.starts_with(b"(*")
                    || (after.is_empty() && !truncated));
            push(if sentence_end {
                Token::SentenceEnd
            } else {
                Token::Punctuation(byte)
            });
            cursor += 1;
        }
    }
    result
}

fn qualified_name_end(tokens: &[Token<'_>], start: usize) -> Option<usize> {
    if !matches!(tokens.get(start), Some(Token::Name(name)) if *name != b"_") {
        return None;
    }
    let mut end = start + 1;
    while matches!(
        tokens.get(end..end + 2),
        Some([Token::Punctuation(b'.'), Token::Name(_)])
    ) {
        end += 2;
    }
    Some(end)
}

fn imports(tokens: &[Token<'_>], mut start: usize) -> bool {
    if matches!(tokens.get(start), Some(Token::Name(b"Import" | b"Export"))) {
        start += 1;
    }
    let first = start;
    while let Some(end) = qualified_name_end(tokens, start) {
        start = end;
    }
    start > first && start == tokens.len()
}

fn command(tokens: &[Lexeme<'_>]) -> bool {
    let compact = tokens
        .iter()
        .map(|lexeme| lexeme.token)
        .filter(|token| *token != Token::Newline)
        .collect::<Vec<_>>();
    let mut rest = compact.as_slice();
    while rest.starts_with(&[Token::Punctuation(b'#'), Token::Punctuation(b'[')]) {
        let mut depth = 1;
        let mut end = 2;
        while let Some(token) = rest.get(end) {
            match token {
                Token::Punctuation(b'[') => depth += 1,
                Token::Punctuation(b']') => depth -= 1,
                _ => {}
            }
            end += 1;
            if depth == 0 {
                break;
            }
        }
        if depth != 0 {
            return false;
        }
        rest = &rest[end..];
    }
    while matches!(
        rest.first(),
        Some(Token::Name(
            b"Local"
                | b"Global"
                | b"Polymorphic"
                | b"Monomorphic"
                | b"Cumulative"
                | b"NonCumulative"
                | b"Private"
                | b"Program"
        ))
    ) {
        rest = &rest[1..];
    }
    match rest {
        [
            Token::Name(b"Declare"),
            Token::Name(b"ML"),
            Token::Name(b"Module"),
            Token::Literal,
            tail @ ..,
        ] => tail.iter().all(|token| *token == Token::Literal),
        [
            Token::Name(b"Declare"),
            Token::Name(b"Instance"),
            Token::Name(name),
            Token::Punctuation(b':'),
            first,
            ..,
        ] => *name != b"_" && !matches!(first, Token::Punctuation(b':' | b'=')),
        [Token::Name(b"Existing"), Token::Name(b"Class"), tail @ ..] => {
            qualified_name_end(tail, 0) == Some(tail.len()) && !tail.contains(&Token::Name(b"_"))
        }
        [Token::Name(b"Require"), tail @ ..] => imports(tail, 0),
        [Token::Name(b"From"), tail @ ..] => qualified_name_end(tail, 0).is_some_and(|end| {
            tail.get(end) == Some(&Token::Name(b"Require")) && imports(tail, end + 1)
        }),
        [Token::Name(b"Section" | b"End"), Token::Name(name)] => *name != b"_",
        [
            Token::Name(b"Module"),
            Token::Name(b"Type"),
            Token::Name(name),
            ..,
        ]
        | [Token::Name(b"Module"), Token::Name(name), ..] => *name != b"_",
        [Token::Name(b"Notation"), Token::Literal, tail @ ..] => tail
            .windows(2)
            .any(|pair| pair == [Token::Punctuation(b':'), Token::Punctuation(b'=')]),
        [
            Token::Name(
                b"Definition" | b"Let" | b"Fixpoint" | b"CoFixpoint" | b"Inductive"
                | b"CoInductive" | b"Record" | b"Structure" | b"Class" | b"Instance" | b"Lemma"
                | b"Theorem" | b"Fact" | b"Corollary" | b"Remark" | b"Proposition" | b"Example"
                | b"Axiom" | b"Parameter" | b"Hypothesis" | b"Variable" | b"Ltac",
            ),
            Token::Name(name),
            tail @ ..,
        ] => *name != b"_" && tail.contains(&Token::Punctuation(b':')),
        _ => false,
    }
}

pub(super) fn has_content_hint(source: &[u8], truncated: bool) -> bool {
    let source = source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source);
    let rocq = tokens(source, truncated, StringSyntax::Rocq);
    let foreign = (source.windows(2).any(|pair| pair == b"\\\"")
        && (super::hdl::has_content_hint(source)
            || super::v_language::has_content_hint(source, truncated)))
    .then(|| tokens(source, truncated, StringSyntax::BackslashEscaped));
    for start in sentence_starts(&rocq) {
        if foreign.as_ref().is_some_and(|foreign| {
            foreign
                .binary_search_by_key(&rocq[start].offset, |lexeme| lexeme.offset)
                .is_err()
        }) {
            continue;
        }
        if first_sentence_is_command(&rocq[start..]) {
            return true;
        }
    }
    // An escaped foreign quote can also hide a later, real command from the
    // Rocq-only view. Recover only visible heads and read each body as Rocq.
    foreign.as_ref().is_some_and(|foreign| {
        sentence_starts(foreign).any(|start| {
            let offset = foreign[start].offset;
            rocq.binary_search_by_key(&offset, |lexeme| lexeme.offset)
                .is_err()
                && first_sentence_is_command(&tokens(
                    &source[offset..],
                    truncated,
                    StringSyntax::Rocq,
                ))
        })
    })
}

fn sentence_starts(tokens: &[Lexeme<'_>]) -> impl Iterator<Item = usize> {
    (0..tokens.len()).filter(|&start| {
        start == 0 || matches!(tokens[start - 1].token, Token::Newline | Token::SentenceEnd)
    })
}

fn first_sentence_is_command(tokens: &[Lexeme<'_>]) -> bool {
    tokens
        .iter()
        .position(|lexeme| lexeme.token == Token::SentenceEnd)
        .is_some_and(|length| command(&tokens[..length]))
}
