// Maps live daemon health and source-free local diagnostics into one bounded surface.

import { Button } from "@heroui/react/button";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  Activity,
  Archive,
  Database,
  Download,
  Gauge,
  Play,
  Radio,
  ShieldCheck,
  TriangleAlert,
} from "lucide-react";
import { useState, type ReactNode } from "react";

import {
  ApiError,
  createSupportBundle,
  downloadSupportBundle,
  fetchHealth,
  runQuickDiagnostics,
} from "../api/client";
import type { DiagnosticCheck } from "../api/contracts";
import { PageHeading } from "../components/page-heading";
import { StatusCard } from "../components/status-card";

export function DiagnosticsPage() {
  const [downloadedReceipt, setDownloadedReceipt] = useState<string>();
  const health = useQuery({
    queryKey: ["health"],
    queryFn: ({ signal }) => fetchHealth(signal),
    retry: false,
  });
  const quick = useMutation({
    mutationFn: () => runQuickDiagnostics(),
  });
  const support = useMutation({
    mutationFn: () => createSupportBundle(),
  });
  const download = useMutation({
    mutationFn: (bundle: Parameters<typeof downloadSupportBundle>[0]) =>
      downloadSupportBundle(bundle),
    onSuccess: (_, bundle) => setDownloadedReceipt(bundle.receipt),
  });

  function runRequest(request: Promise<unknown>) {
    void request.catch(() => undefined);
  }

  function prepareBundle() {
    setDownloadedReceipt(undefined);
    download.reset();
    runRequest(support.mutateAsync());
  }

  return (
    <div className="content-container">
      <PageHeading
        eyebrow="Source-free system status"
        title="Diagnostics"
        subtitle="Daemon readiness, bounded capacity, and local health without repository content."
        actions={
          <>
            <Button
              isDisabled={
                support.isPending ||
                download.isPending ||
                (support.data !== undefined && downloadedReceipt === undefined)
              }
              size="sm"
              variant="ghost"
              onPress={prepareBundle}
            >
              <Download size={15} aria-hidden="true" />
              {support.isPending ? "Preparing bundle" : "Prepare support bundle"}
            </Button>
            <Button
              isDisabled={quick.isPending}
              size="sm"
              variant="primary"
              onPress={() => runRequest(quick.mutateAsync())}
            >
              <Play size={15} aria-hidden="true" />
              {quick.isPending ? "Running checks" : "Quick diagnostics"}
            </Button>
          </>
        }
      />

      {health.isError ? <RequestError message="Live daemon health is unavailable." /> : null}

      <section className="metrics-grid" aria-label="Daemon health summary">
        <StatusCard
          icon={<Radio size={17} />}
          label="Lifecycle"
          value={health.data?.lifecycle ?? "connecting"}
          detail={`${health.data?.webReady === true ? "Web ready" : "Web pending"} · ${
            health.data?.daemonReady === true ? "daemon ready" : "daemon pending"
          }`}
        />
        <StatusCard
          icon={<Activity size={17} />}
          label="Operation slots"
          value={
            health.data === undefined
              ? "—"
              : `${String(health.data.admittedOperations)} / ${String(
                  health.data.operationQueueLimit,
                )}`
          }
          detail={
            health.data === undefined
              ? "Waiting for daemon"
              : `${String(health.data.runningOperations)} running · ${String(
                  health.data.queuedOperations,
                )} queued · ${String(health.data.activeOperations)} active`
          }
        />
        <StatusCard
          icon={<Radio size={17} />}
          label="Connections"
          value={
            health.data === undefined
              ? "—"
              : `${String(health.data.activeConnections)} / ${String(health.data.connectionLimit)}`
          }
          detail="Active daemon connections"
        />
        <StatusCard
          icon={<Gauge size={17} />}
          label="Resource pressure"
          value={health.data?.resourcePressure ?? "unknown"}
          detail={health.data?.acceptingOperations === true ? "Accepting work" : "Admission paused"}
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
        <SubsystemStatus
          icon={<Radio size={16} />}
          label="Endpoint"
          value={health.data?.endpointStatus}
        />
        <SubsystemStatus
          icon={<Archive size={16} />}
          label="Journal"
          value={
            health.data === undefined
              ? undefined
              : health.data.journalHealthy
                ? "healthy"
                : "unavailable"
          }
        />
      </section>

      <dl className="diagnostic-facts" aria-label="Protocol schema versions">
        <div>
          <dt>Protocol</dt>
          <dd>{health.data?.protocolVersion ?? "—"}</dd>
        </div>
        <div>
          <dt>Catalog schema</dt>
          <dd>{health.data?.catalogSchemaVersion ?? "—"}</dd>
        </div>
        <div>
          <dt>Endpoint schema</dt>
          <dd>{health.data?.endpointSchemaVersion ?? "—"}</dd>
        </div>
      </dl>

      <section className="diagnostic-actions-grid" aria-label="Local diagnostic actions">
        <article className="diagnostic-action-card" aria-live="polite">
          <div className="diagnostic-action-card__heading">
            <Play size={17} aria-hidden="true" />
            <div>
              <h2>Quick diagnostics</h2>
              <p>Runs the daemon-owned bounded check set.</p>
            </div>
          </div>
          {quick.isPending ? <p>Running local checks…</p> : null}
          {quick.isError ? (
            <RequestError message={`Quick diagnostics failed: ${errorLabel(quick.error)}.`} />
          ) : null}
          {quick.data === undefined ? null : (
            <>
              <div className="diagnostic-result-summary">
                <span
                  className={`state-label state-label--${statusTone(quick.data.overallStatus)}`}
                >
                  {quick.data.overallStatus}
                </span>
                <span>{quick.data.durationMs} ms</span>
                <span>Schema {quick.data.schemaVersion}</span>
              </div>
              <ul className="diagnostic-check-list">
                {quick.data.checks.map((check) => (
                  <DiagnosticCheckRow key={check.name} check={check} />
                ))}
              </ul>
            </>
          )}
        </article>

        <article className="diagnostic-action-card" aria-live="polite">
          <div className="diagnostic-action-card__heading">
            <ShieldCheck size={17} aria-hidden="true" />
            <div>
              <h2>Local support bundle</h2>
              <p>Creates one source-free, short-lived archive.</p>
            </div>
          </div>
          {support.isPending ? <p>Preparing the local archive…</p> : null}
          {support.isError ? (
            <RequestError message={`Support bundle failed: ${errorLabel(support.error)}.`} />
          ) : null}
          {support.data === undefined ? null : (
            <div className="support-bundle-result">
              <dl>
                <div>
                  <dt>Archive</dt>
                  <dd>{formatBytes(support.data.archiveBytes)}</dd>
                </div>
                <div>
                  <dt>Contains source</dt>
                  <dd>no</dd>
                </div>
                <div>
                  <dt>Expires</dt>
                  <dd>{support.data.expiresInSeconds} seconds</dd>
                </div>
                <div>
                  <dt>SHA-256</dt>
                  <dd title={support.data.sha256}>{support.data.sha256}</dd>
                </div>
              </dl>
              {downloadedReceipt === support.data.receipt ? (
                <p className="support-bundle-consumed">
                  This single-use archive was downloaded. Prepare a new bundle to download again.
                </p>
              ) : (
                <Button
                  isDisabled={download.isPending}
                  size="sm"
                  variant="primary"
                  onPress={() => runRequest(download.mutateAsync(support.data))}
                >
                  <Download size={15} aria-hidden="true" />
                  {download.isPending ? "Verifying archive" : "Verify and download"}
                </Button>
              )}
              {download.isError ? (
                <RequestError message={`Download failed: ${errorLabel(download.error)}.`} />
              ) : null}
            </div>
          )}
        </article>
      </section>

      <p className="privacy-note">
        Support bundles stay on this machine and are source-free by contract. Raw source,
        environment variables, and command lines are excluded.
      </p>
    </div>
  );
}

function DiagnosticCheckRow({ check }: { check: DiagnosticCheck }) {
  return (
    <li>
      <div>
        <strong>{check.name}</strong>
        <span>{check.durationMs} ms</span>
      </div>
      <span className={`state-label state-label--${statusTone(check.outcome)}`}>
        {check.outcome.replaceAll("_", " ")}
      </span>
      {check.error === null ? null : (
        <p>
          {check.error.message}
          {check.error.retryable ? " Retry is allowed." : ""}
        </p>
      )}
    </li>
  );
}

function RequestError({ message }: { message: string }) {
  return (
    <div className="diagnostic-error" role="alert">
      <TriangleAlert size={15} aria-hidden="true" />
      {message}
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
  if (value === "healthy" || value === "passed") {
    return "success";
  }
  if (value === "degraded" || value === "not_configured" || value === "timed_out") {
    return "warning";
  }
  return "neutral";
}

function errorLabel(error: Error): string {
  return error instanceof ApiError ? error.code.replaceAll("_", " ") : "request failed";
}

function formatBytes(value: string): string {
  return `${new Intl.NumberFormat("en-US").format(BigInt(value))} bytes`;
}
