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
    structural_captured_name, structural_entity_kind, structural_entity_kind_from_source,
    structural_syntax_fact_order,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, ContainerRef, CoverageRecord, CoverageScope,
    CoverageStatus, DiagnosticRecord, DiagnosticSeverity, EntityFlag, EntityKind, EntityRecord,
    EntityVisibility, EvidenceKind, FactDomain, FactEvidence, FactRef, FileIdentityClaim,
    FileRecord, LexicalEvidenceFormat, LexicalEvidenceKind, LexicalEvidenceV1, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord, SkippedRegion, SkippedRegionReason,
    SourceMappingKind, SourceMappingRecord, SourceRef, SourceSpan, SymbolIdentityClaim,
    canonical_rust_impl_scope, canonical_symbol_signature, derive_coverage_record_id,
    derive_diagnostic_record_id, derive_occurrence_record_id, derive_provenance_record_id,
    derive_relation_record_id, derive_rust_impl_scope_identity, derive_skipped_region_id,
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
const MAX_OPTIONAL_PROJECT_SYNTAX_FACTS: usize = 256;
/// Diagnostic emitted when project analysis had to discard syntax facts.
pub const PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC: &str = "project-syntax-fact-limit";
const PROJECT_DIAGNOSTICS_TRUNCATED_CODE: &str = "project-parser-diagnostics-truncated";
const PROJECT_DIAGNOSTICS_TRUNCATED_MESSAGE: &str =
    "additional parser diagnostics were omitted by the project diagnostic limit";
const GO_GIN_ROUTE_INCOMPLETE_CODE: &str = "go-gin-route-incomplete";
const GO_GIN_ROUTE_INCOMPLETE_MESSAGE: &str =
    "Gin route syntax was retained without an exact literal route binding";

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
    /// Java packages, declarations, annotations, inheritance, and calls.
    Java,
    /// C++ namespaces, declarations, templates, linkage, and calls.
    Cpp,
    /// C# namespaces, declarations, inheritance, and calls.
    CSharp,
    /// PHP namespaces, declarations, imports, and calls.
    Php,
    /// C translation units, declarations, linkage, and calls.
    C,
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
            Self::Java => "java",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Php => "php",
            Self::C => "c",
        }
    }

    const fn inferred_call_confidence(self) -> u16 {
        match self {
            Self::Rust | Self::Go | Self::Java | Self::Cpp | Self::CSharp | Self::Php | Self::C => {
                STATIC_CALL_CONFIDENCE
            }
            Self::TypeScript => TYPESCRIPT_CALL_CONFIDENCE,
            Self::JavaScript | Self::Python => DYNAMIC_CALL_CONFIDENCE,
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
            if std::str::from_utf8(input.source().bytes()).is_err() {
                // Encoding fixtures are legitimate repository inputs. Retain
                // their identity at Tier B while declaring their facts bounded.
                parsed.push(ParsedInput {
                    input,
                    facts: Vec::new(),
                    diagnostics: vec![AdapterDiagnostic::new(
                        DiagnosticCode::new("invalid-utf8")
                            .map_err(|_| provider_failure("project-diagnostic-code"))?,
                        DiagnosticSeverity::Warning,
                        Some(input.source().source_ref().clone()),
                        CoverageStatus::Bounded,
                    )],
                    parse_status: CoverageStatus::Bounded,
                    syntax_nodes: 0,
                    max_syntax_depth: 0,
                });
                continue;
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
        bound_project_syntax_facts(self.language, &mut parsed)?;
        ProjectFactsBuilder::new(self, request, parsed, cancellation).build()
    }
}

fn bound_project_syntax_facts(
    language: SemanticProjectLanguage,
    parsed: &mut [ParsedInput<'_, '_>],
) -> Result<(), AdapterError> {
    let maximum_optional_facts = project_optional_syntax_fact_limit(parsed.len());
    let total_optional_facts = parsed.iter().try_fold(0_usize, |total, input| {
        let mandatory = mandatory_project_syntax_fact_ids(&input.facts).len();
        let optional = input
            .facts
            .len()
            .checked_sub(mandatory)
            .ok_or_else(|| provider_failure("project-fact-accounting"))?;
        total
            .checked_add(optional)
            .ok_or_else(|| provider_failure("project-fact-accounting"))
    })?;
    if total_optional_facts <= maximum_optional_facts {
        return Ok(());
    }

    let code = DiagnosticCode::new(PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC)
        .map_err(|_| provider_failure("project-diagnostic-code"))?;
    let mut remaining_optional_facts = maximum_optional_facts;
    let mut remaining_inputs = parsed.len();
    // Allocate the fixed budget to positive test evidence first, then to the
    // most repeated local relationship, independent of repository path order.
    let priorities = parsed
        .iter()
        .map(|input| {
            project_input_relationship_priority(
                language,
                &input.facts,
                input.input.source().bytes(),
            )
        })
        .collect::<Vec<_>>();
    let mut selection_order = (0..parsed.len()).collect::<Vec<_>>();
    selection_order.sort_unstable_by(|left, right| {
        priorities[*right]
            .is_test
            .cmp(&priorities[*left].is_test)
            .then_with(|| priorities[*right].demand.cmp(&priorities[*left].demand))
            .then_with(|| {
                parsed[*left]
                    .input
                    .source()
                    .path()
                    .as_str()
                    .cmp(parsed[*right].input.source().path().as_str())
            })
    });
    for input_index in selection_order {
        let input = &mut parsed[input_index];
        let fair_allowance = remaining_optional_facts
            .checked_add(remaining_inputs.saturating_sub(1))
            .and_then(|value| value.checked_div(remaining_inputs))
            .ok_or_else(|| provider_failure("project-fact-accounting"))?;
        // A relationship needs its complete syntax unit. Borrow enough to
        // retain one local edge and one callable binding per import statement,
        // so one dependency's fanout cannot hide every later dependency.
        let preferred_allowance =
            preferred_relationship_allowance(language, &input.facts, input.input.source().bytes());
        let allowance = fair_allowance
            .max(preferred_allowance)
            .min(remaining_optional_facts);
        let mandatory = mandatory_project_syntax_fact_ids(&input.facts).len();
        let original_len = input.facts.len();
        retain_project_syntax_facts(
            language,
            &mut input.facts,
            input.input.source().bytes(),
            allowance,
        );
        if input.facts.len() < original_len {
            input.diagnostics.push(AdapterDiagnostic::new(
                code.clone(),
                DiagnosticSeverity::Warning,
                Some(input.input.source().source_ref().clone()),
                CoverageStatus::Bounded,
            ));
            input.parse_status = merge_status(input.parse_status, CoverageStatus::Bounded);
        }
        let retained_optional = input
            .facts
            .len()
            .checked_sub(mandatory)
            .ok_or_else(|| provider_failure("project-fact-accounting"))?;
        remaining_optional_facts = remaining_optional_facts
            .checked_sub(retained_optional)
            .ok_or_else(|| provider_failure("project-fact-accounting"))?;
        remaining_inputs = remaining_inputs
            .checked_sub(1)
            .ok_or_else(|| provider_failure("project-fact-accounting"))?;
    }
    Ok(())
}

const fn project_optional_syntax_fact_limit(_input_count: usize) -> usize {
    MAX_OPTIONAL_PROJECT_SYNTAX_FACTS
}

fn retain_project_syntax_facts(
    language: SemanticProjectLanguage,
    facts: &mut Vec<SyntaxFact>,
    source: &[u8],
    allowance: usize,
) {
    let facts_by_id = facts
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    // Identity closures are a required linear minimum; only syntax that can
    // expand optional relationship evidence is charged against the fixed cap.
    let mut selected = mandatory_project_syntax_fact_ids(facts);
    let mut remaining = allowance;

    let call_names = terminal_call_names(facts);
    let declared_calls = declared_call_ids(facts, source, &facts_by_id, &call_names);
    if let Some(call) = preferred_local_call_syntax_fact_group(
        language,
        facts,
        source,
        &facts_by_id,
        &call_names,
        &declared_calls,
    )
    .and_then(|group| facts_by_id.get(&group.call_id).copied())
    {
        select_call_syntax_fact_group(
            call,
            &call_names,
            &facts_by_id,
            &mut selected,
            &mut remaining,
        );
    }
    let imported_call_groups = imported_call_syntax_fact_groups(
        language,
        facts,
        source,
        &facts_by_id,
        &call_names,
        &declared_calls,
    );
    for group in imported_call_groups {
        if remaining == 0 {
            break;
        }
        let required = imported_call_syntax_fact_group_ids(&group, &facts_by_id, &call_names);
        select_syntax_fact_ids(required, &mut selected, &mut remaining);
    }
    for call in facts
        .iter()
        .filter(|fact| is_call_fact(fact))
        .filter(|fact| declared_calls.contains(&fact.local_id()))
    {
        if remaining == 0 {
            break;
        }
        select_call_syntax_fact_group(
            call,
            &call_names,
            &facts_by_id,
            &mut selected,
            &mut remaining,
        );
    }
    for fact in facts
        .iter()
        .filter(|fact| fact.kind() == SyntaxFactKind::Import)
    {
        if remaining == 0 {
            break;
        }
        select_syntax_fact_group([fact], &facts_by_id, &mut selected, &mut remaining);
    }
    for call in facts
        .iter()
        .filter(|fact| is_call_fact(fact))
        .filter(|fact| !declared_calls.contains(&fact.local_id()))
    {
        if remaining == 0 {
            break;
        }
        select_call_syntax_fact_group(
            call,
            &call_names,
            &facts_by_id,
            &mut selected,
            &mut remaining,
        );
    }
    for kind in [
        SyntaxFactKind::Occurrence,
        SyntaxFactKind::Scope,
        SyntaxFactKind::Root,
        SyntaxFactKind::Module,
        SyntaxFactKind::Signature,
        SyntaxFactKind::ErrorRecovery,
        SyntaxFactKind::EmbeddedRegion,
        SyntaxFactKind::Comment,
        SyntaxFactKind::StringLiteral,
    ] {
        for fact in facts
            .iter()
            .filter(|fact| fact.kind() == kind && !is_call_fact(fact) && !is_call_name_fact(fact))
        {
            if remaining == 0 {
                break;
            }
            select_syntax_fact_group([fact], &facts_by_id, &mut selected, &mut remaining);
        }
    }
    facts.retain(|fact| selected.contains(&fact.local_id()));
}

fn preferred_relationship_allowance(
    language: SemanticProjectLanguage,
    facts: &[SyntaxFact],
    source: &[u8],
) -> usize {
    let facts_by_id = facts
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    let mandatory = mandatory_project_syntax_fact_ids(facts);
    let call_names = terminal_call_names(facts);
    let declared_calls = declared_call_ids(facts, source, &facts_by_id, &call_names);
    let mut preferred = preferred_local_call_syntax_fact_group(
        language,
        facts,
        source,
        &facts_by_id,
        &call_names,
        &declared_calls,
    )
    .and_then(|group| facts_by_id.get(&group.call_id).copied())
    .map_or_else(BTreeSet::new, |call| {
        let terminal = call_names
            .get(&call.local_id())
            .and_then(|name| facts_by_id.get(name))
            .copied();
        syntax_fact_group_ids(std::iter::once(call).chain(terminal), &facts_by_id)
    });
    let groups = imported_call_syntax_fact_groups(
        language,
        facts,
        source,
        &facts_by_id,
        &call_names,
        &declared_calls,
    );
    let mut represented_imports = BTreeSet::new();
    for group in groups {
        if represented_imports.contains(&group.import_id) {
            continue;
        }
        let mut candidate = preferred.clone();
        candidate.extend(imported_call_syntax_fact_group_ids(
            &group,
            &facts_by_id,
            &call_names,
        ));
        let optional = candidate
            .iter()
            .filter(|local_id| !mandatory.contains(local_id))
            .count();
        if optional <= MAX_OPTIONAL_PROJECT_SYNTAX_FACTS {
            preferred = candidate;
            represented_imports.insert(group.import_id);
        }
    }
    preferred
        .iter()
        .filter(|local_id| !mandatory.contains(local_id))
        .count()
}

#[derive(Debug, Clone, Copy, Default)]
struct PreferredLocalCallGroup {
    call_id: u64,
    is_test: bool,
    demand: usize,
}

fn project_input_relationship_priority(
    language: SemanticProjectLanguage,
    facts: &[SyntaxFact],
    source: &[u8],
) -> PreferredLocalCallGroup {
    let facts_by_id = facts
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    let call_names = terminal_call_names(facts);
    let declared_calls = declared_call_ids(facts, source, &facts_by_id, &call_names);
    preferred_local_call_syntax_fact_group(
        language,
        facts,
        source,
        &facts_by_id,
        &call_names,
        &declared_calls,
    )
    .unwrap_or_default()
}

fn preferred_local_call_syntax_fact_group(
    language: SemanticProjectLanguage,
    facts: &[SyntaxFact],
    source: &[u8],
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    call_names: &BTreeMap<u64, u64>,
    declared_calls: &BTreeSet<u64>,
) -> Option<PreferredLocalCallGroup> {
    let test_declarations = positive_test_declarations(language, facts);
    let mut groups = BTreeMap::<(bool, String), Vec<&SyntaxFact>>::new();
    for call in facts.iter().filter(|fact| is_call_fact(fact)) {
        let is_test = fact_is_enclosed_by(call, facts_by_id, &test_declarations);
        if !is_test && !declared_calls.contains(&call.local_id()) {
            continue;
        }
        let Some(name) = retained_call_name(source, call, call_names, facts_by_id) else {
            continue;
        };
        groups
            .entry((is_test, name.to_owned()))
            .or_default()
            .push(call);
    }
    let ((is_test, _), calls) = groups.into_iter().max_by(
        |((left_test, left_name), left_calls), ((right_test, right_name), right_calls)| {
            left_test
                .cmp(right_test)
                .then_with(|| left_calls.len().cmp(&right_calls.len()))
                .then_with(|| right_name.cmp(left_name))
        },
    )?;
    let demand = calls.len();
    let call = calls
        .into_iter()
        .min_by(|left, right| structural_syntax_fact_order(left, right))?;
    Some(PreferredLocalCallGroup {
        call_id: call.local_id(),
        is_test,
        demand,
    })
}

fn fact_is_enclosed_by(
    fact: &SyntaxFact,
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    declarations: &BTreeSet<u64>,
) -> bool {
    let mut parent = fact.parent();
    let mut remaining = facts_by_id.len();
    while let Some(parent_id) = parent {
        if declarations.contains(&parent_id) {
            return true;
        }
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
        parent = facts_by_id
            .get(&parent_id)
            .and_then(|candidate| candidate.parent());
    }
    false
}

fn declared_call_ids(
    facts: &[SyntaxFact],
    source: &[u8],
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    call_names: &BTreeMap<u64, u64>,
) -> BTreeSet<u64> {
    let declared_names = facts
        .iter()
        .filter(|fact| is_definition_fact(fact))
        .filter_map(|fact| source_text(source, fact.span()))
        .collect::<BTreeSet<_>>();
    facts
        .iter()
        .filter(|fact| is_call_fact(fact))
        .filter_map(|call| {
            retained_call_name(source, call, call_names, facts_by_id)
                .filter(|name| declared_names.contains(name))
                .map(|_| call.local_id())
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ImportedCallBinding {
    Named(String),
    Namespace(String),
    Wildcard(String),
}

#[derive(Debug)]
struct ImportedCallSyntaxFactGroup {
    import_id: u64,
    binding: ImportedCallBinding,
    call_ids: Vec<u64>,
}

fn imported_call_syntax_fact_groups(
    language: SemanticProjectLanguage,
    facts: &[SyntaxFact],
    source: &[u8],
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    call_names: &BTreeMap<u64, u64>,
    declared_calls: &BTreeSet<u64>,
) -> Vec<ImportedCallSyntaxFactGroup> {
    let calls = facts
        .iter()
        .filter(|fact| is_call_fact(fact))
        .filter(|fact| !declared_calls.contains(&fact.local_id()))
        .filter_map(|call| {
            let name = retained_call_name(source, call, call_names, facts_by_id)?;
            let receiver =
                source_text(source, call.span()).and_then(|text| call_receiver(text, name));
            Some((
                call.local_id(),
                name.to_owned(),
                receiver.map(str::to_owned),
            ))
        })
        .collect::<Vec<_>>();
    let mut grouped_calls = BTreeMap::<(u64, ImportedCallBinding), BTreeSet<u64>>::new();
    for import in facts
        .iter()
        .filter(|fact| fact.kind() == SyntaxFactKind::Import)
    {
        let Some(text) = source_text(source, import.span()) else {
            continue;
        };
        for (_, bindings) in parse_import(language, text) {
            for binding in bindings {
                for (call_id, call_name, receiver) in &calls {
                    let binding = match &binding {
                        ImportBinding::Named { local, .. }
                            if call_name == local && receiver.is_none() =>
                        {
                            ImportedCallBinding::Named(local.clone())
                        }
                        ImportBinding::Namespace { local }
                            if receiver.as_deref() == Some(local.as_str()) =>
                        {
                            ImportedCallBinding::Namespace(local.clone())
                        }
                        ImportBinding::Wildcard if receiver.is_none() => {
                            ImportedCallBinding::Wildcard(call_name.clone())
                        }
                        ImportBinding::Named { .. }
                        | ImportBinding::Namespace { .. }
                        | ImportBinding::Wildcard
                        | ImportBinding::SideEffect => continue,
                    };
                    grouped_calls
                        .entry((import.local_id(), binding))
                        .or_default()
                        .insert(*call_id);
                }
            }
        }
    }
    let mut groups_by_import = BTreeMap::<u64, Vec<ImportedCallSyntaxFactGroup>>::new();
    for ((import_id, binding), call_ids) in grouped_calls {
        groups_by_import
            .entry(import_id)
            .or_default()
            .push(ImportedCallSyntaxFactGroup {
                import_id,
                binding,
                call_ids: call_ids.into_iter().collect(),
            });
    }
    let mut groups = groups_by_import
        .into_values()
        .map(|mut groups| {
            groups.sort_unstable_by(|left, right| {
                right
                    .call_ids
                    .len()
                    .cmp(&left.call_ids.len())
                    .then_with(|| left.binding.cmp(&right.binding))
            });
            groups.into_iter()
        })
        .collect::<Vec<_>>();
    let mut interleaved = Vec::new();
    loop {
        let mut retained = false;
        for groups in &mut groups {
            if let Some(group) = groups.next() {
                interleaved.push(group);
                retained = true;
            }
        }
        if !retained {
            break;
        }
    }
    interleaved
}

fn imported_call_syntax_fact_group_ids(
    group: &ImportedCallSyntaxFactGroup,
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    call_names: &BTreeMap<u64, u64>,
) -> BTreeSet<u64> {
    let facts = std::iter::once(group.import_id)
        .chain(group.call_ids.iter().copied())
        .filter_map(|local_id| facts_by_id.get(&local_id).copied())
        .flat_map(|fact| {
            let terminal = is_call_fact(fact)
                .then(|| call_names.get(&fact.local_id()))
                .flatten()
                .and_then(|name| facts_by_id.get(name))
                .copied();
            std::iter::once(fact).chain(terminal)
        });
    syntax_fact_group_ids(facts, facts_by_id)
}

fn select_call_syntax_fact_group(
    call: &SyntaxFact,
    call_names: &BTreeMap<u64, u64>,
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    selected: &mut BTreeSet<u64>,
    remaining: &mut usize,
) {
    // A call and its terminal name form one relationship-bearing unit.
    // Admitting either capture alone spends budget without a resolvable site.
    let terminal = call_names
        .get(&call.local_id())
        .and_then(|name| facts_by_id.get(name))
        .copied();
    select_syntax_fact_group(
        std::iter::once(call).chain(terminal),
        facts_by_id,
        selected,
        remaining,
    );
}

fn retained_call_name<'a>(
    source: &'a [u8],
    call: &SyntaxFact,
    terminal_call_names: &BTreeMap<u64, u64>,
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
) -> Option<&'a str> {
    terminal_call_names
        .get(&call.local_id())
        .and_then(|terminal| facts_by_id.get(terminal))
        .and_then(|terminal| source_text(source, terminal.span()))
        .or_else(|| {
            source_text(source, call.span())
                .and_then(|text| occurrence_name(text, true))
                .map(|parsed| parsed.name)
        })
}

fn mandatory_project_syntax_fact_ids(facts: &[SyntaxFact]) -> BTreeSet<u64> {
    let facts_by_id = facts
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeSet::new();
    for fact in facts.iter().filter(|fact| {
        structural_entity_kind(fact).is_some()
            || is_definition_fact(fact)
            || is_symbol_signature_fact(fact)
            || is_identity_capture_fact(fact)
    }) {
        select_mandatory_syntax_fact_group([fact], &facts_by_id, &mut selected);
    }
    selected
}

fn is_identity_capture_fact(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "rust.impl_trait.scope_trait"
            | "rust.impl_type.scope_type"
            | "rust.test_attribute.test_attribute"
            | "java.test_attribute.test_attribute"
            | "cpp.test_attribute.test_attribute"
    )
}

fn select_mandatory_syntax_fact_group<'fact>(
    facts: impl IntoIterator<Item = &'fact SyntaxFact>,
    facts_by_id: &BTreeMap<u64, &'fact SyntaxFact>,
    selected: &mut BTreeSet<u64>,
) {
    for fact in facts {
        let mut current = Some(fact);
        let mut traversed = 0_usize;
        while let Some(candidate) = current {
            if traversed >= facts_by_id.len() || !selected.insert(candidate.local_id()) {
                break;
            }
            traversed += 1;
            current = candidate
                .parent()
                .and_then(|parent| facts_by_id.get(&parent).copied());
        }
    }
}

