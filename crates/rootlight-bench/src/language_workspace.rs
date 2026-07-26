//! Deterministic evidence for audited grammars and bounded workspace behavior.
//!
//! The artifact recomputes contract observations without repository execution,
//! source reads, network access, or timing-dependent values. Missing promotion
//! evidence remains an explicit fallback reason rather than an inferred pass.

use rootlight_adapter_treesitter::{
    ADAPTER_VERSION as TREE_SITTER_ADAPTER_VERSION, GrammarRegistry, TREE_SITTER_RUNTIME_VERSION,
};
use rootlight_adapters::{
    ADAPTER_VERSION as SEMANTIC_ADAPTER_VERSION, ScipImportLimits, initial_semantic_registry,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId};
use rootlight_ir::{AnalysisTier, Confidence};
use rootlight_workspace::{
    CatalogLimits, CrossLinkVersion, LinkCaveat, LinkDeclaration, LinkDirection, LinkKind,
    LinkLimits, RepositoryDescriptor, RepositoryRootIdentity, RepositoryState, ServiceKey,
    SharedContentIdentity, SnapshotBuildMode, SnapshotLimits, WorkflowBudget, WorkflowKind,
    WorkflowRequest, WorkspaceCatalog, WorkspaceFactRef, WorkspaceId, WorkspaceSnapshot,
    WorkspaceSnapshotRequest, build_link_overlay, execute_workflow,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Schema written by the language and workspace evidence generator.
pub const LANGUAGE_WORKSPACE_EVIDENCE_SCHEMA: &str = "rootlight.language-workspace-evidence/1";
/// Maximum accepted encoded artifact size.
pub const LANGUAGE_WORKSPACE_EVIDENCE_MAX_BYTES: usize = 256 * 1024;

const EXPANDED_LANGUAGES: [&str; 6] = ["c", "cpp", "csharp", "java", "kotlin", "php"];
const CONTEXT_STATES: [&str; 6] = [
    "compile_database",
    "compile_database",
    "dotnet_project_model",
    "jvm_project_model",
    "jvm_project_model",
    "composer_metadata",
];
const LINK_KINDS: [LinkKind; 5] = [
    LinkKind::Package,
    LinkKind::Http,
    LinkKind::Rpc,
    LinkKind::Messaging,
    LinkKind::Database,
];
const WORKFLOW_KINDS: [WorkflowKind; 5] = [
    WorkflowKind::Impact,
    WorkflowKind::Flow,
    WorkflowKind::Context,
    WorkflowKind::Plan,
    WorkflowKind::Migration,
];
const FALLBACK_REASONS: [&str; 7] = [
    "single-repository-product-boundary",
    "language-holdout-evidence-unavailable",
    "cross-platform-promotion-evidence-unavailable",
    "deep-adapter-isolation-unavailable",
    "macro-origin-evidence-incomplete",
    "production-timing-evidence-unavailable",
    "scip-export-deferred",
];
const PERMITTED_CLAIMS: [&str; 5] = [
    "audited-structural-grammars",
    "caller-supplied-project-context-validation",
    "caller-supplied-scip-import",
    "process-local-immutable-workspace-snapshots",
    "bounded-declarative-cross-repository-links",
];
const PROHIBITED_CLAIMS: [&str; 6] = [
    "production-language-tier-promotion",
    "default-compiler-or-build-execution",
    "durable-cross-process-workspace-state",
    "exhaustive-macro-or-generated-origin-claims",
    "scip-export",
    "production-release",
];
const BLOCKED_DOWNSTREAM_WORK: [&str; 3] = [
    "production-operations",
    "release-engineering",
    "release-authorization",
];
const GRAMMAR_LOCK: &[u8] = include_bytes!("../../../adapters/grammars.lock");

/// Source-bound fallback evidence for language and workspace breadth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageWorkspaceEvidence {
    schema: String,
    source_revision: String,
    environment: EnvironmentEvidence,
    disposition: EvidenceDisposition,
    language: LanguageEvidence,
    workspace: WorkspaceEvidence,
    security: SecurityEvidence,
    risks: Vec<RiskObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentEvidence {
    operating_system: String,
    architecture: String,
    toolchain: String,
    build_profile: String,
    feature_profile: String,
    timing_status: String,
    process_memory_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceDisposition {
    outcome: String,
    reason_codes: Vec<String>,
    permitted_claims: Vec<String>,
    prohibited_claims: Vec<String>,
    blocked_downstream_work: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LanguageEvidence {
    tree_sitter_adapter_version: String,
    tree_sitter_runtime_version: String,
    semantic_adapter_version: String,
    grammar_lock_sha256: String,
    grammars: Vec<GrammarEvidence>,
    expanded_languages: Vec<ExpandedLanguageEvidence>,
    scip: ScipEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarEvidence {
    language: String,
    grammar_version: String,
    grammar_source_sha256: String,
    parser_sha256: String,
    scanner_sha256: Option<String>,
    abi_version: usize,
    encoding: String,
    observed_tier: String,
    production_promotion_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandedLanguageEvidence {
    language: String,
    project_context_state: String,
    semantic_policy_maximum_tier: String,
    observed_tier: String,
    uncertainty_codes: Vec<String>,
    holdout_available: bool,
    native_isolation_available: bool,
    compiler_execution_available: bool,
    generated_origin_complete: bool,
    production_promotion_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScipEvidence {
    import_available: bool,
    export_available: bool,
    input_ownership: String,
    max_index_bytes: usize,
    max_documents: usize,
    max_symbols: usize,
    max_occurrences: usize,
    max_relationships: usize,
    max_source_bytes: usize,
    max_total_source_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceEvidence {
    scale_samples: Vec<WorkspaceScaleSample>,
    independent_generation: IndependentGenerationEvidence,
    partial_snapshot: PartialSnapshotEvidence,
    cross_links: CrossLinkEvidence,
    workflows: Vec<WorkflowEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceScaleSample {
    repositories: usize,
    registrations: usize,
    publications: usize,
    requested_members: usize,
    available_members: usize,
    failures: usize,
    encoded_snapshot_bytes: usize,
    complete: bool,
    timing_status: String,
    memory_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndependentGenerationEvidence {
    retained_snapshot_survives_advance: bool,
    reclaimed_member_invalidates_snapshot: bool,
    unrelated_generation_survives_reclamation: bool,
    unrelated_generation_survives_deletion: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialSnapshotEvidence {
    requested_members: usize,
    available_members: usize,
    explicit_failures: usize,
    complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrossLinkEvidence {
    declaration_count: usize,
    links: usize,
    unresolved_consumers: usize,
    kinds: Vec<String>,
    all_endpoints_pin_generations: bool,
    ambiguity_policy: String,
    discovery_capability_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowEvidence {
    kind: String,
    rows: usize,
    edges_scanned: usize,
    repositories_charged: usize,
    source_bytes: usize,
    estimated_json_bytes: usize,
    estimated_tokens: usize,
    exact_generation_references: bool,
    truncated: bool,
    continuation_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecurityEvidence {
    filesystem_capability_available: bool,
    network_capability_available: bool,
    repository_execution_capability_available: bool,
    workspace_source_reads: usize,
    unsafe_code_allowed: bool,
    traversal_budgets_enforced: bool,
    cancellation_contract_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RiskObservation {
    risk: String,
    detection_status: String,
    mitigation: String,
    contingency_exercised: bool,
    owner_role: String,
}

/// Builds deterministic contract evidence for one exact source revision.
///
/// # Errors
///
/// Returns [`LanguageWorkspaceEvidenceError`] when a built-in registry,
/// workspace invariant, bounded workflow, revision, or toolchain value is
/// inconsistent.
pub fn build_language_workspace_evidence(
    source_revision: &str,
    toolchain: &str,
) -> Result<LanguageWorkspaceEvidence, LanguageWorkspaceEvidenceError> {
    validate_revision(source_revision)?;
    validate_toolchain(toolchain)?;
    let language = language_evidence()?;
    let workspace = workspace_evidence()?;
    let evidence = LanguageWorkspaceEvidence {
        schema: LANGUAGE_WORKSPACE_EVIDENCE_SCHEMA.to_owned(),
        source_revision: source_revision.to_owned(),
        environment: EnvironmentEvidence {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            toolchain: toolchain.to_owned(),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
            feature_profile: "workspace-default".to_owned(),
            timing_status: "unavailable_deterministic_contract_only".to_owned(),
            process_memory_status: "unavailable_deterministic_contract_only".to_owned(),
        },
        disposition: EvidenceDisposition {
            outcome: "fallback".to_owned(),
            reason_codes: strings(&FALLBACK_REASONS),
            permitted_claims: strings(&PERMITTED_CLAIMS),
            prohibited_claims: strings(&PROHIBITED_CLAIMS),
            blocked_downstream_work: strings(&BLOCKED_DOWNSTREAM_WORK),
        },
        language,
        workspace,
        security: SecurityEvidence {
            filesystem_capability_available: false,
            network_capability_available: false,
            repository_execution_capability_available: false,
            workspace_source_reads: 0,
            unsafe_code_allowed: false,
            traversal_budgets_enforced: true,
            cancellation_contract_available: true,
        },
        risks: risk_observations(),
    };
    validate_evidence(&evidence, source_revision, toolchain)?;
    Ok(evidence)
}

/// Encodes an evidence artifact using the canonical struct field order.
///
/// # Errors
///
/// Returns [`LanguageWorkspaceEvidenceError`] when serialization fails or the
/// single-file ceiling is exceeded.
pub fn encode_language_workspace_evidence(
    evidence: &LanguageWorkspaceEvidence,
) -> Result<Vec<u8>, LanguageWorkspaceEvidenceError> {
    let encoded = serde_json::to_vec(evidence)?;
    if encoded.len() > LANGUAGE_WORKSPACE_EVIDENCE_MAX_BYTES {
        return Err(LanguageWorkspaceEvidenceError::ArtifactLimit);
    }
    privacy_scan(&encoded)?;
    Ok(encoded)
}

/// Strictly decodes, recomputes, and verifies a source-bound evidence artifact.
///
/// # Errors
///
/// Returns [`LanguageWorkspaceEvidenceError`] for oversized, noncanonical,
/// source-mismatched, private-path-bearing, or irreproducible evidence.
pub fn verify_language_workspace_evidence(
    encoded: &[u8],
    source_revision: &str,
    toolchain: &str,
) -> Result<(), LanguageWorkspaceEvidenceError> {
    if encoded.len() > LANGUAGE_WORKSPACE_EVIDENCE_MAX_BYTES {
        return Err(LanguageWorkspaceEvidenceError::ArtifactLimit);
    }
    privacy_scan(encoded)?;
    let evidence: LanguageWorkspaceEvidence = serde_json::from_slice(encoded)?;
    validate_evidence(&evidence, source_revision, toolchain)?;
    let canonical = encode_language_workspace_evidence(&evidence)?;
    if encoded.strip_suffix(b"\n").unwrap_or(encoded) != canonical {
        return Err(LanguageWorkspaceEvidenceError::NonCanonical);
    }
    let expected = build_language_workspace_evidence(source_revision, toolchain)?;
    if evidence != expected {
        return Err(LanguageWorkspaceEvidenceError::ReproductionMismatch);
    }
    Ok(())
}

fn language_evidence() -> Result<LanguageEvidence, LanguageWorkspaceEvidenceError> {
    let registry =
        GrammarRegistry::audited().map_err(|_| LanguageWorkspaceEvidenceError::LanguageRegistry)?;
    let mut grammars = registry
        .descriptors()
        .iter()
        .map(|descriptor| GrammarEvidence {
            language: descriptor.language().as_str().to_owned(),
            grammar_version: descriptor.grammar_version().to_owned(),
            grammar_source_sha256: descriptor.grammar_source_sha256().to_owned(),
            parser_sha256: descriptor.parser_sha256().to_owned(),
            scanner_sha256: descriptor.scanner_sha256().map(str::to_owned),
            abi_version: descriptor.abi_version(),
            encoding: descriptor.encoding().as_str().to_owned(),
            observed_tier: "tier_d".to_owned(),
            production_promotion_eligible: false,
        })
        .collect::<Vec<_>>();
    grammars.sort_by(|left, right| left.language.cmp(&right.language));

    let semantic_registry = initial_semantic_registry()
        .map_err(|_| LanguageWorkspaceEvidenceError::LanguageRegistry)?;
    let mut expanded_languages = Vec::with_capacity(EXPANDED_LANGUAGES.len());
    for (language, context_state) in EXPANDED_LANGUAGES.into_iter().zip(CONTEXT_STATES) {
        if !grammars.iter().any(|grammar| grammar.language == language) {
            return Err(LanguageWorkspaceEvidenceError::LanguageRegistry);
        }
        let profile = semantic_registry
            .iter()
            .find(|profile| profile.language().as_str() == language)
            .ok_or(LanguageWorkspaceEvidenceError::LanguageRegistry)?;
        expanded_languages.push(ExpandedLanguageEvidence {
            language: language.to_owned(),
            project_context_state: context_state.to_owned(),
            semantic_policy_maximum_tier: tier_name(profile.maximum_tier())?.to_owned(),
            observed_tier: "tier_d".to_owned(),
            uncertainty_codes: profile
                .uncertainties()
                .map(|uncertainty| uncertainty.as_str().to_owned())
                .collect(),
            holdout_available: false,
            native_isolation_available: false,
            compiler_execution_available: false,
            generated_origin_complete: false,
            production_promotion_eligible: false,
        });
    }
    let limits = ScipImportLimits::default();
    Ok(LanguageEvidence {
        tree_sitter_adapter_version: TREE_SITTER_ADAPTER_VERSION.to_owned(),
        tree_sitter_runtime_version: TREE_SITTER_RUNTIME_VERSION.to_owned(),
        semantic_adapter_version: SEMANTIC_ADAPTER_VERSION.to_owned(),
        grammar_lock_sha256: sha256_hex(GRAMMAR_LOCK),
        grammars,
        expanded_languages,
        scip: ScipEvidence {
            import_available: true,
            export_available: false,
            input_ownership: "caller_supplied_exact_bytes".to_owned(),
            max_index_bytes: limits.max_index_bytes(),
            max_documents: limits.max_documents(),
            max_symbols: limits.max_symbols(),
            max_occurrences: limits.max_occurrences(),
            max_relationships: limits.max_relationships(),
            max_source_bytes: limits.max_source_bytes(),
            max_total_source_bytes: limits.max_total_source_bytes(),
        },
    })
}

fn workspace_evidence() -> Result<WorkspaceEvidence, LanguageWorkspaceEvidenceError> {
    let scale_samples = [1_usize, 10, 100]
        .into_iter()
        .map(scale_sample)
        .collect::<Result<Vec<_>, _>>()?;
    let (snapshot, catalog) = snapshot_fixture(10)?;
    let independent_generation = independent_generation_evidence(snapshot.clone(), catalog)?;
    let partial_snapshot = partial_snapshot_evidence()?;
    let cross_links = cross_link_evidence()?;
    let workflows = workflow_evidence()?;
    Ok(WorkspaceEvidence {
        scale_samples,
        independent_generation,
        partial_snapshot,
        cross_links,
        workflows,
    })
}

fn scale_sample(
    repositories: usize,
) -> Result<WorkspaceScaleSample, LanguageWorkspaceEvidenceError> {
    let (snapshot, _) = snapshot_fixture(repositories)?;
    let encoded_snapshot_bytes = serde_json::to_vec(&snapshot)?.len();
    Ok(WorkspaceScaleSample {
        repositories,
        registrations: repositories,
        publications: repositories,
        requested_members: snapshot.requested_members(),
        available_members: snapshot.members().len(),
        failures: snapshot.failures().len(),
        encoded_snapshot_bytes,
        complete: snapshot.is_complete(),
        timing_status: "unavailable_deterministic_contract_only".to_owned(),
        memory_status: "unavailable_deterministic_contract_only".to_owned(),
    })
}

fn independent_generation_evidence(
    snapshot: WorkspaceSnapshot,
    mut catalog: WorkspaceCatalog,
) -> Result<IndependentGenerationEvidence, LanguageWorkspaceEvidenceError> {
    let cancellation = Cancellation::new();
    catalog
        .publish_generation(repository(1), generation(111), &cancellation)
        .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let retained_snapshot_survives_advance = snapshot.validate(&catalog, 15, &cancellation).is_ok();
    catalog
        .reclaim_generation(repository(1), generation(1), &cancellation)
        .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let reclaimed_member_invalidates_snapshot =
        snapshot.validate(&catalog, 15, &cancellation).is_err();
    let unrelated_generation_survives_reclamation = catalog
        .repository(repository(2))
        .is_some_and(|entry| entry.retains(generation(2)));
    catalog
        .set_state(repository(1), RepositoryState::Deleted)
        .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let unrelated_generation_survives_deletion = catalog
        .repository(repository(2))
        .is_some_and(|entry| entry.retains(generation(2)));
    if !retained_snapshot_survives_advance
        || !reclaimed_member_invalidates_snapshot
        || !unrelated_generation_survives_reclamation
        || !unrelated_generation_survives_deletion
    {
        return Err(LanguageWorkspaceEvidenceError::WorkspaceContract);
    }
    Ok(IndependentGenerationEvidence {
        retained_snapshot_survives_advance,
        reclaimed_member_invalidates_snapshot,
        unrelated_generation_survives_reclamation,
        unrelated_generation_survives_deletion,
    })
}

fn partial_snapshot_evidence() -> Result<PartialSnapshotEvidence, LanguageWorkspaceEvidenceError> {
    let (_, mut catalog) = snapshot_fixture(10)?;
    catalog
        .set_state(repository(10), RepositoryState::Unavailable)
        .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let snapshot = WorkspaceSnapshot::build(
        &catalog,
        snapshot_request(10),
        SnapshotBuildMode::AllowPartial,
        SnapshotLimits::new(10, 10)
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?,
        &Cancellation::new(),
    )
    .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    if snapshot.is_complete() || snapshot.members().len() != 9 || snapshot.failures().len() != 1 {
        return Err(LanguageWorkspaceEvidenceError::WorkspaceContract);
    }
    Ok(PartialSnapshotEvidence {
        requested_members: snapshot.requested_members(),
        available_members: snapshot.members().len(),
        explicit_failures: snapshot.failures().len(),
        complete: snapshot.is_complete(),
    })
}

fn cross_link_evidence() -> Result<CrossLinkEvidence, LanguageWorkspaceEvidenceError> {
    let (snapshot, _) = snapshot_fixture(2)?;
    let mut declarations = Vec::with_capacity(LINK_KINDS.len().saturating_mul(2));
    for (index, kind) in LINK_KINDS.into_iter().enumerate() {
        let seed = u8::try_from(index)
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?
            .saturating_add(1);
        let key = service_key(kind, seed)?;
        declarations.push(LinkDeclaration::new(
            endpoint(1, seed),
            kind,
            LinkDirection::Provider,
            key.clone(),
            confidence(950)?,
        ));
        declarations.push(
            LinkDeclaration::new(
                endpoint(2, seed.saturating_add(32)),
                kind,
                LinkDirection::Consumer,
                key,
                confidence(850)?,
            )
            .with_caveat(LinkCaveat::GeneratedConfiguration),
        );
    }
    let overlay = build_link_overlay(
        &snapshot,
        declarations,
        LinkLimits::default(),
        &Cancellation::new(),
    )
    .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let all_endpoints_pin_generations = overlay.links().iter().all(|link| {
        snapshot.generation_for(link.consumer().repository()) == Some(link.consumer().generation())
            && link.candidates().iter().all(|candidate| {
                snapshot.generation_for(candidate.endpoint().repository())
                    == Some(candidate.endpoint().generation())
            })
    });
    if overlay.links().len() != LINK_KINDS.len() || !all_endpoints_pin_generations {
        return Err(LanguageWorkspaceEvidenceError::WorkspaceContract);
    }
    Ok(CrossLinkEvidence {
        declaration_count: overlay.declarations(),
        links: overlay.links().len(),
        unresolved_consumers: overlay.unresolved_consumers(),
        kinds: LINK_KINDS
            .into_iter()
            .map(link_kind_name)
            .collect::<Result<Vec<_>, _>>()?,
        all_endpoints_pin_generations,
        ambiguity_policy: "bounded_explicit_candidate_sets".to_owned(),
        discovery_capability_available: false,
    })
}

fn workflow_evidence() -> Result<Vec<WorkflowEvidence>, LanguageWorkspaceEvidenceError> {
    let (snapshot, _) = snapshot_fixture(10)?;
    let mut declarations = Vec::with_capacity(18);
    for seed in 1_u8..10 {
        let key = ServiceKey::named(LinkKind::Rpc, &format!("service.edge-{seed}"))
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
        declarations.push(LinkDeclaration::new(
            endpoint(seed, seed),
            LinkKind::Rpc,
            LinkDirection::Provider,
            key.clone(),
            confidence(950)?,
        ));
        declarations.push(LinkDeclaration::new(
            endpoint(seed.saturating_add(1), seed.saturating_add(1)),
            LinkKind::Rpc,
            LinkDirection::Consumer,
            key,
            confidence(900)?,
        ));
    }
    let overlay = build_link_overlay(
        &snapshot,
        declarations,
        LinkLimits::default(),
        &Cancellation::new(),
    )
    .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let budget = WorkflowBudget::new(10, 32, 64, 16, 4)
        .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    WORKFLOW_KINDS
        .into_iter()
        .map(|kind| {
            let seed = match kind {
                WorkflowKind::Impact => endpoint(1, 1),
                WorkflowKind::Flow
                | WorkflowKind::Context
                | WorkflowKind::Plan
                | WorkflowKind::Migration => endpoint(10, 10),
                _ => return Err(LanguageWorkspaceEvidenceError::WorkspaceContract),
            };
            let result = execute_workflow(
                &snapshot,
                &overlay,
                WorkflowRequest::new(kind, budget).with_seed(seed),
                &Cancellation::new(),
            )
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
            let expected_rows = if matches!(kind, WorkflowKind::Impact | WorkflowKind::Flow) {
                9
            } else {
                18
            };
            let exact_generation_references = result.rows().iter().all(|row| {
                snapshot.generation_for(row.from().repository()) == Some(row.from().generation())
                    && snapshot.generation_for(row.to().repository()) == Some(row.to().generation())
            });
            if result.rows().len() != expected_rows
                || result.truncated()
                || result.continuation().is_some()
                || result.source_bytes() != 0
                || !exact_generation_references
            {
                return Err(LanguageWorkspaceEvidenceError::WorkspaceContract);
            }
            Ok(WorkflowEvidence {
                kind: workflow_kind_name(kind)?.to_owned(),
                rows: result.rows().len(),
                edges_scanned: result.edges_scanned(),
                repositories_charged: result.repository_usage().len(),
                source_bytes: result.source_bytes(),
                estimated_json_bytes: result.estimated_json_bytes(),
                estimated_tokens: result.estimated_tokens(),
                exact_generation_references,
                truncated: result.truncated(),
                continuation_present: result.continuation().is_some(),
            })
        })
        .collect()
}

fn snapshot_fixture(
    repositories: usize,
) -> Result<(WorkspaceSnapshot, WorkspaceCatalog), LanguageWorkspaceEvidenceError> {
    let cancellation = Cancellation::new();
    let limits = CatalogLimits::new(repositories, 1, 1, 2)
        .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    let mut catalog = WorkspaceCatalog::new(WorkspaceId::from_hash(hash(240)), limits);
    for ordinal in 1..=repositories {
        let seed =
            u8::try_from(ordinal).map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
        catalog
            .register(
                RepositoryDescriptor::new(
                    repository(seed),
                    RepositoryRootIdentity::from_hash(hash(seed)),
                    SharedContentIdentity::from_hash(hash(seed.saturating_add(128))),
                ),
                &cancellation,
            )
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
        catalog
            .publish_generation(repository(seed), generation(seed), &cancellation)
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    }
    let snapshot = WorkspaceSnapshot::build(
        &catalog,
        snapshot_request(repositories),
        SnapshotBuildMode::Strict,
        SnapshotLimits::new(repositories, repositories)
            .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?,
        &cancellation,
    )
    .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)?;
    Ok((snapshot, catalog))
}

fn snapshot_request(repositories: usize) -> WorkspaceSnapshotRequest {
    (1..=repositories).fold(
        WorkspaceSnapshotRequest::new(hash(241), CrossLinkVersion::from_hash(hash(242)), 10, 20),
        |request, ordinal| {
            let seed = u8::try_from(ordinal).unwrap_or(u8::MAX);
            request.with_member(repository(seed), generation(seed))
        },
    )
}

fn endpoint(repository_seed: u8, fact_seed: u8) -> WorkspaceFactRef {
    WorkspaceFactRef::new(
        repository(repository_seed),
        generation(repository_seed),
        FactId::from_bytes([fact_seed; 20]),
    )
}

fn repository(seed: u8) -> RepositoryId {
    RepositoryId::from_bytes([seed; 16])
}

fn generation(seed: u8) -> GenerationId {
    GenerationId::from_bytes([seed; 20])
}

fn hash(seed: u8) -> ContentHash {
    ContentHash::from_bytes([seed; 32])
}

fn confidence(value: u16) -> Result<Confidence, LanguageWorkspaceEvidenceError> {
    Confidence::new(value).map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)
}

fn service_key(kind: LinkKind, seed: u8) -> Result<ServiceKey, LanguageWorkspaceEvidenceError> {
    match kind {
        LinkKind::Package => ServiceKey::immutable(kind, hash(seed)),
        LinkKind::Http => ServiceKey::http("GET", &format!("/service/{seed}/{{resource}}")),
        LinkKind::Rpc => ServiceKey::named(kind, &format!("service.{seed}.call")),
        LinkKind::Messaging => ServiceKey::named(kind, &format!("service.{seed}.events")),
        LinkKind::Database => ServiceKey::named(kind, &format!("service.{seed}.records")),
        _ => return Err(LanguageWorkspaceEvidenceError::WorkspaceContract),
    }
    .map_err(|_| LanguageWorkspaceEvidenceError::WorkspaceContract)
}

fn tier_name(tier: AnalysisTier) -> Result<&'static str, LanguageWorkspaceEvidenceError> {
    match tier {
        AnalysisTier::TierA => Ok("tier_a"),
        AnalysisTier::TierB => Ok("tier_b"),
        AnalysisTier::TierC => Ok("tier_c"),
        AnalysisTier::TierD => Ok("tier_d"),
        _ => Err(LanguageWorkspaceEvidenceError::LanguageRegistry),
    }
}

fn link_kind_name(kind: LinkKind) -> Result<String, LanguageWorkspaceEvidenceError> {
    let value = match kind {
        LinkKind::Package => "package",
        LinkKind::Http => "http",
        LinkKind::Rpc => "rpc",
        LinkKind::Messaging => "messaging",
        LinkKind::Database => "database",
        _ => return Err(LanguageWorkspaceEvidenceError::WorkspaceContract),
    };
    Ok(value.to_owned())
}

fn workflow_kind_name(kind: WorkflowKind) -> Result<&'static str, LanguageWorkspaceEvidenceError> {
    match kind {
        WorkflowKind::Impact => Ok("impact"),
        WorkflowKind::Flow => Ok("flow"),
        WorkflowKind::Context => Ok("context"),
        WorkflowKind::Plan => Ok("plan"),
        WorkflowKind::Migration => Ok("migration"),
        _ => Err(LanguageWorkspaceEvidenceError::WorkspaceContract),
    }
}

fn validate_evidence(
    evidence: &LanguageWorkspaceEvidence,
    source_revision: &str,
    toolchain: &str,
) -> Result<(), LanguageWorkspaceEvidenceError> {
    validate_revision(source_revision)?;
    validate_toolchain(toolchain)?;
    if evidence.schema != LANGUAGE_WORKSPACE_EVIDENCE_SCHEMA
        || evidence.source_revision != source_revision
        || evidence.environment.toolchain != toolchain
        || evidence.disposition.outcome != "fallback"
        || evidence.disposition.reason_codes != strings(&FALLBACK_REASONS)
        || evidence.disposition.permitted_claims != strings(&PERMITTED_CLAIMS)
        || evidence.disposition.prohibited_claims != strings(&PROHIBITED_CLAIMS)
        || evidence.disposition.blocked_downstream_work != strings(&BLOCKED_DOWNSTREAM_WORK)
        || evidence.language.grammars.len() != 11
        || evidence.language.expanded_languages.len() != EXPANDED_LANGUAGES.len()
        || evidence.language.scip.export_available
        || !evidence.language.scip.import_available
        || evidence.workspace.scale_samples.len() != 3
        || evidence.workspace.workflows.len() != WORKFLOW_KINDS.len()
        || evidence.workspace.cross_links.kinds.len() != LINK_KINDS.len()
        || evidence.security.filesystem_capability_available
        || evidence.security.network_capability_available
        || evidence.security.repository_execution_capability_available
        || evidence.security.workspace_source_reads != 0
        || evidence.security.unsafe_code_allowed
        || evidence.risks.len() != 8
    {
        return Err(LanguageWorkspaceEvidenceError::InvalidEvidence);
    }
    if evidence.language.grammars.iter().any(|grammar| {
        !is_sha256(&grammar.grammar_source_sha256)
            || !is_sha256(&grammar.parser_sha256)
            || grammar
                .scanner_sha256
                .as_deref()
                .is_some_and(|digest| !is_sha256(digest))
            || grammar.observed_tier != "tier_d"
            || grammar.production_promotion_eligible
    }) || evidence
        .language
        .expanded_languages
        .iter()
        .zip(EXPANDED_LANGUAGES)
        .any(|(language, expected)| {
            language.language != expected
                || language.observed_tier != "tier_d"
                || language.holdout_available
                || language.native_isolation_available
                || language.compiler_execution_available
                || language.generated_origin_complete
                || language.production_promotion_eligible
        })
        || evidence
            .workspace
            .scale_samples
            .iter()
            .zip([1_usize, 10, 100])
            .any(|(sample, expected)| {
                sample.repositories != expected
                    || sample.registrations != expected
                    || sample.publications != expected
                    || sample.requested_members != expected
                    || sample.available_members != expected
                    || sample.failures != 0
                    || !sample.complete
                    || sample.encoded_snapshot_bytes == 0
            })
        || evidence.workspace.workflows.iter().any(|workflow| {
            workflow.source_bytes != 0
                || !workflow.exact_generation_references
                || workflow.truncated
                || workflow.continuation_present
        })
    {
        return Err(LanguageWorkspaceEvidenceError::InvalidEvidence);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), LanguageWorkspaceEvidenceError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LanguageWorkspaceEvidenceError::InvalidRevision);
    }
    Ok(())
}

fn validate_toolchain(value: &str) -> Result<(), LanguageWorkspaceEvidenceError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() && byte != b' ')
        || value.contains(['/', '\\'])
    {
        return Err(LanguageWorkspaceEvidenceError::InvalidToolchain);
    }
    Ok(())
}

fn privacy_scan(encoded: &[u8]) -> Result<(), LanguageWorkspaceEvidenceError> {
    const MARKERS: [&[u8]; 5] = [
        b"C:\\Users\\",
        b"C:\\\\Users\\\\",
        b"/home/",
        b"/Users/",
        b"file://",
    ];
    if MARKERS.iter().any(|marker| {
        encoded
            .windows(marker.len())
            .any(|window| window == *marker)
    }) {
        return Err(LanguageWorkspaceEvidenceError::PrivacyBoundary);
    }
    Ok(())
}

fn strings<const N: usize>(values: &[&str; N]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn risk_observations() -> Vec<RiskObservation> {
    [
        (
            "structural-call-accuracy",
            "quality_holdout_unavailable",
            "syntax_only_tier_with_unresolved_semantics",
            true,
            "static_analysis_lead",
        ),
        (
            "unsafe-build-context",
            "native_isolation_unavailable",
            "caller_supplied_declarative_context_only",
            true,
            "language_adapter_lead",
        ),
        (
            "repository-execution",
            "not_observed_capability_absent",
            "no_process_network_or_filesystem_capability",
            false,
            "security_lead",
        ),
        (
            "platform-isolation",
            "cross_platform_promotion_evidence_unavailable",
            "deep_tiers_fail_closed",
            true,
            "security_lead",
        ),
        (
            "workspace-memory-scale",
            "rss_measurement_unavailable",
            "hard_repository_and_response_bounds",
            true,
            "performance_engineer",
        ),
        (
            "mixed-generation-links",
            "not_observed_in_contract_fixture",
            "snapshot_and_endpoint_generation_pinning",
            false,
            "workspace_owner",
        ),
        (
            "corpus-overfit",
            "language_holdout_unavailable",
            "no_production_tier_promotion",
            true,
            "quality_lead",
        ),
        (
            "generated-origin-loss",
            "origin_evidence_incomplete",
            "generated_caveats_and_no_exhaustive_claim",
            true,
            "language_adapter_lead",
        ),
    ]
    .into_iter()
    .map(
        |(risk, detection_status, mitigation, contingency_exercised, owner_role)| RiskObservation {
            risk: risk.to_owned(),
            detection_status: detection_status.to_owned(),
            mitigation: mitigation.to_owned(),
            contingency_exercised,
            owner_role: owner_role.to_owned(),
        },
    )
    .collect()
}

/// Invalid or irreproducible language and workspace evidence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LanguageWorkspaceEvidenceError {
    /// Source revision is not a canonical lowercase 40-digit hexadecimal ID.
    #[error("language and workspace evidence source revision is invalid")]
    InvalidRevision,
    /// Toolchain label is empty, oversized, non-ASCII, or path-shaped.
    #[error("language and workspace evidence toolchain is invalid")]
    InvalidToolchain,
    /// Audited grammar or semantic profile registry is inconsistent.
    #[error("language and workspace evidence registry is invalid")]
    LanguageRegistry,
    /// Workspace contract construction or observation failed.
    #[error("language and workspace evidence contract observation failed")]
    WorkspaceContract,
    /// Strict evidence invariants do not match the declared fallback.
    #[error("language and workspace evidence is invalid")]
    InvalidEvidence,
    /// Encoded evidence exceeded the single-file ceiling.
    #[error("language and workspace evidence exceeds its byte limit")]
    ArtifactLimit,
    /// Encoded evidence is not in canonical field order.
    #[error("language and workspace evidence is not canonical")]
    NonCanonical,
    /// Recomputed evidence differs from the supplied artifact.
    #[error("language and workspace evidence cannot be reproduced")]
    ReproductionMismatch,
    /// Encoded evidence contains a private filesystem marker.
    #[error("language and workspace evidence crossed the privacy boundary")]
    PrivacyBoundary,
    /// JSON encoding or strict decoding failed.
    #[error("language and workspace evidence JSON is invalid")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const TOOLCHAIN: &str = "rustc 1.90.0";

    #[test]
    fn evidence_is_deterministic_and_recomputed_during_verification() {
        let first = build_language_workspace_evidence(REVISION, TOOLCHAIN)
            .expect("contract evidence should build");
        let second = build_language_workspace_evidence(REVISION, TOOLCHAIN)
            .expect("repeated contract evidence should build");
        assert_eq!(first, second);
        let encoded =
            encode_language_workspace_evidence(&first).expect("contract evidence should encode");
        verify_language_workspace_evidence(&encoded, REVISION, TOOLCHAIN)
            .expect("contract evidence should verify");
    }

    #[test]
    fn fallback_cannot_promote_languages_or_hide_workspace_failures() {
        let evidence = build_language_workspace_evidence(REVISION, TOOLCHAIN)
            .expect("contract evidence should build");
        assert_eq!(evidence.disposition.outcome, "fallback");
        assert!(
            evidence
                .language
                .expanded_languages
                .iter()
                .all(|language| !language.production_promotion_eligible)
        );
        assert_eq!(evidence.workspace.partial_snapshot.explicit_failures, 1);
        assert!(!evidence.workspace.partial_snapshot.complete);
    }

    #[test]
    fn verifier_rejects_tampering_paths_and_noncanonical_whitespace() {
        let evidence = build_language_workspace_evidence(REVISION, TOOLCHAIN)
            .expect("contract evidence should build");
        let encoded =
            encode_language_workspace_evidence(&evidence).expect("contract evidence should encode");
        let mut value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("contract evidence should decode");
        value["disposition"]["outcome"] = serde_json::Value::String("pass".to_owned());
        let tampered = serde_json::to_vec(&value).expect("tampered evidence should encode");
        assert!(verify_language_workspace_evidence(&tampered, REVISION, TOOLCHAIN).is_err());
        assert!(privacy_scan(br#"{"path":"C:\\Users\\private"}"#).is_err());
        let mut padded = encoded;
        padded.push(b' ');
        assert!(verify_language_workspace_evidence(&padded, REVISION, TOOLCHAIN).is_err());
    }
}
