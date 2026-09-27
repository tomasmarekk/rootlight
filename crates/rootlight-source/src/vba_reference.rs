//! First-party interpretation of MS-OVBA library and project identifiers.
//! Declared identity and original byte ranges stay separate from availability:
//! this parser never opens paths, activates COM or resolves library symbols.

use std::{fmt, ops::Range};

use rootlight_cancel::Cancellation;

use crate::vba_text::{DecodedText, TextError, TextLimits};

#[cfg(test)]
mod tests;

/// One identifier field in a native project's ordered reference inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceField {
    /// Registered Automation library identifier.
    Registered,
    /// External project's absolute identifier.
    ProjectAbsolute,
    /// External project's relative identifier.
    ProjectRelative,
    /// Original Automation identifier preceding a control reference, when present.
    ControlOriginal,
    /// Control's twiddled library identifier, including an explicit placeholder.
    ControlTwiddled,
    /// Control's extended library identifier.
    ControlExtended,
}

/// Path syntax declared by an identifier, not the machine running analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferencePlatform {
    /// Windows path syntax.
    Windows,
    /// Macintosh path syntax.
    Macintosh,
}

/// Whether a project identifier declares a standalone or embedded VBA project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectReferenceKind {
    /// A standalone VBA project.
    Standalone,
    /// A project embedded in a host document.
    Embedded,
}

/// Parsed identity fields; these do not prove that a target exists or is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReferenceIdentity {
    /// Automation library identity, without path-based or display-name aliasing.
    Library {
        /// Declared path syntax.
        platform: ReferencePlatform,
        /// GUID bytes in textual hexadecimal order, not Windows GUID memory order.
        guid: [u8; 16],
        /// Hexadecimal major version interpreted as an unsigned integer.
        major: u16,
        /// Hexadecimal minor version interpreted as an unsigned integer.
        minor: u16,
        /// Hexadecimal locale identifier interpreted as an unsigned integer.
        lcid: u32,
    },
    /// Project form and path syntax; versions live in the enclosing dir record.
    Project {
        /// Declared path syntax.
        platform: ReferencePlatform,
        /// Standalone versus host-embedded project form.
        kind: ProjectReferenceKind,
    },
}

/// Failure to interpret a reference, without disclosing its source content.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReferenceError {
    /// The selected ordinal or identifier field is absent from the project.
    Unavailable,
    /// The requested range is reversed or outside the owning bytes.
    Range,
    /// The identifier does not conform to the requested MS-OVBA grammar.
    Invalid {
        /// Original byte offset of the identifier, not a decoded UTF-8 offset.
        offset: usize,
    },
    /// Declared-codepage decoding, resource admission or cancellation failed.
    Text(TextError),
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("vba reference identifier is unavailable"),
            Self::Range => f.write_str("invalid vba reference range"),
            Self::Invalid { offset } => {
                write!(f, "invalid vba reference identifier at byte {offset}")
            }
            Self::Text(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ReferenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Text(error) => Some(error),
            Self::Unavailable | Self::Range | Self::Invalid { .. } => None,
        }
    }
}

impl From<TextError> for ReferenceError {
    fn from(error: TextError) -> Self {
        Self::Text(error)
    }
}

/// An interpreted identifier retaining exact original bytes and field coordinates.
/// Text decoding is lossless; paths are not normalized, joined or accessed.
pub struct ReferenceIdentifier<'a> {
    decoded: DecodedText<'a>,
    source: Range<usize>,
    identity: ReferenceIdentity,
    path: Range<usize>,
    display_name: Option<Range<usize>>,
}

impl fmt::Debug for ReferenceIdentifier<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReferenceIdentifier")
            .field("source", &self.source)
            .field("code_page", &self.decoded.code_page())
            .finish_non_exhaustive()
    }
}

impl<'a> ReferenceIdentifier<'a> {
    /// Interprets one LibidReference or ProjectReference inside immutable bytes.
    ///
    /// `source` addresses the owning decompressed directory (or a standalone
    /// input). Returned ranges use those same coordinates, not decoded UTF-8.
    /// Library version and LCID fields are hexadecimal. Empty paths and nil GUIDs
    /// remain explicit; neither is evidence that a library can supply symbols.
    ///
    /// # Errors
    /// Returns [`ReferenceError`] for bad ranges, unsupported or invalid text,
    /// resource limits, malformed identifiers, or cancellation. There is no I/O.
    ///
    /// # Examples
    /// ```
    /// use rootlight_cancel::Cancellation;
    /// use rootlight_source::{vba_reference::ReferenceIdentifier, vba_text::TextLimits};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let bytes = br"*\C../Shared.xlsm";
    /// let identifier = ReferenceIdentifier::parse(
    ///     bytes, 0..bytes.len(), 1252, TextLimits::default(), &Cancellation::new(),
    /// )?;
    /// assert_eq!(identifier.path(), "../Shared.xlsm");
    /// assert_eq!(&bytes[identifier.path_range()], b"../Shared.xlsm");
    /// # Ok(())
    /// # }
    /// ```
    pub fn parse(
        bytes: &'a [u8],
        source: Range<usize>,
        code_page: u16,
        limits: TextLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, ReferenceError> {
        cancellation.check().map_err(TextError::from)?;
        let original = bytes.get(source.clone()).ok_or(ReferenceError::Range)?;
        let decoded = DecodedText::decode(original, code_page, limits, cancellation)?;
        let invalid = || ReferenceError::Invalid {
            offset: source.start,
        };
        let text = decoded.text();
        let (identity, path, display_name) = parse_fields(text).ok_or_else(invalid)?;
        if let Some(name) = &display_name {
            let range = decoded.original_range(name.clone()).ok_or_else(invalid)?;
            if range.len() > 255 {
                return Err(invalid());
            }
        }
        cancellation.check().map_err(TextError::from)?;
        Ok(Self {
            decoded,
            source,
            identity,
            path,
            display_name,
        })
    }

    /// Returns declared identity fields, not a resolved external symbol target.
    #[must_use]
    pub const fn identity(&self) -> ReferenceIdentity {
        self.identity
    }

    /// Returns the exact identifier range in the owning original input.
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }

