// Tracks authoritative daemon revisions for operations admitted by this browser session.

import { Button } from "@heroui/react/button";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  Check,
  CircleCheck,
  CircleX,
  Copy,
  ExternalLink,
  LoaderCircle,
  RotateCcw,
  Square,
  TriangleAlert,
  X,
} from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { Link } from "react-router";

import { cancelIndexOperation, fetchIndexOperation } from "../api/client";
import type { OperationState, RepositoryOperation } from "../api/contracts";
import { useOperations, type SessionOperation } from "../operations/operation-context";
import { NativeDialog } from "./native-dialog";

const operationWaitMs = 15_000;
const terminalStates = new Set<OperationState>(["succeeded", "failed", "interrupted", "cancelled"]);

export function SessionOperationList({
  focusOperationId,
  onFocused,
}: {
  focusOperationId?: string;
  onFocused?: () => void;
}) {
  const { operations, dismiss } = useOperations();
  if (operations.length === 0) {
    return null;
  }
  return (
    <section className="session-operations" aria-labelledby="session-operations-heading">
      <div className="session-operations__heading">
        <div>
          <p className="eyebrow">Current browser session</p>
          <h2 id="session-operations-heading">Index operations</h2>
        </div>
        <span>{operations.length} known</span>
      </div>
      <p className="session-operations__scope">
        This bounded list contains operations started in this browser session. Detached work
        continues in the daemon if this tab closes.
      </p>
      <div className="session-operation-list" aria-live="polite">
        {operations.map((operation) => (
          <SessionOperationCard
            key={operation.admission.operationId}
            focusOperationId={focusOperationId}
            operation={operation}
            onDismiss={() => dismiss(operation.admission.operationId)}
            onFocused={onFocused}
          />
        ))}
      </div>
    </section>
  );
}

function SessionOperationCard({
  focusOperationId,
  operation,
  onDismiss,
  onFocused,
}: {
  focusOperationId?: string;
  operation: SessionOperation;
  onDismiss: () => void;
  onFocused?: () => void;
}) {
  const semanticOperationId =
    operation.status?.semanticOperationId ?? operation.admission.semanticOperationId;
  return (
    <article className="session-operation-card">
      <TrackedOperation
        focus={focusOperationId === operation.admission.operationId}
        operation={operation}
        operationId={operation.admission.operationId}
        onDismiss={onDismiss}
        onFocused={onFocused}
      />
      {semanticOperationId === null ? null : (
        <div className="semantic-operation">
          <span className="semantic-operation__connector" aria-hidden="true" />
          <TrackedOperation isSemantic operation={operation} operationId={semanticOperationId} />
        </div>
      )}
    </article>
  );
}