fn select_syntax_fact_group<'fact>(
    facts: impl IntoIterator<Item = &'fact SyntaxFact>,
    facts_by_id: &BTreeMap<u64, &'fact SyntaxFact>,
    selected: &mut BTreeSet<u64>,
    remaining: &mut usize,
) {
    let required = syntax_fact_group_ids(facts, facts_by_id);
    select_syntax_fact_ids(required, selected, remaining);
}

fn select_syntax_fact_ids(
    required: BTreeSet<u64>,
    selected: &mut BTreeSet<u64>,
    remaining: &mut usize,
) {
    let additional = required
        .iter()
        .filter(|local_id| !selected.contains(local_id))
        .count();
    if additional > *remaining {
        return;
    }
    selected.extend(required);
    *remaining -= additional;
}

fn syntax_fact_group_ids<'fact>(
    facts: impl IntoIterator<Item = &'fact SyntaxFact>,
    facts_by_id: &BTreeMap<u64, &'fact SyntaxFact>,
) -> BTreeSet<u64> {
    let mut required = BTreeSet::new();
    for fact in facts {
        let mut current = Some(fact);
        let mut traversed = 0_usize;
        while let Some(candidate) = current {
            if traversed >= facts_by_id.len() || !required.insert(candidate.local_id()) {
                break;
            }
            traversed += 1;
            current = candidate
                .parent()
                .and_then(|parent| facts_by_id.get(&parent).copied());
        }
    }
    required
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
    visibility: EntityVisibility,
    declaring_type: Option<String>,
    arity: Option<usize>,
    is_test: bool,
    file: FileId,
    span: SourceSpan,
    source: SourceRef,
}

#[derive(Debug, Clone)]
struct DeclarationDraft {
    local_id: u64,
    file: FileId,
    span: SourceSpan,
    name: String,
    header: String,
    signature: String,
    kind: EntityKind,
    visibility: EntityVisibility,
    parent_declaration: Option<u64>,
    scope_identity: Option<[u8; 32]>,
    declaring_type: Option<String>,
    arity: Option<usize>,
    is_test: bool,
    source: SourceRef,
}

#[derive(Default)]
struct RustImplCaptures<'fact> {
    self_type: Option<&'fact SyntaxFact>,
    trait_type: Option<&'fact SyntaxFact>,
    invalid: bool,
}

impl<'fact> RustImplCaptures<'fact> {
    fn insert_self_type(&mut self, fact: &'fact SyntaxFact) {
        self.invalid |= self.self_type.replace(fact).is_some();
    }

    fn insert_trait_type(&mut self, fact: &'fact SyntaxFact) {
        self.invalid |= self.trait_type.replace(fact).is_some();
    }
}

#[derive(Debug, Clone)]
struct ImportDraft {
    file: FileId,
    span: SourceSpan,
    source: SourceRef,
    module: String,
    bindings: Vec<ImportBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportBinding {
    Named { local: String, imported: String },
    Namespace { local: String },
    Wildcard,
    SideEffect,
}

#[derive(Debug, Clone)]
struct OccurrenceDraft {
    file: FileId,
    name: String,
    qualifier: Option<String>,
    call_text: Option<String>,
    arity: Option<usize>,
    syntax_kind: String,
    role: OccurrenceRole,
    enclosing_declaration: Option<u64>,
    enclosing: Option<SymbolId>,
    source: SourceRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionKind {
    Binding,
    DynamicDispatch,
}

#[derive(Debug, Clone)]
struct ResolutionCandidates {
    symbols: Vec<SymbolId>,
    kind: ResolutionKind,
}

#[derive(Debug, Clone)]
struct BoundedResolutionCandidates {
    symbols: Vec<SymbolId>,
    total_count: u64,
    completeness: CoverageStatus,
    kind: ResolutionKind,
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
    package_by_file: BTreeMap<FileId, String>,
    symbol_by_declaration: BTreeMap<(FileId, u64), SymbolId>,
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
            package_by_file: BTreeMap::new(),
            symbol_by_declaration: BTreeMap::new(),
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

