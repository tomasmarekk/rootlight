use std::collections::{BTreeSet, HashSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::FileId;
use rootlight_ir::{ContainerRef, EntityFlag, EntityKind, EntityRecord, NormalizedIrDocument};
use rootlight_search::{BuildBudget, LexicalDocument, SearchError, validate_build_admission};
use rootlight_storage::GenerationSnapshot;
use rootlight_vfs::SourceSnapshot;
use serde::Serialize;

use crate::QueryError;

/// Maximum UTF-8 source prefix admitted to one file-only fallback document.
pub const SOURCE_FALLBACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_SOURCE_IDENTIFIERS: usize = 4_096;
const MAX_SOURCE_IDENTIFIER_BYTES: usize = 240;

/// Incremental bounded projection of one immutable generation into lexical documents.
///
/// Entity documents are projected during construction. Callers then supply
/// unsupported source snapshots in the canonical order exposed by
/// [`Self::next_source_file`], allowing each full source body to be released
/// after its bounded fallback document is built.
#[derive(Debug)]
#[must_use = "finish the projection after supplying every required source"]
pub struct LexicalProjectionBuilder<'generation> {
    generation: &'generation GenerationSnapshot,
    budget: BuildBudget,
    projected: Vec<LexicalDocument>,
    text_bytes: usize,
    required_source_files: Vec<FileId>,
    next_source: usize,
}

impl<'generation> LexicalProjectionBuilder<'generation> {
    /// Starts a bounded projection and materializes its semantic entity documents.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid construction budgets, cancellation,
    /// missing direct entity source identity, generation drift, exceeded
    /// bounds, or allocation failure.
    pub fn new(
        generation: &'generation GenerationSnapshot,
        budget: BuildBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, QueryError> {
        validate_build_admission(budget)?;
        cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        let document = generation.document();
        if document.entities.len() > budget.max_documents {
            return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "documents",
            }));
        }
        let mut projected = Vec::new();
        projected
            .try_reserve(document.entities.len())
            .map_err(|_| QueryError::MemoryUnavailable)?;
        let mut text_bytes = 0usize;
        for entity in &document.entities {
            cancellation
                .check()
                .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
            let source = entity
                .evidence
                .source
                .as_ref()
                .ok_or(QueryError::IndexDrift)?;
            let file = document
                .files
                .binary_search_by_key(&source.span().file(), |record| record.id)
                .ok()
                .and_then(|index| document.files.get(index))
                .ok_or(QueryError::IndexDrift)?;
            if file.repository != entity.repository
                || file.generation != entity.generation
                || file.content_hash != source.content_hash()
            {
                return Err(QueryError::IndexDrift);
            }
            let kind = serialized_label(&entity.kind)?;
            let tier = serialized_label(&entity.tier)?;
            text_bytes = [
                entity.display_name.len(),
                entity.qualified_name.len(),
                file.path.len(),
                kind.len(),
                entity.language.len(),
                tier.len(),
            ]
            .into_iter()
            .try_fold(text_bytes, |total, length| {
                total
                    .checked_add(length)
                    .filter(|value| *value <= budget.max_text_bytes)
                    .ok_or(QueryError::Search(SearchError::BuildBudgetExceeded {
                        resource: "text_bytes",
                    }))
            })?;
            projected.push(LexicalDocument {
                symbol_id: Some(entity.id),
                file_id: file.id,
                identifier: try_clone(&entity.display_name)?,
                qualified_name: try_clone(&entity.qualified_name)?,
                path: try_clone(&file.path)?,
                kind,
                language: try_clone(&entity.language)?,
                tier,
                package: None,
                build_target: None,
                signature: None,
                type_names: Vec::new(),
                documentation: None,
                source_identifiers: Vec::new(),
                source_text: None,
                generated: file.generated,
                test: matches!(entity.kind, EntityKind::Test)
                    || entity.flags.contains(&EntityFlag::Test),
                declaration_only: entity_is_declaration_only(document, entity, &file.path),
            });
        }
        let mut unsupported_files = document
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "unsupported-language")
            .filter_map(|diagnostic| {
                diagnostic
                    .source
                    .as_ref()
                    .map(|source| source.span().file())
            })
            .collect::<BTreeSet<_>>();
        unsupported_files.extend(
            generation
                .source_files()
                .entries()
                .iter()
                .map(|entry| entry.file().id),
        );
        if document
            .entities
            .len()
            .checked_add(unsupported_files.len())
            .is_none_or(|documents| documents > budget.max_documents)
        {
            return Err(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "documents",
            }));
        }
        let mut required_source_files = Vec::new();
        required_source_files
            .try_reserve_exact(unsupported_files.len())
            .map_err(|_| QueryError::MemoryUnavailable)?;
        for file_id in unsupported_files {
            cancellation
                .check()
                .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
            let file = generation
                .find_file(file_id)
                .ok_or(QueryError::IndexDrift)?;
            if source_fallback_eligible(&file.path) {
                required_source_files.push(file.id);
            }
        }
        projected
            .try_reserve(required_source_files.len())
            .map_err(|_| QueryError::MemoryUnavailable)?;
        Ok(Self {
            generation,
            budget,
            projected,
            text_bytes,
            required_source_files,
            next_source: 0,
        })
    }

    /// Returns the next canonical file whose source snapshot must be supplied.
    #[must_use]
    pub fn next_source_file(&self) -> Option<FileId> {
        self.required_source_files.get(self.next_source).copied()
    }

    /// Projects the next required unsupported source snapshot.
    ///
    /// The snapshot must match the file returned by
    /// [`Self::next_source_file`], including its path, content identity, and
    /// exact byte length.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, out-of-order or drifted source
    /// identity, exceeded text bounds, or allocation failure.
    pub fn push_source(
        &mut self,
        source: &SourceSnapshot,
        cancellation: &Cancellation,
    ) -> Result<(), QueryError> {
        cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        let expected = self.next_source_file().ok_or(QueryError::IndexDrift)?;
        if source.file() != expected {
            return Err(QueryError::IndexDrift);
        }
        let (projected, next_text_bytes) = project_source_document(
            self.generation,
            source,
            self.text_bytes,
            SOURCE_FALLBACK_TEXT_BYTES,
            self.budget,
            cancellation,
        )?;
        let next_source = self
            .next_source
            .checked_add(1)
            .ok_or(QueryError::IndexDrift)?;
        self.projected.push(projected);
        self.text_bytes = next_text_bytes;
        self.next_source = next_source;
        Ok(())
    }

    /// Completes the projection after every required source was supplied.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::IndexDrift`] when a required source is missing,
    /// or [`QueryError::Cancelled`] when cancellation stops completion.
    pub fn finish(self, cancellation: &Cancellation) -> Result<Vec<LexicalDocument>, QueryError> {
        if self.next_source != self.required_source_files.len() {
            return Err(QueryError::IndexDrift);
        }
        cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        Ok(self.projected)
    }
}

