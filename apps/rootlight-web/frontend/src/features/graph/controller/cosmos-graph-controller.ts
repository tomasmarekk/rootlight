// Owns the single imperative Cosmos lifecycle for an Atlas viewport.
// React supplies immutable models and small ordinal state; GPU arrays stay inside this boundary.

import type { Graph, GraphConfig } from "@cosmos.gl/graph";

import type { GraphView } from "../model/graph-contracts";
import { deriveGraphLayoutSeed, type GraphLayoutIdentity } from "../model/graph-layout";
import { projectGraphSelection, type GraphRenderModel } from "../model/graph-model";
import { createCosmosGraphConfig, graphDensityProfile } from "./graph-config";

/** Explicit lifecycle states observable by React chrome and fallback handling. */
export type CosmosGraphLifecycle =
  "idle" | "initializing" | "ready" | "updating" | "context_lost" | "failed" | "disposed";

/** Simulation status exposed to the advanced toolbar control. */
export type GraphSimulationState = "running" | "paused" | "settled";

/** A small immutable controller snapshot safe to store in React state. */
export type CosmosGraphControllerSnapshot = {
  lifecycle: CosmosGraphLifecycle;
  simulation: GraphSimulationState;
  labelsVisible: boolean;
  selectedOrdinals: readonly number[];
  hoveredOrdinal: number | null;
  contextLossCount: number;
  errorMessage: string | null;
};

/** Cosmos methods used by Rootlight and implemented by controller test doubles. */
export type CosmosGraphPort = Pick<
  Graph,
  | "destroy"
  | "fitView"
  | "pause"
  | "ready"
  | "render"
  | "setClusterPositions"
  | "setConfigPartial"
  | "setLinkColors"
  | "setLinks"
  | "setLinkStyles"
  | "setLinkWidths"
  | "setPointClusterStrength"
  | "setPointClusters"
  | "setPointColors"
  | "setPointPositions"
  | "setPointShapes"
  | "setPointSizes"
  | "setZoomTransformByPointPositions"
  | "start"
  | "stop"
  | "unpause"
>;

/** Creates one Cosmos instance for a container lifecycle. */
export type CosmosGraphFactory = (
  container: HTMLDivElement,
  config: GraphConfig,
) => CosmosGraphPort | Promise<CosmosGraphPort>;

/** Controller construction inputs and small event callbacks. */
export type CosmosGraphControllerOptions = {
  layoutIdentity: GraphLayoutIdentity;
  view: GraphView;
  reducedMotion?: boolean;
  controlledSelection?: boolean;
  factory?: CosmosGraphFactory;
  onSelectionChange?: (ordinals: readonly number[]) => void;
  onHoverChange?: (ordinal: number | null) => void;
  onFallbackRequired?: (reason: "initialization" | "context_loss") => void;
};

/**
 * Owns Cosmos initialization, model updates, selection, simulation, context recovery, and disposal.
 */
