// Builds bounded Cosmos configuration from graph density and user motion preference.
// Init-only fields are produced once; runtime changes use explicit partial updates.

import type { GraphConfig } from "@cosmos.gl/graph";

import type { GraphView } from "../model/graph-contracts";

/** Density bands used to degrade graph effects before interaction becomes unstable. */
export type GraphDensityBand = "a" | "b" | "c" | "d";

/** Measured renderer policy derived from current node and edge counts. */
export type GraphDensityProfile = {
  band: GraphDensityBand;
  labelBudget: number;
  transitionDuration: number;
  collisionStrength: number;
  linkBlending: boolean;
  hoverSamplingDistance: number;
};

/** Event callbacks wired into the imperative Cosmos boundary. */
export type CosmosGraphCallbacks = {
  onPointClick: (index: number, event: MouseEvent) => void;
  onPointHover: (index: number | null) => void;
  onInteractionStart: () => void;
};

/** Inputs for one immutable Cosmos instance configuration. */
export type CosmosGraphConfigInput = {
  randomSeed: string;
  view: GraphView;
  nodeCount: number;
  edgeCount: number;
  reducedMotion: boolean;
  callbacks: CosmosGraphCallbacks;
};

/** Classifies graph density and returns safe effect budgets for the renderer and labels. */
export function graphDensityProfile(nodeCount: number, edgeCount: number): GraphDensityProfile {
  if (nodeCount <= 2_000 && edgeCount <= 10_000) {
    return {
      band: "a",
      labelBudget: 100,
      transitionDuration: 220,
      collisionStrength: 1,
      linkBlending: true,
      hoverSamplingDistance: 72,
    };
  }
  if (nodeCount <= 10_000 && edgeCount <= 30_000) {
    return {
      band: "b",
      labelBudget: 60,
      transitionDuration: 120,
      collisionStrength: 0.75,
      linkBlending: true,
      hoverSamplingDistance: 88,
    };
  }
  if (nodeCount <= 50_000 && edgeCount <= 150_000) {
    return {
      band: "c",
      labelBudget: 24,
      transitionDuration: 0,
      collisionStrength: 0.25,
      linkBlending: false,
      hoverSamplingDistance: 112,
    };
  }
  return {
    band: "d",
    labelBudget: 12,
    transitionDuration: 0,
    collisionStrength: 0,
    linkBlending: false,
    hoverSamplingDistance: 140,
  };
}

/** Builds the init-only and event configuration for a new Cosmos graph instance. */
export function createCosmosGraphConfig(input: CosmosGraphConfigInput): GraphConfig {
  const density = graphDensityProfile(input.nodeCount, input.edgeCount);
  const collisionAllowed = input.view === "architecture" || input.view === "files";
  return {
    attribution: "",
    backgroundColor: "#080a0f",
    randomSeed: input.randomSeed,
    fitViewOnInit: false,
    enableDrag: false,
    enableZoom: true,
    enableSimulation: true,
    enableSimulationDuringZoom: false,
    rescalePositions: false,
    transitionDuration: input.reducedMotion ? 0 : density.transitionDuration,
    simulationFriction: input.view === "symbols" ? 0.22 : 0.16,
    simulationGravity: 0.08,
    simulationRepulsion: 0.32,
    simulationCollision: collisionAllowed ? density.collisionStrength : 0,
    simulationCollisionPadding: 2,
    linkBlending: density.linkBlending,
    pointSamplingDistance: density.hoverSamplingDistance,
    onPointClick: (index, _position, event) => {
      input.callbacks.onPointClick(index, event);
    },
    onPointMouseOver: (index) => {
      input.callbacks.onPointHover(index);
    },
    onPointMouseOut: () => {
      input.callbacks.onPointHover(null);
    },
    onZoomStart: () => {
      input.callbacks.onInteractionStart();
    },
    onDragStart: () => {
      input.callbacks.onInteractionStart();
    },
  };
}
