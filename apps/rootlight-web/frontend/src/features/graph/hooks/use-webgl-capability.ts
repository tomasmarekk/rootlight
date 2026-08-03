// Detects renderer support through browser features instead of user-agent assumptions.
// The result distinguishes pending detection from an actionable text fallback.

import { useEffect, useState } from "react";

/** WebGL 2 capability state consumed by Atlas viewport chrome. */
export type WebGlCapability =
  | { state: "checking"; reason: null }
  | { state: "supported"; reason: null }
  | { state: "unsupported"; reason: string };

/** Performs a bounded WebGL 2 and float-render-target feature check. */
export function detectWebGlCapability(): WebGlCapability {
  try {
    const canvas = document.createElement("canvas");
    const context = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
    });
    if (context === null) {
      return {
        state: "unsupported",
        reason: "WebGL 2 is not available in this browser or graphics environment.",
      };
    }
    if (context.getExtension("EXT_color_buffer_float") === null) {
      context.getExtension("WEBGL_lose_context")?.loseContext();
      return {
        state: "unsupported",
        reason: "The required floating-point render target is not available.",
      };
    }
    context.getExtension("WEBGL_lose_context")?.loseContext();
    return { state: "supported", reason: null };
  } catch {
    return {
      state: "unsupported",
      reason: "Graphics capability detection failed safely.",
    };
  }
}

/** Returns the current feature-detected WebGL capability after mount. */
export function useWebGlCapability(
  detector: () => WebGlCapability = detectWebGlCapability,
): WebGlCapability {
  const [capability, setCapability] = useState<WebGlCapability>({
    state: "checking",
    reason: null,
  });
  useEffect(() => {
    let active = true;
    queueMicrotask(() => {
      if (active) {
        setCapability(detector());
      }
    });
    return () => {
      active = false;
    };
  }, [detector]);
  return capability;
}
