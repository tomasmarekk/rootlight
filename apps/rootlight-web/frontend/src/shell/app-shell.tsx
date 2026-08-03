// Renders the persistent navigation and live daemon connection status.

import { Button } from "@heroui/react/button";
import { Chip } from "@heroui/react/chip";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  Boxes,
  Command,
  FolderGit2,
  LogOut,
  RotateCcw,
  Search,
  ShieldCheck,
} from "lucide-react";
import { useEffect, useRef } from "react";
import { NavLink, Outlet } from "react-router";

import { fetchHealth, publishDaemonReconnected } from "../api/client";
import type { Health } from "../api/contracts";
import { useSession } from "../session/session-context";

const navigation = [
  { label: "Projects", path: "/projects", icon: FolderGit2 },
  { label: "Operations", path: "/operations", icon: Activity },
  { label: "Diagnostics", path: "/diagnostics", icon: ShieldCheck },
] as const;

export function AppShell() {
  const { endSession } = useSession();
  const queryClient = useQueryClient();
  const reconnecting = useRef(false);
  const health = useQuery({
    queryKey: ["health"],
    queryFn: ({ signal }) => fetchHealth(signal),
    refetchInterval: 5_000,
    retry: 2,
    retryDelay: (attempt) => Math.min(500 * 2 ** attempt, 5_000),
  });
  const daemonReady = isDaemonReady(health.data);
  const connection = connectionState(health.data, health.isError);

  useEffect(() => {
    if (health.isError || (health.data !== undefined && !daemonReady)) {
      reconnecting.current = true;
    } else if (daemonReady && reconnecting.current) {
      reconnecting.current = false;
      void queryClient.invalidateQueries({
        predicate: (query) => query.queryKey[0] !== "health",
      });
      publishDaemonReconnected();
    }
  }, [daemonReady, health.data, health.isError, queryClient]);

  return (
    <div className="app-frame">
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>
      <header className="app-header">
        <div className="brand-cluster">
          <div className="brand-mark" aria-hidden="true">
            R
          </div>
          <div className="brand-copy">
            <span className="brand-name">Rootlight</span>
            <span className="daemon-line">
              <span className={`status-dot status-dot--${connection.tone}`} aria-hidden="true" />
              {connection.label}
            </span>
          </div>
        </div>

        <nav className="primary-nav" aria-label="Primary">
          {navigation.map(({ icon: Icon, label, path }) => (
            <NavLink
              className={({ isActive }) => `nav-link${isActive ? " nav-link--active" : ""}`}
              key={path}
              to={path}
            >
              <Icon size={15} strokeWidth={1.8} aria-hidden="true" />
              {label}
            </NavLink>
          ))}
        </nav>

        <div className="header-actions">
          <button className="command-button" type="button" aria-label="Open global search">
            <Search size={15} aria-hidden="true" />
            <span>Search</span>
            <kbd>
              <Command size={11} aria-hidden="true" />K
            </kbd>
          </button>
          <Chip
            className={`connection-chip connection-chip--${connection.tone}`}
            color={connection.tone === "success" ? "success" : "warning"}
            size="sm"
            variant="soft"
          >
            {connection.label}
          </Chip>
          <Button
            aria-label="End local session"
            isIconOnly
            size="sm"
            variant="ghost"
            onPress={() => void endSession()}
          >
            <LogOut size={16} aria-hidden="true" />
          </Button>
        </div>
      </header>

      {health.isError ? (
        <div className="connection-banner" role="status" aria-live="polite">
          <Boxes size={16} aria-hidden="true" />
          <span>
            The daemon is temporarily unreachable. Loaded source-free data remains visible while
            Rootlight reconnects.
          </span>
          <Button
            isDisabled={health.isFetching}
            size="sm"
            variant="ghost"
            onPress={() => void health.refetch()}
          >
            <RotateCcw size={13} aria-hidden="true" />
            Retry now
          </Button>
        </div>
      ) : null}

      <main id="main-content" className="route-content">
        <Outlet />
      </main>
    </div>
  );
}

function connectionState(health: Health | undefined, failed: boolean) {
  if (failed) {
    return { label: "Reconnecting", tone: "warning" } as const;
  }
  if (isDaemonReady(health)) {
    return { label: "Daemon ready", tone: "success" } as const;
  }
  return { label: "Connecting", tone: "neutral" } as const;
}

function isDaemonReady(health: Health | undefined) {
  return health?.webReady === true && health.daemonReady && health.lifecycle === "ready";
}
