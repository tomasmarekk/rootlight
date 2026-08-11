// Verifies the React-to-Cosmos boundary, fallback routing, keyboard reset, and unmount disposal.

import type { GraphConfig } from "@cosmos.gl/graph";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { GraphViewport } from "../src/features/graph/components/graph-viewport";
import type {
  CosmosGraphFactory,
  CosmosGraphPort,
} from "../src/features/graph/controller/cosmos-graph-controller";
import { graphLayoutIdentity, graphModelFixture } from "./graph-engine-fixtures";

beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    media: "(prefers-reduced-motion: reduce)",
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  }));
  vi.stubGlobal(
    "ResizeObserver",
    class {
      readonly observe = vi.fn();
      readonly disconnect = vi.fn();
    },
  );
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn(() => 1),
  );
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("GraphViewport", () => {
  it("mounts Cosmos once, synchronizes companion selection, handles Escape, and disposes", async () => {
    const graphs: CosmosGraphPort[] = [];
    const configs: GraphConfig[] = [];
    const onSelectionChange = vi.fn();
    const { unmount } = render(
      <GraphViewport
        model={graphModelFixture()}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="balanced"
        capabilityOverride={{ state: "supported", reason: null }}
        factory={viewportFactory(graphs, configs)}
        onSelectionChange={onSelectionChange}
      />,
    );

    await waitFor(() => expect(graphs[0]?.setPointPositions).toHaveBeenCalled());
    expect(configs).toHaveLength(1);
    const mainNode = screen.getByRole("button", { name: /main.*symbol/i });
    await userEvent.click(mainNode);
    expect(mainNode).toHaveAttribute("aria-pressed", "true");
    expect(onSelectionChange).toHaveBeenLastCalledWith([1]);
    expect(screen.getByRole("button", { name: "Fit selection" })).toBeEnabled();
    await userEvent.click(screen.getByRole("button", { name: "Fit selection" }));
    expect(graphs[0]?.setZoomTransformByPointPositions).toHaveBeenCalled();

    fireEvent.keyDown(screen.getByRole("region", { name: "Code graph" }), {
      key: "Escape",
    });
    expect(onSelectionChange).toHaveBeenLastCalledWith([]);
    await userEvent.click(screen.getByRole("checkbox", { name: "Labels" }));
    expect(screen.getByRole("checkbox", { name: "Labels" })).not.toBeChecked();

    unmount();
    expect(graphs[0]?.destroy).toHaveBeenCalledOnce();
  });

  it("uses the complete text fallback without constructing Cosmos when WebGL is absent", async () => {
    const factory = vi.fn();
    const selection = vi.fn();
    render(
      <GraphViewport
        model={graphModelFixture()}
        layoutIdentity={graphLayoutIdentity}
        view="files"
        budgetProfile="compact"
        capabilityOverride={{
          state: "unsupported",
          reason: "WebGL 2 is unavailable.",
        }}
        factory={factory}
        onSelectionChange={selection}
      />,
    );

    expect(screen.getByText("Graphical view is unavailable")).toBeVisible();
    expect(screen.getByText("WebGL 2 is unavailable.")).toBeVisible();
    expect(factory).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: /config.*file/i }));
    expect(selection).toHaveBeenCalledWith([2]);
  });

  it("synchronizes controlled route selection without emitting a feedback callback", async () => {
    const graphs: CosmosGraphPort[] = [];
    const configs: GraphConfig[] = [];
    const selection = vi.fn();
    const labels = vi.fn();
    const factory = viewportFactory(graphs, configs);
    const { rerender } = render(
      <GraphViewport
        model={graphModelFixture()}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="balanced"
        selectedOrdinals={[2]}
        labelsVisible={false}
        capabilityOverride={{ state: "supported", reason: null }}
        factory={factory}
        onSelectionChange={selection}
        onLabelsVisibleChange={labels}
      />,
    );

    const configNode = await screen.findByRole("button", { name: /config.*file/i });
    await waitFor(() => expect(configNode).toHaveAttribute("aria-pressed", "true"));
    expect(selection).not.toHaveBeenCalled();
    expect(labels).not.toHaveBeenCalled();
    expect(screen.getByRole("checkbox", { name: "Labels" })).not.toBeChecked();
    await userEvent.click(screen.getByRole("checkbox", { name: "Labels" }));
    expect(labels).toHaveBeenCalledWith(true);
    expect(screen.getByRole("checkbox", { name: "Labels" })).not.toBeChecked();
    expect(graphs[0]?.setConfigPartial).toHaveBeenCalledWith({
      outlinedPointIndices: [2],
      highlightedPointIndices: [1, 2],
      highlightedLinkIndices: [1],
    });

    rerender(
      <GraphViewport
        model={{ ...graphModelFixture(), revision: 3 }}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="balanced"
        selectedOrdinals={[1]}
        labelsVisible
        capabilityOverride={{ state: "supported", reason: null }}
        factory={factory}
        onSelectionChange={selection}
        onLabelsVisibleChange={labels}
      />,
    );
    expect(await screen.findByRole("button", { name: /main.*symbol/i })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(selection).not.toHaveBeenCalled();
    expect(screen.getByRole("checkbox", { name: "Labels" })).toBeChecked();

    await userEvent.dblClick(screen.getByRole("button", { name: /config.*file/i }));
    expect(selection).toHaveBeenLastCalledWith([2]);
    expect(screen.getByRole("button", { name: /main.*symbol/i })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    rerender(
      <GraphViewport
        model={{ ...graphModelFixture(), revision: 4 }}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="balanced"
        selectedOrdinals={[2]}
        labelsVisible
        capabilityOverride={{ state: "supported", reason: null }}
        factory={factory}
        onSelectionChange={selection}
        onLabelsVisibleChange={labels}
      />,
    );
    await waitFor(() => expect(graphs[0]?.setZoomTransformByPointPositions).toHaveBeenCalled());
  });

  it("moves initialization failures to fallback and keeps local selection usable", async () => {
    const selection = vi.fn();
    const model = graphModelFixture();
    const factory = vi.fn(() => Promise.reject(new Error("GPU denied")));
    const { rerender } = render(
      <GraphViewport
        model={model}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="balanced"
        selectedOrdinals={[1]}
        overlayOrdinals={[2]}
        labelsVisible
        capabilityOverride={{ state: "supported", reason: null }}
        factory={factory}
        onSelectionChange={selection}
      />,
    );

    expect(await screen.findByText("Graphical view is unavailable")).toBeVisible();
    expect(screen.getByText("The graphics renderer could not be initialized.")).toBeVisible();
    expect(screen.getByRole("button", { name: /main.*symbol/i })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    rerender(
      <GraphViewport
        model={model}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="balanced"
        selectedOrdinals={[2]}
        overlayOrdinals={[1]}
        labelsVisible={false}
        capabilityOverride={{ state: "supported", reason: null }}
        factory={factory}
        onSelectionChange={selection}
      />,
    );
    expect(screen.getByRole("button", { name: /config.*file/i })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("shows a non-blocking capability check while preserving projection context", () => {
    render(
      <GraphViewport
        model={graphModelFixture(1)}
        layoutIdentity={graphLayoutIdentity}
        view="architecture"
        budgetProfile="compact"
        capabilityOverride={{ state: "checking", reason: null }}
      />,
    );

    expect(screen.getByText("Checking graphics support…")).toBeVisible();
    expect(screen.getByText("Partial projection — server limit reached")).toBeVisible();
  });
});

function viewportFactory(graphs: CosmosGraphPort[], configs: GraphConfig[]): CosmosGraphFactory {
  return (container, config) => {
    configs.push(config);
    const canvas = document.createElement("canvas");
    container.append(canvas);
    const graph: CosmosGraphPort = {
      ready: Promise.resolve(),
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
    graphs.push(graph);
    return graph;
  };
}
