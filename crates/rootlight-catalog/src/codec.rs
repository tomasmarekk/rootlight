//! Scalar codecs between Rootlight domain values and fixed SQLite columns.
//!
//! Decoding treats every database scalar as hostile and reconstructs checked
//! IDs, spans, line ranges, confidence, and closed enum values without panics.

use std::collections::BTreeMap;

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{
    Confidence, ContainerRef, CoverageScope, FactRef, LineRange, RelationEndpoint, SourceRef,
    SourceSpan,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{CatalogError, CatalogErrorKind};

pub(crate) fn encode_enum<T: Serialize>(value: &T) -> Result<String, CatalogError> {
    match serde_json::to_value(value).map_err(CatalogError::json)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(CatalogError::new(CatalogErrorKind::InvalidGeneration)),
    }
}

pub(crate) fn decode_enum<T: DeserializeOwned>(value: String) -> Result<T, CatalogError> {
    serde_json::from_value(serde_json::Value::String(value)).map_err(CatalogError::json)
}

pub(crate) fn repository_id(value: Vec<u8>) -> Result<RepositoryId, CatalogError> {
    Ok(RepositoryId::from_bytes(fixed_bytes(value)?))
}

pub(crate) fn generation_id(value: Vec<u8>) -> Result<GenerationId, CatalogError> {
    Ok(GenerationId::from_bytes(fixed_bytes(value)?))
}

pub(crate) fn optional_generation_id(
    value: Option<Vec<u8>>,
) -> Result<Option<GenerationId>, CatalogError> {
    value.map(generation_id).transpose()
}

pub(crate) fn file_id(value: Vec<u8>) -> Result<FileId, CatalogError> {
    Ok(FileId::from_bytes(fixed_bytes(value)?))
}

pub(crate) fn symbol_id(value: Vec<u8>) -> Result<SymbolId, CatalogError> {
    Ok(SymbolId::from_bytes(fixed_bytes(value)?))
}

pub(crate) fn optional_symbol_id(value: Option<Vec<u8>>) -> Result<Option<SymbolId>, CatalogError> {
    value.map(symbol_id).transpose()
}

pub(crate) fn fact_id(value: Vec<u8>) -> Result<FactId, CatalogError> {
    Ok(FactId::from_bytes(fixed_bytes(value)?))
}

pub(crate) fn content_hash(value: Vec<u8>) -> Result<ContentHash, CatalogError> {
    Ok(ContentHash::from_bytes(fixed_bytes(value)?))
}

pub(crate) fn nonnegative_u64(value: i64) -> Result<u64, CatalogError> {
    u64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

pub(crate) fn sqlite_i64(value: u64) -> Result<i64, CatalogError> {
    i64::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
}

pub(crate) fn bool_value(value: i64) -> Result<bool, CatalogError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

pub(crate) fn confidence(value: i64) -> Result<Confidence, CatalogError> {
    let value = u16::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
    Confidence::new(value).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

pub(crate) fn line_range(start: i64, end: i64) -> Result<Option<LineRange>, CatalogError> {
    match (start, end) {
        (0, 0) => Ok(None),
        _ => LineRange::new(nonnegative_u64(start)?, nonnegative_u64(end)?)
            .map(Some)
            .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fixed source_refs row is decoded column by column"
)]
pub(crate) fn source_ref(
    repository: Vec<u8>,
    generation: Vec<u8>,
    file: Vec<u8>,
    start: i64,
    end: i64,
    hash: Vec<u8>,
    line_start: i64,
    line_end: i64,
) -> Result<SourceRef, CatalogError> {
    let span = SourceSpan::new(
        file_id(file)?,
        nonnegative_u64(start)?,
        nonnegative_u64(end)?,
    )
    .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))?;
    Ok(SourceRef::new(
        repository_id(repository)?,
        generation_id(generation)?,
        span,
        content_hash(hash)?,
        line_range(line_start, line_end)?,
    ))
}

pub(crate) fn source_by_ordinal(
    sources: &BTreeMap<i64, SourceRef>,
    ordinal: i64,
) -> Result<SourceRef, CatalogError> {
    sources
        .get(&ordinal)
        .cloned()
        .ok_or_else(|| CatalogError::new(CatalogErrorKind::Corrupt))
}

