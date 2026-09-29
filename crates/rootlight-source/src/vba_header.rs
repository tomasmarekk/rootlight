//! Reads effective identity and class-accessibility attributes from native headers.
//! Names borrow decoded text and retain original module-byte coordinates; this
//! is not designer-preamble parsing or validation of the module's program body.

use crate::{vba_dir::ModuleKind, vba_text::DecodedText};
use rootlight_cancel::{Cancellation, Cancelled};
use std::{fmt, ops::Range};

#[cfg(test)]
mod tests;

/// Independent ceilings for the leading attribute block, not the full body.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct HeaderLimits {
    /// Maximum UTF-8 bytes in the header, at most the source snapshot ceiling.
    pub bytes: usize,
    /// Maximum attribute declarations, including implementation-specific ones.
    pub attributes: usize,
}

impl Default for HeaderLimits {
    fn default() -> Self {
        Self {
            bytes: 64 * 1024,
            attributes: 4096,
        }
    }
}

/// A missing or malformed source identity, never a guessed module name.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HeaderError {
    /// No VB_Name occurs in the leading attribute block.
    #[error("vba module header has no name")]
    Missing,
    /// A recognized attribute has malformed framing, name or boolean value.
    #[error("invalid vba module header at utf8 byte {offset}")]
    Invalid {
        /// Offset in the decoded UTF-8 view, not compressed/container bytes.
        offset: usize,
    },
    /// A procedural header declares VB_Name more than once.
    #[error("duplicate procedural module name")]
    Duplicate,
    /// Caller-supplied ceilings exceed hard limits.
    #[error("invalid vba header limits")]
    Limits,
    /// Header bytes or attribute count exceeded the caller's ceiling.
    #[error("vba module header limit exceeded")]
    Limit,
    /// The caller cancelled before a complete header was read.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// Effective source spelling with exact decoded and original name byte ranges.
pub struct HeaderName<'a> {
    name: &'a str,
    utf8_range: Range<usize>,
    original_range: Range<usize>,
    declarations: usize,
    kind: ModuleKind,
    predeclared_id: Option<HeaderBoolean>,
    exposed: Option<HeaderBoolean>,
}

/// One effective boolean attribute with exact decoded and original value ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderBoolean {
    value: bool,
    utf8_range: Range<usize>,
    original_range: Range<usize>,
}

impl HeaderBoolean {
    /// Returns the explicit boolean value, not an inferred class characteristic.
    #[must_use]
    pub const fn value(&self) -> bool {
        self.value
    }

    /// Returns the literal's range in the decoded UTF-8 view.
    #[must_use]
    pub fn utf8_range(&self) -> Range<usize> {
        self.utf8_range.clone()
    }

    /// Returns the literal's range in the original decompressed module bytes.
    #[must_use]
    pub fn original_range(&self) -> Range<usize> {
        self.original_range.clone()
    }
}

impl fmt::Debug for HeaderName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeaderName")
            .field("utf8_range", &self.utf8_range)
            .field("original_range", &self.original_range)
            .field("declarations", &self.declarations)
            .field("kind", &self.kind)
            .field("predeclared_id", &self.predeclared_id)
            .field("exposed", &self.exposed)
            .finish()
    }
}

