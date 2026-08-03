// Provides deterministic, source-free browser fixtures for quality and performance gates.
// The maximum profile mirrors the production architecture/files cap, not roadmap stress bands.

import { expect, type Page, type Request, type Response } from "@playwright/test";

/** Stable synthetic repository identity shared by browser fixtures. */
export const repositoryId = `repo1_${"a".repeat(32)}`;
const activeGenerationId = `gen1_${"b".repeat(39)}`;
/** Historical generation used to exercise exact-generation churn. */
export const historicalGenerationId = `gen1_${"c".repeat(39)}`;
/** One-time bootstrap route used by the deterministic local session fixture. */
export const bootstrapUrl = `/#bootstrap=${"a".repeat(43)}`;

type QualityApplicationOptions = {
  edgeCount?: number;
  nodeCount?: number;
  projectCount?: 0 | 1;
};

type QualityApplication = {
  activeProjectionCount: () => number;
  graphOpenCount: () => number;
  graphReleaseCount: () => number;
};

type BrowserQualityMonitor = {
  assertClean: () => Promise<void>;
  externalRequests: readonly string[];
};

type SyntheticGraphNode = {
  ordinal: number;
  stableId: string;
  idKind: string;
  label: string;
  path: string;
  kind: string;
  confidence: number;
  generated: boolean;
  community: string;
  component: null;
  symbolCount: null;
  fanIn: number;
  fanOut: number;
  hotspotScore: number;
  evidence: string;
};

type SyntheticGraphEdge = {
  sourceOrdinal: number;
  targetOrdinal: number;
  relation: string;
  weight: number;
  confidence: number;
  exact: boolean;
  inferred: boolean;
  evidenceCount: number;
  overlay: string;
};

const health = {
  webReady: true,
  daemonReady: true,
  protocolVersion: "1.10",
  lifecycle: "ready",
  acceptingOperations: true,
  activeOperations: 0,
  admittedOperations: 0,
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

/** Installs a bounded local application boundary and exposes projection lifecycle counters. */
export async function installQualityApplication(
  page: Page,
  options: QualityApplicationOptions = {},
): Promise<QualityApplication> {
  const edgeCount = options.edgeCount ?? 1_000;
  const nodeCount = options.nodeCount ?? 250;
  const projects = options.projectCount === 0 ? [] : [projectSummary()];
  let activeProjectionCount = 0;
  let graphOpenCount = 0;
  let graphReleaseCount = 0;
  const retainedPages = new Map<string, ReturnType<typeof syntheticGraphPages>>();

  await page.route("**/api/v1/session/bootstrap", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ csrfToken: "csrf", idleTtlSeconds: 1_800 }),
    });
  });
  await page.route("**/api/v1/health", async (route) => {
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(health) });
  });
  await page.route("**/api/v1/projects**", async (route) => {
    const url = new URL(route.request().url());
    const body =
      url.pathname === "/api/v1/projects"
        ? {
            schema: "rootlight.web-project-catalog-page/1",
            projects,
            snapshot: "synthetic-catalog",
            nextAfter: null,
            totalCount: String(projects.length),
            truncated: false,
            sortVersion: 1,
          }
        : projectDetail(resolveRequestedGeneration(url));
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.route("**/api/v1/graph/projections**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (request.method() === "DELETE") {
      const projectionToken = url.pathname.split("/").at(-1) ?? "";
      graphReleaseCount += 1;
      activeProjectionCount = Math.max(0, activeProjectionCount - 1);
      retainedPages.delete(projectionToken);
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          schema: "rootlight.web-graph-release/1",
          projectionToken,
          released: true,
        }),
      });
      return;
    }
    if (url.pathname.endsWith("/next")) {
      const projectionToken = url.pathname.split("/").at(-2) ?? "";
      const retained = retainedPages.get(projectionToken);
      const nextPage = retained?.[1];
      if (nextPage === undefined) {
        await route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
        return;
      }
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(nextPage),
      });
      return;
    }

    const input = request.postDataJSON() as { generationId?: string };
    graphOpenCount += 1;
    activeProjectionCount += 1;
    const projectionToken = projectionTokenFor(graphOpenCount);
    const pages = syntheticGraphPages({
      edgeCount,
      generationId: input.generationId ?? activeGenerationId,
      nodeCount,
      projectionToken,
    });
    retainedPages.set(projectionToken, pages);
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(pages[0]),
    });
  });

  return {
    activeProjectionCount: () => activeProjectionCount,
    graphOpenCount: () => graphOpenCount,
    graphReleaseCount: () => graphReleaseCount,
  };
}

