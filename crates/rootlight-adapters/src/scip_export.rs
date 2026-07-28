//! Deterministic bounded export from normalized IR to official SCIP protobuf.
//!
//! The host supplies exact immutable source bindings; this module never opens
//! repository paths and never serializes a project root or source text.
//!
//! # Supported subset
//!
//! Version 1 exports canonical file documents, supported semantic entity kinds,
//! exact resolved occurrences, and exact entity relationships corresponding to
//! SCIP reference, implementation, type-definition, and definition links.
//! Files are limited to exact UTF-8 bytes whose canonical relative path derives
//! the normalized file identity.
//! Ambiguous or unresolved occurrences, inexact facts, unsupported entity kinds
//! and relations, provenance, source mappings, coverage, diagnostics, skipped
//! regions, extensions, and source-derived symbol or occurrence metadata are
//! counted as explicit bounded omissions in [`ScipExportReport`].

use std::collections::{BTreeMap, BTreeSet};

use protobuf::{EnumOrUnknown, Message, MessageField};
use rootlight_adapter_sdk::LanguageId;
use rootlight_cancel::Cancellation;
use rootlight_ids::{
    FileId, FileIdentity, GenerationId, RepositoryId, SymbolId, content_hash, derive_file,
};
use rootlight_ir::{
    EntityFlag, EntityKind, EntityRecord, ExtensionSupport, IrDocumentValidationError, IrLimits,
    NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, RelationEndpoint,
    RelationPredicate, SourceSpan, canonicalize_ir_document,
};
use scip::types::{
    Document, Index, Metadata, MultiLineRange, Occurrence, PositionEncoding, Relationship,
    SingleLineRange, SymbolInformation, TextEncoding, ToolInfo, symbol_information::Kind,
};

use crate::ADAPTER_VERSION;

/// Version of Rootlight's documented SCIP interoperability subset.
pub const SCIP_EXPORT_SUBSET_VERSION: &str = "rootlight.scip-export/1";

const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 4_096;
const MAX_SYMBOLS: usize = 200_000;
const MAX_OCCURRENCES: usize = 1_000_000;
const MAX_RELATIONSHIPS: usize = 500_000;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const CHECK_INTERVAL: usize = 128;
const EXACT_CONFIDENCE: u16 = 1_000;
const DEFINITION_ROLE: i32 = 0x1;
const IMPORT_ROLE: i32 = 0x2;
const WRITE_ROLE: i32 = 0x4;
const READ_ROLE: i32 = 0x8;
const GENERATED_ROLE: i32 = 0x10;
const TEST_ROLE: i32 = 0x20;
const FORWARD_DEFINITION_ROLE: i32 = 0x40;

/// Fixed hard limits applied before or while SCIP output is materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScipExportLimits {
    max_encoded_bytes: usize,
    max_documents: usize,
    max_symbols: usize,
    max_occurrences: usize,
    max_relationships: usize,
    max_source_bytes: usize,
    max_total_source_bytes: usize,
}

impl ScipExportLimits {
    /// Creates an export policy no broader than the process-wide hard ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`ScipExportError::LimitExceeded`] when any requested ceiling is
    /// broader than the corresponding process-wide maximum.
    pub fn new(
        max_encoded_bytes: usize,
        max_documents: usize,
        max_symbols: usize,
        max_occurrences: usize,
        max_relationships: usize,
        max_source_bytes: usize,
        max_total_source_bytes: usize,
    ) -> Result<Self, ScipExportError> {
        require_limit(
            ScipExportResource::EncodedBytes,
            max_encoded_bytes,
            MAX_ENCODED_BYTES,
        )?;
        require_limit(ScipExportResource::Documents, max_documents, MAX_DOCUMENTS)?;
        require_limit(ScipExportResource::Symbols, max_symbols, MAX_SYMBOLS)?;
        require_limit(
            ScipExportResource::Occurrences,
            max_occurrences,
            MAX_OCCURRENCES,
        )?;
        require_limit(
            ScipExportResource::Relationships,
            max_relationships,
            MAX_RELATIONSHIPS,
        )?;
        require_limit(
            ScipExportResource::SourceBytes,
            max_source_bytes,
            MAX_SOURCE_BYTES,
        )?;
        require_limit(
            ScipExportResource::SourceBytes,
            max_total_source_bytes,
            MAX_TOTAL_SOURCE_BYTES,
        )?;
        Ok(Self {
            max_encoded_bytes,
            max_documents,
            max_symbols,
            max_occurrences,
            max_relationships,
            max_source_bytes,
            max_total_source_bytes,
        })
    }

    /// Returns the maximum encoded SCIP index size.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        self.max_encoded_bytes
    }

    /// Returns the maximum document count.
    #[must_use]
    pub const fn max_documents(self) -> usize {
        self.max_documents
    }

    /// Returns the maximum normalized entity count examined by the exporter.
    #[must_use]
    pub const fn max_symbols(self) -> usize {
        self.max_symbols
    }

    /// Returns the maximum normalized occurrence count examined by the exporter.
    #[must_use]
    pub const fn max_occurrences(self) -> usize {
        self.max_occurrences
    }

    /// Returns the maximum normalized relationship count examined by the exporter.
    #[must_use]
    pub const fn max_relationships(self) -> usize {
        self.max_relationships
    }

    /// Returns the maximum byte length of one exact source binding.
    #[must_use]
    pub const fn max_source_bytes(self) -> usize {
        self.max_source_bytes
    }

    /// Returns the maximum combined byte length of exact source bindings.
    #[must_use]
    pub const fn max_total_source_bytes(self) -> usize {
        self.max_total_source_bytes
    }
}

impl Default for ScipExportLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: MAX_ENCODED_BYTES,
            max_documents: MAX_DOCUMENTS,
            max_symbols: MAX_SYMBOLS,
            max_occurrences: MAX_OCCURRENCES,
            max_relationships: MAX_RELATIONSHIPS,
            max_source_bytes: MAX_SOURCE_BYTES,
            max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
        }
    }
}

