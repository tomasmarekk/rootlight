// Exposes the first-party query client through the browser's React tree.
// Keeping this component separate preserves fast-refresh isolation for the hooks.

import type { ReactNode } from "react";

import { QueryContext, type QueryClient } from "./local-query";

/** Makes one session-owned query client available to browser components. */
export function QueryClientProvider({
  client,
  children,
}: {
  client: QueryClient;
  children: ReactNode;
}) {
  return <QueryContext.Provider value={client}>{children}</QueryContext.Provider>;
}
