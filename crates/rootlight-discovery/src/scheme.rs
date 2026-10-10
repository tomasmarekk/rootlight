//! The shared `.scm` suffix does not distinguish Scheme from Tree-sitter queries.
//! This bounded lexical fence masks reader data before recognizing capture suffixes.
//! It suppresses extension evidence only; configuration and other signals survive.

use super::MAX_CLASSIFICATION_BYTES;

#[derive(Clone, Copy)]
struct Cursor<'a> {
    rest: &'a str,
    incomplete: bool,
}

impl<'a> Cursor<'a> {
    fn required_character(&mut self) -> Option<char> {
        let character = self.rest.chars().next();
        self.incomplete |= character.is_none();
        character
    }

    fn advance(&mut self, bytes: usize) -> Option<()> {
        self.rest = self.rest.get(bytes..)?;
        Some(())
    }

    fn trivia(&mut self) -> Option<()> {
        loop {
            self.rest = self.rest.trim_start_matches(char::is_whitespace);
            if let Some(comment) = self.rest.strip_prefix(';') {
                self.rest = comment.find(['\r', '\n']).map_or("", |at| &comment[at..]);
            } else if self.rest.starts_with("#|") {
                self.advance(2)?;
                let mut depth = 1_usize;
                while depth != 0 {
                    if self.rest.starts_with("#|") {
                        depth = depth.checked_add(1)?;
                        self.advance(2)?;
                    } else if self.rest.starts_with("|#") {
                        depth = depth.checked_sub(1)?;
                        self.advance(2)?;
                    } else {
                        let character = self.required_character()?;
                        self.advance(character.len_utf8())?;
                    }
                }
            } else {
                return Some(());
            }
        }
    }

    fn literal(&mut self, delimiter: char) -> Option<()> {
        self.advance(delimiter.len_utf8())?;
        while let Some(character) = self.rest.chars().next() {
            self.advance(character.len_utf8())?;
            if character == '\\' {
                let escaped = self.required_character()?;
                self.advance(escaped.len_utf8())?;
            } else if character == delimiter {
                return Some(());
            }
        }
        self.incomplete = true;
        None
    }

    fn atom(&mut self) -> Option<&'a str> {
        let start = self.rest;
        while let Some(character) = self.rest.chars().next() {
            if delimiter(character) {
                break;
            }
            // A Scheme hexadecimal escape owns its semicolon; it cannot start
            // an intertoken comment or expose an embedded capture-looking name.
            if self.rest.starts_with("\\x") || self.rest.starts_with("\\X") {
                self.advance(2)?;
                let digits = self.rest.bytes().take_while(u8::is_ascii_hexdigit).count();
                if digits == 0 {
                    self.incomplete |= self.rest.is_empty();
                    return None;
                }
                self.advance(digits)?;
                self.incomplete |= self.rest.is_empty();
                self.rest = self.rest.strip_prefix(';')?;
            } else {
                self.advance(character.len_utf8())?;
            }
        }
        let bytes = start.len().checked_sub(self.rest.len())?;
        (bytes != 0).then(|| start.get(..bytes)).flatten()
    }

    fn character(&mut self) -> Option<()> {
        self.advance(2)?;
        let first = self.required_character()?;
        self.advance(first.len_utf8())?;
        if !delimiter(first) {
            while let Some(character) = self.rest.chars().next() {
                if delimiter(character) {
                    break;
                }
                self.advance(character.len_utf8())?;
            }
        }
        Some(())
    }

    fn compound(&mut self) -> Option<()> {
        let mut closes = Vec::new();
        loop {
            self.trivia()?;
            let character = self.required_character()?;
            match character {
                '(' | '[' => {
                    closes.try_reserve(1).ok()?;
                    closes.push(if character == '(' { ')' } else { ']' });
                    self.advance(character.len_utf8())?;
                }
                ')' | ']' => {
                    if closes.pop()? != character {
                        return None;
                    }
                    self.advance(character.len_utf8())?;
                    if closes.is_empty() {
                        return Some(());
                    }
                }
                '"' | '|' => self.literal(character)?,
                '#' if self.rest.starts_with("#\\") => self.character()?,
                '#' if self.rest.starts_with("#;") => self.advance(2)?,
                '\'' | '`' | ',' => self.advance(character.len_utf8())?,
                _ => {
                    self.atom()?;
                }
            }
        }
    }

    fn datum(&mut self) -> Option<()> {
        let mut pending = 1_usize;
        while pending != 0 {
            self.trivia()?;
            if self.rest.starts_with("#;") {
                self.advance(2)?;
                pending = pending.checked_add(1)?;
                continue;
            }
            if reader_prefix(self) || label_prefix(self) {
                continue;
            }
            let character = self.required_character()?;
            match character {
                '(' | '[' => self.compound()?,
                '"' | '|' => self.literal(character)?,
                '#' if self.rest.starts_with("#\\") => self.character()?,
                '#' if vector_prefix(self) => self.compound()?,
                ')' | ']' => return None,
                _ => {
                    self.atom()?;
                }
            }
            pending = pending.checked_sub(1)?;
        }
        Some(())
    }

    fn intertoken(&mut self) -> Option<()> {
        loop {
            self.trivia()?;
            if !self.rest.starts_with("#;") {
                return Some(());
            }
            self.advance(2)?;
            self.datum()?;
        }
    }

    fn quoted_form(&mut self) -> Option<Self> {
        let open = self.rest.chars().next()?;
        let close = match open {
            '(' => ')',
            '[' => ']',
            _ => return None,
        };
        let mut cursor = *self;
        cursor.advance(open.len_utf8())?;
        cursor.intertoken()?;
        if !matches!(
            cursor.atom()?,
            "quote"
                | "quasiquote"
                | "unquote"
                | "unquote-splicing"
                | "syntax"
                | "quasisyntax"
                | "unsyntax"
                | "unsyntax-splicing"
        ) {
            return None;
        }
        if cursor.datum().is_none() || cursor.intertoken().is_none() {
            self.incomplete |= cursor.incomplete;
            return None;
        }
        if cursor.rest.is_empty() {
            self.incomplete = true;
            return None;
        }
        cursor.rest = cursor.rest.strip_prefix(close)?;
        Some(cursor)
    }
}

