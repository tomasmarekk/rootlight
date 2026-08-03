// Verifies bounded pointer-independent workspace rail sizing.

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { WorkspaceResizer } from "../src/components/workspace-resizer";
import { clampWorkspaceRailWidth } from "../src/hooks/use-workspace-rail-width";

describe("WorkspaceResizer", () => {
  it("clamps the rail against both panel and canvas minimums", () => {
    expect(clampWorkspaceRailWidth(100, 1_440)).toBe(264);
    expect(clampWorkspaceRailWidth(900, 1_440)).toBe(460);
    expect(clampWorkspaceRailWidth(460, 900)).toBe(340);
  });

  it("supports arrow, home, and end keyboard resizing", () => {
    const onWidthChange = vi.fn();
    render(<WorkspaceResizer width={320} onWidthChange={onWidthChange} />);
    const separator = screen.getByRole("separator", {
      name: "Resize project information panel",
    });

    fireEvent.keyDown(separator, { key: "ArrowLeft" });
    fireEvent.keyDown(separator, { key: "ArrowRight" });
    fireEvent.keyDown(separator, { key: "Home" });
    fireEvent.keyDown(separator, { key: "End" });

    expect(onWidthChange).toHaveBeenNthCalledWith(1, 304);
    expect(onWidthChange).toHaveBeenNthCalledWith(2, 336);
    expect(onWidthChange).toHaveBeenNthCalledWith(3, 264);
    expect(onWidthChange).toHaveBeenNthCalledWith(4, 460);
  });
});
