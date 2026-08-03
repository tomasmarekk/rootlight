// Validates generation-bound evidence and explicit source responses before UI state observes them.

export type EvidenceFreshness = "current" | "stale" | "superseded" | "unknown";
export type EvidenceTier = "tier_a" | "tier_b" | "tier_c" | "tier_d" | "unknown";
export type EvidenceCoverage = "complete" | "bounded" | "sampled" | "unknown";
export type EvidenceCompletenessState =
  "complete" | "truncated" | "unsupported_partial" | "indeterminate";
export type EvidenceContinuation = "not_applicable" | "available" | "unavailable";

export type EvidenceUsage = {
  rows: string;
  edges: string;
  results: string;
  sourceBytes: string;
  jsonBytes: string;
  estimatedTokens: string;
  tokenAccountingProfile: "utf8_byte_upper_bound_v1" | null;
  memoryBytes: string | null;
  elapsedMicros: string;
};

export type EvidenceContext = {
  repositoryId: string;
  generationId: string;
  parentGenerationId: string | null;
  activeGeneration: boolean;
  structuralFreshness: EvidenceFreshness;
  semanticFreshness: EvidenceFreshness;
  tier: EvidenceTier;
  coverageStatus: EvidenceCoverage;
  skippedInputs: string;
  usage: EvidenceUsage;
};

export type EvidenceCompleteness = {
  state: EvidenceCompletenessState;
  limitingResources: {
    kind: string;
    limit: string | null;
    observed: string | null;
  }[];
  continuation: EvidenceContinuation;
  guidance: string[];
};

export type SourceCapability = {
  capability: string;
  expiresInSeconds: number;
};

export type NodeDetail = {
  schema: "rootlight.web-node-detail/1";
  repositoryId: string;
  generationId: string;
  nodeId: string;
  idKind: "symbol";
  kind: string;
  displayName: string;
  qualifiedName: string | null;
  signature: string | null;
  language: string;
  tier: EvidenceTier;
  confidence: number;
  provider: string;
  evidence: string;
  outboundExact: string;
  outboundCandidates: string;
  inboundExact: string;
  inboundCandidates: string;
  referenceCount: string;
  generated: boolean | null;
  sourceReferences: SourceCapability[];
  context: EvidenceContext;
  completeness: EvidenceCompleteness;
};

export type RelationshipTarget = {
  symbolId: string;
  confidence: number;
  sourceReferences: SourceCapability[];
};

export type RelationshipGroup = {
  seedId: string;
  relation: string;
  direction: "inbound" | "outbound" | "both";
  totalCount: string;
  targets: RelationshipTarget[];
};

export type Relationships = {
  schema: "rootlight.web-relationships/1";
  context: EvidenceContext;
  groups: RelationshipGroup[];
  returnedEdges: string;
  totalEdges: string;
  exact: boolean;
  truncated: boolean;
  nextPageOffset: string | null;
  completeness: EvidenceCompleteness;
};

export type SourceChunk = {
  fileId: string;
  path: string;
  requestedStartByte: string;
  requestedEndByte: string;
  includedStartByte: string;
  includedEndByte: string;
  includedStartLine: string | null;
  includedEndLine: string | null;
  content: string;
  encoding: "utf8" | "base64";
  contentHash: string;
  language: string;
  tier: EvidenceTier;
  generated: boolean;
};

export type SourceRead = {
  schema: "rootlight.web-source/1";
  repositoryId: string;
  generationId: string;
  chunks: SourceChunk[];
  totalSourceBytes: string;
  truncated: boolean;
  context: EvidenceContext;
  completeness: EvidenceCompleteness;
};

export type ImpactResolvedChange = {
  symbolId: string | null;
  fileId: string | null;
  classification: string;
  kind: string | null;
};

export type ImpactEntry = {
  symbolId: string;
  kind: string;
  distance: number;
  confidence: number;
  via: string[];
  isPublic: boolean;
};

export type ImpactGroup = {
  sourceIndex: number;
  dependents: ImpactEntry[];
};

export type ChangeImpact = {
  schema: "rootlight.web-change-impact/1";
  context: EvidenceContext;
  resolvedChanges: ImpactResolvedChange[];
  impacted: ImpactGroup[];
  tests: {
    testId: string;
    relevance: number;
    why: string[];
    estimatedCostMs: number | null;
  }[];
  riskSummary: {
    level: string;
    reasons: string[];
    coverage: string;
    breakingSurface: boolean;
    fanout: number;
    dynamicBlindSpots: boolean;
  };
  completeness: EvidenceCompleteness;
};

