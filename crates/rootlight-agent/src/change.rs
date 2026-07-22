//! Transport-neutral normalization and shaping for `plan.change`.
//!
//! The MCP application supplies daemon facts through its client adapter. This
//! module owns request admission, source-free explain shaping, and conversion
//! of client-independent planning facts into the public contract.

use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_mcp_contract::{
    GenerationSelector, RepositorySelector,
    change::{
        ChangePlanStep, ContextPackRequest, PlanChangeData, PlanChangeInput, PlanDecision,
        PlanImpactSummary, PlanObjective, PlanTargetSelector, RiskLevel, TestCandidate,
    },
    context::PlanExplanation,
};

use crate::{
    explain::{finalize_plan, plan_change_plan},
    policy::is_compact_profile,
};

/// Admitted `plan.change` request independent of a concrete daemon client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangeRequest {
    repository: RepositoryId,
    generation: GenerationSelector,
    objective: PlanObjective,
    objective_text: String,
    target_symbols: Vec<SymbolId>,
    target_files: Vec<FileId>,
    max_steps: Option<u8>,
    explain_only: bool,
}

impl PlanChangeRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> &GenerationSelector {
        &self.generation
    }

    /// Returns the stable objective wire label expected by the daemon.
    #[must_use]
    pub const fn objective(&self) -> &'static str {
        objective_label(self.objective)
    }

    /// Returns the typed planning objective.
    #[must_use]
    pub const fn objective_kind(&self) -> PlanObjective {
        self.objective
    }

    /// Returns the concrete objective description.
    #[must_use]
    pub fn objective_text(&self) -> &str {
        &self.objective_text
    }

    /// Returns the explicit target symbol identifiers.
    #[must_use]
    pub fn target_symbols(&self) -> &[SymbolId] {
        &self.target_symbols
    }

    /// Returns the explicit target file identifiers.
    #[must_use]
    pub fn target_files(&self) -> &[FileId] {
        &self.target_files
    }

    /// Returns the optional plan-step ceiling.
    #[must_use]
    pub const fn max_steps(&self) -> Option<u8> {
        self.max_steps
    }

    /// Reports whether the request asks for planning metadata without retrieval.
    #[must_use]
    pub const fn explain_only(&self) -> bool {
        self.explain_only
    }

    /// Returns the total number of explicit targets.
    #[must_use]
    pub fn target_count(&self) -> usize {
        self.target_symbols
            .len()
            .saturating_add(self.target_files.len())
    }
}

/// Request-admission or response-shaping failure for `plan.change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanChangeError {
    /// A repository alias cannot be resolved at the agent boundary.
    #[error("repository selector is not supported")]
    UnsupportedRepository,
    /// One requested option is not implemented by the current planner.
    #[error("plan option is not supported")]
    UnsupportedOption,
    /// The request did not contain a resolvable target.
    #[error("plan requires at least one target")]
    EmptyTargets,
    /// The planning result contained an unknown risk label.
    #[error("plan result contains an invalid risk label")]
    InvalidRisk,
}

/// Normalizes and admits one public `plan.change` request.
///
/// # Errors
///
/// Returns [`PlanChangeError`] when the request requires alias resolution,
/// unsupported context or budget behavior, a non-compact response profile, or
/// contains no symbol or file target.
pub fn normalize_plan_change(input: PlanChangeInput) -> Result<PlanChangeRequest, PlanChangeError> {
    let RepositorySelector::ById(repository) = input.repository else {
        return Err(PlanChangeError::UnsupportedRepository);
    };
    if input.change_context.is_some()
        || input.constraints.is_some()
        || input.budget.is_some()
        || !is_compact_profile(input.profile)
    {
        return Err(PlanChangeError::UnsupportedOption);
    }

    let mut target_symbols = Vec::new();
    let mut target_files = Vec::new();
    for target in input.targets {
        match target {
            PlanTargetSelector::Symbol(symbol) => target_symbols.push(symbol.symbol_id),
            PlanTargetSelector::File(file) => target_files.push(file.file_id),
        }
    }
    if target_symbols.is_empty() && target_files.is_empty() {
        return Err(PlanChangeError::EmptyTargets);
    }

    Ok(PlanChangeRequest {
        repository: repository.repository_id,
        generation: input.generation.unwrap_or(GenerationSelector::Active(
            rootlight_mcp_contract::vertical::ActiveGeneration::Active,
        )),
        objective: input.objective,
        objective_text: input.objective_text,
        target_symbols,
        target_files,
        max_steps: input.max_steps,
        explain_only: input.explain == Some(true),
    })
}

