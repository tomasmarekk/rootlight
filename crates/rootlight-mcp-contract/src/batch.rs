//! Canonical registry for tools admitted to bounded batch composition.
//!
//! Each public [`BatchTool`] has exactly one descriptor that binds its catalog
//! identity, schemas, exposure, response, budget, binding, and adapter policy.

use crate::catalog::{ExposureProfile, McpTool};
use crate::context::BatchTool;
use crate::vertical::{ResponseProfile, VerticalTool};

/// One independently enforced resource dimension for a batch child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatchBudgetDimension {
    /// Returned result objects.
    Results,
    /// Serialized response tokens.
    Tokens,
    /// Repository source bytes.
    SourceBytes,
    /// Examined relationship or traversal facts.
    TraversalFacts,
    /// Traversal or planning depth.
    Depth,
    /// Independently returned paths.
    Paths,
    /// Cooperative wall-clock deadline.
    Timeout,
}

/// Budget controls a batch adapter can transport to its standalone executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBudgetPolicy {
    /// Resource dimensions that a local child budget may reduce.
    pub locally_reducible: &'static [BatchBudgetDimension],
    /// Whether `evidence_level` is accepted in a child budget.
    pub evidence_level: bool,
}

/// Typed output fields from which a dependency binding may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatchBindingSource {
    /// One stable symbol identifier.
    SymbolId,
    /// A bounded collection of stable symbol identifiers.
    SymbolIds,
    /// One immutable generation-pinned source reference.
    SourceRef,
    /// A bounded collection of immutable source references.
    SourceRefs,
    /// A symbol definition source reference.
    Definition,
    /// A bounded collection of graph node identifiers.
    Nodes,
    /// One stable test identifier.
    TestId,
    /// One stable context-pack identifier.
    PackId,
}

/// How materialized binding values are admitted at the child input boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchBindingTargetPolicy {
    /// Accept only leaves proven compatible by the child's strict input schema.
    StrictInputSchemaLeaf,
}

/// Binding policy enforced for one batch adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBindingPolicy {
    /// Closed set of typed dependency-output leaves that may be read.
    pub sources: &'static [BatchBindingSource],
    /// Validation applied to the destination after materialization.
    pub target: BatchBindingTargetPolicy,
}

/// How a child response representation is selected inside a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchResponseProfilePolicy {
    /// The standalone tool always emits this representation.
    Fixed(ResponseProfile),
    /// The batch adapter may select from the tool's standalone representations.
    Selectable {
        /// Exact standalone input field used for profile selection.
        wire_field: &'static str,
        /// Closed set of accepted response representations.
        supported: &'static [ResponseProfile],
        /// Representation used when the selector is omitted.
        default: ResponseProfile,
    },
}

/// Authoritative contract descriptor for one public batch subtool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchToolDescriptor {
    /// Public batch enum member.
    pub batch_tool: BatchTool,
    /// Matching complete MCP catalog member.
    pub tool: McpTool,
    /// Matching schema and standalone execution adapter identity.
    pub adapter: VerticalTool,
    /// Public contract version shared with the standalone tool.
    pub contract_version: &'static str,
    /// Least-privileged exposure profile that may invoke the child.
    pub required_profile: ExposureProfile,
    /// Whether the child observes already published state without mutation.
    pub read_only: bool,
    /// Whether the descriptor is admitted by the current batch contract.
    pub eligible: bool,
    /// Child response-profile disposition.
    pub response_profiles: BatchResponseProfilePolicy,
    /// Child-local budget capabilities.
    pub budget: BatchBudgetPolicy,
    /// Dependency-binding admission policy.
    pub bindings: BatchBindingPolicy,
}

impl BatchToolDescriptor {
    /// Checked standalone input schema consumed by the batch adapter.
    #[must_use]
    pub const fn input_schema_json(self) -> &'static str {
        self.adapter.input_schema_json()
    }

    /// Checked standalone output schema produced by the batch adapter.
    #[must_use]
    pub const fn output_schema_json(self) -> &'static str {
        self.adapter.output_schema_json()
    }
}

const FULL_BUDGET: &[BatchBudgetDimension] = &[
    BatchBudgetDimension::Results,
    BatchBudgetDimension::Tokens,
    BatchBudgetDimension::SourceBytes,
    BatchBudgetDimension::TraversalFacts,
    BatchBudgetDimension::Depth,
    BatchBudgetDimension::Paths,
    BatchBudgetDimension::Timeout,
];
const CONTEXT_BUDGET: &[BatchBudgetDimension] =
    &[BatchBudgetDimension::Tokens, BatchBudgetDimension::Timeout];
