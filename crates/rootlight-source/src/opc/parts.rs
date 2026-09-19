//! Logical OPC part identities and package-relative reference resolution.
//! Names are IRI identifiers, never filesystem paths; ZIP percent encoding and
//! relationship dot segments are interpreted at their respective boundaries.

use super::valid_part_name;
use std::fmt;

const MAX_NAME_BYTES: usize = 65_535;

/// Failure to construct or resolve an OPC part identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PartNameError {
    /// The name or reference cannot identify a package part.
    #[error("invalid opc part name or relative reference")]
    Invalid,
    /// A name or reference exceeds the ZIP name-field ceiling.
    #[error("opc part name exceeds byte limit")]
    Limit,
    /// The ZIP name is not ASCII with valid UTF-8 percent-encoded non-ASCII characters.
    #[error("invalid opc zip name encoding")]
    ZipEncoding,
}

/// A validated part name canonicalized only by ASCII case equivalence.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartName(String);

impl fmt::Debug for PartName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PartName")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl PartName {
    /// Validates an absolute logical part name and folds ASCII letters for identity.
    ///
    /// # Errors
    /// Rejects empty/trailing-dot segments, invalid IRI characters, encoded
    /// unreserved characters or separators, and names above the fixed byte ceiling.
    pub fn parse(name: &str) -> Result<Self, PartNameError> {
        if name.len() > MAX_NAME_BYTES {
            return Err(PartNameError::Limit);
        }
        if !valid_part_name(name) {
            return Err(PartNameError::Invalid);
        }
        Ok(Self(name.to_ascii_lowercase()))
    }

    /// Maps an ASCII ZIP item name to a non-interleaved logical part name.
    ///
    /// # Errors
    /// Rejects unsupported/noncanonical ZIP names, malformed encoded UTF-8,
    /// non-part metadata/directory/piece names, or exceeded name bounds.
    /// ASCII escapes are preserved; only non-ASCII UTF-8 octets are decoded.
    pub fn from_zip_name(name: &[u8]) -> Result<Self, PartNameError> {
        if name.len() >= MAX_NAME_BYTES {
            return Err(PartNameError::Limit);
        }
        if !name.is_ascii() {
            return Err(PartNameError::ZipEncoding);
        }
        let mut logical = String::from("/");
        let mut position = 0;
        while let Some(&byte) = name.get(position) {
            if byte == b'%' && hex_byte(name, position).is_some_and(|byte| byte >= 128) {
                let mut encoded = Vec::new();
                while let Some(byte) = hex_byte(name, position).filter(|byte| *byte >= 128) {
                    encoded.push(byte);
                    position += 3;
                }
                logical.push_str(
                    std::str::from_utf8(&encoded).map_err(|_| PartNameError::ZipEncoding)?,
                );
            } else {
                logical.push(char::from(byte));
                position += 1;
            }
        }
        Self::parse(&logical)
    }

    /// Returns the canonical logical name; source spelling remains in its declaration.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether this name is reserved for a Relationships part.
    #[must_use]
    pub fn is_relationships(&self) -> bool {
        let Some((parent, leaf)) = self.0.rsplit_once('/') else {
            return false;
        };
        parent.rsplit('/').next() == Some("_rels") && leaf.ends_with(".rels")
    }

    /// Constructs the Relationships part name for a package or a source part.
    ///
    /// # Errors
    /// Rejects relationships originating from Relationships parts or an oversized name.
    pub fn relationships_for(source: Option<&Self>) -> Result<Self, PartNameError> {
        let Some(source) = source else {
            return Self::parse("/_rels/.rels");
        };
        if source.is_relationships() {
            return Err(PartNameError::Invalid);
        }
        let (parent, leaf) = source.0.rsplit_once('/').ok_or(PartNameError::Invalid)?;
        Self::parse(&format!("{parent}/_rels/{leaf}.rels"))
    }

    /// Resolves a relative IRI against a source part or the package root.
    ///
    /// # Errors
    /// Rejects schemes/authorities, malformed components, non-part results, encoded
    /// separators/unreserved characters and exceeded bounds. Dot segments follow
    /// RFC 3986, including clamping at the package root. Nothing is accessed.
    pub fn resolve<'a>(
        source: Option<&Self>,
        reference: &'a str,
    ) -> Result<PartTarget<'a>, PartNameError> {
        if reference.len() > MAX_NAME_BYTES {
            return Err(PartNameError::Limit);
        }
        let (before_fragment, fragment) = reference
            .split_once('#')
            .map_or((reference, None), |(a, b)| (a, Some(b)));
        let (path, query) = before_fragment
            .split_once('?')
            .map_or((before_fragment, None), |(a, b)| (a, Some(b)));
        if path.starts_with("//")
            || (!path.starts_with('/')
                && path
                    .split('/')
                    .next()
                    .is_some_and(|first| first.contains(':')))
        {
            return Err(PartNameError::Invalid);
        }
        if query.is_some_and(|value| !valid_suffix(value, true))
            || fragment.is_some_and(|value| !valid_suffix(value, false))
        {
            return Err(PartNameError::Invalid);
        }
        let part = if path.is_empty() {
            source.cloned().ok_or(PartNameError::Invalid)?
        } else {
            let parent = source
                .and_then(|source| source.0.rsplit_once('/').map(|(parent, _)| parent))
                .unwrap_or("");
            let merged = if path.starts_with('/') {
                path.to_owned()
            } else {
                format!("{parent}/{path}")
            };
            if merged.len() > MAX_NAME_BYTES {
                return Err(PartNameError::Limit);
            }
            let mut segments = Vec::new();
            for segment in merged
                .strip_prefix('/')
                .ok_or(PartNameError::Invalid)?
                .split('/')
            {
                match segment {
                    "." => {}
                    ".." => {
                        segments.pop();
                    }
                    other => segments.push(other),
                }
            }
            let mut name = format!("/{}", segments.join("/"));
            if merged.ends_with("/.") || merged.ends_with("/..") {
                name.push('/');
            }
            Self::parse(&name)?
        };
        Ok(PartTarget {
            part,
            query,
            fragment,
        })
    }
}

