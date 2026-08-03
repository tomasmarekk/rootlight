// Centralizes source-free visual encoding for Atlas nodes and relations.
// Pure mappings keep Worker preparation deterministic and boundary-testable.

import type {
  BrowserGraphEdge,
  BrowserGraphNode,
  GraphNodeKind,
  GraphRelationKind,
} from "../model/graph-contracts";

/** A normalized RGBA color accepted by Cosmos typed-array setters. */
export type GraphColor = readonly [number, number, number, number];

/** Encoded arrays for one graph page before accumulation. */
export type GraphVisualArrays = {
  pointColors: Float32Array;
  pointSizes: Float32Array;
  pointShapes: Float32Array;
  linkColors: Float32Array;
  linkWidths: Float32Array;
  linkStyles: Float32Array;
  labelPriorities: Float32Array;
};

const NODE_COLORS: Readonly<Record<GraphNodeKind, GraphColor>> = {
  file: [0.384, 0.847, 0.957, 1],
  symbol: [0.569, 0.592, 1, 1],
  unknown: [0.541, 0.58, 0.651, 1],
};

const RELATION_COLORS: Readonly<Record<GraphRelationKind, GraphColor>> = {
  calls: [0.384, 0.847, 0.957, 1],
  called_by: [0.384, 0.847, 0.957, 1],
  references: [0.486, 0.514, 1, 1],
  types: [0.694, 0.565, 1, 1],
  implements: [0.694, 0.565, 1, 1],
  imports: [0.337, 0.773, 0.541, 1],
  tests: [0.941, 0.443, 0.804, 1],
  ownership: [0.961, 0.725, 0.298, 1],
  service_call: [0.384, 0.847, 0.957, 1],
  calls_route: [0.961, 0.725, 0.298, 1],
  messaging: [0.337, 0.773, 0.541, 1],
  reads_table: [0.486, 0.514, 1, 1],
  writes_table: [0.941, 0.443, 0.804, 1],
  build_dependency: [0.541, 0.58, 0.651, 1],
  data_flow: [0.384, 0.847, 0.957, 1],
  history: [0.541, 0.58, 0.651, 1],
  unknown: [0.541, 0.58, 0.651, 1],
};

/** Maps a node kind to a color-aligned Rootlight design token. */
export function graphNodeColor(kind: GraphNodeKind): GraphColor {
  return NODE_COLORS[kind];
}

/** Maps a typed relation to its stable visual family color. */
export function graphRelationColor(relation: GraphRelationKind): GraphColor {
  return RELATION_COLORS[relation];
}

/** Maps bounded node metrics to a readable point size without allowing hubs to dominate. */
export function graphPointSize(node: BrowserGraphNode): number {
  const symbolCount = node.symbolCount ?? 0;
  const fanIn = node.fanIn ?? 0;
  const hotspot = node.hotspotScore ?? 0;
  const score = Math.log2(1 + symbolCount) + Math.log2(1 + fanIn) * 0.7 + hotspot / 200;
  return clamp(4.5 + score, 4.5, 18);
}

/** Maps confidence from the wire's 0–1000 scale into a visible bounded alpha. */
export function graphConfidenceOpacity(confidence: number, minimum = 0.18): number {
  return clamp(minimum + (confidence / 1_000) * (1 - minimum), minimum, 1);
}

/** Maps an edge weight onto a restrained logarithmic width. */
export function graphLinkWidth(edge: BrowserGraphEdge): number {
  return clamp(0.65 + Math.log2(1 + edge.weight) * 0.42, 0.65, 4);
}

/** Produces all visual typed arrays for a validated graph page. */
export function encodeGraphVisuals(
  nodes: readonly BrowserGraphNode[],
  edges: readonly BrowserGraphEdge[],
): GraphVisualArrays {
  const pointColors = new Float32Array(nodes.length * 4);
  const pointSizes = new Float32Array(nodes.length);
  const pointShapes = new Float32Array(nodes.length);
  const labelPriorities = new Float32Array(nodes.length);
  for (let index = 0; index < nodes.length; index += 1) {
    const node = nodes[index];
    if (node === undefined) {
      continue;
    }
    const color = graphNodeColor(node.kind);
    writeColor(pointColors, index, color, graphConfidenceOpacity(node.confidence, 0.5));
    pointSizes[index] = graphPointSize(node);
    pointShapes[index] = node.generated === true ? 3 : node.kind === "file" ? 1 : 0;
    labelPriorities[index] = calculateLabelPriority(node, pointSizes[index] ?? 0);
  }

  const linkColors = new Float32Array(edges.length * 4);
  const linkWidths = new Float32Array(edges.length);
  const linkStyles = new Float32Array(edges.length);
  for (let index = 0; index < edges.length; index += 1) {
    const edge = edges[index];
    if (edge === undefined) {
      continue;
    }
    const color = graphRelationColor(edge.relation);
    const evidenceFactor = edge.exact ? 1 : edge.inferred ? 0.55 : 0.72;
    writeColor(linkColors, index, color, graphConfidenceOpacity(edge.confidence) * evidenceFactor);
    linkWidths[index] = graphLinkWidth(edge);
    linkStyles[index] = edge.exact ? 0 : edge.inferred ? 1 : 2;
  }

  return {
    pointColors,
    pointSizes,
    pointShapes,
    linkColors,
    linkWidths,
    linkStyles,
    labelPriorities,
  };
}

function calculateLabelPriority(node: BrowserGraphNode, size: number): number {
  const hotspot = node.hotspotScore ?? 0;
  const fanIn = node.fanIn ?? 0;
  return size * 10 + hotspot + Math.log2(1 + fanIn) * 20;
}

function writeColor(target: Float32Array, index: number, color: GraphColor, opacity: number) {
  const offset = index * 4;
  target[offset] = color[0];
  target[offset + 1] = color[1];
  target[offset + 2] = color[2];
  target[offset + 3] = color[3] * opacity;
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}