/// Exact generation-bound source supplied to a SCIP export.
#[derive(Clone, Copy)]
pub struct ScipExportSource<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    path: &'a str,
    content: &'a [u8],
}

impl<'a> ScipExportSource<'a> {
    /// Creates one source binding without reading from the filesystem.
    ///
    /// Repository, generation, file, path, length, encoding, and content hash
    /// are checked atomically by [`export_scip_index`].
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        generation: GenerationId,
        file: FileId,
        path: &'a str,
        content: &'a [u8],
    ) -> Self {
        Self {
            repository,
            generation,
            file,
            path,
            content,
        }
    }

    /// Returns the expected repository identity.
    #[must_use]
    pub const fn repository(self) -> RepositoryId {
        self.repository
    }

    /// Returns the expected immutable generation identity.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the expected file identity.
    #[must_use]
    pub const fn file(self) -> FileId {
        self.file
    }

    /// Returns the canonical repository-relative presentation path.
    #[must_use]
    pub const fn path(self) -> &'a str {
        self.path
    }

    /// Returns exact immutable UTF-8 source bytes.
    #[must_use]
    pub const fn content(self) -> &'a [u8] {
        self.content
    }
}

impl std::fmt::Debug for ScipExportSource<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScipExportSource")
            .field("repository", &self.repository)
            .field("generation", &self.generation)
            .field("file", &self.file)
            .field("content_bytes", &self.content.len())
            .finish_non_exhaustive()
    }
}

/// Aggregate accounting for records deliberately outside the export subset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScipExportOmissions {
    unsupported_entities: usize,
    ambiguous_entities: usize,
    sourceless_entities: usize,
    entity_metadata: usize,
    ambiguous_occurrences: usize,
    unresolved_occurrences: usize,
    inexact_occurrences: usize,
    unsupported_occurrences: usize,
    occurrence_metadata: usize,
    unsupported_relationships: usize,
    provenance: usize,
    source_mappings: usize,
    coverage_records: usize,
    skipped_regions: usize,
    diagnostics: usize,
    extensions: usize,
}

impl ScipExportOmissions {
    /// Returns entities whose kinds have no exact supported SCIP projection.
    #[must_use]
    pub const fn unsupported_entities(self) -> usize {
        self.unsupported_entities
    }

    /// Returns entities with competing exact definition or declaration owners.
    #[must_use]
    pub const fn ambiguous_entities(self) -> usize {
        self.ambiguous_entities
    }

    /// Returns entities without one exact source file.
    #[must_use]
    pub const fn sourceless_entities(self) -> usize {
        self.sourceless_entities
    }

    /// Returns exported entities whose source-derived names and extra metadata were omitted.
    #[must_use]
    pub const fn entity_metadata(self) -> usize {
        self.entity_metadata
    }

    /// Returns bounded-candidate occurrences omitted without selecting a target.
    #[must_use]
    pub const fn ambiguous_occurrences(self) -> usize {
        self.ambiguous_occurrences
    }

    /// Returns unresolved occurrences omitted without inventing a symbol.
    #[must_use]
    pub const fn unresolved_occurrences(self) -> usize {
        self.unresolved_occurrences
    }

    /// Returns resolved occurrences omitted because their confidence was not exact.
    #[must_use]
    pub const fn inexact_occurrences(self) -> usize {
        self.inexact_occurrences
    }

    /// Returns exact resolved occurrences whose target was not exportable.
    #[must_use]
    pub const fn unsupported_occurrences(self) -> usize {
        self.unsupported_occurrences
    }

    /// Returns exported occurrences whose non-SCIP metadata was omitted.
    #[must_use]
    pub const fn occurrence_metadata(self) -> usize {
        self.occurrence_metadata
    }

    /// Returns relations that lacked an exact supported SCIP projection.
    #[must_use]
    pub const fn unsupported_relationships(self) -> usize {
        self.unsupported_relationships
    }

    /// Returns normalized provenance records omitted from SCIP.
    #[must_use]
    pub const fn provenance(self) -> usize {
        self.provenance
    }

    /// Returns normalized source mappings omitted from SCIP.
    #[must_use]
    pub const fn source_mappings(self) -> usize {
        self.source_mappings
    }

    /// Returns normalized coverage records omitted from SCIP.
    #[must_use]
    pub const fn coverage_records(self) -> usize {
        self.coverage_records
    }

    /// Returns normalized skipped-region records omitted from SCIP.
    #[must_use]
    pub const fn skipped_regions(self) -> usize {
        self.skipped_regions
    }

    /// Returns normalized diagnostics omitted from SCIP.
    #[must_use]
    pub const fn diagnostics(self) -> usize {
        self.diagnostics
    }

    /// Returns normalized extension envelopes omitted from SCIP.
    #[must_use]
    pub const fn extensions(self) -> usize {
        self.extensions
    }
}

/// Aggregate accounting for one successful SCIP export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScipExportReport {
    documents: usize,
    symbols: usize,
    occurrences: usize,
    relationships: usize,
    encoded_bytes: usize,
    omissions: ScipExportOmissions,
}

impl ScipExportReport {
    /// Returns the exact documented subset version.
    #[must_use]
    pub const fn subset_version(self) -> &'static str {
        SCIP_EXPORT_SUBSET_VERSION
    }

    /// Returns the emitted document count.
    #[must_use]
    pub const fn documents(self) -> usize {
        self.documents
    }

    /// Returns the emitted symbol-information count.
    #[must_use]
    pub const fn symbols(self) -> usize {
        self.symbols
    }

    /// Returns the emitted exact resolved occurrence count.
    #[must_use]
    pub const fn occurrences(self) -> usize {
        self.occurrences
    }

    /// Returns the emitted SCIP relationship-message count.
    #[must_use]
    pub const fn relationships(self) -> usize {
        self.relationships
    }

    /// Returns the final encoded protobuf byte length.
    #[must_use]
    pub const fn encoded_bytes(self) -> usize {
        self.encoded_bytes
    }

    /// Returns explicit accounting for every bounded omission class.
    #[must_use]
    pub const fn omissions(self) -> ScipExportOmissions {
        self.omissions
    }
}

