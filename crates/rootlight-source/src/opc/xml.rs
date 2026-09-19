//! Non-recursive XML reader for the two flat OPC metadata schemas.
//! Original offsets survive UTF-16 decoding and attribute normalization;
//! unsupported namespaces fail explicitly rather than being stripped implicitly.

use super::{MetadataError, MetadataLimits, MetadataValue};
use rootlight_cancel::Cancellation;
use std::{borrow::Cow, ops::Range};

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

pub(super) struct Element {
    pub name: String,
    pub range: Range<usize>,
    attributes: Vec<(String, MetadataValue)>,
}

impl Element {
    pub(super) fn take(&mut self, name: &str) -> Result<MetadataValue, MetadataError> {
        self.optional(name).ok_or(MetadataError::Invalid {
            offset: self.range.start,
        })
    }
    pub(super) fn optional(&mut self, name: &str) -> Option<MetadataValue> {
        let index = self.attributes.iter().position(|(key, _)| key == name)?;
        Some(self.attributes.remove(index).1)
    }
    pub(super) fn finish(&self) -> Result<(), MetadataError> {
        if self.attributes.is_empty() {
            Ok(())
        } else {
            Err(MetadataError::Invalid {
                offset: self.range.start,
            })
        }
    }
}

struct Text<'a> {
    value: Cow<'a, str>,
    offsets: Vec<(usize, usize)>,
    bom: usize,
    utf16: bool,
}

