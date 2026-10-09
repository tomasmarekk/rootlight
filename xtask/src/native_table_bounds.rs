//! Checks generated parse-table literals against the reviewed uint16_t ABI.
//! Lexical source checks cannot replace native compilation, but prevent an
//! action or state index from being silently narrowed before runtime lookup.

use std::{error::Error, fmt};

/// Observed literal references, not grammar or native-runtime qualification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct NativeTableBounds {
    pub(crate) action_references: usize,
    pub(crate) state_references: usize,
    pub(crate) maximum_action: Option<u16>,
    pub(crate) maximum_state: Option<u16>,
}

/// The native ABI member whose uint16_t declaration was not uniquely proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbiMember {
    StateId,
    ParseTable,
    SmallParseTable,
    Language,
}

impl AbiMember {
    fn name(self) -> &'static str {
        match self {
            Self::StateId => "TSStateId",
            Self::ParseTable => "TSLanguage.parse_table",
            Self::SmallParseTable => "TSLanguage.small_parse_table",
            Self::Language => "TSLanguage",
        }
    }
}

/// The generated macro that writes a native table entry or shift state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexMacro {
    Actions,
    State,
    Shift,
    ShiftRepeat,
}

impl IndexMacro {
    fn name(self) -> &'static str {
        match self {
            Self::Actions => "ACTIONS",
            Self::State => "STATE",
            Self::Shift => "SHIFT",
            Self::ShiftRepeat => "SHIFT_REPEAT",
        }
    }
}

/// Distinguishes lexical failures in the parser from those in its ABI header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeInput {
    Parser,
    Header,
}

/// A malformed source/declaration or an index that would narrow at the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeTableBoundsError {
    HeaderDeclaration {
        member: AbiMember,
    },
    UnterminatedToken {
        input: NativeInput,
        offset: usize,
    },
    InvalidLiteral {
        kind: IndexMacro,
        offset: usize,
    },
    LiteralOverflow {
        kind: IndexMacro,
        offset: usize,
    },
    IndexOutOfRange {
        kind: IndexMacro,
        offset: usize,
        value: u64,
        maximum: u16,
    },
    AccountingOverflow,
}

impl fmt::Display for NativeTableBoundsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderDeclaration { member: AbiMember::Language } => formatter.write_str(
                "native header must declare exactly one TSLanguage struct with reviewed uint16_t tables"),
            Self::HeaderDeclaration { member } => write!(formatter,
                "native header must declare exactly one reviewed uint16_t {} ABI member", member.name()),
            Self::UnterminatedToken { input, offset } => write!(formatter,
                "unterminated native {} token at byte {offset}", match input {
                    NativeInput::Parser => "parser source", NativeInput::Header => "parser header",
                }),
            Self::InvalidLiteral { kind, offset } => write!(formatter,
                "native {} at byte {offset} requires one nonnegative C integer literal", kind.name()),
            Self::LiteralOverflow { kind, offset } => write!(formatter,
                "native {} integer literal at byte {offset} exceeds u64", kind.name()),
            Self::IndexOutOfRange { kind, offset, value, maximum } => write!(formatter,
                "native {} index {value} at byte {offset} exceeds declared uint16_t maximum {maximum}", kind.name()),
            Self::AccountingOverflow => formatter.write_str("native table reference accounting overflow"),
        }
    }
}

impl Error for NativeTableBoundsError {}

