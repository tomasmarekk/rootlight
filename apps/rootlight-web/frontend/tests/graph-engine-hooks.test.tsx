// Verifies feature detection and operating-system motion preference hooks.

import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useReducedMotion } from "../src/features/graph/hooks/use-reduced-motion";
import {
  detectWebGlCapability,
  useWebGlCapability,
} from "../src/features/graph/hooks/use-webgl-capability";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("graph capability hooks", () => {
  it("detects missing WebGL, missing float targets, supported contexts, and safe failures", () => {
    const getContext = vi.spyOn(HTMLCanvasElement.prototype, "getContext");
    getContext.mockReturnValueOnce(null);
    const missingWebGl = detectWebGlCapability();
    expect(missingWebGl.state).toBe("unsupported");
    expect(missingWebGl.reason).toContain("WebGL 2");

    const loseContext = vi.fn();
    getContext.mockReturnValueOnce(
      fakeWebGlContext((extension) =>
        extension === "WEBGL_lose_context" ? { loseContext } : null,
      ),
    );
    const missingFloatTarget = detectWebGlCapability();
    expect(missingFloatTarget.state).toBe("unsupported");
    expect(missingFloatTarget.reason).toContain("floating-point");
    expect(loseContext).toHaveBeenCalledOnce();

    getContext.mockReturnValueOnce(fakeWebGlContext(() => ({ loseContext })));
    expect(detectWebGlCapability()).toEqual({ state: "supported", reason: null });

    getContext.mockImplementationOnce(() => {
      throw new Error("blocked");
    });
    expect(detectWebGlCapability()).toMatchObject({
      state: "unsupported",
      reason: "Graphics capability detection failed safely.",
    });
  });

  it("moves from checking to the injected capability after mount", async () => {
    const detector = vi.fn(() => ({ state: "supported", reason: null }) as const);
    const { result } = renderHook(() => useWebGlCapability(detector));
    expect(result.current.state).toBe("checking");
    await waitFor(() => expect(result.current.state).toBe("supported"));
    expect(detector).toHaveBeenCalledOnce();
  });

  it("tracks reduced-motion media query changes and removes its listener", () => {
    let matches = true;
    let listener: (() => void) | null = null;
    const removeEventListener = vi.fn();
    vi.stubGlobal("matchMedia", () => ({
      get matches() {
        return matches;
      },
      media: "(prefers-reduced-motion: reduce)",
      onchange: null,
      addEventListener: (_type: string, callback: () => void) => {
        listener = callback;
      },
      removeEventListener,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }));

    const { result, unmount } = renderHook(() => useReducedMotion());
    expect(result.current).toBe(true);
    matches = false;
    act(() => {
      listener?.();
    });
    expect(result.current).toBe(false);
    unmount();
    expect(removeEventListener).toHaveBeenCalledWith("change", expect.any(Function));
  });
});

function fakeWebGlContext(
  getExtension: (extension: string) => object | null,
): WebGL2RenderingContext {
  return { getExtension } as unknown as WebGL2RenderingContext;
}