/**
 * Captures page errors, CSP blocks, external requests, and failed static resources.
 *
 * Call `assertClean` only after the exercised state has become idle.
 */
export function monitorBrowserQuality(page: Page): BrowserQualityMonitor {
  const cspMessages: string[] = [];
  const externalRequests: string[] = [];
  const pageErrors: string[] = [];
  const staticFailures: string[] = [];
  const localOrigin = "http://127.0.0.1:4173";

  page.on("console", (message) => {
    const text = message.text();
    if (
      /content[- ]security[- ]policy/iu.test(text) ||
      /refused to (?:apply|connect|execute|load).*directive/iu.test(text)
    ) {
      cspMessages.push(text);
    }
  });
  page.on("pageerror", (error) => {
    pageErrors.push(error.message);
  });
  page.on("request", (request: Request) => {
    const url = request.url();
    if (/^https?:/u.test(url) && !url.startsWith(localOrigin)) {
      externalRequests.push(url);
    }
  });
  page.on("response", (response: Response) => {
    if (
      response.status() >= 400 &&
      ["font", "image", "script", "stylesheet"].includes(response.request().resourceType())
    ) {
      staticFailures.push(`${String(response.status())} ${response.url()}`);
    }
  });

  return {
    assertClean: async () => {
      await expect.poll(() => pageErrors).toEqual([]);
      expect(cspMessages).toEqual([]);
      expect(externalRequests).toEqual([]);
      expect(staticFailures).toEqual([]);
    },
    externalRequests,
  };
}

/** Requires CSP-compatible markup and durable names for visible data-entry controls. */
export async function expectPrimaryMarkupQuality(page: Page): Promise<void> {
  const inlineStyles = await page
    .locator("[style]")
    .evaluateAll((elements) => elements.map((element) => element.outerHTML.slice(0, 500)));
  expect(inlineStyles).toEqual([]);
  const unnamedControls = await page
    .locator(
      'input:not([type="button"]):not([type="checkbox"]):not([type="radio"]), select, textarea',
    )
    .evaluateAll((controls) =>
      controls
        .filter((control) => {
          const element = control as HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement;
          return element.getClientRects().length > 0 && !element.id.trim() && !element.name.trim();
        })
        .map((control) => control.outerHTML),
    );
  expect(unnamedControls).toEqual([]);
}

function resolveRequestedGeneration(url: URL) {
  const requested = url.searchParams.get("generation");
  return requested === historicalGenerationId ? historicalGenerationId : activeGenerationId;
}

function projectSummary() {
  return {
    repositoryId,
    activeGenerationId,
    displayName: "Synthetic Atlas",
    alias: null,
    generationCount: "2",
    lifecycleState: "ready",
    languages: ["synthetic"],
    structuralFreshness: "current",
    semanticFreshness: "current",
    coverage: [
      {
        language: "synthetic",
        tier: "tier_a",
        status: "complete",
        discoveredFiles: "250",
        indexedFiles: "250",
      },
    ],
  };
}

function projectDetail(generationId: string) {
  return {
    schema: "rootlight.web-project-detail/1",
    repositoryId,
    displayName: "Synthetic Atlas",
    alias: null,
    resolvedGenerationId: generationId,
    activeGenerationId,
    parentGenerationId: null,
    activeParentGenerationId: null,
    activeStructuralFreshness: "current",
    activeSemanticFreshness: "current",
    structuralFreshness: "current",
    semanticFreshness: "current",
    lifecycleState: "ready",
    publicationState: "published",
    coverage: projectSummary().coverage,
    operations: [],
  };
}

