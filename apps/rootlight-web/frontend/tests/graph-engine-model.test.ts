// Verifies deterministic Worker preparation, ordered accumulation, and adjacency selection.

import { describe, expect, it } from "vitest";

import {
  graphConfidenceOpacity,
  encodeGraphVisuals,
  graphLinkWidth,
  graphNodeColor,
  graphPointSize,
  graphRelationColor,
} from "../src/features/graph/controller/visual-encoding";
import type { BrowserGraphNode } from "../src/features/graph/model/graph-contracts";
import {
  deriveGraphLayoutSeed,
  deterministicNodePosition,
  stableGraphHash,
} from "../src/features/graph/model/graph-layout";
import { GraphPageAccumulator } from "../src/features/graph/model/graph-page-accumulator";
import { projectGraphSelection } from "../src/features/graph/model/graph-model";
import { decodeGraphPage, graphPageTransferables } from "../src/workers/graph-decoder-protocol";
import {
  graphGenerationId,
  graphLayoutIdentity,
  graphModelFixture,
  graphPageFixture,
  graphProjectionToken,
  graphRepositoryId,
} from "./graph-engine-fixtures";

describe("graph engine model", () => {
  it("derives stable layout seeds and node positions only from immutable identity", () => {
    const seed = deriveGraphLayoutSeed(graphLayoutIdentity);
    const clusterHash = stableGraphHash("community:core");

    expect(deriveGraphLayoutSeed({ ...graphLayoutIdentity })).toBe(seed);
    expect(
      deriveGraphLayoutSeed({ ...graphLayoutIdentity, generationId: `gen1_${"d".repeat(39)}` }),
    ).not.toBe(seed);
    expect(deterministicNodePosition("file:src", clusterHash, seed)).toEqual(
      deterministicNodePosition("file:src", clusterHash, seed),
    );
    expect(deterministicNodePosition("file:other", clusterHash, seed)).not.toEqual(
      deterministicNodePosition("file:src", clusterHash, seed),
    );
  });

  it("validates a page before preparing deterministic transferable arrays", () => {
    const request = {
      type: "decode" as const,
      jobId: 1,
      page: graphPageFixture(0),
      expectedRepositoryId: graphRepositoryId,
      expectedGenerationId: graphGenerationId,
      layoutIdentity: graphLayoutIdentity,
    };
    const first = decodeGraphPage(request);
    const second = decodeGraphPage(request);

    expect(first.nodeOrdinals).toEqual(new Uint32Array([0, 1]));
    expect(first.pointPositions).toEqual(second.pointPositions);
    expect(first.linksByOrdinal).toEqual(new Uint32Array([0, 1]));
    expect(first.memoryBytes).toBeGreaterThan(0);
    expect(graphPageTransferables(first)).toHaveLength(11);
    expect(() =>
      decodeGraphPage({
        ...request,
        expectedGenerationId: `gen1_${"e".repeat(39)}`,
      }),
    ).toThrow("immutable generation");
    expect(() =>
      decodeGraphPage({
        ...request,
        layoutIdentity: {
          ...graphLayoutIdentity,
          generationId: `gen1_${"e".repeat(39)}`,
        },
      }),
    ).toThrow("layout identity");
    expect(() => decodeGraphPage({ ...request, jobId: 0 })).toThrow("positive safe integer");
  });

  it("derives clusters for component, directory, and unclustered node metadata", () => {
    const component = decodeSingleNode({ community: null, component: "runtime" });
    const directory = decodeSingleNode({ community: null, component: null });
    const unclustered = decodeSingleNode({
      community: null,
      component: null,
      path: null,
    });

    expect(component.pointClusterHashes[0]).not.toBe(directory.pointClusterHashes[0]);
    expect(directory.pointClusterHashes[0]).not.toBe(unclustered.pointClusterHashes[0]);
  });

  it("accumulates ordered pages without changing ordinals and precomputes adjacency", () => {
    const accumulator = new GraphPageAccumulator();
    const first = decode(0);
    const second = decode(1);
    accumulator.append(first);
    const firstSnapshot = accumulator.snapshot();
    accumulator.append(second);
    const model = accumulator.snapshot();

    expect(firstSnapshot.nodes).toHaveLength(2);
    expect(model.nodeOrdinals).toEqual(new Uint32Array([0, 1, 2]));
    expect(model.links).toEqual(new Float32Array([0, 1, 1, 2]));
    expect(model.clusterPositions.length).toBe(4);
    expect(model.returnedNodes).toBe("3");
    expect(model.completeness.state).toBe("complete");
    expect(accumulator.snapshot()).toBe(model);
    expect(projectGraphSelection(model, [1])).toEqual({
      selectedPointIndices: [1],
      connectedPointIndices: [0, 1, 2],
      connectedLinkIndices: [0, 1],
    });
    expect(projectGraphSelection(model, [99])).toEqual({
      selectedPointIndices: [],
      connectedPointIndices: [],
      connectedLinkIndices: [],
    });
  });

  it("fails closed on page order, projection substitution, duplicates, and memory budgets", () => {
    const outOfOrder = new GraphPageAccumulator();
    expect(() => outOfOrder.append(decode(1))).toThrow("retained projection");

    const projection = new GraphPageAccumulator();
    projection.append(decode(0));
    expect(() =>
      projection.append({
        ...decode(1),
        projectionToken: "d".repeat(43),
      }),
    ).toThrow("retained projection");

    const duplicate = new GraphPageAccumulator();
    const duplicatePage = decode(0);
    duplicate.append(duplicatePage);
    expect(() =>
      duplicate.append({
        ...decode(1),
        nodeOrdinals: new Uint32Array([1]),
      }),
    ).toThrow("duplicate node ordinal");

    const transactional = new GraphPageAccumulator();
    transactional.append(decode(0));
    expect(() =>
      transactional.append({
        ...decode(1),
        linksByOrdinal: new Uint32Array([1, 99]),
      }),
    ).toThrow("unavailable node ordinal");
    transactional.append(decode(1));
    expect(transactional.snapshot().nodes).toHaveLength(3);

    const counters = new GraphPageAccumulator();
    expect(() => counters.append({ ...decode(0), returnedNodesCumulative: "1" })).toThrow(
      "aggregate counters",
    );
    const shape = new GraphPageAccumulator();
    expect(() => shape.append({ ...decode(0), pointPositions: new Float32Array(1) })).toThrow(
      "typed-array lengths",
    );
    const memoryEstimate = new GraphPageAccumulator();
    expect(() =>
      memoryEstimate.append({ ...decode(0), memoryBytes: decode(0).memoryBytes + 1 }),
    ).toThrow("memory estimate");
    expect(() => new GraphPageAccumulator({ maximumNodes: 0 })).toThrow("positive safe integers");
    expect(() => new GraphPageAccumulator().snapshot()).toThrow("no pages");

    const memoryBound = new GraphPageAccumulator({ maximumMemoryBytes: 1 });
    expect(() => memoryBound.append(decode(0))).toThrow("client memory budget");

    projection.dispose();
    expect(projection.memoryBytes).toBe(0);
    expect(() => projection.snapshot()).toThrow("disposed");
  });

  it("clamps visual encoding and preserves unknown fallbacks", () => {
    const model = graphModelFixture();
    const baseNode = model.nodes[0];
    const edge = model.edges[0];
    if (baseNode === undefined || edge === undefined) {
      throw new Error("Graph fixture must contain node and edge metadata");
    }
    const largeNode = {
      ...baseNode,
      symbolCount: 0xffff_ffff,
      fanIn: 0xffff_ffff,
      hotspotScore: 0xffff_ffff,
    };

    expect(graphPointSize(largeNode)).toBe(18);
    expect(graphConfidenceOpacity(0)).toBe(0.18);
    expect(graphConfidenceOpacity(1_000)).toBe(1);
    expect(graphLinkWidth({ ...edge, weight: 0xffff_ffff })).toBe(4);
    expect(graphNodeColor("unknown")).toEqual([0.541, 0.58, 0.651, 1]);
    expect(graphRelationColor("unknown")).toEqual([0.541, 0.58, 0.651, 1]);

    const visuals = encodeGraphVisuals(
      [
        {
          ...baseNode,
          kind: "unknown",
          generated: true,
          symbolCount: null,
          fanIn: null,
          hotspotScore: null,
          confidence: 0,
        },
      ],
      [
        { ...edge, exact: false, inferred: false, confidence: 0 },
        { ...edge, exact: false, inferred: true, relation: "unknown" },
      ],
    );
    expect(visuals.pointShapes).toEqual(new Float32Array([3]));
    expect(visuals.pointSizes[0]).toBe(4.5);
    expect(visuals.linkStyles).toEqual(new Float32Array([2, 1]));
  });
});

