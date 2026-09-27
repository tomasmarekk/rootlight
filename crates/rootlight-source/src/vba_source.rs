//! Lazy, non-executing extraction of declared VBA module source bytes.
//! Bindings are confined to one caller-selected CFB storage; decoded bytes retain
//! directory, compression and physical-stream provenance without inventing UTF-8.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    ops::Range,
};

use rootlight_cancel::{Cancellation, Cancelled};

use crate::{
    compound::{CompoundEntry, CompoundEntryKind, CompoundError, CompoundFile, CompoundStream},
    vba_compression::{self, DecompressedContainer, DecompressionError, DecompressionLimits},
    vba_dir::{DirectoryError, DirectoryLimits, ProjectDirectory, ReferenceRecord},
};

pub mod root;

#[cfg(test)]
mod tests;

/// Independent compressed-stream and expanded-directory limits for preparation.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct ProjectReadLimits {
    /// Compressed directory bytes, decoded bytes and decoding dependencies.
    pub compression: DecompressionLimits,
    /// Directory metadata input, reference count and module count boundaries.
    pub directory: DirectoryLimits,
}

/// Limits for one lazy source read; callers separately bound aggregate retention.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ModuleReadLimits {
    /// Maximum full stream bytes, including the ignored performance cache.
    pub stream_bytes: usize,
    /// Maximum compressed source bytes, expanded source bytes and chunks.
    pub compression: DecompressionLimits,
}

impl Default for ModuleReadLimits {
    fn default() -> Self {
        Self {
            stream_bytes: 16 * 1024 * 1024,
            compression: DecompressionLimits::default(),
        }
    }
}

/// A declaration that cannot identify one distinct stream in the selected storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BindingError {
    /// The declared Unicode name is empty or exceeds a CFB name's 31 code units.
    #[error("vba module stream name is not a compound stream name")]
    Name,
    /// No direct child has the exact declared Unicode name.
    #[error("vba module stream is missing")]
    Missing,
    /// More than one direct child has the exact declared Unicode name.
    #[error("vba module stream name is ambiguous")]
    Ambiguous,
    /// The exact named child is a storage, not a byte stream.
    #[error("vba module target is not a stream")]
    NotStream,
    /// The module refers to the directory or reserved version/cache stream.
    #[error("vba module target is the metadata stream")]
    Metadata,
    /// Multiple declarations refer to the same physical stream.
    #[error("vba module stream is reused by another declaration")]
    Reused,
}

