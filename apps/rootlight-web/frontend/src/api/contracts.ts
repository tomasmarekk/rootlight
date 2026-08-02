// Parses the source-free browser DTOs exposed by the local Rust host.

export type DaemonLifecycle = "starting" | "ready" | "draining" | "faulted" | "stopped" | "unknown";
export type HealthStatus =
  "healthy" | "degraded" | "unavailable" | "not_configured" | "failed" | "unknown";
export type ResourcePressure = "normal" | "elevated" | "high" | "critical" | "unknown";
export type ProjectLifecycle =
  | "ready"
  | "indexing"
  | "degraded"
  | "corrupt"
  | "migration_required"
  | "rebuild_required"
  | "unknown";
export type ProjectLifecycleFilter = Exclude<ProjectLifecycle, "unknown">;
export type ProjectFreshness = "current" | "superseded" | "stale" | "unknown";
export type ProjectDetailFreshness = ProjectFreshness | "pending_refinement";
export type PublicationState = "published" | "retained" | "unknown";
export type OperationState =
  | "queued"
  | "running"
  | "cancelling"
  | "succeeded"
  | "failed"
  | "interrupted"
  | "cancelled"
  | "unknown";

export type Session = {
  csrfToken: string;
  idleTtlSeconds: number;
};

export type Health = {
  webReady: boolean;
  daemonReady: boolean;
  protocolVersion: string;
  lifecycle: DaemonLifecycle;
  acceptingOperations: boolean;
  activeOperations: number;
  queuedOperations: number;
  runningOperations: number;
  journalHealthy: boolean;
  catalogStatus: HealthStatus;
  generationStatus: HealthStatus;
  adapterStatus: HealthStatus;
  watcherStatus: HealthStatus;
  endpointStatus: HealthStatus;
  resourcePressure: ResourcePressure;
};

export type CoverageEntry = {
  language: string;
  tier: string;
  status: string;
  discoveredFiles: string;
  indexedFiles: string;
};

export type ProjectSummary = {
  repositoryId: string;
  activeGenerationId: string | null;
  displayName: string;
  alias: string | null;
  generationCount: string;
  lifecycleState: ProjectLifecycle;
  languages: string[];
  structuralFreshness: ProjectFreshness;
  semanticFreshness: ProjectFreshness;
  coverage: CoverageEntry[];
};

export type ProjectCatalogPage = {
  schema: "rootlight.web-project-catalog-page/1";
  projects: ProjectSummary[];
  snapshot: string;
  nextAfter: string | null;
  totalCount: string | null;
  truncated: boolean;
  sortVersion: number;
};

export type ProjectOperation = {
  operationId: string;
  kind: "control_probe" | "repository_index" | "unknown";
  state: OperationState;
  completedUnits: number;
  totalUnits: number;
  ownedByClient: boolean;
  startedUnixMs: string;
};

export type ProjectDetail = {
  schema: "rootlight.web-project-detail/1";
  repositoryId: string;
  displayName: string;
  alias: string | null;
  resolvedGenerationId: string;
  activeGenerationId: string;
  parentGenerationId: string | null;
  activeParentGenerationId: string | null;
  activeStructuralFreshness: ProjectDetailFreshness;
  activeSemanticFreshness: ProjectDetailFreshness;
  structuralFreshness: ProjectDetailFreshness;
  semanticFreshness: ProjectDetailFreshness;
  lifecycleState: ProjectLifecycle;
  publicationState: PublicationState;
  coverage: CoverageEntry[];
  operations: ProjectOperation[];
};

const lifecycleValues = new Set<DaemonLifecycle>([
  "starting",
  "ready",
  "draining",
  "faulted",
  "stopped",
]);
const healthStatusValues = new Set<HealthStatus>([
  "healthy",
  "degraded",
  "unavailable",
  "not_configured",
  "failed",
]);
const resourcePressureValues = new Set<ResourcePressure>([
  "normal",
  "elevated",
  "high",
  "critical",
  "unknown",
]);
const projectLifecycleValues = new Set<ProjectLifecycle>([
  "ready",
  "indexing",
  "degraded",
  "corrupt",
  "migration_required",
  "rebuild_required",
]);
const projectFreshnessValues = new Set<ProjectFreshness>(["current", "superseded", "stale"]);
const projectDetailFreshnessValues = new Set<ProjectDetailFreshness>([
  "current",
  "superseded",
  "stale",
  "pending_refinement",
]);
const publicationStateValues = new Set<PublicationState>(["published", "retained"]);
const operationStateValues = new Set<OperationState>([
  "queued",
  "running",
  "cancelling",
  "succeeded",
  "failed",
  "interrupted",
  "cancelled",
]);

