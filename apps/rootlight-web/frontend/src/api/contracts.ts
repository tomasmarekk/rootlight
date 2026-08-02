// Parses the source-free browser DTOs exposed by the local Rust host.

export type DaemonLifecycle = "starting" | "ready" | "draining" | "faulted" | "stopped";
export type HealthStatus = "healthy" | "degraded" | "unavailable" | "not_configured" | "failed";
export type ResourcePressure = "normal" | "elevated" | "high" | "critical" | "unknown";

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
    lifecycle: asEnum(record.lifecycle, lifecycleValues),
    acceptingOperations: asBoolean(record.acceptingOperations),
    activeOperations: asBoundedInteger(record.activeOperations, 0, 1_000_000),
    queuedOperations: asBoundedInteger(record.queuedOperations, 0, 1_000_000),
    runningOperations: asBoundedInteger(record.runningOperations, 0, 1_000_000),
    journalHealthy: asBoolean(record.journalHealthy),
    catalogStatus: asEnum(record.catalogStatus, healthStatusValues),
    generationStatus: asEnum(record.generationStatus, healthStatusValues),
    adapterStatus: asEnum(record.adapterStatus, healthStatusValues),
    watcherStatus: asEnum(record.watcherStatus, healthStatusValues),
    endpointStatus: asEnum(record.endpointStatus, healthStatusValues),
    resourcePressure: asEnum(record.resourcePressure, resourcePressureValues),
  };
}

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("API response has an invalid shape");
  }
  return value as Record<string, unknown>;
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

function asBoundedInteger(value: unknown, minimum: number, maximum: number): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    throw new Error("API response has an invalid integer");
  }
  return value as number;
}

function asEnum<Value extends string>(value: unknown, accepted: ReadonlySet<Value>): Value {
  if (typeof value !== "string" || !accepted.has(value as Value)) {
    throw new Error("API response has an unknown state");
  }
  return value as Value;
}
