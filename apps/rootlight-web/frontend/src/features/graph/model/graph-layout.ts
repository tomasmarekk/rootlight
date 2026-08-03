// Derives deterministic Atlas layout inputs from immutable graph identity.
// The same generation, view, scope, and layout version always produce the same seed.

import type { GraphView } from "./graph-contracts";

const HASH_OFFSET = 0x811c_9dc5;
const HASH_PRIME = 0x0100_0193;
const UINT32_RANGE = 0x1_0000_0000;
const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));

/** Identifies every input that is allowed to influence the initial graph layout. */
export type GraphLayoutIdentity = {
  repositoryId: string;
  generationId: string;
  view: GraphView;
  scopeFingerprint: string;
  layoutVersion: string;
};

/** Returns a stable 32-bit FNV-1a hash for deterministic browser layout work. */
export function stableGraphHash(value: string): number {
  let hash = HASH_OFFSET;
  for (const character of value) {
    hash ^= character.codePointAt(0) ?? 0;
    hash = Math.imul(hash, HASH_PRIME);
  }
  return hash >>> 0;
}

/** Derives the Cosmos random seed from immutable route and projection identity. */
export function deriveGraphLayoutSeed(identity: GraphLayoutIdentity): string {
  const canonical = [
    identity.repositoryId,
    identity.generationId,
    identity.view,
    identity.scopeFingerprint,
    identity.layoutVersion,
  ].join("\u001f");
  return `rootlight-atlas-${stableGraphHash(canonical).toString(16).padStart(8, "0")}`;
}

/**
 * Returns a deterministic cluster center in scene space.
 *
 * Cluster hashes map onto a low-discrepancy spiral so adding a later cluster does
 * not move centers already rendered by an earlier page.
 */
export function deterministicClusterPosition(clusterHash: number): readonly [number, number] {
  const normalized = clusterHash / UINT32_RANGE;
  const radialIndex = (clusterHash % 257) + 1;
  const radius = 180 + Math.sqrt(radialIndex) * 92;
  const angle = radialIndex * GOLDEN_ANGLE + normalized * Math.PI * 2;
  return [Math.cos(angle) * radius, Math.sin(angle) * radius];
}

/**
 * Places a node deterministically around its cluster center.
 *
 * The bounded jitter avoids timestamp or page-arrival-order dependence while
 * leaving Cosmos enough local separation work to perform.
 */
export function deterministicNodePosition(
  stableId: string,
  clusterHash: number,
  layoutSeed: string,
): readonly [number, number] {
  const center = deterministicClusterPosition(clusterHash);
  const nodeHash = stableGraphHash(`${layoutSeed}\u001f${stableId}`);
  const angle = (nodeHash / UINT32_RANGE) * Math.PI * 2;
  const radius = 12 + ((nodeHash >>> 8) % 89);
  return [center[0] + Math.cos(angle) * radius, center[1] + Math.sin(angle) * radius];
}
