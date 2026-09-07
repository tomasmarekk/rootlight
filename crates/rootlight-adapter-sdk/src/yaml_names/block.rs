//! Source-complete YAML block scalar decoding with bounded lexical lookahead.
//! Native captures can omit chomping-significant trailing lines; value decoding
//! therefore retains a separate lexical range, without rewriting parser evidence.

use std::ops::Range;

use super::{append, printable};

/// A decoded YAML block value and its complete, borrowed lexical source.
///
/// The lexical range includes the header and value-significant trailing lines,
/// but excludes a following dedented node, comment or document marker. It can
/// extend beyond the native parser capture. Preserve that original capture as
/// separate syntax evidence. This type does not resolve tags or document owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlBlockScalar<'a> {
    source: &'a str,
    range: Range<usize>,
    value: String,
}

impl<'a> YamlBlockScalar<'a> {
    /// Decodes one grammar-reviewed block capture against its complete source.
    ///
    /// `capture` starts at `|` or `>` and excludes tag/anchor properties.
    /// `parent_indentation` is the enclosing block collection's space indentation;
    /// `None` denotes a document-root scalar, where unindented content is legal.
    /// An explicit indentation digit is relative to the parent (zero at root).
    /// `source` must be the full immutable source, not a truncated capture slice.
    ///
    /// Invalid UTF-8 boundaries, reversed/empty captures, invalid headers or
    /// indentation, non-printable content, excess lexical/value bytes and
    /// allocation failure return `None`. Reads stop after `maximum_bytes` from
    /// the capture start, with bounded indentation/document-marker lookahead.
    /// No newline is invented at EOF. Syntax errors elsewhere in the document
    /// are the caller's responsibility, not proof of a valid scalar value.
    ///
    /// # Examples
    ///
    /// ```
    /// use rootlight_adapter_sdk::{YamlBlockScalar, YamlDocumentContext};
    /// # fn example() -> Option<()> {
    /// let source = "key: |+\n  text\n\nnext: done\n";
    /// let block = YamlBlockScalar::parse(source, 5..14, Some(0), 128)?;
    /// assert_eq!(block.value(), "text\n\n");
    /// assert_eq!(block.lexical_range(), 5..16);
    /// let document = YamlDocumentContext::new(None, &[], 128)?;
    /// assert!(!document.block_scalar(&block, None)?.has_unrecognized_tag());
    /// # Some(())
    /// # }
    /// # assert!(example().is_some());
    /// ```
    #[must_use]
    pub fn parse(
        source: &'a str,
        capture: Range<usize>,
        parent_indentation: Option<usize>,
        maximum_bytes: usize,
    ) -> Option<Self> {
        if capture.end.checked_sub(capture.start)? > maximum_bytes {
            return None;
        }
        let captured = source.get(capture.clone())?;
        if !captured.starts_with(['|', '>']) {
            return None;
        }
        let text = source.get(capture.start..)?;
        let header_line = line(text, 0, maximum_bytes)?;
        let header = Header::parse(header_line.text)?;
        let minimum = match parent_indentation {
            Some(parent) => parent.checked_add(1)?,
            None => 0,
        };
        let indentation = match header.indentation {
            Some(digit) => parent_indentation.unwrap_or(0).checked_add(digit)?,
            None => detect_indentation(text, header_line.end, minimum, maximum_bytes)?,
        };
        let mut position = header_line.end;
        let mut value = String::new();
        let mut previous_spaced = None;
        let mut breaks = 0usize;
        while position < text.len() {
            let (spaces, first) = prefix(text, position, maximum_bytes)?;
            if first.is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
                if spaces == 0 && document_marker(text.get(position..)?) {
                    break;
                }
                if spaces < indentation {
                    if first == Some(b'\t') {
                        let comment = text
                            .get(position.checked_add(spaces)?..)?
                            .bytes()
                            .take(maximum_bytes.saturating_add(1))
                            .find(|b| !matches!(b, b' ' | b'\t'));
                        if comment == Some(b'#') {
                            break;
                        }
                        return None;
                    }
                    if spaces < minimum || first == Some(b'#') {
                        break;
                    }
                    return None;
                }
            }
            let current = line(text, position, maximum_bytes)?;
            let body = current.text.get(indentation.min(spaces)..)?;
            if !body.chars().all(printable) {
                return None;
            }
            position = current.end;
            if body.is_empty() {
                breaks = breaks.checked_add(usize::from(current.has_break))?;
                continue;
            }
            let spaced = body.starts_with([' ', '\t']);
            if header.folded && previous_spaced == Some(false) && !spaced {
                if breaks == 1 {
                    append(&mut value, " ", maximum_bytes)?;
                } else {
                    append_breaks(&mut value, breaks.saturating_sub(1), maximum_bytes)?;
                }
            } else {
                append_breaks(&mut value, breaks, maximum_bytes)?;
            }
            append(&mut value, body, maximum_bytes)?;
            previous_spaced = Some(spaced);
            breaks = usize::from(current.has_break);
        }
        let final_breaks = match header.chomp {
            Chomp::Strip => 0,
            Chomp::Clip => usize::from(previous_spaced.is_some() && breaks > 0),
            Chomp::Keep => breaks,
        };
        append_breaks(&mut value, final_breaks, maximum_bytes)?;
        let end = capture.start.checked_add(position)?;
        if end < capture.end {
            return None;
        }
        Some(Self {
            source: source.get(capture.start..end)?,
            range: capture.start..end,
            value,
        })
    }

    /// Returns the exact full-source byte range used to decode this value.
    #[must_use]
    pub fn lexical_range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Returns the complete lexical source, including the original line endings.
    #[must_use]
    pub fn source_text(&self) -> &'a str {
        self.source
    }

    /// Returns decoded content with YAML line folding and chomping applied.
    ///
    /// This is not a type identity: an explicit tag can still change its type.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Copy)]
