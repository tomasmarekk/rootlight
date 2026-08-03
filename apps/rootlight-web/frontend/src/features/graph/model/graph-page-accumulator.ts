// Accumulates ordered Worker pages without repeatedly spreading large typed arrays.
// Projection identity, ordinals, declared budgets, and memory remain fail-closed.

import { deterministicClusterPosition } from "./graph-layout";
import type { GraphRenderModel, PreparedGraphPage } from "./graph-model";

/** Hard client limits that may be stricter than a retained server projection. */
export type GraphAccumulatorLimits = {
  maximumNodes: number;
  maximumEdges: number;
  maximumMemoryBytes: number;
};

const DEFAULT_LIMITS: GraphAccumulatorLimits = {
  maximumNodes: 50_000,
  maximumEdges: 150_000,
  maximumMemoryBytes: 256 * 1_024 * 1_024,
};

/**
 * Retains validated graph chunks and materializes an immutable renderer snapshot on demand.
 *
 * Pages must arrive in ascending order for one projection. Call `dispose` when
 * route or generation identity changes so transferred buffers can be collected.
 */
export class GraphPageAccumulator {
  readonly #limits: GraphAccumulatorLimits;
  readonly #nodeOrdinals: Uint32Array[] = [];
  readonly #pointPositions: Float32Array[] = [];
  readonly #pointColors: Float32Array[] = [];
  readonly #pointSizes: Float32Array[] = [];
  readonly #pointShapes: Float32Array[] = [];
  readonly #clusterHashes: Uint32Array[] = [];
  readonly #linksByOrdinal: Uint32Array[] = [];
  readonly #linkColors: Float32Array[] = [];
  readonly #linkWidths: Float32Array[] = [];
  readonly #linkStyles: Float32Array[] = [];
  readonly #labelPriorities: Float32Array[] = [];
  readonly #nodes: PreparedGraphPage["nodes"][number][] = [];
  readonly #edges: PreparedGraphPage["edges"][number][] = [];
  readonly #ordinalToPointIndex = new Map<number, number>();
  readonly #edgeKeys = new Set<string>();
  #projectionToken: string | null = null;
  #expectedPageOrdinal = 0;
  #nodeCount = 0;
  #edgeCount = 0;
  #memoryBytes = 0;
  #revision = 0;
  #latestPage: PreparedGraphPage | null = null;
  #snapshot: GraphRenderModel | null = null;
  #disposed = false;