            let module_name = path.clone();
            let module_span = source.span();
            let module_claim = self.symbol_claim(
                EntityKind::Module,
                ContainerRef::File(source.span().file()),
                &module_name,
                "",
                None,
            );
            let module_symbol = module_claim.symbol;
            let mut module_flags = generated_flag(input.is_generated());
            module_flags.push(EntityFlag::Synthetic);
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
                flags: module_flags,
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
                visibility: EntityVisibility::Unknown,
                declaring_type: None,
                arity: None,
                is_test: false,
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
            let mut ordered_facts = facts.iter().collect::<Vec<_>>();
            ordered_facts.sort_by(|left, right| structural_syntax_fact_order(left, right));
            let mut nearest_declaration = BTreeMap::<u64, Option<u64>>::new();
            let mut definition_captures = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
            let mut signature_captures = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
            let mut rust_impl_captures = BTreeMap::<u64, RustImplCaptures<'_>>::new();
            for fact in &ordered_facts {
                let parent_declaration = fact
                    .parent()
                    .and_then(|parent| nearest_declaration.get(&parent).copied().flatten());
                if structural_entity_kind(fact).is_some() {
                    nearest_declaration.insert(fact.local_id(), Some(fact.local_id()));
                } else {
                    nearest_declaration.insert(fact.local_id(), parent_declaration);
                    if let Some(declaration) = parent_declaration {
                        if is_definition_fact(fact) {
                            definition_captures
                                .entry(declaration)
                                .or_default()
                                .push(fact);
                        } else if is_symbol_signature_fact(fact) {
                            signature_captures
                                .entry(declaration)
                                .or_default()
                                .push(fact);
                        }
                    }
                }
                if let Some(parent) = fact.parent() {
                    match fact.syntax_kind().as_str() {
                        "rust.impl_trait.scope_trait" => rust_impl_captures
                            .entry(parent)
                            .or_default()
                            .insert_trait_type(fact),
                        "rust.impl_type.scope_type" => rust_impl_captures
                            .entry(parent)
                            .or_default()
                            .insert_self_type(fact),
                        _ => {}
                    }
                }
            }
            let positive_test_declarations =
                positive_test_declarations(self.analyzer.language, &facts);
            let terminal_call_names = terminal_call_names(&facts);
            let mut scope_facts = facts
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::Scope)
                .collect::<Vec<_>>();
            scope_facts.sort_by(|left, right| structural_syntax_fact_order(left, right));
            let mut rust_impl_identities = BTreeMap::new();
            let mut rust_impl_types = BTreeMap::new();
            if self.analyzer.language == SemanticProjectLanguage::Rust {
                for scope in &scope_facts {
                    if scope.syntax_kind().as_str() != "rust.impl.scope" {
                        continue;
                    }
                    let Some(captures) = rust_impl_captures
                        .get(&scope.local_id())
                        .filter(|captures| !captures.invalid)
                    else {
                        continue;
                    };
                    let Some(self_type) = captures
                        .self_type
                        .and_then(|fact| source_text(bytes, fact.span()))
                    else {
                        continue;
                    };
                    let trait_type = match captures.trait_type {
                        Some(fact) => {
                            let Some(text) = source_text(bytes, fact.span()) else {
                                continue;
                            };
                            Some(text)
                        }
                        None => None,
                    };
                    let Some(header) = canonical_rust_impl_scope(
                        self_type,
                        trait_type,
                        self.request.limits().ir().max_string_bytes,
                    )
                    .map_err(|_| provider_failure("project-rust-impl-identity"))?
                    else {
                        continue;
                    };
                    let parent = enclosing_rust_impl_scope_before_declaration(scope, &facts_by_id)
                        .and_then(|parent| rust_impl_identities.get(&parent.local_id()).copied());
                    let identity = derive_rust_impl_scope_identity(parent, header.header())
                        .map_err(|_| provider_failure("project-rust-impl-identity"))?;
                    rust_impl_identities.insert(scope.local_id(), identity);
                    if let Some(type_name) = rust_impl_self_type_name(self_type) {
                        rust_impl_types.insert(scope.local_id(), type_name.to_owned());
                    }
                }
            }
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
                    None,
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
                    visibility: EntityVisibility::Private,
                    declaring_type: None,
                    arity: None,
                    is_test: false,
                    file: source.span().file(),
                    span: source.span(),
                    source,
                });
                self.state_mut(input)?.increment(FactDomain::Entities)?;
                self.state_mut(input)?.increment(FactDomain::Extensions)?;
            }

            let mut selected_definition_spans = BTreeSet::new();
            let mut declaration_kinds = BTreeMap::<u64, EntityKind>::new();
            let mut declaration_names = BTreeMap::<u64, String>::new();
            // Capture ownership includes every reviewed declaration, but entity
            // ancestry can stop only at declarations that survive materialization.
            // Replaying every fact mirrors structural lowering when an unmaterialized
            // wrapper sits between a child declaration and its real parent.
            let mut materialized_declarations = BTreeSet::new();
            let mut nearest_materialized_declaration = BTreeMap::<u64, Option<u64>>::new();
            let mut nearest_callable_declaration = BTreeMap::<u64, Option<u64>>::new();
            for (fact_index, declaration) in ordered_facts.iter().enumerate() {
                check_periodically(fact_index, self.cancellation)?;
                let declaration = *declaration;
                let parent_declaration = declaration.parent().and_then(|parent| {
                    materialized_declarations
                        .contains(&parent)
                        .then_some(parent)
                        .or_else(|| {
                            nearest_materialized_declaration
                                .get(&parent)
                                .copied()
                                .flatten()
                        })
                });
                let parent_callable = declaration.parent().and_then(|parent| {
                    nearest_callable_declaration.get(&parent).copied().flatten()
                });
                if structural_entity_kind(declaration).is_none() {
                    nearest_materialized_declaration
                        .insert(declaration.local_id(), parent_declaration);
                    nearest_callable_declaration.insert(declaration.local_id(), parent_callable);
                    continue;
                }
                let draft = 'draft: {
                    let Some(definition) = definition_captures
                        .get(&declaration.local_id())
                        .and_then(|captures| select_unique_syntax_fact(captures))
                    else {
                        break 'draft None;
                    };
                    let Some(name) = source_text(bytes, definition.span()).and_then(|name| {
                        structural_captured_name(name, self.request.limits().ir().max_string_bytes)
                    }) else {
                        break 'draft None;
                    };
                    let declaration_text = source_text(bytes, declaration.span())
                        .ok_or_else(|| provider_failure("project-declaration-span"))?;
                    let header = declaration_header(self.analyzer.language, declaration_text);
                    if header.is_empty() {
                        break 'draft None;
                    }
                    let rust_impl_scope = (self.analyzer.language == SemanticProjectLanguage::Rust)
                        .then(|| {
                            enclosing_rust_impl_scope_before_declaration(declaration, &facts_by_id)
                        })
                        .flatten();
                    let scope_identity = if let Some(scope) = rust_impl_scope {
                        let Some(identity) = rust_impl_identities.get(&scope.local_id()).copied()
                        else {
                            break 'draft None;
                        };
                        Some(identity)
                    } else {
                        None
                    };
                    let declaring_type = rust_impl_scope
                        .and_then(|scope| rust_impl_types.get(&scope.local_id()).cloned())
                        .or_else(|| {
                            parent_declaration
                                .and_then(|parent| declaration_names.get(&parent).cloned())
                        });
                    let is_type_member = parent_declaration
                        .and_then(|parent| declaration_kinds.get(&parent))
                        .is_some_and(|kind| {
                            matches!(
                                kind,
                                EntityKind::Class
                                    | EntityKind::Struct
                                    | EntityKind::Enum
                                    | EntityKind::Trait
                                    | EntityKind::Interface
                                    | EntityKind::Protocol
                            )
                        })
                        || scope_identity.is_some();
                    let visibility = declaration_visibility(
                        self.analyzer.language,
                        bytes,
                        declaration.span(),
                        &header,
                        name,
                    );
                    let Some(mut kind) =
                        structural_entity_kind_from_source(declaration, declaration_text)
                    else {
                        break 'draft None;
                    };
                    if kind == EntityKind::Function && is_type_member {
                        kind = EntityKind::Method;
                    }
                    let signature = if supports_symbol_signature(kind) {
                        signature_captures
                            .get(&declaration.local_id())
                            .and_then(|captures| select_unique_syntax_fact(captures))
                            .and_then(|signature| source_text(bytes, signature.span()))
                            .and_then(|signature| {
                                canonical_symbol_signature(
                                    signature,
                                    self.request.limits().ir().max_string_bytes,
                                )
                            })
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let arity = parameter_arity(&signature);
                    break 'draft Some((
                        definition.span(),
                        DeclarationDraft {
                            local_id: declaration.local_id(),
                            file: definition.span().file(),
                            span: declaration.span(),
                            name: name.to_owned(),
                            header,
                            signature,
                            kind,
                            visibility,
                            parent_declaration,
                            scope_identity,
                            declaring_type,
                            arity,
                            is_test: positive_test_declarations.contains(&declaration.local_id())
                                || declaration_is_test(
                                    self.analyzer.language,
                                    self.path_by_file
                                        .get(&definition.span().file())
                                        .map(String::as_str)
                                        .unwrap_or_default(),
                                    name,
                                ),
                            source: source_for_span(input, declaration.span()),
                        },
                    ));
                };
                if let Some((definition_span, draft)) = draft {
                    let callable_declaration =
                        is_callable_entity_kind(draft.kind).then_some(declaration.local_id());
                    selected_definition_spans.insert(definition_span);
                    declaration_kinds.insert(declaration.local_id(), draft.kind);
                    declaration_names.insert(declaration.local_id(), draft.name.clone());
                    self.declarations.push(draft);
                    materialized_declarations.insert(declaration.local_id());
                    nearest_materialized_declaration
                        .insert(declaration.local_id(), Some(declaration.local_id()));
                    let callable = callable_declaration.or(parent_callable);
                    nearest_callable_declaration.insert(declaration.local_id(), callable);
                } else {
                    nearest_materialized_declaration
                        .insert(declaration.local_id(), parent_declaration);
                    nearest_callable_declaration.insert(declaration.local_id(), parent_callable);
                }
            }

            for (fact_index, fact) in facts.iter().enumerate() {
                check_periodically(fact_index, self.cancellation)?;
                match fact.kind() {
                    SyntaxFactKind::Import => {
                        let Some(text) = source_text(bytes, fact.span()) else {
                            continue;
                        };
                        for import in parse_import(self.analyzer.language, text) {
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
                        if is_call_name_fact(fact) {
                            continue;
                        }
                        let Some(observed_text) = source_text(bytes, fact.span()) else {
                            continue;
                        };
                        let call = is_call_fact(fact);
                        let terminal_name = terminal_call_names
                            .get(&fact.local_id())
                            .and_then(|terminal| facts_by_id.get(terminal))
                            .and_then(|terminal| source_text(bytes, terminal.span()));
                        let parsed_name = if call {
                            terminal_name
                                .and_then(|name| {
                                    is_identifier(name).then(|| ParsedOccurrenceName {
                                        name,
                                        qualifier: call_receiver(observed_text, name),
                                    })
                                })
                                .or_else(|| occurrence_name(observed_text, true))
                        } else {
                            occurrence_name(observed_text, false)
                        };
                        let Some(parsed_name) = parsed_name else {
                            continue;
                        };
                        if self.analyzer.language == SemanticProjectLanguage::Go
                            && fact.syntax_kind().as_str() == "go.package_identifier.definition"
                        {
                            match self.package_by_file.entry(fact.span().file()) {
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    entry.insert(parsed_name.name.to_owned());
                                }
                                std::collections::btree_map::Entry::Occupied(entry)
                                    if entry.get() == parsed_name.name => {}
                                std::collections::btree_map::Entry::Occupied(_) => {
                                    return Err(provider_failure("go-package-scope"));
                                }
                            }
                        }
                        let in_import = self.imports.iter().any(|import| {
                            import.file == fact.span().file()
                                && contains_span(import.span, fact.span())
                        });
                        let role = if selected_definition_spans.contains(&fact.span()) {
                            OccurrenceRole::Definition
                        } else if in_import {
                            OccurrenceRole::ImportUse
                        } else if call {
                            OccurrenceRole::CallSite
                        } else {
                            OccurrenceRole::Reference
                        };
                        let nearest_declaration = fact.parent().and_then(|parent| {
                            nearest_materialized_declaration
                                .get(&parent)
                                .copied()
                                .flatten()
                        });
                        // A local declaration can contain a call without being its
                        // executable owner; call graphs must retain the callable scope.
                        // Parser recovery can leave the parent-first cache incomplete,
                        // so the bounded ancestry walk provides the same exact fallback.
                        let enclosing_declaration = if role == OccurrenceRole::CallSite {
                            fact.parent()
                                .and_then(|parent| {
                                    nearest_callable_declaration.get(&parent).copied().flatten()
                                })
                                .or_else(|| {
                                    enclosing_callable_declaration(
                                        fact,
                                        &facts_by_id,
                                        &declaration_kinds,
                                    )
                                })
                                .or(nearest_declaration)
                        } else {
                            nearest_declaration
                        };
                        let enclosing = enclosing_scope(fact, &facts_by_id, &scope_symbols)
                            .or_else(|| self.module_by_file.get(&fact.span().file()).copied());
                        self.occurrences.push(OccurrenceDraft {
                            file: fact.span().file(),
                            name: parsed_name.name.to_owned(),
                            qualifier: parsed_name.qualifier.map(str::to_owned),
                            call_text: call.then(|| observed_text.to_owned()),
                            arity: call.then(|| call_arity(observed_text)).flatten(),
                            syntax_kind: fact.syntax_kind().as_str().to_owned(),
                            role,
                            enclosing_declaration,
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
        let mut materialized_symbols = self
            .entities
            .iter()
            .map(|entity| entity.symbol)
            .collect::<BTreeSet<_>>();
        let mut qualified_by_declaration = BTreeMap::<(FileId, u64), String>::new();
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
                .parent_declaration
                .and_then(|parent| {
                    self.symbol_by_declaration
                        .get(&(draft.file, parent))
                        .copied()
                })
                .map(ContainerRef::Entity)
                .or_else(|| {
                    uses_explicit_file_module(self.analyzer.language)
                        .then(|| {
                            self.module_by_file
                                .get(&draft.file)
                                .copied()
                                .map(ContainerRef::Entity)
                        })
                        .flatten()
                })
                .unwrap_or(ContainerRef::File(draft.file));
            let claim = self.symbol_claim(
                draft.kind,
                container,
                &draft.name,
                &draft.signature,
                draft.scope_identity,
            );
            let symbol = claim.symbol;
            let state = self
                .states
                .get(&draft.file)
                .ok_or_else(|| provider_failure("project-file-state"))?;
            let qualified_name = draft
                .parent_declaration
                .and_then(|parent| qualified_by_declaration.get(&(draft.file, parent)))
                .map_or_else(
                    || format!("{}::{}", state.path, draft.name),
                    |parent| format!("{parent}::{}", draft.name),
                );
            self.symbol_by_declaration
                .insert((draft.file, draft.local_id), symbol);
            qualified_by_declaration.insert((draft.file, draft.local_id), qualified_name.clone());
            // Resolution needs every declaration span even when repeated
            // declarations intentionally share one public stable entity.
            self.entities.push(SemanticEntity {
                symbol,
                name: draft.name.clone(),
                kind: draft.kind,
                visibility: draft.visibility,
                declaring_type: draft.declaring_type.clone(),
                arity: draft.arity,
                is_test: draft.is_test,
                file: draft.file,
                span: draft.span,
                source: draft.source.clone(),
            });
            if !materialized_symbols.insert(symbol) {
                continue;
            }
            let provenance = state.provenance;
            let mut flags = generated_flag(self.input_for_file(draft.file)?.is_generated());
            if draft.is_test {
                flags.push(EntityFlag::Test);
            }
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
                visibility: draft.visibility,
                flags,
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
                draft.source,
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
            let resolution = bound_resolution_candidates(
                self.resolve_occurrence(&draft, &definitions, &import_targets, false),
                self.request.limits().ir().max_nested_items_per_record,
            )?;
            let candidates = &resolution.symbols;
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
            let target = occurrence_target(&draft.name, &resolution);
            let provenance = self.provenance_for_file(draft.file)?;
            let enclosing = draft
                .enclosing_declaration
                .and_then(|declaration| {
                    self.symbol_by_declaration
                        .get(&(draft.file, declaration))
                        .copied()
                })
                .or(draft.enclosing);
            self.materialize_go_route(&draft, &definitions)?;
            let mut record = OccurrenceRecord {
                id: FactId::from_bytes([0; 20]),
                repository: draft.source.repository(),
                generation: draft.source.generation(),
                file: draft.file,
                source: draft.source.clone(),
                role: draft.role,
                enclosing,
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
                for symbol in candidates {
                    definition_occurrences.insert(*symbol, record.id);
                }
            }
            self.add_occurrence_relations(&record, candidates)?;
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

    fn materialize_go_route(
        &mut self,
        occurrence: &OccurrenceDraft,
        definitions: &BTreeMap<String, Vec<SemanticEntity>>,
    ) -> Result<(), AdapterError> {
        if self.analyzer.language != SemanticProjectLanguage::Go
            || occurrence.role != OccurrenceRole::CallSite
        {
            return Ok(());
        }
        let Some(call) = occurrence.call_text.as_deref() else {
            return Ok(());
        };
        let Some(receiver) = occurrence.qualifier.as_deref() else {
            return Ok(());
        };
        let Some(verb) = gin_http_verb(&occurrence.name) else {
            return Ok(());
        };
        let gin_aliases = self
            .imports
            .iter()
            .filter(|import| {
                import.file == occurrence.file && import.module == "github.com/gin-gonic/gin"
            })
            .flat_map(|import| &import.bindings)
            .filter_map(|binding| match binding {
                ImportBinding::Namespace { local } => Some(local.as_str()),
                ImportBinding::Named { .. }
                | ImportBinding::Wildcard
                | ImportBinding::SideEffect => None,
            })
            .collect::<BTreeSet<_>>();
        if gin_aliases.is_empty() {
            return Ok(());
        }
        let input = self.input_for_file(occurrence.file)?;
        if !reviewed_gin_receiver(
            input.source().bytes(),
            occurrence,
            receiver,
            &gin_aliases,
            &self.occurrences,
        ) {
            return Ok(());
        }

        let Some(arguments) = call_arguments(call) else {
            return self.push_go_route_diagnostic(occurrence);
        };
        let Some(path) = arguments
            .first()
            .and_then(|argument| literal_route_path(argument))
        else {
            return self.push_go_route_diagnostic(occurrence);
        };
        let handler_symbols = arguments
            .iter()
            .skip(1)
            .flat_map(|argument| tokenize_identifiers(argument))
            .filter_map(|name| definitions.get(&name))
            .flatten()
            .filter(|entity| {
                matches!(entity.kind, EntityKind::Function | EntityKind::Method)
                    && self
                        .package_by_file
                        .get(&entity.file)
                        .zip(self.package_by_file.get(&occurrence.file))
                        .is_some_and(|(candidate, current)| candidate == current)
            })
            .map(|entity| entity.symbol)
            .collect::<BTreeSet<_>>();
        if handler_symbols.len() != 1 {
            return self.push_go_route_diagnostic(occurrence);
        }
        let handler = handler_symbols
            .first()
            .copied()
            .ok_or_else(|| provider_failure("go-gin-handler"))?;
        let route_name = format!("{verb} {path}");
        let module = self
            .module_by_file
            .get(&occurrence.file)
            .copied()
            .ok_or_else(|| provider_failure("project-module"))?;
        let claim = self.symbol_claim(
            EntityKind::Route,
            ContainerRef::Entity(module),
            &route_name,
            &route_name,
            None,
        );
        let route = claim.symbol;
        if !self.entities.iter().any(|entity| entity.symbol == route) {
            let provenance = self.provenance_for_file(occurrence.file)?;
            self.records.push(IrRecord::Entity(EntityRecord {
                id: route,
                repository: occurrence.source.repository(),
                generation: occurrence.source.generation(),
                kind: EntityKind::Route,
                language: self.analyzer.language.as_str().to_owned(),
                tier: STRUCTURAL_TIER,
                canonical_name: route_name.clone(),
                display_name: route_name.clone(),
                qualified_name: format!(
                    "{}::{route_name}",
                    self.path_by_file
                        .get(&occurrence.file)
                        .map(String::as_str)
                        .unwrap_or_default()
                ),
                container: Some(ContainerRef::Entity(module)),
                visibility: EntityVisibility::Public,
                flags: vec![EntityFlag::Synthetic],
                provenance,
                evidence: direct_evidence(occurrence.source.clone()),
            }));
            self.records.push(IrRecord::Extension(
                new_symbol_identity_claim_envelope(
                    &claim,
                    occurrence.source.generation(),
                    provenance,
                    occurrence.source.clone(),
                )
                .map_err(|_| provider_failure("project-symbol-identity-claim"))?,
            ));
            self.entities.push(SemanticEntity {
                symbol: route,
                name: route_name,
                kind: EntityKind::Route,
                visibility: EntityVisibility::Public,
                declaring_type: None,
                arity: None,
                is_test: false,
                file: occurrence.file,
                span: occurrence.source.span(),
                source: occurrence.source.clone(),
            });
            self.state_mut_by_file(occurrence.file)?
                .increment(FactDomain::Entities)?;
            self.state_mut_by_file(occurrence.file)?
                .increment(FactDomain::Extensions)?;
        }
        self.push_relation(
            occurrence.file,
            RelationEndpoint::Entity(handler),
            RelationPredicate::ServesRoute,
            RelationEndpoint::Entity(route),
            STATIC_CALL_CONFIDENCE,
            EvidenceKind::Derived,
            occurrence.source.clone(),
        )
    }

    fn push_go_route_diagnostic(
        &mut self,
        occurrence: &OccurrenceDraft,
    ) -> Result<(), AdapterError> {
        let diagnostic = AdapterDiagnostic::new(
            DiagnosticCode::new(GO_GIN_ROUTE_INCOMPLETE_CODE)
                .map_err(|_| provider_failure("project-diagnostic-code"))?,
            DiagnosticSeverity::Info,
            Some(occurrence.source.clone()),
            CoverageStatus::Bounded,
        );
        let input = self.input_for_file(occurrence.file)?.clone();
        self.push_diagnostic(&input, &diagnostic)
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
                let mut provenance = ProvenanceRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: from.repository(),
                    generation: from.generation(),
                    producer_kind: ProducerKind::Derivation,
                    producer: self.analyzer.descriptor.identity().clone(),
                    binary_digest: self.analyzer.binary_digest,
                    frontend_version: Some(FRONTEND_VERSION.to_owned()),
                    language: self.analyzer.language.as_str().to_owned(),
                    tier: STRUCTURAL_TIER,
                    build_context: self.request.build_context(),
                    input_sources: vec![
                        input.source().source_ref().clone(),
                        origin_input.source().source_ref().clone(),
                    ],
                    evidence_sources: vec![from.clone(), to.clone()],
                    derivation_parents: vec![FactRef::Fact(self.provenance_for(input)?)],
                    rule: Some(mapping.provenance_rule()),
                };
                provenance.id = derive_provenance_record_id(&provenance)
                    .map_err(|_| provider_failure("project-mapping-provenance-identity"))?;
                let provenance_id = provenance.id;
                let mut record = SourceMappingRecord {
                    id: FactId::from_bytes([0; 20]),
                    repository: from.repository(),
                    generation: from.generation(),
                    from: from.clone(),
                    to,
                    kind: SourceMappingKind::GeneratedToOrigin,
                    provenance: provenance_id,
                    evidence: FactEvidence {
                        source: Some(from),
                        derivation: vec![FactRef::File(
                            origin_input.source().source_ref().span().file(),
                        )],
                    },
                };
                record.id = derive_source_mapping_record_id(&record)
                    .map_err(|_| provider_failure("project-mapping-identity"))?;
                self.records.push(IrRecord::Provenance(provenance));
                self.records.push(IrRecord::SourceMapping(record));
                self.state_mut(input)?.increment(FactDomain::Provenance)?;
                self.state_mut(input)?
                    .increment(FactDomain::SourceMappings)?;
            }
        }
        Ok(())
    }

    fn materialize_parser_diagnostics(&mut self) -> Result<(), AdapterError> {
        let limits = self.request.limits();
        let maximum_diagnostics = limits
            .ir()
            .max_diagnostics
            .min(limits.ir_stream().max_diagnostics());
        let diagnostic_count = self.parsed.iter().try_fold(0_usize, |total, parsed| {
            total
                .checked_add(parsed.diagnostics.len())
                .ok_or_else(|| provider_failure("project-diagnostic-accounting"))
        })?;
        // The sink accounts raw records before canonical deduplication, so keep
        // one raw slot for an explicit truncation summary whenever the cap binds.
        let reserve_summary =
            usize::from(maximum_diagnostics > 0 && diagnostic_count > maximum_diagnostics);
        let mut remaining_diagnostics = maximum_diagnostics.saturating_sub(reserve_summary);
        let mut omitted_diagnostics = 0_usize;
        let mut first_omitted_input = None;

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
                if remaining_diagnostics > 0 {
                    self.push_diagnostic(input, diagnostic)?;
                    remaining_diagnostics -= 1;
                } else {
                    self.record_diagnostic_coverage(input, diagnostic)?;
                    omitted_diagnostics = omitted_diagnostics
                        .checked_add(1)
                        .ok_or_else(|| provider_failure("project-diagnostic-accounting"))?;
                    first_omitted_input.get_or_insert(parsed_index);
                }
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
        if reserve_summary > 0 {
            let parsed_index = first_omitted_input
                .ok_or_else(|| provider_failure("project-diagnostic-summary"))?;
            let input = self.parsed[parsed_index].input;
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(PROJECT_DIAGNOSTICS_TRUNCATED_CODE)
                    .map_err(|_| provider_failure("project-diagnostic-code"))?,
                DiagnosticSeverity::Warning,
                Some(input.source().source_ref().clone()),
                CoverageStatus::Bounded,
            );
            self.push_diagnostic(input, &diagnostic)?;
        }
        debug_assert!(
            reserve_summary == 0 || omitted_diagnostics > 0,
            "a reserved summary requires at least one omitted parser diagnostic"
        );
        Ok(())
    }

    fn materialize_coverage(mut self) -> Result<ProjectFacts, AdapterError> {
        let mut overall_status = CoverageStatus::Complete;
        let mut covered_source_bytes = self.request.total_source_bytes();
        let mut skipped_regions = 0_usize;
        let mut totals = BTreeMap::<FactDomain, (CoverageStatus, usize, usize, usize)>::new();
        let mut incomplete_relations_by_file = BTreeMap::<FileId, usize>::new();
        for record in &self.records {
            let IrRecord::Occurrence(occurrence) = record else {
                continue;
            };
            let relationship_is_incomplete = occurrence.role == OccurrenceRole::CallSite
                && match &occurrence.target {
                    OccurrenceTarget::Resolved { .. } => false,
                    OccurrenceTarget::Candidates { completeness, .. } => {
                        *completeness != CoverageStatus::Complete
                    }
                    OccurrenceTarget::Unresolved { .. } => true,
                };
            if relationship_is_incomplete {
                let count = incomplete_relations_by_file
                    .entry(occurrence.file)
                    .or_default();
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("project-accounting"))?;
            }
        }

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
                let incomplete_relations = if domain == FactDomain::Relations {
                    incomplete_relations_by_file
                        .get(&state.file)
                        .copied()
                        .unwrap_or(0)
                } else {
                    0
                };
                let domain_status = if domain == FactDomain::SourceMappings {
                    generated_mapping_coverage(self.input_for_file(state.file)?)
                } else if incomplete_relations > 0 {
                    CoverageStatus::Bounded
                } else if state.status == CoverageStatus::Complete
                    || matches!(domain, FactDomain::Files | FactDomain::Provenance)
                {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                };
                overall_status = merge_status(overall_status, domain_status);
                let skipped = if domain == FactDomain::Relations {
                    incomplete_relations
                        .checked_add(usize::from(state.status != CoverageStatus::Complete))
                        .ok_or_else(|| provider_failure("project-accounting"))?
                } else {
                    usize::from(domain_status != CoverageStatus::Complete)
                };
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
        scope_identity: Option<[u8; 32]>,
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
        if let Some(scope_identity) = scope_identity {
            container_identity.push(3);
            container_identity.extend_from_slice(&scope_identity);
        }
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: self.request.inputs()[0].source().source_ref().repository(),
            language: self.analyzer.language.as_str().to_owned(),
            kind,
            container: Some(container),
            container_identity,
            declared_identity: name.to_owned(),
            signature_discriminator: signature.as_bytes().to_vec(),
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
    ) -> ResolutionCandidates {
        if occurrence.role == OccurrenceRole::Definition {
            return ResolutionCandidates {
                // Definition captures are already bound to their nearest
                // materialized declaration. Re-resolving by name would let an
                // enclosing same-name declaration contaminate the exact site.
                symbols: occurrence
                    .enclosing_declaration
                    .and_then(|declaration| {
                        self.symbol_by_declaration
                            .get(&(occurrence.file, declaration))
                    })
                    .copied()
                    .into_iter()
                    .collect(),
                kind: ResolutionKind::Binding,
            };
        }
        if let Some(resolution) = self.resolve_reviewed_static_call(occurrence, definitions) {
            return resolution;
        }
        if let Some(qualifier) = occurrence.qualifier.as_deref() {
            let mut namespace_symbols = BTreeSet::new();
            for import in self
                .imports
                .iter()
                .filter(|import| import.file == occurrence.file)
                .filter(|import| {
                    import.bindings.iter().any(|binding| {
                        matches!(
                            binding,
                            ImportBinding::Namespace { local } if local == qualifier
                        )
                    })
                })
            {
                let target_files = import_targets
                    .get(&(import.file, import.module.clone()))
                    .cloned()
                    .unwrap_or_default();
                if let Some(candidates) = definitions.get(&occurrence.name) {
                    namespace_symbols.extend(
                        candidates
                            .iter()
                            .filter(|entity| target_files.contains(&entity.file))
                            .map(|entity| entity.symbol),
                    );
                }
            }
            if !namespace_symbols.is_empty() {
                return ResolutionCandidates {
                    symbols: namespace_symbols.into_iter().collect(),
                    kind: ResolutionKind::Binding,
                };
            }

            if self.analyzer.language == SemanticProjectLanguage::Rust
                && occurrence.syntax_kind.ends_with(".scoped_call")
            {
                let qualifier_tail = qualifier.rsplit("::").next().unwrap_or(qualifier);
                let qualifier_is_type =
                    definitions
                        .get(qualifier_tail)
                        .into_iter()
                        .flatten()
                        .any(|entity| {
                            matches!(
                                entity.kind,
                                EntityKind::Class
                                    | EntityKind::Enum
                                    | EntityKind::Interface
                                    | EntityKind::Struct
                                    | EntityKind::Trait
                                    | EntityKind::TypeAlias
                            )
                        });
                if qualifier_is_type {
                    let symbols = definitions
                        .get(&occurrence.name)
                        .into_iter()
                        .flatten()
                        .filter(|entity| {
                            entity.kind == EntityKind::Method
                                && entity.declaring_type.as_deref() == Some(qualifier_tail)
                        })
                        .map(|entity| entity.symbol)
                        .collect::<BTreeSet<_>>();
                    return ResolutionCandidates {
                        symbols: symbols.into_iter().collect(),
                        kind: ResolutionKind::Binding,
                    };
                } else {
                    let module = qualifier
                        .trim_start_matches("crate::")
                        .trim_start_matches("self::")
                        .trim_start_matches("super::")
                        .replace("::", "/");
                    let current_path = self
                        .path_by_file
                        .get(&occurrence.file)
                        .map(String::as_str)
                        .unwrap_or_default();
                    let target_files = self
                        .path_by_file
                        .iter()
                        .filter_map(|(file, path)| {
                            module_matches(
                                SemanticProjectLanguage::Rust,
                                current_path,
                                &module,
                                path,
                            )
                            .then_some(*file)
                        })
                        .collect::<BTreeSet<_>>();
                    let symbols = definitions
                        .get(&occurrence.name)
                        .into_iter()
                        .flatten()
                        .filter(|entity| target_files.contains(&entity.file))
                        .map(|entity| entity.symbol)
                        .collect::<BTreeSet<_>>();
                    if !symbols.is_empty() {
                        return ResolutionCandidates {
                            symbols: symbols.into_iter().collect(),
                            kind: ResolutionKind::Binding,
                        };
                    }
                }
            }

            let dispatch_candidates = definitions
                .get(&occurrence.name)
                .into_iter()
                .flatten()
                .filter(|entity| entity.kind == EntityKind::Method)
                .map(|entity| entity.symbol)
                .collect::<BTreeSet<_>>();
            return ResolutionCandidates {
                symbols: dispatch_candidates.into_iter().collect(),
                kind: ResolutionKind::DynamicDispatch,
            };
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
            let lookup_names = import
                .bindings
                .iter()
                .filter_map(|binding| match binding {
                    ImportBinding::Named { local, imported } if local == &occurrence.name => {
                        Some(imported.as_str())
                    }
                    ImportBinding::Wildcard => Some(occurrence.name.as_str()),
                    ImportBinding::Named { .. }
                    | ImportBinding::Namespace { .. }
                    | ImportBinding::SideEffect => None,
                })
                .collect::<BTreeSet<_>>();
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
            && self.analyzer.language == SemanticProjectLanguage::Go
            && let (Some(package), Some(path)) = (
                self.package_by_file.get(&occurrence.file),
                self.path_by_file.get(&occurrence.file),
            )
            && let Some(candidates) = definitions.get(&occurrence.name)
        {
            let directory = project_directory(path);
            symbols.extend(
                candidates
                    .iter()
                    .filter(|entity| {
                        self.package_by_file.get(&entity.file) == Some(package)
                            && self
                                .path_by_file
                                .get(&entity.file)
                                .is_some_and(|candidate| project_directory(candidate) == directory)
                    })
                    .map(|entity| entity.symbol),
            );
        }
        if symbols.is_empty()
            && permit_global_fallback
            && let Some(candidates) = definitions.get(&occurrence.name)
        {
            symbols.extend(candidates.iter().map(|entity| entity.symbol));
        }
        ResolutionCandidates {
            symbols: symbols.into_iter().collect(),
            kind: ResolutionKind::Binding,
        }
    }

    fn resolve_reviewed_static_call(
        &self,
        occurrence: &OccurrenceDraft,
        definitions: &BTreeMap<String, Vec<SemanticEntity>>,
    ) -> Option<ResolutionCandidates> {
        if occurrence.role != OccurrenceRole::CallSite {
            return None;
        }
        if !matches!(
            self.analyzer.language,
            SemanticProjectLanguage::Java
                | SemanticProjectLanguage::Cpp
                | SemanticProjectLanguage::CSharp
                | SemanticProjectLanguage::Php
                | SemanticProjectLanguage::C
        ) {
            return None;
        }
        let candidates = definitions.get(&occurrence.name)?;
        let caller = occurrence
            .enclosing_declaration
            .and_then(|declaration| {
                self.symbol_by_declaration
                    .get(&(occurrence.file, declaration))
            })
            .and_then(|symbol| self.entities.iter().find(|entity| entity.symbol == *symbol));
        let caller_type = caller.and_then(|entity| entity.declaring_type.as_deref());
        let qualifier = occurrence.qualifier.as_deref();
        let receiver_type = qualifier.and_then(|qualifier| {
            self.input_for_file(occurrence.file).ok().and_then(|input| {
                local_receiver_type(
                    input.source().bytes(),
                    caller.map_or(0, |entity| entity.span.start_byte()),
                    occurrence.source.span().start_byte(),
                    qualifier,
                    definitions,
                )
            })
        });
        let symbols = candidates
            .iter()
            .filter(|entity| call_arity_matches(occurrence.arity, entity.arity))
            .filter(|entity| match self.analyzer.language {
                SemanticProjectLanguage::Java => {
                    entity.kind == EntityKind::Method
                        && match qualifier {
                            Some("this") => entity.declaring_type.as_deref() == caller_type,
                            Some(_) => entity.declaring_type.as_deref() == receiver_type.as_deref(),
                            None => entity.declaring_type.as_deref() == caller_type,
                        }
                }
                SemanticProjectLanguage::Cpp => {
                    entity.kind == EntityKind::Method
                        && qualifier.is_some()
                        && entity.declaring_type.as_deref() == receiver_type.as_deref()
                }
                SemanticProjectLanguage::CSharp => {
                    entity.kind == EntityKind::Method
                        && qualifier.is_none()
                        && entity.declaring_type.as_deref() == caller_type
                }
                SemanticProjectLanguage::Php => {
                    entity.kind == EntityKind::Method
                        && qualifier == Some("$this")
                        && entity.declaring_type.as_deref() == caller_type
                }
                SemanticProjectLanguage::C => {
                    qualifier.is_none()
                        && entity.kind == EntityKind::Function
                        && entity.visibility == EntityVisibility::Public
                }
                SemanticProjectLanguage::Rust
                | SemanticProjectLanguage::TypeScript
                | SemanticProjectLanguage::JavaScript
                | SemanticProjectLanguage::Python
                | SemanticProjectLanguage::Go => false,
            })
            .map(|entity| entity.symbol)
            .collect::<BTreeSet<_>>();
        Some(ResolutionCandidates {
            symbols: symbols.into_iter().collect(),
            kind: ResolutionKind::Binding,
        })
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
            OccurrenceRole::CallSite
                if matches!(occurrence.target, OccurrenceTarget::Resolved { .. }) =>
            {
                RelationPredicate::Calls
            }
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
        if let (OccurrenceRole::CallSite, OccurrenceTarget::Resolved { symbol: tested }, Some(test)) =
            (occurrence.role, &occurrence.target, occurrence.enclosing)
            && self
                .entities
                .iter()
                .any(|entity| entity.symbol == test && entity.is_test)
        {
            self.push_relation(
                occurrence.file,
                RelationEndpoint::Entity(test),
                RelationPredicate::Tests,
                RelationEndpoint::Entity(*tested),
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
        let claim = self.symbol_claim(EntityKind::ExternalSymbol, container, name, name, None);
        let symbol = claim.symbol;
        let provenance = self.provenance_for_file(file)?;
        let entity = SemanticEntity {
            symbol,
            name: name.to_owned(),
            kind: EntityKind::ExternalSymbol,
            visibility: EntityVisibility::Unknown,
            declaring_type: None,
            arity: None,
            is_test: false,
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
        let message = match diagnostic.code().as_str() {
            "invalid-utf8" => "source is not valid utf-8",
            PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC => {
                "project syntax facts exceeded the bounded semantic limit"
            }
            PROJECT_DIAGNOSTICS_TRUNCATED_CODE => PROJECT_DIAGNOSTICS_TRUNCATED_MESSAGE,
            GO_GIN_ROUTE_INCOMPLETE_CODE => GO_GIN_ROUTE_INCOMPLETE_MESSAGE,
            _ => "parser recovered from malformed or incomplete syntax",
        };
        let mut record = DiagnosticRecord {
            id: FactId::from_bytes([0; 20]),
            repository: input.source().source_ref().repository(),
            generation: input.source().source_ref().generation(),
            code: diagnostic.code().as_str().to_owned(),
            message: message.to_owned(),
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
        self.record_diagnostic_coverage(input, diagnostic)?;
        let state = self.state_mut(input)?;
        state.increment(FactDomain::Diagnostics)
    }

    fn record_diagnostic_coverage(
        &mut self,
        input: &ProjectSourceInput<'_>,
        diagnostic: &AdapterDiagnostic,
    ) -> Result<(), AdapterError> {
        let state = self.state_mut(input)?;
        state.status = merge_status(state.status, diagnostic.coverage_effect());
        Ok(())
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

fn declaration_visibility(
    language: SemanticProjectLanguage,
    source: &[u8],
    span: SourceSpan,
    header: &str,
    name: &str,
) -> EntityVisibility {
    if matches!(
        language,
        SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript
    ) && let Ok(start) = usize::try_from(span.start_byte())
        && let Some(prefix) = source.get(..start)
    {
        let line_start = prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index.saturating_add(1));
        if prefix
            .get(line_start..)
            .and_then(|value| std::str::from_utf8(value).ok())
            .is_some_and(|value| value.trim_start().starts_with("export "))
        {
            return EntityVisibility::Public;
        }
    }
    infer_visibility(language, header, name)
}

fn infer_visibility(
    language: SemanticProjectLanguage,
    header: &str,
    name: &str,
) -> EntityVisibility {
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
            if name.starts_with('_') {
                EntityVisibility::Private
            } else {
                EntityVisibility::Unknown
            }
        }
        SemanticProjectLanguage::Go => {
            if name.starts_with(|character: char| character.is_ascii_uppercase()) {
                EntityVisibility::Public
            } else {
                EntityVisibility::Private
            }
        }
        SemanticProjectLanguage::Java | SemanticProjectLanguage::CSharp => {
            if header.contains("public ") {
                EntityVisibility::Public
            } else if header.contains("private ") {
                EntityVisibility::Private
            } else {
                EntityVisibility::Restricted
            }
        }
        SemanticProjectLanguage::Cpp | SemanticProjectLanguage::C => {
            if header.trim_start().starts_with("static ") {
                EntityVisibility::Private
            } else {
                EntityVisibility::Public
            }
        }
        SemanticProjectLanguage::Php => {
            if header.contains("private ") {
                EntityVisibility::Private
            } else if header.contains("protected ") {
                EntityVisibility::Restricted
            } else {
                EntityVisibility::Public
            }
        }
    }
}

fn occurrence_target(name: &str, resolution: &BoundedResolutionCandidates) -> OccurrenceTarget {
    if resolution.total_count == 0 {
        return OccurrenceTarget::Unresolved {
            text_hash: content_hash(name.as_bytes()),
        };
    }
    if let [symbol] = resolution.symbols.as_slice()
        && resolution.total_count == 1
        && resolution.kind == ResolutionKind::Binding
        && resolution.completeness == CoverageStatus::Complete
    {
        return OccurrenceTarget::Resolved { symbol: *symbol };
    }
    OccurrenceTarget::Candidates {
        symbols: resolution.symbols.clone(),
        total_count: resolution.total_count,
        completeness: resolution.completeness,
    }
}

fn bound_resolution_candidates(
    mut resolution: ResolutionCandidates,
    maximum_symbols: usize,
) -> Result<BoundedResolutionCandidates, AdapterError> {
    resolution.symbols.sort_unstable();
    resolution.symbols.dedup();
    let total_count = u64::try_from(resolution.symbols.len())
        .map_err(|_| provider_failure("project-candidate-count"))?;
    let truncated = resolution.symbols.len() > maximum_symbols;
    resolution.symbols.truncate(maximum_symbols);
    Ok(BoundedResolutionCandidates {
        symbols: resolution.symbols,
        total_count,
        completeness: match resolution.kind {
            ResolutionKind::Binding if truncated => CoverageStatus::Bounded,
            ResolutionKind::Binding => CoverageStatus::Complete,
            ResolutionKind::DynamicDispatch => CoverageStatus::Unknown,
        },
        kind: resolution.kind,
    })
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

fn positive_test_declarations(
    language: SemanticProjectLanguage,
    facts: &[SyntaxFact],
) -> BTreeSet<u64> {
    match language {
        SemanticProjectLanguage::Rust => adjacent_test_declarations(
            facts,
            "rust.test_attribute.test_attribute",
            "rust.function.declaration",
        ),
        SemanticProjectLanguage::Java => enclosing_test_declarations(
            facts,
            "java.test_attribute.test_attribute",
            "java.method.declaration",
        ),
        SemanticProjectLanguage::Cpp => enclosing_test_declarations(
            facts,
            "cpp.test_attribute.test_attribute",
            "cpp.function.declaration",
        ),
        SemanticProjectLanguage::TypeScript
        | SemanticProjectLanguage::JavaScript
        | SemanticProjectLanguage::Python
        | SemanticProjectLanguage::Go
        | SemanticProjectLanguage::CSharp
        | SemanticProjectLanguage::Php
        | SemanticProjectLanguage::C => BTreeSet::new(),
    }
}

fn adjacent_test_declarations(
    facts: &[SyntaxFact],
    attribute_label: &str,
    declaration_label: &str,
) -> BTreeSet<u64> {
    let mut source_order = facts.iter().collect::<Vec<_>>();
    source_order.sort_unstable_by_key(|fact| {
        (
            fact.span().start_byte(),
            fact.span().end_byte(),
            fact.depth(),
            fact.local_id(),
        )
    });
    let mut pending_parents = BTreeSet::new();
    let mut tests = BTreeSet::new();
    for fact in source_order {
        if fact.syntax_kind().as_str() == attribute_label {
            pending_parents.insert(fact.parent());
            continue;
        }
        if fact.kind() == SyntaxFactKind::Declaration
            && pending_parents.remove(&fact.parent())
            && fact.syntax_kind().as_str() == declaration_label
        {
            tests.insert(fact.local_id());
        }
    }
    tests
}

fn enclosing_test_declarations(
    facts: &[SyntaxFact],
    attribute_label: &str,
    declaration_label: &str,
) -> BTreeSet<u64> {
    let facts_by_id = facts
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    facts
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == attribute_label)
        .filter_map(|attribute| {
            let mut parent = attribute.parent();
            let mut remaining = facts.len();
            while let Some(parent_id) = parent {
                if remaining == 0 {
                    return None;
                }
                remaining -= 1;
                let candidate = facts_by_id.get(&parent_id).copied()?;
                if candidate.kind() == SyntaxFactKind::Declaration
                    && candidate.syntax_kind().as_str() == declaration_label
                {
                    return Some(candidate.local_id());
                }
                parent = candidate.parent();
            }
            None
        })
        .collect()
}

fn declaration_is_test(language: SemanticProjectLanguage, path: &str, name: &str) -> bool {
    if matches!(
        language,
        SemanticProjectLanguage::Java | SemanticProjectLanguage::Cpp
    ) {
        return false;
    }
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());
    let test_path = path
        .split('/')
        .any(|component| matches!(component, "test" | "tests" | "__tests__"))
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.py")
        || file_name.ends_with("_test.go")
        || file_name.contains(".test.")
        || file_name.contains(".spec.");
    test_path
        || match language {
            SemanticProjectLanguage::Python => {
                name.starts_with("test_") || name.starts_with("Test")
            }
            SemanticProjectLanguage::Rust
            | SemanticProjectLanguage::TypeScript
            | SemanticProjectLanguage::JavaScript
            | SemanticProjectLanguage::Go
            | SemanticProjectLanguage::Java
            | SemanticProjectLanguage::Cpp
            | SemanticProjectLanguage::CSharp
            | SemanticProjectLanguage::Php
            | SemanticProjectLanguage::C => false,
        }
}

fn project_directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn rust_impl_self_type_name(self_type: &str) -> Option<&str> {
    let self_type = self_type.trim();
    if self_type.starts_with('<') {
        return None;
    }
    let ungeneric = self_type
        .split_once('<')
        .map_or(self_type, |(prefix, _)| prefix)
        .trim();
    let name = ungeneric.rsplit("::").next()?.trim();
    is_identifier(name).then_some(name)
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

fn enclosing_callable_declaration(
    fact: &SyntaxFact,
    facts_by_id: &BTreeMap<u64, &SyntaxFact>,
    declaration_kinds: &BTreeMap<u64, EntityKind>,
) -> Option<u64> {
    let mut parent = fact.parent();
    let mut remaining = facts_by_id.len();
    while let Some(parent_id) = parent {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        if declaration_kinds
            .get(&parent_id)
            .is_some_and(|kind| is_callable_entity_kind(*kind))
        {
            return Some(parent_id);
        }
        parent = facts_by_id.get(&parent_id).and_then(|fact| fact.parent());
    }
    None
}

fn enclosing_rust_impl_scope_before_declaration<'fact>(
    fact: &SyntaxFact,
    facts_by_id: &BTreeMap<u64, &'fact SyntaxFact>,
) -> Option<&'fact SyntaxFact> {
    let mut parent = fact.parent();
    let mut remaining = facts_by_id.len();
    while let Some(parent_id) = parent {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        let candidate = facts_by_id.get(&parent_id).copied()?;
        if candidate.kind() == SyntaxFactKind::Declaration {
            return None;
        }
        if candidate.kind() == SyntaxFactKind::Scope
            && candidate.syntax_kind().as_str() == "rust.impl.scope"
        {
            return Some(candidate);
        }
        parent = candidate.parent();
    }
    None
}

fn is_call_fact(fact: &SyntaxFact) -> bool {
    let label = fact.syntax_kind().as_str();
    label.ends_with(".call") || label.ends_with(".scoped_call")
}

fn is_call_name_fact(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str().ends_with(".call_name")
}

fn terminal_call_names(facts: &[SyntaxFact]) -> BTreeMap<u64, u64> {
    let mut captures = facts
        .iter()
        .filter(|fact| is_call_fact(fact) || is_call_name_fact(fact))
        .collect::<Vec<_>>();
    captures.sort_unstable_by(|left, right| {
        left.span()
            .start_byte()
            .cmp(&right.span().start_byte())
            .then_with(|| right.span().end_byte().cmp(&left.span().end_byte()))
            .then_with(|| is_call_name_fact(left).cmp(&is_call_name_fact(right)))
            .then_with(|| left.local_id().cmp(&right.local_id()))
    });

    let mut active_calls = Vec::<&SyntaxFact>::new();
    let mut names_by_call = BTreeMap::<u64, Option<u64>>::new();
    for capture in captures {
        while active_calls
            .last()
            .is_some_and(|call| !contains_span(call.span(), capture.span()))
        {
            active_calls.pop();
        }
        if is_call_name_fact(capture) {
            if let Some(call) = active_calls.last() {
                names_by_call
                    .entry(call.local_id())
                    .and_modify(|name| *name = None)
                    .or_insert(Some(capture.local_id()));
            }
        } else {
            active_calls.push(capture);
        }
    }
    names_by_call
        .into_iter()
        .filter_map(|(call, name)| name.map(|name| (call, name)))
        .collect()
}

fn is_definition_fact(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str().ends_with(".definition")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedOccurrenceName<'a> {
    name: &'a str,
    qualifier: Option<&'a str>,
}

fn occurrence_name(value: &str, call: bool) -> Option<ParsedOccurrenceName<'_>> {
    if is_identifier(value) {
        return Some(ParsedOccurrenceName {
            name: value,
            qualifier: None,
        });
    }
    if !call {
        return None;
    }
    [".", "::"].into_iter().find_map(|separator| {
        let (qualifier, name) = value.rsplit_once(separator)?;
        if !is_identifier(name)
            || qualifier.is_empty()
            || !qualifier.split(separator).all(is_identifier)
        {
            return None;
        }
        Some(ParsedOccurrenceName {
            name,
            qualifier: Some(qualifier),
        })
    })
}

