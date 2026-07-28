//! Whole-project structural semantics over parser-independent syntax facts.
//!
//! The adapter consumes audited `ParseProvider` output, resolves names only
//! within the immutable request, and never executes repository-owned code.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DiagnosticCode, DomainCoverage, IrBatch,
    IrBatchSink, IrRecord, LanguageId, MemoryAdmissionPolicy, MemoryEnforcement, ParseProvider,
    ParseRequest, ProducerDescriptor, ProjectAnalysisReport, ProjectAnalysisRequest,
    ProjectLanguageAnalyzer, ProjectSourceInput, RemainingBudget, ResourceUsage, SinkError,
    StreamEnd, StreamUsage, SyntaxFact, SyntaxFactKind, WorkReport, execute_parse,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, ContainerRef, CoverageRecord, CoverageScope,
    CoverageStatus, DiagnosticRecord, EntityFlag, EntityKind, EntityRecord, EntityVisibility,
    EvidenceKind, FactDomain, FactEvidence, FactRef, FileIdentityClaim, FileRecord,
    LexicalEvidenceFormat, LexicalEvidenceKind, LexicalEvidenceV1, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SkippedRegion, SkippedRegionReason,
    SourceMappingKind, SourceMappingRecord, SourceRef, SourceSpan, SymbolIdentityClaim,
    derive_coverage_record_id, derive_diagnostic_record_id, derive_occurrence_record_id,
    derive_provenance_record_id, derive_relation_record_id, derive_skipped_region_id,
    derive_source_mapping_record_id, new_file_identity_claim_envelope,
    new_lexical_evidence_envelope, new_symbol_identity_claim_envelope,
};

const STRUCTURAL_TIER: AnalysisTier = AnalysisTier::TierB;
const FRONTEND_VERSION: &str = "rootlight-project-structural-v1";
const CANCELLATION_CHECK_INTERVAL: usize = 64;
const EXACT_CONFIDENCE: u16 = 1_000;
const IMPORT_CONFIDENCE: u16 = 950;
const STATIC_CALL_CONFIDENCE: u16 = 850;
const TYPESCRIPT_CALL_CONFIDENCE: u16 = 800;
const DYNAMIC_CALL_CONFIDENCE: u16 = 650;
const FALLBACK_REFERENCE_CONFIDENCE: u16 = 550;

const ALL_DOMAINS: [FactDomain; 8] = [
    FactDomain::Files,
    FactDomain::Entities,
    FactDomain::Occurrences,
    FactDomain::Relations,
    FactDomain::Provenance,
    FactDomain::SourceMappings,
    FactDomain::Diagnostics,
    FactDomain::Extensions,
];

/// Closed set of languages with reviewed whole-project structural semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SemanticProjectLanguage {
    /// Rust modules, `use` imports, declarations, traits, and calls.
    Rust,
    /// TypeScript modules, declarations, inheritance, and calls.
    TypeScript,
    /// JavaScript modules, declarations, inheritance, and calls.
    JavaScript,
    /// Python modules, imports, declarations, inheritance, and calls.
    Python,
    /// Go packages, imports, declarations, embedding, and calls.
    Go,
}

impl SemanticProjectLanguage {
    /// Returns the canonical SDK language identity.
    ///
    /// # Errors
    ///
    /// Returns a provider failure only if a built-in language label no longer
    /// satisfies the SDK label grammar.
    pub fn language_id(self) -> Result<LanguageId, AdapterError> {
        LanguageId::new(self.as_str()).map_err(|_| provider_failure("built-in-language"))
    }

    /// Returns the canonical language label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }

    const fn inferred_call_confidence(self) -> u16 {
        match self {
            Self::Rust | Self::Go => STATIC_CALL_CONFIDENCE,
            Self::TypeScript => TYPESCRIPT_CALL_CONFIDENCE,
            Self::JavaScript | Self::Python => DYNAMIC_CALL_CONFIDENCE,
        }
    }

    fn entity_kind(self, declaration: &str, is_nested: bool) -> EntityKind {
        let header = declaration.trim_start();
        match self {
            Self::Rust => {
                let keywords = tokenize_identifiers(header);
                if keywords.iter().any(|keyword| keyword == "struct")
                    || keywords.iter().any(|keyword| keyword == "union")
                {
                    EntityKind::Struct
                } else if keywords.iter().any(|keyword| keyword == "enum") {
                    EntityKind::Enum
                } else if keywords.iter().any(|keyword| keyword == "trait") {
                    EntityKind::Trait
                } else if keywords.iter().any(|keyword| keyword == "type") {
                    EntityKind::TypeAlias
                } else if keywords.iter().any(|keyword| keyword == "const")
                    || keywords.iter().any(|keyword| keyword == "static")
                {
                    EntityKind::Constant
                } else if is_nested {
                    EntityKind::Method
                } else {
                    EntityKind::Function
                }
            }
            Self::TypeScript => {
                if header.contains("interface ") {
                    EntityKind::Interface
                } else if header.contains("class ") {
                    EntityKind::Class
                } else if header.contains("enum ") {
                    EntityKind::Enum
                } else if header.contains("type ") {
                    EntityKind::TypeAlias
                } else if header.contains("function ") {
                    EntityKind::Function
                } else if is_nested {
                    EntityKind::Method
                } else {
                    EntityKind::Variable
                }
            }
            Self::JavaScript => {
                if header.contains("class ") {
                    EntityKind::Class
                } else if header.contains("function ") {
                    EntityKind::Function
                } else if is_nested {
                    EntityKind::Method
                } else {
                    EntityKind::Variable
                }
            }
            Self::Python => {
                if starts_with_word(header, "class") {
                    EntityKind::Class
                } else if is_nested {
                    EntityKind::Method
                } else {
                    EntityKind::Function
                }
            }
            Self::Go => {
                if starts_with_word(header, "type") && header.contains(" interface") {
                    EntityKind::Interface
                } else if starts_with_word(header, "type") && header.contains(" struct") {
                    EntityKind::Struct
                } else if starts_with_word(header, "type") {
                    EntityKind::TypeAlias
                } else if starts_with_word(header, "const") {
                    EntityKind::Constant
                } else if starts_with_word(header, "var") {
                    EntityKind::Variable
                } else if header.starts_with("func (") || is_nested {
                    EntityKind::Method
                } else {
                    EntityKind::Function
                }
            }
        }
    }
}

/// Production whole-project analyzer over one audited parser capability.
///
/// One instance is permanently bound to a language and build-context identity.
/// The parser supplies resilient syntax facts; this layer supplies deterministic
/// lexical scopes, signatures, imports, project-local resolution candidates,
/// explicit inheritance or embedding relations, and calibrated call confidence.
#[derive(Clone)]
pub struct SemanticProjectAnalyzer {
    language: SemanticProjectLanguage,
    parser: Arc<dyn ParseProvider>,
    descriptor: ProducerDescriptor,
    binary_digest: ContentHash,
    build_context: BuildContextIdentity,
}

impl std::fmt::Debug for SemanticProjectAnalyzer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticProjectAnalyzer")
            .field("language", &self.language)
            .field("descriptor", &self.descriptor)
            .field("binary_digest", &self.binary_digest)
            .field("build_context", &self.build_context)
            .finish_non_exhaustive()
    }
}

impl SemanticProjectAnalyzer {
    /// Creates a Tier B analyzer from an audited parser and immutable identity.
    ///
    /// The parser must advertise the exact language, UTF-8, error recovery, and
    /// cooperative cancellation checkpoints. The analyzer never invokes build
    /// tools, generators, language servers, or repository-owned executables.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticProjectAnalyzerConfigError`] when the parser cannot
    /// uphold the structural or malformed-source contracts.
    pub fn new(
        language: SemanticProjectLanguage,
        parser: Arc<dyn ParseProvider>,
        producer: ProducerIdentity,
        binary_digest: ContentHash,
        build_context: BuildContextIdentity,
    ) -> Result<Self, SemanticProjectAnalyzerConfigError> {
        let language_id = LanguageId::new(language.as_str())
            .map_err(|_| SemanticProjectAnalyzerConfigError::InvalidLanguage)?;
        let capabilities = parser.capabilities();
        if !capabilities.languages().contains(&language_id) {
            return Err(SemanticProjectAnalyzerConfigError::UnsupportedLanguage);
        }
        if !capabilities
            .encodings()
            .iter()
            .any(|encoding| encoding.as_str() == "utf-8")
        {
            return Err(SemanticProjectAnalyzerConfigError::Utf8Required);
        }
        if !capabilities.supports_error_recovery() {
            return Err(SemanticProjectAnalyzerConfigError::ErrorRecoveryRequired);
        }
        if !capabilities.has_cancellation_checkpoints() {
            return Err(SemanticProjectAnalyzerConfigError::CancellationRequired);
        }
        let descriptor = ProducerDescriptor::new(
            producer,
            ProducerKind::Derivation,
            language_id,
            STRUCTURAL_TIER,
            MemoryEnforcement::Unavailable,
            true,
        );
        Ok(Self {
            language,
            parser,
            descriptor,
            binary_digest,
            build_context,
        })
    }