/// Client-independent planning facts returned by a daemon adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangeResult {
    /// Ordered plan steps.
    pub plan: Vec<ChangePlanStep>,
    /// Compact affected-scope facts.
    pub affected_scope: PlanImpactResult,
    /// Ranked verification candidates.
    pub test_plan: Vec<TestCandidate>,
    /// Decisions that require user input.
    pub open_decisions: Vec<PlanDecision>,
    /// Follow-up context-pack arguments.
    pub context_pack_request: ContextPackRequest,
}

/// Client-independent affected-scope facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanImpactResult {
    /// Total affected symbol count.
    pub affected_symbols: u32,
    /// Total affected file count.
    pub affected_files: u32,
    /// Stable daemon risk label.
    pub risk_level: String,
    /// Whether the public API surface is affected.
    pub touches_public_surface: bool,
}

/// Shapes checked planning facts into the public response data contract.
///
/// # Errors
///
/// Returns [`PlanChangeError::InvalidRisk`] for an unknown daemon risk label.
pub fn shape_plan_change(result: PlanChangeResult) -> Result<PlanChangeData, PlanChangeError> {
    Ok(PlanChangeData {
        plan: result.plan,
        affected_scope: PlanImpactSummary {
            affected_symbols: result.affected_scope.affected_symbols,
            affected_files: result.affected_scope.affected_files,
            risk_level: risk_level(&result.affected_scope.risk_level)?,
            touches_public_surface: result.affected_scope.touches_public_surface,
        },
        test_plan: result.test_plan,
        open_decisions: result.open_decisions,
        context_pack_request: result.context_pack_request,
        explanation: None,
    })
}

/// Builds source-free `plan.change` output for explain-only execution.
#[must_use]
pub fn explain_plan_change(
    request: &PlanChangeRequest,
    generation: GenerationId,
) -> PlanChangeData {
    let explanation = finalize_plan(
        plan_change_plan(request.max_steps, request.target_count()),
        &generation.to_string(),
    );
    explain_data(explanation)
}

const fn objective_label(objective: PlanObjective) -> &'static str {
    match objective {
        PlanObjective::BugFix => "bug_fix",
        PlanObjective::Refactor => "refactor",
        PlanObjective::Explanation => "explanation",
        PlanObjective::Migration => "migration",
        PlanObjective::Review => "review",
    }
}

fn risk_level(label: &str) -> Result<RiskLevel, PlanChangeError> {
    match label {
        "none" => Ok(RiskLevel::None),
        "low" => Ok(RiskLevel::Low),
        "medium" => Ok(RiskLevel::Medium),
        "high" => Ok(RiskLevel::High),
        "critical" => Ok(RiskLevel::Critical),
        _ => Err(PlanChangeError::InvalidRisk),
    }
}

