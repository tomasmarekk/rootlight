// Exposes the authenticated session actions to components inside the provider.

import { createContext, useContext } from "react";

import type { Session } from "../api/contracts";

export type SessionContextValue = {
  session: Session;
  endSession: () => Promise<void>;
};

export const SessionContext = createContext<SessionContextValue | undefined>(undefined);

export function useSession(): SessionContextValue {
  const value = useContext(SessionContext);
  if (value === undefined) {
    throw new Error("Session context is unavailable");
  }
  return value;
}