/// Preparation or per-module extraction failure without source payloads.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// The selected directory ID is absent or is not a named VBA storage.
    #[error("selected compound entry is not a vba storage")]
    Storage,
    /// The selected storage does not contain exactly one `dir` stream.
    #[error("vba directory stream is missing or ambiguous")]
    Metadata,
    /// The enclosing project has no unique PROJECT sibling stream.
    #[error("vba project properties stream is missing or ambiguous")]
    ProjectMetadata,
    /// The requested ordinal is outside the complete declared module list.
    #[error("vba module ordinal is out of bounds: {ordinal}")]
    Ordinal {
        /// Zero-based ordinal in this project's directory metadata.
        ordinal: usize,
    },
    /// The selected module has no unique bound stream.
    #[error("vba module {ordinal} cannot bind its source stream: {source}")]
    Binding {
        /// Zero-based declaration ordinal, not a corpus identifier.
        ordinal: usize,
        /// Exact binding failure retained independently for this declaration.
        #[source]
        source: BindingError,
    },
    /// The source offset leaves no compressed container in the selected stream.
    #[error("vba module {ordinal} source offset exceeds its stream")]
    Offset {
        /// Zero-based declaration ordinal.
        ordinal: usize,
    },
    /// A stream ceiling exceeds the existing source snapshot hard limit.
    #[error("invalid vba source limits")]
    Limits,
    /// The CFB stream could not be read within its own bounds.
    #[error(transparent)]
    Compound(#[from] CompoundError),
    /// The compressed directory or module body is malformed or exceeds a limit.
    #[error(transparent)]
    Compression(#[from] DecompressionError),
    /// Decompressed project metadata is malformed or exceeds a limit.
    #[error(transparent)]
    Directory(#[from] DirectoryError),
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// A declared module and its exact, storage-confined stream binding outcome.
#[derive(Debug, Clone)]
pub struct ModuleDescriptor {
    declaration: Range<usize>,
    offset: u32,
    stream: Result<BoundStream, BindingError>,
}

#[derive(Debug, Clone)]
struct BoundStream {
    id: u32,
    descriptor: Range<usize>,
    length: usize,
}

impl ModuleDescriptor {
    /// Returns the complete record range in the project's decoded directory.
    #[must_use]
    pub fn declaration_range(&self) -> Range<usize> {
        self.declaration.clone()
    }
    /// Returns the original source offset, not yet validated against stream size.
    #[must_use]
    pub const fn source_offset(&self) -> u32 {
        self.offset
    }
    /// Returns the bound CFB entry ID, never a repository-wide source identity.
    ///
    /// # Errors
    /// Returns the recorded binding failure for this declaration. Other modules
    /// remain independently readable; failures are not converted to empty sources.
    pub fn stream_id(&self) -> Result<u32, BindingError> {
        self.stream
            .as_ref()
            .map(|stream| stream.id)
            .map_err(|error| *error)
    }
}

/// One raw MBCS module body and its complete physical and decoding dependencies.
#[derive(Debug)]
pub struct ModuleSource {
    code_page: u16,
    stream_id: u32,
    descriptor: Range<usize>,
    source_offset: usize,
    encoded: CompoundStream,
    decoded: DecompressedContainer,
}

impl ModuleSource {
    /// Decodes with the codepage retained from this module's own project metadata.
    ///
    /// Returned original offsets address [`Self::bytes`]; compression and CFB
    /// mappings remain on this module for the next provenance composition step.
    ///
    /// # Errors
    /// Returns a typed decoding/limit/cancellation error, without losing access
    /// to the raw module bytes when its codepage is unsupported or malformed.
    pub fn decode_text(
        &self,
        limits: crate::vba_text::TextLimits,
        cancellation: &Cancellation,
    ) -> Result<crate::vba_text::DecodedText<'_>, crate::vba_text::TextError> {
        crate::vba_text::DecodedText::decode(self.bytes(), self.code_page, limits, cancellation)
    }

    /// Returns exact expanded MBCS bytes; the project supplies their code page.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.decoded.bytes()
    }
    /// Returns the bound CFB directory ID in the retained compound container.
    #[must_use]
    pub const fn stream_id(&self) -> u32 {
        self.stream_id
    }
    /// Returns the physical CFB descriptor range establishing stream identity.
    #[must_use]
    pub fn descriptor_range(&self) -> Range<usize> {
        self.descriptor.clone()
    }
    /// Returns the complete encoded stream with exact physical byte mappings.
    /// The prefix before [`Self::source_offset`] is an ignored performance cache.
    #[must_use]
    pub fn encoded_stream(&self) -> &CompoundStream {
        &self.encoded
    }
    /// Returns the start of the compressed container in the encoded stream.
    #[must_use]
    pub const fn source_offset(&self) -> usize {
        self.source_offset
    }
    /// Returns decoding dependencies relative to the compressed source container.
    /// Add [`Self::source_offset`] to each encoded range before applying the
    /// physical mappings from [`Self::encoded_stream`]; these are not 1:1 mappings.
    #[must_use]
    pub fn decoded(&self) -> &DecompressedContainer {
        &self.decoded
    }
}

/// A prepared module inventory bound to one immutable CFB container and storage.
/// Bodies are read separately, so one invalid module cannot suppress another.
pub struct VbaProject<'container, 'input> {
    compound: &'container CompoundFile<'input>,
    storage_id: u32,
    directory_id: u32,
    encoded_directory: CompoundStream,
    decoded_directory: DecompressedContainer,
    code_page: u16,
    references: Vec<ReferenceRecord>,
    modules: Vec<ModuleDescriptor>,
}

impl fmt::Debug for VbaProject<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VbaProject")
            .field("storage_id", &self.storage_id)
            .field("directory_id", &self.directory_id)
            .field("code_page", &self.code_page)
            .field("references", &self.references.len())
            .field("modules", &self.modules.len())
            .finish()
    }
}