export function parseSession(value: unknown): Session {
  const record = asRecord(value);
  const csrfToken = asBoundedString(record.csrfToken, 128);
  const idleTtlSeconds = asBoundedInteger(record.idleTtlSeconds, 1, 86_400);
  return { csrfToken, idleTtlSeconds };
}

export function parseHealth(value: unknown): Health {
  const record = asRecord(value);
  return {
    webReady: asBoolean(record.webReady),
    daemonReady: asBoolean(record.daemonReady),
    protocolVersion: asBoundedString(record.protocolVersion, 32),
    lifecycle: asEnumOrUnknown(record.lifecycle, lifecycleValues),
    acceptingOperations: asBoolean(record.acceptingOperations),
    activeOperations: asBoundedInteger(record.activeOperations, 0, 1_000_000),
    queuedOperations: asBoundedInteger(record.queuedOperations, 0, 1_000_000),
    runningOperations: asBoundedInteger(record.runningOperations, 0, 1_000_000),
    journalHealthy: asBoolean(record.journalHealthy),
    catalogStatus: asEnumOrUnknown(record.catalogStatus, healthStatusValues),
    generationStatus: asEnumOrUnknown(record.generationStatus, healthStatusValues),
    adapterStatus: asEnumOrUnknown(record.adapterStatus, healthStatusValues),
    watcherStatus: asEnumOrUnknown(record.watcherStatus, healthStatusValues),
    endpointStatus: asEnumOrUnknown(record.endpointStatus, healthStatusValues),
    resourcePressure: asEnumOrUnknown(record.resourcePressure, resourcePressureValues),
  };
}

export function parseProjectCatalogPage(value: unknown): ProjectCatalogPage {
  const record = asRecord(value);
  const schema = asLiteral(record.schema, "rootlight.web-project-catalog-page/1");
  const projects = asBoundedArray(record.projects, 100).map(parseProjectSummary);
  return {
    schema,
    projects,
    snapshot: asBoundedString(record.snapshot, 128),
    nextAfter: asOptionalBoundedString(record.nextAfter, 2_048),
    totalCount: asOptionalDecimalString(record.totalCount),
    truncated: asBoolean(record.truncated),
    sortVersion: asBoundedInteger(record.sortVersion, 1, 1_000),
  };
}

export function parseProjectDetail(
  value: unknown,
  expectedRepositoryId: string,
  generationSelector: string,
): ProjectDetail {
  const record = asRecord(value);
  const detail: ProjectDetail = {
    schema: asLiteral(record.schema, "rootlight.web-project-detail/1"),
    repositoryId: asStableId(record.repositoryId, "repo1_"),
    displayName: asBoundedString(record.displayName, 256),
    alias: asOptionalBoundedString(record.alias, 256),
    resolvedGenerationId: asStableId(record.resolvedGenerationId, "gen1_"),
    activeGenerationId: asStableId(record.activeGenerationId, "gen1_"),
    parentGenerationId: asOptionalStableId(record.parentGenerationId, "gen1_"),
    activeParentGenerationId: asOptionalStableId(record.activeParentGenerationId, "gen1_"),
    activeStructuralFreshness: asEnumOrUnknown(
      record.activeStructuralFreshness,
      projectDetailFreshnessValues,
    ),
    activeSemanticFreshness: asEnumOrUnknown(
      record.activeSemanticFreshness,
      projectDetailFreshnessValues,
    ),
    structuralFreshness: asEnumOrUnknown(record.structuralFreshness, projectDetailFreshnessValues),
    semanticFreshness: asEnumOrUnknown(record.semanticFreshness, projectDetailFreshnessValues),
    lifecycleState: asEnumOrUnknown(record.lifecycleState, projectLifecycleValues),
    publicationState: asEnumOrUnknown(record.publicationState, publicationStateValues),
    coverage: asBoundedArray(record.coverage, 64).map(parseCoverage),
    operations: asBoundedArray(record.operations, 100).map(parseProjectOperation),
  };
  const generationMatches =
    generationSelector === "active"
      ? detail.resolvedGenerationId === detail.activeGenerationId
      : detail.resolvedGenerationId === generationSelector;
  if (detail.repositoryId !== expectedRepositoryId || !generationMatches) {
    throw new Error("API response does not match the requested project generation");
  }
  return detail;
}