/// Official SCIP protobuf bytes and their bounded export accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipExportOutcome {
    encoded: Vec<u8>,
    report: ScipExportReport,
}

impl ScipExportOutcome {
    /// Returns the official SCIP protobuf bytes.
    #[must_use]
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Returns bounded export and omission accounting.
    #[must_use]
    pub const fn report(&self) -> ScipExportReport {
        self.report
    }

    /// Consumes the outcome and returns official SCIP protobuf bytes.
    #[must_use]
    pub fn into_encoded(self) -> Vec<u8> {
        self.encoded
    }
}

/// Immutable context required to export one normalized IR generation.
pub struct ScipExportRequest<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    sources: &'a [ScipExportSource<'a>],
    limits: ScipExportLimits,
    ir_limits: &'a IrLimits,
    extension_support: &'a ExtensionSupport,
    cancellation: &'a Cancellation,
}

impl<'a> ScipExportRequest<'a> {
    /// Creates a request with fixed safe export limits.
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        generation: GenerationId,
        sources: &'a [ScipExportSource<'a>],
        ir_limits: &'a IrLimits,
        extension_support: &'a ExtensionSupport,
        cancellation: &'a Cancellation,
    ) -> Self {
        Self {
            repository,
            generation,
            sources,
            limits: ScipExportLimits {
                max_encoded_bytes: MAX_ENCODED_BYTES,
                max_documents: MAX_DOCUMENTS,
                max_symbols: MAX_SYMBOLS,
                max_occurrences: MAX_OCCURRENCES,
                max_relationships: MAX_RELATIONSHIPS,
                max_source_bytes: MAX_SOURCE_BYTES,
                max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
            },
            ir_limits,
            extension_support,
            cancellation,
        }
    }

    /// Replaces fixed export ceilings with an explicitly supplied policy.
    #[must_use]
    pub const fn with_limits(mut self, limits: ScipExportLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Returns the repository expected to own every exported fact.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation expected to own every exported fact.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns exact source bindings supplied by the host.
    #[must_use]
    pub const fn sources(&self) -> &[ScipExportSource<'a>] {
        self.sources
    }

    /// Returns hard SCIP export limits.
    #[must_use]
    pub const fn limits(&self) -> ScipExportLimits {
        self.limits
    }
}

/// Exports the documented normalized-IR subset as official SCIP protobuf bytes.
///
/// The exporter validates and canonicalizes a clone of `document`, verifies an
/// exact source binding for every file, emits only UTF-8 positions and
/// repository-relative paths, and never serializes source text or a project
/// root.
///
/// # Errors
///
/// Returns [`ScipExportError`] when normalized IR, identities, source bindings,
/// UTF-8 ranges, resource limits, cancellation, or protobuf encoding fail.
pub fn export_scip_index(
    document: &NormalizedIrDocument,
    request: ScipExportRequest<'_>,
) -> Result<ScipExportOutcome, ScipExportError> {
    check(request.cancellation)?;
    if document.repository != request.repository || document.generation != request.generation {
        return Err(ScipExportError::DocumentIdentityMismatch);
    }
    preflight_limits(document, request.limits)?;
    let document = canonicalize_ir_document(
        document.clone(),
        request.ir_limits,
        request.extension_support,
    )
    .map_err(ScipExportError::InvalidDocument)?;
    if document.files.is_empty() {
        return Err(ScipExportError::MissingDocuments);
    }
    check(request.cancellation)?;
    let sources = materialize_sources(
        &document,
        request.sources,
        request.limits,
        request.cancellation,
    )?;
    Exporter {
        document: &document,
        sources,
        limits: request.limits,
        cancellation: request.cancellation,
    }
    .export()
}

struct Exporter<'a> {
    document: &'a NormalizedIrDocument,
    sources: BTreeMap<FileId, SourceMaterial<'a>>,
    limits: ScipExportLimits,
    cancellation: &'a Cancellation,
}

