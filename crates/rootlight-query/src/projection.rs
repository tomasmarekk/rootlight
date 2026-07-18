use rootlight_cancel::Cancellation;
use rootlight_search::{BuildBudget, LexicalDocument, SearchError, validate_build_admission};
use rootlight_storage::GenerationSnapshot;
use serde::Serialize;

use crate::QueryError;

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
            symbol_id: entity.id,
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
            generated: file.generated,
        });
    }
    cancellation
        .check()
        .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
    Ok(projected)
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