function syntheticGraphPages(input: {
  edgeCount: number;
  generationId: string;
  nodeCount: number;
  projectionToken: string;
}) {
  const nodes: SyntheticGraphNode[] = Array.from({ length: input.nodeCount }, (_, ordinal) => ({
    ordinal,
    stableId: syntheticSymbolId(ordinal),
    idKind: "symbol",
    label: `component-${String(ordinal).padStart(3, "0")}`,
    path: `synthetic/module-${String(ordinal).padStart(3, "0")}`,
    kind: "symbol",
    confidence: 900,
    generated: false,
    community: `community-${String(ordinal % 10)}`,
    component: null,
    symbolCount: null,
    fanIn: 4,
    fanOut: 4,
    hotspotScore: ordinal % 100,
    evidence: "structural",
  }));
  const edges: SyntheticGraphEdge[] = Array.from({ length: input.edgeCount }, (_, edgeOrdinal) => {
    const sourceOrdinal = edgeOrdinal % input.nodeCount;
    const targetOrdinal =
      (sourceOrdinal + 1 + Math.floor(edgeOrdinal / input.nodeCount)) % input.nodeCount;
    return {
      sourceOrdinal,
      targetOrdinal,
      relation: "calls",
      weight: 1,
      confidence: 900,
      exact: true,
      inferred: false,
      evidenceCount: 1,
      overlay: "none",
    };
  });
  const firstNodes = nodes.slice(0, 200);
  const remainingNodes = nodes.slice(200);
  const firstEdges =
    remainingNodes.length === 0
      ? edges
      : edges
          .filter(
            (edge) =>
              edge.sourceOrdinal < firstNodes.length && edge.targetOrdinal < firstNodes.length,
          )
          .slice(0, 500);
  const firstEdgeKeys = new Set(
    firstEdges.map((edge) => `${String(edge.sourceOrdinal)}:${String(edge.targetOrdinal)}`),
  );
  const remainingEdges = edges
    .filter(
      (edge) => !firstEdgeKeys.has(`${String(edge.sourceOrdinal)}:${String(edge.targetOrdinal)}`),
    )
    .slice(0, input.edgeCount - firstEdges.length);
  const pages = [
    graphPage({
      ...input,
      edges: firstEdges,
      hasNextPage: remainingNodes.length > 0,
      nodes: firstNodes,
      pageOrdinal: 0,
      returnedEdgesCumulative: firstEdges.length,
      returnedNodesCumulative: firstNodes.length,
    }),
  ];
  if (remainingNodes.length > 0) {
    pages.push(
      graphPage({
        ...input,
        edges: remainingEdges,
        hasNextPage: false,
        nodes: remainingNodes,
        pageOrdinal: 1,
        returnedEdgesCumulative: firstEdges.length + remainingEdges.length,
        returnedNodesCumulative: firstNodes.length + remainingNodes.length,
      }),
    );
  }
  return pages;
}

function graphPage(input: {
  edgeCount: number;
  edges: SyntheticGraphEdge[];
  generationId: string;
  hasNextPage: boolean;
  nodeCount: number;
  nodes: SyntheticGraphNode[];
  pageOrdinal: number;
  projectionToken: string;
  returnedEdgesCumulative: number;
  returnedNodesCumulative: number;
}) {
  return {
    schema: "rootlight.web-graph-page/1",
    projectionToken: input.projectionToken,
    pageOrdinal: input.pageOrdinal,
    context: {
      repositoryId,
      generationId: input.generationId,
      parentGenerationId: null,
      activeGeneration: input.generationId === activeGenerationId,
      structuralFreshness: "current",
      semanticFreshness: "current",
      tier: "tier_a",
      coverageStatus: "complete",
      skippedInputs: "0",
    },
    nodes: input.nodes,
    edges: input.edges,
    completeness: {
      state: input.hasNextPage ? "truncated" : "complete",
      limitingResources: [],
      continuation: input.hasNextPage ? "available" : "not_applicable",
      guidance: input.hasNextPage ? ["use_cursor"] : [],
    },
    effectiveBudget: {
      pageNodes: Math.min(input.nodeCount, 200),
      pageEdges: Math.min(input.edgeCount, 500),
      aggregateNodes: input.nodeCount,
      aggregateEdges: input.edgeCount,
    },
    returnedNodesCumulative: String(input.returnedNodesCumulative),
    returnedEdgesCumulative: String(input.returnedEdgesCumulative),
    totalMatchingNodes: String(input.nodeCount),
    totalMatchingEdges: String(input.edgeCount),
    totalKnownNodes: String(input.nodeCount),
    totalKnownEdges: String(input.edgeCount),
    edgesOmittedForUnavailableEndpoints: "0",
    skippedForCoverage: "0",
    hasNextPage: input.hasNextPage,
  };
}

function syntheticSymbolId(ordinal: number) {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  const high = alphabet[Math.floor(ordinal / alphabet.length)] ?? "a";
  const low = alphabet[ordinal % alphabet.length] ?? "a";
  return `sym1_${"a".repeat(37)}${high}${low}`;
}

function projectionTokenFor(index: number) {
  const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
  return (alphabet[index % alphabet.length] ?? "p").repeat(43);
}