fn delimiter(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '(' | ')' | '[' | ']' | '"' | '|' | '\'' | '`' | ',' | ';'
        )
}

fn reader_prefix(cursor: &mut Cursor<'_>) -> bool {
    for prefix in ["#,@", "#'", "#`", "#,", ",@", "'", "`", ","] {
        if let Some(rest) = cursor.rest.strip_prefix(prefix) {
            cursor.rest = rest;
            return true;
        }
    }
    false
}

fn label_prefix(cursor: &mut Cursor<'_>) -> bool {
    let Some(rest) = cursor.rest.strip_prefix('#') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits != 0
        && let Some(rest) = rest.get(digits..).and_then(|rest| rest.strip_prefix('='))
    {
        cursor.rest = rest;
        true
    } else {
        false
    }
}

fn vector_prefix(cursor: &mut Cursor<'_>) -> bool {
    for prefix in ["#vu8", "#u8", "#"] {
        if let Some(rest) = cursor.rest.strip_prefix(prefix)
            && rest.starts_with('(')
        {
            cursor.rest = rest;
            return true;
        }
    }
    false
}

#[derive(Debug, PartialEq, Eq)]
enum CaptureSuffix {
    Other,
    Written,
    Ambiguous,
}

fn capture_suffix(token: &str) -> CaptureSuffix {
    let Some(name) = token.strip_prefix('@') else {
        return CaptureSuffix::Other;
    };
    let mut characters = name.chars();
    if characters.next().is_some_and(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
    }) && characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        CaptureSuffix::Written
    } else {
        CaptureSuffix::Ambiguous
    }
}

fn sampled_scheme(cursor: &mut Cursor<'_>) -> Option<()> {
    let mut closes = Vec::new();
    let mut pattern = false;
    while !cursor.rest.is_empty() {
        cursor.trivia()?;
        let Some(character) = cursor.rest.chars().next() else {
            break;
        };
        if cursor.rest.starts_with("#;") {
            cursor.advance(2)?;
            cursor.datum()?;
            pattern = false;
        } else if reader_prefix(cursor) || label_prefix(cursor) {
            cursor.datum()?;
            pattern = false;
        } else if vector_prefix(cursor) {
            cursor.compound()?;
            pattern = false;
        } else if cursor.rest.starts_with("#\\") {
            cursor.character()?;
            pattern = false;
        } else {
            match character {
                '(' | '[' => {
                    if let Some(after) = cursor.quoted_form() {
                        *cursor = after;
                    } else if cursor.incomplete {
                        return None;
                    } else {
                        closes.try_reserve(1).ok()?;
                        closes.push(if character == '(' { ')' } else { ']' });
                        cursor.advance(character.len_utf8())?;
                    }
                    pattern = false;
                }
                ')' | ']' => {
                    pattern = closes.pop().is_some_and(|close| close == character);
                    cursor.advance(character.len_utf8())?;
                }
                '"' => {
                    cursor.literal(character)?;
                    pattern = true;
                }
                '|' => {
                    cursor.literal(character)?;
                    pattern = false;
                }
                _ => {
                    let token = cursor.atom()?;
                    if pattern
                        && matches!(
                            capture_suffix(token),
                            CaptureSuffix::Written | CaptureSuffix::Ambiguous
                        )
                    {
                        return None;
                    }
                    pattern = token == "_" || pattern && matches!(token, "?" | "*" | "+");
                }
            }
        }
    }
    Some(())
}

