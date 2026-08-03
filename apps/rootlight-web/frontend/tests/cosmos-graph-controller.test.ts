// Verifies the imperative Cosmos lifecycle without requiring a GPU in unit-test CI.

import type { GraphConfig } from "@cosmos.gl/graph";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  CosmosGraphController,
  type CosmosGraphFactory,
  type CosmosGraphPort,
} from "../src/features/graph/controller/cosmos-graph-controller";
import { graphLayoutIdentity, graphModelFixture } from "./graph-engine-fixtures";

type FakeGraph = {
  port: CosmosGraphPort;
  canvas: HTMLCanvasElement;
};

let resizeCallback: ResizeObserverCallback | null;
let animationFrameCallback: FrameRequestCallback | null;

beforeEach(() => {
  vi.useFakeTimers();
  resizeCallback = null;
  animationFrameCallback = null;
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
      }
      readonly observe = vi.fn();
      readonly disconnect = vi.fn();
    },
  );
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    animationFrameCallback = callback;
    return 1;
  });
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("CosmosGraphController", () => {
  it("waits for ready, applies incremental pages without refitting, and projects selection", async () => {
    const graphs: FakeGraph[] = [];
    const configs: GraphConfig[] = [];
    const selectionChanges = vi.fn();
    const factory = fakeFactory(graphs, configs);
    const model = graphModelFixture(1);
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "architecture",
      factory,
      onSelectionChange: selectionChanges,
    });
    const container = document.createElement("div");
    controller.applyModel(model);

    await controller.initialize(container);

    const graph = graphs[0]?.port;
    const config = configs[0];
    if (graph === undefined || config === undefined) {
      throw new Error("Fake Cosmos graph was not created");
    }
    expect(controller.getSnapshot()).toMatchObject({
      lifecycle: "ready",
      simulation: "running",
    });
    expect(graph.setPointPositions).toHaveBeenCalledWith(model.pointPositions, true);
    expect(graph.setLinks).toHaveBeenCalledWith(model.links);
    expect(graph.fitView).toHaveBeenCalledOnce();
    controller.syncSelection([0]);
    expect(controller.getSnapshot().selectedOrdinals).toEqual([0]);
    expect(selectionChanges).not.toHaveBeenCalled();

    resizeCallback?.([], {} as ResizeObserver);
    resizeCallback?.([], {} as ResizeObserver);
    expect(animationFrameCallback).not.toBeNull();
    animationFrameCallback?.(0);
    expect(graph.render).toHaveBeenCalledWith(0, 0);

    controller.applyModel({ ...model, revision: 2 });
    expect(graph.fitView).toHaveBeenCalledOnce();

    controller.syncSelection([]);
    controller.syncOverlay([1]);
    expect(graph.setConfigPartial).toHaveBeenLastCalledWith({
      outlinedPointIndices: [],
      highlightedPointIndices: [1],
      highlightedLinkIndices: [],
    });

    config.onPointClick?.(1, [0, 0], new MouseEvent("click"));
    expect(controller.getSnapshot().selectedOrdinals).toEqual([1]);
    expect(selectionChanges).toHaveBeenLastCalledWith([1]);
    expect(graph.setConfigPartial).toHaveBeenLastCalledWith({
      outlinedPointIndices: [1],
      highlightedPointIndices: [0, 1],
      highlightedLinkIndices: [0],
    });
    expect(controller.fitSelection()).toBe(true);
    expect(graph.setZoomTransformByPointPositions).toHaveBeenCalledOnce();

    controller.pauseSimulation();
    expect(controller.getSnapshot().simulation).toBe("paused");
    controller.resumeSimulation();
    expect(controller.getSnapshot().simulation).toBe("running");
    vi.runAllTimers();
    expect(controller.getSnapshot().simulation).toBe("settled");
    expect(graph.stop).toHaveBeenCalled();

    controller.dispose();
    controller.dispose();
    expect(controller.getSnapshot().lifecycle).toBe("disposed");
    expect(graph.destroy).toHaveBeenCalledOnce();
    expect(() => controller.fitAll()).toThrow("disposed");
  });

  it("supports bounded modifier selection and ignores stale model revisions", async () => {
    const graphs: FakeGraph[] = [];
    const configs: GraphConfig[] = [];
    const model = graphModelFixture();
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "architecture",
      factory: fakeFactory(graphs, configs),
    });
    await controller.initialize(document.createElement("div"));
    controller.applyModel(model);
    const config = configs[0];
    const graph = graphs[0]?.port;
    if (config === undefined || graph === undefined) {
      throw new Error("Fake Cosmos graph was not created");
    }

    config.onPointClick?.(0, [0, 0], new MouseEvent("click"));
    config.onPointClick?.(2, [0, 0], new MouseEvent("click", { ctrlKey: true }));
    expect(controller.getSnapshot().selectedOrdinals).toEqual([0, 2]);
    config.onPointClick?.(0, [0, 0], new MouseEvent("click", { metaKey: true }));
    expect(controller.getSnapshot().selectedOrdinals).toEqual([2]);

    const updateCount = vi.mocked(graph.setPointPositions).mock.calls.length;
    controller.applyModel({ ...model, revision: 0 });
    expect(graph.setPointPositions).toHaveBeenCalledTimes(updateCount);
    controller.resetSelection();
    expect(controller.fitSelection()).toBe(false);
  });

  it("covers reduced motion, hover, visibility, labels, subscriptions, and ready guards", async () => {
    const graphs: FakeGraph[] = [];
    const configs: GraphConfig[] = [];
    const hoverChanges = vi.fn();
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "symbols",
      reducedMotion: true,
      factory: fakeFactory(graphs, configs),
      onHoverChange: hoverChanges,
    });
    const subscriber = vi.fn();
    const unsubscribe = controller.subscribe(subscriber);
    await controller.initialize(document.createElement("div"));
    const graph = graphs[0]?.port;
    const config = configs[0];
    if (graph === undefined || config === undefined) {
      throw new Error("Fake Cosmos graph was not created");
    }

    expect(controller.fitSelection()).toBe(false);
    controller.setSelection([0]);
    expect(controller.getSnapshot().selectedOrdinals).toEqual([]);
    controller.setLabelsVisible(false);
    expect(controller.getSnapshot().labelsVisible).toBe(false);
    controller.applyModel(graphModelFixture());
    expect(graph.stop).toHaveBeenCalled();
    expect(graph.fitView).toHaveBeenCalledWith(0, 0.14, false);
    controller.fitAll();
    expect(graph.fitView).toHaveBeenLastCalledWith(0, 0.14, false);

    config.onPointMouseOver?.(0, [0, 0], undefined, false, false);
    config.onPointMouseOver?.(0, [0, 0], undefined, false, false);
    config.onPointMouseOver?.(99, [0, 0], undefined, false, false);
    config.onPointMouseOut?.(undefined);
    expect(hoverChanges).toHaveBeenCalledWith(0);
    expect(hoverChanges).toHaveBeenCalledWith(null);

    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(graph.pause).toHaveBeenCalled();
    expect(controller.getSnapshot().simulation).toBe("paused");

    expect(() =>
      controller.applyModel({ ...graphModelFixture(), projectionToken: "d".repeat(43) }),
    ).toThrow("immutable projection identity");
    expect(() =>
      controller.applyModel({
        ...graphModelFixture(),
        context: {
          ...graphModelFixture().context,
          generationId: `gen1_${"e".repeat(39)}`,
        },
      }),
    ).toThrow("immutable projection identity");
    await expect(controller.initialize(document.createElement("div"))).rejects.toThrow(
      "initialized once",
    );

    const callsBeforeUnsubscribe = subscriber.mock.calls.length;
    unsubscribe();
    controller.setLabelsVisible(true);
    expect(subscriber).toHaveBeenCalledTimes(callsBeforeUnsubscribe);
    controller.dispose();
  });

  it("recovers one lost context and falls back after a second loss", async () => {
    const graphs: FakeGraph[] = [];
    const configs: GraphConfig[] = [];
    const fallback = vi.fn();
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "files",
      factory: fakeFactory(graphs, configs),
      onFallbackRequired: fallback,
    });
    controller.applyModel(graphModelFixture());
    await controller.initialize(document.createElement("div"));
    const firstCanvas = graphs[0]?.canvas;
    if (firstCanvas === undefined) {
      throw new Error("Initial canvas was not created");
    }

    expect(firstCanvas.dispatchEvent(new Event("webglcontextlost", { cancelable: true }))).toBe(
      false,
    );
    expect(controller.getSnapshot()).toMatchObject({
      lifecycle: "context_lost",
      contextLossCount: 1,
    });
    firstCanvas.dispatchEvent(new Event("webglcontextrestored"));
    await vi.waitFor(() => expect(controller.getSnapshot().lifecycle).toBe("ready"));
    expect(graphs).toHaveLength(2);
    expect(fallback).not.toHaveBeenCalled();

    const recoveredCanvas = graphs[1]?.canvas;
    if (recoveredCanvas === undefined) {
      throw new Error("Recovered canvas was not created");
    }
    recoveredCanvas.dispatchEvent(new Event("webglcontextlost", { cancelable: true }));
    expect(controller.getSnapshot()).toMatchObject({
      lifecycle: "failed",
      contextLossCount: 2,
    });
    expect(fallback).toHaveBeenCalledWith("context_loss");
    controller.dispose();
  });

  it("fails safely when Cosmos initialization rejects", async () => {
    const fallback = vi.fn();
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "architecture",
      factory: () => Promise.reject(new Error("GPU denied")),
      onFallbackRequired: fallback,
    });

    await controller.initialize(document.createElement("div"));

    expect(controller.getSnapshot()).toMatchObject({
      lifecycle: "failed",
      simulation: "paused",
      errorMessage: "The graphics renderer could not be initialized.",
    });
    expect(fallback).toHaveBeenCalledWith("initialization");
  });

  it("destroys a candidate whose ready promise rejects", async () => {
    const fallback = vi.fn();
    const candidate = fakeGraphPort(
      document.createElement("canvas"),
      Promise.reject(new Error("device initialization failed")),
    );
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "architecture",
      factory: () => candidate,
      onFallbackRequired: fallback,
    });

    await controller.initialize(document.createElement("div"));

    expect(candidate.destroy).toHaveBeenCalledOnce();
    expect(fallback).toHaveBeenCalledWith("initialization");
  });

  it("destroys a late graph when disposal wins initialization", async () => {
    const canvas = document.createElement("canvas");
    const candidate = fakeGraphPort(canvas);
    let resolveFactory!: (graph: CosmosGraphPort) => void;
    const factoryPromise = new Promise<CosmosGraphPort>((resolve) => {
      resolveFactory = resolve;
    });
    const controller = new CosmosGraphController({
      layoutIdentity: graphLayoutIdentity,
      view: "architecture",
      factory: () => factoryPromise,
    });

    const initialization = controller.initialize(document.createElement("div"));
    controller.dispose();
    resolveFactory(candidate);
    await initialization;

    expect(candidate.destroy).toHaveBeenCalledOnce();
    expect(controller.getSnapshot().lifecycle).toBe("disposed");
  });
});

