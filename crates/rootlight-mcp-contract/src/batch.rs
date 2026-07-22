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

/// Stable semantic name of a reviewed dependency-output slot.
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

/// Runtime representation carried by a typed binding slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchBindingValueType {
    /// One stable symbol identifier.
    SymbolId,
    /// A bounded collection of stable symbol identifiers.
    SymbolIds,
    /// One generation-pinned source reference.
    SourceRef,
    /// A bounded collection of generation-pinned source references.
    SourceRefs,
    /// One bounded Rootlight-owned test identifier.
    TestId,
    /// One bounded Rootlight-owned context-pack identifier.
    PackId,
}

/// Cardinality contract for a source or destination binding slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchBindingCardinality {
    /// Exactly one non-null value.
    Scalar,
    /// A collection whose size is checked before child execution.
    Collection {
        /// Smallest accepted collection size.
        min: u16,
        /// Largest accepted collection size.
        max: u16,
    },
}

/// Trust class assigned after reviewing a structured output field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchBindingTrust {
    /// A Rootlight-validated stable identifier, safe as structured data.
    TypedIdentifier,
    /// A source reference cryptographically bound to repository content.
    GenerationPinnedReference,
    /// An opaque Rootlight-owned identifier with no command semantics.
    OpaqueRootlightIdentifier,
}

/// Relationship between a binding value and the batch's pinned identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchBindingGeneration {
    /// The value is valid only for the one repository and generation pinned by the batch.
    PinnedBatchIdentity,
}

/// One segment in a registry-reviewed compatibility path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchBindingPathSegment {
    /// Exact JSON object field.
    Field(&'static str),
    /// Bounded array index.
    Index {
        /// Exclusive upper bound for the index.
        max_exclusive: u16,
    },
}

/// One typed output slot exposed by a batch subtool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBindingSourceSlot {
    /// Stable semantic source name.
    pub source: BatchBindingSource,
    /// Compatibility path relative to the response `data` object.
    pub path: &'static [BatchBindingPathSegment],
    /// Runtime value representation.
    pub value_type: BatchBindingValueType,
    /// Declared output cardinality.
    pub cardinality: BatchBindingCardinality,
    /// Reviewed trust class.
    pub trust: BatchBindingTrust,
    /// Generation binding carried by the value.
    pub generation: BatchBindingGeneration,
    /// Whether a successful response may omit or null this slot.
    pub optional: bool,
    /// Whether the slot may cross into another child input.
    pub composable: bool,
}

/// One schema-compatible input slot that may receive a typed binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBindingTargetSlot {
    /// Destination path relative to child arguments.
    pub path: &'static [BatchBindingPathSegment],
    /// Required runtime value representation.
    pub value_type: BatchBindingValueType,
    /// Destination cardinality and collection bound.
    pub cardinality: BatchBindingCardinality,
    /// Trust classes permitted at this destination.
    pub accepted_trust: &'static [BatchBindingTrust],
}

/// Binding policy enforced for one batch adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBindingPolicy {
    /// Translator version applied to the compatibility wire form.
    pub translator_version: &'static str,
    /// Closed set of typed dependency-output leaves that may be read.
    pub sources: &'static [BatchBindingSourceSlot],
    /// Closed set of schema-compatible destinations that may receive bindings.
    pub targets: &'static [BatchBindingTargetSlot],
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

/// Version of the typed translator applied to the legacy-compatible wire form.
pub const BATCH_BINDING_TRANSLATOR_VERSION: &str = "1.0";

use BatchBindingCardinality::{Collection, Scalar};
use BatchBindingGeneration::PinnedBatchIdentity;
use BatchBindingPathSegment::{Field, Index};
use BatchBindingTrust::{GenerationPinnedReference, OpaqueRootlightIdentifier, TypedIdentifier};
use BatchBindingValueType::{
    PackId as PackIdValue, SourceRef as SourceRefValue, SourceRefs as SourceRefsValue,
    SymbolId as SymbolIdValue, SymbolIds as SymbolIdsValue, TestId as TestIdValue,
};

const IDENTITY_TRUST: &[BatchBindingTrust] = &[TypedIdentifier];
const SOURCE_TRUST: &[BatchBindingTrust] = &[GenerationPinnedReference];

const fn source_slot(
    source: BatchBindingSource,
    path: &'static [BatchBindingPathSegment],
    value_type: BatchBindingValueType,
    cardinality: BatchBindingCardinality,
    trust: BatchBindingTrust,
    optional: bool,
) -> BatchBindingSourceSlot {
    BatchBindingSourceSlot {
        source,
        path,
        value_type,
        cardinality,
        trust,
        generation: PinnedBatchIdentity,
        optional,
        composable: true,
    }
}

