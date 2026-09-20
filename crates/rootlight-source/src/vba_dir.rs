//! Bounded structural reading of decompressed MS-OVBA project directories.
//! Names retain both encodings and offsets; parsing does not establish code-page
//! equivalence, CFB stream identity, PROJECT membership, or executable semantics.

use std::{fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

mod parser;
#[cfg(test)]
mod tests;

/// Resource boundaries for one already decompressed directory.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DirectoryLimits {
    /// Maximum supplied bytes, at most the existing source snapshot ceiling.
    pub input_bytes: usize,
    /// Maximum module records, at most 65,535 (the format's count width).
    pub modules: usize,
    /// Maximum reference records, at most 65,535.
    pub references: usize,
}

impl Default for DirectoryLimits {
    fn default() -> Self {
        Self {
            input_bytes: 16 * 1024 * 1024,
            modules: 4096,
            references: 4096,
        }
    }
}

/// A structural, resource, or cancellation failure in decompressed metadata.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DirectoryError {
    /// A field extends beyond the supplied decompressed bytes.
    #[error("truncated vba directory at byte {offset}")]
    Truncated {
        /// Offset in the decompressed directory, not the enclosing Office file.
        offset: usize,
    },
    /// An unknown, repeated, or out-of-order record cannot be interpreted safely.
    #[error("unexpected vba directory record {id:#06x} at byte {offset}")]
    Record {
        /// Offset of the unexpected two-byte identifier.
        offset: usize,
        /// Identifier found in the input.
        id: u16,
    },
    /// A size, fixed value, text encoding, count, or terminator is inconsistent.
    #[error("invalid vba directory at byte {offset}")]
    Invalid {
        /// Offset of the inconsistent field in the decompressed directory.
        offset: usize,
    },
    /// Caller-selected limits exceed hard ceilings.
    #[error("invalid vba directory limits")]
    Limits,
    /// A caller-selected resource boundary was reached before allocation.
    #[error("vba directory {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource name, never repository payload.
        resource: &'static str,
        /// Caller-selected maximum.
        maximum: usize,
    },
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// Exact text bytes and their range in the decompressed directory.
/// Encoding is determined by the enclosing field, never guessed or replaced.
#[derive(Clone, PartialEq, Eq)]
pub struct TextField<'a> {
    bytes: &'a [u8],
    range: Range<usize>,
}

impl fmt::Debug for TextField<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextField")
            .field("range", &self.range)
            .finish()
    }
}

impl TextField<'_> {
    /// Returns the exact bytes without code-page conversion or normalization.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Returns the payload range, excluding the record header and length field.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// The distinction encoded by MODULETYPE; object subtypes are not inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    /// A procedural module.
    Procedural,
    /// A document, class, or designer module; PROJECT must distinguish these.
    Object,
}

/// One ordered module declaration, not yet bound to a CFB source stream.
#[derive(Debug, Clone)]
pub struct ModuleRecord<'a> {
    range: Range<usize>,
    name: TextField<'a>,
    unicode_name: Option<TextField<'a>>,
    stream_name: TextField<'a>,
    unicode_stream_name: TextField<'a>,
    text_offset: u32,
    text_offset_range: Range<usize>,
    kind: ModuleKind,
    read_only: bool,
    private: bool,
}

impl<'a> ModuleRecord<'a> {
    /// Returns the complete declaration range, including its terminator.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// Returns the original module name in the project's declared code page.
    #[must_use]
    pub fn name(&self) -> &TextField<'a> {
        &self.name
    }
    /// Returns the optional original UTF-16LE module name, with valid scalars.
    #[must_use]
    pub fn unicode_name(&self) -> Option<&TextField<'a>> {
        self.unicode_name.as_ref()
    }
    /// Returns the declared CFB stream name in the project's code page.
    #[must_use]
    pub fn stream_name(&self) -> &TextField<'a> {
        &self.stream_name
    }
    /// Returns the original UTF-16LE stream name, not a normalized lookup key.
    #[must_use]
    pub fn unicode_stream_name(&self) -> &TextField<'a> {
        &self.unicode_stream_name
    }
    /// Returns the declared source offset; callers must check the matched stream.
    #[must_use]
    pub const fn text_offset(&self) -> u32 {
        self.text_offset
    }
    /// Returns the four bytes declaring the offset in the decompressed directory.
    #[must_use]
    pub fn text_offset_range(&self) -> Range<usize> {
        self.text_offset_range.clone()
    }
    /// Returns only the procedural/object distinction present in this record.
    #[must_use]
    pub const fn kind(&self) -> ModuleKind {
        self.kind
    }
    /// Returns whether the directory declares the module read-only.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
    /// Returns whether the directory restricts the module to its own project.
    #[must_use]
    pub const fn is_private(&self) -> bool {
        self.private
    }
}

