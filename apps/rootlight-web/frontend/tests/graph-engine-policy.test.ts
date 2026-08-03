// Verifies density degradation, Cosmos configuration, and bounded label selection.

import { describe, expect, it, vi } from "vitest";

import {
  createCosmosGraphConfig,
  graphDensityProfile,
} from "../src/features/graph/controller/graph-config";
import {
  selectVisibleGraphLabels,
  type GraphLabelCandidate,
} from "../src/features/graph/controller/label-manager";

describe("graph renderer policy", () => {
  it("degrades transitions, labels, collision, and blending across density bands", () => {
    expect(graphDensityProfile(2_000, 10_000)).toMatchObject({
      band: "a",
      labelBudget: 100,
      linkBlending: true,
    });
    expect(graphDensityProfile(10_000, 30_000).band).toBe("b");
    expect(graphDensityProfile(50_000, 150_000)).toMatchObject({
      band: "c",
      transitionDuration: 0,
      linkBlending: false,
    });
    expect(graphDensityProfile(50_001, 150_001)).toMatchObject({
      band: "d",
      collisionStrength: 0,
      labelBudget: 12,
    });
  });

  it("builds deterministic init config and removes motion on request", () => {
    const onPointClick = vi.fn();
    const onPointHover = vi.fn();
    const onInteractionStart = vi.fn();
    const config = createCosmosGraphConfig({
      randomSeed: "seed",
      view: "architecture",
      nodeCount: 100,
      edgeCount: 200,
      reducedMotion: true,
      callbacks: { onPointClick, onPointHover, onInteractionStart },
    });
    const clickEvent = new MouseEvent("click");

    expect(config).toMatchObject({
      randomSeed: "seed",
      attribution: "Rendered with cosmos.gl",
      transitionDuration: 0,
      fitViewOnInit: false,
      enableDrag: false,
      simulationCollision: 1,
    });
    config.onPointClick?.(2, [0, 0], clickEvent);
    config.onPointMouseOver?.(3, [0, 0], undefined, false, false);
    config.onPointMouseOut?.(undefined);
    config.onZoomStart?.({} as never, true);
    expect(onPointClick).toHaveBeenCalledWith(2, clickEvent);
    expect(onPointHover).toHaveBeenNthCalledWith(1, 3);
    expect(onPointHover).toHaveBeenNthCalledWith(2, null);
    expect(onInteractionStart).toHaveBeenCalledOnce();

    const symbols = createCosmosGraphConfig({
      randomSeed: "seed",
      view: "symbols",
      nodeCount: 60_000,
      edgeCount: 160_000,
      reducedMotion: false,
      callbacks: { onPointClick, onPointHover, onInteractionStart },
    });
    expect(symbols).toMatchObject({
      transitionDuration: 0,
      simulationCollision: 0,
      linkBlending: false,
    });
  });

  it("prioritizes selected labels, rejects overlap, clips text, and honors zero budget", () => {
    const candidates: GraphLabelCandidate[] = [
      candidate(1, "ordinary", 0, 100),
      candidate(2, "selected-with-a-very-long-name", 2, 1, { selected: true }),
      candidate(3, "far", 100, 5),
    ];
    const labels = selectVisibleGraphLabels(candidates, {
      budget: 2,
      maximumTextLength: 12,
    });

    expect(labels.map((label) => label.ordinal)).toEqual([2, 3]);
    expect(labels[0]?.clippedText).toContain("…");
    expect(selectVisibleGraphLabels(candidates, { budget: 0 })).toEqual([]);
    expect(() => selectVisibleGraphLabels(candidates, { budget: -1 })).toThrow(
      "non-negative safe integer",
    );
  });
});

function candidate(
  ordinal: number,
  text: string,
  x: number,
  priority: number,
  state: Partial<Pick<GraphLabelCandidate, "selected" | "hovered" | "directNeighbor">> = {},
): GraphLabelCandidate {
  return {
    ordinal,
    text,
    x,
    y: 0,
    width: 40,
    height: 20,
    priority,
    selected: false,
    hovered: false,
    directNeighbor: false,
    ...state,
  };
}
