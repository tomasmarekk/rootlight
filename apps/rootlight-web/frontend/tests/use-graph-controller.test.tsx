// Verifies that React rerenders cannot duplicate an imperative graph controller.

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { CosmosGraphFactory } from "../src/features/graph/controller/cosmos-graph-controller";
import { useGraphController } from "../src/features/graph/hooks/use-graph-controller";
import { graphLayoutIdentity, graphModelFixture } from "./graph-engine-fixtures";

describe("useGraphController", () => {
  it("retains one controller for equal layout values while forwarding current callbacks", () => {
    const firstSelection = vi.fn();
    const currentSelection = vi.fn();
    const factory = vi.fn() as unknown as CosmosGraphFactory;
    const model = graphModelFixture();
    const { result, rerender } = renderHook(
      ({
        generationId,
        onSelectionChange,
      }: {
        generationId: string;
        onSelectionChange: (ordinals: readonly number[]) => void;
      }) =>
        useGraphController({
          enabled: true,
          model,
          options: {
            controlledSelection: true,
            factory,
            layoutIdentity: { ...graphLayoutIdentity, generationId },
            onSelectionChange,
            view: "architecture",
          },
        }),
      {
        initialProps: {
          generationId: graphLayoutIdentity.generationId,
          onSelectionChange: firstSelection,
        },
      },
    );
    const initialController = result.current.controller;

    rerender({
      generationId: graphLayoutIdentity.generationId,
      onSelectionChange: currentSelection,
    });
    expect(result.current.controller).toBe(initialController);
    act(() => {
      result.current.controller.setSelection([0]);
    });
    expect(firstSelection).not.toHaveBeenCalled();
    expect(currentSelection).toHaveBeenCalledWith([0]);

    rerender({
      generationId: `gen1_${"d".repeat(39)}`,
      onSelectionChange: currentSelection,
    });
    expect(result.current.controller).not.toBe(initialController);
  });
});