const repositoryPattern = /^repo1_[a-z2-7]{32}$/u;
const generationPattern = /^gen1_[a-z2-7]{39}$/u;
const symbolPattern = /^sym1_[a-z2-7]{39}$/u;
const filePattern = /^file1_[a-z2-7]{39}$/u;
const contentHashPattern = /^b3_[a-z2-7]{58}$/u;
const capabilityPattern = /^[A-Za-z0-9_-]{43}$/u;
const decimalPattern = /^(?:0|[1-9][0-9]*)$/u;
const base64Pattern = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u;
const freshnessValues = new Set(["current", "stale", "superseded"]);
const tierValues = new Set(["tier_a", "tier_b", "tier_c", "tier_d"]);
const coverageValues = new Set(["complete", "bounded", "sampled"]);
const completenessValues = new Set([
  "complete",
  "truncated",
  "unsupported_partial",
  "indeterminate",
]);
const continuationValues = new Set(["not_applicable", "available", "unavailable"]);
const directionValues = new Set(["inbound", "outbound", "both"]);
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

export function parseNodeDetail(
  value: unknown,
  expectedRepositoryId: string,
  expectedGenerationId: string,
  expectedNodeId: string,
): NodeDetail {
  const record = asRecord(value);
  const context = parseContext(record.context);
  const detail: NodeDetail = {
    schema: asLiteral(record.schema, "rootlight.web-node-detail/1"),
    repositoryId: asPattern(record.repositoryId, repositoryPattern),
    generationId: asPattern(record.generationId, generationPattern),
    nodeId: asPattern(record.nodeId, symbolPattern),
    idKind: asLiteral(record.idKind, "symbol"),
    kind: asText(record.kind, 256),
    displayName: asText(record.displayName, 512),
    qualifiedName: asOptionalText(record.qualifiedName, 512),
    signature: asOptionalText(record.signature, 4_096),
    language: asText(record.language, 256),
    tier: asEnumOrUnknown(record.tier, tierValues) as EvidenceTier,
    confidence: asInteger(record.confidence, 0, 1_000),
    provider: asText(record.provider, 256),
    evidence: asText(record.evidence, 256),
    outboundExact: asDecimal(record.outboundExact),
    outboundCandidates: asDecimal(record.outboundCandidates),
    inboundExact: asDecimal(record.inboundExact),
    inboundCandidates: asDecimal(record.inboundCandidates),
    referenceCount: asDecimal(record.referenceCount),
    generated: asOptionalBoolean(record.generated),
    sourceReferences: asArray(record.sourceReferences, 64).map(parseSourceCapability),
    context,
    completeness: parseCompleteness(record.completeness),
  };
  validateCorrelation(
    detail.repositoryId,
    detail.generationId,
    context,
    expectedRepositoryId,
    expectedGenerationId,
  );
  if (detail.nodeId !== expectedNodeId || context.usage.sourceBytes !== "0") {
    throw new Error("Node detail does not match the requested source-free symbol");
  }
  return detail;
}

export function parseRelationships(
  value: unknown,
  expectedRepositoryId: string,
  expectedGenerationId: string,
): Relationships {
  const record = asRecord(value);
  const context = parseContext(record.context);
  validateCorrelation(
    context.repositoryId,
    context.generationId,
    context,
    expectedRepositoryId,
    expectedGenerationId,
  );
  const groups = asArray(record.groups, 128).map(parseRelationshipGroup);
  const returnedEdges = asDecimal(record.returnedEdges);
  const totalEdges = asDecimal(record.totalEdges);
  const completeness = parseCompleteness(record.completeness);
  const nextPageOffset = asOptionalDecimal(record.nextPageOffset);
  const returnedTargets = groups.reduce((total, group) => total + group.targets.length, 0);
  const sourceCapabilities = groups.reduce(
    (total, group) =>
      total +
      group.targets.reduce((groupTotal, target) => groupTotal + target.sourceReferences.length, 0),
    0,
  );
  if (
    BigInt(returnedEdges) !== BigInt(returnedTargets) ||
    BigInt(totalEdges) < BigInt(returnedEdges) ||
    sourceCapabilities > 64 ||
    (nextPageOffset !== null) !== (completeness.continuation === "available") ||
    context.usage.sourceBytes !== "0"
  ) {
    throw new Error("Relationship counts or continuation are inconsistent");
  }
  return {
    schema: asLiteral(record.schema, "rootlight.web-relationships/1"),
    context,
    groups,
    returnedEdges,
    totalEdges,
    exact: asBoolean(record.exact),
    truncated: asBoolean(record.truncated),
    nextPageOffset,
    completeness,
  };
}

