// Verifies graph HUD truthfulness, accessible controls, virtualization, and text fallback.

import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { GraphCompanionList } from "../src/features/graph/components/graph-companion-list";
import { GraphFallback } from "../src/features/graph/components/graph-fallback";
import { GraphHud } from "../src/features/graph/components/graph-hud";
import { GraphToolbar } from "../src/features/graph/components/graph-toolbar";
import { graphModelFixture } from "./graph-engine-fixtures";

describe("graph engine components", () => {
  it("reports returned versus total counts and explicit partial loading state", () => {
    render(
      <GraphHud
        model={graphModelFixture(1)}
        budgetProfile="compact"
        visibleNodeCount={1}
        loadingNextPage
      />,
    );

    expect(screen.getByText("2 of 3")).toBeVisible();
    expect(screen.getByText("1 of 2")).toBeVisible();
    expect(screen.getByText("Partial projection — server limit reached")).toBeVisible();
    expect(screen.getByRole("status")).toHaveTextContent("Loading the next bounded page");
  });

  it("labels every server completeness state without implying hidden data is complete", () => {
    const model = graphModelFixture();
    const { rerender } = render(
      <GraphHud
        model={{
          ...model,
          completeness: { ...model.completeness, state: "unsupported_partial" },
        }}
        budgetProfile="balanced"
      />,
    );
    expect(screen.getByText("Partial projection — some relations are unsupported")).toBeVisible();
    rerender(
      <GraphHud
        model={{
          ...model,
          completeness: { ...model.completeness, state: "indeterminate" },
        }}
        budgetProfile="expanded"
      />,
    );
    expect(screen.getByText("Projection completeness is indeterminate")).toBeVisible();
  });

  it("searches returned nodes and keeps selection and fit actions keyboard accessible", async () => {
    const onSelect = vi.fn();
    render(
      <GraphCompanionList
        model={graphModelFixture()}
        selectedOrdinals={[1]}
        overlayOrdinals={[2]}
        onSelect={onSelect}
        height={120}
      />,
    );

    const selected = screen.getByRole("button", { name: /main.*symbol/i });
    expect(selected).toHaveAttribute("aria-pressed", "true");
    selected.focus();
    await userEvent.keyboard("{Enter}");
    expect(onSelect).toHaveBeenCalledWith(1, false);
    await userEvent.dblClick(selected);
    expect(onSelect).toHaveBeenLastCalledWith(1, true);
    const impacted = screen.getByRole("button", { name: /config.*change impact/i });
    expect(impacted).toHaveAttribute("data-impact-overlay", "true");

    await userEvent.type(screen.getByRole("searchbox", { name: "Search visible nodes" }), "config");
    expect(screen.getByText("1 of 3 returned nodes")).toBeVisible();
    expect(screen.getByRole("button", { name: /config.*file/i })).toBeVisible();
    expect(screen.queryByRole("button", { name: /main.*symbol/i })).not.toBeInTheDocument();
    await userEvent.clear(screen.getByRole("searchbox", { name: "Search visible nodes" }));
    await userEvent.type(screen.getByRole("searchbox", { name: "Search visible nodes" }), "absent");
    expect(screen.getByText("No returned nodes match this local filter.")).toBeVisible();
  });

  it("exposes bounded toolbar controls and textual WebGL status", async () => {
    const actions = {
      onFitAll: vi.fn(),
      onFitSelection: vi.fn(),
      onResetSelection: vi.fn(),
      onLabelsVisibleChange: vi.fn(),
      onPauseSimulation: vi.fn(),
      onResumeSimulation: vi.fn(),
    };
    render(
      <GraphToolbar
        capability={{ state: "supported", reason: null }}
        labelsVisible
        simulation="running"
        hasSelection
        {...actions}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Fit all" }));
    await userEvent.click(screen.getByRole("button", { name: "Fit selection" }));
    await userEvent.click(screen.getByRole("button", { name: "Clear selection" }));
    await userEvent.click(screen.getByRole("checkbox", { name: "Labels" }));
    await userEvent.click(screen.getByText("Advanced"));
    await userEvent.click(screen.getByRole("button", { name: "Pause layout" }));

    expect(actions.onFitAll).toHaveBeenCalledOnce();
    expect(actions.onFitSelection).toHaveBeenCalledOnce();
    expect(actions.onResetSelection).toHaveBeenCalledOnce();
    expect(actions.onLabelsVisibleChange).toHaveBeenCalledWith(false);
    expect(actions.onPauseSimulation).toHaveBeenCalledOnce();
    expect(screen.getByLabelText("WebGL status")).toHaveTextContent("WebGL 2 ready");
  });

  it("disables graphics actions while checking and exposes resume after settlement", async () => {
    const resume = vi.fn();
    const { rerender } = render(
      <GraphToolbar
        capability={{ state: "checking", reason: null }}
        labelsVisible={false}
        simulation="settled"
        hasSelection={false}
        onFitAll={vi.fn()}
        onFitSelection={vi.fn()}
        onResetSelection={vi.fn()}
        onLabelsVisibleChange={vi.fn()}
        onPauseSimulation={vi.fn()}
        onResumeSimulation={resume}
      />,
    );
    expect(screen.getByRole("button", { name: "Fit all" })).toBeDisabled();
    expect(screen.getByLabelText("WebGL status")).toHaveTextContent("Checking WebGL 2");
    await userEvent.click(screen.getByText("Advanced"));
    expect(screen.getByRole("button", { name: "Resume layout" })).toBeDisabled();

    rerender(
      <GraphToolbar
        capability={{ state: "supported", reason: null }}
        labelsVisible={false}
        simulation="paused"
        hasSelection={false}
        onFitAll={vi.fn()}
        onFitSelection={vi.fn()}
        onResetSelection={vi.fn()}
        onLabelsVisibleChange={vi.fn()}
        onPauseSimulation={vi.fn()}
        onResumeSimulation={resume}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Resume layout" }));
    expect(resume).toHaveBeenCalledOnce();
  });

  it("retains HUD and companion selection in the no-WebGL fallback", async () => {
    const onSelect = vi.fn();
    const retry = vi.fn();
    render(
      <GraphFallback
        model={graphModelFixture()}
        budgetProfile="balanced"
        reason="WebGL 2 is unavailable."
        selectedOrdinals={[]}
        onSelect={onSelect}
        onRetry={retry}
      />,
    );

    const fallback = screen.getByRole("heading", {
      name: "Graphical view is unavailable",
    }).parentElement;
    if (fallback === null) {
      throw new Error("Fallback region is unavailable");
    }
    expect(within(fallback).getByText("Complete projection")).toBeVisible();
    await userEvent.click(within(fallback).getByRole("button", { name: "Retry graphics" }));
    await userEvent.click(within(fallback).getByRole("button", { name: /main.*symbol/i }));
    expect(retry).toHaveBeenCalledOnce();
    expect(onSelect).toHaveBeenCalledWith(1, false);
  });

  it("omits retry when fallback recovery is not safe", () => {
    const model = graphModelFixture();
    const node = model.nodes[0];
    if (node === undefined) {
      throw new Error("Graph fixture must contain a node");
    }
    render(
      <GraphFallback
        model={{ ...model, nodes: [{ ...node, path: null }], nodeOrdinals: new Uint32Array([0]) }}
        budgetProfile="compact"
        reason="Graphics are unavailable."
        selectedOrdinals={[]}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.queryByRole("button", { name: "Retry graphics" })).not.toBeInTheDocument();
    expect(screen.getByText("No path context")).toBeVisible();
  });
});
