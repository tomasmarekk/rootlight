//! Source-bound OPC content-type and relationship declarations.
//! XML is read without DTDs, external resources or application activation;
//! parsing declarations does not establish ownership of a ZIP member.

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, content_hash};
use std::{collections::BTreeSet, fmt, ops::Range};

mod xml;

const TYPES_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// Independent bounds on one OPC metadata document.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct MetadataLimits {
    /// Maximum original XML bytes, at most one MiB.
    pub bytes: usize,
    /// Maximum child declarations, at most 65,536.
    pub records: usize,
    /// Maximum attributes per element, including namespace declarations (at most 128).
    pub attributes: usize,
    /// Maximum normalized UTF-8 bytes in a single attribute (at most 65,536).
    pub value_bytes: usize,
}

impl Default for MetadataLimits {
    fn default() -> Self {
        Self {
            bytes: 1024 * 1024,
            records: 4096,
            attributes: 32,
            value_bytes: 8192,
        }
    }
}

/// A source-free failure in an OPC metadata document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetadataError {
    /// A configured ceiling exceeds the fixed parser bounds.
    #[error("invalid opc metadata limits")]
    Limits,
    /// An input, declaration, attribute or value ceiling was exceeded.
    #[error("opc metadata limit exceeded: {resource}")]
    Limit {
        /// Static resource label, never document content.
        resource: &'static str,
    },
    /// XML encoding is invalid or its declaration contradicts its bytes.
    #[error("invalid opc xml encoding")]
    Encoding,
    /// XML syntax or schema-shaped metadata is invalid.
    #[error("invalid opc metadata at original byte {offset}")]
    Invalid {
        /// Offset into the supplied original XML document.
        offset: usize,
    },
    /// A feature is not interpreted; its data must not be treated as qualified metadata.
    #[error("unsupported opc metadata feature: {feature}")]
    Unsupported {
        /// Static feature label, never source content.
        feature: &'static str,
    },
    /// Declarations duplicate an identifier, extension or equivalent part name.
    #[error("duplicate opc metadata declaration at original byte {offset}")]
    Duplicate {
        /// Original byte offset of the conflicting declaration.
        offset: usize,
    },
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// A normalized XML attribute with its exact original, unquoted byte dependency.
#[derive(Clone, PartialEq, Eq)]
pub struct MetadataValue {
    value: String,
    range: Range<usize>,
}

impl fmt::Debug for MetadataValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataValue")
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl MetadataValue {
    /// Returns the value after XML character references and whitespace normalization.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
    /// Returns the original attribute value range, excluding quotes.
    /// Escapes and UTF-16 mean this is a dependency, not a byte-for-byte mapping.
    #[must_use]
    pub fn original_range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// The selector controlling a content-type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentTypeSelector {
    /// Extension of the final part-name segment, without its separating dot.
    Extension(MetadataValue),
    /// An absolute package part name, not a filesystem path.
    PartName(MetadataValue),
}

/// One source-backed Default or Override declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentTypeDeclaration {
    selector: ContentTypeSelector,
    media_type: MetadataValue,
    range: Range<usize>,
}

