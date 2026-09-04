//! Canonical generation-bound file-only retrieval records.

use std::collections::BTreeSet;

use rootlight_ids::{FileId, GenerationId, RepositoryId};
use rootlight_ir::{FileIdentityClaim, FileRecord};

/// One exact file record and the lossless path identity that derives it.
///
/// The presentation path is owned only by the file record. Identity claims are
/// reconstructed on demand so large file-only catalogs do not retain a second
/// copy of every repository-relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileCatalogEntry {
    file: FileRecord,
    path_identity: Vec<u8>,
}

impl SourceFileCatalogEntry {
    /// Validates a file-only record against its canonical identity claim.
    ///
    /// # Errors
    ///
    /// Returns [`SourceFileCatalogError`] when identity, source evidence, or
    /// retrieval metadata is incomplete or inconsistent.
    pub fn new(file: FileRecord, claim: FileIdentityClaim) -> Result<Self, SourceFileCatalogError> {
        if claim.derived_file() != claim.file
            || file.id != claim.file
            || file.repository != claim.repository
            || file.path != claim.path
            || file.content_hash != claim.content_hash
            || file.byte_length != claim.byte_length
        {
            return Err(SourceFileCatalogError::IdentityMismatch);
        }
        if file.path.is_empty()
            || file.language.is_empty()
            || file.encoding != "utf-8"
            || file.path_locator.is_none()
            || claim.path_identity.is_empty()
        {
            return Err(SourceFileCatalogError::IncompleteRetrievalMetadata);
        }
        let source = file
            .evidence
            .source
            .as_ref()
            .ok_or(SourceFileCatalogError::MissingSourceEvidence)?;
        if !file.evidence.derivation.is_empty()
            || source.repository() != file.repository
            || source.generation() != file.generation
            || source.span().file() != file.id
            || source.span().start_byte() != 0
            || source.span().end_byte() != file.byte_length
            || source.content_hash() != file.content_hash
        {
            return Err(SourceFileCatalogError::SourceEvidenceMismatch);
        }
        Ok(Self {
            file,
            path_identity: claim.path_identity,
        })
    }

    /// Returns the canonical file-only record.
    #[must_use]
    pub const fn file(&self) -> &FileRecord {
        &self.file
    }

    /// Returns the lossless platform path identity used to derive the file ID.
    #[must_use]
    pub fn path_identity(&self) -> &[u8] {
        &self.path_identity
    }

    /// Reconstructs the independent file-identity inputs.
    #[must_use]
    pub fn identity_claim(&self) -> FileIdentityClaim {
        FileIdentityClaim {
            file: self.file.id,
            repository: self.file.repository,
            path: self.file.path.clone(),
            path_identity: self.path_identity.clone(),
            content_hash: self.file.content_hash,
            byte_length: self.file.byte_length,
        }
    }

    /// Consumes the entry into its record and identity claim.
    #[must_use]
    pub fn into_parts(self) -> (FileRecord, FileIdentityClaim) {
        let claim = FileIdentityClaim {
            file: self.file.id,
            repository: self.file.repository,
            path: self.file.path.clone(),
            path_identity: self.path_identity,
            content_hash: self.file.content_hash,
            byte_length: self.file.byte_length,
        };
        (self.file, claim)
    }
}

/// A deterministic set of exact file-only retrieval records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFileCatalog {
    entries: Vec<SourceFileCatalogEntry>,
}

impl SourceFileCatalog {
    /// Canonicalizes file-only entries by stable file identity.
    ///
    /// # Errors
    ///
    /// Returns [`SourceFileCatalogError::DuplicateFile`] or
    /// [`SourceFileCatalogError::DuplicatePath`] when the input is ambiguous.
    pub fn new(mut entries: Vec<SourceFileCatalogEntry>) -> Result<Self, SourceFileCatalogError> {
        entries.sort_by_key(|entry| entry.file.id);
        if entries
            .windows(2)
            .any(|pair| pair[0].file.id == pair[1].file.id)
        {
            return Err(SourceFileCatalogError::DuplicateFile);
        }
        let mut paths = BTreeSet::new();
        for entry in &entries {
            if !paths.insert(entry.file.path.as_str()) {
                return Err(SourceFileCatalogError::DuplicatePath);
            }
        }
        Ok(Self { entries })
    }

    /// Returns the canonical entries in stable file-identity order.
    #[must_use]
    pub fn entries(&self) -> &[SourceFileCatalogEntry] {
        &self.entries
    }

    /// Finds one file-only record by stable identity.
    #[must_use]
    pub fn find(&self, file: FileId) -> Option<&FileRecord> {
        self.entries
            .binary_search_by_key(&file, |entry| entry.file.id)
            .ok()
            .and_then(|index| self.entries.get(index))
            .map(SourceFileCatalogEntry::file)
    }

    /// Returns the number of file-only records.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the catalog contains no records.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn validate_ownership(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<(), SourceFileCatalogError> {
        if self
            .entries
            .iter()
            .any(|entry| entry.file.repository != repository || entry.file.generation != generation)
        {
            return Err(SourceFileCatalogError::OwnershipMismatch);
        }
        Ok(())
    }
}