function TrackedOperation({
  focus = false,
  isSemantic = false,
  operation,
  operationId,
  onDismiss,
  onFocused,
}: {
  focus?: boolean;
  isSemantic?: boolean;
  operation: SessionOperation;
  operationId: string;
  onDismiss?: () => void;
  onFocused?: () => void;
}) {
  const { update } = useOperations();
  const queryClient = useQueryClient();
  const rowReference = useRef<HTMLDivElement>(null);
  const invalidatedGeneration = useRef<string | undefined>(undefined);
  const current = isSemantic ? operation.semanticStatus : operation.status;
  const initialState = isSemantic ? "queued" : operation.admission.state;
  const state = current?.state ?? initialState;
  const isTerminal = terminalStates.has(state);
  const knownRevision =
    current?.revision ?? (isSemantic ? undefined : operation.admission.revision);
  const publishedGenerationId =
    current?.publishedGenerationId ??
    (isSemantic ? null : operation.admission.publishedGenerationId);
  const waitingForSemanticAdmission = isSemantic && current === undefined && state === "queued";
  const status = useQuery({
    queryKey: ["index-operation", operationId],
    queryFn: ({ signal }) =>
      fetchIndexOperation(
        operationId,
        knownRevision === undefined
          ? {}
          : { waitMs: operationWaitMs, afterRevision: knownRevision },
        signal,
      ),
    enabled: !isTerminal,
    retry: 1,
    refetchInterval: isTerminal ? false : 250,
  });
  const cancel = useMutation({
    mutationFn: () => cancelIndexOperation(operationId),
    onSuccess: (response) => {
      update(operationId, response.operation);
    },
  });
  const [confirmingCancel, setConfirmingCancel] = useState(false);

  useEffect(() => {
    if (status.data !== undefined) {
      update(operationId, status.data);
    }
  }, [operationId, status.data, update]);

  useEffect(() => {
    if (publishedGenerationId !== null && invalidatedGeneration.current !== publishedGenerationId) {
      invalidatedGeneration.current = publishedGenerationId;
      void queryClient.invalidateQueries({ queryKey: ["projects"] });
    }
  }, [publishedGenerationId, queryClient]);

  useEffect(() => {
    if (focus) {
      rowReference.current?.focus();
      onFocused?.();
    }
  }, [focus, onFocused]);

  const cancellationRequested = current?.cancellationRequested ?? state === "cancelling";
  const canCancel = !isTerminal && !cancellationRequested;
  const totalUnits = current?.totalUnits ?? 0;
  const completedUnits = current?.completedUnits ?? 0;
  const percent =
    totalUnits === 0 ? undefined : Math.min(100, Math.floor((completedUnits * 100) / totalUnits));

  return (
    <div
      ref={rowReference}
      className={`tracked-operation tracked-operation--${state}${isSemantic ? " tracked-operation--semantic" : ""}`}
      tabIndex={focus ? -1 : undefined}
    >
      <div className="tracked-operation__summary">
        <OperationStateIcon state={state} />
        <div className="tracked-operation__identity">
          <div>
            <strong>{isSemantic ? "Semantic refinement" : operation.admission.displayLabel}</strong>
            <span>{isSemantic ? "Auto mode follow-up" : `${operation.admission.mode} index`}</span>
          </div>
          <div className="tracked-operation__identifier">
            <code>{operationId}</code>
            <CopyOperationId operationId={operationId} />
          </div>
        </div>
        <span className={`state-badge state-badge--${state}`}>{humanize(state)}</span>
      </div>

      <div className="tracked-operation__progress">
        {percent === undefined ? (
          <div
            className={
              isTerminal ? "operation-progress is-terminal" : "operation-progress is-active"
            }
            role="progressbar"
            aria-label={`${isSemantic ? "Semantic" : "Structural"} operation progress`}
          >
            <span />
          </div>
        ) : (
          <progress
            className="operation-progress"
            aria-label={`${isSemantic ? "Semantic" : "Structural"} operation progress`}
            max={100}
            value={percent}
          />
        )}
        <div>
          <span>{operationStage(current, state)}</span>
          <span>
            {percent === undefined
              ? "Total work is not available"
              : `${completedUnits.toLocaleString()} / ${totalUnits.toLocaleString()} units`}
          </span>
        </div>
      </div>

      {status.isError && !waitingForSemanticAdmission ? (
        <div className="operation-notice operation-notice--warning" role="status">
          <TriangleAlert size={14} aria-hidden="true" />
          <span>The latest daemon revision is temporarily unavailable.</span>
          <Button size="sm" variant="ghost" onPress={() => void status.refetch()}>
            <RotateCcw size={13} aria-hidden="true" />
            Retry
          </Button>
        </div>
      ) : null}

      {current?.error === null || current?.error === undefined ? null : (
        <div className="operation-notice operation-notice--error" role="alert">
          <CircleX size={14} aria-hidden="true" />
          <span>{current.error.message}</span>
          {current.error.retryable ? <small>Retryable after a new root selection.</small> : null}
        </div>
      )}

      {!isSemantic && operation.admission.diagnostics.length > 0 ? (
        <ul className="operation-diagnostics" aria-label="Index admission diagnostics">
          {operation.admission.diagnostics.map((diagnostic) => (
            <li key={`${diagnostic.code}:${diagnostic.message}`}>{diagnostic.message}</li>
          ))}
        </ul>
      ) : null}

      <div className="tracked-operation__actions">
        {publishedGenerationId === null ? null : (
          <Link
            className="operation-open-link"
            to={`/projects/${encodeURIComponent(operation.admission.repositoryId)}?generation=${encodeURIComponent(publishedGenerationId)}`}
          >
            Open project
            <ExternalLink size={13} aria-hidden="true" />
          </Link>
        )}
        {canCancel ? (
          <Button size="sm" variant="ghost" onPress={() => setConfirmingCancel(true)}>
            <Square size={12} aria-hidden="true" />
            Cancel
          </Button>
        ) : null}
        {!isSemantic && onDismiss !== undefined && isTerminal ? (
          <Button size="sm" variant="ghost" onPress={onDismiss}>
            <X size={13} aria-hidden="true" />
            Dismiss
          </Button>
        ) : null}
        <details className="operation-technical">
          <summary>Technical detail</summary>
          <OperationTechnicalDetail
            current={current}
            operation={operation}
            requestId={isSemantic ? undefined : operation.requestId}
          />
        </details>
      </div>

      <CancelOperationDialog
        error={cancel.isError}
        isOpen={confirmingCancel}
        label={isSemantic ? "semantic refinement" : operation.admission.displayLabel}
        operationId={operationId}
        submitting={cancel.isPending}
        onCancel={() => setConfirmingCancel(false)}
        onConfirm={() => {
          cancel.mutate(undefined, {
            onSuccess: () => setConfirmingCancel(false),
          });
        }}
      />
    </div>
  );
}

