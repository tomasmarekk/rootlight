// Defines and executes the bounded Worker protocol for source-free graph pages.
// Validation completes before typed arrays are allocated or transferred to the main thread.

import { encodeGraphVisuals } from "../features/graph/controller/visual-encoding";
import type { GraphLayoutIdentity } from "../features/graph/model/graph-layout";
import {
  deriveGraphLayoutSeed,
  deterministicNodePosition,
  stableGraphHash,
} from "../features/graph/model/graph-layout";
import {
  parseBrowserGraphPage,
  type BrowserGraphNode,
} from "../features/graph/model/graph-contracts";
import type { PreparedGraphPage } from "../features/graph/model/graph-model";

/** Requests validation and preparation of one immutable graph projection page. */
export type GraphDecodeRequest = {
  type: "decode";
  jobId: number;
  page: unknown;
  expectedRepositoryId: string;
  expectedGenerationId: string;
  expectedProjectionToken?: string;
  layoutIdentity: GraphLayoutIdentity;
};

/** Invalidates a pending job so a late Worker response cannot update route state. */
export type GraphDecodeCancelRequest = {
  type: "cancel";
  jobId: number;
};

/** A message accepted by the graph decoder Worker. */
export type GraphDecoderRequest = GraphDecodeRequest | GraphDecodeCancelRequest;

/** Returns a validated transferable page from the Worker. */
export type GraphDecodeSuccess = {
  type: "decoded";
  jobId: number;
  page: PreparedGraphPage;
};

/** Returns a safe typed failure without echoing untrusted payload content. */
export type GraphDecodeFailure = {
  type: "error";
  jobId: number;
  code: "invalid_graph_page" | "worker_failure";
  message: string;
};

/** A message returned by the graph decoder Worker. */
export type GraphDecoderResponse = GraphDecodeSuccess | GraphDecodeFailure;

/**
 * Validates one browser page and prepares deterministic transferable renderer arrays.
 *
 * @throws Error when wire correlation or bounded graph invariants fail.
 */
export function decodeGraphPage(request: GraphDecodeRequest): PreparedGraphPage {
  validateJobId(request.jobId);
  const page = parseBrowserGraphPage(
    request.page,
    request.expectedRepositoryId,
    request.expectedGenerationId,
    request.expectedProjectionToken,
  );
  if (
    request.layoutIdentity.repositoryId !== request.expectedRepositoryId ||
    request.layoutIdentity.generationId !== request.expectedGenerationId
  ) {
    throw new Error("Graph layout identity does not match the requested generation");
  }

  const nodeOrdinals = new Uint32Array(page.nodes.length);
  const pointPositions = new Float32Array(page.nodes.length * 2);
  const pointClusterHashes = new Uint32Array(page.nodes.length);
  const layoutSeed = deriveGraphLayoutSeed(request.layoutIdentity);
  for (let index = 0; index < page.nodes.length; index += 1) {
    const node = page.nodes[index];
    if (node === undefined) {
      throw new Error("Graph page contains a sparse node array");
    }
    const clusterHash = stableGraphHash(clusterIdentity(node));
    const position = deterministicNodePosition(node.stableId, clusterHash, layoutSeed);
    nodeOrdinals[index] = node.ordinal;
    pointPositions[index * 2] = position[0];
    pointPositions[index * 2 + 1] = position[1];
    pointClusterHashes[index] = clusterHash;
  }

  const linksByOrdinal = new Uint32Array(page.edges.length * 2);
  for (let index = 0; index < page.edges.length; index += 1) {
    const edge = page.edges[index];
    if (edge === undefined) {
      throw new Error("Graph page contains a sparse edge array");
    }
    linksByOrdinal[index * 2] = edge.sourceOrdinal;
    linksByOrdinal[index * 2 + 1] = edge.targetOrdinal;
  }

  const visuals = encodeGraphVisuals(page.nodes, page.edges);
  const arrays = [
    nodeOrdinals,
    pointPositions,
    pointClusterHashes,
    linksByOrdinal,
    visuals.pointColors,
    visuals.pointSizes,
    visuals.pointShapes,
    visuals.linkColors,
    visuals.linkWidths,
    visuals.linkStyles,
    visuals.labelPriorities,
  ];
  let memoryBytes = 0;
  for (const array of arrays) {
    memoryBytes += array.byteLength;
  }

  return {
    projectionToken: page.projectionToken,
    pageOrdinal: page.pageOrdinal,
    context: page.context,
    nodes: page.nodes,
    edges: page.edges,
    nodeOrdinals,
    pointPositions,
    pointColors: visuals.pointColors,
    pointSizes: visuals.pointSizes,
    pointShapes: visuals.pointShapes,
    pointClusterHashes,
    linksByOrdinal,
    linkColors: visuals.linkColors,
    linkWidths: visuals.linkWidths,
    linkStyles: visuals.linkStyles,
    labelPriorities: visuals.labelPriorities,
    completeness: page.completeness,
    effectiveBudget: {
      aggregateNodes: page.effectiveBudget.aggregateNodes,
      aggregateEdges: page.effectiveBudget.aggregateEdges,
    },
    returnedNodesCumulative: page.returnedNodesCumulative,
    returnedEdgesCumulative: page.returnedEdgesCumulative,
    totalMatchingNodes: page.totalMatchingNodes,
    totalMatchingEdges: page.totalMatchingEdges,
    hasNextPage: page.hasNextPage,
    memoryBytes,
  };
}

/** Returns every ArrayBuffer that should be transferred with a decoded response. */
export function graphPageTransferables(page: PreparedGraphPage): Transferable[] {
  return [
    page.nodeOrdinals.buffer,
    page.pointPositions.buffer,
    page.pointColors.buffer,
    page.pointSizes.buffer,
    page.pointShapes.buffer,
    page.pointClusterHashes.buffer,
    page.linksByOrdinal.buffer,
    page.linkColors.buffer,
    page.linkWidths.buffer,
    page.linkStyles.buffer,
    page.labelPriorities.buffer,
  ];
}

function clusterIdentity(node: BrowserGraphNode): string {
  if (node.community !== null) {
    return `community:${node.community}`;
  }
  if (node.component !== null) {
    return `component:${node.component}`;
  }
  if (node.path !== null) {
    const separator = Math.max(node.path.lastIndexOf("/"), node.path.lastIndexOf("\\"));
    return `directory:${separator < 0 ? "" : node.path.slice(0, separator)}`;
  }
  return "unclustered";
}

function validateJobId(jobId: number) {
  if (!Number.isSafeInteger(jobId) || jobId < 1) {
    throw new Error("Worker job ID must be a positive safe integer");
  }
}
