//! Bounded PROJECT property framing and module subtype declarations.
//! Names remain original MBCS bytes; this reader neither activates references nor
//! treats UI metadata, protected values, or undecoded names as semantic evidence.

use std::{fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

#[cfg(test)]
mod tests;

/// Independent resource ceilings for one PROJECT stream's property section.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ProjectLimits {
    /// Maximum complete supplied stream bytes, at most the source snapshot ceiling.
    pub input_bytes: usize,
    /// Maximum property-line bytes, excluding the two-byte newline.
    pub line_bytes: usize,
    /// Maximum module and package declarations combined, at most 65,535.
    pub items: usize,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        Self {
            input_bytes: 16 * 1024 * 1024,
            line_bytes: 64 * 1024,
            items: 4096,
        }
    }
}

/// A structural or resource failure; messages never include property payloads.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectError {
    /// A mandatory field, newline pair, declaration, or property order is invalid.
    #[error("invalid vba project property at byte {offset}")]
    Invalid {
        /// Original offset in the uncompressed PROJECT stream.
        offset: usize,
    },
    /// The stream ends before the complete property-section separator.
    #[error("truncated vba project properties at byte {offset}")]
    Truncated {
        /// Original offset where the required bytes are missing.
        offset: usize,
    },
    /// Caller-supplied ceilings exceed hard limits.
    #[error("invalid vba project limits")]
    Limits,
    /// A resource ceiling was reached before retaining another declaration.
    #[error("vba project {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource label, not input content.
        resource: &'static str,
        /// Caller-supplied maximum.
        maximum: usize,
    },
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// The module subtype written in PROJECT, independent of stream naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectModuleKind {
    /// A standard procedural module.
    Procedural,
    /// A class module.
    Class,
    /// A module extending a host document.
    Document {
        /// Raw 32-bit automation version representation, not a host identity.
        automation_version: u32,
    },
    /// A module extending a designer; its designer storage is not opened here.
    Designer,
}

/// A module declaration retaining its original undecoded name and byte ranges.
#[derive(Clone)]
pub struct ProjectModule<'a> {
    name: &'a [u8],
    name_range: Range<usize>,
    range: Range<usize>,
    kind: ProjectModuleKind,
}

impl fmt::Debug for ProjectModule<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectModule")
            .field("range", &self.range)
            .field("name_range", &self.name_range)
            .field("kind", &self.kind)
            .finish()
    }
}

impl ProjectModule<'_> {
    /// Returns exact MBCS name bytes, without case folding or codepage guessing.
    #[must_use]
    pub const fn name(&self) -> &[u8] {
        self.name
    }

    /// Returns the name's range in the original PROJECT stream.
    #[must_use]
    pub fn name_range(&self) -> Range<usize> {
        self.name_range.clone()
    }

    /// Returns the complete declaration range, including its newline pair.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Returns the declared subtype; it is not yet reconciled with dir metadata.
    #[must_use]
    pub const fn kind(&self) -> ProjectModuleKind {
        self.kind
    }
}

/// The ordered module declarations in a structurally framed property section.
/// Duplicate names remain separate: deciding equivalence requires the codepage.
pub struct ProjectProperties<'a> {
    bytes: &'a [u8],
    end: usize,
    modules: Vec<ProjectModule<'a>>,
    packages: Vec<Range<usize>>,
    name_value: &'a [u8],
    name_value_range: Range<usize>,
}

impl fmt::Debug for ProjectProperties<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectProperties")
            .field("end", &self.end)
            .field("modules", &self.modules.len())
            .field("packages", &self.packages.len())
            .finish()
    }
}