/// Invalid or ambiguous file-only retrieval catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceFileCatalogError {
    /// A record disagreed with the inputs that derive its stable identity.
    #[error("source file catalog identity does not match its canonical claim")]
    IdentityMismatch,
    /// A record lacked a lossless locator, language, path, or UTF-8 encoding.
    #[error("source file catalog retrieval metadata is incomplete")]
    IncompleteRetrievalMetadata,
    /// A record carried no direct immutable source reference.
    #[error("source file catalog record has no source evidence")]
    MissingSourceEvidence,
    /// Direct source evidence disagreed with the file record.
    #[error("source file catalog evidence does not match its file record")]
    SourceEvidenceMismatch,
    /// Two entries claimed one stable file identity.
    #[error("source file catalog contains a duplicate file identity")]
    DuplicateFile,
    /// Two entries claimed one presentation path.
    #[error("source file catalog contains a duplicate path")]
    DuplicatePath,
    /// A record belonged to another repository or generation.
    #[error("source file catalog ownership does not match its generation")]
    OwnershipMismatch,
    /// A catalog record collided with a normalized file identity or path.
    #[error("source file catalog overlaps the normalized file set")]
    NormalizedFileOverlap,
    /// A catalog record named no generation-owned provenance record.
    #[error("source file catalog provenance is unavailable")]
    MissingProvenance,
    /// A caller attempted to replace an already attached catalog.
    #[error("generation already carries a source file catalog")]
    AlreadyAttached,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{
        FactId, FileIdentity, GenerationId, content_hash, derive_file, derive_repository,
    };
    use rootlight_ir::{
        FactEvidence, FilePathLocator, FilePathLocatorEncoding, SourceRef, SourceSpan,
    };

    fn entry(path: &str, path_identity: &[u8]) -> SourceFileCatalogEntry {
        let repository = derive_repository(b"repository").id();
        let generation = GenerationId::from_bytes([7; 20]);
        let file = derive_file(FileIdentity {
            repository,
            path_identity,
        })
        .id();
        let bytes = 17;
        let content_hash = content_hash(path.as_bytes());
        let claim = FileIdentityClaim {
            file,
            repository,
            path: path.to_owned(),
            path_identity: path_identity.to_vec(),
            content_hash,
            byte_length: bytes,
        };
        let record = FileRecord {
            id: file,
            repository,
            generation,
            path: path.to_owned(),
            path_locator: Some(
                FilePathLocator::new(FilePathLocatorEncoding::UnixBytesV1, vec!["61".to_owned()])
                    .expect("fixture locator is valid"),
            ),
            content_hash,
            byte_length: bytes,
            language: "text".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([9; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(file, 0, bytes).expect("fixture span is valid"),
                    content_hash,
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        SourceFileCatalogEntry::new(record, claim).expect("fixture entry is valid")
    }

    #[test]
    fn catalog_canonicalizes_entries_and_rejects_ambiguous_paths() {
        let first = entry("src/first.txt", b"src/first.txt");
        let second = entry("src/second.txt", b"src/second.txt");
        let expected = [first.file().id, second.file().id]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let catalog = SourceFileCatalog::new(vec![second, first]).expect("catalog is valid");
        assert_eq!(
            catalog
                .entries()
                .iter()
                .map(|entry| entry.file().id)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(catalog.entries().is_sorted_by_key(|entry| entry.file().id));

        let duplicate_path = entry("src/shared.txt", b"identity/one");
        let other_identity = entry("src/shared.txt", b"identity/two");
        assert_eq!(
            SourceFileCatalog::new(vec![duplicate_path, other_identity]),
            Err(SourceFileCatalogError::DuplicatePath)
        );
    }

    #[test]
    fn entry_reconstructs_the_exact_identity_claim() {
        let entry = entry("src/lib.txt", b"src/lib.txt");
        let file = entry.file().clone();
        let expected = FileIdentityClaim {
            file: file.id,
            repository: file.repository,
            path: file.path.clone(),
            path_identity: b"src/lib.txt".to_vec(),
            content_hash: file.content_hash,
            byte_length: file.byte_length,
        };

        assert_eq!(entry.path_identity(), expected.path_identity);
        assert_eq!(entry.identity_claim(), expected);
        assert_eq!(entry.into_parts(), (file, expected));
    }

    #[test]
    fn entry_requires_exact_full_file_source_evidence() {
        let valid = entry("src/lib.txt", b"src/lib.txt");
        let (mut file, claim) = valid.into_parts();
        file.evidence.source = Some(SourceRef::new(
            file.repository,
            file.generation,
            SourceSpan::new(file.id, 1, file.byte_length).expect("fixture span is valid"),
            file.content_hash,
            None,
        ));

        assert_eq!(
            SourceFileCatalogEntry::new(file, claim),
            Err(SourceFileCatalogError::SourceEvidenceMismatch)
        );
    }
}
