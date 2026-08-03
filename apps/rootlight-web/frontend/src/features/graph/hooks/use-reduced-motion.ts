// Tracks the operating-system reduced-motion preference for graph transitions.
// Listener cleanup keeps long-lived route mounts from accumulating media-query callbacks.

import { useEffect, useState } from "react";

/** Returns whether non-essential Atlas simulation and camera transitions should be suppressed. */
export function useReducedMotion(): boolean {
  const [reducedMotion, setReducedMotion] = useState(false);
  useEffect(() => {
    const query = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => {
      setReducedMotion(query.matches);
    };
    update();
    query.addEventListener("change", update);
    return () => {
      query.removeEventListener("change", update);
    };
  }, []);
  return reducedMotion;
}
