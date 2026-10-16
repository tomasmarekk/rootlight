//! Generation-authorized source access across snapshots and private streamed blobs.
//!
//! The caller retains a generation lease throughout each visit. Callback errors
//! and private authentication failures cannot expose a provisional public response.

use std::{borrow::Cow, fmt, sync::Arc};

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{FileRecord, SourceRef};
use rootlight_query::GenerationLease;
use rootlight_search::LexicalIndex;
use rootlight_source::{SourceError, SourceService, VerifiedSourceBacking};
use rootlight_storage::GenerationSnapshot;
use rootlight_vfs::{SourceSnapshot, platform::PlatformError};

use crate::{FirstSliceError, FirstSliceService, map_source_error};

/// Visits ordered physical bytes through the private VFS error boundary.
pub(super) type VerifiedSourceVisitor<'a> = dyn FnMut(u64, &[u8]) -> Result<(), PlatformError> + 'a;

/// A query lease also retains ephemeral source bytes after logical eviction.
pub(super) struct ServiceGenerationLease {
    query: GenerationLease<LexicalIndex>,
    _sources: Option<Arc<crate::source_owner::CapturedGenerationSources>>,
}

impl ServiceGenerationLease {
    pub(super) fn new(
        query: GenerationLease<LexicalIndex>,
        sources: Option<Arc<crate::source_owner::CapturedGenerationSources>>,
    ) -> Self {
        Self {
            query,
            _sources: sources,
        }
    }
}

impl std::ops::Deref for ServiceGenerationLease {
    type Target = GenerationLease<LexicalIndex>;

    fn deref(&self) -> &Self::Target {
        &self.query
    }
}

pub(super) enum ResolvedSources<'a> {
    Snapshots(Cow<'a, [Arc<SourceSnapshot>]>),
    Streamed(GenerationBacking<'a>),
}

impl ResolvedSources<'_> {
    pub(super) fn service<'a>(
        &'a self,
        generation: &'a GenerationSnapshot,
        cancellation: &Cancellation,
    ) -> Result<SourceService<'a>, FirstSliceError> {
        match self {
            Self::Snapshots(snapshots) => SourceService::from_snapshots(snapshots, generation),
            Self::Streamed(backing) => SourceService::from_verified_backing(backing, generation),
        }
        .map_err(|error| map_source_error(error, cancellation))
    }
}

pub(super) struct GenerationBacking<'a> {
    service: &'a FirstSliceService,
    generation: &'a GenerationSnapshot,
}

impl fmt::Debug for GenerationBacking<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationBacking")
            .field("repository", &self.repository())
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl FirstSliceService {
    pub(super) fn resolve_source_backing<'a>(
        &'a self,
        generation: &'a GenerationSnapshot,
        references: &[SourceRef],
        cancellation: &Cancellation,
    ) -> Result<ResolvedSources<'a>, FirstSliceError> {
        let identity = generation.metadata().generation();
        let captured = self.streamed_sources.get(&identity);
        let snapshot_limit = self
            .config
            .analysis()
            .max_source_file_bytes
            .min(rootlight_vfs::MAX_SNAPSHOT_BYTES);
        let streamed = references.iter().any(|reference| {
            generation
                .find_file(reference.span().file())
                .is_some_and(|file| {
                    file.byte_length > snapshot_limit
                        || file.encoding == "bytes"
                        || captured.is_some_and(|sources| sources.contains(file.id))
                })
        });
        if streamed {
            Ok(ResolvedSources::Streamed(GenerationBacking {
                service: self,
                generation,
            }))
        } else {
            self.source_snapshots_for_read(identity, generation, references, cancellation)
                .map(ResolvedSources::Snapshots)
        }
    }
}

impl VerifiedSourceBacking for GenerationBacking<'_> {
    fn repository(&self) -> RepositoryId {
        self.generation.metadata().repository()
    }
    fn generation(&self) -> GenerationId {
        self.generation.metadata().generation()
    }

    fn visit_verified(
        &self,
        file: &FileRecord,
        maximum_verified_bytes: u64,
        cancellation: &Cancellation,
        visitor: &mut dyn FnMut(u64, &[u8]) -> Result<(), SourceError>,
    ) -> Result<u64, SourceError> {
        if file.repository != self.repository()
            || file.generation != self.generation()
            || self.generation.find_file(file.id) != Some(file)
        {
            return Err(SourceError::StaleSource);
        }
        if file.byte_length > maximum_verified_bytes {
            return Err(SourceError::VerificationBudgetExceeded);
        }
        cancellation
            .check()
            .map_err(|cancelled| SourceError::Cancelled(cancelled.reason()))?;
        let mut callback_error = None;
        let mut callback = |offset, bytes: &[u8]| {
            if callback_error.is_some() {
                return Err(PlatformError::SecurityPolicy);
            }
            visitor(offset, bytes).map_err(|error| {
                callback_error = Some(error);
                PlatformError::SecurityPolicy
            })
        };
        let result = if let Some(owner) = self.service.streamed_sources.get(&self.generation())
            && owner.contains(file.id)
        {
            owner.source(file).and_then(|_| {
                owner.visit(file.id, maximum_verified_bytes, cancellation, &mut callback)
            })
        } else if let Some(snapshot) = self
            .service
            .source_snapshots
            .snapshots(self.generation())
            .and_then(|sources| sources.iter().find(|source| source.file() == file.id))
        {
            if snapshot.content_hash() != file.content_hash
                || snapshot.metadata().length != file.byte_length
                || snapshot.path().as_str() != file.path
            {
                return Err(SourceError::StaleSource);
            }
            let mut count = 0u64;
            for bytes in snapshot.content().chunks(64 * 1024) {
                cancellation
                    .check()
                    .map_err(|cancelled| SourceError::Cancelled(cancelled.reason()))?;
                if callback(count, bytes).is_err() {
                    break;
                }
                count = count
                    .checked_add(
                        u64::try_from(bytes.len()).map_err(|_| SourceError::InvalidSourceSpan)?,
                    )
                    .ok_or(SourceError::InvalidSourceSpan)?;
            }
            if count == file.byte_length {
                Ok(count)
            } else {
                Err(FirstSliceError::CatalogCorrupt)
            }
        } else if let Some(durable) = &self.service.durable {
            durable.visit_source(file, maximum_verified_bytes, cancellation, &mut callback)
        } else {
            Err(FirstSliceError::Query)
        };
        if let Some(error) = callback_error {
            return Err(error);
        }
        cancellation
            .check()
            .map_err(|cancelled| SourceError::Cancelled(cancelled.reason()))?;
        result.map_err(|_| SourceError::StaleSource)
    }
}
