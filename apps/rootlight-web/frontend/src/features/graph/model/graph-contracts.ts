// Validates source-free graph pages before the renderer allocates typed arrays.

export type GraphView = "architecture" | "files" | "symbols" | "neighborhood";
export type GraphBudgetProfile = "compact" | "balanced" | "expanded";
export type GraphNodeKind = "file" | "symbol" | "unknown";
export type GraphNodeIdKind = "file" | "symbol" | "unknown";
export type GraphEvidenceClass = "structural" | "aggregated" | "candidate" | "unknown";
export type GraphRelationKind =
  | "calls"
  | "called_by"
  | "references"
  | "types"
  | "implements"
  | "imports"
  | "tests"
  | "ownership"
  | "service_call"
  | "calls_route"
  | "messaging"
  | "reads_table"
  | "writes_table"
  | "build_dependency"
  | "data_flow"
  | "history"
  | "unknown";
export type GraphCompletenessState =
  "complete" | "truncated" | "unsupported_partial" | "indeterminate";
export type GraphContinuationAvailability = "not_applicable" | "available" | "unavailable";
export type GraphGuidance =
  | "use_cursor"
  | "narrow_scope"
  | "split_request"
  | "reduce_depth"
  | "reduce_relations"
  | "request_source"
  | "increase_budget_within_limit"
  | "refresh_coverage"
  | "unsupported_no_continuation"
  | "unknown";
export type GraphLimitingResourceKind =
  | "rows"
  | "edges"
  | "results"
  | "depth"
  | "paths"
  | "source_bytes"
  | "response_bytes"
  | "memory_bytes"
  | "deadline"
  | "estimated_tokens"
  | "cancellation"
  | "capability"
  | "coverage"
  | "page_size"
  | "unknown";

export type GraphProjectionOpenRequest = {
  repositoryId: string;
  generationId: string;
  view: GraphView;
  symbolIds?: string[];
  relations?: Exclude<GraphRelationKind, "unknown">[];
  minConfidence: number;
  budgetProfile: GraphBudgetProfile;
};

export type BrowserGraphContext = {
  repositoryId: string;
  generationId: string;
  parentGenerationId: string | null;
  activeGeneration: boolean;
  structuralFreshness: "current" | "stale" | "superseded" | "unknown";
  semanticFreshness: "current" | "stale" | "superseded" | "unknown";
  tier: "tier_a" | "tier_b" | "tier_c" | "tier_d" | "unknown";
  coverageStatus: "complete" | "bounded" | "sampled" | "unknown";
  skippedInputs: string;
};

export type BrowserGraphNode = {
  ordinal: number;
  stableId: string;
  idKind: GraphNodeIdKind;
  label: string;
  path: string | null;
  kind: GraphNodeKind;
  confidence: number;
  generated: boolean | null;
  community: string | null;
  component: string | null;
  symbolCount: number | null;
  fanIn: number | null;
  fanOut: number | null;
  hotspotScore: number | null;
  evidence: GraphEvidenceClass;
};

export type BrowserGraphEdge = {
  sourceOrdinal: number;
  targetOrdinal: number;
  relation: GraphRelationKind;
  weight: number;
  confidence: number;
  exact: boolean;
  inferred: boolean;
  evidenceCount: number;
  overlay: "none" | "unknown";
};

export type BrowserGraphCompleteness = {
  state: GraphCompletenessState;
  limitingResources: {
    kind: GraphLimitingResourceKind;
    limit: string | null;
    observed: string | null;
  }[];
  continuation: GraphContinuationAvailability;
  guidance: GraphGuidance[];
};

export type BrowserGraphPage = {
  schema: "rootlight.web-graph-page/1";
  projectionToken: string;
  pageOrdinal: number;
  context: BrowserGraphContext;
  nodes: BrowserGraphNode[];
  edges: BrowserGraphEdge[];
  completeness: BrowserGraphCompleteness;
  effectiveBudget: {
    pageNodes: number;
    pageEdges: number;
    aggregateNodes: number;
    aggregateEdges: number;
  };
  returnedNodesCumulative: string;
  returnedEdgesCumulative: string;
  totalMatchingNodes: string;
  totalMatchingEdges: string;
  totalKnownNodes: string | null;
  totalKnownEdges: string | null;
  edgesOmittedForUnavailableEndpoints: string;
  skippedForCoverage: string;
  hasNextPage: boolean;
};