function fakeFactory(graphs: FakeGraph[], configs: GraphConfig[]): CosmosGraphFactory {
  return (container, config) => {
    configs.push(config);
    const canvas = document.createElement("canvas");
    container.append(canvas);
    const port = fakeGraphPort(canvas);
    graphs.push({ port, canvas });
    return port;
  };
}

function fakeGraphPort(
  canvas: HTMLCanvasElement,
  ready: Promise<void> = Promise.resolve(),
): CosmosGraphPort {
  return {
    ready,
    destroy: vi.fn(() => {
      canvas.remove();
    }),
    fitView: vi.fn(),
    pause: vi.fn(),
    render: vi.fn(),
    setClusterPositions: vi.fn(),
    setConfigPartial: vi.fn(),
    setLinkColors: vi.fn(),
    setLinks: vi.fn(),
    setLinkStyles: vi.fn(),
    setLinkWidths: vi.fn(),
    setPointClusterStrength: vi.fn(),
    setPointClusters: vi.fn(),
    setPointColors: vi.fn(),
    setPointPositions: vi.fn(),
    setPointShapes: vi.fn(),
    setPointSizes: vi.fn(),
    setZoomTransformByPointPositions: vi.fn(),
    start: vi.fn(),
    stop: vi.fn(),
    unpause: vi.fn(),
  };
}
