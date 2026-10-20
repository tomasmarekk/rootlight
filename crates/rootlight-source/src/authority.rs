//! Source-only publication authority, separate from a normalized semantic graph.
//! Every file recipe and complete origin dependency is checked before a source
//! lease is exposed; durable codecs own their separate integrity boundary.

use rootlight_ids::FileId;
use rootlight_ir::{FileRecord, SourceRef};
use rootlight_storage::{
    GenerationContext, GenerationManifestRecipe, GenerationMetadata, GenerationResource,
    SourceFileCatalog, SourceFileCatalogEntry,
};
use rootlight_vfs::RelativePath;

/// Exact source metadata authenticated independently of semantic graph residency.
/// This capability offers no semantic records or graph-completeness claim.
pub struct VerifiedSourceAuthority {
    metadata: GenerationMetadata,
    files: SourceFileCatalog,
    origins: Vec<(SourceRef, SourceRef)>,
    recovery: Option<rootlight_ir::AggregateSourceRecovery>,
}

/// A compact source artifact failed identity, control, allocation or encoding checks.
#[derive(Debug, thiserror::Error)]
pub enum SourceAuthorityError {
    /// The artifact or one of its source identities is inconsistent.
    #[error("invalid source authority")]
    Invalid,
    /// Existing generation control rejected the operation.
    #[error(transparent)]
    Control(#[from] rootlight_storage::GenerationControlError),
    /// A checked allocation could not be reserved.
    #[error("source authority memory unavailable")]
    Memory,
    /// The artifact could not be completely written.
    #[error("source authority encoding failed")]
    Encoding,
}

impl VerifiedSourceAuthority {
    /// Verifies every file identity, manifest member and whole-source origin.
    ///
    /// The caller admits encoded/owned workspace before constructing entries.
    /// # Errors
    /// Rejects identity, source-path, origin-cycle, cancellation and resource failures.
    pub fn verify(
        metadata: GenerationMetadata,
        entries: Vec<SourceFileCatalogEntry>,
        mut origins: Vec<(SourceRef, SourceRef)>,
        recovery: Option<rootlight_ir::AggregateSourceRecovery>,
        context: &GenerationContext<'_>,
    ) -> Result<Self, SourceAuthorityError> {
        if origins.len() > entries.len() {
            return Err(SourceAuthorityError::Invalid);
        }
        context.require(
            GenerationResource::Rows,
            u64::try_from(origins.len()).map_err(|_| SourceAuthorityError::Invalid)?,
        )?;
        let repository = metadata.repository();
        let generation = metadata.generation();
        context.require(
            GenerationResource::Rows,
            u64::try_from(entries.len()).map_err(|_| SourceAuthorityError::Invalid)?,
        )?;
        let mut claims = Vec::new();
        claims
            .try_reserve_exact(entries.len())
            .map_err(|_| SourceAuthorityError::Memory)?;
        for entry in &entries {
            context.check()?;
            let file = entry.file();
            let claim = entry.identity_claim();
            let relative = RelativePath::from_identity_bytes(&claim.path_identity)
                .map_err(|_| SourceAuthorityError::Invalid)?;
            if claim.repository != repository
                || file.generation != generation
                || file.path != relative.as_str()
                || file.path_locator.as_ref() != Some(&relative.to_locator())
            {
                return Err(SourceAuthorityError::Invalid);
            }
            claims.push(claim);
        }
        let recipe =
            GenerationManifestRecipe::new(repository, metadata.configuration_hash(), claims)
                .map_err(|_| SourceAuthorityError::Invalid)?;
        context.check()?;
        if recipe
            .canonical_hash()
            .map_err(|_| SourceAuthorityError::Invalid)?
            != metadata.manifest_hash()
        {
            return Err(SourceAuthorityError::Invalid);
        }
        let files = SourceFileCatalog::new(entries).map_err(|_| SourceAuthorityError::Invalid)?;
        origins.sort_by_key(|(derived, _)| derived.span().file());
        for (index, (derived, container)) in origins.iter().enumerate() {
            context.check()?;
            let exact = |source: &SourceRef| {
                files.find(source.span().file()).is_some_and(|file| {
                    source.repository() == repository
                        && source.generation() == generation
                        && source.span().start_byte() == 0
                        && source.span().end_byte() == file.byte_length
                        && source.content_hash() == file.content_hash
                        && file.evidence.source.as_ref() == Some(source)
                })
            };
            if !exact(derived)
                || !exact(container)
                || derived.span().file() == container.span().file()
                || index > 0 && origins[index - 1].0.span().file() == derived.span().file()
            {
                return Err(SourceAuthorityError::Invalid);
            }
        }
        let mut colors = Vec::new();
        colors
            .try_reserve_exact(origins.len())
            .map_err(|_| SourceAuthorityError::Memory)?;
        colors.resize(origins.len(), 0_u8);
        let mut stack = Vec::new();
        stack
            .try_reserve_exact(origins.len())
            .map_err(|_| SourceAuthorityError::Memory)?;
        for start in 0..origins.len() {
            let mut current = Some(start);
            while let Some(index) = current {
                context.check()?;
                match colors[index] {
                    2 => break,
                    1 => return Err(SourceAuthorityError::Invalid),
                    _ => {}
                }
                colors[index] = 1;
                stack.push(index);
                current = origins
                    .binary_search_by_key(&origins[index].1.span().file(), |(from, _)| {
                        from.span().file()
                    })
                    .ok();
            }
            for index in stack.drain(..) {
                colors[index] = 2;
            }
        }
        if recovery.is_some_and(|cause| {
            !cause.is_valid()
                || cause.source_inventory != metadata.manifest_hash()
                || cause.members != files.len() as u64
                || cause.derived_members != origins.len() as u64
        }) {
            return Err(SourceAuthorityError::Invalid);
        }
        context.check()?;
        Ok(Self {
            metadata,
            files,
            origins,
            recovery,
        })
    }

    /// Returns the authenticated aggregate source-retention cause, when present.
    #[must_use]
    pub const fn recovery(&self) -> Option<rootlight_ir::AggregateSourceRecovery> {
        self.recovery
    }

    /// Returns the source generation's exact immutable identity.
    #[must_use]
    pub const fn metadata(&self) -> GenerationMetadata {
        self.metadata
    }

    /// Resolves only authenticated file records in this immutable source set.
    #[must_use]
    pub fn find_file(&self, file: FileId) -> Option<&FileRecord> {
        self.files.find(file)
    }

    /// Returns the number of completely authenticated physical and derived files.
    #[must_use]
    pub const fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns one immediate whole-file origin, preserving its exact source identity.
    #[must_use]
    pub fn origin(&self, file: FileId) -> Option<&SourceRef> {
        self.origins
            .binary_search_by_key(&file, |(from, _)| from.span().file())
            .ok()
            .map(|i| &self.origins[i].1)
    }

    /// Returns the number of distinct authenticated immediate origin links.
    #[must_use]
    pub const fn origin_count(&self) -> usize {
        self.origins.len()
    }
}