impl Exporter<'_> {
    fn export(self) -> Result<ScipExportOutcome, ScipExportError> {
        let mut output = self.empty_documents()?;
        self.validate_occurrence_ranges()?;
        let definition_files = definition_files(self.document);
        let mut omissions = ScipExportOmissions {
            provenance: self.document.provenance.len(),
            source_mappings: self.document.source_mappings.len(),
            coverage_records: self.document.coverage_records.len(),
            skipped_regions: self.document.skipped_regions.len(),
            diagnostics: self.document.diagnostics.len(),
            extensions: self.document.extensions.len(),
            ..ScipExportOmissions::default()
        };
        let symbols = self.materialize_symbols(&definition_files, &mut output, &mut omissions)?;
        let relationships =
            self.materialize_relationships(&symbols, &mut output, &mut omissions)?;
        let occurrences = self.materialize_occurrences(&symbols, &mut output, &mut omissions)?;
        check(self.cancellation)?;

        let mut documents = Vec::with_capacity(output.len());
        for (index, document) in output.into_values().enumerate() {
            check_periodically(index, self.cancellation)?;
            documents.push(document.into_scip());
        }
        let mut metadata = Metadata {
            text_document_encoding: EnumOrUnknown::new(TextEncoding::UTF8),
            ..Default::default()
        };
        metadata.tool_info = MessageField::some(ToolInfo {
            name: "rootlight-adapters".to_owned(),
            version: ADAPTER_VERSION.to_owned(),
            arguments: vec![format!("subset={SCIP_EXPORT_SUBSET_VERSION}")],
            ..Default::default()
        });
        let index = Index {
            metadata: MessageField::some(metadata),
            documents,
            ..Default::default()
        };
        let computed =
            usize::try_from(index.compute_size()).map_err(|_| ScipExportError::Accounting)?;
        require_limit(
            ScipExportResource::EncodedBytes,
            computed,
            self.limits.max_encoded_bytes,
        )?;
        check(self.cancellation)?;
        let encoded = index.write_to_bytes().map_err(ScipExportError::Encode)?;
        require_limit(
            ScipExportResource::EncodedBytes,
            encoded.len(),
            self.limits.max_encoded_bytes,
        )?;
        check(self.cancellation)?;
        let report = ScipExportReport {
            documents: index.documents.len(),
            symbols: symbols.len(),
            occurrences,
            relationships,
            encoded_bytes: encoded.len(),
            omissions,
        };
        Ok(ScipExportOutcome { encoded, report })
    }

    fn empty_documents(&self) -> Result<BTreeMap<String, OutputDocument>, ScipExportError> {
        let mut output = BTreeMap::new();
        for (index, file) in self.document.files.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let source = self
                .sources
                .get(&file.id)
                .ok_or(ScipExportError::MissingSource)?;
            if output
                .insert(
                    source.path.to_owned(),
                    OutputDocument {
                        language: file.language.clone(),
                        relative_path: source.path.to_owned(),
                        symbols: BTreeMap::new(),
                        occurrences: Vec::new(),
                    },
                )
                .is_some()
            {
                return Err(ScipExportError::DuplicateSource);
            }
        }
        Ok(output)
    }

    fn validate_occurrence_ranges(&self) -> Result<(), ScipExportError> {
        for (index, occurrence) in self.document.occurrences.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let source = self
                .sources
                .get(&occurrence.file)
                .ok_or(ScipExportError::MissingSource)?;
            source.scip_range(occurrence.source.span())?;
        }
        Ok(())
    }

    fn materialize_symbols(
        &self,
        definition_files: &BTreeMap<SymbolId, DefinitionFiles>,
        output: &mut BTreeMap<String, OutputDocument>,
        omissions: &mut ScipExportOmissions,
    ) -> Result<BTreeMap<SymbolId, ExportedSymbol>, ScipExportError> {
        let mut symbols = BTreeMap::new();
        for (index, entity) in self.document.entities.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let Some(kind) = project_entity_kind(entity.kind) else {
                omissions.unsupported_entities = checked_add(
                    omissions.unsupported_entities,
                    1,
                    ScipExportResource::Symbols,
                )?;
                continue;
            };
            let Some(owner) = self.entity_owner(entity, definition_files, omissions)? else {
                continue;
            };
            let symbol = format!("rootlight . . . {}.", entity.id);
            scip::symbol::parse_symbol(&symbol).map_err(|_| ScipExportError::SymbolIdentity)?;
            let source = self
                .sources
                .get(&owner)
                .ok_or(ScipExportError::MissingSource)?;
            let target_document = output
                .get_mut(source.path)
                .ok_or(ScipExportError::MissingSource)?;
            target_document.symbols.insert(
                symbol.clone(),
                SymbolInformation {
                    symbol: symbol.clone(),
                    kind: EnumOrUnknown::new(kind),
                    ..Default::default()
                },
            );
            symbols.insert(
                entity.id,
                ExportedSymbol {
                    symbol,
                    file: owner,
                    generated: entity.flags.contains(&EntityFlag::Generated),
                    test: entity.flags.contains(&EntityFlag::Test),
                },
            );
            omissions.entity_metadata =
                checked_add(omissions.entity_metadata, 1, ScipExportResource::Symbols)?;
        }
        Ok(symbols)
    }

    fn entity_owner(
        &self,
        entity: &EntityRecord,
        definition_files: &BTreeMap<SymbolId, DefinitionFiles>,
        omissions: &mut ScipExportOmissions,
    ) -> Result<Option<FileId>, ScipExportError> {
        match definition_files.get(&entity.id) {
            Some(files) if files.definitions.len() == 1 => Ok(files.definitions.first().copied()),
            Some(files) if files.definitions.is_empty() && files.declarations.len() == 1 => {
                Ok(files.declarations.first().copied())
            }
            Some(_) => {
                omissions.ambiguous_entities =
                    checked_add(omissions.ambiguous_entities, 1, ScipExportResource::Symbols)?;
                Ok(None)
            }
            None => match entity.evidence.source.as_ref() {
                Some(source) => Ok(Some(source.span().file())),
                None => {
                    omissions.sourceless_entities = checked_add(
                        omissions.sourceless_entities,
                        1,
                        ScipExportResource::Symbols,
                    )?;
                    Ok(None)
                }
            },
        }
    }

    fn materialize_occurrences(
        &self,
        symbols: &BTreeMap<SymbolId, ExportedSymbol>,
        output: &mut BTreeMap<String, OutputDocument>,
        omissions: &mut ScipExportOmissions,
    ) -> Result<usize, ScipExportError> {
        let mut emitted = 0_usize;
        for (index, occurrence) in self.document.occurrences.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let target = match occurrence.target {
                OccurrenceTarget::Resolved { symbol } => symbol,
                OccurrenceTarget::Candidates { .. } => {
                    omissions.ambiguous_occurrences = checked_add(
                        omissions.ambiguous_occurrences,
                        1,
                        ScipExportResource::Occurrences,
                    )?;
                    continue;
                }
                OccurrenceTarget::Unresolved { .. } => {
                    omissions.unresolved_occurrences = checked_add(
                        omissions.unresolved_occurrences,
                        1,
                        ScipExportResource::Occurrences,
                    )?;
                    continue;
                }
            };
            if occurrence.confidence.get() != EXACT_CONFIDENCE {
                omissions.inexact_occurrences = checked_add(
                    omissions.inexact_occurrences,
                    1,
                    ScipExportResource::Occurrences,
                )?;
                continue;
            }
            let Some(symbol) = symbols.get(&target) else {
                omissions.unsupported_occurrences = checked_add(
                    omissions.unsupported_occurrences,
                    1,
                    ScipExportResource::Occurrences,
                )?;
                continue;
            };
            let source = self
                .sources
                .get(&occurrence.file)
                .ok_or(ScipExportError::MissingSource)?;
            let mut scip_occurrence = Occurrence {
                symbol: symbol.symbol.clone(),
                symbol_roles: occurrence_roles(occurrence, symbol, source.generated),
                ..Default::default()
            };
            source.apply_range(occurrence.source.span(), &mut scip_occurrence)?;
            let target_document = output
                .get_mut(source.path)
                .ok_or(ScipExportError::MissingSource)?;
            target_document.occurrences.push(scip_occurrence);
            emitted = checked_add(emitted, 1, ScipExportResource::Occurrences)?;
            omissions.occurrence_metadata = checked_add(
                omissions.occurrence_metadata,
                1,
                ScipExportResource::Occurrences,
            )?;
        }
        for (index, document) in output.values_mut().enumerate() {
            check_periodically(index, self.cancellation)?;
            document
                .occurrences
                .sort_by(|left, right| occurrence_key(left).cmp(&occurrence_key(right)));
        }
        Ok(emitted)
    }

    fn materialize_relationships(
        &self,
        symbols: &BTreeMap<SymbolId, ExportedSymbol>,
        output: &mut BTreeMap<String, OutputDocument>,
        omissions: &mut ScipExportOmissions,
    ) -> Result<usize, ScipExportError> {
        let mut projected = BTreeMap::<(SymbolId, SymbolId), RelationshipFlags>::new();
        for (index, relation) in self.document.relations.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            if relation.confidence.get() != EXACT_CONFIDENCE {
                omissions.unsupported_relationships = checked_add(
                    omissions.unsupported_relationships,
                    1,
                    ScipExportResource::Relationships,
                )?;
                continue;
            }
            let (RelationEndpoint::Entity(subject), RelationEndpoint::Entity(object)) =
                (relation.subject, relation.object)
            else {
                omissions.unsupported_relationships = checked_add(
                    omissions.unsupported_relationships,
                    1,
                    ScipExportResource::Relationships,
                )?;
                continue;
            };
            let Some(flag) = relationship_flag(relation.predicate) else {
                omissions.unsupported_relationships = checked_add(
                    omissions.unsupported_relationships,
                    1,
                    ScipExportResource::Relationships,
                )?;
                continue;
            };
            if !symbols.contains_key(&subject) || !symbols.contains_key(&object) {
                omissions.unsupported_relationships = checked_add(
                    omissions.unsupported_relationships,
                    1,
                    ScipExportResource::Relationships,
                )?;
                continue;
            }
            projected.entry((subject, object)).or_default().set(flag);
        }

        for (index, ((subject, object), flags)) in projected.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let subject = symbols
                .get(subject)
                .ok_or(ScipExportError::SymbolIdentity)?;
            let object = symbols.get(object).ok_or(ScipExportError::SymbolIdentity)?;
            let source = self
                .sources
                .get(&subject.file)
                .ok_or(ScipExportError::MissingSource)?;
            let information = output
                .get_mut(source.path)
                .and_then(|document| document.symbols.get_mut(&subject.symbol))
                .ok_or(ScipExportError::SymbolIdentity)?;
            information.relationships.push(Relationship {
                symbol: object.symbol.clone(),
                is_reference: flags.is_reference,
                is_implementation: flags.is_implementation,
                is_type_definition: flags.is_type_definition,
                is_definition: flags.is_definition,
                ..Default::default()
            });
        }
        for (index, document) in output.values_mut().enumerate() {
            check_periodically(index, self.cancellation)?;
            for information in document.symbols.values_mut() {
                information
                    .relationships
                    .sort_by(|left, right| relationship_key(left).cmp(&relationship_key(right)));
            }
        }
        Ok(projected.len())
    }
}