/// Structural reference variants; no library or external project is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    /// A twiddled/extended type library reference.
    Control,
    /// An original library identifier followed by its control reference.
    OriginalControl,
    /// A registered Automation type library reference.
    Registered,
    /// An external VBA project reference.
    Project,
}

/// A complete reference boundary, retaining uninterpreted identifiers in input.
#[derive(Debug, Clone)]
pub struct ReferenceRecord {
    range: Range<usize>,
    kind: ReferenceKind,
}

impl ReferenceRecord {
    /// Returns the complete reference range, including any leading name record.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// Returns the structural reference variant, not evidence of resolution.
    #[must_use]
    pub const fn kind(&self) -> ReferenceKind {
        self.kind
    }
}

/// Structurally parsed directory with complete ordered module declarations.
pub struct ProjectDirectory<'a> {
    bytes: &'a [u8],
    information_range: Range<usize>,
    code_page: u16,
    name: TextField<'a>,
    references: Vec<ReferenceRecord>,
    modules: Vec<ModuleRecord<'a>>,
}

impl fmt::Debug for ProjectDirectory<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectDirectory")
            .field("bytes", &self.bytes.len())
            .field("code_page", &self.code_page)
            .field("references", &self.references.len())
            .field("modules", &self.modules.len())
            .finish()
    }
}

impl<'a> ProjectDirectory<'a> {
    /// Reads a complete decompressed MS-OVBA `dir` stream without I/O or execution.
    ///
    /// Validates ordered known record layouts, lengths, fixed values, null-free
    /// text and UTF-16 scalars. Reserved reader-ignored fields remain ignored.
    /// Does not validate MBCS/Unicode equivalence, identifier/Libid grammar,
    /// cross-stream PROJECT membership, uniqueness, or module source offsets.
    /// Callers must establish those before using a declaration as source identity.
    /// Ranges refer to decompressed input, not compressed or physical file bytes.
    ///
    /// # Errors
    /// Returns [`DirectoryError`] for malformed or unsupported record sequences,
    /// trailing bytes, exceeded limits, or caller cancellation. No partial success
    /// is returned and unknown records are never skipped by searching for markers.
    pub fn parse(
        bytes: &'a [u8],
        limits: DirectoryLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, DirectoryError> {
        parser::parse(bytes, limits, cancellation)
    }

    /// Returns all original decompressed bytes, including retained reference data.
    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    /// Returns the complete PROJECTINFORMATION range before references/modules.
    #[must_use]
    pub fn information_range(&self) -> Range<usize> {
        self.information_range.clone()
    }
    /// Returns the declared code page without defaulting unknown values to ANSI.
    #[must_use]
    pub const fn code_page(&self) -> u16 {
        self.code_page
    }

    /// Returns the original PROJECTNAME payload and its decompressed byte range.
    /// The bounded, nonempty MBCS bytes are not yet decoded or matched to PROJECT.
    #[must_use]
    pub fn name(&self) -> &TextField<'a> {
        &self.name
    }
    /// Returns references in their original order, never resolved or executed.
    #[must_use]
    pub fn references(&self) -> &[ReferenceRecord] {
        &self.references
    }
    /// Returns exactly the declared number of ordered module records.
    #[must_use]
    pub fn modules(&self) -> &[ModuleRecord<'a>] {
        &self.modules
    }
}