impl<'a> ProjectProperties<'a> {
    /// Reads the property section through its mandatory empty-line separator.
    ///
    /// Accepts both MS-OVBA NWLN pairs and ASCII-case-insensitive ABNF keywords.
    /// Property order, module forms, GUIDs, and document version syntax are checked.
    /// Other property values are retained opaque: this is not protection decoding,
    /// full PROJECT semantic validation, name/codepage validation, or a dir binding.
    /// The host-extender/workspace suffix is not parsed and cannot add modules.
    ///
    /// # Errors
    /// Returns [`ProjectError`] for malformed framing, unknown/out-of-order
    /// properties, incomplete input, limits, or cancellation. No partial inventory
    /// substitutes for a failed parse.
    pub fn parse(
        bytes: &'a [u8],
        limits: ProjectLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, ProjectError> {
        cancellation.check()?;
        if u64::try_from(limits.input_bytes).map_err(|_| ProjectError::Limits)?
            > crate::HARD_MAX_SNAPSHOT_BYTES
            || limits.line_bytes > limits.input_bytes
            || limits.items > usize::from(u16::MAX)
        {
            return Err(ProjectError::Limits);
        }
        if bytes.len() > limits.input_bytes {
            return Err(ProjectError::Limit {
                resource: "input bytes",
                maximum: limits.input_bytes,
            });
        }
        let mut reader = Reader {
            bytes,
            offset: 0,
            line_bytes: limits.line_bytes,
            cancellation,
        };
        let first = reader.line()?;
        let (key, value) = first.property()?;
        let id = value
            .strip_prefix(b"\"")
            .and_then(|v| v.strip_suffix(b"\""));
        if !key.eq_ignore_ascii_case(b"ID") || !id.is_some_and(guid) {
            return Err(first.invalid());
        }
        let mut modules = Vec::new();
        let mut packages = Vec::new();
        let mut next = reader.line()?;
        loop {
            let (key, value) = next.property()?;
            let kind = if key.eq_ignore_ascii_case(b"Module") {
                Some(ProjectModuleKind::Procedural)
            } else if key.eq_ignore_ascii_case(b"Class") {
                Some(ProjectModuleKind::Class)
            } else if key.eq_ignore_ascii_case(b"BaseClass") {
                Some(ProjectModuleKind::Designer)
            } else if key.eq_ignore_ascii_case(b"Document") {
                Some(ProjectModuleKind::Document {
                    automation_version: 0,
                })
            } else {
                None
            };
            if kind.is_none() && !key.eq_ignore_ascii_case(b"Package") {
                break;
            }
            if modules.len() + packages.len() >= limits.items {
                return Err(ProjectError::Limit {
                    resource: "items",
                    maximum: limits.items,
                });
            }
            if let Some(mut kind) = kind {
                let name = if matches!(kind, ProjectModuleKind::Document { .. }) {
                    // The suffix is fixed-width; splitting on the first slash
                    // would corrupt a permissive ModuleIdentifier containing '/'.
                    let split = value.len().checked_sub(11).ok_or_else(|| next.invalid())?;
                    let (name, suffix) = value
                        .split_at_checked(split)
                        .ok_or_else(|| next.invalid())?;
                    let version = suffix.strip_prefix(b"/").ok_or_else(|| next.invalid())?;
                    kind = ProjectModuleKind::Document {
                        automation_version: hex32(version).ok_or_else(|| next.invalid())?,
                    };
                    name
                } else {
                    value
                };
                if name.is_empty() {
                    return Err(next.invalid());
                }
                let start = next.start + key.len() + 1;
                modules.push(ProjectModule {
                    name,
                    name_range: start..start + name.len(),
                    range: next.start..reader.offset,
                    kind,
                });
            } else {
                if !guid(value) {
                    return Err(next.invalid());
                }
                packages.push(next.start..reader.offset);
            }
            next = reader.line()?;
        }
        // Only the ProjectItem region supplies module identities. In particular,
        // text in descriptions or the UI suffix must never be scanned for names.
        let mut name = None;
        for (expected, required) in [
            (b"HelpFile".as_slice(), false),
            (b"ExeName32".as_slice(), false),
            (b"Name".as_slice(), true),
            (b"HelpContextID".as_slice(), true),
            (b"Description".as_slice(), false),
            (b"VersionCompatible32".as_slice(), false),
            (b"CMG".as_slice(), true),
            (b"DPB".as_slice(), true),
            (b"GC".as_slice(), true),
        ] {
            let (key, value) = next.property()?;
            if key.eq_ignore_ascii_case(expected) {
                if expected == b"Name" {
                    let start = next.start + key.len() + 1;
                    name = Some((value, start..start + value.len()));
                }
                next = reader.line()?;
            } else if required {
                return Err(next.invalid());
            }
        }
        if !next.bytes.is_empty() {
            return Err(next.invalid());
        }
        let (name_value, name_value_range) = name.ok_or_else(|| next.invalid())?;
        Ok(Self {
            bytes,
            end: reader.offset,
            modules,
            packages,
            name_value,
            name_value_range,
        })
    }

    /// Returns the original Name property value, including any quotation marks.
    /// Framing alone does not validate its quoting, codepage or dir agreement.
    #[must_use]
    pub fn name_value(&self) -> &[u8] {
        self.name_value
    }

    /// Returns the Name value range in PROJECT, including any quotation marks.
    #[must_use]
    pub fn name_value_range(&self) -> Range<usize> {
        self.name_value_range.clone()
    }

    /// Returns every written module declaration in order, including duplicates.
    #[must_use]
    pub fn modules(&self) -> &[ProjectModule<'a>] {
        &self.modules
    }

    /// Returns the physical stream ranges of the written package declarations.
    #[must_use]
    pub fn package_ranges(&self) -> &[Range<usize>] {
        &self.packages
    }

    /// Returns the property section including the empty-line separator.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        0..self.end
    }