const fn informational_source_slot(
    source: BatchBindingSource,
    path: &'static [BatchBindingPathSegment],
    value_type: BatchBindingValueType,
    cardinality: BatchBindingCardinality,
    trust: BatchBindingTrust,
    optional: bool,
) -> BatchBindingSourceSlot {
    let mut slot = source_slot(source, path, value_type, cardinality, trust, optional);
    slot.composable = false;
    slot
}

const fn target_slot(
    path: &'static [BatchBindingPathSegment],
    value_type: BatchBindingValueType,
    cardinality: BatchBindingCardinality,
    accepted_trust: &'static [BatchBindingTrust],
) -> BatchBindingTargetSlot {
    BatchBindingTargetSlot {
        path,
        value_type,
        cardinality,
        accepted_trust,
    }
}

const CODE_LOCATE_SOURCES: &[BatchBindingSourceSlot] = &[
    source_slot(
        BatchBindingSource::SymbolId,
        &[
            Field("matches"),
            Index { max_exclusive: 200 },
            Field("symbol_id"),
        ],
        SymbolIdValue,
        Scalar,
        TypedIdentifier,
        true,
    ),
    source_slot(
        BatchBindingSource::SourceRef,
        &[
            Field("matches"),
            Index { max_exclusive: 200 },
            Field("source_ref"),
        ],
        SourceRefValue,
        Scalar,
        GenerationPinnedReference,
        true,
    ),
];
const SYMBOL_EXPLAIN_SOURCES: &[BatchBindingSourceSlot] = &[
    source_slot(
        BatchBindingSource::SymbolId,
        &[
            Field("symbols"),
            Index { max_exclusive: 16 },
            Field("symbol_id"),
        ],
        SymbolIdValue,
        Scalar,
        TypedIdentifier,
        false,
    ),
    source_slot(
        BatchBindingSource::Definition,
        &[
            Field("symbols"),
            Index { max_exclusive: 16 },
            Field("definition"),
        ],
        SourceRefValue,
        Scalar,
        GenerationPinnedReference,
        false,
    ),
];
const FLOW_TRACE_SOURCES: &[BatchBindingSourceSlot] = &[source_slot(
    BatchBindingSource::Nodes,
    &[Field("paths"), Index { max_exclusive: 100 }, Field("nodes")],
    SymbolIdsValue,
    Collection { min: 2, max: 9 },
    TypedIdentifier,
    false,
)];
const TESTS_SELECT_SOURCES: &[BatchBindingSourceSlot] = &[informational_source_slot(
    BatchBindingSource::TestId,
    &[
        Field("tests"),
        Index { max_exclusive: 500 },
        Field("test_id"),
    ],
    TestIdValue,
    Scalar,
    OpaqueRootlightIdentifier,
    false,
)];
const CODE_DEAD_SOURCES: &[BatchBindingSourceSlot] = &[
    source_slot(
        BatchBindingSource::SymbolId,
        &[
            Field("candidates"),
            Index { max_exclusive: 500 },
            Field("symbol_id"),
        ],
        SymbolIdValue,
        Scalar,
        TypedIdentifier,
        false,
    ),
    informational_source_slot(
        BatchBindingSource::SourceRefs,
        &[
            Field("candidates"),
            Index { max_exclusive: 500 },
            Field("source_refs"),
        ],
        SourceRefsValue,
        Collection { min: 0, max: 8 },
        GenerationPinnedReference,
        false,
    ),
];
const PLAN_CHANGE_SOURCES: &[BatchBindingSourceSlot] = &[source_slot(
    BatchBindingSource::SymbolIds,
    &[
        Field("plan"),
        Index { max_exclusive: 100 },
        Field("targets"),
    ],
    SymbolIdsValue,
    Collection { min: 0, max: 32 },
    TypedIdentifier,
    false,
)];
const CONTEXT_PACK_SOURCES: &[BatchBindingSourceSlot] = &[
    informational_source_slot(
        BatchBindingSource::PackId,
        &[Field("pack_id")],
        PackIdValue,
        Scalar,
        OpaqueRootlightIdentifier,
        false,
    ),
    source_slot(
        BatchBindingSource::SymbolId,
        &[
            Field("items"),
            Index { max_exclusive: 200 },
            Field("symbol_id"),
        ],
        SymbolIdValue,
        Scalar,
        TypedIdentifier,
        true,
    ),
    source_slot(
        BatchBindingSource::SourceRef,
        &[
            Field("items"),
            Index { max_exclusive: 200 },
            Field("source_ref"),
        ],
        SourceRefValue,
        Scalar,
        GenerationPinnedReference,
        true,
    ),
];
const SOURCE_READ_SOURCES: &[BatchBindingSourceSlot] = &[source_slot(
    BatchBindingSource::SourceRef,
    &[
        Field("chunks"),
        Index { max_exclusive: 32 },
        Field("source_ref"),
    ],
    SourceRefValue,
    Scalar,
    GenerationPinnedReference,
    false,
)];

