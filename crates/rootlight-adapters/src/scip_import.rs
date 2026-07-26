//! Bounded SCIP protobuf import into generation-bound normalized IR.
//!
//! Import requires exact source bytes from the host, never reads repository
//! paths itself, and rejects ranges that cannot be mapped without ambiguity.

use std::collections::{BTreeMap, BTreeSet};

use protobuf::Message;
use rootlight_adapter_sdk::LanguageId;
use rootlight_cancel::Cancellation;
use rootlight_ids::{
    FactId, FileIdentity, GenerationId, RepositoryId, SymbolId, content_hash, derive_file,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, ContainerRef, CoverageRecord, CoverageScope,
    CoverageStatus, EntityFlag, EntityKind, EntityRecord, EntityVisibility, EvidenceKind,
    ExtensionSupport, FactDomain, FactEvidence, FileIdentityClaim, FileRecord,
    IrDocumentValidationError, IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord, RelationEndpoint,
    RelationPredicate, RelationRecord, SourceRef, SourceSpan, SymbolIdentityClaim,
    canonicalize_ir_document, derive_coverage_record_id, derive_occurrence_record_id,
    derive_provenance_record_id, derive_relation_record_id, new_file_identity_claim_envelope,
    new_symbol_identity_claim_envelope,
};
use scip::types::{
    Document, Index, Occurrence, PositionEncoding, Relationship, SymbolInformation,
    occurrence::Typed_range, symbol_information::Kind as ScipKind,
};

use crate::ADAPTER_VERSION;

const SCIP_TIER: AnalysisTier = AnalysisTier::TierB;
const EXACT_CONFIDENCE: u16 = 1_000;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 4_096;
const MAX_SYMBOLS: usize = 200_000;
const MAX_OCCURRENCES: usize = 1_000_000;
const MAX_RELATIONSHIPS: usize = 500_000;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const CHECK_INTERVAL: usize = 128;
const DEFINITION_ROLE: i32 = 0x1;
const IMPORT_ROLE: i32 = 0x2;
const WRITE_ROLE: i32 = 0x4;
const READ_ROLE: i32 = 0x8;
const GENERATED_ROLE: i32 = 0x10;
const TEST_ROLE: i32 = 0x20;
const FORWARD_DEFINITION_ROLE: i32 = 0x40;

/// Fixed hard limits applied before SCIP facts are materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScipImportLimits {
    max_index_bytes: usize,
    max_documents: usize,
    max_symbols: usize,
    max_occurrences: usize,
    max_relationships: usize,
    max_source_bytes: usize,
    max_total_source_bytes: usize,
}

impl ScipImportLimits {
    /// Creates an import policy no broader than the process-wide hard ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`ScipImportError::LimitExceeded`] when any requested ceiling is
    /// broader than the corresponding process-wide maximum.
    pub fn new(
        max_index_bytes: usize,
        max_documents: usize,
        max_symbols: usize,
        max_occurrences: usize,
        max_relationships: usize,
        max_source_bytes: usize,
        max_total_source_bytes: usize,
    ) -> Result<Self, ScipImportError> {
        require_limit(ScipResource::IndexBytes, max_index_bytes, MAX_INDEX_BYTES)?;
        require_limit(ScipResource::Documents, max_documents, MAX_DOCUMENTS)?;
        require_limit(ScipResource::Symbols, max_symbols, MAX_SYMBOLS)?;
        require_limit(ScipResource::Occurrences, max_occurrences, MAX_OCCURRENCES)?;
        require_limit(
            ScipResource::Relationships,
            max_relationships,
            MAX_RELATIONSHIPS,
        )?;
        require_limit(
            ScipResource::SourceBytes,
            max_source_bytes,
            MAX_SOURCE_BYTES,
        )?;
        require_limit(
            ScipResource::SourceBytes,
            max_total_source_bytes,
            MAX_TOTAL_SOURCE_BYTES,
        )?;
        Ok(Self {
            max_index_bytes,
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
    pub const fn max_index_bytes(self) -> usize {
        self.max_index_bytes
    }

    /// Returns the maximum document count.
    #[must_use]
    pub const fn max_documents(self) -> usize {
        self.max_documents
    }

    /// Returns the maximum combined internal and external symbol count.
    #[must_use]
    pub const fn max_symbols(self) -> usize {
        self.max_symbols
    }

    /// Returns the maximum occurrence count.
    #[must_use]
    pub const fn max_occurrences(self) -> usize {
        self.max_occurrences
    }

    /// Returns the maximum declared relationship count.
    #[must_use]
    pub const fn max_relationships(self) -> usize {
        self.max_relationships
    }

    /// Returns the maximum byte length of one source document.
    #[must_use]
    pub const fn max_source_bytes(self) -> usize {
        self.max_source_bytes
    }

    /// Returns the maximum combined byte length of source documents.
    #[must_use]
    pub const fn max_total_source_bytes(self) -> usize {
        self.max_total_source_bytes
    }
}

impl Default for ScipImportLimits {
    fn default() -> Self {
        Self {
            max_index_bytes: MAX_INDEX_BYTES,
            max_documents: MAX_DOCUMENTS,
            max_symbols: MAX_SYMBOLS,
            max_occurrences: MAX_OCCURRENCES,
            max_relationships: MAX_RELATIONSHIPS,
            max_source_bytes: MAX_SOURCE_BYTES,
            max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
        }
    }
}

/// Exact immutable source supplied to a SCIP import.
#[derive(Debug, Clone, Copy)]
pub struct ScipImportSource<'a> {
    path: &'a str,
    content: &'a [u8],
    generated: bool,
}

impl<'a> ScipImportSource<'a> {
    /// Creates one source binding without reading from the filesystem.
    ///
    /// Validation against repository, generation, language, content, and import
    /// limits occurs atomically in [`import_scip_index`].
    #[must_use]
    pub const fn new(path: &'a str, content: &'a [u8], generated: bool) -> Self {
        Self {
            path,
            content,
            generated,
        }
    }

    /// Returns the canonical repository-relative path.
    #[must_use]
    pub const fn path(self) -> &'a str {
        self.path
    }

    /// Returns exact immutable UTF-8 source bytes.
    #[must_use]
    pub const fn content(self) -> &'a [u8] {
        self.content
    }