impl ContentTypeDeclaration {
    /// Returns the validated declaration selector.
    #[must_use]
    pub const fn selector(&self) -> &ContentTypeSelector {
        &self.selector
    }
    /// Returns the media type, including any declared parameters.
    #[must_use]
    pub const fn media_type(&self) -> &MetadataValue {
        &self.media_type
    }
    /// Returns the complete original element range.
    #[must_use]
    pub fn original_range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// Content-type declarations pinned to the supplied XML bytes.
#[derive(Debug)]
pub struct ContentTypes {
    hash: ContentHash,
    declarations: Vec<ContentTypeDeclaration>,
}

impl ContentTypes {
    /// Reads OPC Default/Override metadata in UTF-8 or BOM-marked UTF-16.
    ///
    /// # Errors
    /// Rejects malformed XML, DTDs, unsupported extensions, invalid selectors/media
    /// types, duplicate equivalent selectors, exceeded bounds or cancellation.
    /// No external entities, schemas or other resources are accessed.
    pub fn parse(
        input: &[u8],
        limits: MetadataLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, MetadataError> {
        let elements = xml::parse(input, limits, TYPES_NS, "Types", cancellation)?;
        let mut declarations = Vec::new();
        let mut seen = BTreeSet::new();
        for mut element in elements {
            cancellation.check()?;
            let override_part = match element.name.as_str() {
                "Default" => false,
                "Override" => true,
                _ => {
                    return Err(MetadataError::Invalid {
                        offset: element.range.start,
                    });
                }
            };
            let mut selector = element.take(if override_part {
                "PartName"
            } else {
                "Extension"
            })?;
            if override_part {
                collapse(&mut selector);
            }
            let media_type = element.take("ContentType")?;
            element.finish()?;
            if !valid_media_type(media_type.value())
                || !(if override_part {
                    valid_part_name(selector.value())
                } else {
                    valid_extension(selector.value())
                })
            {
                return Err(MetadataError::Invalid {
                    offset: element.range.start,
                });
            }
            if !seen.insert((override_part, selector.value.to_ascii_lowercase())) {
                return Err(MetadataError::Duplicate {
                    offset: element.range.start,
                });
            }
            declarations.push(ContentTypeDeclaration {
                selector: if override_part {
                    ContentTypeSelector::PartName(selector)
                } else {
                    ContentTypeSelector::Extension(selector)
                },
                media_type,
                range: element.range,
            });
        }
        cancellation.check()?;
        Ok(Self {
            hash: content_hash(input),
            declarations,
        })
    }
    /// Returns the hash of the original XML, before normalization or transcoding.
    #[must_use]
    pub const fn source_hash(&self) -> ContentHash {
        self.hash
    }
    /// Returns declarations in document order, preserving source provenance.
    #[must_use]
    pub fn declarations(&self) -> &[ContentTypeDeclaration] {
        &self.declarations
    }
    /// Resolves a syntactically valid part name, preferring Override over Default.
    /// Absence or an invalid name returns None; this does not prove ZIP membership.
    #[must_use]
    pub fn for_part(&self, name: &str) -> Option<&ContentTypeDeclaration> {
        if !valid_part_name(name) {
            return None;
        }
        let extension = name
            .rsplit('/')
            .next()?
            .rsplit_once('.')
            .map(|(_, value)| value);
        let mut default = None;
        for declaration in &self.declarations {
            match &declaration.selector {
                ContentTypeSelector::PartName(part) if part.value().eq_ignore_ascii_case(name) => {
                    return Some(declaration);
                }
                ContentTypeSelector::Extension(value)
                    if extension.is_some_and(|ext| value.value().eq_ignore_ascii_case(ext)) =>
                {
                    default = Some(declaration)
                }
                _ => {}
            }
        }
        default
    }
}

/// Whether a relationship addresses a package part or an external resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    /// Relative reference against the source part, or the package root.
    Internal,
    /// External resource; never loaded by this parser.
    External,
}

/// One source-backed OPC relationship, before package target resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    id: MetadataValue,
    kind: MetadataValue,
    target: MetadataValue,
    mode: TargetMode,
    mode_source: Option<MetadataValue>,
    range: Range<usize>,
}