fn call_receiver<'call>(call: &'call str, terminal_name: &str) -> Option<&'call str> {
    let open = call.find('(')?;
    let prefix = call.get(..open)?.trim_end();
    let terminal_start = prefix.rfind(terminal_name)?;
    let before_terminal = prefix.get(..terminal_start)?.trim_end();
    let before_terminal = before_terminal
        .strip_suffix("template")
        .unwrap_or(before_terminal)
        .trim_end();
    let receiver = [".", "::", "->"]
        .into_iter()
        .find_map(|separator| before_terminal.strip_suffix(separator))
        .map(str::trim)?;
    let receiver = receiver
        .rsplit_once(['.', ':', '>', ' '])
        .map_or(receiver, |(_, tail)| tail)
        .trim();
    (is_identifier(receiver) || receiver == "$this").then_some(receiver)
}

const fn gin_http_verb(name: &str) -> Option<&'static str> {
    match name.as_bytes() {
        b"GET" => Some("GET"),
        b"POST" => Some("POST"),
        b"PUT" => Some("PUT"),
        b"PATCH" => Some("PATCH"),
        b"DELETE" => Some("DELETE"),
        b"HEAD" => Some("HEAD"),
        b"OPTIONS" => Some("OPTIONS"),
        _ => None,
    }
}