    /// Returns whether the host identified this source as generated.
    #[must_use]
    pub const fn generated(self) -> bool {
        self.generated
    }
}

/// Aggregate accounting for one successful SCIP import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScipImportReport {
    documents: usize,
    symbols: usize,
    occurrences: usize,
    relationships: usize,
    skipped_symbols: usize,
    skipped_occurrences: usize,
    skipped_relationships: usize,
    ignored_diagnostics: usize,
    external_symbols: usize,
}

impl ScipImportReport {
    /// Returns the imported document count.
    #[must_use]
    pub const fn documents(self) -> usize {
        self.documents
    }

    /// Returns the materialized entity count.
    #[must_use]
    pub const fn symbols(self) -> usize {
        self.symbols
    }

    /// Returns the materialized semantic occurrence count.
    #[must_use]
    pub const fn occurrences(self) -> usize {
        self.occurrences
    }

    /// Returns the materialized relationship count.
    #[must_use]
    pub const fn relationships(self) -> usize {
        self.relationships
    }

    /// Returns symbols omitted because their kind was not safely projectable.
    #[must_use]
    pub const fn skipped_symbols(self) -> usize {
        self.skipped_symbols
    }

    /// Returns syntax-only occurrences omitted from semantic IR.
    #[must_use]
    pub const fn skipped_occurrences(self) -> usize {
        self.skipped_occurrences
    }

    /// Returns relationships whose endpoint or role was not safely projectable.
    #[must_use]
    pub const fn skipped_relationships(self) -> usize {
        self.skipped_relationships
    }

    /// Returns SCIP diagnostics retained only as explicit coverage loss.
    #[must_use]
    pub const fn ignored_diagnostics(self) -> usize {
        self.ignored_diagnostics
    }

    /// Returns external symbols retained only as explicit coverage loss.
    #[must_use]
    pub const fn external_symbols(self) -> usize {
        self.external_symbols
    }
}

/// Canonical normalized IR and its bounded import accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipImportOutcome {
    document: NormalizedIrDocument,
    report: ScipImportReport,
}

impl ScipImportOutcome {
    /// Returns the canonical imported IR.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        &self.document
    }

    /// Returns bounded import accounting.
    #[must_use]
    pub const fn report(&self) -> ScipImportReport {
        self.report
    }

    /// Consumes the outcome and returns canonical imported IR.
    #[must_use]
    pub fn into_document(self) -> NormalizedIrDocument {
        self.document
    }
}

/// Immutable context required to import one SCIP index.
pub struct ScipImportRequest<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    build_context: BuildContextIdentity,
    sources: &'a [ScipImportSource<'a>],
    limits: ScipImportLimits,
    ir_limits: &'a IrLimits,
    cancellation: &'a Cancellation,
}

impl<'a> ScipImportRequest<'a> {
    /// Creates a request with fixed safe import limits.
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        generation: GenerationId,
        build_context: BuildContextIdentity,
        sources: &'a [ScipImportSource<'a>],
        ir_limits: &'a IrLimits,
        cancellation: &'a Cancellation,
    ) -> Self {
        Self {
            repository,
            generation,
            build_context,
            sources,
            limits: ScipImportLimits {
                max_index_bytes: MAX_INDEX_BYTES,
                max_documents: MAX_DOCUMENTS,
                max_symbols: MAX_SYMBOLS,
                max_occurrences: MAX_OCCURRENCES,
                max_relationships: MAX_RELATIONSHIPS,
                max_source_bytes: MAX_SOURCE_BYTES,
                max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
            },
            ir_limits,
            cancellation,
        }
    }

    /// Replaces fixed import ceilings with an explicitly supplied policy.
    #[must_use]
    pub const fn with_limits(mut self, limits: ScipImportLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Returns the repository that will own every imported fact.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation that will own every imported fact.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the declarative build-context identity.
    #[must_use]
    pub const fn build_context(&self) -> BuildContextIdentity {
        self.build_context
    }

    /// Returns exact source bindings supplied by the host.
    #[must_use]
    pub const fn sources(&self) -> &[ScipImportSource<'a>] {
        self.sources
    }

    /// Returns hard SCIP import limits.
    #[must_use]
    pub const fn limits(&self) -> ScipImportLimits {
        self.limits
    }
}

/// Imports one official SCIP protobuf index into normalized IR.
///
/// The importer only consumes the supplied byte slices. It does not execute an
/// indexer, invoke a build, access the network, or open paths named by the SCIP
/// payload.
///
/// # Errors
///
/// Returns [`ScipImportError`] when encoded input, source bindings, ranges,
/// identities, resource limits, cancellation, or normalized-IR invariants fail.
pub fn import_scip_index(
    encoded: &[u8],
    request: ScipImportRequest<'_>,
) -> Result<ScipImportOutcome, ScipImportError> {
    check(request.cancellation)?;
    if encoded.is_empty() {
        return Err(ScipImportError::EmptyIndex);
    }
    require_limit(
        ScipResource::IndexBytes,
        encoded.len(),
        request.limits.max_index_bytes,
    )?;
    let index = Index::parse_from_bytes(encoded)?;
    check(request.cancellation)?;
    let identity = ImportIdentity {
        repository: request.repository,
        generation: request.generation,
        build_context: request.build_context,
    };
    Importer::new(
        &index,
        encoded,
        identity,
        request.sources,
        request.limits,
        request.ir_limits,
        request.cancellation,
    )?
    .import()
}

#[derive(Debug, Clone, Copy)]
struct ImportIdentity {
    repository: RepositoryId,
    generation: GenerationId,
    build_context: BuildContextIdentity,
}

struct Importer<'a> {
    index: &'a Index,
    repository: RepositoryId,
    generation: GenerationId,
    build_context: BuildContextIdentity,
    sources: BTreeMap<&'a str, SourceMaterial<'a>>,
    producer: ProducerIdentity,
    index_digest: rootlight_ids::ContentHash,
    limits: ScipImportLimits,
    ir_limits: &'a IrLimits,
    cancellation: &'a Cancellation,
}