struct SourceMaterial<'a> {
    path: &'a str,
    text: &'a str,
    generated: bool,
    line_starts: Vec<usize>,
}

impl SourceMaterial<'_> {
    fn apply_range(
        &self,
        span: SourceSpan,
        occurrence: &mut Occurrence,
    ) -> Result<(), ScipExportError> {
        match self.scip_range(span)? {
            ScipRange::Single(value) => occurrence.set_single_line_range(value),
            ScipRange::Multi(value) => occurrence.set_multi_line_range(value),
        }
        Ok(())
    }

    fn scip_range(&self, span: SourceSpan) -> Result<ScipRange, ScipExportError> {
        let start =
            usize::try_from(span.start_byte()).map_err(|_| ScipExportError::InvalidRange)?;
        let end = usize::try_from(span.end_byte()).map_err(|_| ScipExportError::InvalidRange)?;
        let (start_line, start_character) = self.position(start)?;
        let (end_line, end_character) = self.position(end)?;
        if start_line == end_line {
            Ok(ScipRange::Single(SingleLineRange {
                line: start_line,
                start_character,
                end_character,
                ..Default::default()
            }))
        } else {
            Ok(ScipRange::Multi(MultiLineRange {
                start_line,
                start_character,
                end_line,
                end_character,
                ..Default::default()
            }))
        }
    }

    fn position(&self, offset: usize) -> Result<(i32, i32), ScipExportError> {
        if offset > self.text.len() || !self.text.is_char_boundary(offset) {
            return Err(ScipExportError::InvalidRange);
        }
        let insertion = self.line_starts.partition_point(|start| *start <= offset);
        let line_index = insertion
            .checked_sub(1)
            .ok_or(ScipExportError::InvalidRange)?;
        let line_start = *self
            .line_starts
            .get(line_index)
            .ok_or(ScipExportError::InvalidRange)?;
        let next_start = self
            .line_starts
            .get(line_index.saturating_add(1))
            .copied()
            .unwrap_or(self.text.len());
        let bytes = self.text.as_bytes();
        let mut logical_end = next_start;
        if logical_end > line_start && bytes.get(logical_end - 1) == Some(&b'\n') {
            logical_end -= 1;
        }
        if logical_end > line_start && bytes.get(logical_end - 1) == Some(&b'\r') {
            logical_end -= 1;
        }
        if offset > logical_end {
            return Err(ScipExportError::InvalidRange);
        }
        let character = offset
            .checked_sub(line_start)
            .ok_or(ScipExportError::InvalidRange)?;
        Ok((
            i32::try_from(line_index).map_err(|_| ScipExportError::InvalidRange)?,
            i32::try_from(character).map_err(|_| ScipExportError::InvalidRange)?,
        ))
    }
}

