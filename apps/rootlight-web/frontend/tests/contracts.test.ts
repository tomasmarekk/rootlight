// Verifies runtime DTO parsing remains strict and fail-closed.

import { describe, expect, it } from "vitest";

import {
  parseHealth,
  parseProjectCatalogPage,
  parseProjectDetail,
  parseSession,
} from "../src/api/contracts";

describe("browser API contracts", () => {
  it("accepts the complete health shape", () => {
    expect(parseHealth(healthFixture()).protocolVersion).toBe("1.10");
  });

  it("maps additive health states to unknown and rejects unsafe values", () => {
    expect(parseHealth({ ...healthFixture(), lifecycle: "invented" }).lifecycle).toBe("unknown");
    expect(() =>
      parseHealth({ ...healthFixture(), activeOperations: Number.MAX_SAFE_INTEGER + 1 }),
    ).toThrow();
    expect(() => parseHealth({ ...healthFixture(), lifecycle: 1 })).toThrow();
    expect(() => parseHealth({ ...healthFixture(), webReady: "yes" })).toThrow();
    expect(() => parseHealth(null)).toThrow();
  });

  it("bounds session credentials", () => {
    expect(parseSession({ csrfToken: "token", idleTtlSeconds: 1_800 })).toEqual({
      csrfToken: "token",
      idleTtlSeconds: 1_800,
    });
    expect(() => parseSession({ csrfToken: "", idleTtlSeconds: 1_800 })).toThrow();
  });

  it("preserves immutable catalog identity and decimal counts", () => {
    const catalog = parseProjectCatalogPage(catalogFixture());
    expect(catalog.projects[0]).toMatchObject({
      repositoryId: repositoryId,
      activeGenerationId: generationId,
      generationCount: "18446744073709551615",
      lifecycleState: "ready",
    });
    expect(catalog.totalCount).toBe("18446744073709551615");
  });

  it("never interprets additive project states as healthy or complete", () => {
    const fixture = catalogFixture();
    const parsed = parseProjectCatalogPage({
      ...fixture,
      projects: [
        {
          ...fixture.projects[0],
          lifecycleState: "future_ready",
          structuralFreshness: "future_current",
        },
      ],
    });
    expect(parsed.projects[0]).toMatchObject({
      lifecycleState: "unknown",
      structuralFreshness: "unknown",
    });
  });

  it("rejects malformed catalog identities, counts, and oversized pages", () => {
    const fixture = catalogFixture();
    expect(() =>
      parseProjectCatalogPage({
        ...fixture,
        projects: [{ ...fixture.projects[0], repositoryId: "repo1_invalid" }],
      }),
    ).toThrow();
    expect(() =>
      parseProjectCatalogPage({
        ...fixture,
        projects: [{ ...fixture.projects[0], generationCount: "01" }],
      }),
    ).toThrow();
    expect(() =>
      parseProjectCatalogPage({
        ...fixture,
        projects: Array.from({ length: 101 }, () => fixture.projects[0]),
      }),
    ).toThrow();
  });

  it("parses correlated project detail operations up to the daemon bound", () => {
    const detail = projectDetailFixture();
    detail.operations = Array.from({ length: 100 }, projectOperationFixture);
    expect(parseProjectDetail(detail, repositoryId, "active").operations).toHaveLength(100);

    detail.operations.push(projectOperationFixture());
    expect(() => parseProjectDetail(detail, repositoryId, "active")).toThrow();
  });

  it("rejects valid-shaped project detail returned for another route identity", () => {
    const detail = projectDetailFixture();
    expect(() =>
      parseProjectDetail(
        { ...detail, repositoryId: `repo1_${"d".repeat(32)}` },
        repositoryId,
        "active",
      ),
    ).toThrow();
    expect(() =>
      parseProjectDetail(
        { ...detail, resolvedGenerationId: `gen1_${"e".repeat(39)}` },
        repositoryId,
        generationId,
      ),
    ).toThrow();
  });

  it("maps additive project detail states to unknown", () => {
    const detail = projectDetailFixture();
    const parsed = parseProjectDetail(
      {
        ...detail,
        activeSemanticFreshness: "future_current",
        lifecycleState: "future_ready",
        publicationState: "future_published",
      },
      repositoryId,
      "active",
    );
    expect(parsed).toMatchObject({
      activeSemanticFreshness: "unknown",
      lifecycleState: "unknown",
      publicationState: "unknown",
    });
  });
});

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const operationId = `op1_${"c".repeat(32)}`;

function healthFixture() {
  return {
    webReady: true,
    daemonReady: true,
    protocolVersion: "1.10",
    lifecycle: "ready",
    acceptingOperations: true,
    activeOperations: 0,
    queuedOperations: 0,
    runningOperations: 0,
    journalHealthy: true,
    catalogStatus: "healthy",
    generationStatus: "healthy",
    adapterStatus: "healthy",
    watcherStatus: "not_configured",
    endpointStatus: "healthy",
    resourcePressure: "normal",
  };
}

function catalogFixture() {
  return {
    schema: "rootlight.web-project-catalog-page/1",
    projects: [
      {
        repositoryId,
        activeGenerationId: generationId,
        displayName: "Rootlight",
        alias: null,
        generationCount: "18446744073709551615",
        lifecycleState: "ready",
        languages: ["rust"],
        structuralFreshness: "current",
        semanticFreshness: "stale",
        coverage: [
          {
            language: "rust",
            tier: "tier_b",
            status: "bounded",
            discoveredFiles: "100",
            indexedFiles: "90",
          },
        ],
      },
    ],
    snapshot: "snapshot",
    nextAfter: null,
    totalCount: "18446744073709551615",
    truncated: false,
    sortVersion: 1,
  };
}

function projectDetailFixture() {
  return {
    schema: "rootlight.web-project-detail/1",
    repositoryId,
    displayName: "Rootlight",
    alias: null,
    resolvedGenerationId: generationId,
    activeGenerationId: generationId,
    parentGenerationId: null,
    activeParentGenerationId: null,
    activeStructuralFreshness: "current",
    activeSemanticFreshness: "pending_refinement",
    structuralFreshness: "current",
    semanticFreshness: "stale",
    lifecycleState: "ready",
    publicationState: "published",
    coverage: [],
    operations: [projectOperationFixture()],
  };
}

function projectOperationFixture() {
  return {
    operationId,
    kind: "repository_index",
    state: "running",
    completedUnits: 2,
    totalUnits: 4,
    ownedByClient: true,
    startedUnixMs: "1",
  };
}