impl<'a> Importer<'a> {
    fn new(
        index: &'a Index,
        encoded: &[u8],
        identity: ImportIdentity,
        sources: &'a [ScipImportSource<'a>],
        limits: ScipImportLimits,
        ir_limits: &'a IrLimits,
        cancellation: &'a Cancellation,
    ) -> Result<Self, ScipImportError> {
        require_limit(
            ScipResource::Documents,
            index.documents.len(),
            limits.max_documents,
        )?;
        if index.documents.is_empty() {
            return Err(ScipImportError::MissingDocuments);
        }
        if index.metadata.is_none() {
            return Err(ScipImportError::MissingMetadata);
        }
        let metadata = index
            .metadata
            .as_ref()
            .ok_or(ScipImportError::MissingMetadata)?;
        if !matches!(metadata.text_document_encoding.value(), 0 | 1) {
            return Err(ScipImportError::UnsupportedTextEncoding);
        }
        let index_digest = content_hash(encoded);
        let producer = ProducerIdentity::new("rootlight-adapters", ADAPTER_VERSION, index_digest)
            .map_err(|_| ScipImportError::Identity)?;
        let sources = materialize_sources(
            identity.repository,
            identity.generation,
            sources,
            limits,
            cancellation,
        )?;
        Ok(Self {
            index,
            repository: identity.repository,
            generation: identity.generation,
            build_context: identity.build_context,
            sources,
            producer,
            index_digest,
            limits,
            ir_limits,
            cancellation,
        })
    }

    fn import(self) -> Result<ScipImportOutcome, ScipImportError> {
        self.preflight()?;
        let mut document = NormalizedIrDocument::empty(self.repository, self.generation);
        let mut report = ScipImportReport {
            documents: self.index.documents.len(),
            symbols: 0,
            occurrences: 0,
            relationships: 0,
            skipped_symbols: 0,
            skipped_occurrences: 0,
            skipped_relationships: relationship_unit_count(&self.index.external_symbols)?,
            ignored_diagnostics: 0,
            external_symbols: self.index.external_symbols.len(),
        };
        report.skipped_symbols = report.external_symbols;

        let mut document_contexts = Vec::with_capacity(self.index.documents.len());
        for (index, scip_document) in self.index.documents.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let source = self
                .sources
                .get(scip_document.relative_path.as_str())
                .ok_or(ScipImportError::MissingSource)?;
            let language = normalize_language(&scip_document.language)?;
            let provenance =
                self.materialize_provenance(source, language.as_str(), &mut document)?;
            self.materialize_file(source, language.as_str(), provenance, &mut document)?;
            document_contexts.push(DocumentContext {
                scip: scip_document,
                source,
                language,
                provenance,
            });
        }

        let mut symbols = BTreeMap::<String, MaterializedSymbol>::new();
        for context in &document_contexts {
            for (symbol_index, information) in context.scip.symbols.iter().enumerate() {
                check_periodically(symbol_index, self.cancellation)?;
                let Some(kind) = project_entity_kind(information) else {
                    report.skipped_symbols =
                        checked_add(report.skipped_symbols, 1, ScipResource::Symbols)?;
                    continue;
                };
                let materialized =
                    self.materialize_symbol(information, context, kind, &mut document)?;
                if symbols
                    .insert(information.symbol.clone(), materialized)
                    .is_some()
                {
                    return Err(ScipImportError::DuplicateSymbol);
                }
                report.symbols = checked_add(report.symbols, 1, ScipResource::Symbols)?;
            }
        }

        for context in &document_contexts {
            self.materialize_occurrences(context, &symbols, &mut report, &mut document)?;
        }
        self.materialize_relationships(&document_contexts, &symbols, &mut report, &mut document)?;
        self.materialize_coverage(&document_contexts, &report, &mut document)?;
        check(self.cancellation)?;
        let document =
            canonicalize_ir_document(document, self.ir_limits, &ExtensionSupport::default())
                .map_err(ScipImportError::InvalidDocument)?;
        Ok(ScipImportOutcome { document, report })
    }

    fn preflight(&self) -> Result<(), ScipImportError> {
        if self.sources.len() != self.index.documents.len() {
            return Err(ScipImportError::SourceSetMismatch);
        }
        let mut paths = BTreeSet::new();
        let mut symbol_count = self.index.external_symbols.len();
        let mut occurrence_count = 0_usize;
        let mut relationship_count = relationship_unit_count(&self.index.external_symbols)?;
        for (index, document) in self.index.documents.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            if !is_normalized_relative_path(&document.relative_path) {
                return Err(ScipImportError::InvalidPath);
            }
            if !paths.insert(document.relative_path.as_str()) {
                return Err(ScipImportError::DuplicateDocument);
            }
            let source = self
                .sources
                .get(document.relative_path.as_str())
                .ok_or(ScipImportError::MissingSource)?;
            if !document.text.is_empty() && document.text.as_bytes() != source.content {
                return Err(ScipImportError::EmbeddedTextMismatch);
            }
            normalize_language(&document.language)?;
            document
                .position_encoding
                .enum_value()
                .map_err(|_| ScipImportError::UnsupportedPositionEncoding)
                .and_then(|encoding| match encoding {
                    PositionEncoding::UTF8CodeUnitOffsetFromLineStart
                    | PositionEncoding::UTF16CodeUnitOffsetFromLineStart
                    | PositionEncoding::UTF32CodeUnitOffsetFromLineStart => Ok(()),
                    PositionEncoding::UnspecifiedPositionEncoding => {
                        Err(ScipImportError::UnsupportedPositionEncoding)
                    }
                })?;
            symbol_count =
                checked_add(symbol_count, document.symbols.len(), ScipResource::Symbols)?;
            occurrence_count = checked_add(
                occurrence_count,
                document.occurrences.len(),
                ScipResource::Occurrences,
            )?;
            relationship_count = checked_add(
                relationship_count,
                relationship_unit_count(&document.symbols)?,
                ScipResource::Relationships,
            )?;
        }
        require_limit(ScipResource::Symbols, symbol_count, self.limits.max_symbols)?;
        require_limit(
            ScipResource::Occurrences,
            occurrence_count,
            self.limits.max_occurrences,
        )?;
        require_limit(
            ScipResource::Relationships,
            relationship_count,
            self.limits.max_relationships,
        )
    }

    fn materialize_provenance(
        &self,
        source: &SourceMaterial<'_>,
        language: &str,
        document: &mut NormalizedIrDocument,
    ) -> Result<FactId, ScipImportError> {
        let mut record = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository: self.repository,
            generation: self.generation,
            producer_kind: ProducerKind::Scip,
            producer: self.producer.clone(),
            binary_digest: self.index_digest,
            frontend_version: Some("scip-0.9.0".to_owned()),
            language: language.to_owned(),
            tier: SCIP_TIER,
            build_context: self.build_context,
            input_sources: vec![source.source.clone()],
            evidence_sources: vec![source.source.clone()],
            derivation_parents: Vec::new(),
            rule: None,
        };
        record.id = derive_provenance_record_id(&record).map_err(|_| ScipImportError::Identity)?;
        let id = record.id;
        document.provenance.push(record);
        Ok(id)
    }

    fn materialize_file(
        &self,
        source: &SourceMaterial<'_>,
        language: &str,
        provenance: FactId,
        document: &mut NormalizedIrDocument,
    ) -> Result<(), ScipImportError> {
        let byte_length =
            u64::try_from(source.content.len()).map_err(|_| ScipImportError::Accounting)?;
        document.files.push(FileRecord {
            id: source.source.span().file(),
            repository: self.repository,
            generation: self.generation,
            path: source.path.to_owned(),
            path_locator: None,
            content_hash: source.source.content_hash(),
            byte_length,
            language: language.to_owned(),
            encoding: "utf-8".to_owned(),
            generated: source.generated,
            provenance,
            evidence: direct_evidence(source.source.clone()),
        });
        let claim = FileIdentityClaim {
            file: source.source.span().file(),
            repository: self.repository,
            path: source.path.to_owned(),
            path_identity: source.path.as_bytes().to_vec(),
            content_hash: source.source.content_hash(),
            byte_length,
        };
        document.extensions.push(
            new_file_identity_claim_envelope(
                &claim,
                self.generation,
                provenance,
                source.source.clone(),
            )
            .map_err(|_| ScipImportError::Identity)?,
        );
        Ok(())
    }

    fn materialize_symbol(
        &self,
        information: &SymbolInformation,
        context: &DocumentContext<'_>,
        kind: EntityKind,
        document: &mut NormalizedIrDocument,
    ) -> Result<MaterializedSymbol, ScipImportError> {
        require_nonempty_bounded_string(&information.symbol, self.ir_limits)?;
        let display_name = if information.display_name.is_empty() {
            information.symbol.as_str()
        } else {
            information.display_name.as_str()
        };
        require_nonempty_bounded_string(display_name, self.ir_limits)?;
        let file = context.source.source.span().file();
        let mut container_identity = Vec::with_capacity(1 + file.as_bytes().len());
        container_identity.push(1);
        container_identity.extend_from_slice(file.as_bytes());
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: self.repository,
            language: context.language.as_str().to_owned(),
            kind,
            container: Some(ContainerRef::File(file)),
            container_identity,
            declared_identity: information.symbol.clone(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: self.build_context.digest().as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let flags = symbol_flags(context.scip, &information.symbol, context.source.generated);
        document.entities.push(EntityRecord {
            id: claim.symbol,
            repository: self.repository,
            generation: self.generation,
            kind,
            language: context.language.as_str().to_owned(),
            tier: SCIP_TIER,
            canonical_name: information.symbol.clone(),
            display_name: display_name.to_owned(),
            qualified_name: information.symbol.clone(),
            container: claim.container,
            visibility: EntityVisibility::Unknown,
            flags,
            provenance: context.provenance,
            evidence: direct_evidence(context.source.source.clone()),
        });
        document.extensions.push(
            new_symbol_identity_claim_envelope(
                &claim,
                self.generation,
                context.provenance,
                context.source.source.clone(),
            )
            .map_err(|_| ScipImportError::Identity)?,
        );
        Ok(MaterializedSymbol {
            id: claim.symbol,
            provenance: context.provenance,
            source: context.source.source.clone(),
        })
    }

    fn materialize_occurrences(
        &self,
        context: &DocumentContext<'_>,
        symbols: &BTreeMap<String, MaterializedSymbol>,
        report: &mut ScipImportReport,
        document: &mut NormalizedIrDocument,
    ) -> Result<(), ScipImportError> {
        let encoding = context
            .scip
            .position_encoding
            .enum_value()
            .map_err(|_| ScipImportError::UnsupportedPositionEncoding)?;
        for (index, occurrence) in context.scip.occurrences.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            report.ignored_diagnostics = checked_add(
                report.ignored_diagnostics,
                occurrence.diagnostics.len(),
                ScipResource::Diagnostics,
            )?;
            if occurrence.symbol.is_empty() {
                report.skipped_occurrences =
                    checked_add(report.skipped_occurrences, 1, ScipResource::Occurrences)?;
                continue;
            }
            require_nonempty_bounded_string(&occurrence.symbol, self.ir_limits)?;
            let span = occurrence_span(occurrence, context.source, encoding)?;
            let source = source_for_span(&context.source.source, span);
            let bytes = context
                .source
                .content
                .get(
                    usize::try_from(span.start_byte()).map_err(|_| ScipImportError::InvalidRange)?
                        ..usize::try_from(span.end_byte())
                            .map_err(|_| ScipImportError::InvalidRange)?,
                )
                .ok_or(ScipImportError::InvalidRange)?;
            let target = symbols.get(&occurrence.symbol).map_or_else(
                || OccurrenceTarget::Unresolved {
                    text_hash: content_hash(occurrence.symbol.as_bytes()),
                },
                |symbol| OccurrenceTarget::Resolved { symbol: symbol.id },
            );
            let mut record = OccurrenceRecord {
                id: FactId::from_bytes([0; 20]),
                repository: self.repository,
                generation: self.generation,
                file: span.file(),
                source: source.clone(),
                role: occurrence_role(occurrence),
                enclosing: None,
                target,
                syntactic_text_hash: content_hash(bytes),
                syntax_kind: format!("scip:{}", occurrence.syntax_kind.value()),
                provenance: context.provenance,
                confidence: exact_confidence()?,
                evidence: direct_evidence(source),
            };
            record.id =
                derive_occurrence_record_id(&record).map_err(|_| ScipImportError::Identity)?;
            document.occurrences.push(record);
            report.occurrences = checked_add(report.occurrences, 1, ScipResource::Occurrences)?;
        }
        Ok(())
    }

    fn materialize_relationships(
        &self,
        contexts: &[DocumentContext<'_>],
        symbols: &BTreeMap<String, MaterializedSymbol>,
        report: &mut ScipImportReport,
        document: &mut NormalizedIrDocument,
    ) -> Result<(), ScipImportError> {
        for context in contexts {
            for information in &context.scip.symbols {
                let Some(subject) = symbols.get(&information.symbol) else {
                    report.skipped_relationships = checked_add(
                        report.skipped_relationships,
                        relationship_count(&information.relationships)?,
                        ScipResource::Relationships,
                    )?;
                    continue;
                };
                for relationship in &information.relationships {
                    self.materialize_relationship(
                        relationship,
                        subject,
                        symbols,
                        report,
                        document,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn materialize_relationship(
        &self,
        relationship: &Relationship,
        subject: &MaterializedSymbol,
        symbols: &BTreeMap<String, MaterializedSymbol>,
        report: &mut ScipImportReport,
        document: &mut NormalizedIrDocument,
    ) -> Result<(), ScipImportError> {
        let Some(object) = symbols.get(&relationship.symbol) else {
            report.skipped_relationships =
                checked_add(report.skipped_relationships, 1, ScipResource::Relationships)?;
            return Ok(());
        };
        let predicates = relationship_predicates(relationship);
        if predicates.is_empty() {
            report.skipped_relationships =
                checked_add(report.skipped_relationships, 1, ScipResource::Relationships)?;
            return Ok(());
        }
        for predicate in predicates {
            let mut record = RelationRecord {
                id: FactId::from_bytes([0; 20]),
                repository: self.repository,
                generation: self.generation,
                subject: RelationEndpoint::Entity(subject.id),
                predicate,
                object: RelationEndpoint::Entity(object.id),
                confidence: exact_confidence()?,
                evidence_kind: EvidenceKind::Scip,
                provenance: subject.provenance,
                evidence: direct_evidence(subject.source.clone()),
            };
            record.id =
                derive_relation_record_id(&record).map_err(|_| ScipImportError::Identity)?;
            document.relations.push(record);
            report.relationships =
                checked_add(report.relationships, 1, ScipResource::Relationships)?;
        }
        Ok(())
    }

    fn materialize_coverage(
        &self,
        contexts: &[DocumentContext<'_>],
        report: &ScipImportReport,
        document: &mut NormalizedIrDocument,
    ) -> Result<(), ScipImportError> {
        for (index, context) in contexts.iter().enumerate() {
            check_periodically(index, self.cancellation)?;
            let mapped_symbols = document
                .entities
                .iter()
                .filter(|entity| entity.provenance == context.provenance)
                .count();
            let mapped_occurrences = document
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.provenance == context.provenance)
                .count();
            let mapped_relations = document
                .relations
                .iter()
                .filter(|relation| relation.provenance == context.provenance)
                .count();
            let diagnostics =
                context
                    .scip
                    .occurrences
                    .iter()
                    .try_fold(0_usize, |total, occurrence| {
                        checked_add(
                            total,
                            occurrence.diagnostics.len(),
                            ScipResource::Diagnostics,
                        )
                    })?;
            let domains = [
                (FactDomain::Files, 1, 1, 0),
                (
                    FactDomain::Entities,
                    context.scip.symbols.len(),
                    mapped_symbols,
                    context.scip.symbols.len().saturating_sub(mapped_symbols),
                ),
                (
                    FactDomain::Occurrences,
                    context.scip.occurrences.len(),
                    mapped_occurrences,
                    context
                        .scip
                        .occurrences
                        .len()
                        .saturating_sub(mapped_occurrences),
                ),
                (
                    FactDomain::Relations,
                    relationship_unit_count(&context.scip.symbols)?,
                    mapped_relations,
                    relationship_unit_count(&context.scip.symbols)?
                        .saturating_sub(mapped_relations),
                ),
                (FactDomain::Provenance, 1, 1, 0),
                (FactDomain::SourceMappings, 0, 0, 0),
                (FactDomain::Diagnostics, diagnostics, 0, diagnostics),
                (
                    FactDomain::Extensions,
                    mapped_symbols.saturating_add(1),
                    mapped_symbols.saturating_add(1),
                    0,
                ),
            ];
            for (domain, discovered, indexed, skipped) in domains {
                let status = if skipped == 0 {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                };
                let mut record = CoverageRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: self.repository,
                    generation: self.generation,
                    scope: CoverageScope::File(context.source.source.span().file()),
                    domain,
                    tier: SCIP_TIER,
                    status,
                    discovered: u64::try_from(discovered)
                        .map_err(|_| ScipImportError::Accounting)?,
                    indexed: u64::try_from(indexed).map_err(|_| ScipImportError::Accounting)?,
                    skipped: u64::try_from(skipped).map_err(|_| ScipImportError::Accounting)?,
                    provenance: context.provenance,
                    evidence: direct_evidence(context.source.source.clone()),
                };
                record.id =
                    derive_coverage_record_id(&record).map_err(|_| ScipImportError::Identity)?;
                document.coverage_records.push(record);
            }
        }
        debug_assert_eq!(report.documents, contexts.len());
        Ok(())
    }
}

struct SourceMaterial<'a> {
    path: &'a str,
    content: &'a [u8],
    text: &'a str,
    generated: bool,
    source: SourceRef,
    line_starts: Vec<usize>,
}

struct DocumentContext<'a> {
    scip: &'a Document,
    source: &'a SourceMaterial<'a>,
    language: LanguageId,
    provenance: FactId,
}

struct MaterializedSymbol {
    id: SymbolId,
    provenance: FactId,
    source: SourceRef,
}

fn materialize_sources<'a>(
    repository: RepositoryId,
    generation: GenerationId,
    sources: &'a [ScipImportSource<'a>],
    limits: ScipImportLimits,
    cancellation: &Cancellation,
) -> Result<BTreeMap<&'a str, SourceMaterial<'a>>, ScipImportError> {
    require_limit(ScipResource::Documents, sources.len(), limits.max_documents)?;
    let mut total_bytes = 0_usize;
    let mut materialized = BTreeMap::new();
    for (index, source) in sources.iter().copied().enumerate() {
        check_periodically(index, cancellation)?;
        if !is_normalized_relative_path(source.path) {
            return Err(ScipImportError::InvalidPath);
        }
        require_limit(
            ScipResource::SourceBytes,
            source.content.len(),
            limits.max_source_bytes,
        )?;
        total_bytes = checked_add(total_bytes, source.content.len(), ScipResource::SourceBytes)?;
        require_limit(
            ScipResource::SourceBytes,
            total_bytes,
            limits.max_total_source_bytes,
        )?;
        let text =
            std::str::from_utf8(source.content).map_err(|_| ScipImportError::NonUtf8Source)?;
        let file = derive_file(FileIdentity {
            repository,
            path_identity: source.path.as_bytes(),
        })
        .id();
        let end = u64::try_from(source.content.len()).map_err(|_| ScipImportError::Accounting)?;
        let span = SourceSpan::new(file, 0, end).map_err(|_| ScipImportError::InvalidRange)?;
        let reference = SourceRef::new(
            repository,
            generation,
            span,
            content_hash(source.content),
            None,
        );
        let value = SourceMaterial {
            path: source.path,
            content: source.content,
            text,
            generated: source.generated,
            source: reference,
            line_starts: line_starts(source.content)?,
        };
        if materialized.insert(source.path, value).is_some() {
            return Err(ScipImportError::DuplicateSource);
        }
    }
    Ok(materialized)
}

