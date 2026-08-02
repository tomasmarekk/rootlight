// Renders the persistent navigation and live daemon connection status.

import { Button } from "@heroui/react/button";
import { Chip } from "@heroui/react/chip";
import { useQuery } from "@tanstack/react-query";
import { Activity, Boxes, Command, FolderGit2, LogOut, Search, ShieldCheck } from "lucide-react";
import { NavLink, Outlet } from "react-router";

import { fetchHealth } from "../api/client";
import { useSession } from "../session/session-context";

const navigation = [
  { label: "Projects", path: "/projects", icon: FolderGit2 },
  { label: "Operations", path: "/operations", icon: Activity },
  { label: "Diagnostics", path: "/diagnostics", icon: ShieldCheck },
] as const;

export function AppShell() {
  const { endSession } = useSession();
  const health = useQuery({
    queryKey: ["health"],
    queryFn: ({ signal }) => fetchHealth(signal),
    refetchInterval: 5_000,
  });
  const connection = connectionState(health.data?.daemonReady, health.isError);

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
        <div className="connection-banner" role="status">
          <Boxes size={16} aria-hidden="true" />
          The daemon is temporarily unreachable. Loaded local data remains visible while Rootlight
          reconnects.
        </div>
      ) : null}

      <main id="main-content" className="route-content">
        <Outlet />
      </main>
    </div>
  );
}

function connectionState(ready: boolean | undefined, failed: boolean) {
  if (failed) {
    return { label: "Reconnecting", tone: "warning" } as const;
  }
  if (ready === true) {
    return { label: "Daemon ready", tone: "success" } as const;
  }
  return { label: "Connecting", tone: "neutral" } as const;
}