/// A resolved part identity with uninterpreted query and fragment components.
/// Queries do not affect OPC part selection; fragments can address only part content.
pub struct PartTarget<'a> {
    part: PartName,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

impl fmt::Debug for PartTarget<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PartTarget")
            .field("part", &self.part)
            .field("has_query", &self.query.is_some())
            .field("has_fragment", &self.fragment.is_some())
            .finish()
    }
}

impl<'a> PartTarget<'a> {
    /// Returns the package-local part identity, independent of query/fragment.
    #[must_use]
    pub const fn part(&self) -> &PartName {
        &self.part
    }
    /// Returns the original query component, without its question mark.
    #[must_use]
    pub const fn query(&self) -> Option<&'a str> {
        self.query
    }
    /// Returns the original fragment component, without its number sign.
    #[must_use]
    pub const fn fragment(&self) -> Option<&'a str> {
        self.fragment
    }
}

fn hex_byte(bytes: &[u8], offset: usize) -> Option<u8> {
    let [b'%', high, low] = bytes.get(offset..offset.checked_add(3)?)? else {
        return None;
    };
    u8::try_from(char::from(*high).to_digit(16)? * 16 + char::from(*low).to_digit(16)?).ok()
}

fn valid_suffix(value: &str, query: bool) -> bool {
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            if !chars.next().is_some_and(|c| c.is_ascii_hexdigit())
                || !chars.next().is_some_and(|c| c.is_ascii_hexdigit())
            {
                return false;
            }
        } else if !super::unreserved(c)
            && !matches!(
                c,
                '!' | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | ':'
                    | '@'
                    | '/'
                    | '?'
            )
            && !(query
                && matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}'))
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
