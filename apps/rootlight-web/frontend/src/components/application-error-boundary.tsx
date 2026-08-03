// Isolates unexpected React failures without rendering repository-derived exception text.

import { Button } from "@heroui/react/button";
import { Component, type ReactNode } from "react";

type BoundaryState = {
  failed: boolean;
};

export class ApplicationErrorBoundary extends Component<{ children: ReactNode }, BoundaryState> {
  public override state: BoundaryState = { failed: false };

  public static getDerivedStateFromError(): BoundaryState {
    return { failed: true };
  }

  public override componentDidCatch() {
    // Exception bodies may contain untrusted repository data, so the local UI does not log them.
  }

  public override render() {
    if (!this.state.failed) {
      return this.props.children;
    }
    return (
      <main className="session-state">
        <div className="brand-mark brand-mark--large" aria-hidden="true">
          R
        </div>
        <p className="eyebrow eyebrow--danger">Interface recovery</p>
        <h1>Rootlight could not render this view</h1>
        <p>
          The local host is still isolated. Reload the interface to clear transient browser state
          and reconnect to the daemon.
        </p>
        <Button variant="primary" onPress={() => window.location.reload()}>
          Reload interface
        </Button>
      </main>
    );
  }
}