pub(crate) fn optional_source_by_ordinal(
    sources: &BTreeMap<i64, SourceRef>,
    ordinal: Option<i64>,
) -> Result<Option<SourceRef>, CatalogError> {
    ordinal
        .map(|ordinal| source_by_ordinal(sources, ordinal))
        .transpose()
}

pub(crate) fn encode_container(
    container: &Option<ContainerRef>,
) -> (Option<&'static str>, Option<&[u8]>) {
    match container {
        None => (None, None),
        Some(ContainerRef::Repository(id)) => (Some("repository"), Some(id.as_bytes())),
        Some(ContainerRef::File(id)) => (Some("file"), Some(id.as_bytes().as_slice())),
        Some(ContainerRef::Entity(id)) => (Some("entity"), Some(id.as_bytes().as_slice())),
    }
}

pub(crate) fn decode_container(
    kind: Option<String>,
    id: Option<Vec<u8>>,
) -> Result<Option<ContainerRef>, CatalogError> {
    match (kind.as_deref(), id) {
        (None, None) => Ok(None),
        (Some("repository"), Some(id)) => Ok(Some(ContainerRef::Repository(repository_id(id)?))),
        (Some("file"), Some(id)) => Ok(Some(ContainerRef::File(file_id(id)?))),
        (Some("entity"), Some(id)) => Ok(Some(ContainerRef::Entity(symbol_id(id)?))),
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

pub(crate) fn encode_endpoint(endpoint: &RelationEndpoint) -> (&'static str, &[u8]) {
    match endpoint {
        RelationEndpoint::Repository(id) => ("repository", id.as_bytes().as_slice()),
        RelationEndpoint::File(id) => ("file", id.as_bytes().as_slice()),
        RelationEndpoint::Entity(id) => ("entity", id.as_bytes().as_slice()),
        RelationEndpoint::Occurrence(id) => ("fact", id.as_bytes().as_slice()),
    }
}

pub(crate) fn decode_endpoint(kind: &str, id: Vec<u8>) -> Result<RelationEndpoint, CatalogError> {
    match kind {
        "repository" => Ok(RelationEndpoint::Repository(repository_id(id)?)),
        "file" => Ok(RelationEndpoint::File(file_id(id)?)),
        "entity" => Ok(RelationEndpoint::Entity(symbol_id(id)?)),
        "fact" => Ok(RelationEndpoint::Occurrence(fact_id(id)?)),
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

pub(crate) fn encode_scope(scope: &CoverageScope) -> (&'static str, &[u8]) {
    match scope {
        CoverageScope::Repository(id) => ("repository", id.as_bytes().as_slice()),
        CoverageScope::File(id) => ("file", id.as_bytes().as_slice()),
        CoverageScope::Entity(id) => ("entity", id.as_bytes().as_slice()),
    }
}

pub(crate) fn decode_scope(kind: &str, id: Vec<u8>) -> Result<CoverageScope, CatalogError> {
    match kind {
        "repository" => Ok(CoverageScope::Repository(repository_id(id)?)),
        "file" => Ok(CoverageScope::File(file_id(id)?)),
        "entity" => Ok(CoverageScope::Entity(symbol_id(id)?)),
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

pub(crate) fn encode_fact_ref(reference: &FactRef) -> (&'static str, &[u8]) {
    match reference {
        FactRef::File(id) => ("file", id.as_bytes().as_slice()),
        FactRef::Entity(id) => ("entity", id.as_bytes().as_slice()),
        FactRef::Fact(id) => ("fact", id.as_bytes().as_slice()),
    }
}

pub(crate) fn decode_fact_ref(kind: &str, id: Vec<u8>) -> Result<FactRef, CatalogError> {
    match kind {
        "file" => Ok(FactRef::File(file_id(id)?)),
        "entity" => Ok(FactRef::Entity(symbol_id(id)?)),
        "fact" => Ok(FactRef::Fact(fact_id(id)?)),
        _ => Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    }
}

fn fixed_bytes<const N: usize>(value: Vec<u8>) -> Result<[u8; N], CatalogError> {
    value
        .try_into()
        .map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}