fn reviewed_gin_receiver(
    bytes: &[u8],
    route: &OccurrenceDraft,
    receiver: &str,
    aliases: &BTreeSet<&str>,
    occurrences: &[OccurrenceDraft],
) -> bool {
    occurrences.iter().any(|initializer| {
        if initializer.file != route.file
            || initializer.role != OccurrenceRole::CallSite
            || initializer.source.span().start_byte() >= route.source.span().start_byte()
            || initializer.enclosing_declaration != route.enclosing_declaration
            || !matches!(initializer.name.as_str(), "Default" | "New")
            || !initializer
                .qualifier
                .as_deref()
                .is_some_and(|qualifier| aliases.contains(qualifier))
        {
            return false;
        }
        let Ok(start) = usize::try_from(initializer.source.span().start_byte()) else {
            return false;
        };
        let Ok(end) = usize::try_from(initializer.source.span().end_byte()) else {
            return false;
        };
        let line_start = bytes
            .get(..start)
            .and_then(|prefix| prefix.iter().rposition(|byte| *byte == b'\n'))
            .map_or(0, |newline| newline + 1);
        let line_end = bytes
            .get(end..)
            .and_then(|suffix| suffix.iter().position(|byte| *byte == b'\n'))
            .and_then(|newline| end.checked_add(newline))
            .unwrap_or(bytes.len());
        let Ok(line) = std::str::from_utf8(bytes.get(line_start..line_end).unwrap_or_default())
        else {
            return false;
        };
        let compact = line
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let Some(call) = initializer.call_text.as_deref() else {
            return false;
        };
        let expected = format!("{receiver}:={call}");
        compact == expected || compact == format!("{expected};")
    })
}

