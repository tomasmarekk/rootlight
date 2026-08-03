// Owns browser authentication state without persisting session credentials.

import { Button } from "@heroui/react/button";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState, type ReactNode } from "react";

import { initializeSession, logout, subscribeSessionExpired } from "../api/client";
import type { Session } from "../api/contracts";
import { SessionContext, type SessionContextValue } from "./session-context";

export function SessionProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [state, setState] = useState<
    | { kind: "loading" }
    | { kind: "ready"; session: Session }
    | { kind: "error" }
    | { kind: "ended" }
    | { kind: "expired" }
  >({ kind: "loading" });

  useEffect(() => {
    let active = true;
    const unsubscribe = subscribeSessionExpired(() => {
      if (active) {
        queryClient.clear();
        setState({ kind: "expired" });
      }
    });
    void initializeSession().then(
      (session) => {
        if (active) {
          setState({ kind: "ready", session });
        }
      },
      () => {
        if (active) {
          setState({ kind: "error" });
        }
      },
    );
    return () => {
      active = false;
      unsubscribe();
    };
  }, [queryClient]);

  const value = useMemo<SessionContextValue | undefined>(() => {
    if (state.kind !== "ready") {
      return undefined;
    }
    return {
      session: state.session,
      endSession: async () => {
        await logout();
        setState({ kind: "ended" });
      },
    };
  }, [state]);

  if (state.kind === "loading") {
    return <SessionLoading />;
  }
  if (state.kind === "error") {
    return <SessionFailure />;
  }
  if (state.kind === "ended") {
    return <SessionEnded />;
  }
  if (state.kind === "expired") {
    return <SessionExpired />;
  }
  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}

function SessionLoading() {
  return (
    <main className="session-state" aria-busy="true">
      <div className="brand-mark brand-mark--large" aria-hidden="true">
        R
      </div>
      <p className="eyebrow">Rootlight local session</p>
      <h1>Opening your workspace</h1>
      <p>Establishing an authenticated connection to the local Rootlight host.</p>
      <div className="session-progress" aria-hidden="true" />
    </main>
  );
}

function SessionFailure() {
  return (
    <main className="session-state">
      <div className="brand-mark brand-mark--large" aria-hidden="true">
        R
      </div>
      <p className="eyebrow eyebrow--danger">Session unavailable</p>
      <h1>This Rootlight link is no longer valid</h1>
      <p>Return to the terminal and run the web command again to create a fresh local session.</p>
      <Button variant="primary" onPress={() => window.location.reload()}>
        Retry session
      </Button>
    </main>
  );
}

function SessionEnded() {
  return (
    <main className="session-state">
      <div className="brand-mark brand-mark--large" aria-hidden="true">
        R
      </div>
      <p className="eyebrow">Session closed</p>
      <h1>Rootlight is disconnected from this tab</h1>
      <p>Run the web command again when you want to reopen the local workspace.</p>
    </main>
  );
}

function SessionExpired() {
  return (
    <main className="session-state">
      <div className="brand-mark brand-mark--large" aria-hidden="true">
        R
      </div>
      <p className="eyebrow eyebrow--danger">Session expired</p>
      <h1>This local session has ended</h1>
      <p>
        Sensitive browser state was cleared. Return to the terminal and run the web command again to
        open a new authenticated session.
      </p>
    </main>
  );
}