export type BrowserGraphRelease = {
  schema: "rootlight.web-graph-release/1";
  released: boolean;
};

const opaqueTokenPattern = /^[A-Za-z0-9_-]{43}$/u;
const stableRepositoryPattern = /^repo1_[a-z2-7]{32}$/u;
const stableGenerationPattern = /^gen1_[a-z2-7]{39}$/u;
const decimalPattern = /^(0|[1-9][0-9]*)$/u;
const freshnessValues = new Set(["current", "stale", "superseded"]);
const tierValues = new Set(["tier_a", "tier_b", "tier_c", "tier_d"]);
const coverageValues = new Set(["complete", "bounded", "sampled"]);
const nodeKindValues = new Set(["file", "symbol"]);
const evidenceValues = new Set(["structural", "aggregated", "candidate"]);
const relationValues = new Set([
  "calls",
  "called_by",
  "references",
  "types",
  "implements",
  "imports",
  "tests",
  "ownership",
  "service_call",
  "calls_route",
  "messaging",
  "reads_table",
  "writes_table",
  "build_dependency",
  "data_flow",
  "history",
]);
const completenessValues = new Set([
  "complete",
  "truncated",
  "unsupported_partial",
  "indeterminate",
]);
const continuationValues = new Set(["not_applicable", "available", "unavailable"]);
const guidanceValues = new Set([
  "use_cursor",
  "narrow_scope",
  "split_request",
  "reduce_depth",
  "reduce_relations",
  "request_source",
  "increase_budget_within_limit",
  "refresh_coverage",
  "unsupported_no_continuation",
]);
const limitingResourceValues = new Set([
  "rows",
  "edges",
  "results",
  "depth",
  "paths",
  "source_bytes",
  "response_bytes",
  "memory_bytes",
  "deadline",
  "estimated_tokens",
  "cancellation",
  "capability",
  "coverage",
  "page_size",
]);

export function parseBrowserGraphPage(
  value: unknown,
  expectedRepositoryId: string,
  expectedGenerationId: string,
  expectedProjectionToken?: string,
): BrowserGraphPage {
  const record = asRecord(value);
  const context = parseContext(record.context);
  if (
    context.repositoryId !== expectedRepositoryId ||
    context.generationId !== expectedGenerationId
  ) {
    throw new Error("Graph page does not match the requested immutable generation");
  }
  const projectionToken = asOpaqueToken(record.projectionToken);
  if (expectedProjectionToken !== undefined && projectionToken !== expectedProjectionToken) {
    throw new Error("Graph page does not match the retained projection");
  }
  const nodes = asArray(record.nodes, 200).map(parseNode);
  const edges = asArray(record.edges, 500).map(parseEdge);
  validateNodeOrdinals(nodes);
  const returnedNodesCumulative = asDecimal(record.returnedNodesCumulative);
  const returnedEdgesCumulative = asDecimal(record.returnedEdgesCumulative);
  const totalMatchingNodes = asDecimal(record.totalMatchingNodes);
  const totalMatchingEdges = asDecimal(record.totalMatchingEdges);
  const completeness = parseCompleteness(record.completeness);
  const effectiveBudget = parseBudget(record.effectiveBudget);
  const hasNextPage = asBoolean(record.hasNextPage);

  if (
    BigInt(returnedNodesCumulative) < BigInt(nodes.length) ||
    BigInt(returnedEdgesCumulative) < BigInt(edges.length) ||
    BigInt(totalMatchingNodes) < BigInt(returnedNodesCumulative) ||
    BigInt(totalMatchingEdges) < BigInt(returnedEdgesCumulative) ||
    nodes.length > effectiveBudget.pageNodes ||
    edges.length > effectiveBudget.pageEdges ||
    hasNextPage !== (completeness.continuation === "available")
  ) {
    throw new Error("Graph page counters or continuation are inconsistent");
  }
  const returnedNodeBound = BigInt(returnedNodesCumulative);
  for (const edge of edges) {
    if (
      BigInt(edge.sourceOrdinal) >= returnedNodeBound ||
      BigInt(edge.targetOrdinal) >= returnedNodeBound
    ) {
      throw new Error("Graph edge references an unavailable node ordinal");
    }
  }

  return {
    schema: asLiteral(record.schema, "rootlight.web-graph-page/1"),
    projectionToken,
    pageOrdinal: asInteger(record.pageOrdinal, 0, 10_000),
    context,
    nodes,
    edges,
    completeness,
    effectiveBudget,
    returnedNodesCumulative,
    returnedEdgesCumulative,
    totalMatchingNodes,
    totalMatchingEdges,
    totalKnownNodes: asOptionalDecimal(record.totalKnownNodes),
    totalKnownEdges: asOptionalDecimal(record.totalKnownEdges),
    edgesOmittedForUnavailableEndpoints: asDecimal(record.edgesOmittedForUnavailableEndpoints),
    skippedForCoverage: asDecimal(record.skippedForCoverage),
    hasNextPage,
  };
}

