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
export type IndexMode = "auto" | "structural" | "deep";
export type AdapterIsolation = "available" | "degraded" | "unavailable" | "not_configured";

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
  admittedOperations: number;
  queuedOperations: number;
  runningOperations: number;
  activeConnections: number;
  connectionLimit: number;
  operationQueueLimit: number;
  journalHealthy: boolean;
  catalogSchemaVersion: number;
  endpointSchemaVersion: number;
  catalogStatus: HealthStatus;
  generationStatus: HealthStatus;
  adapterStatus: HealthStatus;
  watcherStatus: HealthStatus;
  endpointStatus: HealthStatus;
  resourcePressure: ResourcePressure;
};

export type DiagnosticOutcome = "passed" | "failed" | "timed_out" | "unavailable" | "unknown";

export type DiagnosticError = {
  code: number;
  message: string;
  retryable: boolean;
  retryAfterMs: string | null;
};

export type DiagnosticCheck = {
  name: string;
  outcome: DiagnosticOutcome;
  durationMs: number;
  error: DiagnosticError | null;
};

export type QuickDiagnostics = {
  schema: "rootlight.web-quick-diagnostics/1";
  schemaVersion: number;
  overallStatus: HealthStatus;
  durationMs: number;
  checks: DiagnosticCheck[];
};

export type SupportBundle = {
  schema: "rootlight.web-support-bundle/1";
  receipt: string;
  downloadPath: string;
  archiveBytes: string;
  sha256: string;
  containsSource: false;
  expiresInSeconds: number;
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
  rootPath: string | null;
  generationCount: string;
  lifecycleState: ProjectLifecycle;
  languages: string[];
  structuralFreshness: ProjectFreshness;
  semanticFreshness: ProjectFreshness;
  coverage: CoverageEntry[];
};