enum ScipRange {
    Single(SingleLineRange),
    Multi(MultiLineRange),
}

struct OutputDocument {
    language: String,
    relative_path: String,
    symbols: BTreeMap<String, SymbolInformation>,
    occurrences: Vec<Occurrence>,
}

impl OutputDocument {
    fn into_scip(self) -> Document {
        Document {
            language: self.language,
            relative_path: self.relative_path,
            occurrences: self.occurrences,
            symbols: self.symbols.into_values().collect(),
            position_encoding: EnumOrUnknown::new(
                PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
            ),
            ..Default::default()
        }
    }
}

struct ExportedSymbol {
    symbol: String,
    file: FileId,
    generated: bool,
    test: bool,
}

#[derive(Default)]
struct DefinitionFiles {
    definitions: BTreeSet<FileId>,
    declarations: BTreeSet<FileId>,
}

#[derive(Debug, Clone, Copy)]
enum RelationshipFlag {
    Reference,
    Implementation,
    TypeDefinition,
    Definition,
}

#[derive(Debug, Clone, Copy, Default)]
struct RelationshipFlags {
    is_reference: bool,
    is_implementation: bool,
    is_type_definition: bool,
    is_definition: bool,
}

impl RelationshipFlags {
    fn set(&mut self, flag: RelationshipFlag) {
        match flag {
            RelationshipFlag::Reference => self.is_reference = true,
            RelationshipFlag::Implementation => self.is_implementation = true,
            RelationshipFlag::TypeDefinition => self.is_type_definition = true,
            RelationshipFlag::Definition => self.is_definition = true,
        }
    }
}

fn preflight_limits(
    document: &NormalizedIrDocument,
    limits: ScipExportLimits,
) -> Result<(), ScipExportError> {
    require_limit(
        ScipExportResource::Documents,
        document.files.len(),
        limits.max_documents,
    )?;
    require_limit(
        ScipExportResource::Symbols,
        document.entities.len(),
        limits.max_symbols,
    )?;
    require_limit(
        ScipExportResource::Occurrences,
        document.occurrences.len(),
        limits.max_occurrences,
    )?;
    require_limit(
        ScipExportResource::Relationships,
        document.relations.len(),
        limits.max_relationships,
    )
}

fn materialize_sources<'a>(
    document: &NormalizedIrDocument,
    sources: &'a [ScipExportSource<'a>],
    limits: ScipExportLimits,
    cancellation: &Cancellation,
) -> Result<BTreeMap<FileId, SourceMaterial<'a>>, ScipExportError> {
    if document.files.len() != sources.len() {
        return Err(ScipExportError::SourceSetMismatch);
    }
    require_limit(
        ScipExportResource::Documents,
        sources.len(),
        limits.max_documents,
    )?;
    let files = document
        .files
        .iter()
        .map(|file| (file.id, file))
        .collect::<BTreeMap<_, _>>();
    let mut materialized = BTreeMap::new();
    let mut paths = BTreeSet::new();
    let mut total_bytes = 0_usize;
    for (index, source) in sources.iter().copied().enumerate() {
        check_periodically(index, cancellation)?;
        if source.repository != document.repository || source.generation != document.generation {
            return Err(ScipExportError::SourceIdentityMismatch);
        }
        if !is_normalized_relative_path(source.path) {
            return Err(ScipExportError::InvalidPath);
        }
        if !paths.insert(source.path) {
            return Err(ScipExportError::DuplicateSource);
        }
        require_limit(
            ScipExportResource::SourceBytes,
            source.content.len(),
            limits.max_source_bytes,
        )?;
        total_bytes = checked_add(
            total_bytes,
            source.content.len(),
            ScipExportResource::SourceBytes,
        )?;
        require_limit(
            ScipExportResource::SourceBytes,
            total_bytes,
            limits.max_total_source_bytes,
        )?;
        let file = files
            .get(&source.file)
            .ok_or(ScipExportError::MissingSource)?;
        let derived = derive_file(FileIdentity {
            repository: document.repository,
            path_identity: source.path.as_bytes(),
        })
        .id();
        if file.id != derived || file.path != source.path {
            return Err(ScipExportError::SourceIdentityMismatch);
        }
        if !file.encoding.eq_ignore_ascii_case("utf-8") {
            return Err(ScipExportError::UnsupportedSourceEncoding);
        }
        LanguageId::new(&file.language).map_err(|_| ScipExportError::InvalidLanguage)?;
        let expected_length =
            usize::try_from(file.byte_length).map_err(|_| ScipExportError::SourceMismatch)?;
        if expected_length != source.content.len()
            || file.content_hash != content_hash(source.content)
        {
            return Err(ScipExportError::SourceMismatch);
        }
        let text = std::str::from_utf8(source.content)
            .map_err(|_| ScipExportError::UnsupportedSourceEncoding)?;
        let value = SourceMaterial {
            path: source.path,
            text,
            generated: file.generated,
            line_starts: line_starts(source.content)?,
        };
        if materialized.insert(source.file, value).is_some() {
            return Err(ScipExportError::DuplicateSource);
        }
    }
    if materialized.len() != files.len() {
        return Err(ScipExportError::SourceSetMismatch);
    }
    Ok(materialized)
}

fn line_starts(content: &[u8]) -> Result<Vec<usize>, ScipExportError> {
    let newline_count = content.iter().filter(|byte| **byte == b'\n').count();
    let capacity = newline_count
        .checked_add(1)
        .ok_or(ScipExportError::Accounting)?;
    let mut starts = Vec::with_capacity(capacity);
    starts.push(0);
    for (index, byte) in content.iter().copied().enumerate() {
        if byte == b'\n' {
            starts.push(index.checked_add(1).ok_or(ScipExportError::Accounting)?);
        }
    }
    Ok(starts)
}

