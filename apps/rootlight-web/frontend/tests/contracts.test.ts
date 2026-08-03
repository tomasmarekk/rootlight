// Verifies runtime DTO parsing remains strict and fail-closed.

import { describe, expect, it } from "vitest";

import {
  parseFilesystemBrowsePage,
  parseFilesystemRoots,
  parseHealth,
  parseIndexPreflight,
  parseOpenFilesystemPath,
  parseOperationCancel,
  parseProjectCatalogPage,
  parseProjectDetail,
  parseProjectIndexAdmission,
  parseQuickDiagnostics,
  parseRepositoryOperation,
  parseSession,
  parseSupportBundle,
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

  it("parses bounded source-free diagnostics and additive outcomes", () => {
    const diagnostics = {
      schema: "rootlight.web-quick-diagnostics/1",
      schemaVersion: 1,
      overallStatus: "degraded",
      durationMs: 125,
      checks: [
        {
          name: "catalog",
          outcome: "future_outcome",
          durationMs: 125,
          error: {
            code: 12,
            message: "Catalog check timed out",
            retryable: true,
            retryAfterMs: "1000",
          },
        },
      ],
    };

    expect(parseQuickDiagnostics(diagnostics).checks[0]?.outcome).toBe("unknown");
    expect(() =>
      parseQuickDiagnostics({
        ...diagnostics,
        checks: Array.from({ length: 65 }, () => diagnostics.checks[0]),
      }),
    ).toThrow();
    expect(() =>
      parseQuickDiagnostics({
        ...diagnostics,
        checks: [{ ...diagnostics.checks[0], error: { message: "incomplete" } }],
      }),
    ).toThrow();
  });

  it("admits only matching source-free support bundle receipts", () => {
    const receipt = "a".repeat(43);
    const bundle = {
      schema: "rootlight.web-support-bundle/1",
      receipt,
      downloadPath: `/api/v1/diagnostics/support-bundles/${receipt}`,
      archiveBytes: "1024",
      sha256: "b".repeat(64),
      containsSource: false,
      expiresInSeconds: 120,
    };

    expect(parseSupportBundle(bundle)).toEqual(bundle);
    expect(() => parseSupportBundle({ ...bundle, containsSource: true })).toThrow();
    expect(() =>
      parseSupportBundle({
        ...bundle,
        downloadPath: "/api/v1/diagnostics/support-bundles/other",
      }),
    ).toThrow();
    expect(() => parseSupportBundle({ ...bundle, archiveBytes: "786433" })).toThrow();
    expect(() => parseSupportBundle({ ...bundle, sha256: "not-a-digest" })).toThrow();
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

  it("parses bounded filesystem capabilities without accepting path-like tokens", () => {
    const token = "a".repeat(43);
    expect(
      parseFilesystemRoots({
        schema: "rootlight.web-filesystem-roots/1",
        roots: [{ label: "Home", browseToken: token, readable: true, selectable: true }],
      }).roots[0],
    ).toEqual({ label: "Home", browseToken: token, readable: true, selectable: true });
    expect(
      parseOpenFilesystemPath({
        schema: "rootlight.web-filesystem-open-path/1",
        label: "rootlight",
        browseToken: token,
      }).label,
    ).toBe("rootlight");
    expect(() =>
      parseOpenFilesystemPath({
        schema: "rootlight.web-filesystem-open-path/1",
        label: "rootlight",
        browseToken: "C:\\source",
      }),
    ).toThrow();
  });

  it("correlates filesystem depth, breadcrumbs, and bounded directory pages", () => {
    const token = "a".repeat(43);
    const page = parseFilesystemBrowsePage({
      schema: "rootlight.web-filesystem-browse/1",
      browseToken: token,
      label: "rootlight",
      depth: 1,
      maximumDepth: 32,
      breadcrumbs: [
        { label: "Home", browseToken: token },
        { label: "rootlight", browseToken: "b".repeat(43) },
      ],
      directories: [{ name: "crates", kind: "directory", readable: true, selectable: true }],
      nextCursor: "c".repeat(43),
    });
    expect(page.directories).toHaveLength(1);
    expect(page.nextCursor).toBe("c".repeat(43));
    expect(() =>
      parseFilesystemBrowsePage({
        ...page,
        breadcrumbs: page.breadcrumbs.slice(0, 1),
      }),
    ).toThrow();
    expect(() =>
      parseFilesystemBrowsePage({
        ...page,
        directories: Array.from({ length: 257 }, () => page.directories[0]),
      }),
    ).toThrow();
  });

  it("keeps index modes and preflight capability semantics closed", () => {
    const token = "a".repeat(43);
    const preflight = {
      schema: "rootlight.web-index-preflight/1",
      selectable: true,
      normalizedDisplayLabel: "rootlight",
      daemonAcceptingOperations: true,
      selectedMode: "auto",
      supportedModes: ["auto", "structural", "deep"],
      adapterIsolation: "available",
      estimatedLimitations: ["repository_contents_not_scanned"],
      warnings: [],
      rootCapability: token,
      rootCapabilityExpiresInSeconds: 120,
    };
    expect(parseIndexPreflight(preflight).supportedModes).toEqual(["auto", "structural", "deep"]);
    expect(() => parseIndexPreflight({ ...preflight, selectedMode: "future" })).toThrow();
    expect(() => parseIndexPreflight({ ...preflight, supportedModes: ["auto", "auto"] })).toThrow();
  });

  it("parses bounded index admission and source-free diagnostics", () => {
    const admission = parseProjectIndexAdmission(indexAdmissionFixture());
    expect(admission).toMatchObject({
      repositoryId,
      operationId,
      semanticOperationId,
      state: "running",
      mode: "auto",
    });
    expect(admission.diagnostics).toEqual([
      { code: "adapter_degraded", message: "Semantic refinement will run separately." },
    ]);
    expect(() =>
      parseProjectIndexAdmission({
        ...indexAdmissionFixture(),
        diagnostics: Array.from({ length: 65 }, () => ({
          code: "bounded",
          message: "bounded",
        })),
      }),
    ).toThrow();
  });

  it("correlates operation status and maps additive states conservatively", () => {
    const operation = parseRepositoryOperation(operationFixture(), operationId);
    expect(operation).toMatchObject({
      operationId,
      state: "running",
      stage: "executing",
      recoveryClass: "not_applicable",
      cancellationRequested: false,
    });
    expect(
      parseRepositoryOperation(
        {
          ...operationFixture(),
          state: "future_running",
          stage: "future_stage",
          recoveryClass: "future_recovery",
          kind: "future_kind",
        },
        operationId,
      ),
    ).toMatchObject({
      state: "unknown",
      stage: "unknown",
      recoveryClass: "unknown",
      kind: "unknown",
    });
    expect(() => parseRepositoryOperation(operationFixture(), `op1_${"e".repeat(32)}`)).toThrow();
    expect(() =>
      parseRepositoryOperation({ ...operationFixture(), revision: "01" }, operationId),
    ).toThrow();
  });

  it("parses cancellation only when the nested operation remains correlated", () => {
    expect(
      parseOperationCancel(
        {
          schema: "rootlight.web-operation-cancel/1",
          accepted: true,
          operation: { ...operationFixture(), cancellationRequested: true, state: "cancelling" },
        },
        operationId,
      ),
    ).toMatchObject({
      accepted: true,
      operation: { state: "cancelling", cancellationRequested: true },
    });
    expect(() =>
      parseOperationCancel(
        {
          schema: "rootlight.web-operation-cancel/1",
          accepted: true,
          operation: { ...operationFixture(), operationId: `op1_${"f".repeat(32)}` },
        },
        operationId,
      ),
    ).toThrow();
  });
});

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const operationId = `op1_${"c".repeat(32)}`;
const semanticOperationId = `op1_${"d".repeat(32)}`;

function healthFixture() {
  return {
    webReady: true,
    daemonReady: true,
    protocolVersion: "1.10",
    lifecycle: "ready",
    acceptingOperations: true,
    activeOperations: 0,
    admittedOperations: 1,
    queuedOperations: 0,
    runningOperations: 0,
    activeConnections: 1,
    connectionLimit: 64,
    operationQueueLimit: 128,
    journalHealthy: true,
    catalogSchemaVersion: 4,
    endpointSchemaVersion: 1,
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

function indexAdmissionFixture() {
  return {
    schema: "rootlight.web-project-index/1",
    displayLabel: "rootlight",
    repositoryId,
    operationId,
    semanticOperationId,
    state: "running",
    revision: "1",
    mode: "auto",
    parentGenerationId: null,
    publishedGenerationId: null,
    discoveredInputs: "10",
    indexedFiles: "4",
    entities: "100",
    elapsedMicros: "2000",
    estimatedDiskBytes: "4096",
    diagnostics: [
      {
        code: "adapter_degraded",
        message: "Semantic refinement will run separately.",
      },
    ],
  };
}

function operationFixture() {
  return {
    schema: "rootlight.web-repository-operation/1",
    displayLabel: "rootlight",
    mode: "auto",
    ownedBySession: true,
    operationId,
    state: "running",
    revision: "2",
    completedUnits: 2,
    totalUnits: 4,
    kind: "repository_index",
    stage: "executing",
    detached: true,
    cancellationRequested: false,
    recoveryClass: "not_applicable",
    error: null,
    publishedGenerationId: null,
    semanticOperationId,
    startedUnixMs: "1",
    peakRssBytes: "2048",
    writtenBytes: "1024",
    filesExamined: "5",
    bytesExamined: "4096",
    indexStage: "indexing",
    retryAfterMs: 100,
  };
}