const SYMBOL_SCOPE_TARGET: BatchBindingTargetSlot = target_slot(
    &[Field("scope"), Field("symbols")],
    SymbolIdsValue,
    Collection { min: 1, max: 64 },
    IDENTITY_TRUST,
);
const CODE_LOCATE_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[Field("related_to")],
        SymbolIdsValue,
        Collection { min: 1, max: 16 },
        IDENTITY_TRUST,
    ),
    SYMBOL_SCOPE_TARGET,
];
const SYMBOL_EXPLAIN_TARGETS: &[BatchBindingTargetSlot] = &[target_slot(
    &[Field("symbol_ids")],
    SymbolIdsValue,
    Collection { min: 1, max: 16 },
    IDENTITY_TRUST,
)];
const SYMBOL_RELATIONSHIPS_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[Field("symbol_ids")],
        SymbolIdsValue,
        Collection { min: 1, max: 64 },
        IDENTITY_TRUST,
    ),
    SYMBOL_SCOPE_TARGET,
];
const FLOW_TRACE_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[Field("from"), Field("symbol_id")],
        SymbolIdValue,
        Scalar,
        IDENTITY_TRUST,
    ),
    target_slot(
        &[Field("to"), Field("symbol_id")],
        SymbolIdValue,
        Scalar,
        IDENTITY_TRUST,
    ),
];
const CHANGE_IMPACT_TARGETS: &[BatchBindingTargetSlot] = &[target_slot(
    &[Field("change"), Field("symbol_ids")],
    SymbolIdsValue,
    Collection { min: 1, max: 256 },
    IDENTITY_TRUST,
)];
const TESTS_SELECT_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[Field("seeds"), Field("symbols")],
        SymbolIdsValue,
        Collection { min: 1, max: 64 },
        IDENTITY_TRUST,
    ),
    target_slot(
        &[Field("seeds"), Field("change"), Field("symbol_ids")],
        SymbolIdsValue,
        Collection { min: 1, max: 256 },
        IDENTITY_TRUST,
    ),
];
const SYMBOL_SCOPE_TARGETS: &[BatchBindingTargetSlot] = &[SYMBOL_SCOPE_TARGET];
const PLAN_CHANGE_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[
            Field("targets"),
            Index { max_exclusive: 64 },
            Field("symbol_id"),
        ],
        SymbolIdValue,
        Scalar,
        IDENTITY_TRUST,
    ),
    target_slot(
        &[Field("change_context"), Field("symbol_ids")],
        SymbolIdsValue,
        Collection { min: 1, max: 256 },
        IDENTITY_TRUST,
    ),
];
const CONTEXT_PACK_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[Field("seeds"), Field("symbols")],
        SymbolIdsValue,
        Collection { min: 0, max: 32 },
        IDENTITY_TRUST,
    ),
    target_slot(
        &[Field("seeds"), Field("tests")],
        SymbolIdsValue,
        Collection { min: 0, max: 32 },
        IDENTITY_TRUST,
    ),
];
const SOURCE_READ_TARGETS: &[BatchBindingTargetSlot] = &[
    target_slot(
        &[
            Field("references"),
            Index { max_exclusive: 32 },
            Field("source_ref"),
        ],
        SourceRefValue,
        Scalar,
        SOURCE_TRUST,
    ),
    target_slot(
        &[
            Field("references"),
            Index { max_exclusive: 32 },
            Field("symbol_id"),
        ],
        SymbolIdValue,
        Scalar,
        IDENTITY_TRUST,
    ),
];