fn call_arguments(call: &str) -> Option<Vec<&str>> {
    let open = call.find('(')?;
    let close = call.rfind(')')?;
    if open >= close {
        return None;
    }
    let body = call.get(open + 1..close)?;
    let mut arguments = Vec::new();
    let mut start = 0;
    let mut depth = 0_u8;
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in body.char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' && active_quote != '`' {
                escaped = true;
            } else if character == active_quote {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth = depth.checked_add(1)?,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                let argument = body.get(start..offset)?.trim();
                if argument.is_empty() {
                    return None;
                }
                arguments.push(argument);
                if arguments.len() > 64 {
                    return None;
                }
                start = offset + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return None;
    }
    let argument = body.get(start..)?.trim();
    if !argument.is_empty() {
        arguments.push(argument);
    }
    (!arguments.is_empty() && arguments.len() <= 64).then_some(arguments)
}

fn literal_route_path(value: &str) -> Option<&str> {
    let value = value.trim();
    let quote = value.chars().next()?;
    if !matches!(quote, '"' | '`') || value.chars().last()? != quote {
        return None;
    }
    let inner = value.get(quote.len_utf8()..value.len().checked_sub(quote.len_utf8())?)?;
    (!inner.is_empty() && inner.starts_with('/') && !inner.contains(['\\', '\r', '\n', '"', '`']))
        .then_some(inner)
}