/// Validates ACTIONS, STATE, SHIFT and SHIFT_REPEAT literal invocations.
///
/// The supplied header must retain the runtime's uint16_t state alias and both
/// table pointers. Comments, quoted contents and C line splices are handled
/// lexically; preprocessing, compilation and table-array completeness are not
/// proved. Decimal, octal and hexadecimal literals with C U/L suffixes are
/// accepted, but expressions and negative values are not generated literals.
///
/// # Errors
/// Returns a typed error for an unproved ABI declaration, malformed lexical
/// input/literal, numeric overflow or a value outside the declared width.
pub(crate) fn validate(
    parser_source: &str,
    parser_header: &str,
) -> Result<NativeTableBounds, NativeTableBoundsError> {
    validate_header(parser_header)?;
    let mut lexer = Lexer::new(parser_source, NativeInput::Parser);
    let mut result = NativeTableBounds::default();
    while let Some(token) = lexer.next()? {
        let Some(kind) = index_macro(token, lexer.source) else {
            continue;
        };
        let mut invocation = lexer;
        if invocation
            .next()?
            .is_none_or(|token| token.kind != TokenKind::Punctuation(b'('))
        {
            continue;
        }
        let invalid = NativeTableBoundsError::InvalidLiteral {
            kind,
            offset: token.start,
        };
        let argument = invocation
            .next()?
            .filter(|token| token.kind == TokenKind::Number)
            .ok_or(invalid)?;
        if invocation
            .next()?
            .is_none_or(|token| token.kind != TokenKind::Punctuation(b')'))
        {
            return Err(invalid);
        }
        let value = integer(argument, lexer.source).map_err(|overflow| {
            if overflow {
                NativeTableBoundsError::LiteralOverflow {
                    kind,
                    offset: token.start,
                }
            } else {
                invalid
            }
        })?;
        let value = u16::try_from(value).map_err(|_| NativeTableBoundsError::IndexOutOfRange {
            kind,
            offset: token.start,
            value,
            maximum: u16::MAX,
        })?;
        let (count, maximum) = if kind == IndexMacro::Actions {
            (&mut result.action_references, &mut result.maximum_action)
        } else {
            (&mut result.state_references, &mut result.maximum_state)
        };
        *count = count
            .checked_add(1)
            .ok_or(NativeTableBoundsError::AccountingOverflow)?;
        *maximum = Some(maximum.map_or(value, |previous| previous.max(value)));
        lexer = invocation;
    }
    Ok(result)
}

fn index_macro(token: Token, source: &[u8]) -> Option<IndexMacro> {
    [
        IndexMacro::Actions,
        IndexMacro::State,
        IndexMacro::Shift,
        IndexMacro::ShiftRepeat,
    ]
    .into_iter()
    .find(|kind| token.is_word(source, kind.name().as_bytes()))
}

fn validate_header(header: &str) -> Result<(), NativeTableBoundsError> {
    let mut lexer = Lexer::new(header, NativeInput::Header);
    let mut aliases = 0usize;
    let mut languages = 0usize;
    while let Some(token) = lexer.next()? {
        if token.is_word(lexer.source, b"typedef") && state_alias(token, lexer)? {
            aliases = aliases
                .checked_add(1)
                .ok_or(NativeTableBoundsError::AccountingOverflow)?;
        }
        if token.is_word(lexer.source, b"struct") {
            let mut body = lexer;
            if body
                .next()?
                .is_some_and(|token| token.is_word(body.source, b"TSLanguage"))
                && body
                    .next()?
                    .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
            {
                language_fields(&mut body)?;
                languages = languages
                    .checked_add(1)
                    .ok_or(NativeTableBoundsError::AccountingOverflow)?;
                lexer = body;
            }
        }
    }
    if aliases != 1 {
        return Err(NativeTableBoundsError::HeaderDeclaration {
            member: AbiMember::StateId,
        });
    }
    if languages != 1 {
        return Err(NativeTableBoundsError::HeaderDeclaration {
            member: AbiMember::Language,
        });
    }
    Ok(())
}

fn state_alias(first: Token, mut lexer: Lexer<'_>) -> Result<bool, NativeTableBoundsError> {
    let mut tokens = [Some(first), None, None, None];
    let mut count = 1usize;
    let mut nesting = 0usize;
    let mut target = false;
    while let Some(token) = lexer.next()? {
        if nesting == 0 && token.is_word(lexer.source, b"TSStateId") {
            target = true;
        }
        if let Some(slot) = tokens.get_mut(count) {
            *slot = Some(token);
        }
        count = count
            .checked_add(1)
            .ok_or(NativeTableBoundsError::AccountingOverflow)?;
        match token.kind {
            TokenKind::Punctuation(b'{' | b'(' | b'[') => {
                nesting = nesting
                    .checked_add(1)
                    .ok_or(NativeTableBoundsError::AccountingOverflow)?;
            }
            TokenKind::Punctuation(b'}' | b')' | b']') => {
                nesting =
                    nesting
                        .checked_sub(1)
                        .ok_or(NativeTableBoundsError::HeaderDeclaration {
                            member: AbiMember::StateId,
                        })?;
            }
            TokenKind::Punctuation(b';') if nesting == 0 => {
                if target
                    && (count != 4
                        || !tokens[1].is_some_and(|token| token.is_word(lexer.source, b"uint16_t"))
                        || !tokens[2]
                            .is_some_and(|token| token.is_word(lexer.source, b"TSStateId")))
                {
                    return Err(NativeTableBoundsError::HeaderDeclaration {
                        member: AbiMember::StateId,
                    });
                }
                return Ok(target);
            }
            _ => {}
        }
    }
    Err(NativeTableBoundsError::HeaderDeclaration {
        member: AbiMember::StateId,
    })
}