fn line_starts(content: &[u8]) -> Result<Vec<usize>, ScipImportError> {
    let newline_count = content.iter().filter(|byte| **byte == b'\n').count();
    let capacity = newline_count
        .checked_add(1)
        .ok_or(ScipImportError::Accounting)?;
    let mut starts = Vec::with_capacity(capacity);
    starts.push(0);
    for (index, byte) in content.iter().copied().enumerate() {
        if byte == b'\n' {
            starts.push(index.checked_add(1).ok_or(ScipImportError::Accounting)?);
        }
    }
    Ok(starts)
}

fn occurrence_span(
    occurrence: &Occurrence,
    source: &SourceMaterial<'_>,
    encoding: PositionEncoding,
) -> Result<SourceSpan, ScipImportError> {
    let (start_line, start_character, end_line, end_character) = occurrence_range(occurrence)?;
    let start = position_to_byte(source, start_line, start_character, encoding)?;
    let end = position_to_byte(source, end_line, end_character, encoding)?;
    let start = u64::try_from(start).map_err(|_| ScipImportError::InvalidRange)?;
    let end = u64::try_from(end).map_err(|_| ScipImportError::InvalidRange)?;
    SourceSpan::new(source.source.span().file(), start, end)
        .map_err(|_| ScipImportError::InvalidRange)
}

