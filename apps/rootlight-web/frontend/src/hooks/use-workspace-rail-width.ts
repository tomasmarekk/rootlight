// Persists only a bounded display width and never repository-specific state.

import { useCallback, useEffect, useState } from "react";

const storageKey = "rootlight:workspace-rail-width:v1";
const preferredMaximumWidth = 460;
const defaultWidth = 320;
const minimumCanvasWidth = 560;

export const minimumWorkspaceRailWidth = 264;

export function useWorkspaceRailWidth() {
  const [width, setWidth] = useState(() => loadWidth(window.innerWidth));

  useEffect(() => {
    function clampToViewport() {
      setWidth((current) => clampWorkspaceRailWidth(current, window.innerWidth));
    }
    window.addEventListener("resize", clampToViewport);
    return () => window.removeEventListener("resize", clampToViewport);
  }, []);

  const update = useCallback((next: number) => {
    const clamped = clampWorkspaceRailWidth(next, window.innerWidth);
    setWidth(clamped);
    try {
      localStorage.setItem(storageKey, String(clamped));
    } catch {
      // Display preferences are optional when browser storage is unavailable.
    }
  }, []);

  return { width, setWidth: update };
}

export function clampWorkspaceRailWidth(width: number, viewportWidth: number): number {
  return Math.min(
    Math.max(Math.round(width), minimumWorkspaceRailWidth),
    maximumWorkspaceRailWidth(viewportWidth),
  );
}

export function maximumWorkspaceRailWidth(viewportWidth: number) {
  return Math.max(
    minimumWorkspaceRailWidth,
    Math.min(preferredMaximumWidth, viewportWidth - minimumCanvasWidth),
  );
}

function loadWidth(viewportWidth: number) {
  try {
    const stored = Number(localStorage.getItem(storageKey));
    return clampWorkspaceRailWidth(
      Number.isFinite(stored) && stored > 0 ? stored : defaultWidth,
      viewportWidth,
    );
  } catch {
    return clampWorkspaceRailWidth(defaultWidth, viewportWidth);
  }
}