fn language_fields(lexer: &mut Lexer<'_>) -> Result<(), NativeTableBoundsError> {
    let mut nesting = 1usize;
    let mut tokens = [None; 5];
    let mut count = 0usize;
    let mut seen = [false; 2];
    let mut declarations = [0usize; 2];
    let members = [
        (b"parse_table".as_slice(), AbiMember::ParseTable),
        (b"small_parse_table".as_slice(), AbiMember::SmallParseTable),
    ];
    while let Some(token) = lexer.next()? {
        match token.kind {
            TokenKind::Punctuation(b'{') => {
                nesting = nesting
                    .checked_add(1)
                    .ok_or(NativeTableBoundsError::AccountingOverflow)?;
            }
            TokenKind::Punctuation(b'}') => {
                nesting -= 1;
                if nesting == 0 {
                    for (index, (_, member)) in members.iter().enumerate() {
                        if declarations[index] != 1 {
                            return Err(NativeTableBoundsError::HeaderDeclaration {
                                member: *member,
                            });
                        }
                    }
                    return Ok(());
                }
            }
            _ if nesting == 1 => {
                if let Some(slot) = tokens.get_mut(count) {
                    *slot = Some(token);
                }
                count = count
                    .checked_add(1)
                    .ok_or(NativeTableBoundsError::AccountingOverflow)?;
                for (index, (name, _)) in members.iter().enumerate() {
                    seen[index] |= token.is_word(lexer.source, name);
                }
                if token.kind == TokenKind::Punctuation(b';') {
                    for (index, (name, member)) in members.iter().enumerate() {
                        if !seen[index] {
                            continue;
                        }
                        if count != 5
                            || !tokens[0].is_some_and(|token| token.is_word(lexer.source, b"const"))
                            || !tokens[1]
                                .is_some_and(|token| token.is_word(lexer.source, b"uint16_t"))
                            || !tokens[2]
                                .is_some_and(|token| token.kind == TokenKind::Punctuation(b'*'))
                            || !tokens[3].is_some_and(|token| token.is_word(lexer.source, name))
                        {
                            return Err(NativeTableBoundsError::HeaderDeclaration {
                                member: *member,
                            });
                        }
                        declarations[index] = declarations[index]
                            .checked_add(1)
                            .ok_or(NativeTableBoundsError::AccountingOverflow)?;
                    }
                    tokens = [None; 5];
                    count = 0;
                    seen = [false; 2];
                }
            }
            _ => {}
        }
    }
    Err(NativeTableBoundsError::HeaderDeclaration {
        member: AbiMember::Language,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    Number,
    Quoted,
    Punctuation(u8),
}

#[derive(Debug, Clone, Copy)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

impl Token {
    fn is_word(self, source: &[u8], word: &[u8]) -> bool {
        self.kind == TokenKind::Word && self.bytes(source).eq(word.iter().copied())
    }

    fn bytes(self, source: &[u8]) -> impl Iterator<Item = u8> + '_ {
        let mut lexer = Lexer {
            source: source.get(..self.end).unwrap_or_default(),
            position: self.start,
            input: NativeInput::Parser,
        };
        std::iter::from_fn(move || lexer.take())
    }
}

#[derive(Clone, Copy)]
struct Lexer<'a> {
    source: &'a [u8],
    position: usize,
    input: NativeInput,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, input: NativeInput) -> Self {
        Self {
            source: source.as_bytes(),
            position: 0,
            input,
        }
    }

    fn peek(&mut self) -> Option<u8> {
        while self.source.get(self.position) == Some(&b'\\') {
            let suffix = self.source.get(self.position..)?;
            let width = if suffix.starts_with(b"\\\r\n") {
                3
            } else if suffix.starts_with(b"\\\n") {
                2
            } else {
                break;
            };
            self.position += width;
        }
        self.source.get(self.position).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn next(&mut self) -> Result<Option<Token>, NativeTableBoundsError> {
        loop {
            while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
                self.take();
            }
            let Some(byte) = self.peek() else {
                return Ok(None);
            };
            let start = self.position;
            self.take();
            if byte == b'/' {
                if self.peek() == Some(b'/') {
                    while self.take().is_some_and(|byte| byte != b'\n') {}
                    continue;
                }
                if self.peek() == Some(b'*') {
                    self.take();
                    let mut previous = 0u8;
                    loop {
                        let byte =
                            self.take()
                                .ok_or(NativeTableBoundsError::UnterminatedToken {
                                    input: self.input,
                                    offset: start,
                                })?;
                        if previous == b'*' && byte == b'/' {
                            break;
                        }
                        previous = byte;
                    }
                    continue;
                }
            }
            let kind = if matches!(byte, b'\'' | b'"') {
                loop {
                    let next = self
                        .take()
                        .ok_or(NativeTableBoundsError::UnterminatedToken {
                            input: self.input,
                            offset: start,
                        })?;
                    if next == byte {
                        break;
                    }
                    if next == b'\\' {
                        self.take()
                            .ok_or(NativeTableBoundsError::UnterminatedToken {
                                input: self.input,
                                offset: start,
                            })?;
                    } else if matches!(next, b'\r' | b'\n') {
                        return Err(NativeTableBoundsError::UnterminatedToken {
                            input: self.input,
                            offset: start,
                        });
                    }
                }
                TokenKind::Quoted
            } else if byte.is_ascii_alphabetic() || byte == b'_' || byte.is_ascii_digit() {
                while self.peek().is_some_and(|next| {
                    next.is_ascii_alphanumeric()
                        || next == b'_'
                        || (byte.is_ascii_digit() && next == b'.')
                }) {
                    self.take();
                }
                if byte.is_ascii_digit() {
                    TokenKind::Number
                } else {
                    TokenKind::Word
                }
            } else {
                TokenKind::Punctuation(byte)
            };
            return Ok(Some(Token {
                kind,
                start,
                end: self.position,
            }));
        }
    }
}