/// Permits `.scm` extension evidence unless the bounded sample proves a capture
/// suffix or has complete-file reader ambiguity. This is not parse acceptance.
/// Sample-incomplete containers, literals and comments are not query evidence.
pub(super) fn extension_allowed(content: &[u8], prefix_truncated: bool) -> bool {
    let sample = content
        .get(..content.len().min(MAX_CLASSIFICATION_BYTES))
        .unwrap_or(content);
    let truncated = prefix_truncated || sample.len() < content.len();
    let source = match std::str::from_utf8(sample) {
        Ok(source) => source,
        Err(error) if truncated && error.error_len().is_none() => {
            let Some(source) = sample
                .get(..error.valid_up_to())
                .and_then(|sample| std::str::from_utf8(sample).ok())
            else {
                return false;
            };
            source
        }
        Err(_) => return false,
    };
    if source.contains('\0') {
        return false;
    }
    let mut cursor = Cursor {
        rest: source.strip_prefix('\u{feff}').unwrap_or(source),
        incomplete: false,
    };
    let allowed = sampled_scheme(&mut cursor).is_some();
    if cursor.incomplete {
        truncated
    } else {
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_scheme_and_embedded_at_signs_keep_extension_evidence() {
        for source in [
            "",
            "(define (visit value) value)",
            "[define value 1]",
            "(define send@domain 1)",
            "(define escaped \\x40;value)",
            "#!fold-case\n(define value 1)",
            "(define character #\\@)",
        ] {
            assert!(extension_allowed(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn complete_query_capture_suffixes_do_not_claim_scheme() {
        assert_eq!(capture_suffix("@name.part"), CaptureSuffix::Written);
        assert_eq!(capture_suffix("@"), CaptureSuffix::Ambiguous);
        assert_eq!(capture_suffix("send@domain"), CaptureSuffix::Other);
        for source in [
            "(written) @name",
            "[(written) (other)] @name.part",
            "((written) @name (#eq? @name \"x\"))",
            "(written)+ @name",
            "\"+\" @operator",
            "_ @name",
            "(written); comment\n@name",
            "(quote (written) @name)",
        ] {
            assert!(!extension_allowed(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn reader_comments_literals_and_quoted_data_mask_captures() {
        for source in [
            "; (written) @name\n(define value 1)",
            "#| outer #| (written) @name |# tail |# (define value 1)",
            "(display \"(written) @name\")",
            "(define |(written) @name| 1)",
            "'((written) @name)",
            "`((written) @name ,@(value))",
            "(quote ((written) @name))",
            "(quasiquote ((written) @name))",
            "#;((written) @name)\n(define value 1)",
            "#; #; (written) @name (define value 1)",
            "#((written) @name)",
            "#1=((written) @name)",
        ] {
            assert!(extension_allowed(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn complete_file_reader_ambiguity_is_distinct_from_sampling_truncation() {
        for source in [
            "\"(written) @name",
            "|(written) @name",
            "#| (written) @name",
            "'",
            "#;",
            "#1=",
            "(quote ((written) @name",
            "(define value \\x",
            "(define value \\x41",
        ] {
            assert!(!extension_allowed(source.as_bytes(), false), "{source}");
            assert!(extension_allowed(source.as_bytes(), true), "{source}");
        }
        assert!(!extension_allowed(b"(written) @", false));
        assert!(!extension_allowed(b"(written) @", true));
        assert!(!extension_allowed(b"(define value 1)\xff", false));
        assert!(!extension_allowed(b"(define value\0 1)", false));
    }

    #[test]
    fn bounded_samples_keep_open_executable_prefixes_without_inspecting_the_tail() {
        let open = format!(
            "(define (visit value){}",
            " ".repeat(MAX_CLASSIFICATION_BYTES)
        );
        assert!(extension_allowed(open.as_bytes(), true));
        let unseen = format!("{}(written) @name", " ".repeat(MAX_CLASSIFICATION_BYTES));
        assert!(extension_allowed(unseen.as_bytes(), true));
        for (prefix, tail) in [
            ("(display \"", "(written) @name\")"),
            ("#|", "(written) @name |# (define value 1)"),
            ("'(", "(written) @name)"),
            ("(quote (", "(written) @name))"),
        ] {
            let source = format!("{prefix}{}{tail}", "x".repeat(MAX_CLASSIFICATION_BYTES));
            assert!(extension_allowed(source.as_bytes(), false), "{prefix}");
        }
        assert!(!extension_allowed(b"(written) @name", true));
        assert!(!extension_allowed(b"(written) @name\n\"unterminated", true));
        assert!(extension_allowed(b"(define value 1)\xc3", true));
        assert!(!extension_allowed(b"(define value 1)\xc3", false));
    }
}