    /// Returns the unchanged original identifier bytes in the declared code page.
    #[must_use]
    pub fn original_bytes(&self) -> &'a [u8] {
        self.decoded.original_bytes()
    }

    /// Returns the unnormalized decoded path, which is not safe to open implicitly.
    #[must_use]
    pub fn path(&self) -> &str {
        self.decoded_field(self.path.clone())
    }

    /// Returns exact original path bytes in the owning input's coordinates.
    #[must_use]
    pub fn path_range(&self) -> Range<usize> {
        self.original_range(self.path.clone())
    }

    /// Returns a library display name; projects have no display-name field.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name
            .clone()
            .map(|range| self.decoded_field(range))
    }

    /// Returns the exact display-name byte range, including an explicitly empty name.
    #[must_use]
    pub fn display_name_range(&self) -> Option<Range<usize>> {
        self.display_name
            .clone()
            .map(|range| self.original_range(range))
    }

    fn decoded_field(&self, range: Range<usize>) -> &str {
        self.decoded
            .text()
            .get(range)
            .expect("parsed scalar-boundary field")
    }

    fn original_range(&self, range: Range<usize>) -> Range<usize> {
        let range = self
            .decoded
            .original_range(range)
            .expect("parsed scalar-boundary field");
        // Both offsets are within the checked input window, so adding its base
        // cannot exceed the length of the owning immutable byte slice.
        self.source.start + range.start..self.source.start + range.end
    }
}

fn parse_fields(text: &str) -> Option<(ReferenceIdentity, Range<usize>, Option<Range<usize>>)> {
    if text.contains('\0') || !text.starts_with("*\\") {
        return None;
    }
    let tag = *text.as_bytes().get(2)?;
    let platform = match tag {
        b'A' | b'C' | b'G' => ReferencePlatform::Windows,
        b'B' | b'D' | b'H' => ReferencePlatform::Macintosh,
        _ => return None,
    };
    if matches!(tag, b'A'..=b'D') {
        let kind = if matches!(tag, b'A' | b'B') {
            ProjectReferenceKind::Standalone
        } else {
            ProjectReferenceKind::Embedded
        };
        return Some((
            ReferenceIdentity::Project { platform, kind },
            3..text.len(),
            None,
        ));
    }
    let (guid, rest) = text.get(3..)?.split_once('#')?;
    let guid = parse_guid(guid.as_bytes())?;
    let (version, rest) = rest.split_once('#')?;
    let (major, minor) = version.split_once('.')?;
    let major = u16::try_from(hex(major, 4)?).ok()?;
    let minor = u16::try_from(hex(minor, 4)?).ok()?;
    let (lcid, rest) = rest.split_once('#')?;
    let lcid = hex(lcid, 8)?;
    let (path, name) = rest.split_once('#')?;
    let path_start = text.len().checked_sub(rest.len())?;
    let name_start = text.len().checked_sub(name.len())?;
    Some((
        ReferenceIdentity::Library {
            platform,
            guid,
            major,
            minor,
            lcid,
        },
        path_start..path_start.checked_add(path.len())?,
        Some(name_start..text.len()),
    ))
}

fn hex(text: &str, maximum: usize) -> Option<u32> {
    if text.is_empty() || text.len() > maximum || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(text, 16).ok()
}

fn parse_guid(bytes: &[u8]) -> Option<[u8; 16]> {
    if bytes.len() != 38 || bytes.first() != Some(&b'{') || bytes.last() != Some(&b'}') {
        return None;
    }
    let mut result = [0; 16];
    let mut nibble = 0;
    for (index, byte) in bytes.iter().copied().enumerate().skip(1).take(36) {
        if matches!(index, 9 | 14 | 19 | 24) {
            if byte != b'-' {
                return None;
            }
        } else {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return None,
            };
            let value = result.get_mut(nibble / 2)?;
            *value = (*value << 4) | digit;
            nibble += 1;
        }
    }
    (nibble == 32).then_some(result)
}