fn occurrence_range(occurrence: &Occurrence) -> Result<(i32, i32, i32, i32), ScipImportError> {
    match &occurrence.typed_range {
        Some(Typed_range::SingleLineRange(range)) => Ok((
            range.line,
            range.start_character,
            range.line,
            range.end_character,
        )),
        Some(Typed_range::MultiLineRange(range)) => Ok((
            range.start_line,
            range.start_character,
            range.end_line,
            range.end_character,
        )),
        Some(_) => Err(ScipImportError::InvalidRange),
        None => deprecated_occurrence_range(occurrence),
    }
}

// The official schema requires consumers to retain compatibility with the
// legacy packed range when a typed range is absent.
#[allow(deprecated)]
fn deprecated_occurrence_range(
    occurrence: &Occurrence,
) -> Result<(i32, i32, i32, i32), ScipImportError> {
    match occurrence.range.as_slice() {
        [line, start_character, end_character] => {
            Ok((*line, *start_character, *line, *end_character))
        }
        [start_line, start_character, end_line, end_character] => {
            Ok((*start_line, *start_character, *end_line, *end_character))
        }
        _ => Err(ScipImportError::InvalidRange),
    }
}

fn position_to_byte(
    source: &SourceMaterial<'_>,
    line: i32,
    character: i32,
    encoding: PositionEncoding,
) -> Result<usize, ScipImportError> {
    let line = usize::try_from(line).map_err(|_| ScipImportError::InvalidRange)?;
    let character = usize::try_from(character).map_err(|_| ScipImportError::InvalidRange)?;
    let start = *source
        .line_starts
        .get(line)
        .ok_or(ScipImportError::InvalidRange)?;
    let next = source
        .line_starts
        .get(line.saturating_add(1))
        .copied()
        .unwrap_or(source.content.len());
    let mut end = next;
    if end > start && source.content.get(end - 1) == Some(&b'\n') {
        end -= 1;
    }
    if end > start && source.content.get(end - 1) == Some(&b'\r') {
        end -= 1;
    }
    let line_text = source
        .text
        .get(start..end)
        .ok_or(ScipImportError::InvalidRange)?;
    let relative = match encoding {
        PositionEncoding::UTF8CodeUnitOffsetFromLineStart => {
            if character > line_text.len() || !line_text.is_char_boundary(character) {
                return Err(ScipImportError::InvalidRange);
            }
            character
        }
        PositionEncoding::UTF16CodeUnitOffsetFromLineStart => {
            code_unit_offset(line_text, character, char::len_utf16)?
        }
        PositionEncoding::UTF32CodeUnitOffsetFromLineStart => {
            code_unit_offset(line_text, character, |_| 1)?
        }
        PositionEncoding::UnspecifiedPositionEncoding => {
            return Err(ScipImportError::UnsupportedPositionEncoding);
        }
    };
    start
        .checked_add(relative)
        .ok_or(ScipImportError::InvalidRange)
}