fn call_arity_matches(call: Option<usize>, declaration: Option<usize>) -> bool {
    matches!((call, declaration), (Some(call), Some(declaration)) if call == declaration)
}

fn local_receiver_type(
    bytes: &[u8],
    scope_start: u64,
    call_start: u64,
    receiver: &str,
    definitions: &BTreeMap<String, Vec<SemanticEntity>>,
) -> Option<String> {
    if !is_identifier(receiver) || receiver == "$this" {
        return None;
    }
    let scope_start = usize::try_from(scope_start).ok()?;
    let call_start = usize::try_from(call_start).ok()?;
    let prefix = std::str::from_utf8(bytes.get(scope_start..call_start)?).ok()?;
    for statement in prefix.rsplit(';') {
        // Balanced initializer braces do not end a lexical scope. An
        // unmatched closing brace does, so do not bind through it.
        if has_unmatched_closing_brace(statement) {
            return None;
        }
        let (declaration, initializer) = statement.split_once('=').unwrap_or((statement, ""));
        let tokens = tokenize_identifiers(declaration);
        let Some(pair) = tokens
            .windows(2)
            .rev()
            .find(|pair| pair[1] == receiver && is_identifier(&pair[0]))
        else {
            continue;
        };
        if pair[0] != "auto" && is_declared_type(definitions, &pair[0]) {
            return Some(pair[0].clone());
        }
        if pair[0] == "auto" {
            let constructor = initializer.split_once('(')?.0;
            let candidate = tokenize_identifiers(constructor).into_iter().last()?;
            if is_declared_type(definitions, &candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn has_unmatched_closing_brace(value: &str) -> bool {
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active_quote {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            '{' => depth = depth.saturating_add(1),
            '}' if depth == 0 => return true,
            '}' => depth -= 1,
            _ => {}
        }
    }
    false
}

fn is_declared_type(definitions: &BTreeMap<String, Vec<SemanticEntity>>, name: &str) -> bool {
    definitions.get(name).is_some_and(|entities| {
        entities.iter().any(|entity| {
            matches!(
                entity.kind,
                EntityKind::Class
                    | EntityKind::Enum
                    | EntityKind::Interface
                    | EntityKind::Struct
                    | EntityKind::Trait
                    | EntityKind::TypeAlias
            )
        })
    })
}

fn parameter_arity(signature: &str) -> Option<usize> {
    delimited_arity(signature, '(', ')')
}

fn call_arity(call: &str) -> Option<usize> {
    delimited_arity(call, '(', ')')
}

fn delimited_arity(value: &str, open: char, close: char) -> Option<usize> {
    let start = value.find(open)?;
    let end = value.rfind(close)?;
    let body = value.get(start + open.len_utf8()..end)?.trim();
    if body.is_empty() {
        return Some(0);
    }
    let mut depth = 0_u32;
    let mut arity = 1_usize;
    for character in body.chars() {
        match character {
            '(' | '[' | '{' | '<' => depth = depth.checked_add(1)?,
            ')' | ']' | '}' | '>' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => arity = arity.checked_add(1)?,
            _ => {}
        }
    }
    (depth == 0).then_some(arity)
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

fn declaration_header(language: SemanticProjectLanguage, declaration: &str) -> String {
    let line_end = declaration.find('\n').unwrap_or(declaration.len());
    match language {
        SemanticProjectLanguage::Python => declaration
            .get(..line_end)
            .unwrap_or(declaration)
            .trim()
            .to_owned(),
        SemanticProjectLanguage::Go
            if declaration
                .get(..line_end)
                .unwrap_or(declaration)
                .contains(" interface") =>
        {
            declaration
                .get(..line_end)
                .unwrap_or(declaration)
                .trim()
                .to_owned()
        }
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

fn select_unique_syntax_fact<'a>(facts: &[&'a SyntaxFact]) -> Option<&'a SyntaxFact> {
    let selected = facts.first().copied()?;
    facts
        .iter()
        .copied()
        .all(|candidate| {
            candidate.span() == selected.span()
                && candidate.syntax_kind().as_str() == selected.syntax_kind().as_str()
        })
        .then_some(selected)
}

fn is_symbol_signature_fact(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str().ends_with(".signature")
}

const fn supports_symbol_signature(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Function
            | EntityKind::Method
            | EntityKind::Constructor
            | EntityKind::Class
            | EntityKind::Struct
            | EntityKind::Enum
            | EntityKind::Trait
            | EntityKind::Interface
    )
}

const fn is_callable_entity_kind(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Function | EntityKind::Method | EntityKind::Constructor | EntityKind::Closure
    )
}

const fn uses_explicit_file_module(language: SemanticProjectLanguage) -> bool {
    matches!(
        language,
        SemanticProjectLanguage::TypeScript
            | SemanticProjectLanguage::JavaScript
            | SemanticProjectLanguage::Python
    )
}

fn parse_import(
    language: SemanticProjectLanguage,
    text: &str,
) -> Vec<(String, Vec<ImportBinding>)> {
    match language {
        SemanticProjectLanguage::Rust => parse_rust_imports(text),
        SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript => {
            parse_ecmascript_import(text).into_iter().collect()
        }
        SemanticProjectLanguage::Python => parse_python_imports(text),
        SemanticProjectLanguage::Go => parse_go_imports(text),
        SemanticProjectLanguage::Java
        | SemanticProjectLanguage::Cpp
        | SemanticProjectLanguage::CSharp
        | SemanticProjectLanguage::Php
        | SemanticProjectLanguage::C => Vec::new(),
    }
}

fn parse_rust_imports(text: &str) -> Vec<(String, Vec<ImportBinding>)> {
    let Some(body) = text.trim().strip_prefix("use ") else {
        return Vec::new();
    };
    let normalized = trim_rust_path_root(body.trim_end_matches(';').trim());
    let mut imports = Vec::new();
    parse_rust_use_tree("", normalized, &mut imports);
    imports
}

fn parse_rust_use_tree(prefix: &str, tree: &str, imports: &mut Vec<(String, Vec<ImportBinding>)>) {
    let tree = tree.trim();
    if let Some((group_prefix, items)) = rust_use_group(tree) {
        let prefix = join_rust_path(prefix, group_prefix);
        for item in split_top_level_commas(items) {
            parse_rust_use_tree(&prefix, item, imports);
        }
        return;
    }

    let (path, alias) = tree
        .split_once(" as ")
        .map_or((tree, None), |(path, alias)| {
            (path.trim(), Some(alias.trim()))
        });
    let full_path = join_rust_path(prefix, path);
    let mut components = full_path
        .split("::")
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let Some(imported) = components.pop() else {
        return;
    };

    if imported == "*" {
        push_import_binding(imports, &components.join("/"), ImportBinding::Wildcard);
        return;
    }

    if imported == "self" {
        let module = components.join("/");
        let local = alias.unwrap_or_else(|| components.last().copied().unwrap_or_default());
        push_namespace_or_side_effect(imports, &module, local);
        return;
    }

    if components.is_empty() {
        let local = alias.unwrap_or(imported);
        push_namespace_or_side_effect(imports, imported, local);
        return;
    }

    let binding = match alias.unwrap_or(imported) {
        "_" => ImportBinding::SideEffect,
        local if is_identifier(local) && is_identifier(imported) => ImportBinding::Named {
            local: local.to_owned(),
            imported: imported.to_owned(),
        },
        _ => return,
    };
    push_import_binding(imports, &components.join("/"), binding);
}

fn trim_rust_path_root(mut path: &str) -> &str {
    while let Some(remainder) = ["crate::", "self::", "super::"]
        .into_iter()
        .find_map(|root| path.strip_prefix(root))
    {
        path = remainder;
    }
    path
}

fn rust_use_group(tree: &str) -> Option<(&str, &str)> {
    let tree = tree.strip_suffix('}')?;
    if let Some(items) = tree.strip_prefix('{') {
        return Some(("", items));
    }
    tree.split_once("::{")
}

fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut depth = 0_u32;
    let mut start = 0;
    let mut parts = Vec::new();
    for (offset, character) in value.char_indices() {
        match character {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if let Some(part) = value.get(start..offset).map(str::trim)
                    && !part.is_empty()
                {
                    parts.push(part);
                }
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    if let Some(part) = value.get(start..).map(str::trim)
        && !part.is_empty()
    {
        parts.push(part);
    }
    parts
}

fn join_rust_path(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.trim().to_owned()
    } else if suffix.trim().is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}::{}", suffix.trim())
    }
}

fn push_namespace_or_side_effect(
    imports: &mut Vec<(String, Vec<ImportBinding>)>,
    module: &str,
    local: &str,
) {
    let binding = match local {
        "_" => ImportBinding::SideEffect,
        local if is_identifier(local) => ImportBinding::Namespace {
            local: local.to_owned(),
        },
        _ => return,
    };
    push_import_binding(imports, module, binding);
}

fn push_import_binding(
    imports: &mut Vec<(String, Vec<ImportBinding>)>,
    module: &str,
    binding: ImportBinding,
) {
    if module.is_empty() {
        return;
    }
    if let Some((_, bindings)) = imports
        .iter_mut()
        .find(|(existing_module, _)| existing_module == module)
    {
        bindings.push(binding);
    } else {
        imports.push((module.to_owned(), vec![binding]));
    }
}

fn parse_ecmascript_import(text: &str) -> Option<(String, Vec<ImportBinding>)> {
    let module = first_quoted(text)?;
    let mut bindings = Vec::new();
    if let Some((head, _)) = text.rsplit_once(" from ") {
        let body = head.trim().strip_prefix("import ")?.trim();
        let remainder = if !body.starts_with('{')
            && !body.starts_with('*')
            && let Some((default, remainder)) = body.split_once(',')
        {
            let default = default.trim();
            if is_identifier(default) {
                bindings.push(ImportBinding::Named {
                    local: default.to_owned(),
                    imported: "default".to_owned(),
                });
            }
            remainder.trim()
        } else {
            body
        };
        if let Some(named) = remainder.strip_prefix('{') {
            for item in named.trim_end_matches('}').split(',') {
                push_alias_binding(&mut bindings, item.trim(), " as ");
            }
        } else if let Some(alias) = remainder.strip_prefix("* as ") {
            if is_identifier(alias.trim()) {
                bindings.push(ImportBinding::Namespace {
                    local: alias.trim().to_owned(),
                });
            }
        } else if !body.contains(',') && is_identifier(remainder) {
            bindings.push(ImportBinding::Named {
                local: remainder.to_owned(),
                imported: "default".to_owned(),
            });
        }
    } else {
        bindings.push(ImportBinding::SideEffect);
    }
    Some((module, bindings))
}

fn parse_python_imports(text: &str) -> Vec<(String, Vec<ImportBinding>)> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("from ") {
        let Some((module, items)) = rest.split_once(" import ") else {
            return Vec::new();
        };
        let bindings = items
            .trim_matches(['(', ')'])
            .split(',')
            .filter_map(|item| {
                let item = item.trim();
                (item == "*")
                    .then_some(ImportBinding::Wildcard)
                    .or_else(|| alias_binding(item, " as "))
            })
            .collect();
        vec![(module.trim().to_owned(), bindings)]
    } else {
        let Some(rest) = trimmed.strip_prefix("import ") else {
            return Vec::new();
        };
        rest.split(',')
            .filter_map(|item| {
                let item = item.trim();
                let (module, local) = item.split_once(" as ").map_or_else(
                    || (item, item.split('.').next().unwrap_or(item)),
                    |(module, local)| (module.trim(), local.trim()),
                );
                is_identifier(local).then(|| {
                    (
                        module.to_owned(),
                        vec![ImportBinding::Namespace {
                            local: local.to_owned(),
                        }],
                    )
                })
            })
            .collect()
    }
}