impl Relationship {
    /// Returns the document-local XML identifier.
    #[must_use]
    pub const fn id(&self) -> &MetadataValue {
        &self.id
    }
    /// Returns the relationship type; comparisons are exact and case-sensitive.
    #[must_use]
    pub const fn kind(&self) -> &MetadataValue {
        &self.kind
    }
    /// Returns the declared, untrusted target before package-relative resolution.
    #[must_use]
    pub const fn target(&self) -> &MetadataValue {
        &self.target
    }
    /// Returns the effective target mode; omission means Internal.
    #[must_use]
    pub const fn mode(&self) -> TargetMode {
        self.mode
    }
    /// Returns the explicit target-mode attribute, if present.
    #[must_use]
    pub const fn mode_source(&self) -> Option<&MetadataValue> {
        self.mode_source.as_ref()
    }
    /// Returns the complete original relationship element range.
    #[must_use]
    pub fn original_range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// Relationships pinned to one metadata source, not yet to their owning part.
#[derive(Debug)]
pub struct Relationships {
    hash: ContentHash,
    declarations: Vec<Relationship>,
}

impl Relationships {
    /// Reads OPC relationships without resolving or fetching their targets.
    ///
    /// # Errors
    /// Rejects malformed XML, duplicate IDs, invalid attributes, DTDs, unsupported
    /// markup-compatibility extensions, exceeded bounds or cancellation.
    /// The owning part and internal target existence require separate validation.
    pub fn parse(
        input: &[u8],
        limits: MetadataLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, MetadataError> {
        let elements = xml::parse(input, limits, RELS_NS, "Relationships", cancellation)?;
        let mut declarations = Vec::new();
        let mut ids = BTreeSet::new();
        for mut element in elements {
            cancellation.check()?;
            if element.name != "Relationship" {
                return Err(MetadataError::Invalid {
                    offset: element.range.start,
                });
            }
            let mut id = element.take("Id")?;
            let mut kind = element.take("Type")?;
            let mut target = element.take("Target")?;
            // xsd:ID and xsd:anyURI collapse XML whitespace after attribute normalization.
            for value in [&mut id, &mut kind, &mut target] {
                collapse(value);
            }
            if !xml::ncname(id.value()) {
                return Err(MetadataError::Invalid {
                    offset: id.range.start,
                });
            }
            if !ids.insert(id.value.clone()) {
                return Err(MetadataError::Duplicate {
                    offset: id.range.start,
                });
            }
            let mode_source = element.optional("TargetMode");
            let mode = match mode_source.as_ref().map(MetadataValue::value) {
                None | Some("Internal") => TargetMode::Internal,
                Some("External") => TargetMode::External,
                _ => {
                    return Err(MetadataError::Invalid {
                        offset: element.range.start,
                    });
                }
            };
            element.finish()?;
            declarations.push(Relationship {
                id,
                kind,
                target,
                mode,
                mode_source,
                range: element.range,
            });
        }
        cancellation.check()?;
        Ok(Self {
            hash: content_hash(input),
            declarations,
        })
    }
    /// Returns the original XML hash, not a package or repository identity.
    #[must_use]
    pub const fn source_hash(&self) -> ContentHash {
        self.hash
    }
    /// Returns relationships in original document order.
    #[must_use]
    pub fn declarations(&self) -> &[Relationship] {
        &self.declarations
    }
}

fn valid_extension(value: &str) -> bool {
    !value.is_empty() && value.is_ascii() && !value.contains(['.', ';']) && valid_segment(value)
}

fn collapse(value: &mut MetadataValue) {
    let mut normalized = String::new();
    for word in value
        .value
        .split(xml::space)
        .filter(|part| !part.is_empty())
    {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    value.value = normalized;
}

fn valid_part_name(value: &str) -> bool {
    value
        .strip_prefix('/')
        .is_some_and(|rest| rest.split('/').all(valid_segment))
}

fn valid_segment(value: &str) -> bool {
    if value.is_empty() || value.ends_with('.') {
        return false;
    }
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let Some(high) = chars.next().and_then(|c| c.to_digit(16)) else {
                return false;
            };
            let Some(low) = chars.next().and_then(|c| c.to_digit(16)) else {
                return false;
            };
            let byte = high * 16 + low;
            if byte == 47
                || byte == 92
                || char::from_u32(byte).is_some_and(unreserved)
                || byte >= 128
            {
                return false;
            }
        } else if !unreserved(c)
            && !matches!(
                c,
                '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | ':' | '@'
            )
        {
            return false;
        }
    }
    true
}

fn unreserved(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '-' | '.' | '_' | '~')
        || matches!(c, '\u{a0}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{ffef}')
        || (u32::from(c) >= 0x10000 && u32::from(c) <= 0xefffd && u32::from(c) & 0xffff <= 0xfffd)
}

fn token(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn valid_media_type(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    for separator in ['/', ';'] {
        let mut count = 0;
        while chars.peek().is_some_and(|c| token(*c)) {
            chars.next();
            count += 1;
        }
        if count == 0 {
            return false;
        }
        if separator == '/' && chars.next() != Some('/') {
            return false;
        }
    }
    loop {
        while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
            chars.next();
        }
        match chars.next() {
            None => return true,
            Some(';') => {}
            _ => return false,
        }
        while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
            chars.next();
        }
        let mut count = 0;
        while chars.peek().is_some_and(|c| token(*c)) {
            chars.next();
            count += 1;
        }
        if count == 0 || chars.next() != Some('=') {
            return false;
        }
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => {
                        if !chars
                            .next()
                            .is_some_and(|c| c == '\t' || (' '..='~').contains(&c))
                        {
                            return false;
                        }
                    }
                    Some(c)
                        if c == '\t'
                            || (' '..='~').contains(&c)
                            || ('\u{80}'..='\u{ff}').contains(&c) => {}
                    _ => return false,
                }
            }
        } else {
            let mut count = 0;
            while chars.peek().is_some_and(|c| token(*c)) {
                chars.next();
                count += 1;
            }
            if count == 0 {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests;