enum Chomp {
    Strip,
    Clip,
    Keep,
}

struct Header {
    folded: bool,
    indentation: Option<usize>,
    chomp: Chomp,
}

impl Header {
    fn parse(text: &str) -> Option<Self> {
        if !text.chars().all(printable) {
            return None;
        }
        let mut bytes = text.bytes();
        let folded = match bytes.next()? {
            b'|' => false,
            b'>' => true,
            _ => return None,
        };
        let mut indentation = None;
        let mut chomp = None;
        while let Some(byte) = bytes.next() {
            match byte {
                b'1'..=b'9' if indentation.is_none() => {
                    indentation = Some(usize::from(byte - b'0'))
                }
                b'-' | b'+' if chomp.is_none() => {
                    chomp = Some(if byte == b'-' {
                        Chomp::Strip
                    } else {
                        Chomp::Keep
                    })
                }
                b' ' | b'\t' => {
                    return bytes
                        .find(|b| !matches!(b, b' ' | b'\t'))
                        .is_none_or(|b| b == b'#')
                        .then_some(Self {
                            folded,
                            indentation,
                            chomp: chomp.unwrap_or(Chomp::Clip),
                        });
                }
                _ => return None,
            }
        }
        Some(Self {
            folded,
            indentation,
            chomp: chomp.unwrap_or(Chomp::Clip),
        })
    }
}

struct Line<'a> {
    text: &'a str,
    end: usize,
    has_break: bool,
}

fn line(text: &str, start: usize, maximum: usize) -> Option<Line<'_>> {
    let rest = text.get(start..)?;
    let budget = maximum.checked_sub(start)?;
    let length = rest
        .bytes()
        .take(budget)
        .position(|b| matches!(b, b'\r' | b'\n'));
    if let Some(length) = length {
        let content = rest.get(..length)?;
        let after = rest.get(length..)?;
        let break_bytes = if after.starts_with("\r\n") { 2 } else { 1 };
        let end = start.checked_add(length)?.checked_add(break_bytes)?;
        (end <= maximum).then_some(Line {
            text: content,
            end,
            has_break: true,
        })
    } else if rest.len() <= budget {
        Some(Line {
            text: rest,
            end: text.len(),
            has_break: false,
        })
    } else {
        None
    }
}

fn prefix(text: &str, start: usize, maximum: usize) -> Option<(usize, Option<u8>)> {
    let rest = text.get(start..)?;
    let available = maximum.checked_sub(start)?;
    let spaces = rest
        .bytes()
        .take(available.saturating_add(1))
        .take_while(|b| *b == b' ')
        .count();
    if spaces > available {
        return None;
    }
    Some((spaces, rest.as_bytes().get(spaces).copied()))
}

fn document_marker(text: &str) -> bool {
    (text.starts_with("---") || text.starts_with("..."))
        && text
            .as_bytes()
            .get(3)
            .is_none_or(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

fn detect_indentation(
    text: &str,
    mut position: usize,
    minimum: usize,
    maximum: usize,
) -> Option<usize> {
    let mut longest_empty = 0;
    while position < text.len() {
        let (spaces, first) = prefix(text, position, maximum)?;
        if first.is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
            if spaces < minimum
                || (first == Some(b'#') && spaces < longest_empty)
                || (spaces == 0 && document_marker(text.get(position..)?))
            {
                break;
            }
            return (spaces >= longest_empty).then_some(spaces);
        }
        longest_empty = longest_empty.max(spaces);
        position = line(text, position, maximum)?.end;
    }
    Some(longest_empty.max(minimum))
}

fn append_breaks(output: &mut String, count: usize, maximum: usize) -> Option<()> {
    for _ in 0..count {
        append(output, "\n", maximum)?;
    }
    Some(())
}