impl<'a> Text<'a> {
    fn decode(input: &'a [u8], cancel: &Cancellation) -> Result<Self, MetadataError> {
        let utf16 = input.starts_with(&[0xff, 0xfe]) || input.starts_with(&[0xfe, 0xff]);
        if !utf16 {
            let bom = usize::from(input.starts_with(&[0xef, 0xbb, 0xbf])) * 3;
            let value = std::str::from_utf8(input.get(bom..).ok_or(MetadataError::Encoding)?)
                .map_err(|_| MetadataError::Encoding)?;
            return Ok(Self {
                value: Cow::Borrowed(value),
                offsets: Vec::new(),
                bom,
                utf16,
            });
        }
        let little = input.starts_with(&[0xff, 0xfe]);
        let bytes = input.get(2..).ok_or(MetadataError::Encoding)?;
        let (pairs, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(MetadataError::Encoding);
        }
        let units = pairs.iter().map(|&pair| {
            if little {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        let mut value = String::new();
        let mut offsets = Vec::new();
        let mut original = 2;
        for (index, character) in char::decode_utf16(units).enumerate() {
            if index % 4096 == 0 {
                cancel.check()?;
            }
            let c = character.map_err(|_| MetadataError::Encoding)?;
            offsets.push((value.len(), original));
            value.push(c);
            original += c.len_utf16() * 2;
        }
        offsets.push((value.len(), original));
        Ok(Self {
            value: Cow::Owned(value),
            offsets,
            bom: 2,
            utf16,
        })
    }
    fn original(&self, offset: usize) -> usize {
        if !self.utf16 {
            return self.bom + offset;
        }
        self.offsets
            .binary_search_by_key(&offset, |&(utf8, _)| utf8)
            .ok()
            .and_then(|index| self.offsets.get(index))
            .map_or(self.bom, |&(_, raw)| raw)
    }
}

struct Tag {
    name: String,
    attrs: Vec<(String, MetadataValue)>,
    start: usize,
    empty: bool,
}

struct Parser<'a> {
    text: Text<'a>,
    position: usize,
    limits: MetadataLimits,
    cancel: &'a Cancellation,
}

pub(super) fn parse(
    input: &[u8],
    limits: MetadataLimits,
    namespace: &str,
    root: &str,
    cancel: &Cancellation,
) -> Result<Vec<Element>, MetadataError> {
    cancel.check()?;
    if limits.bytes > 1024 * 1024
        || limits.records > 65_536
        || limits.attributes > 128
        || limits.value_bytes > 65_536
    {
        return Err(MetadataError::Limits);
    }
    if input.len() > limits.bytes {
        return Err(MetadataError::Limit {
            resource: "xml bytes",
        });
    }
    let text = Text::decode(input, cancel)?;
    for (index, (offset, c)) in text.value.char_indices().enumerate() {
        if index % 4096 == 0 {
            cancel.check()?;
        }
        if !xml_char(c) {
            return Err(MetadataError::Invalid {
                offset: text.original(offset),
            });
        }
    }
    let mut parser = Parser {
        text,
        position: 0,
        limits,
        cancel,
    };
    parser.declaration()?;
    parser.misc()?;
    let outer = parser.tag()?;
    parser.check_tag(&outer, &[], namespace)?;
    if local_name(&outer.name) != root || outer.attrs.iter().any(|(name, _)| !is_namespace(name)) {
        return Err(parser.invalid());
    }
    let mut result = Vec::new();
    if !outer.empty {
        loop {
            parser.content_space()?;
            if parser.remaining().starts_with("</") {
                parser.close(&outer.name)?;
                break;
            }
            if result.len() >= limits.records {
                return Err(MetadataError::Limit {
                    resource: "xml records",
                });
            }
            let child = parser.tag()?;
            parser.check_tag(&child, &outer.attrs, namespace)?;
            let name = local_name(&child.name).to_owned();
            if !child.empty {
                parser.leaf(&child.name, root == "Relationships")?;
            }
            result.push(Element {
                name,
                range: child.start..parser.text.original(parser.position),
                attributes: child
                    .attrs
                    .into_iter()
                    .filter(|(name, _)| !is_namespace(name))
                    .collect(),
            });
        }
    }
    parser.misc()?;
    if parser.position != parser.text.value.len() {
        return Err(parser.invalid());
    }
    cancel.check()?;
    Ok(result)
}

impl Parser<'_> {
    fn remaining(&self) -> &str {
        self.text.value.get(self.position..).unwrap_or("")
    }
    fn invalid(&self) -> MetadataError {
        MetadataError::Invalid {
            offset: self.text.original(self.position),
        }
    }
    fn eat(&mut self, value: &str) -> bool {
        if self.remaining().starts_with(value) {
            self.position += value.len();
            true
        } else {
            false
        }
    }
    fn required(&mut self, value: &str) -> Result<(), MetadataError> {
        if self.eat(value) {
            Ok(())
        } else {
            Err(self.invalid())
        }
    }
    fn whitespace(&mut self) -> bool {
        let start = self.position;
        while self.remaining().chars().next().is_some_and(space) {
            self.position += 1;
        }
        self.position != start
    }
    fn name(&mut self) -> Result<String, MetadataError> {
        let start = self.position;
        let mut chars = self.remaining().chars();
        let first = chars.next().ok_or_else(|| self.invalid())?;
        if !name_start(first) && first != ':' {
            return Err(self.invalid());
        }
        let mut bytes = first.len_utf8();
        for c in chars {
            if !name_char(c) && c != ':' {
                break;
            }
            bytes += c.len_utf8();
            if bytes > self.limits.value_bytes {
                return Err(MetadataError::Limit {
                    resource: "xml name bytes",
                });
            }
        }
        if bytes > self.limits.value_bytes {
            return Err(MetadataError::Limit {
                resource: "xml name bytes",
            });
        }
        self.position += bytes;
        Ok(self
            .text
            .value
            .get(start..self.position)
            .ok_or_else(|| self.invalid())?
            .to_owned())
    }
    fn value(&mut self, references: bool) -> Result<MetadataValue, MetadataError> {
        let quote = self
            .remaining()
            .chars()
            .next()
            .ok_or_else(|| self.invalid())?;
        if !matches!(quote, '\'' | '"') {
            return Err(self.invalid());
        }
        self.position += 1;
        let start = self.position;
        let mut value = String::new();
        loop {
            self.cancel.check()?;
            let c = self
                .remaining()
                .chars()
                .next()
                .ok_or_else(|| self.invalid())?;
            if c == quote {
                let end = self.position;
                self.position += 1;
                return Ok(MetadataValue {
                    value,
                    range: self.text.original(start)..self.text.original(end),
                });
            }
            if c == '<' {
                return Err(self.invalid());
            }
            if c == '&' {
                if !references {
                    return Err(self.invalid());
                }
                value.push(self.reference()?);
            } else {
                self.position += c.len_utf8();
                if c == '\r' {
                    self.eat("\n");
                }
                value.push(if space(c) { ' ' } else { c });
            }
            if value.len() > self.limits.value_bytes {
                return Err(MetadataError::Limit {
                    resource: "xml value bytes",
                });
            }
        }
    }
    fn reference(&mut self) -> Result<char, MetadataError> {
        self.required("&")?;
        let length = self
            .remaining()
            .find(';')
            .filter(|n| *n <= 16)
            .ok_or_else(|| self.invalid())?;
        let name = self
            .remaining()
            .get(..length)
            .ok_or_else(|| self.invalid())?;
        let c = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "apos" => '\'',
            "quot" => '"',
            _ => {
                let (digits, radix) = if let Some(digits) = name.strip_prefix("#x") {
                    (digits, 16)
                } else if let Some(digits) = name.strip_prefix('#') {
                    (digits, 10)
                } else {
                    return Err(self.invalid());
                };
                if digits.is_empty()
                    || !digits.chars().all(|c| {
                        if radix == 16 {
                            c.is_ascii_hexdigit()
                        } else {
                            c.is_ascii_digit()
                        }
                    })
                {
                    return Err(self.invalid());
                }
                u32::from_str_radix(digits, radix)
                    .ok()
                    .and_then(char::from_u32)
                    .filter(|c| xml_char(*c))
                    .ok_or_else(|| self.invalid())?
            }
        };
        self.position += length + 1;
        Ok(c)
    }
    fn declaration(&mut self) -> Result<(), MetadataError> {
        if !self.remaining().starts_with("<?xml")
            || !self
                .remaining()
                .get(5..)
                .and_then(|s| s.chars().next())
                .is_some_and(space)
        {
            return Ok(());
        }
        self.position += 5;
        let mut attrs = Vec::new();
        loop {
            let separated = self.whitespace();
            if self.eat("?>") {
                break;
            }
            if !separated || attrs.len() == 3 {
                return Err(self.invalid());
            }
            let key = self.name()?;
            self.whitespace();
            self.required("=")?;
            self.whitespace();
            attrs.push((key, self.value(false)?));
        }
        let mut iter = attrs.iter();
        if !iter
            .next()
            .is_some_and(|(key, value)| key == "version" && value.value() == "1.0")
        {
            return Err(self.invalid());
        }
        let mut next = iter.next();
        if let Some((key, value)) = next
            && key == "encoding"
        {
            let expected = if self.text.utf16 { "UTF-16" } else { "UTF-8" };
            if !value.value().eq_ignore_ascii_case(expected) {
                return Err(MetadataError::Encoding);
            }
            next = iter.next();
        }
        if let Some((key, value)) = next
            && (key != "standalone" || !matches!(value.value(), "yes" | "no"))
        {
            return Err(self.invalid());
        }
        if iter.next().is_some() {
            return Err(self.invalid());
        }
        Ok(())
    }
    fn misc(&mut self) -> Result<(), MetadataError> {
        loop {
            self.cancel.check()?;
            self.whitespace();
            if self.remaining().starts_with("<!--") {
                self.comment()?;
            } else if self.remaining().starts_with("<?") {
                self.instruction()?;
            } else if self.remaining().starts_with("<!DOCTYPE") {
                return Err(MetadataError::Unsupported { feature: "dtd" });
            } else {
                return Ok(());
            }
        }
    }
    fn comment(&mut self) -> Result<(), MetadataError> {
        self.required("<!--")?;
        let length = self.remaining().find("--").ok_or_else(|| self.invalid())?;
        self.position += length;
        self.required("-->")
    }