    /// Returns the unvalidated host-extender/workspace suffix's original range.
    #[must_use]
    pub fn remainder_range(&self) -> Range<usize> {
        self.end..self.bytes.len()
    }
}

struct Line<'a> {
    bytes: &'a [u8],
    start: usize,
}

impl<'a> Line<'a> {
    fn invalid(&self) -> ProjectError {
        ProjectError::Invalid { offset: self.start }
    }

    fn property(&self) -> Result<(&'a [u8], &'a [u8]), ProjectError> {
        let at = self
            .bytes
            .iter()
            .position(|b| *b == b'=')
            .ok_or_else(|| self.invalid())?;
        let (key, rest) = self
            .bytes
            .split_at_checked(at)
            .ok_or_else(|| self.invalid())?;
        let value = rest.strip_prefix(b"=").ok_or_else(|| self.invalid())?;
        Ok((key, value))
    }
}

struct Reader<'a, 'c> {
    bytes: &'a [u8],
    offset: usize,
    line_bytes: usize,
    cancellation: &'c Cancellation,
}

impl<'a> Reader<'a, '_> {
    fn line(&mut self) -> Result<Line<'a>, ProjectError> {
        self.cancellation.check()?;
        let start = self.offset;
        let remaining = self
            .bytes
            .get(start..)
            .ok_or(ProjectError::Truncated { offset: start })?;
        for (length, byte) in remaining.iter().copied().enumerate() {
            if length % 4096 == 0 {
                self.cancellation.check()?;
            }
            if length > self.line_bytes {
                return Err(ProjectError::Limit {
                    resource: "line bytes",
                    maximum: self.line_bytes,
                });
            }
            if byte == 0 {
                return Err(ProjectError::Invalid {
                    offset: start + length,
                });
            }
            if matches!(byte, b'\r' | b'\n') {
                let second = remaining.get(length + 1).ok_or(ProjectError::Truncated {
                    offset: start + length,
                })?;
                if !matches!((byte, *second), (b'\r', b'\n') | (b'\n', b'\r')) {
                    return Err(ProjectError::Invalid {
                        offset: start + length,
                    });
                }
                let bytes = remaining
                    .get(..length)
                    .ok_or(ProjectError::Truncated { offset: start })?;
                self.offset = start + length + 2;
                return Ok(Line { bytes, start });
            }
        }
        Err(ProjectError::Truncated {
            offset: self.bytes.len(),
        })
    }
}

fn guid(bytes: &[u8]) -> bool {
    bytes.len() == 38
        && bytes.iter().enumerate().all(|(i, b)| match i {
            0 => *b == b'{',
            37 => *b == b'}',
            9 | 14 | 19 | 24 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn hex32(bytes: &[u8]) -> Option<u32> {
    let prefix = bytes.get(..2)?;
    if bytes.len() != 10 || !prefix.eq_ignore_ascii_case(b"&H") {
        return None;
    }
    bytes.get(2..)?.iter().try_fold(0u32, |value, byte| {
        let digit = char::from(*byte).to_digit(16)?;
        value.checked_mul(16)?.checked_add(digit)
    })
}
