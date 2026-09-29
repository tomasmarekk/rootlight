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

/// Ranges of the two name encodings in one REFERENCENAME record.
/// Both ranges address the owning directory's decompressed bytes. Their decoded
/// values have not been compared and must not be treated as a resolved identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceName {
    name: Range<usize>,
    unicode_name: Range<usize>,
}

impl ReferenceName {
    /// Returns the MBCS name payload in the project's declared code page.
    #[must_use]
    pub fn name_range(&self) -> Range<usize> {
        self.name.clone()
    }

    /// Returns the UTF-16LE name payload, without its length or reserved word.
    #[must_use]
    pub fn unicode_name_range(&self) -> Range<usize> {
        self.unicode_name.clone()
    }
}

/// Structured reference fields over the owning decompressed directory.
/// Text ranges retain exact MBCS bytes, not validated Libid grammar or filesystem
/// locations. No referenced library, project, registry entry or network is read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReferencePayload {
    /// A registered Automation library's uninterpreted identifier.
    Registered {
        /// Exact Libid payload, excluding its byte count.
        libid: Range<usize>,
    },
    /// An external project identifier pair and declared version.
    Project {
        /// LibidAbsolute payload; this is not a trusted local path.
        absolute_libid: Range<usize>,
        /// LibidRelative payload; no path resolution has occurred.
        relative_libid: Range<usize>,
        /// Declared major version, decoded as an unsigned 32-bit value.
        major_version: u32,
        /// Declared minor version, decoded as an unsigned 16-bit value.
        minor_version: u16,
        /// Exact six bytes carrying the major then minor version.
        version_range: Range<usize>,
    },
    /// A control library and its optional original Automation identifier.
    Control {
        /// LibidOriginal payload when preceded by REFERENCEORIGINAL.
        original_libid: Option<Range<usize>>,
        /// LibidTwiddled payload, without defaulting an empty or placeholder ID.
        twiddled_libid: Range<usize>,
        /// Optional name record belonging specifically to the extended library.
        extended_name: Option<ReferenceName>,
        /// LibidExtended payload.
        extended_libid: Range<usize>,
        /// Exact 16-byte OriginalTypeLib GUID in its original binary layout.
        original_type_library: Range<usize>,
        /// Declared cookie; uniqueness within the project is not established.
        cookie: u32,
        /// Exact four little-endian bytes carrying the cookie.
        cookie_range: Range<usize>,
    },
}

/// A complete reference boundary and its uninterpreted, source-bound fields.
#[derive(Debug, Clone)]
pub struct ReferenceRecord {
    range: Range<usize>,
    name: Option<ReferenceName>,
    payload: ReferencePayload,
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
        match &self.payload {
            ReferencePayload::Registered { .. } => ReferenceKind::Registered,
            ReferencePayload::Project { .. } => ReferenceKind::Project,
            ReferencePayload::Control {
                original_libid: Some(_),
                ..
            } => ReferenceKind::OriginalControl,
            ReferencePayload::Control {
                original_libid: None,
                ..
            } => ReferenceKind::Control,
        }
    }
    /// Returns the optional leading name record, separate from a control's extended name.
    #[must_use]
    pub fn name(&self) -> Option<&ReferenceName> {
        self.name.as_ref()
    }
    /// Returns parsed field ranges and numeric metadata, not resolution evidence.
    #[must_use]
    pub fn payload(&self) -> &ReferencePayload {
        &self.payload
    }
}

/// A native project's declared version with exact decompressed source coordinates.
/// Equality includes the source range; matching numeric versions alone does not
/// establish project identity, reference precedence or binary compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectVersionRecord {
    major: u32,
    minor: u16,
    range: Range<usize>,
}

impl ProjectVersionRecord {
    /// Returns the full unsigned major version from PROJECTVERSION.
    #[must_use]
    pub const fn major(&self) -> u32 {
        self.major
    }

    /// Returns the full unsigned minor version from PROJECTVERSION.
    #[must_use]
    pub const fn minor(&self) -> u16 {
        self.minor
    }

    /// Returns the six little-endian payload bytes in the decompressed directory.
    /// The record identifier and reader-ignored reserved word are excluded.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// Structurally parsed directory with complete ordered module declarations.
pub struct ProjectDirectory<'a> {
    bytes: &'a [u8],
    information_range: Range<usize>,
    code_page: u16,
    name: TextField<'a>,
    version: ProjectVersionRecord,
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

    /// Returns this project's declared version, not a referenced project's version
    /// or the implementation-specific performance-cache format version.
    #[must_use]
    pub const fn version(&self) -> &ProjectVersionRecord {
        &self.version
    }
    /// Returns references in their original order, never resolved or executed.
    #[must_use]
    pub fn references(&self) -> &[ReferenceRecord] {
        &self.references
    }
    /// Moves the owned reference inventory after consumers have read borrowed module fields.
    pub(crate) fn into_references(self) -> Vec<ReferenceRecord> {
        self.references
    }
    /// Returns exactly the declared number of ordered module records.
    #[must_use]
    pub fn modules(&self) -> &[ModuleRecord<'a>] {
        &self.modules
    }
}
