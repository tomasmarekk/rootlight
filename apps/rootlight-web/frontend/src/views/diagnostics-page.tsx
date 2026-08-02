// Maps live daemon health into a source-free diagnostic overview.

import { Button } from "@heroui/react/button";
import { useQuery } from "@tanstack/react-query";
import { Activity, Archive, Database, Download, Gauge, Play, Radio } from "lucide-react";
import type { ReactNode } from "react";

import { fetchHealth } from "../api/client";
import { PageHeading } from "../components/page-heading";
import { StatusCard } from "../components/status-card";

export function DiagnosticsPage() {
  const health = useQuery({
    queryKey: ["health"],
    queryFn: ({ signal }) => fetchHealth(signal),
  });

  return (
    <div className="content-container">
      <PageHeading
        eyebrow="Source-free system status"
        title="Diagnostics"
        subtitle="Daemon readiness, capacity, and local subsystem health without repository content."
        actions={
          <>
            <Button size="sm" variant="ghost">
              <Download size={15} aria-hidden="true" />
              Support bundle
            </Button>
            <Button size="sm" variant="primary">
              <Play size={15} aria-hidden="true" />
              Quick diagnostics
            </Button>
          </>
        }
      />
      <section className="metrics-grid metrics-grid--three" aria-label="Daemon health summary">
        <StatusCard
          icon={<Radio size={17} />}
          label="Lifecycle"
          value={health.data?.lifecycle ?? "connecting"}
          detail={`Protocol ${health.data?.protocolVersion ?? "—"}`}
        />
        <StatusCard
          icon={<Activity size={17} />}
          label="Operations"
          value={String(health.data?.activeOperations ?? "—")}
          detail={`${String(health.data?.queuedOperations ?? "—")} queued`}
        />
        <StatusCard
          icon={<Gauge size={17} />}
          label="Resource pressure"
          value={health.data?.resourcePressure ?? "unknown"}
          detail={
            health.data?.acceptingOperations === true ? "Accepting work" : "Admission pending"
          }
        />
      </section>
      <section className="diagnostic-grid" aria-label="Subsystem health">
        <SubsystemStatus
          icon={<Archive size={16} />}
          label="Catalog"
          value={health.data?.catalogStatus}
        />
        <SubsystemStatus
          icon={<Database size={16} />}
          label="Generations"
          value={health.data?.generationStatus}
        />
        <SubsystemStatus
          icon={<Activity size={16} />}
          label="Adapters"
          value={health.data?.adapterStatus}
        />
        <SubsystemStatus
          icon={<Radio size={16} />}
          label="Watchers"
          value={health.data?.watcherStatus}
        />
      </section>
      <p className="privacy-note">
        Support bundles are source-free by contract. Raw source, environment variables, and command
        lines are excluded.
      </p>
    </div>
  );
}

function SubsystemStatus({
  icon,
  label,
  value = "unknown",
}: {
  icon: ReactNode;
  label: string;
  value: string | undefined;
}) {
  return (
    <article className="subsystem-row">
      <span aria-hidden="true">{icon}</span>
      <strong>{label}</strong>
      <span className={`state-label state-label--${statusTone(value)}`}>{value}</span>
    </article>
  );
}

function statusTone(value: string): string {
  if (value === "healthy") {
    return "success";
  }
  if (value === "degraded" || value === "not_configured") {
    return "warning";
  }
  return "neutral";
}