    fn content_space(&mut self) -> Result<(), MetadataError> {
        loop {
            self.misc()?;
            if self.remaining().starts_with('&') {
                if !space(self.reference()?) {
                    return Err(self.invalid());
                }
            } else if self.eat("<![CDATA[") {
                let length = self.remaining().find("]]>").ok_or_else(|| self.invalid())?;
                if self
                    .remaining()
                    .get(..length)
                    .is_none_or(|value| !value.chars().all(space))
                {
                    return Err(self.invalid());
                }
                self.position += length + 3;
            } else {
                return Ok(());
            }
        }
    }
    fn instruction(&mut self) -> Result<(), MetadataError> {
        self.required("<?")?;
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            return Err(self.invalid());
        }
        if self.eat("?>") {
            return Ok(());
        }
        if !self.whitespace() {
            return Err(self.invalid());
        }
        let length = self.remaining().find("?>").ok_or_else(|| self.invalid())?;
        self.position += length + 2;
        Ok(())
    }
    fn tag(&mut self) -> Result<Tag, MetadataError> {
        self.cancel.check()?;
        let start = self.text.original(self.position);
        self.required("<")?;
        let name = self.name()?;
        if !qname(&name) {
            return Err(self.invalid());
        }
        let mut attrs: Vec<(String, MetadataValue)> = Vec::new();
        loop {
            let separated = self.whitespace();
            let empty = self.eat("/>");
            if empty || self.eat(">") {
                return Ok(Tag {
                    name,
                    attrs,
                    start,
                    empty,
                });
            }
            if !separated {
                return Err(self.invalid());
            }
            if attrs.len() >= self.limits.attributes {
                return Err(MetadataError::Limit {
                    resource: "xml attributes",
                });
            }
            let key = self.name()?;
            if !qname(&key) || attrs.iter().any(|(name, _)| name == &key) {
                return Err(self.invalid());
            }
            self.whitespace();
            self.required("=")?;
            self.whitespace();
            let value = self.value(true)?;
            attrs.push((key, value));
        }
    }
    fn close(&mut self, name: &str) -> Result<(), MetadataError> {
        self.required("</")?;
        if self.name()? != name {
            return Err(self.invalid());
        }
        self.whitespace();
        self.required(">")
    }
    fn leaf(&mut self, name: &str, string_content: bool) -> Result<(), MetadataError> {
        loop {
            self.cancel.check()?;
            if self.remaining().starts_with("</") {
                return self.close(name);
            }
            if self.remaining().starts_with("<!--") {
                self.comment()?;
            } else if self.remaining().starts_with("<?") {
                self.instruction()?;
            } else if self.eat("<![CDATA[") {
                let length = self.remaining().find("]]>").ok_or_else(|| self.invalid())?;
                if !string_content
                    && self
                        .remaining()
                        .get(..length)
                        .is_none_or(|s| !s.chars().all(space))
                {
                    return Err(self.invalid());
                }
                self.position += length + 3;
            } else if self.remaining().starts_with('&') {
                let c = self.reference()?;
                if !string_content && !space(c) {
                    return Err(self.invalid());
                }
            } else {
                let c = self
                    .remaining()
                    .chars()
                    .next()
                    .ok_or_else(|| self.invalid())?;
                if c == '<' || self.remaining().starts_with("]]>") || (!string_content && !space(c))
                {
                    return Err(self.invalid());
                }
                self.position += c.len_utf8();
            }
        }
    }
    fn check_tag(
        &self,
        tag: &Tag,
        inherited: &[(String, MetadataValue)],
        expected: &str,
    ) -> Result<(), MetadataError> {
        for (name, value) in &tag.attrs {
            if is_namespace(name) {
                let prefix = name.strip_prefix("xmlns:").unwrap_or("");
                if prefix == "xmlns"
                    || value.value() == XMLNS_NS
                    || (prefix == "xml") != (value.value() == XML_NS)
                    || (!prefix.is_empty() && value.value().is_empty())
                {
                    return Err(self.invalid());
                }
            } else if name.contains(':') {
                return Err(MetadataError::Unsupported {
                    feature: "qualified metadata attributes",
                });
            }
        }
        let prefix = tag.name.split_once(':').map_or("", |(prefix, _)| prefix);
        let namespace = if prefix == "xml" {
            Some(XML_NS)
        } else {
            tag.attrs
                .iter()
                .rev()
                .chain(inherited.iter().rev())
                .find_map(|(name, value)| {
                    let binding = if name == "xmlns" {
                        Some("")
                    } else {
                        name.strip_prefix("xmlns:")
                    };
                    (binding == Some(prefix)).then_some(value.value())
                })
        };
        if namespace != Some(expected) {
            return Err(MetadataError::Unsupported {
                feature: "metadata namespace or markup compatibility",
            });
        }
        Ok(())
    }
}

fn is_namespace(name: &str) -> bool {
    name == "xmlns" || name.starts_with("xmlns:")
}
fn local_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(_, local)| local)
}
fn qname(name: &str) -> bool {
    match name.split_once(':') {
        Some((prefix, local)) => ncname(prefix) && ncname(local),
        None => ncname(name),
    }
}
pub(super) fn ncname(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(name_start) && chars.all(name_char)
}
pub(super) fn space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}
fn xml_char(c: char) -> bool {
    space(c)
        || matches!(c, '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}
fn name_start(c: char) -> bool {
    c.is_ascii_alphabetic()
        || c == '_'
        || matches!(c, '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}
fn name_char(c: char) -> bool {
    name_start(c)
        || c.is_ascii_digit()
        || matches!(c, '-' | '.' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}')
}
