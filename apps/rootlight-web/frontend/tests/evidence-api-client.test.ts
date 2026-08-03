// Verifies that evidence requests stay exact-generation and source capabilities never enter URLs.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const symbolId = `sym1_${"c".repeat(39)}`;
const capability = "d".repeat(43);

beforeEach(() => {
  vi.resetModules();
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("evidence API client", () => {
  it("uses authenticated typed routes and sends source only after an explicit mutation", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({ csrfToken: "csrf", idleTtlSeconds: 1_800 }))
      .mockResolvedValueOnce(jsonResponse(nodeDetail()))
      .mockResolvedValueOnce(jsonResponse(relationships()))
      .mockResolvedValueOnce(jsonResponse(sourceRead()))
      .mockResolvedValueOnce(jsonResponse(changeImpact()));
    vi.stubGlobal("fetch", fetchMock);
    const { fetchNodeDetail, fetchRelationships, initializeSession, readSource, runChangeImpact } =
      await import("../src/api/client");
    await initializeSession();

    await fetchNodeDetail(repositoryId, generationId, symbolId);
    await fetchRelationships({
      repositoryId,
      generationId,
      seedIds: [symbolId],
      relations: ["calls"],
      direction: "both",
      minimumConfidence: 500,
    });
    expect(JSON.stringify(fetchMock.mock.calls.slice(0, 3))).not.toContain(capability);

    await readSource({
      repositoryId,
      generationId,
      capability,
      encoding: "utf8",
    });
    await runChangeImpact({
      repositoryId,
      generationId,
      changedSymbolIds: [symbolId],
      maximumDepth: 3,
      minimumConfidence: 500,
      includeTests: true,
    });

    expect(fetchMock.mock.calls[1]?.[0]).toBe(
      `/api/v1/projects/${repositoryId}/nodes/${symbolId}?generation=${generationId}&kind=symbol`,
    );
    expect(fetchMock.mock.calls[1]?.[1]).toMatchObject({
      credentials: "same-origin",
      cache: "no-store",
    });
    expect(fetchMock.mock.calls[2]?.[0]).toBe(`/api/v1/projects/${repositoryId}/relationships`);
    expect(JSON.parse(fetchMock.mock.calls[2]?.[1]?.body as string)).toEqual({
      schema: "rootlight.web-relationships-request/1",
      generationId,
      seedIds: [symbolId],
      relations: ["calls"],
      direction: "both",
      minConfidence: 500,
      maxResults: 100,
      pageOffset: "0",
    });
    expect(fetchMock.mock.calls[3]?.[0]).toBe(`/api/v1/projects/${repositoryId}/source`);
    expect(fetchMock.mock.calls[3]?.[0]).not.toContain(capability);
    expect(fetchMock.mock.calls[3]?.[1]).toMatchObject({
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-rootlight-csrf": "csrf",
      },
    });
    expect(JSON.parse(fetchMock.mock.calls[3]?.[1]?.body as string)).toMatchObject({
      schema: "rootlight.web-source-request/1",
      generationId,
      sourceCapability: capability,
      encoding: "utf8",
    });
    expect(JSON.parse(fetchMock.mock.calls[4]?.[1]?.body as string)).toEqual({
      schema: "rootlight.web-change-impact-request/1",
      generationId,
      changedSymbolIds: [symbolId],
      maxDepth: 3,
      minConfidence: 500,
      includeTests: true,
      maxDependents: 200,
    });
  });
});

function nodeDetail() {
  return {
    schema: "rootlight.web-node-detail/1",
    repositoryId,
    generationId,
    nodeId: symbolId,
    idKind: "symbol",
    kind: "function",
    displayName: "run",
    qualifiedName: null,
    signature: null,
    language: "rust",
    tier: "tier_a",
    confidence: 900,
    provider: "scip",
    evidence: "definition",
    outboundExact: "0",
    outboundCandidates: "0",
    inboundExact: "0",
    inboundCandidates: "0",
    referenceCount: "0",
    generated: null,
    sourceReferences: [{ capability, expiresInSeconds: 60 }],
    context: context(),
    completeness: complete(),
  };
}

function relationships() {
  return {
    schema: "rootlight.web-relationships/1",
    context: context(),
    groups: [],
    returnedEdges: "0",
    totalEdges: "0",
    exact: true,
    truncated: false,
    nextPageOffset: null,
    completeness: complete(),
  };
}

function sourceRead() {
  return {
    schema: "rootlight.web-source/1",
    repositoryId,
    generationId,
    chunks: [],
    totalSourceBytes: "0",
    truncated: false,
    context: context(),
    completeness: complete(),
  };
}

function changeImpact() {
  return {
    schema: "rootlight.web-change-impact/1",
    context: context(),
    resolvedChanges: [{ symbolId, fileId: null, classification: "resolved", kind: "function" }],
    impacted: [],
    tests: [],
    riskSummary: {
      level: "low",
      reasons: [],
      coverage: "complete",
      breakingSurface: false,
      fanout: 0,
      dynamicBlindSpots: false,
    },
    completeness: complete(),
  };
}

function context() {
  return {
    repositoryId,
    generationId,
    parentGenerationId: null,
    activeGeneration: true,
    structuralFreshness: "current",
    semanticFreshness: "current",
    tier: "tier_a",
    coverageStatus: "complete",
    skippedInputs: "0",
    usage: {
      rows: "0",
      edges: "0",
      results: "0",
      sourceBytes: "0",
      jsonBytes: "0",
      estimatedTokens: "0",
      tokenAccountingProfile: null,
      memoryBytes: null,
      elapsedMicros: "0",
    },
  };
}

function complete() {
  return {
    state: "complete",
    limitingResources: [],
    continuation: "not_applicable",
    guidance: [],
  };
}

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}
