// Verifies exact-generation graph pages before worker or GPU allocation.

import { describe, expect, it } from "vitest";

import {
  parseBrowserGraphPage,
  parseBrowserGraphRelease,
} from "../src/features/graph/model/graph-contracts";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const projectionToken = "c".repeat(43);

describe("graph browser contracts", () => {
  it("accepts a correlated bounded page and preserves exact counts", () => {
    const parsed = parseBrowserGraphPage(pageFixture(), repositoryId, generationId);

    expect(parsed.context.generationId).toBe(generationId);
    expect(parsed.nodes[0]).toMatchObject({
      ordinal: 0,
      stableId: "file:root",
      kind: "file",
      confidence: 1_000,
    });
    expect(parsed.returnedNodesCumulative).toBe("1");
    expect(parsed.hasNextPage).toBe(false);
  });

  it("rejects generation, projection, endpoint, and counter substitution", () => {
    expect(() =>
      parseBrowserGraphPage(pageFixture(), repositoryId, `gen1_${"d".repeat(39)}`),
    ).toThrow();
    expect(() =>
      parseBrowserGraphPage(pageFixture(), repositoryId, generationId, "e".repeat(43)),
    ).toThrow();
    expect(() =>
      parseBrowserGraphPage(
        {
          ...pageFixture(),
          edges: [
            {
              sourceOrdinal: 0,
              targetOrdinal: 1,
              relation: "imports",
              weight: 1,
              confidence: 1_000,
              exact: true,
              inferred: false,
              evidenceCount: 1,
              overlay: "none",
            },
          ],
          returnedEdgesCumulative: "1",
          totalMatchingEdges: "1",
        },
        repositoryId,
        generationId,
      ),
    ).toThrow();
    expect(() =>
      parseBrowserGraphPage(
        { ...pageFixture(), returnedNodesCumulative: "0" },
        repositoryId,
        generationId,
      ),
    ).toThrow();
  });

  it("maps additive graph values to unknown but rejects type confusion", () => {
    const page = pageFixture();
    const firstNode = page.nodes[0];
    if (firstNode === undefined) {
      throw new Error("Fixture must contain one graph node");
    }
    page.nodes[0] = {
      ...firstNode,
      kind: "future_kind",
      evidence: "future_evidence",
    };
    const parsed = parseBrowserGraphPage(page, repositoryId, generationId);
    expect(parsed.nodes[0]?.kind).toBe("unknown");
    expect(parsed.nodes[0]?.evidence).toBe("unknown");

    expect(() =>
      parseBrowserGraphPage(
        {
          ...pageFixture(),
          nodes: [{ ...pageFixture().nodes[0], kind: 7 }],
        },
        repositoryId,
        generationId,
      ),
    ).toThrow();
  });

  it("requires continuation metadata to agree with the browser handle", () => {
    expect(() =>
      parseBrowserGraphPage(
        {
          ...pageFixture(),
          hasNextPage: true,
          completeness: {
            ...pageFixture().completeness,
            continuation: "unavailable",
          },
        },
        repositoryId,
        generationId,
      ),
    ).toThrow();
    expect(
      parseBrowserGraphRelease({
        schema: "rootlight.web-graph-release/1",
        released: true,
      }).released,
    ).toBe(true);
  });
});

function pageFixture() {
  return {
    schema: "rootlight.web-graph-page/1" as const,
    projectionToken,
    pageOrdinal: 0,
    context: {
      repositoryId,
      generationId,
      parentGenerationId: null,
      activeGeneration: true,
      structuralFreshness: "current",
      semanticFreshness: "stale",
      tier: "tier_b",
      coverageStatus: "complete",
      skippedInputs: "0",
    },
    nodes: [
      {
        ordinal: 0,
        stableId: "file:root",
        idKind: "file",
        label: "root.rs",
        path: "src/root.rs",
        kind: "file",
        confidence: 1_000,
        generated: false,
        community: "core",
        component: null,
        symbolCount: 10,
        fanIn: 1,
        fanOut: 2,
        hotspotScore: 50,
        evidence: "structural",
      },
    ],
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
