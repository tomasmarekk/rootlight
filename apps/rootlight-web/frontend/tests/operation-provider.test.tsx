// Verifies bounded in-memory registration and monotonic authoritative revisions.

import { act, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ProjectIndexAdmission, RepositoryOperation } from "../src/api/contracts";
import { useOperations, type OperationContextValue } from "../src/operations/operation-context";
import { OperationProvider } from "../src/operations/operation-provider";

const firstOperationId = `op1_${"a".repeat(32)}`;
const secondOperationId = `op1_${"b".repeat(32)}`;
const semanticOperationId = `op1_${"c".repeat(32)}`;

describe("OperationProvider", () => {
  it("deduplicates registration, ignores stale revisions, and updates semantic children", () => {
    let store: OperationContextValue | undefined;
    render(
      <OperationProvider>
        <StoreProbe capture={(value) => (store = value)} />
      </OperationProvider>,
    );
    const current = () => {
      if (store === undefined) {
        throw new Error("operation store was not captured");
      }
      return store;
    };

    act(() => current().register(admission(firstOperationId, semanticOperationId), "request-1"));
    act(() => current().register(admission(secondOperationId, null), "request-2"));
    act(() => current().register(admission(firstOperationId, semanticOperationId), "request-3"));
    expect(current().operations).toHaveLength(2);
    const first = () =>
      current().operations.find((entry) => entry.admission.operationId === firstOperationId);
    expect(first()?.requestId).toBe("request-3");

    act(() => current().update(firstOperationId, operation(firstOperationId, "2", "running")));
    act(() => current().update(firstOperationId, operation(firstOperationId, "1", "queued")));
    expect(first()?.status).toMatchObject({ revision: "2", state: "running" });

    act(() => current().update(firstOperationId, operation(firstOperationId, "2", "failed")));
    expect(first()?.status?.state).toBe("failed");
    act(() => current().update(firstOperationId, operation(firstOperationId, "2", "failed")));

    act(() =>
      current().update(semanticOperationId, operation(semanticOperationId, "1", "succeeded")),
    );
    expect(first()?.semanticStatus?.state).toBe("succeeded");
    act(() =>
      current().update(`op1_${"d".repeat(32)}`, operation(`op1_${"d".repeat(32)}`, "1", "running")),
    );

    act(() => current().dismiss(firstOperationId));
    expect(current().operations.map((entry) => entry.admission.operationId)).toEqual([
      secondOperationId,
    ]);
  });
});

function StoreProbe({ capture }: { capture: (value: OperationContextValue) => void }) {
  capture(useOperations());
  return null;
}

function admission(operationId: string, semantic: string | null): ProjectIndexAdmission {
  return {
    schema: "rootlight.web-project-index/1",
    displayLabel: operationId,
    repositoryId: `repo1_${"e".repeat(32)}`,
    operationId,
    semanticOperationId: semantic,
    state: "queued",
    revision: "1",
    mode: "auto",
    parentGenerationId: null,
    publishedGenerationId: null,
    discoveredInputs: "0",
    indexedFiles: "0",
    entities: "0",
    elapsedMicros: "0",
    estimatedDiskBytes: "0",
    diagnostics: [],
  };
}

function operation(
  operationId: string,
  revision: string,
  state: RepositoryOperation["state"],
): RepositoryOperation {
  return {
    schema: "rootlight.web-repository-operation/1",
    displayLabel: operationId,
    mode: "auto",
    ownedBySession: true,
    operationId,
    state,
    revision,
    completedUnits: 0,
    totalUnits: 0,
    kind: "repository_index",
    stage: "executing",
    detached: true,
    cancellationRequested: false,
    recoveryClass: "not_applicable",
    error: null,
    publishedGenerationId: null,
    semanticOperationId: null,
    startedUnixMs: "1",
    peakRssBytes: "0",
    writtenBytes: "0",
    filesExamined: "0",
    bytesExamined: "0",
    indexStage: "",
    retryAfterMs: null,
  };
}