export class CosmosGraphController {
  readonly #options: CosmosGraphControllerOptions;
  readonly #factory: CosmosGraphFactory;
  readonly #subscribers = new Set<() => void>();
  #snapshot: CosmosGraphControllerSnapshot = {
    lifecycle: "idle",
    simulation: "paused",
    labelsVisible: true,
    selectedOrdinals: [],
    hoveredOrdinal: null,
    contextLossCount: 0,
    errorMessage: null,
  };
  #container: HTMLDivElement | null = null;
  #graph: CosmosGraphPort | null = null;
  #model: GraphRenderModel | null = null;
  #overlayOrdinals: readonly number[] = [];
  #canvas: HTMLCanvasElement | null = null;
  #resizeObserver: ResizeObserver | null = null;
  #resizeFrame: number | null = null;
  #settleTimer: ReturnType<typeof setTimeout> | null = null;
  #initialFitApplied = false;
  #recoveryAttempted = false;
  #initializationRevision = 0;

  /** Creates a controller without allocating GPU resources until `initialize`. */
  constructor(options: CosmosGraphControllerOptions) {
    this.#options = options;
    this.#factory = options.factory ?? createDefaultCosmosFactory;
  }

  /** Returns the current small controller snapshot. */
  getSnapshot = (): CosmosGraphControllerSnapshot => this.#snapshot;

  /** Registers a state listener and returns its deterministic cleanup function. */
  subscribe = (listener: () => void): (() => void) => {
    this.#subscribers.add(listener);
    return () => {
      this.#subscribers.delete(listener);
    };
  };

  /**
   * Allocates the one Cosmos instance owned by a viewport container.
   *
   * @throws Error when called twice or after disposal.
   */
  async initialize(container: HTMLDivElement): Promise<void> {
    if (this.#snapshot.lifecycle !== "idle") {
      throw new Error("Graph controller can only be initialized once");
    }
    this.#container = container;
    this.#setSnapshot({ lifecycle: "initializing", errorMessage: null });
    await this.#createGraph("initialization");
  }

  /**
   * Applies an immutable accumulated model without resetting camera after the first page.
   *
   * A model may be provided during async initialization; it is applied after `ready`.
   */
  applyModel(model: GraphRenderModel): void {
    this.#assertActive();
    if (
      this.#model !== null &&
      (this.#model.projectionToken !== model.projectionToken ||
        this.#model.context.generationId !== model.context.generationId)
    ) {
      throw new Error("Graph controller cannot change immutable projection identity");
    }
    const previousRevision = this.#model?.revision ?? 0;
    if (model.revision < previousRevision) {
      return;
    }
    this.#model = model;
    if (this.#snapshot.lifecycle === "ready" || this.#snapshot.lifecycle === "updating") {
      this.#applyReadyModel(model, previousRevision === 0);
    }
  }

  /** Replaces ordinal selection while preserving only nodes present in the projection. */
  setSelection(ordinals: readonly number[]): void {
    if (this.#options.controlledSelection === true) {
      this.#assertActive();
      const uniqueOrdinals = this.#normalizeSelection(ordinals);
      if (!sameOrdinals(uniqueOrdinals, this.#snapshot.selectedOrdinals)) {
        if (this.#snapshot.hoveredOrdinal !== null) {
          this.#setSnapshot({ hoveredOrdinal: null });
        }
        this.#options.onSelectionChange?.(uniqueOrdinals);
        this.#options.onHoverChange?.(null);
      }
      return;
    }
    this.#updateSelection(ordinals, true);
  }

  /**
   * Synchronizes externally controlled selection without emitting a user-change callback.
   *
   * This is the route/history input path; user interactions continue to use `setSelection`.
   */
  syncSelection(ordinals: readonly number[]): void {
    this.#updateSelection(ordinals, false);
  }

  /** Synchronizes a typed analytical overlay without changing controlled graph selection. */
  syncOverlay(ordinals: readonly number[]): void {
    this.#assertActive();
    const normalized = this.#normalizeOrdinals(ordinals, 200);
    if (sameOrdinals(normalized, this.#overlayOrdinals)) {
      return;
    }
    this.#overlayOrdinals = normalized;
    this.#applySelectionConfig();
  }

  #updateSelection(ordinals: readonly number[], notify: boolean): void {
    this.#assertActive();
    const uniqueOrdinals = this.#normalizeSelection(ordinals);
    if (sameOrdinals(uniqueOrdinals, this.#snapshot.selectedOrdinals)) {
      return;
    }
    this.#setSnapshot({ selectedOrdinals: uniqueOrdinals, hoveredOrdinal: null });
    this.#applySelectionConfig();
    if (notify) {
      this.#options.onSelectionChange?.(uniqueOrdinals);
      this.#options.onHoverChange?.(null);
    }
  }

  /** Clears the current ordinal selection. */
  resetSelection(): void {
    this.setSelection([]);
  }

  /** Fits all currently rendered points without restarting simulation. */
  fitAll(): void {
    const graph = this.#readyGraph();
    graph.fitView(this.#motionDuration(220), 0.14, false);
  }

  /** Fits the current selection, returning false when no selected point is visible. */
  fitSelection(): boolean {
    const graph = this.#readyGraph();
    const model = this.#model;
    if (model === null) {
      return false;
    }
    const selection = projectGraphSelection(model, this.#snapshot.selectedOrdinals);
    if (selection.selectedPointIndices.length === 0) {
      return false;
    }
    const positions = new Float32Array(selection.selectedPointIndices.length * 2);
    for (let index = 0; index < selection.selectedPointIndices.length; index += 1) {
      const pointIndex = selection.selectedPointIndices[index];
      if (pointIndex === undefined) {
        continue;
      }
      positions[index * 2] = model.pointPositions[pointIndex * 2] ?? 0;
      positions[index * 2 + 1] = model.pointPositions[pointIndex * 2 + 1] ?? 0;
    }
    graph.setZoomTransformByPointPositions(
      positions,
      this.#motionDuration(180),
      undefined,
      0.2,
      false,
    );
    return true;
  }

  /** Enables or disables the bounded HTML label layer. */
  setLabelsVisible(visible: boolean): void {
    this.#assertActive();
    if (visible === this.#snapshot.labelsVisible) {
      return;
    }
    this.#setSnapshot({ labelsVisible: visible });
  }

  /** Pauses simulation and rendering work until explicitly resumed or updated. */
  pauseSimulation(): void {
    const graph = this.#readyGraph();
    this.#clearSettleTimer();
    graph.pause();
    this.#setSnapshot({ simulation: "paused" });
  }

  /** Resumes bounded simulation from a controlled alpha. */
  resumeSimulation(): void {
    const graph = this.#readyGraph();
    graph.unpause();
    graph.start(0.22);
    this.#setSnapshot({ simulation: "running" });
    this.#scheduleSettle();
  }

  /** Destroys GPU, observer, event, animation, and timer resources exactly once. */
  dispose(): void {
    if (this.#snapshot.lifecycle === "disposed") {
      return;
    }
    this.#initializationRevision += 1;
    this.#clearSettleTimer();
    this.#detachGraphResources();
    this.#container = null;
    this.#model = null;
    this.#overlayOrdinals = [];
    this.#subscribers.clear();
    this.#snapshot = { ...this.#snapshot, lifecycle: "disposed", simulation: "paused" };
    performance.mark("rootlight.graph.controller.dispose");
  }

  async #createGraph(failureReason: "initialization" | "context_loss") {
    const container = this.#container;
    if (container === null) {
      throw new Error("Graph controller container is unavailable");
    }
    const initializationRevision = this.#initializationRevision + 1;
    this.#initializationRevision = initializationRevision;
    const model = this.#model;
    const config = createCosmosGraphConfig({
      randomSeed: deriveGraphLayoutSeed(this.#options.layoutIdentity),
      view: this.#options.view,
      nodeCount: model?.nodes.length ?? 0,
      edgeCount: model?.edges.length ?? 0,
      reducedMotion: this.#options.reducedMotion ?? false,
      callbacks: {
        onPointClick: this.#handlePointClick,
        onPointHover: this.#handlePointHover,
        onInteractionStart: this.#handleInteractionStart,
      },
    });
    let graph: CosmosGraphPort | null = null;
    try {
      graph = await this.#factory(container, config);
      await graph.ready;
      if (
        this.#snapshot.lifecycle === "disposed" ||
        initializationRevision !== this.#initializationRevision
      ) {
        graph.destroy();
        return;
      }
      this.#graph = graph;
      this.#attachGraphResources();
      performance.mark("rootlight.graph.controller.ready");
      this.#setSnapshot({ lifecycle: "ready", errorMessage: null });
      if (this.#model !== null) {
        this.#applyReadyModel(this.#model, !this.#initialFitApplied);
      }
    } catch {
      graph?.destroy();
      if (this.#snapshot.lifecycle === "disposed") {
        return;
      }
      this.#setSnapshot({
        lifecycle: "failed",
        simulation: "paused",
        errorMessage: "The graphics renderer could not be initialized.",
      });
      this.#options.onFallbackRequired?.(failureReason);
    }
  }

  #applyReadyModel(model: GraphRenderModel, firstPage: boolean) {
    const graph = this.#readyGraph();
    this.#setSnapshot({ lifecycle: "updating" });
    const density = graphDensityProfile(model.nodes.length, model.edges.length);
    graph.setConfigPartial({
      transitionDuration: this.#options.reducedMotion === true ? 0 : density.transitionDuration,
      linkBlending: density.linkBlending,
      simulationCollision:
        this.#options.view === "architecture" || this.#options.view === "files"
          ? density.collisionStrength
          : 0,
      pointSamplingDistance: density.hoverSamplingDistance,
    });
    graph.setPointPositions(model.pointPositions, true);
    graph.setPointColors(model.pointColors);
    graph.setPointSizes(model.pointSizes);
    graph.setPointShapes(model.pointShapes);
    graph.setPointClusters(Array.from(model.pointClusters));
    graph.setClusterPositions(Array.from(model.clusterPositions));
    graph.setPointClusterStrength(new Float32Array(model.nodes.length).fill(0.28));
    graph.setLinks(model.links);
    graph.setLinkColors(model.linkColors);
    graph.setLinkWidths(model.linkWidths);
    graph.setLinkStyles(model.linkStyles);
    this.#applySelectionConfig();

    const reducedMotion = this.#options.reducedMotion === true;
    graph.render(reducedMotion ? 0 : firstPage ? 0.65 : 0.18, reducedMotion ? 0 : undefined);
    if (reducedMotion) {
      graph.stop();
      this.#setSnapshot({ lifecycle: "ready", simulation: "settled" });
    } else {
      graph.start(firstPage ? 0.65 : 0.18);
      this.#setSnapshot({ lifecycle: "ready", simulation: "running" });
      this.#scheduleSettle();
    }
    if (firstPage && !this.#initialFitApplied) {
      graph.fitView(this.#motionDuration(220), 0.14, false);
      this.#initialFitApplied = true;
      performance.mark("rootlight.graph.first-useful");
    }
  }

  #applySelectionConfig() {
    const graph = this.#graph;
    const model = this.#model;
    if (graph === null || model === null || this.#snapshot.lifecycle === "initializing") {
      return;
    }
    const selection = projectGraphSelection(model, this.#snapshot.selectedOrdinals);
    const overlay = projectGraphSelection(model, this.#overlayOrdinals, 200);
    graph.setConfigPartial({
      outlinedPointIndices: [...selection.selectedPointIndices],
      highlightedPointIndices: [
        ...new Set([...selection.connectedPointIndices, ...overlay.selectedPointIndices]),
      ],
      highlightedLinkIndices: [...selection.connectedLinkIndices],
    });
    graph.render(undefined, this.#motionDuration(100));
  }

  readonly #handlePointClick = (pointIndex: number, event: MouseEvent) => {
    const ordinal = this.#model?.nodeOrdinals[pointIndex];
    if (ordinal === undefined) {
      return;
    }
    if (event.ctrlKey || event.metaKey) {
      const selection = new Set(this.#snapshot.selectedOrdinals);
      if (selection.has(ordinal)) {
        selection.delete(ordinal);
      } else if (selection.size < 64) {
        selection.add(ordinal);
      }
      this.setSelection([...selection]);
      return;
    }
    this.setSelection([ordinal]);
  };

  readonly #handlePointHover = (pointIndex: number | null) => {
    const ordinal = pointIndex === null ? null : (this.#model?.nodeOrdinals[pointIndex] ?? null);
    if (ordinal === this.#snapshot.hoveredOrdinal) {
      return;
    }
    this.#setSnapshot({ hoveredOrdinal: ordinal });
    this.#options.onHoverChange?.(ordinal);
  };

  readonly #handleInteractionStart = () => {
    this.#handlePointHover(null);
  };

  readonly #handleVisibilityChange = () => {
    if (document.visibilityState === "hidden" && this.#snapshot.lifecycle === "ready") {
      this.#graph?.pause();
      this.#clearSettleTimer();
      this.#setSnapshot({ simulation: "paused" });
    }
  };

  readonly #handleContextLost = (event: Event) => {
    event.preventDefault();
    this.#clearSettleTimer();
    this.#graph?.pause();
    const contextLossCount = this.#snapshot.contextLossCount + 1;
    this.#setSnapshot({
      lifecycle: "context_lost",
      simulation: "paused",
      hoveredOrdinal: null,
      contextLossCount,
      errorMessage: "The graphics context was lost.",
    });
    if (this.#recoveryAttempted) {
      this.#detachGraphResources();
      this.#setSnapshot({
        lifecycle: "failed",
        errorMessage: "The graphics context could not be restored safely.",
      });
      this.#options.onFallbackRequired?.("context_loss");
    }
  };

  readonly #handleContextRestored = () => {
    if (this.#snapshot.lifecycle !== "context_lost" || this.#recoveryAttempted) {
      return;
    }
    this.#recoveryAttempted = true;
    this.#detachGraphResources();
    this.#setSnapshot({ lifecycle: "initializing", errorMessage: null });
    void this.#createGraph("context_loss");
  };

  #attachGraphResources() {
    const container = this.#container;
    if (container === null) {
      return;
    }
    this.#canvas = container.querySelector("canvas");
    this.#canvas?.addEventListener("webglcontextlost", this.#handleContextLost);
    this.#canvas?.addEventListener("webglcontextrestored", this.#handleContextRestored);
    document.addEventListener("visibilitychange", this.#handleVisibilityChange);
    this.#resizeObserver = new ResizeObserver(() => {
      if (this.#resizeFrame !== null) {
        return;
      }
      this.#resizeFrame = requestAnimationFrame(() => {
        this.#resizeFrame = null;
        if (this.#snapshot.lifecycle === "ready") {
          this.#graph?.render(0, 0);
        }
      });
    });
    this.#resizeObserver.observe(container);
  }

  #detachGraphResources() {
    this.#canvas?.removeEventListener("webglcontextlost", this.#handleContextLost);
    this.#canvas?.removeEventListener("webglcontextrestored", this.#handleContextRestored);
    this.#canvas = null;
    document.removeEventListener("visibilitychange", this.#handleVisibilityChange);
    this.#resizeObserver?.disconnect();
    this.#resizeObserver = null;
    if (this.#resizeFrame !== null) {
      cancelAnimationFrame(this.#resizeFrame);
      this.#resizeFrame = null;
    }
    this.#graph?.destroy();
    this.#graph = null;
  }

  #scheduleSettle() {
    this.#clearSettleTimer();
    const delay = this.#options.view === "symbols" ? 650 : 1_200;
    this.#settleTimer = setTimeout(() => {
      this.#settleTimer = null;
      if (this.#snapshot.lifecycle === "ready") {
        this.#graph?.stop();
        this.#setSnapshot({ simulation: "settled" });
        performance.mark("rootlight.graph.controller.settle");
      }
    }, delay);
  }

  #clearSettleTimer() {
    if (this.#settleTimer !== null) {
      clearTimeout(this.#settleTimer);
      this.#settleTimer = null;
    }
  }

  #readyGraph(): CosmosGraphPort {
    this.#assertActive();
    if (
      this.#graph === null ||
      (this.#snapshot.lifecycle !== "ready" && this.#snapshot.lifecycle !== "updating")
    ) {
      throw new Error("Graph controller is not ready");
    }
    return this.#graph;
  }

  #assertActive() {
    if (this.#snapshot.lifecycle === "disposed") {
      throw new Error("Graph controller is disposed");
    }
  }

  #motionDuration(duration: number) {
    return this.#options.reducedMotion === true ? 0 : duration;
  }

  #normalizeSelection(ordinals: readonly number[]) {
    return this.#normalizeOrdinals(ordinals, 64);
  }

  #normalizeOrdinals(ordinals: readonly number[], maximum: number) {
    const model = this.#model;
    return model === null
      ? []
      : [...new Set(ordinals)]
          .filter((ordinal) => model.ordinalToPointIndex.has(ordinal))
          .slice(0, maximum);
  }

  #setSnapshot(patch: Partial<CosmosGraphControllerSnapshot>) {
    this.#snapshot = { ...this.#snapshot, ...patch };
    for (const subscriber of this.#subscribers) {
      subscriber();
    }
  }
}

async function createDefaultCosmosFactory(
  container: HTMLDivElement,
  config: GraphConfig,
): Promise<CosmosGraphPort> {
  const { Graph: CosmosGraph } = await import("@cosmos.gl/graph");
  return new CosmosGraph(container, config);
}

function sameOrdinals(left: readonly number[], right: readonly number[]) {
  return left.length === right.length && left.every((ordinal, index) => ordinal === right[index]);
}
