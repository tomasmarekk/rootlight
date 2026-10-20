//! Stage-owned fresh projection refusal; discarded graph facts stay unverified.

use super::*;

pub(super) struct Context<'a> {
    pub(super) inputs: &'a [ManifestInput],
    pub(super) claims: &'a [FileIdentityClaim],
    pub(super) derived: &'a container_sources::PreparedSources,
    pub(super) languages: &'a BTreeMap<FileId, String>,
    pub(super) sources: &'a [RustSourceInput],
}

pub(super) fn eligible(
    service: &FirstSliceService,
    mode: FirstSliceIndexMode,
    context: Context<'_>,
    cancellation: &Cancellation,
) -> Result<bool, FirstSliceError> {
    check_cancellation(cancellation)?;
    if mode != FirstSliceIndexMode::Structural
        || service.generation_budget != GenerationBudget::default()
        || service.analysis_limits != analysis_limits(service.analysis_limits.max_source_bytes())?
        || !context.derived.inputs.is_empty()
        || !context.derived.source_origins().is_empty()
        || !context.derived.native_projects.is_empty()
        || context.inputs.is_empty()
        || context.inputs.len() != context.claims.len()
    {
        return Ok(false);
    }
    for input in context.inputs {
        check_cancellation(cancellation)?;
        if detected_source_language(input) == Some("office-open-xml") {
            return Ok(false);
        }
    }
    for claim in context.claims {
        check_cancellation(cancellation)?;
        if claim
            .path
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("vbp"))
        {
            return Ok(false);
        }
    }
    // Generated classification is exact file metadata, not an origin claim.
    // Real generated-to-authored mappings cannot be lost with the discarded graph.
    for source in context.sources {
        check_cancellation(cancellation)?;
        if !source.origins.is_empty() {
            return Ok(false);
        }
    }
    for language in context.languages.values() {
        check_cancellation(cancellation)?;
        let (Some(analyzer), Some(parser)) = (
            service.analyzers.get(language),
            service.structural_parsers.get(language),
        ) else {
            return Ok(false);
        };
        // Source recovery does not reuse graph identity evidence. Its separate
        // built-in capability must still belong to this exact analyzer instance.
        if !parser.supports_fresh_source_preservation()
            || !analyzer.uses_parse_provider(parser.as_ref())
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    DocumentFootprint,
    FullProjectionPeak,
}

/// Issued only at a fresh projection's intrinsic own-byte boundary.
/// It authenticates no graph identity or analysis fact.
#[derive(Debug, Clone, Copy)]
pub(super) struct FreshProjectionCapacity {
    repository: RepositoryId,
    generation: GenerationId,
    manifest: ContentHash,
    observed: u64,
    limit: u64,
    stage: Stage,
}

impl FreshProjectionCapacity {
    pub(super) fn from_actual_peak(
        repository: RepositoryId,
        generation: GenerationId,
        manifest: ContentHash,
        observed: u64,
    ) -> Option<Self> {
        Self::from_own_bytes(
            repository,
            generation,
            manifest,
            observed,
            Stage::FullProjectionPeak,
        )
    }

    pub(super) fn from_document_footprint(
        repository: RepositoryId,
        generation: GenerationId,
        manifest: ContentHash,
        observed: u64,
    ) -> Option<Self> {
        Self::from_own_bytes(
            repository,
            generation,
            manifest,
            observed,
            Stage::DocumentFootprint,
        )
    }

    fn from_own_bytes(
        repository: RepositoryId,
        generation: GenerationId,
        manifest: ContentHash,
        observed: u64,
        stage: Stage,
    ) -> Option<Self> {
        let limit = generation_memory_ceiling();
        (observed > limit && observed != u64::MAX).then_some(Self {
            repository,
            generation,
            manifest,
            observed,
            limit,
            stage,
        })
    }

    pub(super) fn error(self) -> FirstSliceError {
        let mut error = generation_memory_limit(0, self.observed, PendingGenerationMemory::Staged);
        if let FirstSliceError::GenerationMemoryLimit { limit, .. } = &mut error {
            *limit = self.limit;
        }
        error
    }

    pub(super) fn matches(
        self,
        repository: RepositoryId,
        generation: GenerationId,
        manifest: ContentHash,
        error: &FirstSliceError,
    ) -> bool {
        self.repository == repository
            && self.generation == generation
            && self.manifest == manifest
            && self.limit == generation_memory_ceiling()
            && *error == self.error()
    }