export function parseSourceRead(
  value: unknown,
  expectedRepositoryId: string,
  expectedGenerationId: string,
): SourceRead {
  const record = asRecord(value);
  const context = parseContext(record.context);
  const repositoryId = asPattern(record.repositoryId, repositoryPattern);
  const generationId = asPattern(record.generationId, generationPattern);
  validateCorrelation(
    repositoryId,
    generationId,
    context,
    expectedRepositoryId,
    expectedGenerationId,
  );
  const chunks = asArray(record.chunks, 1).map(parseSourceChunk);
  const totalSourceBytes = asDecimal(record.totalSourceBytes);
  const measuredBytes = chunks.reduce((total, chunk) => total + sourceChunkBytes(chunk), 0);
  if (
    measuredBytes > 64 * 1_024 ||
    BigInt(totalSourceBytes) !== BigInt(measuredBytes) ||
    context.usage.sourceBytes !== totalSourceBytes
  ) {
    throw new Error("Source response byte accounting is inconsistent");
  }
  return {
    schema: asLiteral(record.schema, "rootlight.web-source/1"),
    repositoryId,
    generationId,
    chunks,
    totalSourceBytes,
    truncated: asBoolean(record.truncated),
    context,
    completeness: parseCompleteness(record.completeness),
  };
}

export function parseChangeImpact(
  value: unknown,
  expectedRepositoryId: string,
  expectedGenerationId: string,
): ChangeImpact {
  const record = asRecord(value);
  const context = parseContext(record.context);
  validateCorrelation(
    context.repositoryId,
    context.generationId,
    context,
    expectedRepositoryId,
    expectedGenerationId,
  );
  if (context.usage.sourceBytes !== "0") {
    throw new Error("Change impact response unexpectedly contains source");
  }
  const resolvedChanges = asArray(record.resolvedChanges, 16).map((value) => {
    const item = asRecord(value);
    const symbolId = asOptionalPattern(item.symbolId, symbolPattern);
    const fileId = asOptionalPattern(item.fileId, filePattern);
    if (symbolId === null && fileId === null) {
      throw new Error("Impact change has no stable identity");
    }
    return {
      symbolId,
      fileId,
      classification: asText(item.classification, 256),
      kind: asOptionalText(item.kind, 256),
    };
  });
  const impacted = asArray(record.impacted, 16).map((value) => {
    const item = asRecord(value);
    const sourceIndex = asInteger(item.sourceIndex, 0, 15);
    return {
      sourceIndex,
      dependents: asArray(item.dependents, 200).map(parseImpactEntry),
    };
  });
  const dependentCount = impacted.reduce((total, group) => total + group.dependents.length, 0);
  if (
    dependentCount > 200 ||
    impacted.some((group) => group.sourceIndex >= resolvedChanges.length)
  ) {
    throw new Error("Impact response is inconsistent or exceeds the dependent bound");
  }
  const risk = asRecord(record.riskSummary);
  return {
    schema: asLiteral(record.schema, "rootlight.web-change-impact/1"),
    context,
    resolvedChanges,
    impacted,
    tests: asArray(record.tests, 500).map((value) => {
      const item = asRecord(value);
      return {
        testId: asText(item.testId, 256),
        relevance: asInteger(item.relevance, 0, 1_000),
        why: asArray(item.why, 32).map((reason) => asText(reason, 256)),
        estimatedCostMs: asOptionalInteger(item.estimatedCostMs, 0, 0xffff_ffff),
      };
    }),
    riskSummary: {
      level: asText(risk.level, 256),
      reasons: asArray(risk.reasons, 32).map((reason) => asText(reason, 256)),
      coverage: asText(risk.coverage, 256),
      breakingSurface: asBoolean(risk.breakingSurface),
      fanout: asInteger(risk.fanout, 0, 0xffff_ffff),
      dynamicBlindSpots: asBoolean(risk.dynamicBlindSpots),
    },
    completeness: parseCompleteness(record.completeness),
  };
}

