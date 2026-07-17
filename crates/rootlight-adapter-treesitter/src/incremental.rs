//! Parser-independent edit and opaque previous-parse contracts.
//!
//! Handles identify provider-owned bounded cache entries. They never own or
//! expose native trees, and an evicted entry invalidates reuse explicitly.

use std::fmt;

use rootlight_adapter_sdk::{IncludedRange, ParseOutput};
use rootlight_ids::ContentHash;

use crate::{GrammarFamily, ParserSettings};

/// One sequential UTF-8 source replacement used for checked incremental edits.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceEdit {
    start_byte: usize,
    old_end_byte: usize,
    replacement: Vec<u8>,
}

impl SourceEdit {
    /// Creates a sequential replacement using UTF-8 text.
    ///
    /// Offsets are interpreted against the source state produced by all earlier
    /// edits in the same request.
    ///
    /// # Errors
    ///
    /// Returns [`SourceEditError`] for an inverted range or offset overflow.
    pub fn new(
        start_byte: usize,
        old_end_byte: usize,
        replacement: &str,
    ) -> Result<Self, SourceEditError> {
        if start_byte > old_end_byte {
            return Err(SourceEditError::InvertedRange);
        }
        start_byte
            .checked_add(replacement.len())
            .ok_or(SourceEditError::OffsetOverflow)?;
        Ok(Self {
            start_byte,
            old_end_byte,
            replacement: replacement.as_bytes().to_vec(),
        })
    }

    /// Returns the replacement start byte in the current sequential source.
    #[must_use]
    pub const fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// Returns the exclusive old end byte in the current sequential source.
    #[must_use]
    pub const fn old_end_byte(&self) -> usize {
        self.old_end_byte
    }

    /// Returns the exclusive new end byte after applying this edit.
    #[must_use]
    pub fn new_end_byte(&self) -> usize {
        self.start_byte.saturating_add(self.replacement.len())
    }

    /// Returns the UTF-8 replacement byte length without exposing source text.
    #[must_use]
    pub const fn replacement_bytes(&self) -> usize {
        self.replacement.len()
    }

    pub(crate) fn replacement(&self) -> &[u8] {
        &self.replacement
    }
}

impl fmt::Debug for SourceEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceEdit")
            .field("start_byte", &self.start_byte)
            .field("old_end_byte", &self.old_end_byte)
            .field("replacement_bytes", &self.replacement.len())
            .finish()
    }
}

/// Source-free identity of one validated incremental replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceEditIdentity {
    start_byte: usize,
    old_end_byte: usize,
    new_end_byte: usize,
    replacement_bytes: usize,
    replacement_hash: ContentHash,
}

impl SourceEditIdentity {
    pub(crate) fn from_edit(edit: &SourceEdit, replacement_hash: ContentHash) -> Self {
        Self {
            start_byte: edit.start_byte(),
            old_end_byte: edit.old_end_byte(),
            new_end_byte: edit.new_end_byte(),
            replacement_bytes: edit.replacement_bytes(),
            replacement_hash,
        }
    }

    /// Returns the replacement start byte.
    #[must_use]
    pub const fn start_byte(self) -> usize {
        self.start_byte
    }

    /// Returns the exclusive end byte in the previous sequential source.
    #[must_use]
    pub const fn old_end_byte(self) -> usize {
        self.old_end_byte
    }

    /// Returns the exclusive end byte after applying the replacement.
    #[must_use]
    pub const fn new_end_byte(self) -> usize {
        self.new_end_byte
    }

    /// Returns the replacement byte length.
    #[must_use]
    pub const fn replacement_bytes(self) -> usize {
        self.replacement_bytes
    }

    /// Returns the replacement content hash without retaining source bytes.
    #[must_use]
    pub const fn replacement_hash(self) -> ContentHash {
        self.replacement_hash
    }
}

/// Invalid parser-independent incremental edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceEditError {
    /// Start was after the old end.
    #[error("source edit range is inverted")]
    InvertedRange,
    /// The replacement end was not representable.
    #[error("source edit offset overflowed")]
    OffsetOverflow,
}

/// Opaque identity of a provider-owned previous parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousParse {
    pub(crate) provider_id: u64,
    pub(crate) entry_id: u64,
}

