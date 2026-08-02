// Frames Rootlight-owned operation activity without exposing system processes.

import { Activity, CircleCheck, Clock3 } from "lucide-react";

import { PageHeading } from "../components/page-heading";
import { StatusCard } from "../components/status-card";

export function OperationsPage() {
  return (
    <div className="content-container">
      <PageHeading
        eyebrow="Daemon work queue"
        title="Operations"
        subtitle="Indexing and maintenance work admitted by Rootlight on this account."
      />
      <section className="metrics-grid metrics-grid--three" aria-label="Operation summary">
        <StatusCard
          icon={<Activity size={17} />}
          label="Running"
          value="—"
          detail="Waiting for operation catalog"
        />
        <StatusCard
          icon={<Clock3 size={17} />}
          label="Queued"
          value="—"
          detail="Bounded daemon admission queue"
        />
        <StatusCard
          icon={<CircleCheck size={17} />}
          label="Recent terminal"
          value="—"
          detail="No snapshot loaded"
        />
      </section>
      <section className="quiet-panel">
        <div className="empty-state-icon" aria-hidden="true">
          <Activity size={22} />
        </div>
        <h2>No operation snapshot loaded</h2>
        <p>Active Rootlight work will appear here with stage, progress, and cancellation state.</p>
      </section>
    </div>
  );
}
