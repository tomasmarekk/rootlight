// Builds small source-free graph pages and accumulated models for engine tests.
// Fixtures preserve the same strict identifier, counter, and continuation contracts as production.

import type {
  BrowserGraphNode,
  BrowserGraphPage,
} from "../src/features/graph/model/graph-contracts";
import type { GraphLayoutIdentity } from "../src/features/graph/model/graph-layout";
import { GraphPageAccumulator } from "../src/features/graph/model/graph-page-accumulator";
import type { GraphRenderModel } from "../src/features/graph/model/graph-model";
import { decodeGraphPage } from "../src/workers/graph-decoder-protocol";

/** Stable repository identifier shared by graph engine fixtures. */
export const graphRepositoryId = `repo1_${"a".repeat(32)}`;

/** Stable generation identifier shared by graph engine fixtures. */
export const graphGenerationId = `gen1_${"b".repeat(39)}`;

/** Stable projection token shared by graph engine fixtures. */
export const graphProjectionToken = "c".repeat(43);

/** Immutable layout identity used to prove deterministic initial placement. */
export const graphLayoutIdentity: GraphLayoutIdentity = {
  repositoryId: graphRepositoryId,
  generationId: graphGenerationId,
  view: "architecture",
  scopeFingerprint: "repository",
  layoutVersion: "atlas-v1",
};

/** Returns one of two ordered graph page wire fixtures. */
export function graphPageFixture(pageOrdinal: 0 | 1 = 0): BrowserGraphPage {
  const firstPage = pageOrdinal === 0;
  return {
    schema: "rootlight.web-graph-page/1" as const,
    projectionToken: graphProjectionToken,
    pageOrdinal,
    context: {
      repositoryId: graphRepositoryId,
      generationId: graphGenerationId,
      parentGenerationId: null,
      activeGeneration: true,
      structuralFreshness: "current",
      semanticFreshness: "current",
      tier: "tier_a",
      coverageStatus: "complete",
      skippedInputs: "0",
    },
    nodes: firstPage
      ? [graphNode(0, "src", "src", "file"), graphNode(1, "main", "src/main.rs", "symbol")]
      : [graphNode(2, "config", "src/config.rs", "file")],
    edges: firstPage
      ? [
          {
            sourceOrdinal: 0,
            targetOrdinal: 1,
            relation: "imports",
            weight: 2,
            confidence: 1_000,
            exact: true,
            inferred: false,
            evidenceCount: 2,
            overlay: "none",
          },
        ]
      : [
          {
            sourceOrdinal: 1,
            targetOrdinal: 2,
            relation: "calls",
            weight: 1,
            confidence: 700,
            exact: false,
            inferred: true,
            evidenceCount: 1,
            overlay: "none",
          },
        ],
    completeness: {
      state: firstPage ? "truncated" : "complete",
      limitingResources: firstPage ? [{ kind: "page_size", limit: "2", observed: "3" }] : [],
      continuation: firstPage ? "available" : "not_applicable",
      guidance: firstPage ? ["use_cursor"] : [],
    },
    effectiveBudget: {
      pageNodes: 200,
      pageEdges: 500,
      aggregateNodes: 512,
      aggregateEdges: 2_048,
    },
    returnedNodesCumulative: firstPage ? "2" : "3",
    returnedEdgesCumulative: firstPage ? "1" : "2",
    totalMatchingNodes: "3",
    totalMatchingEdges: "2",
    totalKnownNodes: "3",
    totalKnownEdges: "2",
    edgesOmittedForUnavailableEndpoints: "0",
    skippedForCoverage: "0",
    hasNextPage: firstPage,
  };
}

/** Creates an accumulated one-page or two-page renderer model. */
export function graphModelFixture(pageCount: 1 | 2 = 2): GraphRenderModel {
  const accumulator = new GraphPageAccumulator();
  accumulator.append(
    decodeGraphPage({
      type: "decode",
      jobId: 1,
      page: graphPageFixture(0),
      expectedRepositoryId: graphRepositoryId,
      expectedGenerationId: graphGenerationId,
      layoutIdentity: graphLayoutIdentity,
    }),
  );
  if (pageCount === 2) {
    accumulator.append(
      decodeGraphPage({
        type: "decode",
        jobId: 2,
        page: graphPageFixture(1),
        expectedRepositoryId: graphRepositoryId,
        expectedGenerationId: graphGenerationId,
        expectedProjectionToken: graphProjectionToken,
        layoutIdentity: graphLayoutIdentity,
      }),
    );
  }
  return accumulator.snapshot();
}

function graphNode(
  ordinal: number,
  label: string,
  path: string,
  kind: BrowserGraphNode["kind"],
): BrowserGraphNode {
  return {
    ordinal,
    stableId: `${kind}:${label}`,
    idKind: kind,
    label,
    path,
    kind,
    confidence: ordinal === 2 ? 650 : 1_000,
    generated: false,
    community: ordinal === 2 ? "configuration" : "core",
    component: null,
    symbolCount: 4 + ordinal,
    fanIn: ordinal,
    fanOut: 2,
    hotspotScore: ordinal * 30,
    evidence: "structural",
  };
}