// `true` distinguishes arithmetic overflow from malformed integer syntax.
fn integer(token: Token, source: &[u8]) -> Result<u64, bool> {
    let mut bytes = token.bytes(source).peekable();
    let first = bytes.next().filter(u8::is_ascii_digit).ok_or(false)?;
    let mut value = u64::from(first - b'0');
    let mut radix = if first == b'0' { 8u64 } else { 10u64 };
    let mut has_digit = true;
    if first == b'0'
        && bytes
            .peek()
            .is_some_and(|byte| matches!(*byte, b'x' | b'X'))
    {
        bytes.next();
        radix = 16;
        has_digit = false;
    }
    while let Some(byte) = bytes.peek().copied() {
        let digit = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            b'a'..=b'f' if radix == 16 => u64::from(byte - b'a' + 10),
            b'A'..=b'F' if radix == 16 => u64::from(byte - b'A' + 10),
            _ => break,
        };
        if digit >= radix {
            return Err(false);
        }
        value = value
            .checked_mul(radix)
            .and_then(|value| value.checked_add(digit))
            .ok_or(true)?;
        has_digit = true;
        bytes.next();
    }
    let mut suffix = [0u8; 3];
    let mut length = 0usize;
    for byte in bytes {
        *suffix.get_mut(length).ok_or(false)? = byte;
        length += 1;
    }
    let unsigned = |byte: u8| matches!(byte, b'u' | b'U');
    let long = |byte: u8| matches!(byte, b'l' | b'L');
    let valid_suffix = match suffix.get(..length).ok_or(false)? {
        [] => true,
        [a] => unsigned(*a) || long(*a),
        [a, b] => (unsigned(*a) && long(*b)) || (long(*a) && unsigned(*b)) || (long(*a) && a == b),
        [a, b, c] => (unsigned(*a) && long(*b) && b == c) || (long(*a) && a == b && unsigned(*c)),
        _ => false,
    };
    (has_digit && valid_suffix).then_some(value).ok_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "typedef uint16_t TSStateId;\nstruct TSLanguage {\nconst uint16_t *parse_table;\nconst uint16_t *small_parse_table;\nconst uint32_t *small_parse_table_map;\n};";

    #[test]
    fn accepts_the_declared_boundary_without_a_smaller_policy_cap() {
        assert_eq!(validate("ACTIONS(0), ACTIONS(46020), ACTIONS(65535), STATE(65535), SHIFT(0), SHIFT_REPEAT(65535)", HEADER).unwrap(),
            NativeTableBounds { action_references: 3, state_references: 3,
                maximum_action: Some(65535), maximum_state: Some(65535) });
    }

    #[test]
    fn rejects_action_offsets_that_would_silently_narrow() {
        for value in [65536u64, 70779, u64::from(u32::MAX)] {
            assert_eq!(
                validate(&format!("ACTIONS({value})"), HEADER),
                Err(NativeTableBoundsError::IndexOutOfRange {
                    kind: IndexMacro::Actions,
                    offset: 0,
                    value,
                    maximum: u16::MAX
                })
            );
        }
    }

    #[test]
    fn all_state_macro_forms_reject_out_of_width_indexes() {
        for kind in [
            IndexMacro::State,
            IndexMacro::Shift,
            IndexMacro::ShiftRepeat,
        ] {
            assert_eq!(
                validate(&format!("{}(65536)", kind.name()), HEADER),
                Err(NativeTableBoundsError::IndexOutOfRange {
                    kind,
                    offset: 0,
                    value: 65536,
                    maximum: u16::MAX
                })
            );
        }
    }

    #[test]
    fn c_integer_radices_and_suffixes_cannot_disguise_overflow() {
        for value in [
            "0xffff",
            "0177777",
            "65535U",
            "0XffffULL",
            "0177777lu",
            "65535LL",
        ] {
            assert_eq!(
                validate(&format!("ACTIONS({value})"), HEADER)
                    .unwrap()
                    .maximum_action,
                Some(u16::MAX)
            );
        }
        for value in ["0x10000", "0200000", "65536ULL"] {
            assert!(matches!(
                validate(&format!("ACTIONS({value})"), HEADER),
                Err(NativeTableBoundsError::IndexOutOfRange { value: 65536, .. })
            ));
        }
    }

    #[test]
    fn malformed_empty_signed_expression_and_overflowing_arguments_are_rejected() {
        for value in [
            "", "-1", "+1", "value", "1+2", "1,2", "(1)", "0x", "09", "1.0", "1e2", "1UU", "1lL",
            "0b10",
        ] {
            assert!(
                matches!(
                    validate(&format!("ACTIONS({value})"), HEADER),
                    Err(NativeTableBoundsError::InvalidLiteral {
                        kind: IndexMacro::Actions,
                        ..
                    })
                ),
                "{value}"
            );
        }
        assert!(matches!(
            validate("ACTIONS(18446744073709551616)", HEADER),
            Err(NativeTableBoundsError::LiteralOverflow { .. })
        ));
        assert!(matches!(
            validate("SHIFT(1", HEADER),
            Err(NativeTableBoundsError::InvalidLiteral { .. })
        ));
    }

    #[test]
    fn comments_quoted_contents_and_identifier_suffixes_are_inert() {
        let source = "/* ACTIONS(70779) */\n// SHIFT(65536)\n\
            const char *s=\"STATE(65536) \\\" ACTIONS(999999)\"; char c='\\'';\n\
            MY_ACTIONS(70000); SHIFT_EXTRA(); END_STATE(); ACTIONS /* trivia */ (7);";
        assert_eq!(
            validate(source, HEADER).unwrap(),
            NativeTableBounds {
                action_references: 1,
                state_references: 0,
                maximum_action: Some(7),
                maximum_state: None
            }
        );
    }

    #[test]
    fn c_line_splices_are_removed_before_macro_and_comment_recognition() {
        assert_eq!(
            validate("AC\\\nTIONS(65\\\r\n535)", HEADER)
                .unwrap()
                .maximum_action,
            Some(u16::MAX)
        );
        assert!(matches!(
            validate("SHI\\\nFT_REPEAT(65536)", HEADER),
            Err(NativeTableBoundsError::IndexOutOfRange {
                kind: IndexMacro::ShiftRepeat,
                ..
            })
        ));
        assert_eq!(
            validate("/\\\n/ ACTIONS(70779)\\\n SHIFT(65536)\nACTIONS(1)", HEADER)
                .unwrap()
                .action_references,
            1
        );
    }

    #[test]
    fn widening_a_header_does_not_change_the_reviewed_runtime_abi() {
        for (original, replacement, member) in [
            (
                "typedef uint16_t TSStateId;",
                "typedef uint32_t TSStateId;",
                AbiMember::StateId,
            ),
            (
                "const uint16_t *parse_table;",
                "const uint32_t *parse_table;",
                AbiMember::ParseTable,
            ),
            (
                "const uint16_t *small_parse_table;",
                "const uint32_t *small_parse_table;",
                AbiMember::SmallParseTable,
            ),
        ] {
            assert_eq!(
                validate("ACTIONS(1)", &HEADER.replace(original, replacement)),
                Err(NativeTableBoundsError::HeaderDeclaration { member })
            );
        }
    }

    #[test]
    fn missing_duplicate_or_unrelated_declarations_cannot_prove_the_header() {
        for header in [
            HEADER.replace("const uint16_t *parse_table;", ""),
            HEADER.replace(
                "const uint16_t *parse_table;",
                "/* const uint16_t *parse_table; */",
            ),
            HEADER.replace(
                "const uint16_t *parse_table;",
                "struct Other { const uint16_t *parse_table; };",
            ),
            HEADER.replace(
                "const uint16_t *parse_table;",
                "const uint16_t *parse_table; const uint16_t *parse_table;",
            ),
            HEADER.replace("TSLanguage {", "Other {"),
            format!("{HEADER}\n{HEADER}"),
            HEADER.replace(
                "typedef uint16_t TSStateId;",
                "/* typedef uint16_t TSStateId; */",
            ),
            HEADER.replace(
                "typedef uint16_t TSStateId;",
                "typedef uint16_t TSStateId; typedef uint32_t TSStateId;",
            ),
        ] {
            assert!(matches!(
                validate("ACTIONS(1)", &header),
                Err(NativeTableBoundsError::HeaderDeclaration { .. })
            ));
        }
    }

    #[test]
    fn header_trivia_and_unrelated_nested_members_do_not_change_width_proof() {
        let header = "extern \"C\" { typedef /* state */ uint16_t TSStateId;\n\
            struct TSLanguage { const uint16_t /* table */ *parse_table;\n\
            struct { void (*scan)(TSStateId); } external_scanner;\n\
            const uint16_t *small_parse_table; const uint32_t *small_parse_table_map; }; }";
        assert_eq!(
            validate("ACTIONS(1)", header).unwrap().maximum_action,
            Some(1)
        );
    }

    #[test]
    fn unterminated_lexical_contents_cannot_hide_later_indexes() {
        for source in [
            "/* ACTIONS(70779)",
            "\"ACTIONS(70779)",
            "'x",
            "\"raw\nnewline\"",
        ] {
            assert!(matches!(
                validate(source, HEADER),
                Err(NativeTableBoundsError::UnterminatedToken {
                    input: NativeInput::Parser,
                    ..
                })
            ));
        }
        assert!(matches!(
            validate("ACTIONS(1)", "/* typedef uint16_t TSStateId;"),
            Err(NativeTableBoundsError::UnterminatedToken {
                input: NativeInput::Header,
                ..
            })
        ));
    }

    #[test]
    fn diagnostic_offsets_are_original_physical_source_offsets() {
        let source = "/* π */\nSHIFT(70779)";
        assert_eq!(
            validate(source, HEADER),
            Err(NativeTableBoundsError::IndexOutOfRange {
                kind: IndexMacro::Shift,
                offset: source.find("SHIFT").unwrap(),
                value: 70779,
                maximum: u16::MAX
            })
        );
        let error = validate("ACTIONS(secret_token)", HEADER)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret_token"));
        assert!(error.contains("ACTIONS"));
    }
}