const TYPED_BINDING_SOURCES: &[BatchBindingSource] = &[
    BatchBindingSource::SymbolId,
    BatchBindingSource::SymbolIds,
    BatchBindingSource::SourceRef,
    BatchBindingSource::SourceRefs,
    BatchBindingSource::Definition,
    BatchBindingSource::Nodes,
    BatchBindingSource::TestId,
    BatchBindingSource::PackId,
];
const COMPACT: &[ResponseProfile] = &[ResponseProfile::Compact];
const ANALYTICAL: &[ResponseProfile] = &[
    ResponseProfile::Compact,
    ResponseProfile::Standard,
    ResponseProfile::Evidence,
];

const FULL_BUDGET_POLICY: BatchBudgetPolicy = BatchBudgetPolicy {
    locally_reducible: FULL_BUDGET,
    evidence_level: false,
};
const CONTEXT_BUDGET_POLICY: BatchBudgetPolicy = BatchBudgetPolicy {
    locally_reducible: CONTEXT_BUDGET,
    evidence_level: false,
};
const TYPED_BINDING_POLICY: BatchBindingPolicy = BatchBindingPolicy {
    sources: TYPED_BINDING_SOURCES,
    target: BatchBindingTargetPolicy::StrictInputSchemaLeaf,
};

const fn selectable(
    wire_field: &'static str,
    supported: &'static [ResponseProfile],
) -> BatchResponseProfilePolicy {
    BatchResponseProfilePolicy::Selectable {
        wire_field,
        supported,
        default: ResponseProfile::Compact,
    }
}

const fn descriptor(
    batch_tool: BatchTool,
    tool: McpTool,
    adapter: VerticalTool,
    required_profile: ExposureProfile,
    response_profiles: BatchResponseProfilePolicy,
    budget: BatchBudgetPolicy,
) -> BatchToolDescriptor {
    BatchToolDescriptor {
        batch_tool,
        tool,
        adapter,
        contract_version: tool.contract_version(),
        required_profile,
        read_only: tool.read_only(),
        eligible: true,
        response_profiles,
        budget,
        bindings: TYPED_BINDING_POLICY,
    }
}