impl<'a> HeaderName<'a> {
    /// Reads the contiguous leading native Attribute block and its effective name.
    ///
    /// Stops at the first non-attribute line; never searches comments, procedures,
    /// designer preambles or string payloads for a later name. Physical newlines
    /// and explicit continuations follow MS-VBAL 3.2. Attributes other than
    /// VB_Name, VB_PredeclaredId and VB_Exposed remain opaque. Class headers use the last
    /// value for each recognized attribute (MS-VBAL 4.2); procedural
    /// headers reject repeated names. EOF may terminate the final physical line.
    /// Names are bounded, unescaped non-whitespace spellings, not proof of every
    /// codepage-dependent identifier rule or locale-dependent case equivalence.
    ///
    /// # Errors
    /// Returns [`HeaderError`] for absent/malformed identity, duplicate procedural
    /// names, exhausted/invalid limits or cancellation. No partial name is returned.
    pub fn parse(
        text: &'a DecodedText<'_>,
        kind: ModuleKind,
        limits: HeaderLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, HeaderError> {
        cancellation.check()?;
        if u64::try_from(limits.bytes).map_err(|_| HeaderError::Limits)?
            > crate::HARD_MAX_SNAPSHOT_BYTES
            || limits.attributes > usize::from(u16::MAX)
        {
            return Err(HeaderError::Limits);
        }
        let input = text.text();
        let mut position = 0;
        let mut attributes = 0;
        let mut declarations = 0;
        let mut selected = None;
        let mut predeclared_id = None;
        let mut exposed = None;
        while position < input.len() {
            cancellation.check()?;
            let start = position;
            let mut token = start;
            let mut next_check = token;
            while let Some(c) = input
                .get(token..)
                .and_then(|s| s.chars().next())
                .filter(|c| wsc(*c))
            {
                if token >= limits.bytes {
                    return Err(HeaderError::Limit);
                }
                if token >= next_check {
                    cancellation.check()?;
                    next_check = token + 256;
                }
                token += c.len_utf8();
            }
            let Some(rest) = input.get(token..) else {
                break;
            };
            if !rest
                .get(..9)
                .is_some_and(|s| s.eq_ignore_ascii_case("Attribute"))
                || !rest
                    .get(9..)
                    .and_then(|s| s.chars().next())
                    .is_some_and(wsc)
            {
                break;
            }
            if attributes >= limits.attributes {
                return Err(HeaderError::Limit);
            }
            attributes += 1;
            let (end, next) = logical_line(input, start, limits.bytes, cancellation)?;
            let line = input
                .get(token + 9..end)
                .ok_or(HeaderError::Invalid { offset: token })?;
            match attribute(line, token + 9, cancellation)? {
                Attribute::Name(name, range) => {
                    if declarations > 0 && kind == ModuleKind::Procedural {
                        return Err(HeaderError::Duplicate);
                    }
                    declarations += 1;
                    selected = Some((name, range));
                }
                Attribute::Predeclared(value, utf8_range) => {
                    if kind == ModuleKind::Procedural {
                        return Err(HeaderError::Invalid { offset: token });
                    }
                    let original_range =
                        text.original_range(utf8_range.clone())
                            .ok_or(HeaderError::Invalid {
                                offset: utf8_range.start,
                            })?;
                    predeclared_id = Some(HeaderBoolean {
                        value,
                        utf8_range,
                        original_range,
                    });
                }
                Attribute::Exposed(value, utf8_range) => {
                    if kind == ModuleKind::Object {
                        let original_range = text.original_range(utf8_range.clone()).ok_or(
                            HeaderError::Invalid {
                                offset: utf8_range.start,
                            },
                        )?;
                        exposed = Some(HeaderBoolean {
                            value,
                            utf8_range,
                            original_range,
                        });
                    }
                }
                Attribute::Other => {}
            }
            position = next;
        }
        cancellation.check()?;
        let (name, utf8_range) = selected.ok_or(HeaderError::Missing)?;
        let original_range =
            text.original_range(utf8_range.clone())
                .ok_or(HeaderError::Invalid {
                    offset: utf8_range.start,
                })?;
        Ok(Self {
            name,
            utf8_range,
            original_range,
            declarations,
            kind,
            predeclared_id,
            exposed,
        })
    }

    /// Returns the effective unnormalized spelling without surrounding quotes.
    #[must_use]
    pub const fn name(&self) -> &'a str {
        self.name
    }
    /// Returns the name range in the decoded UTF-8 view.
    #[must_use]
    pub fn utf8_range(&self) -> Range<usize> {
        self.utf8_range.clone()
    }
    /// Returns the exact name range in the original decompressed module bytes.
    #[must_use]
    pub fn original_range(&self) -> Range<usize> {
        self.original_range.clone()
    }
    /// Returns how many VB_Name declarations participated in header selection.
    #[must_use]
    pub const fn declarations(&self) -> usize {
        self.declarations
    }
    /// Returns the procedural/object mode used to interpret repeated attributes.
    #[must_use]
    pub const fn kind(&self) -> ModuleKind {
        self.kind
    }

    /// Returns the last explicit VB_PredeclaredId value and its exact source ranges.
    /// Absence means the False default, not evidence for a named instance.
    #[must_use]
    pub const fn predeclared_id(&self) -> Option<&HeaderBoolean> {
        self.predeclared_id.as_ref()
    }

    /// Returns the last explicit class VB_Exposed literal and its exact ranges.
    /// Absence has the False default; procedural modules do not gain class accessibility.
    #[must_use]
    pub const fn exposed(&self) -> Option<&HeaderBoolean> {
        self.exposed.as_ref()
    }
}