function parseContext(value: unknown): EvidenceContext {
  const record = asRecord(value);
  const usage = asRecord(record.usage);
  return {
    repositoryId: asPattern(record.repositoryId, repositoryPattern),
    generationId: asPattern(record.generationId, generationPattern),
    parentGenerationId: asOptionalPattern(record.parentGenerationId, generationPattern),
    activeGeneration: asBoolean(record.activeGeneration),
    structuralFreshness: asEnumOrUnknown(
      record.structuralFreshness,
      freshnessValues,
    ) as EvidenceFreshness,
    semanticFreshness: asEnumOrUnknown(
      record.semanticFreshness,
      freshnessValues,
    ) as EvidenceFreshness,
    tier: asEnumOrUnknown(record.tier, tierValues) as EvidenceTier,
    coverageStatus: asEnumOrUnknown(record.coverageStatus, coverageValues) as EvidenceCoverage,
    skippedInputs: asDecimal(record.skippedInputs),
    usage: {
      rows: asDecimal(usage.rows),
      edges: asDecimal(usage.edges),
      results: asDecimal(usage.results),
      sourceBytes: asDecimal(usage.sourceBytes),
      jsonBytes: asDecimal(usage.jsonBytes),
      estimatedTokens: asDecimal(usage.estimatedTokens),
      tokenAccountingProfile:
        usage.tokenAccountingProfile === null
          ? null
          : asLiteral(usage.tokenAccountingProfile, "utf8_byte_upper_bound_v1"),
      memoryBytes: asOptionalDecimal(usage.memoryBytes),
      elapsedMicros: asDecimal(usage.elapsedMicros),
    },
  };
}

function parseCompleteness(value: unknown): EvidenceCompleteness {
  const record = asRecord(value);
  const state = asClosedEnum(record.state, completenessValues) as EvidenceCompletenessState;
  const continuation = asClosedEnum(
    record.continuation,
    continuationValues,
  ) as EvidenceContinuation;
  if ((state === "complete") !== (continuation === "not_applicable")) {
    throw new Error("Evidence completeness and continuation are inconsistent");
  }
  return {
    state,
    limitingResources: asArray(record.limitingResources, 16).map((value) => {
      const resource = asRecord(value);
      return {
        kind: asEnumOrUnknown(resource.kind, limitingResourceValues),
        limit: asOptionalDecimal(resource.limit),
        observed: asOptionalDecimal(resource.observed),
      };
    }),
    continuation,
    guidance: asArray(record.guidance, 16).map((guidance) =>
      asEnumOrUnknown(guidance, guidanceValues),
    ),
  };
}

function parseSourceCapability(value: unknown): SourceCapability {
  const record = asRecord(value);
  return {
    capability: asPattern(record.capability, capabilityPattern),
    expiresInSeconds: asInteger(record.expiresInSeconds, 1, 60),
  };
}

function parseRelationshipGroup(value: unknown): RelationshipGroup {
  const record = asRecord(value);
  const targets = asArray(record.targets, 100).map((targetValue) => {
    const target = asRecord(targetValue);
    return {
      symbolId: asPattern(target.symbolId, symbolPattern),
      confidence: asInteger(target.confidence, 0, 1_000),
      sourceReferences: asArray(target.sourceReferences, 64).map(parseSourceCapability),
    };
  });
  const totalCount = asDecimal(record.totalCount);
  if (BigInt(totalCount) < BigInt(targets.length)) {
    throw new Error("Relationship group total is smaller than its returned targets");
  }
  return {
    seedId: asPattern(record.seedId, symbolPattern),
    relation: asText(record.relation, 256),
    direction: asClosedEnum(record.direction, directionValues) as RelationshipGroup["direction"],
    totalCount,
    targets,
  };
}

function parseSourceChunk(value: unknown): SourceChunk {
  const record = asRecord(value);
  const encoding = asClosedEnum(record.encoding, new Set(["utf8", "base64"])) as "utf8" | "base64";
  const content = asBoundedSource(record.content, encoding);
  const requestedStartByte = asDecimal(record.requestedStartByte);
  const requestedEndByte = asDecimal(record.requestedEndByte);
  const includedStartByte = asDecimal(record.includedStartByte);
  const includedEndByte = asDecimal(record.includedEndByte);
  const includedBytes = BigInt(includedEndByte) - BigInt(includedStartByte);
  if (
    BigInt(requestedStartByte) > BigInt(requestedEndByte) ||
    BigInt(includedStartByte) > BigInt(requestedStartByte) ||
    BigInt(includedEndByte) < BigInt(requestedEndByte) ||
    includedBytes !== BigInt(encodedSourceBytes(content, encoding))
  ) {
    throw new Error("Source range does not match its bounded content");
  }
  return {
    fileId: asPattern(record.fileId, filePattern),
    path: asText(record.path, 8_192),
    requestedStartByte,
    requestedEndByte,
    includedStartByte,
    includedEndByte,
    includedStartLine: asOptionalDecimal(record.includedStartLine),
    includedEndLine: asOptionalDecimal(record.includedEndLine),
    content,
    encoding,
    contentHash: asPattern(record.contentHash, contentHashPattern),
    language: asText(record.language, 256),
    tier: asEnumOrUnknown(record.tier, tierValues) as EvidenceTier,
    generated: asBoolean(record.generated),
  };
}