/// Canonical batch registry in the exact order of [`BatchTool::ALL`].
pub const BATCH_TOOL_REGISTRY: [BatchToolDescriptor; 12] = [
    descriptor(
        BatchTool::CodeLocate,
        McpTool::CodeLocate,
        VerticalTool::CodeLocate,
        ExposureProfile::Scout,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::SymbolExplain,
        McpTool::SymbolExplain,
        VerticalTool::SymbolExplain,
        ExposureProfile::Scout,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::SymbolRelationships,
        McpTool::SymbolRelationships,
        VerticalTool::SymbolRelationships,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::FlowTrace,
        McpTool::FlowTrace,
        VerticalTool::FlowTrace,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::ChangeImpact,
        McpTool::ChangeImpact,
        VerticalTool::ChangeImpact,
        ExposureProfile::Analysis,
        selectable("profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::TestsSelect,
        McpTool::TestsSelect,
        VerticalTool::TestsSelect,
        ExposureProfile::Analysis,
        selectable("profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::ArchitectureOverview,
        McpTool::ArchitectureOverview,
        VerticalTool::ArchitectureOverview,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::ArchitectureCycles,
        McpTool::ArchitectureCycles,
        VerticalTool::ArchitectureCycles,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::CodeDead,
        McpTool::CodeDead,
        VerticalTool::CodeDead,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::PlanChange,
        McpTool::PlanChange,
        VerticalTool::PlanChange,
        ExposureProfile::Developer,
        selectable("profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::ContextPack,
        McpTool::ContextPack,
        VerticalTool::ContextPack,
        ExposureProfile::Scout,
        BatchResponseProfilePolicy::Fixed(ResponseProfile::Compact),
        CONTEXT_BUDGET_POLICY,
    ),
    descriptor(
        BatchTool::SourceRead,
        McpTool::SourceRead,
        VerticalTool::SourceRead,
        ExposureProfile::Scout,
        selectable("response_profile", COMPACT),
        FULL_BUDGET_POLICY,
    ),
];

/// Returns the canonical descriptor for one public batch enum member.
#[must_use]
pub const fn batch_descriptor(tool: BatchTool) -> &'static BatchToolDescriptor {
    &BATCH_TOOL_REGISTRY[tool as usize]
}

/// Finds the canonical batch descriptor for one complete catalog tool.
#[must_use]
pub const fn batch_descriptor_for_tool(tool: McpTool) -> Option<&'static BatchToolDescriptor> {
    let mut index = 0;
    while index < BATCH_TOOL_REGISTRY.len() {
        let descriptor = &BATCH_TOOL_REGISTRY[index];
        if descriptor.tool as u8 == tool as u8 {
            return Some(descriptor);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use schemars::schema_for;
    use serde_json::Value;

    use super::{
        BATCH_TOOL_REGISTRY, BatchBindingSource, BatchBudgetDimension, BatchResponseProfilePolicy,
        batch_descriptor, batch_descriptor_for_tool,
    };
    use crate::capability::{ResponseProfileSupport, capability_for};
    use crate::context::BatchTool;
    use crate::vertical::ResponseProfile;

    #[test]
    fn registry_is_exhaustive_unique_and_ordered() {
        assert_eq!(BATCH_TOOL_REGISTRY.len(), BatchTool::ALL.len());
        let mut batch_tools = BTreeSet::new();
        let mut catalog_tools = BTreeSet::new();
        for (expected, descriptor) in BatchTool::ALL.iter().zip(BATCH_TOOL_REGISTRY) {
            assert_eq!(*expected, descriptor.batch_tool);
            assert_eq!(batch_descriptor(*expected), &descriptor);
            assert!(batch_tools.insert(descriptor.batch_tool));
            assert!(catalog_tools.insert(descriptor.tool));
            assert_eq!(descriptor.tool.name(), descriptor.batch_tool.name());
            assert_eq!(descriptor.adapter.name(), descriptor.batch_tool.name());
            assert_eq!(
                descriptor.contract_version,
                descriptor.tool.contract_version()
            );
            assert_eq!(
                descriptor.contract_version,
                descriptor.adapter.contract_version()
            );
            assert!(descriptor.read_only);
            assert!(descriptor.eligible);
            assert!(!descriptor.input_schema_json().is_empty());
            assert!(!descriptor.output_schema_json().is_empty());
        }
    }

    #[test]
    fn batch_schema_membership_matches_the_registry() {
        let schema =
            serde_json::to_value(schema_for!(BatchTool)).expect("batch enum schema serializes");
        let values = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .expect("batch enum remains a closed string enum");
        let actual: Vec<&str> = values
            .iter()
            .map(|value| {
                value
                    .get("const")
                    .and_then(Value::as_str)
                    .expect("batch enum variants declare string constants")
            })
            .collect();
        let expected: Vec<&str> = BATCH_TOOL_REGISTRY
            .iter()
            .map(|descriptor| descriptor.tool.name())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn exposure_and_response_profiles_match_catalog_capabilities() {
        for descriptor in BATCH_TOOL_REGISTRY {
            assert_eq!(
                batch_descriptor_for_tool(descriptor.tool),
                Some(batch_descriptor(descriptor.batch_tool))
            );
            let capability = capability_for(descriptor.tool);
            assert_eq!(
                capability.profiles.first(),
                Some(&descriptor.required_profile)
            );
            match (descriptor.response_profiles, capability.response_profiles) {
                (
                    BatchResponseProfilePolicy::Fixed(representation),
                    ResponseProfileSupport::Fixed {
                        representation: expected,
                    },
                ) => assert_eq!(representation, expected),
                (
                    BatchResponseProfilePolicy::Selectable {
                        wire_field,
                        supported,
                        default,
                    },
                    ResponseProfileSupport::Selectable {
                        wire_field: expected_field,
                        supported: expected_supported,
                        default: expected_default,
                    },
                ) => {
                    assert_eq!(wire_field, expected_field.name());
                    assert_eq!(supported, expected_supported);
                    assert_eq!(default, expected_default);
                }
                mismatch => panic!("batch response-profile mismatch: {mismatch:?}"),
            }
        }
    }

    #[test]
    fn budget_and_binding_policies_are_explicit_for_every_adapter() {
        for descriptor in BATCH_TOOL_REGISTRY {
            assert!(
                descriptor
                    .budget
                    .locally_reducible
                    .contains(&BatchBudgetDimension::Tokens)
            );
            assert!(
                descriptor
                    .budget
                    .locally_reducible
                    .contains(&BatchBudgetDimension::Timeout)
            );
            assert!(!descriptor.budget.evidence_level);
            assert!(
                descriptor
                    .bindings
                    .sources
                    .contains(&BatchBindingSource::SymbolId)
            );
            assert!(
                descriptor
                    .bindings
                    .sources
                    .contains(&BatchBindingSource::SourceRef)
            );
        }
        let context = batch_descriptor(BatchTool::ContextPack);
        assert_eq!(
            context.budget.locally_reducible,
            &[BatchBudgetDimension::Tokens, BatchBudgetDimension::Timeout]
        );
        let source = batch_descriptor(BatchTool::SourceRead);
        assert_eq!(
            source.response_profiles,
            BatchResponseProfilePolicy::Selectable {
                wire_field: "response_profile",
                supported: &[ResponseProfile::Compact],
                default: ResponseProfile::Compact,
            }
        );
    }
}