fn wsc(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{19}' | ' ' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

fn newline(text: &str) -> Option<usize> {
    if text.starts_with("\r\n") {
        Some(2)
    } else {
        text.chars()
            .next()
            .filter(|c| matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
            .map(char::len_utf8)
    }
}

fn logical_line(
    text: &str,
    start: usize,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(usize, usize), HeaderError> {
    let mut at = start;
    let mut next_check = at;
    while at < text.len() {
        if at >= maximum {
            return Err(HeaderError::Limit);
        }
        if at >= next_check {
            cancellation.check()?;
            next_check = at + 256;
        }
        let rest = text.get(at..).ok_or(HeaderError::Invalid { offset: at })?;
        if let Some(size) = newline(rest) {
            if at + size > maximum {
                return Err(HeaderError::Limit);
            }
            let prefix = text
                .get(start..at)
                .ok_or(HeaderError::Invalid { offset: at })?;
            let continued = prefix
                .strip_suffix('_')
                .and_then(|s| s.chars().next_back())
                .is_some_and(wsc);
            if !continued {
                return Ok((at, at + size));
            }
            at += size;
        } else {
            let c = rest
                .chars()
                .next()
                .ok_or(HeaderError::Invalid { offset: at })?;
            at += c.len_utf8();
            if at > maximum {
                return Err(HeaderError::Limit);
            }
        }
    }
    Ok((at, at))
}

enum Attribute<'a> {
    Name(&'a str, Range<usize>),
    Predeclared(bool, Range<usize>),
    Exposed(bool, Range<usize>),
    Other,
}

fn attribute<'a>(
    line: &'a str,
    offset: usize,
    cancellation: &Cancellation,
) -> Result<Attribute<'a>, HeaderError> {
    let invalid = || HeaderError::Invalid { offset };
    let mut cursor = Cursor {
        text: line,
        position: 0,
        next_check: 0,
        cancellation,
    };
    cursor.whitespace()?;
    let key_start = cursor.position;
    while let Some(c) = cursor.peek().filter(|c| !wsc(*c) && *c != '=') {
        cursor.advance(c.len_utf8())?;
    }
    let key = line.get(key_start..cursor.position).ok_or_else(invalid)?;
    cursor.whitespace()?;
    if key.is_empty() || cursor.peek() != Some('=') {
        return Err(invalid());
    }
    cursor.advance(1)?;
    cursor.whitespace()?;
    if cursor.position == line.len() {
        return Err(invalid());
    }
    if key.eq_ignore_ascii_case("VB_PredeclaredId") || key.eq_ignore_ascii_case("VB_Exposed") {
        let begin = cursor.position;
        while let Some(c) = cursor.peek().filter(|c| c.is_ascii_alphabetic()) {
            cursor.advance(c.len_utf8())?;
        }
        let finish = cursor.position;
        let literal = line.get(begin..finish).ok_or_else(invalid)?;
        let value = if literal.eq_ignore_ascii_case("True") {
            true
        } else if literal.eq_ignore_ascii_case("False") {
            false
        } else {
            return Err(invalid());
        };
        cursor.whitespace()?;
        if cursor.position != line.len() {
            return Err(invalid());
        }
        let range = offset + begin..offset + finish;
        return Ok(if key.eq_ignore_ascii_case("VB_PredeclaredId") {
            Attribute::Predeclared(value, range)
        } else {
            Attribute::Exposed(value, range)
        });
    }
    if !key.eq_ignore_ascii_case("VB_Name") {
        return Ok(Attribute::Other);
    }
    if cursor.peek() != Some('"') {
        return Err(invalid());
    }
    cursor.advance(1)?;
    let begin = cursor.position;
    let mut count = 0;
    while let Some(c) = cursor.peek().filter(|c| *c != '"') {
        if count == 31 || c.is_control() || c.is_whitespace() {
            return Err(invalid());
        }
        count += 1;
        cursor.advance(c.len_utf8())?;
    }
    let finish = cursor.position;
    let name = line.get(begin..finish).ok_or_else(invalid)?;
    if cursor.peek() != Some('"') || name.is_empty() {
        return Err(invalid());
    }
    cursor.advance(1)?;
    cursor.whitespace()?;
    if cursor.position != line.len() {
        return Err(invalid());
    }
    Ok(Attribute::Name(name, offset + begin..offset + finish))
}

struct Cursor<'a> {
    text: &'a str,
    position: usize,
    next_check: usize,
    cancellation: &'a Cancellation,
}
impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.text.get(self.position..)?.chars().next()
    }
    fn advance(&mut self, size: usize) -> Result<(), HeaderError> {
        self.position += size;
        if self.position >= self.next_check {
            self.cancellation.check()?;
            self.next_check = self.position + 256;
        }
        Ok(())
    }
    fn whitespace(&mut self) -> Result<(), HeaderError> {
        loop {
            let before = self.position;
            while let Some(c) = self.peek().filter(|c| wsc(*c)) {
                self.advance(c.len_utf8())?;
            }
            if self.position == before || self.peek() != Some('_') {
                break;
            }
            let Some(size) = self.text.get(self.position + 1..).and_then(newline) else {
                break;
            };
            self.advance(1 + size)?;
        }
        Ok(())
    }
}