fn explain_data(explanation: PlanExplanation) -> PlanChangeData {
    PlanChangeData {
        plan: vec![ChangePlanStep {
            step: 1,
            action: "explain_only_no_change_planning_executed".to_owned(),
            targets: Vec::new(),
            depends_on: Vec::new(),
            risks: Vec::new(),
            verification: None,
        }],
        affected_scope: PlanImpactSummary {
            affected_symbols: 0,
            affected_files: 0,
            risk_level: RiskLevel::None,
            touches_public_surface: false,
        },
        test_plan: Vec::new(),
        open_decisions: Vec::new(),
        context_pack_request: ContextPackRequest {
            symbols: Vec::new(),
            files: Vec::new(),
        },
        explanation: Some(explanation),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_mcp_contract::{
        RepositorySelector,
        change::{
            ContextPackRequest, PlanChangeInput, PlanFileTarget, PlanImpactSummary, PlanObjective,
            PlanSymbolTarget, PlanTargetSelector, RiskLevel,
        },
        vertical::{RepositoryIdSelector, ResponseProfile},
    };

    use super::{
        PlanChangeError, PlanChangeResult, PlanImpactResult, explain_plan_change,
        normalize_plan_change, shape_plan_change,
    };

    fn input() -> PlanChangeInput {
        PlanChangeInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: RepositoryId::from_bytes([1; 16]),
            }),
            generation: None,
            objective: PlanObjective::BugFix,
            objective_text: "repair request admission".to_owned(),
            targets: vec![
                PlanTargetSelector::Symbol(PlanSymbolTarget {
                    symbol_id: SymbolId::from_bytes([2; 20]),
                }),
                PlanTargetSelector::File(PlanFileTarget {
                    file_id: FileId::from_bytes([3; 20]),
                }),
            ],
            constraints: None,
            change_context: None,
            max_steps: Some(4),
            budget: None,
            profile: Some(ResponseProfile::Compact),
            explain: None,
        }
    }

    #[test]
    fn normalization_owns_profile_and_target_admission() {
        let request = normalize_plan_change(input()).expect("request is admitted");

        assert_eq!(request.objective(), "bug_fix");
        assert_eq!(request.target_symbols().len(), 1);
        assert_eq!(request.target_files().len(), 1);
        assert_eq!(request.target_count(), 2);
    }

    #[test]
    fn unsupported_profile_is_rejected_without_an_application() {
        let mut input = input();
        input.profile = Some(ResponseProfile::Evidence);

        assert_eq!(
            normalize_plan_change(input),
            Err(PlanChangeError::UnsupportedOption)
        );
    }

    #[test]
    fn result_shaping_maps_checked_risk_labels() {
        let data = shape_plan_change(PlanChangeResult {
            plan: Vec::new(),
            affected_scope: PlanImpactResult {
                affected_symbols: 2,
                affected_files: 1,
                risk_level: "high".to_owned(),
                touches_public_surface: true,
            },
            test_plan: Vec::new(),
            open_decisions: Vec::new(),
            context_pack_request: ContextPackRequest {
                symbols: Vec::new(),
                files: Vec::new(),
            },
        })
        .expect("known risk is shaped");

        assert_eq!(
            data.affected_scope,
            PlanImpactSummary {
                affected_symbols: 2,
                affected_files: 1,
                risk_level: RiskLevel::High,
                touches_public_surface: true,
            }
        );
    }

    #[test]
    fn unknown_risk_label_fails_closed() {
        let result = PlanChangeResult {
            plan: Vec::new(),
            affected_scope: PlanImpactResult {
                affected_symbols: 0,
                affected_files: 0,
                risk_level: "severe".to_owned(),
                touches_public_surface: false,
            },
            test_plan: Vec::new(),
            open_decisions: Vec::new(),
            context_pack_request: ContextPackRequest {
                symbols: Vec::new(),
                files: Vec::new(),
            },
        };

        assert_eq!(shape_plan_change(result), Err(PlanChangeError::InvalidRisk));
    }

    #[test]
    fn explain_output_is_schema_shaped_and_source_free() {
        let mut input = input();
        input.explain = Some(true);
        let request = normalize_plan_change(input).expect("request is admitted");

        let data = explain_plan_change(&request, GenerationId::from_bytes([4; 20]));

        assert_eq!(data.plan.len(), 1);
        assert_eq!(data.affected_scope.risk_level, RiskLevel::None);
        assert!(data.explanation.is_some());
        serde_json::to_value(data).expect("public plan data serializes");
    }
}