    /// Returns the exact language capability selected for this instance.
    #[must_use]
    pub const fn language(&self) -> SemanticProjectLanguage {
        self.language
    }

    /// Returns the immutable build-context identity accepted by this instance.
    #[must_use]
    pub const fn build_context(&self) -> BuildContextIdentity {
        self.build_context
    }

    fn analyze(
        &self,
        request: &ProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<ProjectFacts, AdapterError> {
        if request.build_context() != self.build_context {
            return Err(provider_failure("project-build-context"));
        }
        let mut parsed = Vec::new();
        for (index, input) in request.inputs().iter().enumerate() {
            check_periodically(index, cancellation)?;
            if input.language().as_str() != self.language.as_str()
                || input.encoding().as_str() != "utf-8"
            {
                return Err(provider_failure("project-language-input"));
            }
            let parse_request = ParseRequest::new(
                input.source().clone(),
                input.language().clone(),
                input.encoding().clone(),
                Vec::new(),
                request.limits(),
            )?;
            let output = execute_parse(
                self.parser.as_ref(),
                &parse_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                cancellation,
            )?;
            parsed.push(ParsedInput {
                input,
                facts: output.facts().to_vec(),
                diagnostics: output.diagnostics().to_vec(),
                parse_status: output.report().coverage().status(),
                syntax_nodes: output.report().resources().syntax_nodes(),
                max_syntax_depth: output.report().resources().max_syntax_depth(),
            });
        }
        ProjectFactsBuilder::new(self, request, parsed, cancellation).build()
    }
}

impl ProjectLanguageAnalyzer for SemanticProjectAnalyzer {
    fn descriptor(&self) -> &ProducerDescriptor {
        &self.descriptor
    }

    fn analyze_project(
        &self,
        request: &ProjectAnalysisRequest<'_>,
        sink: &mut dyn IrBatchSink,
        cancellation: &Cancellation,
    ) -> Result<ProjectAnalysisReport, AdapterError> {
        cancellation.check()?;
        let facts = self.analyze(request, cancellation)?;
        emit_records(facts.records, request.limits().ir(), sink, cancellation)?;
        cancellation.check()?;
        let usage = sink.staged_usage();
        let coverage = CoverageReport::new(
            STRUCTURAL_TIER,
            facts.status,
            request.total_source_bytes(),
            facts.covered_source_bytes,
            facts.skipped_regions,
            facts.domain_coverage,
        )?;
        let work = WorkReport::new(
            coverage,
            ResourceUsage::new(
                request.total_source_bytes(),
                usage.records(),
                facts.syntax_nodes,
                facts.max_syntax_depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?;
        Ok(ProjectAnalysisReport::new(
            work,
            request.analysis_unit().clone(),
            request.build_target().clone(),
            request.build_context(),
            request.requested_tier(),
        ))
    }
}

/// Invalid immutable configuration for [`SemanticProjectAnalyzer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SemanticProjectAnalyzerConfigError {
    /// A built-in language identity violated the SDK label contract.
    #[error("semantic project language identity is invalid")]
    InvalidLanguage,
    /// The parser does not advertise the selected language.
    #[error("parser does not support the selected semantic project language")]
    UnsupportedLanguage,
    /// Whole-project structural analysis requires UTF-8 parser input.
    #[error("parser must support UTF-8 project input")]
    Utf8Required,
    /// Partial malformed-file output requires parser recovery support.
    #[error("parser must support malformed-source recovery")]
    ErrorRecoveryRequired,
    /// Bounded project analysis requires cooperative parser checkpoints.
    #[error("parser must support cancellation checkpoints")]
    CancellationRequired,
}

struct ParsedInput<'request, 'source> {
    input: &'request ProjectSourceInput<'source>,
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    parse_status: CoverageStatus,
    syntax_nodes: usize,
    max_syntax_depth: usize,
}

#[derive(Debug, Clone)]
struct SemanticEntity {
    symbol: SymbolId,
    name: String,
    kind: EntityKind,
    file: FileId,
    span: SourceSpan,
    source: SourceRef,
}

#[derive(Debug, Clone)]
struct DeclarationDraft {
    file: FileId,
    span: SourceSpan,
    name: String,
    header: String,
    kind: EntityKind,
    container: Option<SymbolId>,
    source: SourceRef,
}

#[derive(Debug, Clone)]
struct ImportDraft {
    file: FileId,
    span: SourceSpan,
    source: SourceRef,
    module: String,
    bindings: Vec<ImportBinding>,
}

#[derive(Debug, Clone)]
struct ImportBinding {
    local: String,
    imported: String,
}

#[derive(Debug, Clone)]
struct OccurrenceDraft {
    file: FileId,
    name: String,
    syntax_kind: String,
    role: OccurrenceRole,
    enclosing: Option<SymbolId>,
    source: SourceRef,
}

#[derive(Debug)]
struct FileBuildState {
    file: FileId,
    provenance: FactId,
    path: String,
    status: CoverageStatus,
    skipped_regions: usize,
    skipped_bytes: usize,
    domain_counts: BTreeMap<FactDomain, usize>,
}

impl FileBuildState {
    fn increment(&mut self, domain: FactDomain) -> Result<(), AdapterError> {
        let count = self.domain_counts.entry(domain).or_default();
        *count = count
            .checked_add(1)
            .ok_or_else(|| provider_failure("project-accounting"))?;
        Ok(())
    }
}

struct ProjectFacts {
    records: Vec<IrRecord>,
    status: CoverageStatus,
    covered_source_bytes: usize,
    skipped_regions: usize,
    domain_coverage: Vec<DomainCoverage>,
    syntax_nodes: usize,
    max_syntax_depth: usize,
}

struct ProjectFactsBuilder<'analyzer, 'request, 'source> {
    analyzer: &'analyzer SemanticProjectAnalyzer,
    request: &'request ProjectAnalysisRequest<'source>,
    parsed: Vec<ParsedInput<'request, 'source>>,
    cancellation: &'analyzer Cancellation,
    records: Vec<IrRecord>,
    states: BTreeMap<FileId, FileBuildState>,
    entities: Vec<SemanticEntity>,
    declarations: Vec<DeclarationDraft>,
    imports: Vec<ImportDraft>,
    occurrences: Vec<OccurrenceDraft>,
    module_by_file: BTreeMap<FileId, SymbolId>,
    path_by_file: BTreeMap<FileId, String>,
}

impl<'analyzer, 'request, 'source> ProjectFactsBuilder<'analyzer, 'request, 'source> {
    fn new(
        analyzer: &'analyzer SemanticProjectAnalyzer,
        request: &'request ProjectAnalysisRequest<'source>,
        parsed: Vec<ParsedInput<'request, 'source>>,
        cancellation: &'analyzer Cancellation,
    ) -> Self {
        Self {
            analyzer,
            request,
            parsed,
            cancellation,
            records: Vec::new(),
            states: BTreeMap::new(),
            entities: Vec::new(),
            declarations: Vec::new(),
            imports: Vec::new(),
            occurrences: Vec::new(),
            module_by_file: BTreeMap::new(),
            path_by_file: BTreeMap::new(),
        }
    }

    fn build(mut self) -> Result<ProjectFacts, AdapterError> {
        self.materialize_files_and_provenance()?;
        self.collect_syntax()?;
        self.materialize_entities()?;
        self.materialize_imports_and_occurrences()?;
        self.materialize_inheritance()?;
        self.materialize_generated_mappings()?;
        self.materialize_parser_diagnostics()?;
        self.materialize_coverage()
    }