fn code_unit_offset(
    text: &str,
    requested: usize,
    units: impl Fn(char) -> usize,
) -> Result<usize, ScipImportError> {
    let mut observed = 0_usize;
    for (offset, character) in text.char_indices() {
        if observed == requested {
            return Ok(offset);
        }
        observed = observed
            .checked_add(units(character))
            .ok_or(ScipImportError::InvalidRange)?;
        if observed > requested {
            return Err(ScipImportError::InvalidRange);
        }
    }
    if observed == requested {
        Ok(text.len())
    } else {
        Err(ScipImportError::InvalidRange)
    }
}

fn normalize_language(value: &str) -> Result<LanguageId, ScipImportError> {
    if value.is_empty() || !value.is_ascii() {
        return Err(ScipImportError::InvalidLanguage);
    }
    let normalized = value.to_ascii_lowercase();
    LanguageId::new(&normalized).map_err(|_| ScipImportError::InvalidLanguage)
}

fn project_entity_kind(information: &SymbolInformation) -> Option<EntityKind> {
    let kind = information.kind.enum_value().ok()?;
    match kind {
        ScipKind::Class | ScipKind::SingletonClass => Some(EntityKind::Class),
        ScipKind::Struct | ScipKind::Message => Some(EntityKind::Struct),
        ScipKind::Enum => Some(EntityKind::Enum),
        ScipKind::Union => Some(EntityKind::Union),
        ScipKind::TypeAlias | ScipKind::AssociatedType => Some(EntityKind::TypeAlias),
        ScipKind::Trait => Some(EntityKind::Trait),
        ScipKind::Interface | ScipKind::TypeClass | ScipKind::Concept => {
            Some(EntityKind::Interface)
        }
        ScipKind::Protocol => Some(EntityKind::Protocol),
        ScipKind::Function | ScipKind::Operator => Some(EntityKind::Function),
        ScipKind::Method
        | ScipKind::AbstractMethod
        | ScipKind::MethodAlias
        | ScipKind::MethodSpecification
        | ScipKind::ProtocolMethod
        | ScipKind::PureVirtualMethod
        | ScipKind::SingletonMethod
        | ScipKind::StaticMethod
        | ScipKind::TraitMethod
        | ScipKind::TypeClassMethod => Some(EntityKind::Method),
        ScipKind::Constructor => Some(EntityKind::Constructor),
        ScipKind::Field
        | ScipKind::StaticField
        | ScipKind::StaticDataMember
        | ScipKind::EnumMember => Some(EntityKind::Field),
        ScipKind::Property
        | ScipKind::Accessor
        | ScipKind::Getter
        | ScipKind::Setter
        | ScipKind::StaticProperty
        | ScipKind::Subscript => Some(EntityKind::Property),
        ScipKind::Constant => Some(EntityKind::Constant),
        ScipKind::Variable | ScipKind::StaticVariable | ScipKind::Value => {
            Some(EntityKind::Variable)
        }
        ScipKind::Parameter
        | ScipKind::ParameterLabel
        | ScipKind::SelfParameter
        | ScipKind::ThisParameter
        | ScipKind::MethodReceiver => Some(EntityKind::Parameter),
        ScipKind::TypeParameter => Some(EntityKind::TypeParameter),
        ScipKind::Module => Some(EntityKind::Module),
        ScipKind::Namespace => Some(EntityKind::Namespace),
        ScipKind::Package | ScipKind::PackageObject | ScipKind::Library => {
            Some(EntityKind::Package)
        }
        ScipKind::File => Some(EntityKind::File),
        _ => None,
    }
}