function OperationTechnicalDetail({
  current,
  operation,
  requestId,
}: {
  current: RepositoryOperation | undefined;
  operation: SessionOperation;
  requestId?: string;
}) {
  const admission = operation.admission;
  return (
    <dl>
      <div>
        <dt>Revision</dt>
        <dd>{current?.revision ?? admission.revision}</dd>
      </div>
      {requestId === undefined ? null : (
        <div>
          <dt>Request ID</dt>
          <dd>
            <code>{requestId}</code>
          </dd>
        </div>
      )}
      <div>
        <dt>Repository</dt>
        <dd>
          <code>{admission.repositoryId}</code>
        </dd>
      </div>
      <div>
        <dt>Recovery</dt>
        <dd>{humanize(current?.recoveryClass ?? "not_applicable")}</dd>
      </div>
      <div>
        <dt>Files examined</dt>
        <dd>{formatCount(current?.filesExamined ?? admission.discoveredInputs)}</dd>
      </div>
      <div>
        <dt>Bytes examined</dt>
        <dd>{formatBytes(current?.bytesExamined ?? "0")}</dd>
      </div>
      <div>
        <dt>Peak RSS</dt>
        <dd>{formatBytes(current?.peakRssBytes ?? "0")}</dd>
      </div>
      <div>
        <dt>Written</dt>
        <dd>{formatBytes(current?.writtenBytes ?? admission.estimatedDiskBytes)}</dd>
      </div>
      <div>
        <dt>Detached</dt>
        <dd>{current?.detached === false ? "no" : "yes"}</dd>
      </div>
    </dl>
  );
}

function CancelOperationDialog({
  error,
  isOpen,
  label,
  operationId,
  submitting,
  onCancel,
  onConfirm,
}: {
  error: boolean;
  isOpen: boolean;
  label: string;
  operationId: string;
  submitting: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const headingId = useId();
  return (
    <NativeDialog
      ariaLabelledBy={headingId}
      className="cancel-operation-modal"
      isDismissable={!submitting}
      isOpen={isOpen}
      onDismiss={onCancel}
    >
      <header data-slot="modal-header">
        <h2 id={headingId} data-slot="modal-heading">
          Cancel index operation?
        </h2>
      </header>
      <div data-slot="modal-body">
        <p>
          Rootlight will request cancellation for <strong>{label}</strong>. The daemon may still
          publish a generation if the operation finishes first.
        </p>
        <code>{operationId}</code>
        {error ? (
          <div className="operation-notice operation-notice--error" role="alert">
            The cancellation request failed. The operation remains unchanged.
          </div>
        ) : null}
      </div>
      <footer data-slot="modal-footer">
        <Button isDisabled={submitting} variant="ghost" onPress={onCancel}>
          Keep running
        </Button>
        <Button isDisabled={submitting} variant="primary" onPress={onConfirm}>
          {submitting ? "Requesting cancellation" : "Request cancellation"}
        </Button>
      </footer>
    </NativeDialog>
  );
}

function OperationStateIcon({ state }: { state: OperationState }) {
  if (state === "succeeded") {
    return <CircleCheck className="operation-state-icon is-success" size={18} aria-hidden="true" />;
  }
  if (terminalStates.has(state)) {
    return <CircleX className="operation-state-icon is-error" size={18} aria-hidden="true" />;
  }
  if (state === "unknown") {
    return <Activity className="operation-state-icon" size={18} aria-hidden="true" />;
  }
  return <LoaderCircle className="operation-state-icon spin" size={18} aria-hidden="true" />;
}

function CopyOperationId({ operationId }: { operationId: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      aria-label={copied ? "Operation ID copied" : "Copy operation ID"}
      onClick={() => {
        try {
          void navigator.clipboard.writeText(operationId).then(
            () => setCopied(true),
            () => setCopied(false),
          );
        } catch {
          setCopied(false);
        }
      }}
    >
      {copied ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
    </button>
  );
}

function operationStage(current: RepositoryOperation | undefined, state: OperationState) {
  if (current?.indexStage !== undefined && current.indexStage.length > 0) {
    return humanize(current.indexStage);
  }
  if (current?.stage !== undefined && current.stage !== "unknown") {
    return humanize(current.stage);
  }
  return humanize(state);
}

function formatCount(value: string) {
  return BigInt(value).toLocaleString();
}

function formatBytes(value: string) {
  const bytes = BigInt(value);
  if (bytes === 0n) {
    return "0 B";
  }
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let scaled = bytes;
  let unit = 0;
  while (scaled >= 1024n && unit < units.length - 1) {
    scaled /= 1024n;
    unit += 1;
  }
  return `${scaled.toLocaleString()} ${units[unit] ?? "B"}`;
}

function humanize(value: string) {
  return value.replaceAll("_", " ");
}
