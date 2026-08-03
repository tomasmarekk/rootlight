// Bounds the non-sensitive catalog state carried through project navigation.

import type { ProjectLifecycleFilter } from "../api/contracts";

const lifecycleFilters = new Set<ProjectLifecycleFilter>([
  "ready",
  "indexing",
  "degraded",
  "corrupt",
  "migration_required",
  "rebuild_required",
]);

export type CatalogCursorState = {
  snapshot: string;
  after: string;
  sortVersion: number;
};

export type CatalogNavigation = {
  searchInput: string;
  query: string;
  stateFilter: ProjectLifecycleFilter | "all";
  history: CatalogCursorState[];
};

export type CatalogLocationState = {
  schema: "rootlight.catalog-navigation/1";
  catalog: CatalogNavigation;
};

export function createCatalogLocationState(catalog: CatalogNavigation): CatalogLocationState {
  return {
    schema: "rootlight.catalog-navigation/1",
    catalog,
  };
}

export function parseCatalogLocationState(value: unknown): CatalogLocationState | undefined {
  if (!isRecord(value) || value.schema !== "rootlight.catalog-navigation/1") {
    return undefined;
  }
  const catalog = value.catalog;
  if (!isRecord(catalog)) {
    return undefined;
  }
  const searchInput = boundedString(catalog.searchInput, 256);
  const query = boundedString(catalog.query, 256);
  const stateFilter = parseStateFilter(catalog.stateFilter);
  const history = parseHistory(catalog.history);
  if (
    searchInput === undefined ||
    query === undefined ||
    stateFilter === undefined ||
    history === undefined
  ) {
    return undefined;
  }
  return createCatalogLocationState({ searchInput, query, stateFilter, history });
}

function parseHistory(value: unknown): CatalogCursorState[] | undefined {
  if (!Array.isArray(value) || value.length > 100) {
    return undefined;
  }
  const history: CatalogCursorState[] = [];
  for (const cursor of value) {
    if (!isRecord(cursor)) {
      return undefined;
    }
    const snapshot = boundedNonEmptyString(cursor.snapshot, 128);
    const after = boundedNonEmptyString(cursor.after, 2_048);
    const sortVersion = cursor.sortVersion;
    if (
      snapshot === undefined ||
      after === undefined ||
      typeof sortVersion !== "number" ||
      !Number.isSafeInteger(sortVersion) ||
      sortVersion < 1 ||
      sortVersion > 1_000
    ) {
      return undefined;
    }
    history.push({ snapshot, after, sortVersion });
  }
  return history;
}

function parseStateFilter(value: unknown): ProjectLifecycleFilter | "all" | undefined {
  if (value === "all") {
    return value;
  }
  return typeof value === "string" && lifecycleFilters.has(value as ProjectLifecycleFilter)
    ? (value as ProjectLifecycleFilter)
    : undefined;
}

function boundedString(value: unknown, maximumLength: number): string | undefined {
  return typeof value === "string" && value.length <= maximumLength ? value : undefined;
}

function boundedNonEmptyString(value: unknown, maximumLength: number): string | undefined {
  const text = boundedString(value, maximumLength);
  return text === undefined || text.length === 0 ? undefined : text;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