    fn materialize_files_and_provenance(&mut self) -> Result<(), AdapterError> {
        for index in 0..self.parsed.len() {
            check_periodically(index, self.cancellation)?;
            let input = self.parsed[index].input;
            let source = input.source().source_ref().clone();
            let provenance = self.provenance(&source)?;
            let provenance_id = provenance.id;
            let path = input.source().path().as_str().to_owned();
            let byte_length = u64::try_from(input.source().bytes().len())
                .map_err(|_| provider_failure("project-source-length"))?;
            let file_claim = FileIdentityClaim {
                file: source.span().file(),
                repository: source.repository(),
                path: path.clone(),
                path_identity: input.source().path().identity_bytes().to_vec(),
                content_hash: source.content_hash(),
                byte_length,
            };
            self.records.push(IrRecord::File(FileRecord {
                id: source.span().file(),
                repository: source.repository(),
                generation: source.generation(),
                path: path.clone(),
                path_locator: Some(input.source().path().to_locator()),
                content_hash: source.content_hash(),
                byte_length,
                language: self.analyzer.language.as_str().to_owned(),
                encoding: input.encoding().as_str().to_owned(),
                generated: input.is_generated(),
                provenance: provenance_id,
                evidence: direct_evidence(source.clone()),
            }));
            self.records.push(IrRecord::Provenance(provenance));
            self.records.push(IrRecord::Extension(
                new_file_identity_claim_envelope(
                    &file_claim,
                    source.generation(),
                    provenance_id,
                    source.clone(),
                )
                .map_err(|_| provider_failure("project-file-identity-claim"))?,
            ));

            let module_name = module_name(&path);
            let module_span = source.span();
            let module_claim = self.symbol_claim(
                EntityKind::Module,
                ContainerRef::File(source.span().file()),
                &module_name,
                &path,
            );
            let module_symbol = module_claim.symbol;
            self.records.push(IrRecord::Entity(EntityRecord {
                id: module_symbol,
                repository: source.repository(),
                generation: source.generation(),
                kind: EntityKind::Module,
                language: self.analyzer.language.as_str().to_owned(),
                tier: STRUCTURAL_TIER,
                canonical_name: module_name.clone(),
                display_name: module_name.clone(),
                qualified_name: path.clone(),
                container: Some(ContainerRef::File(source.span().file())),
                visibility: EntityVisibility::Unknown,
                flags: generated_flag(input.is_generated()),
                provenance: provenance_id,
                evidence: direct_evidence(source.clone()),
            }));
            self.records.push(IrRecord::Extension(
                new_symbol_identity_claim_envelope(
                    &module_claim,
                    source.generation(),
                    provenance_id,
                    source.clone(),
                )
                .map_err(|_| provider_failure("project-symbol-identity-claim"))?,
            ));
            self.entities.push(SemanticEntity {
                symbol: module_symbol,
                name: module_name,
                kind: EntityKind::Module,
                file: source.span().file(),
                span: module_span,
                source: source.clone(),
            });
            self.module_by_file
                .insert(source.span().file(), module_symbol);
            self.path_by_file.insert(source.span().file(), path.clone());
            let mut domain_counts = BTreeMap::new();
            domain_counts.insert(FactDomain::Files, 1);
            domain_counts.insert(FactDomain::Provenance, 1);
            domain_counts.insert(FactDomain::Entities, 1);
            domain_counts.insert(FactDomain::Extensions, 2);
            self.states.insert(
                source.span().file(),
                FileBuildState {
                    file: source.span().file(),
                    provenance: provenance_id,
                    path,
                    status: self.parsed[index].parse_status,
                    skipped_regions: 0,
                    skipped_bytes: 0,
                    domain_counts,
                },
            );
        }
        Ok(())
    }

    fn collect_syntax(&mut self) -> Result<(), AdapterError> {
        for file_index in 0..self.parsed.len() {
            check_periodically(file_index, self.cancellation)?;
            let input = self.parsed[file_index].input;
            let facts = self.parsed[file_index].facts.clone();
            let bytes = input.source().bytes();
            let facts_by_id = facts
                .iter()
                .map(|fact| (fact.local_id(), fact))
                .collect::<BTreeMap<_, _>>();
            let declaration_facts = facts
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::Declaration)
                .collect::<Vec<_>>();
            let scope_facts = facts
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::Scope)
                .collect::<Vec<_>>();
            let mut scope_symbols = BTreeMap::new();

            for (scope_index, scope) in scope_facts.iter().enumerate() {
                check_periodically(scope_index, self.cancellation)?;
                let source = source_for_span(input, scope.span());
                let name = format!(
                    "scope@{}:{}",
                    scope.span().start_byte(),
                    scope.span().end_byte()
                );
                let module = self.module_for(input)?;
                let claim = self.symbol_claim(
                    EntityKind::Namespace,
                    ContainerRef::Entity(module),
                    &name,
                    &name,
                );
                let symbol = claim.symbol;
                let provenance = self.provenance_for(input)?;
                scope_symbols.insert(scope.local_id(), symbol);
                self.records.push(IrRecord::Entity(EntityRecord {
                    id: symbol,
                    repository: source.repository(),
                    generation: source.generation(),
                    kind: EntityKind::Namespace,
                    language: self.analyzer.language.as_str().to_owned(),
                    tier: STRUCTURAL_TIER,
                    canonical_name: name.clone(),
                    display_name: "<lexical scope>".to_owned(),
                    qualified_name: format!("{}::{name}", input.source().path().as_str()),
                    container: Some(ContainerRef::Entity(module)),
                    visibility: EntityVisibility::Private,
                    flags: vec![EntityFlag::Synthetic],
                    provenance,
                    evidence: direct_evidence(source.clone()),
                }));
                self.records.push(IrRecord::Extension(
                    new_symbol_identity_claim_envelope(
                        &claim,
                        source.generation(),
                        provenance,
                        source.clone(),
                    )
                    .map_err(|_| provider_failure("project-symbol-identity-claim"))?,
                ));
                self.entities.push(SemanticEntity {
                    symbol,
                    name,
                    kind: EntityKind::Namespace,
                    file: source.span().file(),
                    span: source.span(),
                    source,
                });
                self.state_mut(input)?.increment(FactDomain::Entities)?;
                self.state_mut(input)?.increment(FactDomain::Extensions)?;
            }

            let mut selected_definition_spans = BTreeSet::new();
            for (declaration_index, declaration) in declaration_facts.iter().enumerate() {
                check_periodically(declaration_index, self.cancellation)?;
                let definition = facts
                    .iter()
                    .filter(|fact| {
                        fact.kind() == SyntaxFactKind::Occurrence
                            && !is_call_fact(fact)
                            && contains_span(declaration.span(), fact.span())
                    })
                    .min_by_key(|fact| (fact.span().start_byte(), span_len(fact.span())));
                let Some(definition) = definition else {
                    continue;
                };
                let Some(name) = source_text(bytes, definition.span()) else {
                    continue;
                };
                if !is_identifier(name) || name.len() > self.request.limits().ir().max_string_bytes
                {
                    continue;
                }
                let declaration_text = source_text(bytes, declaration.span())
                    .ok_or_else(|| provider_failure("project-declaration-span"))?;
                let header = declaration_header(self.analyzer.language, declaration_text);
                if header.is_empty() {
                    continue;
                }
                selected_definition_spans.insert(definition.span());
                let container = enclosing_scope(declaration, &facts_by_id, &scope_symbols)
                    .or_else(|| self.module_by_file.get(&definition.span().file()).copied());
                let is_nested = container.is_some_and(|candidate| {
                    self.entities.iter().any(|entity| {
                        entity.symbol == candidate && entity.kind != EntityKind::Module
                    })
                });
                self.declarations.push(DeclarationDraft {
                    file: definition.span().file(),
                    span: declaration.span(),
                    name: name.to_owned(),
                    header,
                    kind: self
                        .analyzer
                        .language
                        .entity_kind(declaration_text, is_nested),
                    container,
                    source: source_for_span(input, declaration.span()),
                });
            }

