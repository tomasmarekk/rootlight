// Provides keyboard and pointer resizing for the project information rail.

import { useRef } from "react";

import {
  maximumWorkspaceRailWidth,
  minimumWorkspaceRailWidth,
} from "../hooks/use-workspace-rail-width";

const keyboardStep = 16;

export function WorkspaceResizer({
  width,
  onWidthChange,
}: {
  width: number;
  onWidthChange: (width: number) => void;
}) {
  const drag = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(null);
  const maximum = maximumWorkspaceRailWidth(window.innerWidth);

  return (
    <div
      className="workspace-resizer"
      role="separator"
      aria-label="Resize project information panel"
      aria-orientation="vertical"
      aria-valuemin={minimumWorkspaceRailWidth}
      aria-valuemax={maximum}
      aria-valuenow={width}
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft") {
          event.preventDefault();
          onWidthChange(width - keyboardStep);
        } else if (event.key === "ArrowRight") {
          event.preventDefault();
          onWidthChange(width + keyboardStep);
        } else if (event.key === "Home") {
          event.preventDefault();
          onWidthChange(minimumWorkspaceRailWidth);
        } else if (event.key === "End") {
          event.preventDefault();
          onWidthChange(maximum);
        }
      }}
      onPointerDown={(event) => {
        drag.current = {
          pointerId: event.pointerId,
          startX: event.clientX,
          startWidth: width,
        };
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        const active = drag.current;
        if (active !== null && active.pointerId === event.pointerId) {
          onWidthChange(active.startWidth + event.clientX - active.startX);
        }
      }}
      onPointerUp={(event) => {
        const active = drag.current;
        if (active !== null && active.pointerId === event.pointerId) {
          drag.current = null;
          event.currentTarget.releasePointerCapture(event.pointerId);
        }
      }}
      onPointerCancel={() => {
        drag.current = null;
      }}
    />
  );
}