export function parseBrowserGraphRelease(value: unknown): BrowserGraphRelease {
  const record = asRecord(value);
  return {
    schema: asLiteral(record.schema, "rootlight.web-graph-release/1"),
    released: asBoolean(record.released),
  };
}

function parseContext(value: unknown): BrowserGraphContext {
  const record = asRecord(value);
  return {
    repositoryId: asPattern(record.repositoryId, stableRepositoryPattern),
    generationId: asPattern(record.generationId, stableGenerationPattern),
    parentGenerationId: asOptionalPattern(record.parentGenerationId, stableGenerationPattern),
    activeGeneration: asBoolean(record.activeGeneration),
    structuralFreshness: asEnumOrUnknown(
      record.structuralFreshness,
      freshnessValues,
    ) as BrowserGraphContext["structuralFreshness"],
    semanticFreshness: asEnumOrUnknown(
      record.semanticFreshness,
      freshnessValues,
    ) as BrowserGraphContext["semanticFreshness"],
    tier: asEnumOrUnknown(record.tier, tierValues) as BrowserGraphContext["tier"],
    coverageStatus: asEnumOrUnknown(
      record.coverageStatus,
      coverageValues,
    ) as BrowserGraphContext["coverageStatus"],
    skippedInputs: asDecimal(record.skippedInputs),
  };
}

function parseNode(value: unknown): BrowserGraphNode {
  const record = asRecord(value);
  return {
    ordinal: asInteger(record.ordinal, 0, 0xffff_ffff),
    stableId: asText(record.stableId, 512),
    idKind: asEnumOrUnknown(record.idKind, nodeKindValues) as GraphNodeIdKind,
    label: asText(record.label, 1_024),
    path: asOptionalText(record.path, 1_024),
    kind: asEnumOrUnknown(record.kind, nodeKindValues) as GraphNodeKind,
    confidence: asInteger(record.confidence, 0, 1_000),
    generated: asOptionalBoolean(record.generated),
    community: asOptionalText(record.community, 1_024),
    component: asOptionalText(record.component, 1_024),
    symbolCount: asOptionalInteger(record.symbolCount, 0, 0xffff_ffff),
    fanIn: asOptionalInteger(record.fanIn, 0, 0xffff_ffff),
    fanOut: asOptionalInteger(record.fanOut, 0, 0xffff_ffff),
    hotspotScore: asOptionalInteger(record.hotspotScore, 0, 0xffff_ffff),
    evidence: asEnumOrUnknown(record.evidence, evidenceValues) as GraphEvidenceClass,
  };
}

function parseEdge(value: unknown): BrowserGraphEdge {
  const record = asRecord(value);
  return {
    sourceOrdinal: asInteger(record.sourceOrdinal, 0, 0xffff_ffff),
    targetOrdinal: asInteger(record.targetOrdinal, 0, 0xffff_ffff),
    relation: asEnumOrUnknown(record.relation, relationValues) as GraphRelationKind,
    weight: asInteger(record.weight, 1, 0xffff_ffff),
    confidence: asInteger(record.confidence, 0, 1_000),
    exact: asBoolean(record.exact),
    inferred: asBoolean(record.inferred),
    evidenceCount: asInteger(record.evidenceCount, 1, 0xffff_ffff),
    overlay: asEnumOrUnknown(record.overlay, new Set(["none"])) as "none" | "unknown",
  };
}