/// An exact parser-native-free key describing one reuse attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseReuseKey {
    pub(crate) previous_content_hash: Option<ContentHash>,
    pub(crate) current_content_hash: ContentHash,
    pub(crate) family: GrammarFamily,
    pub(crate) grammar_version: &'static str,
    pub(crate) encoding: String,
    pub(crate) included_ranges: Vec<IncludedRange>,
    pub(crate) settings: ParserSettings,
    pub(crate) edits: Vec<SourceEditIdentity>,
}

impl ParseReuseKey {
    /// Returns the previous content hash when a handle was supplied and found.
    #[must_use]
    pub const fn previous_content_hash(&self) -> Option<ContentHash> {
        self.previous_content_hash
    }

    /// Returns the current immutable source content hash.
    #[must_use]
    pub const fn current_content_hash(&self) -> ContentHash {
        self.current_content_hash
    }

    /// Returns the requested grammar family.
    #[must_use]
    pub const fn family(&self) -> GrammarFamily {
        self.family
    }

    /// Returns the exact grammar version.
    #[must_use]
    pub const fn grammar_version(&self) -> &'static str {
        self.grammar_version
    }

    /// Returns the normalized source encoding.
    #[must_use]
    pub fn encoding(&self) -> &str {
        &self.encoding
    }

    /// Returns the exact checked included ranges.
    #[must_use]
    pub fn included_ranges(&self) -> &[IncludedRange] {
        &self.included_ranges
    }

    /// Returns parser scheduling settings.
    #[must_use]
    pub const fn settings(&self) -> ParserSettings {
        self.settings
    }

    /// Returns source-free identities for the validated sequential edits.
    #[must_use]
    pub fn edits(&self) -> &[SourceEditIdentity] {
        &self.edits
    }
}

/// Why a supplied previous parse was conservatively not reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReuseInvalidation {
    /// The handle belongs to another provider instance.
    Provider,
    /// The bounded cache evicted the native tree.
    Evicted,
    /// The language family changed.
    Language,
    /// The generated grammar version changed.
    GrammarVersion,
    /// The declared source encoding changed.
    Encoding,
    /// Included source ranges changed.
    IncludedRanges,
    /// Parser scheduling options changed.
    ParserSettings,
    /// Content changed without a validating edit sequence.
    MissingEdits,
    /// An edit referenced bytes outside the sequential old source.
    EditOutsideSource,
    /// An edit offset was not a UTF-8 scalar boundary.
    EditNotCharacterBoundary,
    /// Applying the edits did not reproduce the exact immutable new source.
    EditResultMismatch,
    /// Edit or cache accounting overflowed.
    AccountingOverflow,
}

/// Outcome of an incremental reuse attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReuseStatus {
    /// No previous handle was supplied.
    Fresh,
    /// The prior tree was edited and reused.
    Reused {
        /// Changed syntax ranges reported by Tree-sitter.
        changed_ranges: usize,
    },
    /// A previous handle was supplied but conservatively invalidated.
    Invalidated(ReuseInvalidation),
}

/// Committed parse output plus its bounded previous-parse continuation.
#[derive(Debug, Clone)]
pub struct ParseWithPrevious {
    pub(crate) output: ParseOutput,
    pub(crate) previous: Option<PreviousParse>,
    pub(crate) reuse_status: ReuseStatus,
    pub(crate) reuse_key: ParseReuseKey,
}

impl ParseWithPrevious {
    /// Returns the transactional parse report.
    #[must_use]
    pub const fn report(&self) -> &rootlight_adapter_sdk::ParseReport {
        self.output.report()
    }

    /// Returns the committed transactional parser output.
    #[must_use]
    pub const fn output(&self) -> &ParseOutput {
        &self.output
    }

    /// Returns a cache-backed handle when the parsed tree fit the cache budget.
    #[must_use]
    pub const fn previous(&self) -> Option<&PreviousParse> {
        self.previous.as_ref()
    }

    /// Returns whether and how prior work was reused.
    #[must_use]
    pub const fn reuse_status(&self) -> ReuseStatus {
        self.reuse_status
    }

    /// Returns the complete parser-independent reuse identity.
    #[must_use]
    pub const fn reuse_key(&self) -> &ParseReuseKey {
        &self.reuse_key
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseIdentity {
    pub(crate) content_hash: ContentHash,
    pub(crate) family: GrammarFamily,
    pub(crate) grammar_version: &'static str,
    pub(crate) encoding: String,
    pub(crate) included_ranges: Vec<IncludedRange>,
    pub(crate) settings: ParserSettings,
}