/// Projects one exact source snapshot into a bounded file-only lexical document.
///
/// This streaming primitive admits one document at a time, so callers can
/// release the full source body immediately after handing the document to an
/// incremental index builder.
///
/// # Errors
///
/// Returns [`QueryError`] for source or generation drift, invalid build limits,
/// cancellation, bounded text overflow, or allocation failure.
pub fn project_source_fallback_document(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<LexicalDocument, QueryError> {
    project_source_fallback_document_with_text_limit(
        generation,
        source,
        SOURCE_FALLBACK_TEXT_BYTES,
        budget,
        cancellation,
    )
}

/// Projects one exact source with an optionally tighter text-prefix ceiling.
///
/// Values above [`SOURCE_FALLBACK_TEXT_BYTES`] are clamped to the existing
/// hard fallback bound. A zero ceiling still produces exact file and path
/// fields without retaining source text.
///
/// # Errors
///
/// Returns the same errors as [`project_source_fallback_document`].
pub fn project_source_fallback_document_with_text_limit(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    maximum_source_text_bytes: usize,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<LexicalDocument, QueryError> {
    validate_build_admission(budget)?;
    project_source_document(
        generation,
        source,
        0,
        maximum_source_text_bytes.min(SOURCE_FALLBACK_TEXT_BYTES),
        budget,
        cancellation,
    )
    .map(|(document, _)| document)
}

fn project_source_document(
    generation: &GenerationSnapshot,
    source: &SourceSnapshot,
    current_text_bytes: usize,
    maximum_source_text_bytes: usize,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<(LexicalDocument, usize), QueryError> {
    cancellation
        .check()
        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
    let document = generation.document();
    let file = generation
        .find_file(source.file())
        .ok_or(QueryError::IndexDrift)?;
    if source.content_hash() != file.content_hash
        || source.path().as_str() != file.path
        || u64::try_from(source.content().len()).ok() != Some(file.byte_length)
    {
        return Err(QueryError::IndexDrift);
    }
    let source_prefix = bounded_utf8_prefix(source.content(), maximum_source_text_bytes);
    let (source_identifiers, source_text) = source_prefix
        .as_deref()
        .map(bounded_source_projection)
        .map(|(identifiers, text)| {
            let text = (!text.is_empty()).then_some(text);
            (identifiers, text)
        })
        .unwrap_or_default();
    let identifier = file
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && name.len() <= 512)
        .unwrap_or("file");
    let tier = serialized_label(&analysis_tier_for_file(document, file)?)?;
    let next_text_bytes = [
        identifier.len(),
        identifier.len(),
        file.path.len(),
        "file".len(),
        file.language.len(),
        tier.len(),
        source_text.as_deref().map_or(0, str::len),
        source_identifiers.iter().map(String::len).sum(),
    ]
    .into_iter()
    .try_fold(current_text_bytes, |total, length| {
        total
            .checked_add(length)
            .filter(|value| *value <= budget.max_text_bytes)
            .ok_or(QueryError::Search(SearchError::BuildBudgetExceeded {
                resource: "text_bytes",
            }))
    })?;
    Ok((
        LexicalDocument {
            symbol_id: None,
            file_id: file.id,
            identifier: try_clone(identifier)?,
            qualified_name: try_clone(identifier)?,
            path: try_clone(&file.path)?,
            kind: "file".to_owned(),
            language: try_clone(&file.language)?,
            tier,
            package: None,
            build_target: None,
            signature: None,
            type_names: Vec::new(),
            documentation: None,
            source_identifiers,
            source_text,
            generated: file.generated,
            test: false,
            declaration_only: false,
        },
        next_text_bytes,
    ))
}

/// Projects normalized entities into bounded source-free lexical documents.
///
/// The projection copies only indexed metadata. Repository source bodies,
/// comments, and documentation remain outside the core index.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid construction budgets, cancellation,
/// missing direct entity source identity, generation drift, or allocation
/// failure.
pub fn project_lexical_documents(
    generation: &GenerationSnapshot,
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    LexicalProjectionBuilder::new(generation, budget, cancellation)?.finish(cancellation)
}

/// Projects normalized entities and unsupported retained text into bounded lexical documents.
///
/// File-only documents retain the real file identity and never synthesize a
/// semantic entity. Source text is admitted only for files explicitly marked
/// unsupported by the normalized generation.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid construction budgets, cancellation,
/// source or generation drift, exceeded bounds, or allocation failure.
pub fn project_lexical_documents_with_sources(
    generation: &GenerationSnapshot,
    sources: &[&SourceSnapshot],
    budget: BuildBudget,
    cancellation: &Cancellation,
) -> Result<Vec<LexicalDocument>, QueryError> {
    let mut projection = LexicalProjectionBuilder::new(generation, budget, cancellation)?;
    let mut ordered_sources = Vec::new();
    ordered_sources
        .try_reserve_exact(sources.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    ordered_sources.extend_from_slice(sources);
    ordered_sources.sort_unstable_by_key(|source| source.file());
    if ordered_sources
        .windows(2)
        .any(|pair| pair[0].file() == pair[1].file())
    {
        return Err(QueryError::IndexDrift);
    }
    while let Some(file_id) = projection.next_source_file() {
        let source = ordered_sources
            .binary_search_by_key(&file_id, |source| source.file())
            .ok()
            .and_then(|index| ordered_sources.get(index).copied())
            .ok_or(QueryError::IndexDrift)?;
        projection.push_source(source, cancellation)?;
    }
    projection.finish(cancellation)
}

fn analysis_tier_for_file(
    document: &NormalizedIrDocument,
    file: &rootlight_ir::FileRecord,
) -> Result<rootlight_ir::AnalysisTier, QueryError> {
    document
        .provenance
        .binary_search_by_key(&file.provenance, |candidate| candidate.id)
        .ok()
        .and_then(|index| document.provenance.get(index))
        .map(|provenance| provenance.tier)
        .ok_or(QueryError::IndexDrift)
}

fn bounded_utf8_prefix(content: &[u8], maximum: usize) -> Option<String> {
    let text = std::str::from_utf8(content).ok()?;
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    Some(text[..end].to_owned())
}

fn bounded_source_projection(source: &str) -> (Vec<String>, String) {
    let mut identifiers = HashSet::new();
    let mut output = String::with_capacity(source.len());
    for candidate in
        source.split(|character: char| !(character == '_' || character.is_alphanumeric()))
    {
        if candidate.is_empty() || candidate.len() > MAX_SOURCE_IDENTIFIER_BYTES {
            continue;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(candidate);
        if identifiers.len() < MAX_SOURCE_IDENTIFIERS
            && candidate
                .chars()
                .next()
                .is_none_or(|character| !(character == '_' || character.is_alphabetic()))
        {
            continue;
        }
        if identifiers.len() < MAX_SOURCE_IDENTIFIERS {
            identifiers.insert(candidate);
        }
    }
    let mut identifiers = identifiers
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    identifiers.sort_unstable();
    (identifiers, output)
}

fn source_fallback_eligible(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    !name.starts_with('.')
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "cargo.toml"
                | "go.mod"
                | "package.json"
                | "pyproject.toml"
                | "requirements.txt"
                | "tsconfig.json"
        )
}

fn entity_is_declaration_only(
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    path: &str,
) -> bool {
    if path_is_declaration_only(path) {
        return true;
    }
    if matches!(
        entity.kind,
        EntityKind::Interface | EntityKind::Trait | EntityKind::Protocol | EntityKind::TypeAlias
    ) {
        return true;
    }
    let Some(ContainerRef::Entity(container)) = entity.container else {
        return false;
    };
    document
        .entities
        .binary_search_by_key(&container, |candidate| candidate.id)
        .ok()
        .and_then(|index| document.entities.get(index))
        .is_some_and(|container| {
            matches!(
                container.kind,
                EntityKind::Interface | EntityKind::Trait | EntityKind::Protocol
            )
        })
}

fn path_is_declaration_only(path: &str) -> bool {
    path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts")
}

fn serialized_label(value: &impl Serialize) -> Result<String, QueryError> {
    let encoded = serde_json::to_string(value).map_err(|_| QueryError::ResultEncoding)?;
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or(QueryError::IndexDrift)
}

fn try_clone(value: &str) -> Result<String, QueryError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    cloned.push_str(value);
    Ok(cloned)
}

#[cfg(test)]
mod tests {
    use super::{bounded_source_projection, bounded_utf8_prefix, source_fallback_eligible};

    #[test]
    fn fallback_prefix_respects_tighter_utf8_boundaries() {
        let source = "abé".as_bytes();

        assert_eq!(bounded_utf8_prefix(source, 0).as_deref(), Some(""));
        assert_eq!(bounded_utf8_prefix(source, 3).as_deref(), Some("ab"));
        assert_eq!(bounded_utf8_prefix(source, 4).as_deref(), Some("abé"));
    }

    #[test]
    fn typescript_declaration_file_suffixes_are_unambiguous() {
        for path in ["index.d.ts", "index.d.mts", "index.d.cts"] {
            assert!(super::path_is_declaration_only(path));
        }
        assert!(
            !["index.ts", "index.mts", "index.cts"]
                .iter()
                .any(|path| super::path_is_declaration_only(path))
        );
    }

    #[test]
    fn fallback_text_drops_oversized_terms_without_losing_neighboring_identifiers() {
        let source = format!("before {} after", "x".repeat(241));
        let (identifiers, text) = bounded_source_projection(&source);

        assert_eq!(identifiers, ["after".to_owned(), "before".to_owned()]);
        assert_eq!(text, "before after");
    }

    #[test]
    fn project_metadata_is_not_promoted_to_source_fallback() {
        for path in [
            "Cargo.toml",
            "go.mod",
            "package.json",
            "pyproject.toml",
            "requirements.txt",
            "tsconfig.json",
            "src/.gitignore",
        ] {
            assert!(!source_fallback_eligible(path), "{path}");
        }
        assert!(source_fallback_eligible("mystery.sourceblob"));
        assert!(source_fallback_eligible("normalize.css"));
    }
}