  /** Creates an accumulator with explicit client resource ceilings. */
  constructor(limits: Partial<GraphAccumulatorLimits> = {}) {
    this.#limits = { ...DEFAULT_LIMITS, ...limits };
    validateLimits(this.#limits);
  }

  /** Returns the number of pages accepted by this accumulator. */
  get pageCount(): number {
    return this.#expectedPageOrdinal;
  }

  /** Returns the retained typed-array byte estimate. */
  get memoryBytes(): number {
    return this.#memoryBytes;
  }

  /**
   * Adds the next prepared page after validating projection and aggregate invariants.
   *
   * @throws Error when identity, ordering, array shape, endpoint, or resource bounds fail.
   */
  append(page: PreparedGraphPage): void {
    this.#assertUsable();
    validatePreparedPageShape(page);
    if (
      (this.#projectionToken !== null && page.projectionToken !== this.#projectionToken) ||
      page.pageOrdinal !== this.#expectedPageOrdinal
    ) {
      throw new Error("Graph page does not continue the retained projection");
    }

    const nextNodeCount = this.#nodeCount + page.nodes.length;
    const nextEdgeCount = this.#edgeCount + page.edges.length;
    if (
      nextNodeCount > this.#limits.maximumNodes ||
      nextNodeCount > page.effectiveBudget.aggregateNodes ||
      nextEdgeCount > this.#limits.maximumEdges ||
      nextEdgeCount > page.effectiveBudget.aggregateEdges ||
      BigInt(page.returnedNodesCumulative) !== BigInt(nextNodeCount) ||
      BigInt(page.returnedEdgesCumulative) !== BigInt(nextEdgeCount)
    ) {
      throw new Error("Graph page exceeds aggregate counters or resource budgets");
    }
    if (this.#memoryBytes + page.memoryBytes > this.#limits.maximumMemoryBytes) {
      throw new Error("Graph projection exceeds the client memory budget");
    }

    const pageOrdinals = new Set<number>();
    const pendingOrdinalIndices = new Map<number, number>();
    for (let index = 0; index < page.nodeOrdinals.length; index += 1) {
      const ordinal = page.nodeOrdinals[index];
      if (
        ordinal === undefined ||
        pageOrdinals.has(ordinal) ||
        this.#ordinalToPointIndex.has(ordinal)
      ) {
        throw new Error("Graph page contains a duplicate node ordinal");
      }
      pageOrdinals.add(ordinal);
      pendingOrdinalIndices.set(ordinal, this.#nodeCount + index);
    }

    const pendingEdgeKeys = new Set<string>();
    for (let index = 0; index < page.edges.length; index += 1) {
      const edge = page.edges[index];
      const sourceOrdinal = page.linksByOrdinal[index * 2];
      const targetOrdinal = page.linksByOrdinal[index * 2 + 1];
      if (
        edge === undefined ||
        sourceOrdinal === undefined ||
        targetOrdinal === undefined ||
        (!this.#ordinalToPointIndex.has(sourceOrdinal) &&
          !pendingOrdinalIndices.has(sourceOrdinal)) ||
        (!this.#ordinalToPointIndex.has(targetOrdinal) && !pendingOrdinalIndices.has(targetOrdinal))
      ) {
        throw new Error("Graph edge references an unavailable node ordinal");
      }
      const edgeKey = `${String(sourceOrdinal)}\u001f${String(targetOrdinal)}\u001f${edge.relation}`;
      if (this.#edgeKeys.has(edgeKey) || pendingEdgeKeys.has(edgeKey)) {
        throw new Error("Graph page contains a duplicate typed edge");
      }
      pendingEdgeKeys.add(edgeKey);
    }

    for (const [ordinal, pointIndex] of pendingOrdinalIndices) {
      this.#ordinalToPointIndex.set(ordinal, pointIndex);
    }
    for (const edgeKey of pendingEdgeKeys) {
      this.#edgeKeys.add(edgeKey);
    }

    this.#projectionToken = page.projectionToken;
    this.#expectedPageOrdinal += 1;
    this.#nodeCount = nextNodeCount;
    this.#edgeCount = nextEdgeCount;
    this.#memoryBytes += page.memoryBytes;
    this.#revision += 1;
    this.#latestPage = page;
    this.#snapshot = null;
    this.#nodes.push(...page.nodes);
    this.#edges.push(...page.edges);
    this.#nodeOrdinals.push(page.nodeOrdinals);
    this.#pointPositions.push(page.pointPositions);
    this.#pointColors.push(page.pointColors);
    this.#pointSizes.push(page.pointSizes);
    this.#pointShapes.push(page.pointShapes);
    this.#clusterHashes.push(page.pointClusterHashes);
    this.#linksByOrdinal.push(page.linksByOrdinal);
    this.#linkColors.push(page.linkColors);
    this.#linkWidths.push(page.linkWidths);
    this.#linkStyles.push(page.linkStyles);
    this.#labelPriorities.push(page.labelPriorities);
  }

  /**
   * Materializes and caches a renderer model for the pages accepted so far.
   *
   * @throws Error when no page has been accepted or the accumulator was disposed.
   */
  snapshot(): GraphRenderModel {
    this.#assertUsable();
    if (this.#snapshot !== null) {
      return this.#snapshot;
    }
    const latestPage = this.#latestPage;
    const projectionToken = this.#projectionToken;
    if (latestPage === null || projectionToken === null) {
      throw new Error("Graph projection has no pages");
    }

    const nodeOrdinals = concatenateTypedArrays(this.#nodeOrdinals, Uint32Array);
    const pointClusterHashes = concatenateTypedArrays(this.#clusterHashes, Uint32Array);
    const { pointClusters, clusterPositions } = compactClusters(pointClusterHashes);
    const linksByOrdinal = concatenateTypedArrays(this.#linksByOrdinal, Uint32Array);
    const links = new Float32Array(linksByOrdinal.length);
    for (let index = 0; index < linksByOrdinal.length; index += 1) {
      const ordinal = linksByOrdinal[index];
      const pointIndex = ordinal === undefined ? undefined : this.#ordinalToPointIndex.get(ordinal);
      if (pointIndex === undefined) {
        throw new Error("Graph endpoint disappeared during accumulation");
      }
      links[index] = pointIndex;
    }
    const { offsets, points, linkIndices } = createAdjacency(this.#nodeCount, links);

    this.#snapshot = {
      revision: this.#revision,
      projectionToken,
      context: latestPage.context,
      nodes: [...this.#nodes],
      edges: [...this.#edges],
      nodeOrdinals,
      ordinalToPointIndex: new Map(this.#ordinalToPointIndex),
      pointPositions: concatenateTypedArrays(this.#pointPositions, Float32Array),
      pointColors: concatenateTypedArrays(this.#pointColors, Float32Array),
      pointSizes: concatenateTypedArrays(this.#pointSizes, Float32Array),
      pointShapes: concatenateTypedArrays(this.#pointShapes, Float32Array),
      pointClusters,
      clusterPositions,
      links,
      linkColors: concatenateTypedArrays(this.#linkColors, Float32Array),
      linkWidths: concatenateTypedArrays(this.#linkWidths, Float32Array),
      linkStyles: concatenateTypedArrays(this.#linkStyles, Float32Array),
      labelPriorities: concatenateTypedArrays(this.#labelPriorities, Float32Array),
      adjacencyOffsets: offsets,
      adjacencyPointIndices: points,
      adjacencyLinkIndices: linkIndices,
      completeness: latestPage.completeness,
      returnedNodes: latestPage.returnedNodesCumulative,
      returnedEdges: latestPage.returnedEdgesCumulative,
      totalMatchingNodes: latestPage.totalMatchingNodes,
      totalMatchingEdges: latestPage.totalMatchingEdges,
      hasNextPage: latestPage.hasNextPage,
      memoryBytes: this.#memoryBytes,
    };
    return this.#snapshot;
  }

  /** Releases retained buffers and prevents later reuse across projection identity. */
  dispose(): void {
    this.#disposed = true;
    this.#snapshot = null;
    this.#latestPage = null;
    this.#projectionToken = null;
    this.#nodes.length = 0;
    this.#edges.length = 0;
    this.#nodeOrdinals.length = 0;
    this.#pointPositions.length = 0;
    this.#pointColors.length = 0;
    this.#pointSizes.length = 0;
    this.#pointShapes.length = 0;
    this.#clusterHashes.length = 0;
    this.#linksByOrdinal.length = 0;
    this.#linkColors.length = 0;
    this.#linkWidths.length = 0;
    this.#linkStyles.length = 0;
    this.#labelPriorities.length = 0;
    this.#ordinalToPointIndex.clear();
    this.#edgeKeys.clear();
    this.#memoryBytes = 0;
  }

  #assertUsable() {
    if (this.#disposed) {
      throw new Error("Graph projection accumulator is disposed");
    }
  }
}

type TypedArrayConstructor<ArrayType extends Float32Array | Uint32Array> = new (
  length: number,
) => ArrayType;

function concatenateTypedArrays<ArrayType extends Float32Array | Uint32Array>(
  chunks: readonly ArrayType[],
  constructor: TypedArrayConstructor<ArrayType>,
): ArrayType {
  let length = 0;
  for (const chunk of chunks) {
    length += chunk.length;
  }
  const result = new constructor(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}

function compactClusters(hashes: Uint32Array) {
  const clusterIndexByHash = new Map<number, number>();
  const pointClusters = new Uint32Array(hashes.length);
  const positions: number[] = [];
  for (let index = 0; index < hashes.length; index += 1) {
    const hash = hashes[index] ?? 0;
    let clusterIndex = clusterIndexByHash.get(hash);
    if (clusterIndex === undefined) {
      clusterIndex = clusterIndexByHash.size;
      clusterIndexByHash.set(hash, clusterIndex);
      const position = deterministicClusterPosition(hash);
      positions.push(position[0], position[1]);
    }
    pointClusters[index] = clusterIndex;
  }
  return { pointClusters, clusterPositions: Float32Array.from(positions) };
}

function createAdjacency(pointCount: number, links: Float32Array) {
  const degree = new Uint32Array(pointCount);
  for (let index = 0; index < links.length; index += 2) {
    const source = links[index];
    const target = links[index + 1];
    if (source !== undefined && target !== undefined) {
      degree[source] = (degree[source] ?? 0) + 1;
      degree[target] = (degree[target] ?? 0) + 1;
    }
  }
  const offsets = new Uint32Array(pointCount + 1);
  for (let index = 0; index < pointCount; index += 1) {
    offsets[index + 1] = (offsets[index] ?? 0) + (degree[index] ?? 0);
  }
  const adjacencyLength = offsets[pointCount] ?? 0;
  const points = new Uint32Array(adjacencyLength);
  const linkIndices = new Uint32Array(adjacencyLength);
  const cursors = offsets.slice(0, pointCount);
  for (let index = 0; index < links.length; index += 2) {
    const source = links[index];
    const target = links[index + 1];
    if (source === undefined || target === undefined) {
      continue;
    }
    const linkIndex = index / 2;
    const sourceCursor = cursors[source] ?? 0;
    points[sourceCursor] = target;
    linkIndices[sourceCursor] = linkIndex;
    cursors[source] = sourceCursor + 1;
    const targetCursor = cursors[target] ?? 0;
    points[targetCursor] = source;
    linkIndices[targetCursor] = linkIndex;
    cursors[target] = targetCursor + 1;
  }
  return { offsets, points, linkIndices };
}

function validatePreparedPageShape(page: PreparedGraphPage) {
  const nodeCount = page.nodes.length;
  const edgeCount = page.edges.length;
  if (
    page.nodeOrdinals.length !== nodeCount ||
    page.pointPositions.length !== nodeCount * 2 ||
    page.pointColors.length !== nodeCount * 4 ||
    page.pointSizes.length !== nodeCount ||
    page.pointShapes.length !== nodeCount ||
    page.pointClusterHashes.length !== nodeCount ||
    page.labelPriorities.length !== nodeCount ||
    page.linksByOrdinal.length !== edgeCount * 2 ||
    page.linkColors.length !== edgeCount * 4 ||
    page.linkWidths.length !== edgeCount ||
    page.linkStyles.length !== edgeCount
  ) {
    throw new Error("Prepared graph page contains inconsistent typed-array lengths");
  }
  const arrays = [
    page.nodeOrdinals,
    page.pointPositions,
    page.pointColors,
    page.pointSizes,
    page.pointShapes,
    page.pointClusterHashes,
    page.linksByOrdinal,
    page.linkColors,
    page.linkWidths,
    page.linkStyles,
    page.labelPriorities,
  ];
  const memoryBytes = arrays.reduce((total, array) => total + array.byteLength, 0);
  if (page.memoryBytes !== memoryBytes) {
    throw new Error("Prepared graph page contains an inconsistent memory estimate");
  }
}

function validateLimits(limits: GraphAccumulatorLimits) {
  if (
    !Number.isSafeInteger(limits.maximumNodes) ||
    limits.maximumNodes < 1 ||
    !Number.isSafeInteger(limits.maximumEdges) ||
    limits.maximumEdges < 1 ||
    !Number.isSafeInteger(limits.maximumMemoryBytes) ||
    limits.maximumMemoryBytes < 1
  ) {
    throw new Error("Graph accumulator limits must be positive safe integers");
  }
}