function parseImpactEntry(value: unknown): ImpactEntry {
  const record = asRecord(value);
  return {
    symbolId: asPattern(record.symbolId, symbolPattern),
    kind: asText(record.kind, 256),
    distance: asInteger(record.distance, 1, 8),
    confidence: asInteger(record.confidence, 0, 1_000),
    via: asArray(record.via, 32).map((relation) => asText(relation, 256)),
    isPublic: asBoolean(record.isPublic),
  };
}

function validateCorrelation(
  repositoryId: string,
  generationId: string,
  context: EvidenceContext,
  expectedRepositoryId: string,
  expectedGenerationId: string,
) {
  if (
    repositoryId !== expectedRepositoryId ||
    generationId !== expectedGenerationId ||
    context.repositoryId !== repositoryId ||
    context.generationId !== generationId
  ) {
    throw new Error("Evidence response does not match the requested immutable generation");
  }
}

function sourceChunkBytes(chunk: SourceChunk): number {
  return encodedSourceBytes(chunk.content, chunk.encoding);
}

function encodedSourceBytes(content: string, encoding: "utf8" | "base64"): number {
  if (encoding === "utf8") {
    return new TextEncoder().encode(content).byteLength;
  }
  const padding = content.endsWith("==") ? 2 : content.endsWith("=") ? 1 : 0;
  return (content.length / 4) * 3 - padding;
}

function asBoundedSource(value: unknown, encoding: "utf8" | "base64"): string {
  if (typeof value !== "string") {
    throw new Error("Source response has invalid content");
  }
  if (encoding === "utf8") {
    if (new TextEncoder().encode(value).byteLength > 64 * 1_024) {
      throw new Error("Source response exceeds its byte bound");
    }
    return value;
  }
  if (value.length > 87_384 || !base64Pattern.test(value)) {
    throw new Error("Source response has invalid base64 content");
  }
  return value;
}

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("Evidence response has an invalid shape");
  }
  return value as Record<string, unknown>;
}

function asArray(value: unknown, maximum: number): unknown[] {
  if (!Array.isArray(value) || value.length > maximum) {
    throw new Error("Evidence response has an invalid array");
  }
  return value;
}

function asText(value: unknown, maximumLength: number): string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > maximumLength ||
    Array.from(value).some((character) => {
      const codePoint = character.codePointAt(0);
      return (
        codePoint !== undefined &&
        (codePoint === 0 ||
          (codePoint < 32 && character !== "\n" && character !== "\r" && character !== "\t"))
      );
    })
  ) {
    throw new Error("Evidence response has invalid text");
  }
  return value;
}

function asOptionalText(value: unknown, maximumLength: number): string | null {
  return value === null ? null : asText(value, maximumLength);
}

function asBoolean(value: unknown): boolean {
  if (typeof value !== "boolean") {
    throw new Error("Evidence response has an invalid boolean");
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
    throw new Error("Evidence response has an invalid integer");
  }
  return value;
}

function asOptionalInteger(value: unknown, minimum: number, maximum: number): number | null {
  return value === null ? null : asInteger(value, minimum, maximum);
}

function asDecimal(value: unknown): string {
  if (typeof value !== "string" || value.length > 20 || !decimalPattern.test(value)) {
    throw new Error("Evidence response has an invalid decimal");
  }
  return value;
}

function asOptionalDecimal(value: unknown): string | null {
  return value === null ? null : asDecimal(value);
}

function asPattern(value: unknown, pattern: RegExp): string {
  if (typeof value !== "string" || !pattern.test(value)) {
    throw new Error("Evidence response has an invalid identifier");
  }
  return value;
}

function asOptionalPattern(value: unknown, pattern: RegExp): string | null {
  return value === null ? null : asPattern(value, pattern);
}

function asLiteral<Value extends string>(value: unknown, expected: Value): Value {
  if (value !== expected) {
    throw new Error("Evidence response has an unknown schema");
  }
  return expected;
}

function asEnumOrUnknown(value: unknown, accepted: ReadonlySet<string>): string {
  if (typeof value !== "string") {
    throw new Error("Evidence response has an invalid enum");
  }
  return accepted.has(value) ? value : "unknown";
}

function asClosedEnum(value: unknown, accepted: ReadonlySet<string>): string {
  if (typeof value !== "string" || !accepted.has(value)) {
    throw new Error("Evidence response has an unsupported enum");
  }
  return value;
}