fn definition_files(document: &NormalizedIrDocument) -> BTreeMap<SymbolId, DefinitionFiles> {
    let mut definitions = BTreeMap::<SymbolId, DefinitionFiles>::new();
    for occurrence in &document.occurrences {
        if occurrence.confidence.get() == EXACT_CONFIDENCE
            && matches!(
                occurrence.role,
                OccurrenceRole::Definition | OccurrenceRole::Declaration
            )
            && let OccurrenceTarget::Resolved { symbol } = occurrence.target
        {
            let files = definitions.entry(symbol).or_default();
            match occurrence.role {
                OccurrenceRole::Definition => {
                    files.definitions.insert(occurrence.file);
                }
                OccurrenceRole::Declaration => {
                    files.declarations.insert(occurrence.file);
                }
                OccurrenceRole::Reference
                | OccurrenceRole::CallSite
                | OccurrenceRole::TypeUse
                | OccurrenceRole::ImportUse
                | OccurrenceRole::Write
                | OccurrenceRole::Read
                | OccurrenceRole::InheritanceUse
                | OccurrenceRole::ImplementationUse
                | OccurrenceRole::DecoratorUse
                | OccurrenceRole::MacroUse
                | OccurrenceRole::RouteUse
                | OccurrenceRole::TestUse
                | OccurrenceRole::Documentation
                | OccurrenceRole::StringEvidence => {}
            }
        }
    }
    definitions
}

fn project_entity_kind(kind: EntityKind) -> Option<Kind> {
    match kind {
        EntityKind::Package => Some(Kind::Package),
        EntityKind::File => Some(Kind::File),
        EntityKind::Module => Some(Kind::Module),
        EntityKind::Namespace => Some(Kind::Namespace),
        EntityKind::Class => Some(Kind::Class),
        EntityKind::Struct => Some(Kind::Struct),
        EntityKind::Enum => Some(Kind::Enum),
        EntityKind::Union => Some(Kind::Union),
        EntityKind::TypeAlias => Some(Kind::TypeAlias),
        EntityKind::Trait => Some(Kind::Trait),
        EntityKind::Interface => Some(Kind::Interface),
        EntityKind::Protocol => Some(Kind::Protocol),
        EntityKind::Function => Some(Kind::Function),
        EntityKind::Method => Some(Kind::Method),
        EntityKind::Constructor => Some(Kind::Constructor),
        EntityKind::Field => Some(Kind::Field),
        EntityKind::Property => Some(Kind::Property),
        EntityKind::Constant => Some(Kind::Constant),
        EntityKind::Variable => Some(Kind::Variable),
        EntityKind::Parameter => Some(Kind::Parameter),
        EntityKind::TypeParameter => Some(Kind::TypeParameter),
        EntityKind::Repository
        | EntityKind::Worktree
        | EntityKind::BuildTarget
        | EntityKind::Directory
        | EntityKind::Closure
        | EntityKind::Import
        | EntityKind::Export
        | EntityKind::Route
        | EntityKind::Service
        | EntityKind::MessageTopic
        | EntityKind::DatabaseObject
        | EntityKind::Test
        | EntityKind::ConfigurationKey
        | EntityKind::Commit
        | EntityKind::Change
        | EntityKind::CommunityView
        | EntityKind::ExternalSymbol => None,
    }
}

fn occurrence_roles(
    occurrence: &OccurrenceRecord,
    symbol: &ExportedSymbol,
    generated_source: bool,
) -> i32 {
    let mut roles = match occurrence.role {
        OccurrenceRole::Definition => DEFINITION_ROLE,
        OccurrenceRole::Declaration => FORWARD_DEFINITION_ROLE,
        OccurrenceRole::ImportUse => IMPORT_ROLE,
        OccurrenceRole::Write => WRITE_ROLE,
        OccurrenceRole::Read => READ_ROLE,
        OccurrenceRole::TestUse => TEST_ROLE,
        OccurrenceRole::Reference
        | OccurrenceRole::CallSite
        | OccurrenceRole::TypeUse
        | OccurrenceRole::InheritanceUse
        | OccurrenceRole::ImplementationUse
        | OccurrenceRole::DecoratorUse
        | OccurrenceRole::MacroUse
        | OccurrenceRole::RouteUse
        | OccurrenceRole::Documentation
        | OccurrenceRole::StringEvidence => 0,
    };
    if generated_source || symbol.generated {
        roles |= GENERATED_ROLE;
    }
    if symbol.test {
        roles |= TEST_ROLE;
    }
    roles
}

fn relationship_flag(predicate: RelationPredicate) -> Option<RelationshipFlag> {
    match predicate {
        RelationPredicate::RefersTo => Some(RelationshipFlag::Reference),
        RelationPredicate::Implements => Some(RelationshipFlag::Implementation),
        RelationPredicate::UsesType => Some(RelationshipFlag::TypeDefinition),
        RelationPredicate::BindsTo => Some(RelationshipFlag::Definition),
        RelationPredicate::Contains
        | RelationPredicate::Declares
        | RelationPredicate::DefinesAt
        | RelationPredicate::Calls
        | RelationPredicate::DispatchCandidate
        | RelationPredicate::Imports
        | RelationPredicate::Exports
        | RelationPredicate::ReturnsType
        | RelationPredicate::ParameterType
        | RelationPredicate::Extends
        | RelationPredicate::Satisfies
        | RelationPredicate::Embeds
        | RelationPredicate::MixesIn
        | RelationPredicate::Overrides
        | RelationPredicate::Reads
        | RelationPredicate::Writes
        | RelationPredicate::Throws
        | RelationPredicate::HandlesError
        | RelationPredicate::Tests
        | RelationPredicate::DependsOn
        | RelationPredicate::CallsRoute
        | RelationPredicate::ServesRoute
        | RelationPredicate::Publishes
        | RelationPredicate::Consumes
        | RelationPredicate::ReadsTable
        | RelationPredicate::WritesTable
        | RelationPredicate::CallsForeign
        | RelationPredicate::GeneratedFrom
        | RelationPredicate::ChangedIn
        | RelationPredicate::LineageRenamedFrom
        | RelationPredicate::LineageMovedFrom
        | RelationPredicate::LineageSplitFrom
        | RelationPredicate::LineageMergedFrom
        | RelationPredicate::CoChangedWith
        | RelationPredicate::OwnedBy
        | RelationPredicate::MemberOfView => None,
    }
}

