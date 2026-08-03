// Verifies bounded pointer-independent workspace rail sizing.

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { WorkspaceResizer } from "../src/components/workspace-resizer";
import {
  clampWorkspaceRailWidth,
  workspaceRailWidthClass,
} from "../src/hooks/use-workspace-rail-width";

describe("WorkspaceResizer", () => {
  it("clamps the rail against both panel and canvas minimums", () => {
    expect(clampWorkspaceRailWidth(100, 1_440)).toBe(264);
    expect(clampWorkspaceRailWidth(900, 1_440)).toBe(460);
    expect(clampWorkspaceRailWidth(460, 900)).toBe(340);
  });

  it("maps pointer widths to CSP-safe four-pixel classes", () => {
    expect(clampWorkspaceRailWidth(321, 1_440)).toBe(320);
    expect(clampWorkspaceRailWidth(323, 1_440)).toBe(324);
    expect(workspaceRailWidthClass(324)).toBe("workspace-grid--rail-324");
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

  it("tracks only the captured pointer and clears drag state on release or cancellation", () => {
    const onWidthChange = vi.fn();
    render(<WorkspaceResizer width={320} onWidthChange={onWidthChange} />);
    const separator = screen.getByRole("separator", {
      name: "Resize project information panel",
    });
    const setPointerCapture = vi.fn();
    const releasePointerCapture = vi.fn();
    Object.assign(separator, { releasePointerCapture, setPointerCapture });

    fireEvent.pointerMove(separator, { clientX: 90, pointerId: 7 });
    fireEvent.pointerDown(separator, { clientX: 100, pointerId: 7 });
    fireEvent.pointerMove(separator, { clientX: 124, pointerId: 8 });
    fireEvent.pointerMove(separator, { clientX: 124, pointerId: 7 });
    fireEvent.pointerUp(separator, { pointerId: 8 });
    fireEvent.pointerUp(separator, { pointerId: 7 });
    fireEvent.pointerMove(separator, { clientX: 140, pointerId: 7 });

    expect(setPointerCapture).toHaveBeenCalledWith(7);
    expect(releasePointerCapture).toHaveBeenCalledWith(7);
    expect(onWidthChange).toHaveBeenCalledOnce();
    expect(onWidthChange).toHaveBeenCalledWith(344);

    fireEvent.pointerDown(separator, { clientX: 200, pointerId: 9 });
    fireEvent.pointerCancel(separator, { pointerId: 9 });
    fireEvent.pointerMove(separator, { clientX: 250, pointerId: 9 });
    fireEvent.keyDown(separator, { key: "PageDown" });
    expect(onWidthChange).toHaveBeenCalledOnce();
  });
});