            for (fact_index, fact) in facts.iter().enumerate() {
                check_periodically(fact_index, self.cancellation)?;
                match fact.kind() {
                    SyntaxFactKind::Import => {
                        let Some(text) = source_text(bytes, fact.span()) else {
                            continue;
                        };
                        if let Some(import) = parse_import(self.analyzer.language, text) {
                            self.imports.push(ImportDraft {
                                file: fact.span().file(),
                                span: fact.span(),
                                source: source_for_span(input, fact.span()),
                                module: import.0,
                                bindings: import.1,
                            });
                        }
                    }
                    SyntaxFactKind::Occurrence => {
                        let Some(name) = source_text(bytes, fact.span()) else {
                            continue;
                        };
                        if !is_identifier(name) {
                            continue;
                        }
                        let in_import = self.imports.iter().any(|import| {
                            import.file == fact.span().file()
                                && contains_span(import.span, fact.span())
                        });
                        let role = if selected_definition_spans.contains(&fact.span()) {
                            OccurrenceRole::Definition
                        } else if in_import {
                            OccurrenceRole::ImportUse
                        } else if is_call_fact(fact) {
                            OccurrenceRole::CallSite
                        } else {
                            OccurrenceRole::Reference
                        };
                        let enclosing = enclosing_scope(fact, &facts_by_id, &scope_symbols)
                            .or_else(|| self.module_by_file.get(&fact.span().file()).copied());
                        self.occurrences.push(OccurrenceDraft {
                            file: fact.span().file(),
                            name: name.to_owned(),
                            syntax_kind: fact.syntax_kind().as_str().to_owned(),
                            role,
                            enclosing,
                            source: source_for_span(input, fact.span()),
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn materialize_entities(&mut self) -> Result<(), AdapterError> {
        let structural_containers = self
            .entities
            .iter()
            .filter(|entity| matches!(entity.kind, EntityKind::Module | EntityKind::Namespace))
            .cloned()
            .collect::<Vec<_>>();
        for entity in structural_containers {
            let subject = if entity.kind == EntityKind::Module {
                RelationEndpoint::File(entity.file)
            } else {
                RelationEndpoint::Entity(
                    self.module_by_file
                        .get(&entity.file)
                        .copied()
                        .ok_or_else(|| provider_failure("project-module"))?,
                )
            };
            self.push_relation(
                entity.file,
                subject,
                RelationPredicate::Contains,
                RelationEndpoint::Entity(entity.symbol),
                EXACT_CONFIDENCE,
                EvidenceKind::Syntax,
                entity.source,
            )?;
        }

        for index in 0..self.declarations.len() {
            check_periodically(index, self.cancellation)?;
            let draft = self.declarations[index].clone();
            let container = draft
                .container
                .map(ContainerRef::Entity)
                .unwrap_or(ContainerRef::File(draft.file));
            let claim = self.symbol_claim(draft.kind, container, &draft.name, &draft.header);
            let symbol = claim.symbol;
            let state = self
                .states
                .get(&draft.file)
                .ok_or_else(|| provider_failure("project-file-state"))?;
            let qualified_name = format!("{}::{}", state.path, draft.name);
            let provenance = state.provenance;
            self.records.push(IrRecord::Entity(EntityRecord {
                id: symbol,
                repository: draft.source.repository(),
                generation: draft.source.generation(),
                kind: draft.kind,
                language: self.analyzer.language.as_str().to_owned(),
                tier: STRUCTURAL_TIER,
                canonical_name: draft.name.clone(),
                display_name: draft.name.clone(),
                qualified_name,
                container: Some(container),
                visibility: infer_visibility(self.analyzer.language, &draft.header),
                flags: generated_flag(self.input_for_file(draft.file)?.is_generated()),
                provenance,
                evidence: direct_evidence(draft.source.clone()),
            }));
            self.records.push(IrRecord::Extension(
                new_symbol_identity_claim_envelope(
                    &claim,
                    draft.source.generation(),
                    provenance,
                    draft.source.clone(),
                )
                .map_err(|_| provider_failure("project-symbol-identity-claim"))?,
            ));
            let signature_source = source_for_span(
                self.input_for_file(draft.file)?,
                SourceSpan::new(
                    draft.file,
                    draft.span.start_byte(),
                    draft
                        .span
                        .start_byte()
                        .checked_add(
                            u64::try_from(draft.header.len())
                                .map_err(|_| provider_failure("project-signature-length"))?,
                        )
                        .ok_or_else(|| provider_failure("project-signature-length"))?,
                )
                .map_err(|_| provider_failure("project-signature-span"))?,
            );
            let lexical = LexicalEvidenceV1::from_complete_text(
                LexicalEvidenceKind::Signature,
                FactRef::Entity(symbol),
                LexicalEvidenceFormat::SourceText,
                &draft.header,
            )
            .map_err(|_| provider_failure("project-signature"))?;
            let envelope = new_lexical_evidence_envelope(
                draft.source.repository(),
                draft.source.generation(),
                provenance,
                signature_source,
                &lexical,
            )
            .map_err(|_| provider_failure("project-signature-envelope"))?;
            self.records.push(IrRecord::Extension(envelope));
            self.entities.push(SemanticEntity {
                symbol,
                name: draft.name,
                kind: draft.kind,
                file: draft.file,
                span: draft.span,
                source: draft.source,
            });
            self.state_mut_by_file(draft.file)?
                .increment(FactDomain::Entities)?;
            self.state_mut_by_file(draft.file)?
                .increment(FactDomain::Extensions)?;
            self.state_mut_by_file(draft.file)?
                .increment(FactDomain::Extensions)?;
            let subject = match container {
                ContainerRef::Repository(repository) => RelationEndpoint::Repository(repository),
                ContainerRef::File(file) => RelationEndpoint::File(file),
                ContainerRef::Entity(entity) => RelationEndpoint::Entity(entity),
            };
            self.push_relation(
                draft.file,
                subject,
                RelationPredicate::Declares,
                RelationEndpoint::Entity(symbol),
                EXACT_CONFIDENCE,
                EvidenceKind::Syntax,
                self.entities
                    .last()
                    .ok_or_else(|| provider_failure("project-entity"))?
                    .source
                    .clone(),
            )?;
        }
        Ok(())
    }

    fn materialize_imports_and_occurrences(&mut self) -> Result<(), AdapterError> {
        let definitions = self.definition_index();
        let import_targets = self.import_target_index();
        let mut definition_occurrences = BTreeMap::new();

        for index in 0..self.occurrences.len() {
            check_periodically(index, self.cancellation)?;
            let draft = self.occurrences[index].clone();
            let candidates = self.resolve_occurrence(&draft, &definitions, &import_targets, false);
            let confidence_value = match draft.role {
                OccurrenceRole::Definition => EXACT_CONFIDENCE,
                OccurrenceRole::ImportUse => IMPORT_CONFIDENCE,
                OccurrenceRole::CallSite => self.analyzer.language.inferred_call_confidence(),
                _ => {
                    if candidates.is_empty() {
                        FALLBACK_REFERENCE_CONFIDENCE
                    } else {
                        STATIC_CALL_CONFIDENCE
                    }
                }
            };
            let target = occurrence_target(&draft.name, &candidates)?;
            let provenance = self.provenance_for_file(draft.file)?;
            let mut record = OccurrenceRecord {
                id: FactId::from_bytes([0; 20]),
                repository: draft.source.repository(),
                generation: draft.source.generation(),
                file: draft.file,
                source: draft.source.clone(),
                role: draft.role,
                enclosing: draft.enclosing,
                target,
                syntactic_text_hash: content_hash(draft.name.as_bytes()),
                syntax_kind: draft.syntax_kind,
                provenance,
                confidence: confidence(confidence_value)?,
                evidence: direct_evidence(draft.source.clone()),
            };
            record.id = derive_occurrence_record_id(&record)
                .map_err(|_| provider_failure("project-occurrence-identity"))?;
            if draft.role == OccurrenceRole::Definition {
                for symbol in &candidates {
                    definition_occurrences.insert(*symbol, record.id);
                }
            }
            self.add_occurrence_relations(&record, &candidates)?;
            self.records.push(IrRecord::Occurrence(record));
            self.state_mut_by_file(draft.file)?
                .increment(FactDomain::Occurrences)?;
        }

        for (symbol, occurrence) in definition_occurrences {
            let Some(entity) = self
                .entities
                .iter()
                .find(|entity| entity.symbol == symbol)
                .cloned()
            else {
                continue;
            };
            self.push_relation(
                entity.file,
                RelationEndpoint::Entity(symbol),
                RelationPredicate::DefinesAt,
                RelationEndpoint::Occurrence(occurrence),
                EXACT_CONFIDENCE,
                EvidenceKind::Syntax,
                entity.source,
            )?;
        }

        for index in 0..self.imports.len() {
            check_periodically(index, self.cancellation)?;
            let import = self.imports[index].clone();
            let targets = import_targets
                .get(&(import.file, import.module.clone()))
                .cloned()
                .unwrap_or_default();
            for target_file in targets {
                let Some(target_module) = self.module_by_file.get(&target_file).copied() else {
                    continue;
                };
                self.push_relation(
                    import.file,
                    RelationEndpoint::File(import.file),
                    RelationPredicate::Imports,
                    RelationEndpoint::Entity(target_module),
                    IMPORT_CONFIDENCE,
                    EvidenceKind::Derived,
                    import.source.clone(),
                )?;
            }
        }
        Ok(())
    }

    fn materialize_inheritance(&mut self) -> Result<(), AdapterError> {
        let definitions = self.definition_index();
        for index in 0..self.declarations.len() {
            check_periodically(index, self.cancellation)?;
            let declaration = self.declarations[index].clone();
            let Some(subject) = definitions
                .get(&declaration.name)
                .and_then(|entities| {
                    entities
                        .iter()
                        .find(|entity| entity.file == declaration.file)
                })
                .cloned()
            else {
                continue;
            };
            for (predicate, name) in inheritance_names(self.analyzer.language, &declaration.header)
            {
                let target = definitions
                    .get(&name)
                    .and_then(|entities| entities.first())
                    .cloned();
                let target = match target {
                    Some(target) => target,
                    None => {
                        self.external_entity(declaration.file, &name, declaration.source.clone())?
                    }
                };
                self.push_relation(
                    declaration.file,
                    RelationEndpoint::Entity(subject.symbol),
                    predicate,
                    RelationEndpoint::Entity(target.symbol),
                    STATIC_CALL_CONFIDENCE,
                    EvidenceKind::Syntax,
                    declaration.source.clone(),
                )?;
            }
        }

        if self.analyzer.language == SemanticProjectLanguage::Rust {
            for parsed_index in 0..self.parsed.len() {
                let parsed = &self.parsed[parsed_index];
                let text = std::str::from_utf8(parsed.input.source().bytes())
                    .map_err(|_| provider_failure("project-utf8"))?;
                for (type_name, trait_name) in rust_impl_pairs(text) {
                    let Some(subject) = definitions
                        .get(&type_name)
                        .and_then(|entities| entities.first())
                        .cloned()
                    else {
                        continue;
                    };
                    let target = match definitions
                        .get(&trait_name)
                        .and_then(|entities| entities.first())
                        .cloned()
                    {
                        Some(target) => target,
                        None => {
                            self.external_entity(subject.file, &trait_name, subject.source.clone())?
                        }
                    };
                    self.push_relation(
                        subject.file,
                        RelationEndpoint::Entity(subject.symbol),
                        RelationPredicate::Implements,
                        RelationEndpoint::Entity(target.symbol),
                        STATIC_CALL_CONFIDENCE,
                        EvidenceKind::Syntax,
                        subject.source.clone(),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn materialize_generated_mappings(&mut self) -> Result<(), AdapterError> {
        for input_index in 0..self.request.inputs().len() {
            check_periodically(input_index, self.cancellation)?;
            let input = &self.request.inputs()[input_index];
            for mapping in input.origins() {
                let Some(origin_input) = self
                    .request
                    .inputs()
                    .iter()
                    .find(|candidate| candidate.source().path() == mapping.origin_path())
                else {
                    return Err(provider_failure("project-origin-input"));
                };
                let from = source_for_span(input, mapping.generated());
                let to = source_for_span(origin_input, mapping.origin());
                let provenance = self.provenance_for(input)?;
                let mut record = SourceMappingRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: from.repository(),
                    generation: from.generation(),
                    from: from.clone(),
                    to,
                    kind: SourceMappingKind::GeneratedToOrigin,
                    provenance,
                    evidence: FactEvidence {
                        source: Some(from),
                        derivation: vec![FactRef::File(
                            origin_input.source().source_ref().span().file(),
                        )],
                    },
                };
                record.id = derive_source_mapping_record_id(&record)
                    .map_err(|_| provider_failure("project-mapping-identity"))?;
                self.records.push(IrRecord::SourceMapping(record));
                self.state_mut(input)?
                    .increment(FactDomain::SourceMappings)?;
            }
        }
        Ok(())
    }

    fn materialize_parser_diagnostics(&mut self) -> Result<(), AdapterError> {
        for parsed_index in 0..self.parsed.len() {
            check_periodically(parsed_index, self.cancellation)?;
            let input = self.parsed[parsed_index].input;
            let diagnostics = self.parsed[parsed_index].diagnostics.clone();
            let recovery_spans = self.parsed[parsed_index]
                .facts
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::ErrorRecovery)
                .map(SyntaxFact::span)
                .collect::<BTreeSet<_>>();

            for (diagnostic_index, diagnostic) in diagnostics.iter().enumerate() {
                check_periodically(diagnostic_index, self.cancellation)?;
                self.push_diagnostic(input, diagnostic)?;
            }

            let spans = if recovery_spans.is_empty()
                && self.parsed[parsed_index].parse_status != CoverageStatus::Complete
            {
                BTreeSet::from([input.source().source_ref().span()])
            } else {
                recovery_spans
            };
            for span in spans {
                self.push_skipped_region(input, span)?;
            }
        }
        Ok(())
    }

    fn materialize_coverage(mut self) -> Result<ProjectFacts, AdapterError> {
        let mut overall_status = CoverageStatus::Complete;
        let mut covered_source_bytes = self.request.total_source_bytes();
        let mut skipped_regions = 0_usize;
        let mut totals = BTreeMap::<FactDomain, (CoverageStatus, usize, usize, usize)>::new();

        let files = self.states.keys().copied().collect::<Vec<_>>();
        for (file_index, file) in files.iter().enumerate() {
            check_periodically(file_index, self.cancellation)?;
            let state = self
                .states
                .get(file)
                .ok_or_else(|| provider_failure("project-file-state"))?;
            overall_status = merge_status(overall_status, state.status);
            covered_source_bytes = covered_source_bytes.saturating_sub(state.skipped_bytes);
            skipped_regions = skipped_regions
                .checked_add(state.skipped_regions)
                .ok_or_else(|| provider_failure("project-accounting"))?;
            for domain in ALL_DOMAINS {
                let indexed = state.domain_counts.get(&domain).copied().unwrap_or(0);
                let domain_status = if state.status == CoverageStatus::Complete
                    || matches!(domain, FactDomain::Files | FactDomain::Provenance)
                {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                };
                let skipped = usize::from(domain_status != CoverageStatus::Complete);
                let discovered = indexed
                    .checked_add(skipped)
                    .ok_or_else(|| provider_failure("project-accounting"))?;
                let mut record = CoverageRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: self.request.inputs()[0].source().source_ref().repository(),
                    generation: self.request.inputs()[0].source().source_ref().generation(),
                    scope: CoverageScope::File(state.file),
                    domain,
                    tier: STRUCTURAL_TIER,
                    status: domain_status,
                    discovered: u64::try_from(discovered)
                        .map_err(|_| provider_failure("project-accounting"))?,
                    indexed: u64::try_from(indexed)
                        .map_err(|_| provider_failure("project-accounting"))?,
                    skipped: u64::try_from(skipped)
                        .map_err(|_| provider_failure("project-accounting"))?,
                    provenance: state.provenance,
                    evidence: direct_evidence(
                        self.input_for_file(state.file)?
                            .source()
                            .source_ref()
                            .clone(),
                    ),
                };
                record.id = derive_coverage_record_id(&record)
                    .map_err(|_| provider_failure("project-coverage-identity"))?;
                self.records.push(IrRecord::Coverage(record));
                let aggregate = totals
                    .entry(domain)
                    .or_insert((CoverageStatus::Complete, 0, 0, 0));
                aggregate.0 = merge_status(aggregate.0, domain_status);
                aggregate.1 = aggregate
                    .1
                    .checked_add(discovered)
                    .ok_or_else(|| provider_failure("project-accounting"))?;
                aggregate.2 = aggregate
                    .2
                    .checked_add(indexed)
                    .ok_or_else(|| provider_failure("project-accounting"))?;
                aggregate.3 = aggregate
                    .3
                    .checked_add(skipped)
                    .ok_or_else(|| provider_failure("project-accounting"))?;
            }
        }
        let domain_coverage = totals
            .into_iter()
            .map(|(domain, (status, discovered, indexed, skipped))| {
                DomainCoverage::new(domain, status, discovered, indexed, skipped)
                    .map_err(AdapterError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let syntax_nodes = self.parsed.iter().try_fold(0_usize, |total, parsed| {
            total
                .checked_add(parsed.syntax_nodes)
                .ok_or_else(|| provider_failure("project-accounting"))
        })?;
        let max_syntax_depth = self
            .parsed
            .iter()
            .map(|parsed| parsed.max_syntax_depth)
            .max()
            .unwrap_or(0);
        Ok(ProjectFacts {
            records: self.records,
            status: overall_status,
            covered_source_bytes,
            skipped_regions,
            domain_coverage,
            syntax_nodes,
            max_syntax_depth,
        })
    }

    fn provenance(&self, source: &SourceRef) -> Result<ProvenanceRecord, AdapterError> {
        let mut record = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository: source.repository(),
            generation: source.generation(),
            producer_kind: self.analyzer.descriptor.kind(),
            producer: self.analyzer.descriptor.identity().clone(),
            binary_digest: self.analyzer.binary_digest,
            frontend_version: Some(FRONTEND_VERSION.to_owned()),
            language: self.analyzer.language.as_str().to_owned(),
            tier: STRUCTURAL_TIER,
            build_context: self.request.build_context(),
            input_sources: vec![source.clone()],
            evidence_sources: vec![source.clone()],
            derivation_parents: Vec::new(),
            rule: Some("rootlight.project-structural.v1".to_owned()),
        };
        record.id = derive_provenance_record_id(&record)
            .map_err(|_| provider_failure("project-provenance-identity"))?;
        Ok(record)
    }

    fn symbol_claim(
        &self,
        kind: EntityKind,
        container: ContainerRef,
        name: &str,
        signature: &str,
    ) -> SymbolIdentityClaim {
        let mut container_identity = Vec::new();
        match container {
            ContainerRef::Repository(repository) => {
                container_identity.push(0);
                container_identity.extend_from_slice(repository.as_bytes());
            }
            ContainerRef::File(file) => {
                container_identity.push(1);
                container_identity.extend_from_slice(file.as_bytes());
            }
            ContainerRef::Entity(entity) => {
                container_identity.push(2);
                container_identity.extend_from_slice(entity.as_bytes());
            }
        }
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: self.request.inputs()[0].source().source_ref().repository(),
            language: self.analyzer.language.as_str().to_owned(),
            kind,
            container: Some(container),
            container_identity,
            declared_identity: name.to_owned(),
            signature_discriminator: content_hash(signature.as_bytes()).as_bytes().to_vec(),
            build_context_discriminator: self.request.build_context().digest().as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        claim
    }

    fn module_for(&self, input: &ProjectSourceInput<'_>) -> Result<SymbolId, AdapterError> {
        self.module_by_file
            .get(&input.source().source_ref().span().file())
            .copied()
            .ok_or_else(|| provider_failure("project-module"))
    }

    fn state_mut(
        &mut self,
        input: &ProjectSourceInput<'_>,
    ) -> Result<&mut FileBuildState, AdapterError> {
        self.state_mut_by_file(input.source().source_ref().span().file())
    }

    fn state_mut_by_file(&mut self, file: FileId) -> Result<&mut FileBuildState, AdapterError> {
        self.states
            .get_mut(&file)
            .ok_or_else(|| provider_failure("project-file-state"))
    }

    fn provenance_for(&self, input: &ProjectSourceInput<'_>) -> Result<FactId, AdapterError> {
        self.provenance_for_file(input.source().source_ref().span().file())
    }

    fn provenance_for_file(&self, file: FileId) -> Result<FactId, AdapterError> {
        self.states
            .get(&file)
            .map(|state| state.provenance)
            .ok_or_else(|| provider_failure("project-provenance"))
    }

    fn input_for_file(&self, file: FileId) -> Result<&ProjectSourceInput<'source>, AdapterError> {
        self.request
            .inputs()
            .iter()
            .find(|input| input.source().source_ref().span().file() == file)
            .ok_or_else(|| provider_failure("project-file-input"))
    }

    fn definition_index(&self) -> BTreeMap<String, Vec<SemanticEntity>> {
        let mut index = BTreeMap::<String, Vec<SemanticEntity>>::new();
        for entity in &self.entities {
            if !matches!(entity.kind, EntityKind::Module | EntityKind::Namespace) {
                index
                    .entry(entity.name.clone())
                    .or_default()
                    .push(entity.clone());
            }
        }
        for candidates in index.values_mut() {
            candidates.sort_by_key(|entity| (entity.file, entity.span.start_byte(), entity.symbol));
        }
        index
    }

    fn import_target_index(&self) -> BTreeMap<(FileId, String), Vec<FileId>> {
        let mut index = BTreeMap::new();
        for import in &self.imports {
            let current_path = self
                .path_by_file
                .get(&import.file)
                .map(String::as_str)
                .unwrap_or_default();
            let mut targets = self
                .path_by_file
                .iter()
                .filter_map(|(file, path)| {
                    module_matches(self.analyzer.language, current_path, &import.module, path)
                        .then_some(*file)
                })
                .collect::<Vec<_>>();
            targets.sort_unstable();
            targets.dedup();
            index.insert((import.file, import.module.clone()), targets);
        }
        index
    }

    fn resolve_occurrence(
        &self,
        occurrence: &OccurrenceDraft,
        definitions: &BTreeMap<String, Vec<SemanticEntity>>,
        import_targets: &BTreeMap<(FileId, String), Vec<FileId>>,
        permit_global_fallback: bool,
    ) -> Vec<SymbolId> {
        if occurrence.role == OccurrenceRole::Definition {
            return definitions
                .get(&occurrence.name)
                .into_iter()
                .flatten()
                .filter(|entity| {
                    entity.file == occurrence.file
                        && contains_span(entity.span, occurrence.source.span())
                })
                .map(|entity| entity.symbol)
                .collect();
        }
        let mut symbols = definitions
            .get(&occurrence.name)
            .into_iter()
            .flatten()
            .filter(|entity| entity.file == occurrence.file)
            .map(|entity| entity.symbol)
            .collect::<BTreeSet<_>>();
        for import in self
            .imports
            .iter()
            .filter(|import| import.file == occurrence.file)
        {
            let imported_names = import
                .bindings
                .iter()
                .filter(|binding| binding.local == occurrence.name)
                .map(|binding| binding.imported.as_str())
                .collect::<BTreeSet<_>>();
            let lookup_names = if imported_names.is_empty() {
                BTreeSet::from([occurrence.name.as_str()])
            } else {
                imported_names
            };
            let target_files = import_targets
                .get(&(import.file, import.module.clone()))
                .cloned()
                .unwrap_or_default();
            for lookup_name in lookup_names {
                if let Some(candidates) = definitions.get(lookup_name) {
                    symbols.extend(
                        candidates
                            .iter()
                            .filter(|entity| target_files.contains(&entity.file))
                            .map(|entity| entity.symbol),
                    );
                }
            }
        }
        if symbols.is_empty()
            && permit_global_fallback
            && let Some(candidates) = definitions.get(&occurrence.name)
        {
            symbols.extend(candidates.iter().map(|entity| entity.symbol));
        }
        symbols.into_iter().collect()
    }

    fn add_occurrence_relations(
        &mut self,
        occurrence: &OccurrenceRecord,
        candidates: &[SymbolId],
    ) -> Result<(), AdapterError> {
        if occurrence.role == OccurrenceRole::Definition {
            return Ok(());
        }
        let predicate = match occurrence.role {
            OccurrenceRole::CallSite if candidates.len() == 1 => RelationPredicate::Calls,
            OccurrenceRole::CallSite => RelationPredicate::DispatchCandidate,
            _ => RelationPredicate::RefersTo,
        };
        for candidate in candidates {
            self.push_relation(
                occurrence.file,
                RelationEndpoint::Occurrence(occurrence.id),
                predicate,
                RelationEndpoint::Entity(*candidate),
                occurrence.confidence.get(),
                EvidenceKind::Derived,
                occurrence.source.clone(),
            )?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "relation construction mirrors the normalized IR evidence contract"
    )]
    fn push_relation(
        &mut self,
        file: FileId,
        subject: RelationEndpoint,
        predicate: RelationPredicate,
        object: RelationEndpoint,
        confidence_value: u16,
        evidence_kind: EvidenceKind,
        source: SourceRef,
    ) -> Result<(), AdapterError> {
        let mut record = RelationRecord {
            id: FactId::from_bytes([0; 20]),
            repository: source.repository(),
            generation: source.generation(),
            subject,
            predicate,
            object,
            confidence: confidence(confidence_value)?,
            evidence_kind,
            provenance: self.provenance_for_file(file)?,
            evidence: direct_evidence(source),
        };
        record.id = derive_relation_record_id(&record)
            .map_err(|_| provider_failure("project-relation-identity"))?;
        self.records.push(IrRecord::Relation(record));
        self.state_mut_by_file(file)?
            .increment(FactDomain::Relations)
    }

    fn external_entity(
        &mut self,
        file: FileId,
        name: &str,
        source: SourceRef,
    ) -> Result<SemanticEntity, AdapterError> {
        if let Some(existing) = self
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::ExternalSymbol && entity.name == name)
        {
            return Ok(existing.clone());
        }
        let container = ContainerRef::Repository(source.repository());
        let claim = self.symbol_claim(EntityKind::ExternalSymbol, container, name, name);
        let symbol = claim.symbol;
        let provenance = self.provenance_for_file(file)?;
        let entity = SemanticEntity {
            symbol,
            name: name.to_owned(),
            kind: EntityKind::ExternalSymbol,
            file,
            span: source.span(),
            source: source.clone(),
        };
        self.records.push(IrRecord::Entity(EntityRecord {
            id: symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind: EntityKind::ExternalSymbol,
            language: self.analyzer.language.as_str().to_owned(),
            tier: STRUCTURAL_TIER,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: name.to_owned(),
            container: Some(container),
            visibility: EntityVisibility::Unknown,
            flags: vec![EntityFlag::External, EntityFlag::Synthetic],
            provenance,
            evidence: direct_evidence(source.clone()),
        }));
        self.records.push(IrRecord::Extension(
            new_symbol_identity_claim_envelope(&claim, source.generation(), provenance, source)
                .map_err(|_| provider_failure("project-symbol-identity-claim"))?,
        ));
        self.entities.push(entity.clone());
        self.state_mut_by_file(file)?
            .increment(FactDomain::Entities)?;
        self.state_mut_by_file(file)?
            .increment(FactDomain::Extensions)?;
        Ok(entity)
    }

    fn push_diagnostic(
        &mut self,
        input: &ProjectSourceInput<'_>,
        diagnostic: &AdapterDiagnostic,
    ) -> Result<(), AdapterError> {
        let source = diagnostic
            .source()
            .cloned()
            .or_else(|| Some(input.source().source_ref().clone()));
        let mut record = DiagnosticRecord {
            id: FactId::from_bytes([0; 20]),
            repository: input.source().source_ref().repository(),
            generation: input.source().source_ref().generation(),
            code: diagnostic.code().as_str().to_owned(),
            message: "parser recovered from malformed or incomplete syntax".to_owned(),
            severity: diagnostic.severity(),
            source: source.clone(),
            coverage_effect: diagnostic.coverage_effect(),
            provenance: self.provenance_for(input)?,
            evidence: FactEvidence {
                source,
                derivation: Vec::new(),
            },
        };
        record.id = derive_diagnostic_record_id(&record)
            .map_err(|_| provider_failure("project-diagnostic-identity"))?;
        self.records.push(IrRecord::Diagnostic(record));
        let state = self.state_mut(input)?;
        state.status = merge_status(state.status, diagnostic.coverage_effect());
        state.increment(FactDomain::Diagnostics)
    }

    fn push_skipped_region(
        &mut self,
        input: &ProjectSourceInput<'_>,
        span: SourceSpan,
    ) -> Result<(), AdapterError> {
        let source = source_for_span(input, span);
        let mut record = SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository: source.repository(),
            generation: source.generation(),
            source: source.clone(),
            domain: FactDomain::Entities,
            reason: SkippedRegionReason::ParseError,
            detail: "parser-recovery-region".to_owned(),
            provenance: self.provenance_for(input)?,
            evidence: direct_evidence(source),
        };
        record.id = derive_skipped_region_id(&record)
            .map_err(|_| provider_failure("project-skip-identity"))?;
        self.records.push(IrRecord::SkippedRegion(record));
        let state = self.state_mut(input)?;
        state.status = merge_status(state.status, CoverageStatus::Bounded);
        state.skipped_regions = state
            .skipped_regions
            .checked_add(1)
            .ok_or_else(|| provider_failure("project-accounting"))?;
        state.skipped_bytes = state
            .skipped_bytes
            .checked_add(span_len(span))
            .ok_or_else(|| provider_failure("project-accounting"))?;
        state.increment(FactDomain::Diagnostics)
    }
}

fn source_for_span(input: &ProjectSourceInput<'_>, span: SourceSpan) -> SourceRef {
    let full = input.source().source_ref();
    SourceRef::new(
        full.repository(),
        full.generation(),
        span,
        full.content_hash(),
        None,
    )
}

fn direct_evidence(source: SourceRef) -> FactEvidence {
    FactEvidence {
        source: Some(source),
        derivation: Vec::new(),
    }
}

fn generated_flag(generated: bool) -> Vec<EntityFlag> {
    if generated {
        vec![EntityFlag::Generated]
    } else {
        Vec::new()
    }
}

fn infer_visibility(language: SemanticProjectLanguage, header: &str) -> EntityVisibility {
    match language {
        SemanticProjectLanguage::Rust => {
            if header.trim_start().starts_with("pub") {
                EntityVisibility::Public
            } else {
                EntityVisibility::Private
            }
        }
        SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript => {
            if header.contains("export ") {
                EntityVisibility::Public
            } else {
                EntityVisibility::Unknown
            }
        }
        SemanticProjectLanguage::Python => {
            if header
                .trim_start_matches("async ")
                .split_whitespace()
                .nth(1)
                .is_some_and(|name| name.starts_with('_'))
            {
                EntityVisibility::Private
            } else {
                EntityVisibility::Unknown
            }
        }
        SemanticProjectLanguage::Go => {
            let name = tokenize_identifiers(header)
                .into_iter()
                .nth(1)
                .unwrap_or_default();
            if name.starts_with(|character: char| character.is_ascii_uppercase()) {
                EntityVisibility::Public
            } else {
                EntityVisibility::Private
            }
        }
    }
}

fn occurrence_target(
    name: &str,
    candidates: &[SymbolId],
) -> Result<OccurrenceTarget, AdapterError> {
    match candidates {
        [] => Ok(OccurrenceTarget::Unresolved {
            text_hash: content_hash(name.as_bytes()),
        }),
        [symbol] => Ok(OccurrenceTarget::Resolved { symbol: *symbol }),
        symbols => Ok(OccurrenceTarget::Candidates {
            symbols: symbols.to_vec(),
            total_count: u64::try_from(symbols.len())
                .map_err(|_| provider_failure("project-candidate-count"))?,
            completeness: CoverageStatus::Complete,
        }),
    }
}

fn confidence(value: u16) -> Result<Confidence, AdapterError> {
    Confidence::new(value).map_err(|_| provider_failure("project-confidence"))
}

fn contains_span(container: SourceSpan, child: SourceSpan) -> bool {
    container.file() == child.file()
        && container.start_byte() <= child.start_byte()
        && container.end_byte() >= child.end_byte()
}

fn span_len(span: SourceSpan) -> usize {
    usize::try_from(span.end_byte().saturating_sub(span.start_byte())).unwrap_or(usize::MAX)
}

fn source_text(bytes: &[u8], span: SourceSpan) -> Option<&str> {
    let start = usize::try_from(span.start_byte()).ok()?;
    let end = usize::try_from(span.end_byte()).ok()?;
    std::str::from_utf8(bytes.get(start..end)?).ok()
}

fn enclosing_scope(
    fact: &SyntaxFact,
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    scopes: &BTreeMap<u64, SymbolId>,
) -> Option<SymbolId> {
    let mut parent = fact.parent();
    let mut remaining = facts_by_id.len();
    while let Some(parent_id) = parent {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        if let Some(symbol) = scopes.get(&parent_id) {
            return Some(*symbol);
        }
        parent = facts_by_id.get(&parent_id).and_then(|fact| fact.parent());
    }
    None
}

fn is_call_fact(fact: &SyntaxFact) -> bool {
    let label = fact.syntax_kind().as_str();
    label.ends_with(".call") || label.ends_with(".scoped_call")
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().next().is_some_and(|character| {
            character == '_' || character == '$' || character.is_alphabetic()
        })
        && value
            .chars()
            .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

fn starts_with_word(value: &str, word: &str) -> bool {
    value == word
        || value
            .strip_prefix(word)
            .is_some_and(|tail| tail.starts_with(char::is_whitespace))
}

fn declaration_header(language: SemanticProjectLanguage, declaration: &str) -> String {
    let line_end = declaration.find('\n').unwrap_or(declaration.len());
    match language {
        SemanticProjectLanguage::Python => declaration
            .get(..line_end)
            .unwrap_or(declaration)
            .trim()
            .to_owned(),
        _ => {
            let brace = declaration.find('{').unwrap_or(line_end);
            declaration
                .get(..brace.min(line_end))
                .unwrap_or(declaration)
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_owned()
        }
    }
}

fn module_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit_once('.')
        .map_or(file, |(stem, _)| stem)
        .to_owned()
}

fn parse_import(
    language: SemanticProjectLanguage,
    text: &str,
) -> Option<(String, Vec<ImportBinding>)> {
    match language {
        SemanticProjectLanguage::Rust => parse_rust_import(text),
        SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript => {
            parse_ecmascript_import(text)
        }
        SemanticProjectLanguage::Python => parse_python_import(text),
        SemanticProjectLanguage::Go => parse_go_import(text),
    }
}

fn parse_rust_import(text: &str) -> Option<(String, Vec<ImportBinding>)> {
    let body = text
        .trim()
        .strip_prefix("use ")?
        .trim_end_matches(';')
        .trim();
    let normalized = body
        .trim_start_matches("crate::")
        .trim_start_matches("self::")
        .trim_start_matches("super::");
    let module = normalized
        .split("::{")
        .next()
        .unwrap_or(normalized)
        .rsplit_once("::")
        .map_or(normalized, |(module, _)| module)
        .replace("::", "/");
    let mut bindings = Vec::new();
    if let Some((_, items)) = normalized.split_once("::{") {
        for item in items.trim_end_matches('}').split(',') {
            push_alias_binding(&mut bindings, item.trim(), " as ");
        }
    } else if let Some(item) = normalized.rsplit("::").next() {
        push_alias_binding(&mut bindings, item.trim(), " as ");
    }
    Some((module, bindings))
}

fn parse_ecmascript_import(text: &str) -> Option<(String, Vec<ImportBinding>)> {
    let module = first_quoted(text)?;
    let mut bindings = Vec::new();
    if let Some((head, _)) = text.rsplit_once(" from ") {
        if let Some((_, named)) = head.split_once('{') {
            for item in named.trim_end_matches('}').split(',') {
                push_alias_binding(&mut bindings, item.trim(), " as ");
            }
        } else if let Some((_, alias)) = head.split_once("* as ") {
            bindings.push(ImportBinding {
                local: alias.trim().to_owned(),
                imported: "*".to_owned(),
            });
        } else if let Some(default) = head.trim().strip_prefix("import ") {
            let local = default.split(',').next().unwrap_or(default).trim();
            if is_identifier(local) {
                bindings.push(ImportBinding {
                    local: local.to_owned(),
                    imported: "default".to_owned(),
                });
            }
        }
    }
    Some((module, bindings))
}

fn parse_python_import(text: &str) -> Option<(String, Vec<ImportBinding>)> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("from ") {
        let (module, items) = rest.split_once(" import ")?;
        let bindings = items
            .trim_matches(['(', ')'])
            .split(',')
            .filter_map(|item| alias_binding(item.trim(), " as "))
            .collect();
        Some((module.trim().to_owned(), bindings))
    } else {
        let rest = trimmed.strip_prefix("import ")?;
        let first = rest.split(',').next()?.trim();
        let binding = alias_binding(first, " as ")?;
        Some((binding.imported.clone(), vec![binding]))
    }
}

fn parse_go_import(text: &str) -> Option<(String, Vec<ImportBinding>)> {
    let module = first_quoted(text)?;
    let before_quote = text
        .split(['"', '`'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches("import")
        .trim_matches(['(', ')'])
        .trim();
    let local = if before_quote.is_empty() || matches!(before_quote, "_" | ".") {
        module.rsplit('/').next().unwrap_or(&module).to_owned()
    } else {
        before_quote
            .split_whitespace()
            .last()
            .unwrap_or_default()
            .to_owned()
    };
    Some((
        module,
        vec![ImportBinding {
            local,
            imported: "*".to_owned(),
        }],
    ))
}

fn push_alias_binding(bindings: &mut Vec<ImportBinding>, value: &str, separator: &str) {
    if let Some(binding) = alias_binding(value, separator) {
        bindings.push(binding);
    }
}

fn alias_binding(value: &str, separator: &str) -> Option<ImportBinding> {
    if value.is_empty() {
        return None;
    }
    let (imported, local) = value
        .split_once(separator)
        .map_or((value, value), |(imported, local)| {
            (imported.trim(), local.trim())
        });
    is_identifier(local).then(|| ImportBinding {
        local: local.to_owned(),
        imported: imported.to_owned(),
    })
}

fn first_quoted(value: &str) -> Option<String> {
    let (start, quote) = value
        .char_indices()
        .find(|(_, character)| matches!(character, '\'' | '"' | '`'))?;
    let tail = value.get(start + quote.len_utf8()..)?;
    let end = tail.find(quote)?;
    Some(tail.get(..end)?.to_owned())
}

fn module_matches(
    language: SemanticProjectLanguage,
    current_path: &str,
    module: &str,
    candidate_path: &str,
) -> bool {
    let candidate_no_extension = strip_extension(candidate_path);
    match language {
        SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript => {
            let resolved = resolve_relative_module(current_path, module);
            candidate_no_extension == resolved
                || candidate_no_extension == format!("{resolved}/index")
        }
        SemanticProjectLanguage::Python => {
            let module = module.trim_start_matches('.').replace('.', "/");
            candidate_no_extension.ends_with(&module)
                || candidate_no_extension.ends_with(&format!("{module}/__init__"))
        }
        SemanticProjectLanguage::Rust => {
            candidate_no_extension.ends_with(module)
                || candidate_no_extension.ends_with(&format!("{module}/mod"))
                || candidate_no_extension.ends_with(&format!("src/{module}"))
        }
        SemanticProjectLanguage::Go => {
            let parent = candidate_path
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent);
            parent.ends_with(module)
                || parent.ends_with(module.rsplit('/').next().unwrap_or(module))
        }
    }
}

fn resolve_relative_module(current_path: &str, module: &str) -> String {
    if !module.starts_with('.') {
        return module.trim_start_matches('/').to_owned();
    }
    let mut components = current_path
        .rsplit_once('/')
        .map_or(Vec::new(), |(parent, _)| {
            parent.split('/').map(str::to_owned).collect()
        });
    for component in module.split('/') {
        match component {
            "." | "" => {}
            ".." => {
                components.pop();
            }
            value => components.push(value.to_owned()),
        }
    }
    components.join("/")
}

fn strip_extension(path: &str) -> String {
    path.rsplit_once('.')
        .map_or(path, |(prefix, _)| prefix)
        .to_owned()
}

fn inheritance_names(
    language: SemanticProjectLanguage,
    header: &str,
) -> Vec<(RelationPredicate, String)> {
    match language {
        SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript => {
            keyword_targets(header, "extends", RelationPredicate::Extends)
                .into_iter()
                .chain(keyword_targets(
                    header,
                    "implements",
                    RelationPredicate::Implements,
                ))
                .collect()
        }
        SemanticProjectLanguage::Python => {
            let Some((_, bases)) = header.split_once('(') else {
                return Vec::new();
            };
            bases
                .split(')')
                .next()
                .unwrap_or_default()
                .split(',')
                .filter_map(|base| {
                    tokenize_identifiers(base)
                        .into_iter()
                        .next()
                        .map(|name| (RelationPredicate::Extends, name))
                })
                .collect()
        }
        SemanticProjectLanguage::Go => {
            if !header.contains(" interface") {
                return Vec::new();
            }
            tokenize_identifiers(header)
                .into_iter()
                .skip(2)
                .filter(|name| name != "interface")
                .map(|name| (RelationPredicate::Embeds, name))
                .collect()
        }
        SemanticProjectLanguage::Rust => Vec::new(),
    }
}

fn keyword_targets(
    header: &str,
    keyword: &str,
    predicate: RelationPredicate,
) -> Vec<(RelationPredicate, String)> {
    let tokens = tokenize_identifiers(header);
    let Some(start) = tokens.iter().position(|token| token == keyword) else {
        return Vec::new();
    };
    tokens
        .into_iter()
        .skip(start + 1)
        .take_while(|token| !matches!(token.as_str(), "extends" | "implements"))
        .map(|name| (predicate, name))
        .collect()
}

fn rust_impl_pairs(source: &str) -> Vec<(String, String)> {
    source
        .lines()
        .filter_map(|line| {
            let tokens = tokenize_identifiers(line.trim_start());
            let impl_index = tokens.iter().position(|token| token == "impl")?;
            let for_index = tokens.iter().position(|token| token == "for")?;
            if for_index <= impl_index + 1 {
                return None;
            }
            let trait_name = tokens.get(for_index.checked_sub(1)?)?.clone();
            let type_name = tokens.get(for_index + 1)?.clone();
            Some((type_name, trait_name))
        })
        .collect()
}

fn tokenize_identifiers(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character == '_' || character == '$' || character.is_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn merge_status(left: CoverageStatus, right: CoverageStatus) -> CoverageStatus {
    if left == CoverageStatus::Unknown || right == CoverageStatus::Unknown {
        CoverageStatus::Unknown
    } else if left == CoverageStatus::Sampled || right == CoverageStatus::Sampled {
        CoverageStatus::Sampled
    } else if left == CoverageStatus::Bounded || right == CoverageStatus::Bounded {
        CoverageStatus::Bounded
    } else {
        CoverageStatus::Complete
    }
}

fn emit_records(
    records: Vec<IrRecord>,
    limits: &rootlight_ir::IrLimits,
    sink: &mut dyn IrBatchSink,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut batch = Vec::new();
    let mut usage = empty_batch_usage();
    for (index, record) in records.into_iter().enumerate() {
        check_periodically(index, cancellation)?;
        let item_usage = IrBatch::new(sink.next_sequence(), vec![record.clone()]).usage(limits)?;
        let candidate = combine_batch_usage(usage, item_usage)?;
        if !batch.is_empty() && !usage_fits(candidate, sink.remaining_budget()) {
            cancellation.check()?;
            sink.push(IrBatch::new(
                sink.next_sequence(),
                std::mem::take(&mut batch),
            ))?;
            usage = empty_batch_usage();
        }
        usage = combine_batch_usage(usage, item_usage)?;
        batch.push(record);
    }
    if !batch.is_empty() {
        cancellation.check()?;
        sink.push(IrBatch::new(sink.next_sequence(), batch))?;
    }
    Ok(())
}

const fn empty_batch_usage() -> StreamUsage {
    StreamUsage::new(1, 0, 0, 0, 0, 0)
}

fn combine_batch_usage(current: StreamUsage, item: StreamUsage) -> Result<StreamUsage, SinkError> {
    Ok(StreamUsage::new(
        1,
        current
            .records()
            .checked_add(item.records())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .output_bytes()
            .checked_add(item.output_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .diagnostics()
            .checked_add(item.diagnostics())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .diagnostic_bytes()
            .checked_add(item.diagnostic_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
        current
            .string_bytes()
            .checked_add(item.string_bytes())
            .ok_or(SinkError::AccountingOverflow)?,
    ))
}

fn usage_fits(usage: StreamUsage, budget: RemainingBudget) -> bool {
    let batch = budget.batch();
    let remaining = budget.remaining();
    usage.batches() <= remaining.batches()
        && usage.records() <= batch.max_records().min(remaining.records())
        && usage.output_bytes() <= batch.max_output_bytes().min(remaining.output_bytes())
        && usage.diagnostics() <= batch.max_diagnostics().min(remaining.diagnostics())
        && usage.diagnostic_bytes()
            <= batch
                .max_diagnostic_bytes()
                .min(remaining.diagnostic_bytes())
        && usage.string_bytes() <= remaining.string_bytes()
}

fn check_periodically(index: usize, cancellation: &Cancellation) -> Result<(), AdapterError> {
    if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
        cancellation.check()?;
    }
    Ok(())
}

fn provider_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("built-in project failure code is valid"),
    }
}