fn symbol_flags(document: &Document, symbol: &str, generated_source: bool) -> Vec<EntityFlag> {
    let mut generated = generated_source;
    let mut test = false;
    for occurrence in document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.symbol == symbol)
    {
        generated |= occurrence.symbol_roles & GENERATED_ROLE != 0;
        test |= occurrence.symbol_roles & TEST_ROLE != 0;
    }
    let mut flags = Vec::new();
    if generated {
        flags.push(EntityFlag::Generated);
    }
    if test {
        flags.push(EntityFlag::Test);
    }
    flags
}

fn occurrence_role(occurrence: &Occurrence) -> OccurrenceRole {
    let roles = occurrence.symbol_roles;
    if roles & FORWARD_DEFINITION_ROLE != 0 {
        OccurrenceRole::Declaration
    } else if roles & DEFINITION_ROLE != 0 {
        OccurrenceRole::Definition
    } else if roles & IMPORT_ROLE != 0 {
        OccurrenceRole::ImportUse
    } else if roles & WRITE_ROLE != 0 {
        OccurrenceRole::Write
    } else if roles & READ_ROLE != 0 {
        OccurrenceRole::Read
    } else if roles & TEST_ROLE != 0 {
        OccurrenceRole::TestUse
    } else if matches!(occurrence.syntax_kind.value(), 17 | 18) {
        OccurrenceRole::MacroUse
    } else if matches!(occurrence.syntax_kind.value(), 19 | 20) {
        OccurrenceRole::TypeUse
    } else {
        OccurrenceRole::Reference
    }
}

fn relationship_predicates(relationship: &Relationship) -> Vec<RelationPredicate> {
    let mut predicates = Vec::new();
    if relationship.is_reference {
        predicates.push(RelationPredicate::RefersTo);
    }
    if relationship.is_implementation {
        predicates.push(RelationPredicate::Implements);
    }
    if relationship.is_type_definition {
        predicates.push(RelationPredicate::UsesType);
    }
    if relationship.is_definition {
        predicates.push(RelationPredicate::BindsTo);
    }
    predicates
}

fn relationship_unit_count(symbols: &[SymbolInformation]) -> Result<usize, ScipImportError> {
    symbols.iter().try_fold(0_usize, |total, symbol| {
        checked_add(
            total,
            relationship_count(&symbol.relationships)?,
            ScipResource::Relationships,
        )
    })
}

