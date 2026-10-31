//! Disambiguate shared .ru sources only from a complete leading SPARQL prologue.
//! Prefixless requests remain ambiguous with Ruby DSLs; parser acceptance is not ownership.

pub(super) fn has_content_hint(source: &[u8], truncated: bool) -> bool {
    let mut cursor = Cursor { source, offset: 0 };
    if source.starts_with(&[0xef, 0xbb, 0xbf]) {
        cursor.offset = 3;
    }
    let mut prologue = false;
    loop {
        cursor.trivia();
        if cursor.keyword(b"BASE") {
            cursor.trivia();
            if !cursor.iri() {
                return false;
            }
        } else if cursor.keyword(b"PREFIX") {
            cursor.trivia();
            if !cursor.prefix() {
                return false;
            }
            cursor.trivia();
            if !cursor.iri() {
                return false;
            }
        } else {
            break;
        }
        prologue = true;
    }
    if !prologue {
        return false;
    }
    cursor.trivia();
    if cursor.tail().is_empty() {
        // A complete prologue-only update is source evidence. A sampled prefix
        // cannot prove that the unobserved continuation is another request.
        return !truncated;
    }
    let tail = cursor.tail();
    let end = tail
        .iter()
        .take_while(|byte| byte.is_ascii_alphabetic())
        .count();
    let Some(next) = tail.get(end) else {
        return false;
    };
    (next.is_ascii_whitespace() || b"{(?$*<#".contains(next))
        && [
            b"SELECT".as_slice(),
            b"CONSTRUCT",
            b"DESCRIBE",
            b"ASK",
            b"LOAD",
            b"CLEAR",
            b"DROP",
            b"ADD",
            b"MOVE",
            b"COPY",
            b"CREATE",
            b"INSERT",
            b"DELETE",
            b"WITH",
        ]
        .iter()
        .any(|word| tail[..end].eq_ignore_ascii_case(word))
}

struct Cursor<'a> {
    source: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn tail(&self) -> &[u8] {
        &self.source[self.offset..]
    }

    fn trivia(&mut self) {
        loop {
            let count = self
                .tail()
                .iter()
                .take_while(|byte| byte.is_ascii_whitespace())
                .count();
            self.offset += count;
            if self.tail().first() != Some(&b'#') {
                return;
            }
            let count = self
                .tail()
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .unwrap_or(self.tail().len());
            self.offset += count;
        }
    }

    fn keyword(&mut self, word: &[u8]) -> bool {
        let tail = self.tail();
        if !tail
            .get(..word.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(word))
            || !tail
                .get(word.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || b":<#".contains(byte))
        {
            return false;
        }
        self.offset += word.len();
        true
    }

    fn prefix(&mut self) -> bool {
        let Some(colon) = self.tail().iter().position(|byte| *byte == b':') else {
            return false;
        };
        let Ok(name) = std::str::from_utf8(&self.tail()[..colon]) else {
            return false;
        };
        if !name.is_empty() {
            let mut chars = name.chars();
            if !chars.next().is_some_and(pn_base)
                || !chars.all(|ch| pn_base(ch) || ch.is_ascii_digit()
                    || matches!(ch, '_' | '-' | '.' | '\u{00b7}' | '\u{0300}'..='\u{036f}' | '\u{203f}'..='\u{2040}'))
                || name.ends_with('.') {
                return false;
            }
        }
        self.offset += colon + 1;
        true
    }

    fn iri(&mut self) -> bool {
        if self.tail().first() != Some(&b'<') {
            return false;
        }
        self.offset += 1;
        while let Some(byte) = self.tail().first().copied() {
            self.offset += 1;
            match byte {
                b'>' => return true,
                b'\\' => {
                    let digits = match self.tail().first() {
                        Some(b'u') => 4,
                        Some(b'U') => 8,
                        _ => return false,
                    };
                    self.offset += 1;
                    if !self
                        .tail()
                        .get(..digits)
                        .is_some_and(|bytes| bytes.iter().all(u8::is_ascii_hexdigit))
                    {
                        return false;
                    }
                    self.offset += digits;
                }
                0..=0x20 | b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' => return false,
                _ => {}
            }
        }
        false
    }
}

fn pn_base(ch: char) -> bool {
    ch.is_ascii_alphabetic()
        || matches!(ch,
        '\u{00c0}'..='\u{00d6}' | '\u{00d8}'..='\u{00f6}' | '\u{00f8}'..='\u{02ff}'
        | '\u{0370}'..='\u{037d}' | '\u{037f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}'
        | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}'
        | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}
