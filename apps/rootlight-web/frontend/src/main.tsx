// Boots the authenticated React application and its bounded query cache.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { RootlightRouter } from "./router";
import { OperationProvider } from "./operations/operation-provider";
import { SessionProvider } from "./session/session-provider";
import "./styles/globals.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      gcTime: 5 * 60 * 1_000,
      refetchOnWindowFocus: false,
      retry: 1,
      staleTime: 5_000,
    },
  },
});

const root = document.querySelector<HTMLElement>("#root");
if (root === null) {
  throw new Error("Rootlight application root is unavailable");
}

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <SessionProvider>
        <OperationProvider>
          <RootlightRouter />
        </OperationProvider>
      </SessionProvider>
    </QueryClientProvider>
  </StrictMode>,
);