const fn binding_policy(
    sources: &'static [BatchBindingSourceSlot],
    targets: &'static [BatchBindingTargetSlot],
) -> BatchBindingPolicy {
    BatchBindingPolicy {
        translator_version: BATCH_BINDING_TRANSLATOR_VERSION,
        sources,
        targets,
    }
}

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
    bindings: BatchBindingPolicy,
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
        bindings,
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
        binding_policy(CODE_LOCATE_SOURCES, CODE_LOCATE_TARGETS),
    ),
    descriptor(
        BatchTool::SymbolExplain,
        McpTool::SymbolExplain,
        VerticalTool::SymbolExplain,
        ExposureProfile::Scout,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(SYMBOL_EXPLAIN_SOURCES, SYMBOL_EXPLAIN_TARGETS),
    ),
    descriptor(
        BatchTool::SymbolRelationships,
        McpTool::SymbolRelationships,
        VerticalTool::SymbolRelationships,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(&[], SYMBOL_RELATIONSHIPS_TARGETS),
    ),
    descriptor(
        BatchTool::FlowTrace,
        McpTool::FlowTrace,
        VerticalTool::FlowTrace,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(FLOW_TRACE_SOURCES, FLOW_TRACE_TARGETS),
    ),
    descriptor(
        BatchTool::ChangeImpact,
        McpTool::ChangeImpact,
        VerticalTool::ChangeImpact,
        ExposureProfile::Analysis,
        selectable("profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(&[], CHANGE_IMPACT_TARGETS),
    ),
    descriptor(
        BatchTool::TestsSelect,
        McpTool::TestsSelect,
        VerticalTool::TestsSelect,
        ExposureProfile::Analysis,
        selectable("profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(TESTS_SELECT_SOURCES, TESTS_SELECT_TARGETS),
    ),
    descriptor(
        BatchTool::ArchitectureOverview,
        McpTool::ArchitectureOverview,
        VerticalTool::ArchitectureOverview,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(&[], SYMBOL_SCOPE_TARGETS),
    ),
    descriptor(
        BatchTool::ArchitectureCycles,
        McpTool::ArchitectureCycles,
        VerticalTool::ArchitectureCycles,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(&[], SYMBOL_SCOPE_TARGETS),
    ),
    descriptor(
        BatchTool::CodeDead,
        McpTool::CodeDead,
        VerticalTool::CodeDead,
        ExposureProfile::Analysis,
        selectable("response_profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(CODE_DEAD_SOURCES, SYMBOL_SCOPE_TARGETS),
    ),
    descriptor(
        BatchTool::PlanChange,
        McpTool::PlanChange,
        VerticalTool::PlanChange,
        ExposureProfile::Developer,
        selectable("profile", ANALYTICAL),
        FULL_BUDGET_POLICY,
        binding_policy(PLAN_CHANGE_SOURCES, PLAN_CHANGE_TARGETS),
    ),
    descriptor(
        BatchTool::ContextPack,
        McpTool::ContextPack,
        VerticalTool::ContextPack,
        ExposureProfile::Scout,
        BatchResponseProfilePolicy::Fixed(ResponseProfile::Compact),
        CONTEXT_BUDGET_POLICY,
        binding_policy(CONTEXT_PACK_SOURCES, CONTEXT_PACK_TARGETS),
    ),
    descriptor(
        BatchTool::SourceRead,
        McpTool::SourceRead,
        VerticalTool::SourceRead,
        ExposureProfile::Scout,
        selectable("response_profile", COMPACT),
        FULL_BUDGET_POLICY,
        binding_policy(SOURCE_READ_SOURCES, SOURCE_READ_TARGETS),
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
        BATCH_BINDING_TRANSLATOR_VERSION, BATCH_TOOL_REGISTRY, BatchBindingSource,
        BatchBudgetDimension, BatchResponseProfilePolicy, batch_descriptor,
        batch_descriptor_for_tool,
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
        let mut declared_sources = BTreeSet::new();
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
            assert_eq!(
                descriptor.bindings.translator_version,
                BATCH_BINDING_TRANSLATOR_VERSION
            );
            for source in descriptor.bindings.sources {
                assert!(!source.path.is_empty());
                declared_sources.insert(source.source);
            }
            for target in descriptor.bindings.targets {
                assert!(!target.path.is_empty());
                assert!(!target.accepted_trust.is_empty());
            }
        }
        assert_eq!(
            declared_sources,
            BTreeSet::from([
                BatchBindingSource::SymbolId,
                BatchBindingSource::SymbolIds,
                BatchBindingSource::SourceRef,
                BatchBindingSource::SourceRefs,
                BatchBindingSource::Definition,
                BatchBindingSource::Nodes,
                BatchBindingSource::TestId,
                BatchBindingSource::PackId,
            ])
        );
        assert!(
            BATCH_TOOL_REGISTRY
                .iter()
                .flat_map(|descriptor| descriptor.bindings.sources)
                .any(|source| source.composable)
        );
        assert!(
            BATCH_TOOL_REGISTRY
                .iter()
                .flat_map(|descriptor| descriptor.bindings.sources)
                .filter(|source| {
                    matches!(
                        source.source,
                        BatchBindingSource::SourceRefs
                            | BatchBindingSource::TestId
                            | BatchBindingSource::PackId
                    )
                })
                .all(|source| !source.composable)
        );
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