export type ProjectRenameResponse = {
  schema: "rootlight.web-project-rename/1";
  alias: string;
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

export type FilesystemRoot = {
  label: string;
  browseToken: string;
  readable: boolean;
  selectable: boolean;
};

export type FilesystemRoots = {
  schema: "rootlight.web-filesystem-roots/1";
  roots: FilesystemRoot[];
};

export type OpenFilesystemPath = {
  schema: "rootlight.web-filesystem-open-path/1";
  label: string;
  browseToken: string;
};

export type FilesystemBreadcrumb = {
  label: string;
  browseToken: string;
};

export type FilesystemDirectory = {
  name: string;
  kind: "directory";
  readable: boolean;
  selectable: boolean;
};

export type FilesystemBrowsePage = {
  schema: "rootlight.web-filesystem-browse/1";
  browseToken: string;
  label: string;
  depth: number;
  maximumDepth: number;
  breadcrumbs: FilesystemBreadcrumb[];
  directories: FilesystemDirectory[];
  nextCursor: string | null;
};

export type IndexPreflight = {
  schema: "rootlight.web-index-preflight/1";
  selectable: boolean;
  normalizedDisplayLabel: string;
  daemonAcceptingOperations: boolean;
  selectedMode: IndexMode;
  supportedModes: IndexMode[];
  adapterIsolation: AdapterIsolation;
  estimatedLimitations: string[];
  warnings: string[];
  rootCapability: string;
  rootCapabilityExpiresInSeconds: number;
};

export type IndexDiagnostic = {
  code: string;
  message: string;
};

export type ProjectIndexAdmission = {
  schema: "rootlight.web-project-index/1";
  displayLabel: string;
  repositoryId: string;
  operationId: string;
  semanticOperationId: string | null;
  state: OperationState;
  revision: string;
  mode: IndexMode;
  parentGenerationId: string | null;
  publishedGenerationId: string | null;
  discoveredInputs: string;
  indexedFiles: string;
  entities: string;
  elapsedMicros: string;
  estimatedDiskBytes: string;
  diagnostics: IndexDiagnostic[];
};

export type OperationStage = "accepted" | "executing" | "cleanup" | "unknown";
export type RecoveryClass =
  "not_applicable" | "interrupted_by_restart" | "deadline_elapsed" | "lease_expired" | "unknown";

export type OperationError = {
  code: number;
  message: string;
  retryable: boolean;
  retryAfterMs: string | null;
};

export type RepositoryOperation = {
  schema: "rootlight.web-repository-operation/1";
  displayLabel: string;
  mode: IndexMode;
  ownedBySession: boolean;
  operationId: string;
  state: OperationState;
  revision: string;
  completedUnits: number;
  totalUnits: number;
  kind: "repository_index" | "unknown";
  stage: OperationStage;
  detached: boolean;
  cancellationRequested: boolean;
  recoveryClass: RecoveryClass;
  error: OperationError | null;
  publishedGenerationId: string | null;
  semanticOperationId: string | null;
  startedUnixMs: string;
  peakRssBytes: string;
  writtenBytes: string;
  filesExamined: string;
  bytesExamined: string;
  indexStage: string;
  retryAfterMs: number | null;
};

export type OperationCancel = {
  schema: "rootlight.web-operation-cancel/1";
  accepted: boolean;
  operation: RepositoryOperation;
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
const indexModeValues = new Set<IndexMode>(["auto", "structural", "deep"]);
const adapterIsolationValues = new Set<AdapterIsolation>([
  "available",
  "degraded",
  "unavailable",
  "not_configured",
]);
const operationStageValues = new Set<OperationStage>(["accepted", "executing", "cleanup"]);
const recoveryClassValues = new Set<RecoveryClass>([
  "not_applicable",
  "interrupted_by_restart",
  "deadline_elapsed",
  "lease_expired",
]);
const diagnosticOutcomeValues = new Set<DiagnosticOutcome>([
  "passed",
  "failed",
  "timed_out",
  "unavailable",
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
    admittedOperations: asBoundedInteger(record.admittedOperations, 0, 1_000_000),
    queuedOperations: asBoundedInteger(record.queuedOperations, 0, 1_000_000),
    runningOperations: asBoundedInteger(record.runningOperations, 0, 1_000_000),
    activeConnections: asBoundedInteger(record.activeConnections, 0, 1_000_000),
    connectionLimit: asBoundedInteger(record.connectionLimit, 1, 1_000_000),
    operationQueueLimit: asBoundedInteger(record.operationQueueLimit, 1, 1_000_000),
    journalHealthy: asBoolean(record.journalHealthy),
    catalogSchemaVersion: asBoundedInteger(record.catalogSchemaVersion, 1, 1_000),
    endpointSchemaVersion: asBoundedInteger(record.endpointSchemaVersion, 1, 1_000),
    catalogStatus: asEnumOrUnknown(record.catalogStatus, healthStatusValues),
    generationStatus: asEnumOrUnknown(record.generationStatus, healthStatusValues),
    adapterStatus: asEnumOrUnknown(record.adapterStatus, healthStatusValues),
    watcherStatus: asEnumOrUnknown(record.watcherStatus, healthStatusValues),
    endpointStatus: asEnumOrUnknown(record.endpointStatus, healthStatusValues),
    resourcePressure: asEnumOrUnknown(record.resourcePressure, resourcePressureValues),
  };
}

export function parseQuickDiagnostics(value: unknown): QuickDiagnostics {
  const record = asRecord(value);
  return {
    schema: asLiteral(record.schema, "rootlight.web-quick-diagnostics/1"),
    schemaVersion: asBoundedInteger(record.schemaVersion, 1, 1_000),
    overallStatus: asEnumOrUnknown(record.overallStatus, healthStatusValues),
    durationMs: asBoundedInteger(record.durationMs, 0, 4_294_967_295),
    checks: asBoundedArray(record.checks, 64).map(parseDiagnosticCheck),
  };
}

export function parseSupportBundle(value: unknown): SupportBundle {
  const record = asRecord(value);
  const receipt = asOpaqueToken(record.receipt);
  const downloadPath = asBoundedString(record.downloadPath, 256);
  const expectedPath = `/api/v1/diagnostics/support-bundles/${receipt}`;
  if (downloadPath !== expectedPath) {
    throw new Error("API response has an invalid support bundle path");
  }
  const containsSource = asBoolean(record.containsSource);
  if (containsSource) {
    throw new Error("API response contains a source-bearing support bundle");
  }
  const sha256 = asBoundedString(record.sha256, 64);
  if (!/^[a-f0-9]{64}$/u.test(sha256)) {
    throw new Error("API response has an invalid support bundle digest");
  }
  return {
    schema: asLiteral(record.schema, "rootlight.web-support-bundle/1"),
    receipt,
    downloadPath,
    archiveBytes: asBoundedDecimalString(record.archiveBytes, 768 * 1_024),
    sha256,
    containsSource: false,
    expiresInSeconds: asBoundedInteger(record.expiresInSeconds, 1, 3_600),
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

export function parseProjectRenameResponse(
  value: unknown,
  expectedAlias: string,
): ProjectRenameResponse {
  const record = asRecord(value);
  const alias = asBoundedString(record.alias, 256);
  if (alias !== expectedAlias) {
    throw new Error("API response has an uncorrelated project alias");
  }
  return {
    schema: asLiteral(record.schema, "rootlight.web-project-rename/1"),
    alias,
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

export function parseFilesystemRoots(value: unknown): FilesystemRoots {
  const record = asRecord(value);
  return {
    schema: asLiteral(record.schema, "rootlight.web-filesystem-roots/1"),
    roots: asBoundedArray(record.roots, 32).map((root) => {
      const rootRecord = asRecord(root);
      return {
        label: asBoundedString(rootRecord.label, 256),
        browseToken: asOpaqueToken(rootRecord.browseToken),
        readable: asBoolean(rootRecord.readable),
        selectable: asBoolean(rootRecord.selectable),
      };
    }),
  };
}

export function parseOpenFilesystemPath(value: unknown): OpenFilesystemPath {
  const record = asRecord(value);
  return {
    schema: asLiteral(record.schema, "rootlight.web-filesystem-open-path/1"),
    label: asBoundedString(record.label, 256),
    browseToken: asOpaqueToken(record.browseToken),
  };
}

export function parseFilesystemBrowsePage(value: unknown): FilesystemBrowsePage {
  const record = asRecord(value);
  const maximumDepth = asBoundedInteger(record.maximumDepth, 0, 64);
  const depth = asBoundedInteger(record.depth, 0, maximumDepth);
  const breadcrumbs = asBoundedArray(record.breadcrumbs, maximumDepth + 1).map((breadcrumb) => {
    const breadcrumbRecord = asRecord(breadcrumb);
    return {
      label: asBoundedString(breadcrumbRecord.label, 256),
      browseToken: asOpaqueToken(breadcrumbRecord.browseToken),
    };
  });
  if (breadcrumbs.length !== depth + 1) {
    throw new Error("API response has invalid filesystem breadcrumbs");
  }
  return {
    schema: asLiteral(record.schema, "rootlight.web-filesystem-browse/1"),
    browseToken: asOpaqueToken(record.browseToken),
    label: asBoundedString(record.label, 256),
    depth,
    maximumDepth,
    breadcrumbs,
    directories: asBoundedArray(record.directories, 256).map((directory) => {
      const directoryRecord = asRecord(directory);
      return {
        name: asBoundedString(directoryRecord.name, 1_024),
        kind: asLiteral(directoryRecord.kind, "directory"),
        readable: asBoolean(directoryRecord.readable),
        selectable: asBoolean(directoryRecord.selectable),
      };
    }),
    nextCursor: asOptionalOpaqueToken(record.nextCursor),
  };
}

export function parseIndexPreflight(value: unknown): IndexPreflight {
  const record = asRecord(value);
  const selectedMode = asClosedEnum(record.selectedMode, indexModeValues);
  const supportedModes = asBoundedArray(record.supportedModes, 3).map((mode) =>
    asClosedEnum(mode, indexModeValues),
  );
  if (new Set(supportedModes).size !== supportedModes.length) {
    throw new Error("API response has duplicate index modes");
  }
  return {
    schema: asLiteral(record.schema, "rootlight.web-index-preflight/1"),
    selectable: asBoolean(record.selectable),
    normalizedDisplayLabel: asBoundedString(record.normalizedDisplayLabel, 256),
    daemonAcceptingOperations: asBoolean(record.daemonAcceptingOperations),
    selectedMode,
    supportedModes,
    adapterIsolation: asClosedEnum(record.adapterIsolation, adapterIsolationValues),
    estimatedLimitations: asBoundedArray(record.estimatedLimitations, 16).map((limitation) =>
      asBoundedString(limitation, 128),
    ),
    warnings: asBoundedArray(record.warnings, 16).map((warning) => asBoundedString(warning, 128)),
    rootCapability: asOpaqueToken(record.rootCapability),
    rootCapabilityExpiresInSeconds: asBoundedInteger(
      record.rootCapabilityExpiresInSeconds,
      1,
      3_600,
    ),
  };
}

export function parseProjectIndexAdmission(value: unknown): ProjectIndexAdmission {
  const record = asRecord(value);
  return {
    schema: asLiteral(record.schema, "rootlight.web-project-index/1"),
    displayLabel: asBoundedString(record.displayLabel, 256),
    repositoryId: asStableId(record.repositoryId, "repo1_"),
    operationId: asStableId(record.operationId, "op1_"),
    semanticOperationId: asOptionalStableId(record.semanticOperationId, "op1_"),
    state: asEnumOrUnknown(record.state, operationStateValues),
    revision: asDecimalString(record.revision),
    mode: asClosedEnum(record.mode, indexModeValues),
    parentGenerationId: asOptionalStableId(record.parentGenerationId, "gen1_"),
    publishedGenerationId: asOptionalStableId(record.publishedGenerationId, "gen1_"),
    discoveredInputs: asDecimalString(record.discoveredInputs),
    indexedFiles: asDecimalString(record.indexedFiles),
    entities: asDecimalString(record.entities),
    elapsedMicros: asDecimalString(record.elapsedMicros),
    estimatedDiskBytes: asDecimalString(record.estimatedDiskBytes),
    diagnostics: asBoundedArray(record.diagnostics, 64).map(parseIndexDiagnostic),
  };
}

export function parseRepositoryOperation(
  value: unknown,
  expectedOperationId: string,
): RepositoryOperation {
  const record = asRecord(value);
  const operation: RepositoryOperation = {
    schema: asLiteral(record.schema, "rootlight.web-repository-operation/1"),
    displayLabel: asBoundedString(record.displayLabel, 256),
    mode: asClosedEnum(record.mode, indexModeValues),
    ownedBySession: asBoolean(record.ownedBySession),
    operationId: asStableId(record.operationId, "op1_"),
    state: asEnumOrUnknown(record.state, operationStateValues),
    revision: asDecimalString(record.revision),
    completedUnits: asBoundedInteger(record.completedUnits, 0, 1_000_000_000),
    totalUnits: asBoundedInteger(record.totalUnits, 0, 1_000_000_000),
    kind: parseOperationKind(record.kind),
    stage: asEnumOrUnknown(record.stage, operationStageValues),
    detached: asBoolean(record.detached),
    cancellationRequested: asBoolean(record.cancellationRequested),
    recoveryClass: asEnumOrUnknown(record.recoveryClass, recoveryClassValues),
    error: record.error === null ? null : parseOperationError(record.error),
    publishedGenerationId: asOptionalStableId(record.publishedGenerationId, "gen1_"),
    semanticOperationId: asOptionalStableId(record.semanticOperationId, "op1_"),
    startedUnixMs: asDecimalString(record.startedUnixMs),
    peakRssBytes: asDecimalString(record.peakRssBytes),
    writtenBytes: asDecimalString(record.writtenBytes),
    filesExamined: asDecimalString(record.filesExamined),
    bytesExamined: asDecimalString(record.bytesExamined),
    indexStage: asBoundedText(record.indexStage, 128),
    retryAfterMs: asOptionalBoundedInteger(record.retryAfterMs, 0, 4_294_967_295),
  };
  if (operation.operationId !== expectedOperationId) {
    throw new Error("API response does not match the requested operation");
  }
  return operation;
}

export function parseOperationCancel(value: unknown, expectedOperationId: string): OperationCancel {
  const record = asRecord(value);
  return {
    schema: asLiteral(record.schema, "rootlight.web-operation-cancel/1"),
    accepted: asBoolean(record.accepted),
    operation: parseRepositoryOperation(record.operation, expectedOperationId),
  };
}

function parseProjectSummary(value: unknown): ProjectSummary {
  const record = asRecord(value);
  return {
    repositoryId: asStableId(record.repositoryId, "repo1_"),
    activeGenerationId: asOptionalStableId(record.activeGenerationId, "gen1_"),
    displayName: asBoundedString(record.displayName, 256),
    alias: asOptionalBoundedString(record.alias, 256),
    rootPath: asOptionalBoundedString(record.rootPath, 32 * 1_024),
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

function parseIndexDiagnostic(value: unknown): IndexDiagnostic {
  const record = asRecord(value);
  return {
    code: asBoundedString(record.code, 128),
    message: asBoundedString(record.message, 512),
  };
}

function parseOperationError(value: unknown): OperationError {
  const record = asRecord(value);
  return {
    code: asBoundedInteger(record.code, -2_147_483_648, 2_147_483_647),
    message: asBoundedString(record.message, 512),
    retryable: asBoolean(record.retryable),
    retryAfterMs: asOptionalDecimalString(record.retryAfterMs),
  };
}

function parseOperationKind(value: unknown): RepositoryOperation["kind"] {
  const kind = asBoundedString(value, 64);
  return kind === "repository_index" ? kind : "unknown";
}

function parseDiagnosticCheck(value: unknown): DiagnosticCheck {
  const record = asRecord(value);
  return {
    name: asBoundedString(record.name, 64),
    outcome: asEnumOrUnknown(record.outcome, diagnosticOutcomeValues),
    durationMs: asBoundedInteger(record.durationMs, 0, 4_294_967_295),
    error: record.error === null ? null : parseDiagnosticError(record.error),
  };
}

function parseDiagnosticError(value: unknown): DiagnosticError {
  const record = asRecord(value);
  return {
    code: asBoundedInteger(record.code, -2_147_483_648, 2_147_483_647),
    message: asBoundedString(record.message, 512),
    retryable: asBoolean(record.retryable),
    retryAfterMs: asOptionalDecimalString(record.retryAfterMs),
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

function asBoundedText(value: unknown, maximumLength: number): string {
  if (typeof value !== "string" || value.length > maximumLength) {
    throw new Error("API response has invalid text");
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

function asOpaqueToken(value: unknown): string {
  const token = asBoundedString(value, 128);
  if (!/^[A-Za-z0-9_-]{43}$/u.test(token)) {
    throw new Error("API response has an invalid opaque token");
  }
  return token;
}

function asOptionalOpaqueToken(value: unknown): string | null {
  return value === null ? null : asOpaqueToken(value);
}

function asDecimalString(value: unknown): string {
  const text = asBoundedString(value, 20);
  if (!/^(?:0|[1-9][0-9]*)$/u.test(text)) {
    throw new Error("API response has an invalid decimal");
  }
  return text;
}

function asBoundedDecimalString(value: unknown, maximum: number): string {
  const text = asDecimalString(value);
  if (BigInt(text) > BigInt(maximum)) {
    throw new Error("API response has an out-of-range decimal");
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

function asOptionalBoundedInteger(value: unknown, minimum: number, maximum: number): number | null {
  return value === null ? null : asBoundedInteger(value, minimum, maximum);
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

function asClosedEnum<Value extends string>(value: unknown, accepted: ReadonlySet<Value>): Value {
  if (typeof value !== "string" || !accepted.has(value as Value)) {
    throw new Error("API response has an invalid closed enum");
  }
  return value as Value;
}