function parseProjectSummary(value: unknown): ProjectSummary {
  const record = asRecord(value);
  return {
    repositoryId: asStableId(record.repositoryId, "repo1_"),
    activeGenerationId: asOptionalStableId(record.activeGenerationId, "gen1_"),
    displayName: asBoundedString(record.displayName, 256),
    alias: asOptionalBoundedString(record.alias, 256),
    generationCount: asDecimalString(record.generationCount),
    lifecycleState: asEnumOrUnknown(record.lifecycleState, projectLifecycleValues),
    languages: asBoundedArray(record.languages, 64).map((language) =>
      asBoundedString(language, 64),
    ),
    structuralFreshness: asEnumOrUnknown(record.structuralFreshness, projectFreshnessValues),
    semanticFreshness: asEnumOrUnknown(record.semanticFreshness, projectFreshnessValues),
    coverage: asBoundedArray(record.coverage, 64).map(parseCoverage),
  };
}

function parseCoverage(value: unknown): CoverageEntry {
  const record = asRecord(value);
  return {
    language: asBoundedString(record.language, 64),
    tier: asBoundedString(record.tier, 64),
    status: asBoundedString(record.status, 64),
    discoveredFiles: asDecimalString(record.discoveredFiles),
    indexedFiles: asDecimalString(record.indexedFiles),
  };
}

function parseProjectOperation(value: unknown): ProjectOperation {
  const record = asRecord(value);
  const kindValue = asBoundedString(record.kind, 64);
  const kind =
    kindValue === "control_probe" || kindValue === "repository_index" ? kindValue : "unknown";
  return {
    operationId: asStableId(record.operationId, "op1_"),
    kind,
    state: asEnumOrUnknown(record.state, operationStateValues),
    completedUnits: asBoundedInteger(record.completedUnits, 0, 1_000_000_000),
    totalUnits: asBoundedInteger(record.totalUnits, 0, 1_000_000_000),
    ownedByClient: asBoolean(record.ownedByClient),
    startedUnixMs: asDecimalString(record.startedUnixMs),
  };
}

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("API response has an invalid shape");
  }
  return value as Record<string, unknown>;
}

function asBoundedArray(value: unknown, maximumLength: number): unknown[] {
  if (!Array.isArray(value) || value.length > maximumLength) {
    throw new Error("API response has an invalid array");
  }
  return value;
}

function asBoolean(value: unknown): boolean {
  if (typeof value !== "boolean") {
    throw new Error("API response has an invalid boolean");
  }
  return value;
}

function asBoundedString(value: unknown, maximumLength: number): string {
  if (typeof value !== "string" || value.length === 0 || value.length > maximumLength) {
    throw new Error("API response has an invalid string");
  }
  return value;
}

function asOptionalBoundedString(value: unknown, maximumLength: number): string | null {
  return value === null ? null : asBoundedString(value, maximumLength);
}

function asStableId(value: unknown, prefix: string): string {
  const identifier = asBoundedString(value, 64);
  const suffixLength = prefix === "repo1_" || prefix === "op1_" ? 32 : 39;
  const suffix = identifier.slice(prefix.length);
  if (
    !identifier.startsWith(prefix) ||
    suffix.length !== suffixLength ||
    !/^[a-z2-7]+$/u.test(suffix)
  ) {
    throw new Error("API response has an invalid identifier");
  }
  return identifier;
}

function asOptionalStableId(value: unknown, prefix: string): string | null {
  return value === null ? null : asStableId(value, prefix);
}

function asDecimalString(value: unknown): string {
  const text = asBoundedString(value, 20);
  if (!/^(?:0|[1-9][0-9]*)$/u.test(text)) {
    throw new Error("API response has an invalid decimal");
  }
  return text;
}

function asOptionalDecimalString(value: unknown): string | null {
  return value === null ? null : asDecimalString(value);
}

function asLiteral<Value extends string>(value: unknown, expected: Value): Value {
  if (value !== expected) {
    throw new Error("API response has an unknown schema");
  }
  return expected;
}

function asBoundedInteger(value: unknown, minimum: number, maximum: number): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    throw new Error("API response has an invalid integer");
  }
  return value as number;
}

function asEnumOrUnknown<Value extends string>(
  value: unknown,
  accepted: ReadonlySet<Value>,
): Value | "unknown" {
  if (typeof value !== "string") {
    throw new Error("API response has an invalid state");
  }
  return accepted.has(value as Value) ? (value as Value) : "unknown";
}