fn occurrence_key(occurrence: &Occurrence) -> (i32, i32, i32, i32, &str, i32) {
    let (start_line, start_character, end_line, end_character) =
        match occurrence.typed_range.as_ref() {
            Some(scip::types::occurrence::Typed_range::SingleLineRange(range)) => (
                range.line,
                range.start_character,
                range.line,
                range.end_character,
            ),
            Some(scip::types::occurrence::Typed_range::MultiLineRange(range)) => (
                range.start_line,
                range.start_character,
                range.end_line,
                range.end_character,
            ),
            Some(_) | None => (i32::MAX, i32::MAX, i32::MAX, i32::MAX),
        };
    (
        start_line,
        start_character,
        end_line,
        end_character,
        occurrence.symbol.as_str(),
        occurrence.symbol_roles,
    )
}

fn relationship_key(relationship: &Relationship) -> (&str, bool, bool, bool, bool) {
    (
        relationship.symbol.as_str(),
        relationship.is_reference,
        relationship.is_implementation,
        relationship.is_type_definition,
        relationship.is_definition,
    )
}

fn is_normalized_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.starts_with('\\')
        && !value.contains('\\')
        && !value.split('/').any(|component| {
            component.is_empty() || component == "." || component == ".." || component.contains(':')
        })
}

fn check(cancellation: &Cancellation) -> Result<(), ScipExportError> {
    cancellation.check().map_err(|_| ScipExportError::Cancelled)
}

fn check_periodically(index: usize, cancellation: &Cancellation) -> Result<(), ScipExportError> {
    if index.is_multiple_of(CHECK_INTERVAL) {
        check(cancellation)?;
    }
    Ok(())
}

fn checked_add(
    left: usize,
    right: usize,
    resource: ScipExportResource,
) -> Result<usize, ScipExportError> {
    left.checked_add(right)
        .ok_or(ScipExportError::LimitExceeded {
            resource,
            observed: usize::MAX,
            limit: usize::MAX - 1,
        })
}

fn require_limit(
    resource: ScipExportResource,
    observed: usize,
    limit: usize,
) -> Result<(), ScipExportError> {
    if observed > limit {
        Err(ScipExportError::LimitExceeded {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

/// Bounded SCIP export resource named by a limit failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScipExportResource {
    /// Encoded protobuf bytes.
    EncodedBytes,
    /// Source document count.
    Documents,
    /// Normalized entities examined as potential SCIP symbols.
    Symbols,
    /// Normalized source occurrences.
    Occurrences,
    /// Normalized semantic relationships.
    Relationships,
    /// Exact source bytes supplied by the host.
    SourceBytes,
}

impl std::fmt::Display for ScipExportResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::EncodedBytes => "encoded_bytes",
            Self::Documents => "documents",
            Self::Symbols => "symbols",
            Self::Occurrences => "occurrences",
            Self::Relationships => "relationships",
            Self::SourceBytes => "source_bytes",
        })
    }
}

/// Invalid, inconsistent, unsupported, or cancelled SCIP export.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScipExportError {
    /// The document and request named different owners.
    #[error("SCIP export document identity does not match the request")]
    DocumentIdentityMismatch,
    /// A SCIP index requires at least one source document.
    #[error("SCIP export requires at least one document")]
    MissingDocuments,
    /// The document and host source sets differ.
    #[error("SCIP export document and source sets must match exactly")]
    SourceSetMismatch,
    /// A document file had no exact host source.
    #[error("SCIP export document has no exact host source")]
    MissingSource,
    /// Two host sources used the same file identity or path.
    #[error("SCIP export contains a duplicate source binding")]
    DuplicateSource,
    /// A path was not canonical and repository-relative.
    #[error("SCIP export contains an invalid repository-relative path")]
    InvalidPath,
    /// A source named a different repository, generation, file, or path identity.
    #[error("SCIP export source identity does not match normalized IR")]
    SourceIdentityMismatch,
    /// Exact host bytes differed in length or content hash.
    #[error("SCIP export source bytes do not match normalized IR")]
    SourceMismatch,
    /// A file or host source was not exact UTF-8.
    #[error("SCIP export source must use exact UTF-8 encoding")]
    UnsupportedSourceEncoding,
    /// A file language was not a bounded canonical SDK label.
    #[error("SCIP export file language is invalid")]
    InvalidLanguage,
    /// A source range was out of bounds, split UTF-8, or addressed a line ending.
    #[error("SCIP export occurrence contains an invalid source range")]
    InvalidRange,
    /// A generated Rootlight SCIP symbol violated the official symbol grammar.
    #[error("SCIP export symbol identity could not be encoded")]
    SymbolIdentity,
    /// Checked resource accounting failed.
    #[error("SCIP export resource accounting overflowed")]
    Accounting,
    /// A hard resource ceiling was exceeded.
    #[error("SCIP export {resource} count {observed} exceeds limit {limit}")]
    LimitExceeded {
        /// Resource that exceeded its ceiling.
        resource: ScipExportResource,
        /// Observed count or byte length.
        observed: usize,
        /// Configured hard ceiling.
        limit: usize,
    },
    /// The operation was cancelled or its deadline expired.
    #[error("SCIP export was cancelled")]
    Cancelled,
    /// The supplied document violated normalized-IR invariants.
    #[error("SCIP export received invalid normalized IR: {0}")]
    InvalidDocument(IrDocumentValidationError),
    /// Official protobuf encoding failed.
    #[error("SCIP protobuf encoding failed: {0}")]
    Encode(protobuf::Error),
}
