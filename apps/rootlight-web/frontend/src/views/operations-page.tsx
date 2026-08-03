// Frames Rootlight-owned operation activity without exposing system processes.

import { Activity, CircleCheck, Clock3 } from "lucide-react";

import { PageHeading } from "../components/page-heading";
import { SessionOperationList } from "../components/session-operation-list";
import { StatusCard } from "../components/status-card";
import { useOperations } from "../operations/operation-context";

export function OperationsPage() {
  const { operations } = useOperations();
  const states = operations.map(
    (operation) => operation.status?.state ?? operation.admission.state,
  );
  const running = states.filter((state) =>
    ["running", "cancelling", "unknown"].includes(state),
  ).length;
  const queued = states.filter((state) => state === "queued").length;
  const terminal = states.length - running - queued;
  return (
    <div className="content-container">
      <PageHeading
        eyebrow="Daemon work queue"
        title="Operations"
        subtitle="Indexing work known to this authenticated browser session."
      />
      <section className="metrics-grid metrics-grid--three" aria-label="Operation summary">
        <StatusCard
          icon={<Activity size={17} />}
          label="Running"
          value={String(running)}
          detail="Current session known set"
        />
        <StatusCard
          icon={<Clock3 size={17} />}
          label="Queued"
          value={String(queued)}
          detail="Bounded daemon admission queue"
        />
        <StatusCard
          icon={<CircleCheck size={17} />}
          label="Recent terminal"
          value={String(terminal)}
          detail="Current session known set"
        />
      </section>
      {operations.length === 0 ? (
        <section className="quiet-panel">
          <div className="empty-state-icon" aria-hidden="true">
            <Activity size={22} />
          </div>
          <h2>No session-known operations</h2>
          <p>
            Operations started from Add project will appear here. This view does not claim a global
            daemon history.
          </p>
        </section>
      ) : (
        <SessionOperationList />
      )}
    </div>
  );
}