fn relationship_count(relationships: &[Relationship]) -> Result<usize, ScipImportError> {
    relationships.iter().try_fold(0_usize, |total, relation| {
        checked_add(
            total,
            relationship_predicates(relation).len().max(1),
            ScipResource::Relationships,
        )
    })
}

fn direct_evidence(source: SourceRef) -> FactEvidence {
    FactEvidence {
        source: Some(source),
        derivation: Vec::new(),
    }
}

fn source_for_span(full: &SourceRef, span: SourceSpan) -> SourceRef {
    SourceRef::new(
        full.repository(),
        full.generation(),
        span,
        full.content_hash(),
        None,
    )
}

fn exact_confidence() -> Result<Confidence, ScipImportError> {
    Confidence::new(EXACT_CONFIDENCE).map_err(|_| ScipImportError::Identity)
}

fn require_nonempty_bounded_string(value: &str, limits: &IrLimits) -> Result<(), ScipImportError> {
    if value.is_empty() || value.len() > limits.max_string_bytes {
        return Err(ScipImportError::InvalidString);
    }
    Ok(())
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

fn check(cancellation: &Cancellation) -> Result<(), ScipImportError> {
    cancellation.check().map_err(|_| ScipImportError::Cancelled)
}

fn check_periodically(index: usize, cancellation: &Cancellation) -> Result<(), ScipImportError> {
    if index.is_multiple_of(CHECK_INTERVAL) {
        check(cancellation)?;
    }
    Ok(())
}

fn checked_add(
    left: usize,
    right: usize,
    resource: ScipResource,
) -> Result<usize, ScipImportError> {
    left.checked_add(right)
        .ok_or(ScipImportError::LimitExceeded {
            resource,
            observed: usize::MAX,
            limit: usize::MAX - 1,
        })
}

fn require_limit(
    resource: ScipResource,
    observed: usize,
    limit: usize,
) -> Result<(), ScipImportError> {
    if observed > limit {
        Err(ScipImportError::LimitExceeded {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

/// Bounded SCIP resource named by a limit failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScipResource {
    /// Encoded protobuf bytes.
    IndexBytes,
    /// Source document count.
    Documents,
    /// Internal and external symbols.
    Symbols,
    /// Source occurrences.
    Occurrences,
    /// Symbol relationships.
    Relationships,
    /// Exact source bytes supplied by the host.
    SourceBytes,
    /// Diagnostics intentionally omitted from normalized semantic facts.
    Diagnostics,
}

impl std::fmt::Display for ScipResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::IndexBytes => "index_bytes",
            Self::Documents => "documents",
            Self::Symbols => "symbols",
            Self::Occurrences => "occurrences",
            Self::Relationships => "relationships",
            Self::SourceBytes => "source_bytes",
            Self::Diagnostics => "diagnostics",
        })
    }
}

/// Invalid, inconsistent, or unsupported SCIP import.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScipImportError {
    /// The encoded index was empty.
    #[error("SCIP index must not be empty")]
    EmptyIndex,
    /// Official protobuf decoding failed.
    #[error("SCIP protobuf is malformed: {0}")]
    Decode(#[from] protobuf::Error),
    /// The index omitted required metadata.
    #[error("SCIP index metadata is required")]
    MissingMetadata,
    /// The index did not contain any document.
    #[error("SCIP index must contain at least one document")]
    MissingDocuments,
    /// The index and host source sets differ.
    #[error("SCIP document and source sets must match exactly")]
    SourceSetMismatch,
    /// A SCIP document had no exact host source.
    #[error("SCIP document has no exact host source")]
    MissingSource,
    /// Two SCIP documents used the same path.
    #[error("SCIP index contains a duplicate document path")]
    DuplicateDocument,
    /// Two host sources used the same path.
    #[error("SCIP import contains a duplicate source path")]
    DuplicateSource,
    /// A path was not canonical and repository-relative.
    #[error("SCIP import contains an invalid repository-relative path")]
    InvalidPath,
    /// Embedded SCIP text did not match the exact host source.
    #[error("SCIP embedded text does not match the exact host source")]
    EmbeddedTextMismatch,
    /// Host source was not valid UTF-8.
    #[error("SCIP import source must be valid UTF-8")]
    NonUtf8Source,
    /// Metadata declared an unsupported on-disk text encoding.
    #[error("SCIP index uses an unsupported text-document encoding")]
    UnsupportedTextEncoding,
    /// A document omitted or used an unsupported position encoding.
    #[error("SCIP document uses an unsupported position encoding")]
    UnsupportedPositionEncoding,
    /// A source range was negative, out of bounds, or split a code unit.
    #[error("SCIP occurrence contains an invalid source range")]
    InvalidRange,
    /// A document language was empty or not a bounded canonical label.
    #[error("SCIP document language is invalid")]
    InvalidLanguage,
    /// A required symbol string was empty or exceeded normalized-IR limits.
    #[error("SCIP symbol contains an invalid string")]
    InvalidString,
    /// One SCIP symbol was declared in more than one document.
    #[error("SCIP index contains a duplicate symbol declaration")]
    DuplicateSymbol,
    /// A deterministic identity recipe failed.
    #[error("SCIP fact identity could not be derived")]
    Identity,
    /// Checked resource accounting failed.
    #[error("SCIP resource accounting overflowed")]
    Accounting,
    /// A hard resource ceiling was exceeded.
    #[error("SCIP {resource} count {observed} exceeds limit {limit}")]
    LimitExceeded {
        /// Resource that exceeded its ceiling.
        resource: ScipResource,
        /// Observed count or byte length.
        observed: usize,
        /// Configured hard ceiling.
        limit: usize,
    },
    /// The operation was cancelled or its deadline expired.
    #[error("SCIP import was cancelled")]
    Cancelled,
    /// Materialized facts violated normalized-IR invariants.
    #[error("SCIP import produced invalid normalized IR: {0}")]
    InvalidDocument(IrDocumentValidationError),
}
