//! Bounded physical header evidence for the Apex/classic BASIC shared suffix.
//! A complete braced type header is a routing hint, not compiler acceptance.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    Symbol(char),
    Literal,
}

#[derive(Clone, Copy)]
struct Cursor<'a> {
    rest: &'a str,
    line_boundary: bool,
}

impl<'a> Cursor<'a> {
    fn next(&mut self) -> Result<Option<Token<'a>>, ()> {
        loop {
            let trimmed = self.rest.trim_start_matches(char::is_whitespace);
            self.line_boundary |=
                self.rest[..self.rest.len() - trimmed.len()].contains(['\r', '\n']);
            self.rest = trimmed;
            if let Some(comment) = self.rest.strip_prefix("//") {
                self.rest = comment.find(['\r', '\n']).map_or("", |at| &comment[at..]);
            } else if let Some(comment) = self.rest.strip_prefix("/*") {
                let end = comment.find("*/").ok_or(())?;
                self.line_boundary |= comment[..end].contains(['\r', '\n']);
                self.rest = &comment[end + 2..];
            } else {
                break;
            }
        }
        let Some(first) = self.rest.chars().next() else {
            return Ok(None);
        };
        if matches!(first, '\'' | '"') {
            let mut escaped = false;
            for (at, character) in self.rest[first.len_utf8()..].char_indices() {
                if matches!(character, '\r' | '\n') {
                    return Err(());
                }
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == first {
                    self.rest = &self.rest[at + 2 * first.len_utf8()..];
                    return Ok(Some(Token::Literal));
                }
            }
            return Err(());
        }
        if first.is_alphabetic() || matches!(first, '_' | '$') {
            let end = self
                .rest
                .char_indices()
                .skip(1)
                .find_map(|(at, character)| {
                    (!(character.is_alphanumeric() || matches!(character, '_' | '$'))).then_some(at)
                })
                .unwrap_or(self.rest.len());
            let word = &self.rest[..end];
            self.rest = &self.rest[end..];
            Ok(Some(Token::Word(word)))
        } else {
            self.rest = &self.rest[first.len_utf8()..];
            Ok(Some(Token::Symbol(first)))
        }
    }
}

fn word(cursor: &mut Cursor<'_>) -> Result<(), ()> {
    matches!(cursor.next()?, Some(Token::Word(_)))
        .then_some(())
        .ok_or(())
}

fn balanced(cursor: &mut Cursor<'_>, open: char, close: char) -> Result<(), ()> {
    let mut depth = 1_usize;
    while let Some(token) = cursor.next()? {
        match token {
            Token::Symbol(symbol) if symbol == open => depth += 1,
            Token::Symbol(symbol) if symbol == close => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            // A body or terminator cannot complete an annotation/type argument.
            Token::Symbol('{' | '}' | ';') => return Err(()),
            _ => {}
        }
    }
    Err(())
}

