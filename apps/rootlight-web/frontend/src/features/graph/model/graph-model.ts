// Defines the immutable renderer model shared by the Worker, accumulator, and controller.
// Only source-free graph metadata crosses this browser boundary.

import type {
  BrowserGraphCompleteness,
  BrowserGraphContext,
  BrowserGraphEdge,
  BrowserGraphNode,
} from "./graph-contracts";

/** A validated, transferable page prepared outside the main thread. */
export type PreparedGraphPage = {
  projectionToken: string;
  pageOrdinal: number;
  context: BrowserGraphContext;
  nodes: readonly BrowserGraphNode[];
  edges: readonly BrowserGraphEdge[];
  nodeOrdinals: Uint32Array;
  pointPositions: Float32Array;
  pointColors: Float32Array;
  pointSizes: Float32Array;
  pointShapes: Float32Array;
  pointClusterHashes: Uint32Array;
  linksByOrdinal: Uint32Array;
  linkColors: Float32Array;
  linkWidths: Float32Array;
  linkStyles: Float32Array;
  labelPriorities: Float32Array;
  completeness: BrowserGraphCompleteness;
  effectiveBudget: {
    aggregateNodes: number;
    aggregateEdges: number;
  };
  returnedNodesCumulative: string;
  returnedEdgesCumulative: string;
  totalMatchingNodes: string;
  totalMatchingEdges: string;
  hasNextPage: boolean;
  memoryBytes: number;
};

/** An immutable compact graph snapshot ready for Cosmos and companion UI consumers. */
export type GraphRenderModel = {
  revision: number;
  projectionToken: string;
  context: BrowserGraphContext;
  nodes: readonly BrowserGraphNode[];
  edges: readonly BrowserGraphEdge[];
  nodeOrdinals: Uint32Array;
  ordinalToPointIndex: ReadonlyMap<number, number>;
  pointPositions: Float32Array;
  pointColors: Float32Array;
  pointSizes: Float32Array;
  pointShapes: Float32Array;
  pointClusters: Uint32Array;
  clusterPositions: Float32Array;
  links: Float32Array;
  linkColors: Float32Array;
  linkWidths: Float32Array;
  linkStyles: Float32Array;
  labelPriorities: Float32Array;
  adjacencyOffsets: Uint32Array;
  adjacencyPointIndices: Uint32Array;
  adjacencyLinkIndices: Uint32Array;
  completeness: BrowserGraphCompleteness;
  returnedNodes: string;
  returnedEdges: string;
  totalMatchingNodes: string;
  totalMatchingEdges: string;
  hasNextPage: boolean;
  memoryBytes: number;
};

/** A renderer-index projection of ordinal-based graph selection. */
export type GraphSelectionProjection = {
  selectedPointIndices: readonly number[];
  connectedPointIndices: readonly number[];
  connectedLinkIndices: readonly number[];
};

/**
 * Projects stable node ordinals into renderer indices and precomputed adjacency.
 *
 * Unknown ordinals are ignored because search or history state can legitimately
 * reference nodes outside the current bounded projection.
 */
export function projectGraphSelection(
  model: GraphRenderModel,
  selectedOrdinals: readonly number[],
  maximumSelection = 64,
): GraphSelectionProjection {
  const selected = new Set<number>();
  for (const ordinal of selectedOrdinals) {
    const pointIndex = model.ordinalToPointIndex.get(ordinal);
    if (pointIndex !== undefined) {
      selected.add(pointIndex);
      if (selected.size >= maximumSelection) {
        break;
      }
    }
  }

  const connectedPoints = new Set(selected);
  const connectedLinks = new Set<number>();
  for (const pointIndex of selected) {
    const start = model.adjacencyOffsets[pointIndex] ?? 0;
    const end = model.adjacencyOffsets[pointIndex + 1] ?? start;
    for (let cursor = start; cursor < end; cursor += 1) {
      const neighbor = model.adjacencyPointIndices[cursor];
      const link = model.adjacencyLinkIndices[cursor];
      if (neighbor !== undefined) {
        connectedPoints.add(neighbor);
      }
      if (link !== undefined) {
        connectedLinks.add(link);
      }
    }
  }

  return {
    selectedPointIndices: [...selected].sort(numericSort),
    connectedPointIndices: [...connectedPoints].sort(numericSort),
    connectedLinkIndices: [...connectedLinks].sort(numericSort),
  };
}

function numericSort(left: number, right: number) {
  return left - right;
}