function decode(pageOrdinal: 0 | 1) {
  return decodeGraphPage({
    type: "decode",
    jobId: pageOrdinal + 1,
    page: graphPageFixture(pageOrdinal),
    expectedRepositoryId: graphRepositoryId,
    expectedGenerationId: graphGenerationId,
    expectedProjectionToken: pageOrdinal === 0 ? undefined : graphProjectionToken,
    layoutIdentity: graphLayoutIdentity,
  });
}

function decodeSingleNode(overrides: Partial<BrowserGraphNode>) {
  const page = graphPageFixture(0);
  const node = page.nodes[0];
  if (node === undefined) {
    throw new Error("Graph fixture must contain a node");
  }
  return decodeGraphPage({
    type: "decode",
    jobId: 1,
    page: {
      ...page,
      nodes: [{ ...node, ...overrides }],
      edges: [],
      completeness: {
        state: "complete",
        limitingResources: [],
        continuation: "not_applicable",
        guidance: [],
      },
      returnedNodesCumulative: "1",
      returnedEdgesCumulative: "0",
      totalMatchingNodes: "1",
      totalMatchingEdges: "0",
      totalKnownNodes: "1",
      totalKnownEdges: "0",
      hasNextPage: false,
    },
    expectedRepositoryId: graphRepositoryId,
    expectedGenerationId: graphGenerationId,
    layoutIdentity: graphLayoutIdentity,
  });
}
