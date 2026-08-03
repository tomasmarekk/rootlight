// Exercises bounded response handling and fail-closed session bootstrap parsing.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("browser API client", () => {
  it("restores an existing HttpOnly-cookie session", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValue(jsonResponse({ csrfToken: "csrf", idleTtlSeconds: 1_800 }));
    vi.stubGlobal("fetch", fetchMock);
    const { initializeSession } = await import("../src/api/client");

    await expect(initializeSession()).resolves.toEqual({
      csrfToken: "csrf",
      idleTtlSeconds: 1_800,
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/session");
  });

  it("removes and rejects a malformed bootstrap fragment before fetch", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);
    window.history.replaceState(null, "", "/#bootstrap=too-short");
    const { initializeSession } = await import("../src/api/client");

    await expect(initializeSession()).rejects.toMatchObject({
      status: 401,
      code: "invalid_bootstrap",
    });
    expect(window.location.hash).toBe("");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("rejects oversized, malformed, and unauthorized health responses", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response("{}", {
          status: 200,
          headers: { "content-length": String(1024 * 1024 + 1) },
        }),
      )
      .mockResolvedValueOnce(new Response("not-json", { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);
    const { fetchHealth } = await import("../src/api/client");

    await expect(fetchHealth()).rejects.toMatchObject({ code: "response_too_large" });
    await expect(fetchHealth()).rejects.toMatchObject({ code: "invalid_response" });
    await expect(fetchHealth()).rejects.toMatchObject({ code: "session_required" });
  });

  it("keeps only bounded public error codes from the host envelope", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ error: { code: "index_limit_reached" } }), {
          status: 429,
          headers: { "content-type": "application/json" },
        }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ error: { code: "unsafe-error detail" } }), {
          status: 502,
          headers: { "content-type": "application/json" },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const { fetchHealth } = await import("../src/api/client");

    await expect(fetchHealth()).rejects.toMatchObject({ code: "index_limit_reached" });
    await expect(fetchHealth()).rejects.toMatchObject({ code: "request_failed" });
  });

  it("encodes bounded catalog filters and continuation without merging snapshots", async () => {
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      jsonResponse({
        schema: "rootlight.web-project-catalog-page/1",
        projects: [],
        snapshot: "snapshot-token",
        nextAfter: null,
        totalCount: "0",
        truncated: false,
        sortVersion: 1,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const { fetchProjects } = await import("../src/api/client");

    await fetchProjects({
      pageSize: 50,
      query: "root light",
      states: ["ready", "degraded"],
      snapshot: "snapshot-token",
      after: "cursor-token",
      sortVersion: 1,
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/projects?page_size=50&query=root+light&state=ready&state=degraded&snapshot=snapshot-token&after=cursor-token&sort_version=1",
    );
  });

  it("sends filesystem capabilities only through authenticated CSRF mutations", async () => {
    const token = "a".repeat(43);
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf", idleTtlSeconds: 1_800 }))
      .mockResolvedValueOnce(
        jsonResponse({
          schema: "rootlight.web-filesystem-browse/1",
          browseToken: token,
          label: "rootlight",
          depth: 0,
          maximumDepth: 32,
          breadcrumbs: [{ label: "rootlight", browseToken: token }],
          directories: [],
          nextCursor: null,
        }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const { browseFilesystem, initializeSession } = await import("../src/api/client");
    await initializeSession();

    await browseFilesystem({
      browseToken: token,
      action: { type: "current" },
      pageSize: 64,
      filter: "src",
    });

    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/v1/filesystem/browse");
    expect(fetchMock.mock.calls[1]?.[1]).toMatchObject({
      method: "POST",
      credentials: "same-origin",
      headers: {
        "content-type": "application/json",
        "x-rootlight-csrf": "csrf",
      },
    });
    const requestBody = fetchMock.mock.calls[1]?.[1]?.body;
    expect(typeof requestBody).toBe("string");
    expect(JSON.parse(requestBody as string)).toEqual({
      browseToken: token,
      action: { type: "current" },
      pageSize: 64,
      filter: "src",
    });
  });

  it("submits detached indexes and follows bounded operation revisions with CSRF", async () => {
    const operationId = `op1_${"c".repeat(32)}`;
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf", idleTtlSeconds: 1_800 }))
      .mockResolvedValueOnce(jsonResponse(indexAdmissionFixture(operationId)))
      .mockResolvedValueOnce(jsonResponse(operationFixture(operationId)))
      .mockResolvedValueOnce(
        jsonResponse({
          schema: "rootlight.web-operation-cancel/1",
          accepted: true,
          operation: {
            ...operationFixture(operationId),
            state: "cancelling",
            revision: "3",
            cancellationRequested: true,
          },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const {
      cancelIndexOperation,
      createClientRequestId,
      fetchIndexOperation,
      initializeSession,
      submitProjectIndex,
    } = await import("../src/api/client");
    await initializeSession();
    const requestId = createClientRequestId();

    expect(requestId).toMatch(/^idx_[a-f0-9]{48}$/u);
    await submitProjectIndex({
      rootCapability: "a".repeat(43),
      mode: "auto",
      clientRequestId: requestId,
    });
    await fetchIndexOperation(operationId, { waitMs: 15_000, afterRevision: "1" });
    await cancelIndexOperation(operationId);

    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/v1/projects/index");
    expect(JSON.parse(fetchMock.mock.calls[1]?.[1]?.body as string)).toEqual({
      rootCapability: "a".repeat(43),
      mode: "auto",
      detached: true,
      clientRequestId: requestId,
    });
    expect(fetchMock.mock.calls[2]?.[0]).toBe(
      `/api/v1/operations/${operationId}?wait_ms=15000&after_revision=1`,
    );
    expect(fetchMock.mock.calls[3]?.[0]).toBe(`/api/v1/operations/${operationId}/cancel`);
    expect(fetchMock.mock.calls[3]?.[1]).toMatchObject({
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-rootlight-csrf": "csrf",
      },
    });
  });

  it("keeps daemon graph cursors behind CSRF-bound browser projection handles", async () => {
    const repositoryId = `repo1_${"a".repeat(32)}`;
    const generationId = `gen1_${"b".repeat(39)}`;
    const projectionToken = "c".repeat(43);
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf", idleTtlSeconds: 1_800 }))
      .mockResolvedValueOnce(
        jsonResponse(graphPageFixture(repositoryId, generationId, projectionToken, 0)),
      )
      .mockResolvedValueOnce(
        jsonResponse(graphPageFixture(repositoryId, generationId, projectionToken, 1)),
      )
      .mockResolvedValueOnce(
        jsonResponse({ schema: "rootlight.web-graph-release/1", released: true }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const { fetchNextGraphPage, initializeSession, openGraphProjection, releaseGraphProjection } =
      await import("../src/api/client");
    await initializeSession();

    await openGraphProjection({
      repositoryId,
      generationId,
      view: "architecture",
      minConfidence: 500,
      budgetProfile: "balanced",
    });
    await fetchNextGraphPage(projectionToken, repositoryId, generationId);
    await releaseGraphProjection(projectionToken);

    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/v1/graph/projections");
    expect(fetchMock.mock.calls[1]?.[1]).toMatchObject({
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-rootlight-csrf": "csrf",
      },
    });
    expect(JSON.parse(fetchMock.mock.calls[1]?.[1]?.body as string)).toEqual({
      repositoryId,
      generationId,
      view: "architecture",
      minConfidence: 500,
      budgetProfile: "balanced",
    });
    expect(fetchMock.mock.calls[2]?.[0]).toBe(`/api/v1/graph/projections/${projectionToken}/next`);
    expect(fetchMock.mock.calls[3]?.[0]).toBe(`/api/v1/graph/projections/${projectionToken}`);
    expect(fetchMock.mock.calls[3]?.[1]).toMatchObject({ method: "DELETE" });
    expect(JSON.stringify(fetchMock.mock.calls)).not.toContain("daemon-cursor");
  });
});

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function indexAdmissionFixture(operationId: string) {
  return {
    schema: "rootlight.web-project-index/1",
    displayLabel: "rootlight",
    repositoryId: `repo1_${"a".repeat(32)}`,
    operationId,
    semanticOperationId: null,
    state: "queued",
    revision: "1",
    mode: "auto",
    parentGenerationId: null,
    publishedGenerationId: null,
    discoveredInputs: "0",
    indexedFiles: "0",
    entities: "0",
    elapsedMicros: "0",
    estimatedDiskBytes: "0",
    diagnostics: [],
  };
}

function operationFixture(operationId: string) {
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
    semanticOperationId: null,
    startedUnixMs: "1",
    peakRssBytes: "2",
    writtenBytes: "3",
    filesExamined: "4",
    bytesExamined: "5",
    indexStage: "indexing",
    retryAfterMs: 100,
  };
}

function graphPageFixture(
  repositoryId: string,
  generationId: string,
  projectionToken: string,
  pageOrdinal: number,
) {
  return {
    schema: "rootlight.web-graph-page/1",
    projectionToken,
    pageOrdinal,
    context: {
      repositoryId,
      generationId,
      parentGenerationId: null,
      activeGeneration: true,
      structuralFreshness: "current",
      semanticFreshness: "current",
      tier: "tier_b",
      coverageStatus: "complete",
      skippedInputs: "0",
    },
    nodes:
      pageOrdinal === 0
        ? [
            {
              ordinal: 0,
              stableId: "file:root",
              idKind: "file",
              label: "root.rs",
              path: "src/root.rs",
              kind: "file",
              confidence: 1_000,
              generated: false,
              community: null,
              component: null,
              symbolCount: null,
              fanIn: null,
              fanOut: null,
              hotspotScore: null,
              evidence: "structural",
            },
          ]
        : [],
    edges: [],
    completeness: {
      state: "complete",
      limitingResources: [],
      continuation: "not_applicable",
      guidance: [],
    },
    effectiveBudget: {
      pageNodes: 127,
      pageEdges: 300,
      aggregateNodes: 250,
      aggregateEdges: 750,
    },
    returnedNodesCumulative: "1",
    returnedEdgesCumulative: "0",
    totalMatchingNodes: "1",
    totalMatchingEdges: "0",
    totalKnownNodes: "1",
    totalKnownEdges: "0",
    edgesOmittedForUnavailableEndpoints: "0",
    skippedForCoverage: "0",
    hasNextPage: false,
  };
}