fn parse_go_imports(text: &str) -> Vec<(String, Vec<ImportBinding>)> {
    let Some(body) = text.trim().strip_prefix("import") else {
        return Vec::new();
    };
    if body.trim_start().starts_with('(') {
        body.trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .lines()
            .filter_map(parse_go_import_spec)
            .collect()
    } else {
        parse_go_import_spec(body).into_iter().collect()
    }
}

fn parse_go_import_spec(text: &str) -> Option<(String, Vec<ImportBinding>)> {
    let text = text.trim().trim_end_matches(';').trim();
    let module = first_quoted(text)?;
    let before_quote = text
        .split(['"', '`'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim();
    let local = if before_quote.is_empty() {
        module.rsplit('/').next().unwrap_or(&module).to_owned()
    } else {
        before_quote
            .split_whitespace()
            .last()
            .unwrap_or_default()
            .to_owned()
    };
    let binding = match local.as_str() {
        "_" => ImportBinding::SideEffect,
        "." => ImportBinding::Wildcard,
        _ => ImportBinding::Namespace { local },
    };
    Some((module, vec![binding]))
}

fn push_alias_binding(bindings: &mut Vec<ImportBinding>, value: &str, separator: &str) {
    if value == "*" {
        bindings.push(ImportBinding::Wildcard);
    } else if let Some(binding) = alias_binding(value, separator) {
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
    is_identifier(local).then(|| ImportBinding::Named {
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
        SemanticProjectLanguage::Java
        | SemanticProjectLanguage::Cpp
        | SemanticProjectLanguage::CSharp
        | SemanticProjectLanguage::Php
        | SemanticProjectLanguage::C => false,
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
        SemanticProjectLanguage::Java
        | SemanticProjectLanguage::Cpp
        | SemanticProjectLanguage::CSharp
        | SemanticProjectLanguage::Php => {
            keyword_targets(header, "extends", RelationPredicate::Extends)
                .into_iter()
                .chain(keyword_targets(
                    header,
                    "implements",
                    RelationPredicate::Implements,
                ))
                .collect()
        }
        SemanticProjectLanguage::Rust | SemanticProjectLanguage::C => Vec::new(),
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

fn generated_mapping_coverage(input: &ProjectSourceInput<'_>) -> CoverageStatus {
    if !input.is_generated() {
        return CoverageStatus::Complete;
    }
    let full = input.source().source_ref().span();
    let mut next_byte = full.start_byte();
    for mapping in input.origins() {
        let generated = mapping.generated();
        if generated.file() != full.file() || generated.start_byte() != next_byte {
            return CoverageStatus::Unknown;
        }
        next_byte = generated.end_byte();
    }
    if next_byte == full.end_byte() {
        CoverageStatus::Complete
    } else {
        CoverageStatus::Unknown
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, SyntaxKindLabel};
    use rootlight_ids::{FileId, SymbolId};
    use rootlight_ir::{
        CoverageStatus, EntityKind, EntityVisibility, OccurrenceTarget, SourceSpan,
    };

    use super::{
        ImportBinding, MAX_OPTIONAL_PROJECT_SYNTAX_FACTS, ParsedOccurrenceName,
        ResolutionCandidates, ResolutionKind, SemanticProjectLanguage, bound_resolution_candidates,
        enclosing_callable_declaration, infer_visibility, occurrence_name, occurrence_target,
        parse_import, project_optional_syntax_fact_limit,
    };

    #[test]
    fn project_optional_syntax_fact_limit_does_not_scale_with_partition_width() {
        for input_count in [1, 512, usize::MAX] {
            assert_eq!(
                project_optional_syntax_fact_limit(input_count),
                MAX_OPTIONAL_PROJECT_SYNTAX_FACTS
            );
        }
    }

    #[test]
    fn callable_ancestry_skips_materialized_local_declarations() {
        let file = FileId::from_bytes([1; 20]);
        let function = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(file, 0, 64).expect("function span is valid"),
            0,
            SyntaxKindLabel::new("javascript.function.declaration").expect("label is valid"),
        );
        let local = SyntaxFact::new(
            2,
            Some(function.local_id()),
            SyntaxFactKind::Declaration,
            SourceSpan::new(file, 16, 48).expect("local span is valid"),
            1,
            SyntaxKindLabel::new("javascript.variable.declaration").expect("label is valid"),
        );
        let call = SyntaxFact::new(
            3,
            Some(local.local_id()),
            SyntaxFactKind::Occurrence,
            SourceSpan::new(file, 24, 32).expect("call span is valid"),
            2,
            SyntaxKindLabel::new("javascript.call").expect("label is valid"),
        );
        let facts_by_id = BTreeMap::from([
            (function.local_id(), &function),
            (local.local_id(), &local),
            (call.local_id(), &call),
        ]);
        let declaration_kinds = BTreeMap::from([
            (function.local_id(), EntityKind::Function),
            (local.local_id(), EntityKind::Variable),
        ]);

        assert_eq!(
            enclosing_callable_declaration(&call, &facts_by_id, &declaration_kinds),
            Some(function.local_id())
        );
    }

    #[test]
    fn qualified_call_names_retain_the_receiver_and_terminal_identifier() {
        assert_eq!(
            occurrence_name("package.Run", true),
            Some(ParsedOccurrenceName {
                name: "Run",
                qualifier: Some("package"),
            })
        );
        assert_eq!(
            occurrence_name("crate::module::run", true),
            Some(ParsedOccurrenceName {
                name: "run",
                qualifier: Some("crate::module"),
            })
        );
        assert_eq!(
            occurrence_name("plain", true),
            Some(ParsedOccurrenceName {
                name: "plain",
                qualifier: None,
            })
        );
        assert_eq!(occurrence_name("package.Run", false), None);
        assert_eq!(occurrence_name("package..Run", true), None);
        assert_eq!(occurrence_name("package.Run()", true), None);
    }

    #[test]
    fn language_imports_preserve_explicit_binding_kinds() {
        assert_eq!(
            parse_import(SemanticProjectLanguage::Rust, "use crate::module as local;"),
            [(
                "module".to_owned(),
                vec![ImportBinding::Namespace {
                    local: "local".to_owned(),
                }]
            )]
        );
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::TypeScript,
                "import primary, { named as local } from \"./dep\";"
            )[0]
            .1,
            [
                ImportBinding::Named {
                    local: "primary".to_owned(),
                    imported: "default".to_owned(),
                },
                ImportBinding::Named {
                    local: "local".to_owned(),
                    imported: "named".to_owned(),
                },
            ]
        );
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::TypeScript,
                "import { first, second as local } from \"./dep\";"
            )[0]
            .1,
            [
                ImportBinding::Named {
                    local: "first".to_owned(),
                    imported: "first".to_owned(),
                },
                ImportBinding::Named {
                    local: "local".to_owned(),
                    imported: "second".to_owned(),
                },
            ]
        );
        assert_eq!(
            parse_import(SemanticProjectLanguage::Python, "import package as local")[0].1,
            [ImportBinding::Namespace {
                local: "local".to_owned(),
            }]
        );
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::Go,
                "import (\n\"example/one\"\n_ \"example/two\"\n. \"example/three\"\n)"
            ),
            [
                (
                    "example/one".to_owned(),
                    vec![ImportBinding::Namespace {
                        local: "one".to_owned(),
                    }]
                ),
                ("example/two".to_owned(), vec![ImportBinding::SideEffect]),
                ("example/three".to_owned(), vec![ImportBinding::Wildcard]),
            ]
        );
    }

    #[test]
    fn rust_grouped_imports_preserve_each_module_boundary() {
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::Rust,
                "use crate::{alpha, beta as local_beta};"
            ),
            [
                (
                    "alpha".to_owned(),
                    vec![ImportBinding::Namespace {
                        local: "alpha".to_owned(),
                    }]
                ),
                (
                    "beta".to_owned(),
                    vec![ImportBinding::Namespace {
                        local: "local_beta".to_owned(),
                    }]
                ),
            ]
        );
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::Rust,
                "use crate::{alpha::{first, second as local_second}, beta::third};"
            ),
            [
                (
                    "alpha".to_owned(),
                    vec![
                        ImportBinding::Named {
                            local: "first".to_owned(),
                            imported: "first".to_owned(),
                        },
                        ImportBinding::Named {
                            local: "local_second".to_owned(),
                            imported: "second".to_owned(),
                        },
                    ]
                ),
                (
                    "beta".to_owned(),
                    vec![ImportBinding::Named {
                        local: "third".to_owned(),
                        imported: "third".to_owned(),
                    }]
                ),
            ]
        );
        assert!(
            parse_import(SemanticProjectLanguage::Rust, "use crate::{alpha, beta};")
                .into_iter()
                .flat_map(|(_, bindings)| bindings)
                .all(|binding| matches!(binding, ImportBinding::Namespace { .. }))
        );
    }

    #[test]
    fn python_multi_imports_create_only_declared_namespaces() {
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::Python,
                "import alpha, package.beta, gamma.delta as local_gamma"
            ),
            [
                (
                    "alpha".to_owned(),
                    vec![ImportBinding::Namespace {
                        local: "alpha".to_owned(),
                    }]
                ),
                (
                    "package.beta".to_owned(),
                    vec![ImportBinding::Namespace {
                        local: "package".to_owned(),
                    }]
                ),
                (
                    "gamma.delta".to_owned(),
                    vec![ImportBinding::Namespace {
                        local: "local_gamma".to_owned(),
                    }]
                ),
            ]
        );
    }

    #[test]
    fn ecmascript_default_and_namespace_imports_retain_both_bindings() {
        assert_eq!(
            parse_import(
                SemanticProjectLanguage::TypeScript,
                "import primary, * as dependency from \"./dep\";"
            ),
            [(
                "./dep".to_owned(),
                vec![
                    ImportBinding::Named {
                        local: "primary".to_owned(),
                        imported: "default".to_owned(),
                    },
                    ImportBinding::Namespace {
                        local: "dependency".to_owned(),
                    },
                ]
            )]
        );
        assert!(
            parse_import(
                SemanticProjectLanguage::JavaScript,
                "import primary, * as dependency from \"./dep\";"
            )[0]
            .1
            .iter()
            .all(|binding| !matches!(binding, ImportBinding::Wildcard))
        );
    }

    #[test]
    fn dynamic_dispatch_never_promotes_a_single_candidate_to_exact() {
        let symbol = SymbolId::from_bytes([7; 20]);
        let dynamic = ResolutionCandidates {
            symbols: vec![symbol],
            kind: ResolutionKind::DynamicDispatch,
        };
        let dynamic =
            bound_resolution_candidates(dynamic, 8).expect("dynamic candidates are bounded");
        assert_eq!(
            occurrence_target("run", &dynamic),
            OccurrenceTarget::Candidates {
                symbols: vec![symbol],
                total_count: 1,
                completeness: CoverageStatus::Unknown,
            }
        );

        let binding = ResolutionCandidates {
            symbols: vec![symbol],
            kind: ResolutionKind::Binding,
        };
        let binding =
            bound_resolution_candidates(binding, 8).expect("binding candidates are bounded");
        assert_eq!(
            occurrence_target("run", &binding),
            OccurrenceTarget::Resolved { symbol }
        );
    }

    #[test]
    fn occurrence_candidates_are_canonical_before_identity_derivation() {
        let first = SymbolId::from_bytes([1; 20]);
        let last = SymbolId::from_bytes([9; 20]);
        let resolution = ResolutionCandidates {
            symbols: vec![last, first, last],
            kind: ResolutionKind::Binding,
        };
        let resolution =
            bound_resolution_candidates(resolution, 8).expect("candidates are bounded");

        assert_eq!(
            occurrence_target("duplicate", &resolution),
            OccurrenceTarget::Candidates {
                symbols: vec![first, last],
                total_count: 2,
                completeness: CoverageStatus::Complete,
            }
        );
    }

    #[test]
    fn occurrence_candidates_retain_total_count_when_the_sample_is_bounded() {
        let first = SymbolId::from_bytes([1; 20]);
        let middle = SymbolId::from_bytes([5; 20]);
        let last = SymbolId::from_bytes([9; 20]);
        let resolution = ResolutionCandidates {
            symbols: vec![last, middle, first, last],
            kind: ResolutionKind::Binding,
        };
        let resolution =
            bound_resolution_candidates(resolution, 2).expect("candidates are bounded");

        assert_eq!(
            occurrence_target("duplicate", &resolution),
            OccurrenceTarget::Candidates {
                symbols: vec![first, middle],
                total_count: 3,
                completeness: CoverageStatus::Bounded,
            }
        );
    }

    #[test]
    fn go_and_python_visibility_uses_the_declared_identifier() {
        assert_eq!(
            infer_visibility(
                SemanticProjectLanguage::Go,
                "ExportedType struct{}",
                "ExportedType"
            ),
            EntityVisibility::Public
        );
        assert_eq!(
            infer_visibility(SemanticProjectLanguage::Go, "holdoutA struct{}", "holdoutA"),
            EntityVisibility::Private
        );
        assert_eq!(
            infer_visibility(SemanticProjectLanguage::Python, "def _hidden():", "_hidden"),
            EntityVisibility::Private
        );
    }
}