fn type_name(cursor: &mut Cursor<'_>) -> Result<(), ()> {
    word(cursor)?;
    loop {
        let mut lookahead = *cursor;
        match lookahead.next()? {
            Some(Token::Symbol('.')) => {
                word(&mut lookahead)?;
                *cursor = lookahead;
            }
            Some(Token::Symbol('<')) => {
                balanced(&mut lookahead, '<', '>')?;
                *cursor = lookahead;
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
}

fn header(mut cursor: Cursor<'_>) -> Result<(), ()> {
    loop {
        match cursor.next()? {
            Some(Token::Symbol('@')) => {
                word(&mut cursor)?;
                let mut lookahead = cursor;
                if lookahead.next()? == Some(Token::Symbol('(')) {
                    balanced(&mut lookahead, '(', ')')?;
                    cursor = lookahead;
                }
            }
            Some(Token::Word(keyword))
                if ["class", "interface", "enum"]
                    .iter()
                    .any(|candidate| keyword.eq_ignore_ascii_case(candidate)) =>
            {
                break;
            }
            Some(Token::Word(modifier))
                if [
                    "public",
                    "private",
                    "protected",
                    "global",
                    "virtual",
                    "abstract",
                    "static",
                    "final",
                    "with",
                    "without",
                    "inherited",
                    "sharing",
                ]
                .iter()
                .any(|candidate| modifier.eq_ignore_ascii_case(candidate)) => {}
            _ => return Err(()),
        }
    }
    word(&mut cursor)?;
    let mut next = cursor.next()?;
    if next == Some(Token::Symbol('<')) {
        balanced(&mut cursor, '<', '>')?;
        next = cursor.next()?;
    }
    if matches!(next, Some(Token::Word(keyword)) if keyword.eq_ignore_ascii_case("extends")) {
        type_name(&mut cursor)?;
        next = cursor.next()?;
    }
    if matches!(next, Some(Token::Word(keyword)) if keyword.eq_ignore_ascii_case("implements")) {
        loop {
            type_name(&mut cursor)?;
            next = cursor.next()?;
            if next != Some(Token::Symbol(',')) {
                break;
            }
        }
    }
    (next == Some(Token::Symbol('{'))).then_some(()).ok_or(())
}

pub(super) fn has_content_hint(source: &[u8], prefix_truncated: bool) -> bool {
    let source = match std::str::from_utf8(source) {
        Ok(source) => source,
        // A split scalar after a complete header must not erase prior evidence.
        // Only an incomplete terminal scalar in a genuinely sampled prefix is safe.
        Err(error) if prefix_truncated && error.error_len().is_none() => {
            let Some(source) = source
                .get(..error.valid_up_to())
                .and_then(|prefix| std::str::from_utf8(prefix).ok())
            else {
                return false;
            };
            source
        }
        Err(_) => return false,
    };
    let mut cursor = Cursor {
        rest: source.strip_prefix('\u{feff}').unwrap_or(source),
        line_boundary: true,
    };
    let mut depth = 0_usize;
    let mut basic_statement_boundary = true;
    loop {
        let before = cursor;
        let Ok(Some(token)) = cursor.next() else {
            return false;
        };
        // BASIC Rem text shares this suffix; its semicolons are not Apex code.
        // Keep this outside header parsing so a written type named Rem stays intact.
        if depth == 0
            && (cursor.line_boundary || basic_statement_boundary)
            && matches!(token, Token::Word(word) if word.eq_ignore_ascii_case("rem"))
            && cursor.rest.chars().next().is_none_or(char::is_whitespace)
        {
            cursor.rest = cursor
                .rest
                .find(['\r', '\n'])
                .map_or("", |at| &cursor.rest[at..]);
            cursor.line_boundary = true;
            basic_statement_boundary = true;
            continue;
        }
        if depth == 0 && cursor.line_boundary && header(before).is_ok() {
            return true;
        }
        cursor.line_boundary = false;
        basic_statement_boundary = token == Token::Symbol(':');
        match token {
            Token::Symbol('{') => depth += 1,
            Token::Symbol('}') => {
                depth = depth.saturating_sub(1);
                cursor.line_boundary = depth == 0;
            }
            Token::Symbol(';') if depth == 0 => cursor.line_boundary = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::has_content_hint;

    #[test]
    fn complete_written_type_headers_supply_evidence() {
        for source in [
            "class Plain {}",
            "public virtual class Box extends Parent implements One, pkg.Two {}",
            "@IsTest\r\nprivate inherited sharing class CaseBox {}",
            "@Flag(value='class NotAType {}') public interface Reader {}",
            "/* header */ GLOBAL enum Mode { Ready, Waiting }",
            "\u{feff}// header\r\npublic class Métrique {}",
            "public class Box extends Map<String, List<Integer>> {}",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn comments_literals_expressions_and_incomplete_headers_are_not_evidence() {
        for source in [
            "// public class Phantom {}",
            "/* public class Phantom {} */",
            "value = 'public class Phantom {}';",
            "value = \"public class Phantom {}\";",
            "Rem public class Phantom {}",
            "' public class Phantom {}",
            "public class",
            "public class Box",
            "public class Box extends",
            "public class Box extends Parent implements",
            "public class Box extends Map<String, Integer {",
            "public class Box unexpected words {}",
            "@Flag(value='unterminated) public class Box {}",
            "/* unterminated public class Box {}",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn later_top_level_evidence_survives_inert_basic_header_text() {
        let source = "Attribute VB_Name = \"BasicBox\"\r\npublic class ApexBox {}";
        assert!(has_content_hint(source.as_bytes(), false));
        // Discovery must retain this conflict instead of choosing this hint.
    }

    #[test]
    fn truncated_terminal_scalars_preserve_only_complete_prior_headers() {
        let mut source = b"public class Written {}\n// ".to_vec();
        source.extend_from_slice("é".as_bytes());
        let sampled = &source[..source.len() - 1];
        assert!(std::str::from_utf8(&source).is_ok());
        assert!(has_content_hint(sampled, true));
        assert!(!has_content_hint(sampled, false));
        for source in [
            b"public class Incomplete\xc3".as_slice(),
            b"public class Written {}\n// invalid \xff tail",
            b"\xff public class Written {}\xc3",
        ] {
            assert!(!has_content_hint(source, true));
        }
    }

    #[test]
    fn basic_rem_physical_and_statement_comments_do_not_supply_type_headers() {
        for comment in [
            "Rem documentation; class Phantom {}",
            "\tReM documentation; public class Phantom {}",
            "value = 1: Rem documentation; interface Phantom {}",
            "Label: rem documentation; enum Phantom {}",
        ] {
            let source = format!("Attribute VB_Name = \"ClassicBox\"\r\n{comment}\r\n");
            assert!(!has_content_hint(source.as_bytes(), false), "{source}");
        }
        for source in [
            "class Rem {}",
            "@Rem public class Written {}",
            "public class Written extends Rem {}",
            "RemValue = 1; public class Written {}",
            "Rem documentation\r\npublic class Written {}",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source}");
        }
    }
}