    pub(super) fn annotate(
        self,
        document: &mut NormalizedIrDocument,
        manifest: ContentHash,
    ) -> Result<(), FirstSliceError> {
        if document.repository != self.repository
            || document.generation != self.generation
            || manifest != self.manifest
            || !document.entities.is_empty()
            || !document.occurrences.is_empty()
            || !document.relations.is_empty()
        {
            return Err(FirstSliceError::Identity);
        }
        let diagnostic = document
            .diagnostics
            .iter_mut()
            .find(|record| record.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
            .ok_or(FirstSliceError::Identity)?;
        let stage = match self.stage {
            Stage::DocumentFootprint => "document and lexical coverage",
            Stage::FullProjectionPeak => "full projection peak",
        };
        diagnostic.message = format!(
            "fresh projection capacity ({stage}) requires {} modeled bytes above limit {}; this prevented graph verification and analysis; exact source retrieval remains available with unknown analysis coverage",
            self.observed, self.limit,
        );
        diagnostic.id =
            derive_diagnostic_record_id(diagnostic).map_err(|_| FirstSliceError::Identity)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_peak_context_and_cause_cannot_be_forged_from_public_errors() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let manifest = content_hash(b"exact original source inventory");
        let limit = generation_memory_ceiling();
        assert!(
            FreshProjectionCapacity::from_actual_peak(repository, generation, manifest, limit)
                .is_none()
        );
        assert!(
            FreshProjectionCapacity::from_actual_peak(repository, generation, manifest, u64::MAX)
                .is_none()
        );
        let cause =
            FreshProjectionCapacity::from_actual_peak(repository, generation, manifest, limit + 1)
                .unwrap();
        let error = cause.error();
        assert!(cause.matches(repository, generation, manifest, &error));
        for (repository, generation, manifest, error) in [
            (
                RepositoryId::from_bytes([3; 16]),
                generation,
                manifest,
                error,
            ),
            (
                repository,
                GenerationId::from_bytes([3; 20]),
                manifest,
                error,
            ),
            (
                repository,
                generation,
                content_hash(b"foreign inventory"),
                error,
            ),
            (
                repository,
                generation,
                manifest,
                generation_memory_limit(0, limit + 1, PendingGenerationMemory::Reserved),
            ),
            (
                repository,
                generation,
                manifest,
                generation_memory_limit(0, limit + 2, PendingGenerationMemory::Staged),
            ),
            (repository, generation, manifest, FirstSliceError::Identity),
            (
                repository,
                generation,
                manifest,
                FirstSliceError::Cancelled(rootlight_cancel::CancellationReason::ClientRequest),
            ),
        ] {
            assert!(!cause.matches(repository, generation, manifest, &error));
        }
        let mut foreign =
            NormalizedIrDocument::empty(RepositoryId::from_bytes([4; 16]), generation);
        assert_eq!(
            cause.annotate(&mut foreign, manifest),
            Err(FirstSliceError::Identity)
        );
        let mut missing = NormalizedIrDocument::empty(repository, generation);
        assert_eq!(
            cause.annotate(&mut missing, manifest),
            Err(FirstSliceError::Identity)
        );
    }

    #[test]
    fn context_barriers_keep_authenticated_vbp_generated_and_derived_inputs_strict() {
        use rootlight_ids::{FileIdentity, derive_file};
        let repository = RepositoryId::from_bytes([1; 16]);
        let path = RelativePath::parse(Path::new("authored.rs")).unwrap();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        let input = ManifestInput {
            file,
            path: "authored.rs".to_owned(),
            content_hash: content_hash(b"pub fn keep() {}\n"),
            bytes: 17,
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: Vec::new(),
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        let claims = source_file_identity_claims(
            std::slice::from_ref(&input),
            repository,
            &Cancellation::new(),
        )
        .unwrap();
        let service = FirstSliceService::new(2).unwrap();
        let mut inputs = vec![input];
        let mut claims = claims;
        let mut derived = container_sources::PreparedSources::default();
        let mut languages = BTreeMap::from([(file, "rust".to_owned())]);
        let eligible_now = |inputs: &[ManifestInput],
                            claims: &[FileIdentityClaim],
                            derived: &container_sources::PreparedSources,
                            languages: &BTreeMap<FileId, String>| {
            eligible(
                &service,
                FirstSliceIndexMode::Structural,
                Context {
                    inputs,
                    claims,
                    derived,
                    languages,
                    sources: &[],
                },
                &Cancellation::new(),
            )
            .unwrap()
        };
        assert!(eligible_now(&inputs, &claims, &derived, &languages));
        claims[0].path = "hidden.VbP".to_owned();
        assert!(!eligible_now(&inputs, &claims, &derived, &languages));
        claims[0].path = inputs[0].path.clone();
        inputs[0].class = InputClass::Generated;
        // A physical classification alone is preserved; actual origin mappings
        // are checked separately and remain a strict boundary.
        assert!(eligible_now(&inputs, &claims, &derived, &languages));
        inputs[0].class = InputClass::Source;
        derived.inputs.push(inputs[0].clone());
        assert!(!eligible_now(&inputs, &claims, &derived, &languages));
        derived.inputs.clear();
        languages.insert(file, "foreign-provider".to_owned());
        assert!(!eligible_now(&inputs, &claims, &derived, &languages));
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert_eq!(
            eligible(
                &service,
                FirstSliceIndexMode::Structural,
                Context {
                    inputs: &inputs,
                    claims: &claims,
                    derived: &derived,
                    languages: &languages,
                    sources: &[],
                },
                &cancelled
            ),
            Err(FirstSliceError::Cancelled(
                rootlight_cancel::CancellationReason::ClientRequest
            ))
        );
    }
}
