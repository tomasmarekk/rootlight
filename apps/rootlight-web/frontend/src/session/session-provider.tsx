// Owns browser authentication state without persisting session credentials.

import { Button } from "@heroui/react/button";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState, type ReactNode } from "react";

import { initializeSession, logout, subscribeSessionExpired } from "../api/client";
import type { Session } from "../api/contracts";
import { SessionContext, type SessionContextValue } from "./session-context";

export function SessionProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<
    | { kind: "loading" }
    | { kind: "ready"; session: Session }
    | { kind: "error" }
    | { kind: "ended" }
  >({ kind: "loading" });

  useEffect(() => {
    let active = true;
    const unsubscribe = subscribeSessionExpired(() => {
      if (active) {
        // A replacement service requires a fresh CLI bootstrap, so clear all
        // data from the old in-memory session before the status check fails closed.
        queryClient.clear();
        setState({ kind: "loading" });
        setAttempt((current) => current + 1);
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
  }, [attempt, queryClient]);

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
    return (
      <SessionFailure
        onRetry={() => {
          setState({ kind: "loading" });
          setAttempt((current) => current + 1);
        }}
      />
    );
  }
  if (state.kind === "ended") {
    return <SessionEnded />;
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

function SessionFailure({ onRetry }: { onRetry: () => void }) {
  return (
    <main className="session-state">
      <div className="brand-mark brand-mark--large" aria-hidden="true">
        R
      </div>
      <p className="eyebrow eyebrow--danger">Local service unavailable</p>
      <h1>Rootlight could not reconnect</h1>
      <p>The local service may still be starting. Retry the connection in a moment.</p>
      <Button variant="primary" onPress={onRetry}>
        Retry connection
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
      <p>Reload the page when you want to reconnect to the local workspace.</p>
    </main>
  );
}