impl<'container, 'input> VbaProject<'container, 'input> {
    /// Opens the caller-selected `VBA` storage and binds its declared modules.
    ///
    /// Reserved ASCII names `VBA` and `dir` ignore case, as MS-OVBA requires.
    /// Module streams still use exact Unicode names among direct children; this
    /// does not perform general CFB collation, MBCS/Unicode equivalence, PROJECT
    /// membership or VBA semantic validation. Invalid bindings stay explicit.
    /// External references and performance caches are never executed.
    ///
    /// # Errors
    /// Returns [`SourceError`] for invalid storage selection, missing/ambiguous
    /// metadata, directory/decompression limits, malformed metadata or cancellation.
    pub fn open(
        compound: &'container CompoundFile<'input>,
        storage_id: u32,
        limits: ProjectReadLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, SourceError> {
        cancellation.check()?;
        let storage = compound
            .entries()
            .find(|entry| entry.id() == storage_id)
            .filter(|entry| {
                entry.kind() == CompoundEntryKind::Storage
                    && entry.name().eq_ignore_ascii_case("VBA")
            })
            .ok_or(SourceError::Storage)?;
        let mut children: BTreeMap<&str, Option<&CompoundEntry>> = BTreeMap::new();
        let mut directory = None;
        for entry in compound.entries() {
            cancellation.check()?;
            if entry.parent() == Some(storage.id()) {
                if entry.name().eq_ignore_ascii_case("dir") {
                    if directory.is_some() || entry.kind() != CompoundEntryKind::Stream {
                        return Err(SourceError::Metadata);
                    }
                    directory = Some(entry);
                }
                children
                    .entry(entry.name())
                    .and_modify(|value| *value = None)
                    .or_insert(Some(entry));
            }
        }
        let directory = directory.ok_or(SourceError::Metadata)?;
        let encoded_directory =
            compound.read_stream(directory.id(), limits.compression.input_bytes, cancellation)?;
        let decoded_directory = vba_compression::decompress(
            encoded_directory.bytes(),
            limits.compression,
            cancellation,
        )?;
        let metadata =
            ProjectDirectory::parse(decoded_directory.bytes(), limits.directory, cancellation)?;
        let code_page = metadata.code_page();
        let mut modules = Vec::new();
        let mut first_use = BTreeSet::new();
        let mut reused = BTreeSet::new();
        for record in metadata.modules() {
            cancellation.check()?;
            let stream = bind_name(
                record.unicode_stream_name().bytes(),
                &children,
                directory.id(),
            );
            if let Ok(binding) = &stream
                && !first_use.insert(binding.id)
            {
                reused.insert(binding.id);
            }
            modules.push(ModuleDescriptor {
                declaration: record.range(),
                offset: record.text_offset(),
                stream,
            });
        }
        // Every claimant is invalidated, not only the later declaration: record
        // order must not pick a winner for the same physical source identity.
        for module in &mut modules {
            cancellation.check()?;
            if module
                .stream
                .as_ref()
                .is_ok_and(|binding| reused.contains(&binding.id))
            {
                module.stream = Err(BindingError::Reused);
            }
        }
        let references = metadata.into_references();
        Ok(Self {
            compound,
            storage_id,
            directory_id: directory.id(),
            encoded_directory,
            decoded_directory,
            code_page,
            references,
            modules,
        })
    }

    /// Returns all declared modules, including explicit failures, in record order.
    #[must_use]
    pub fn modules(&self) -> &[ModuleDescriptor] {
        &self.modules
    }
    /// Returns untrusted reference metadata in declaration order, without loading it.
    /// All field ranges address [`Self::decoded_directory`]'s bytes; they do not
    /// establish library availability, identity equivalence or symbol resolution.
    #[must_use]
    pub fn references(&self) -> &[ReferenceRecord] {
        &self.references
    }
    /// Returns the original project code page without guessing an encoding.
    #[must_use]
    pub const fn code_page(&self) -> u16 {
        self.code_page
    }
    /// Returns the caller-selected CFB storage identity, not a global project ID.
    #[must_use]
    pub const fn storage_id(&self) -> u32 {
        self.storage_id
    }
    /// Returns the CFB stream identity containing this project's metadata.
    #[must_use]
    pub const fn directory_id(&self) -> u32 {
        self.directory_id
    }
    /// Returns original directory stream bytes and physical source mappings.
    #[must_use]
    pub fn encoded_directory(&self) -> &CompoundStream {
        &self.encoded_directory
    }
    /// Returns decompressed metadata and its compressed chunk dependencies.
    #[must_use]
    pub fn decoded_directory(&self) -> &DecompressedContainer {
        &self.decoded_directory
    }

    /// Reads the unique PROJECT stream beside the selected VBA storage.
    ///
    /// The reserved ASCII name ignores case; all matching siblings participate
    /// in ambiguity detection, independently of how their names are capitalized.
    /// The returned physical mappings are original container byte ranges. Parsing
    /// with [`crate::vba_project::ProjectProperties`] is separate so malformed or
    /// absent project properties do not suppress independently readable bodies.
    ///
    /// # Errors
    /// Returns [`SourceError::ProjectMetadata`] for an absent, ambiguous or
    /// non-stream sibling; returns a compound limit/read error or cancellation.
    pub fn read_project_stream(
        &self,
        maximum_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<CompoundStream, SourceError> {
        cancellation.check()?;
        let parent = self
            .compound
            .entries()
            .find(|entry| entry.id() == self.storage_id)
            .and_then(CompoundEntry::parent)
            .ok_or(SourceError::ProjectMetadata)?;
        let mut selected = None;
        for entry in self.compound.entries() {
            cancellation.check()?;
            if entry.parent() == Some(parent) && entry.name().eq_ignore_ascii_case("PROJECT") {
                if selected.is_some() || entry.kind() != CompoundEntryKind::Stream {
                    return Err(SourceError::ProjectMetadata);
                }
                selected = Some(entry.id());
            }
        }
        let id = selected.ok_or(SourceError::ProjectMetadata)?;
        Ok(self.compound.read_stream(id, maximum_bytes, cancellation)?)
    }

    /// Reads one module's declared source bytes without decoding its code page.
    ///
    /// No partial or empty success substitutes for a failed read. Other ordinals
    /// can still be read. Limits apply to this call; consumers retaining multiple
    /// returned bodies must additionally bound their aggregate bytes and metadata.
    ///
    /// # Errors
    /// Returns [`SourceError`] for an invalid ordinal/binding/offset, compressed
    /// source failure, exceeded input/output/chunk bounds, or cancellation.
    pub fn read_module(
        &self,
        ordinal: usize,
        limits: ModuleReadLimits,
        cancellation: &Cancellation,
    ) -> Result<ModuleSource, SourceError> {
        cancellation.check()?;
        if u64::try_from(limits.stream_bytes).map_err(|_| SourceError::Limits)?
            > crate::HARD_MAX_SNAPSHOT_BYTES
        {
            return Err(SourceError::Limits);
        }
        let module = self
            .modules
            .get(ordinal)
            .ok_or(SourceError::Ordinal { ordinal })?;
        let binding = module
            .stream
            .as_ref()
            .map_err(|source| SourceError::Binding {
                ordinal,
                source: *source,
            })?;
        let stream_id = binding.id;
        let source_offset =
            usize::try_from(module.offset).map_err(|_| SourceError::Offset { ordinal })?;
        if source_offset >= binding.length {
            return Err(SourceError::Offset { ordinal });
        }
        let encoded = self
            .compound
            .read_stream(stream_id, limits.stream_bytes, cancellation)?;
        let compressed = encoded
            .bytes()
            .get(source_offset..)
            .ok_or(SourceError::Offset { ordinal })?;
        let decoded = vba_compression::decompress(compressed, limits.compression, cancellation)?;
        Ok(ModuleSource {
            code_page: self.code_page,
            stream_id,
            descriptor: binding.descriptor.clone(),
            source_offset,
            encoded,
            decoded,
        })
    }
}

fn bind_name(
    bytes: &[u8],
    children: &BTreeMap<&str, Option<&CompoundEntry>>,
    directory_id: u32,
) -> Result<BoundStream, BindingError> {
    if bytes.is_empty() || bytes.len() > 62 {
        return Err(BindingError::Name);
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let name = String::from_utf16(&units).map_err(|_| BindingError::Name)?;
    let entry = children
        .get(name.as_str())
        .ok_or(BindingError::Missing)?
        .ok_or(BindingError::Ambiguous)?;
    if entry.kind() != CompoundEntryKind::Stream {
        return Err(BindingError::NotStream);
    }
    // Version-dependent cached code is never an authoritative module source,
    // even when its bytes happen to parse as a compressed source container.
    if entry.id() == directory_id || entry.name().eq_ignore_ascii_case("_VBA_PROJECT") {
        return Err(BindingError::Metadata);
    }
    Ok(BoundStream {
        id: entry.id(),
        descriptor: entry.directory_range(),
        length: entry.byte_length(),
    })
}