function parseCompleteness(value: unknown): BrowserGraphCompleteness {
  const record = asRecord(value);
  return {
    state: asClosedEnum(record.state, completenessValues) as GraphCompletenessState,
    limitingResources: asArray(record.limitingResources, 16).map((resource) => {
      const item = asRecord(resource);
      return {
        kind: asEnumOrUnknown(item.kind, limitingResourceValues) as GraphLimitingResourceKind,
        limit: asOptionalDecimal(item.limit),
        observed: asOptionalDecimal(item.observed),
      };
    }),
    continuation: asClosedEnum(
      record.continuation,
      continuationValues,
    ) as GraphContinuationAvailability,
    guidance: asArray(record.guidance, 16).map(
      (guidance) => asEnumOrUnknown(guidance, guidanceValues) as GraphGuidance,
    ),
  };
}

function parseBudget(value: unknown): BrowserGraphPage["effectiveBudget"] {
  const record = asRecord(value);
  const pageNodes = asInteger(record.pageNodes, 1, 200);
  const pageEdges = asInteger(record.pageEdges, 1, 500);
  const aggregateNodes = asInteger(record.aggregateNodes, pageNodes, 512);
  const aggregateEdges = asInteger(record.aggregateEdges, pageEdges, 2_048);
  return { pageNodes, pageEdges, aggregateNodes, aggregateEdges };
}

function validateNodeOrdinals(nodes: BrowserGraphNode[]) {
  for (let index = 1; index < nodes.length; index += 1) {
    const previous = nodes[index - 1];
    const current = nodes[index];
    if (previous === undefined || current === undefined || previous.ordinal >= current.ordinal) {
      throw new Error("Graph node ordinals are not strictly increasing");
    }
  }
}

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("Expected an object");
  }
  return value as Record<string, unknown>;
}

function asArray(value: unknown, maximum: number): unknown[] {
  if (!Array.isArray(value) || value.length > maximum) {
    throw new Error("Expected a bounded array");
  }
  return value;
}

function asText(value: unknown, maximum: number): string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > maximum ||
    Array.from(value).some((character) => {
      const codePoint = character.codePointAt(0);
      return codePoint !== undefined && (codePoint <= 31 || codePoint === 127);
    })
  ) {
    throw new Error("Expected bounded safe text");
  }
  return value;
}

function asOptionalText(value: unknown, maximum: number): string | null {
  return value === null ? null : asText(value, maximum);
}

function asBoolean(value: unknown): boolean {
  if (typeof value !== "boolean") {
    throw new Error("Expected a boolean");
  }
  return value;
}

function asOptionalBoolean(value: unknown): boolean | null {
  return value === null ? null : asBoolean(value);
}

function asInteger(value: unknown, minimum: number, maximum: number): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < minimum ||
    value > maximum
  ) {
    throw new Error("Expected a bounded integer");
  }
  return value;
}

function asOptionalInteger(value: unknown, minimum: number, maximum: number): number | null {
  return value === null ? null : asInteger(value, minimum, maximum);
}

function asDecimal(value: unknown): string {
  if (typeof value !== "string" || value.length > 20 || !decimalPattern.test(value)) {
    throw new Error("Expected a canonical decimal string");
  }
  return value;
}

function asOptionalDecimal(value: unknown): string | null {
  return value === null ? null : asDecimal(value);
}

function asPattern(value: unknown, pattern: RegExp): string {
  if (typeof value !== "string" || !pattern.test(value)) {
    throw new Error("Expected a canonical identifier");
  }
  return value;
}

function asOptionalPattern(value: unknown, pattern: RegExp): string | null {
  return value === null ? null : asPattern(value, pattern);
}

function asOpaqueToken(value: unknown): string {
  return asPattern(value, opaqueTokenPattern);
}

function asLiteral<Value extends string>(value: unknown, expected: Value): Value {
  if (value !== expected) {
    throw new Error("Unexpected schema");
  }
  return expected;
}

function asEnumOrUnknown(value: unknown, accepted: ReadonlySet<string>): string {
  if (typeof value !== "string") {
    throw new Error("Expected an enum string");
  }
  return accepted.has(value) ? value : "unknown";
}

function asClosedEnum(value: unknown, accepted: ReadonlySet<string>): string {
  if (typeof value !== "string" || !accepted.has(value)) {
    throw new Error("Expected a supported enum value");
  }
  return value;
}
