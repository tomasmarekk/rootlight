// Verifies revision tracking, explicit cancellation, and separate semantic refinement rows.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useEffect } from "react";
import { MemoryRouter } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { cancelIndexOperation, fetchIndexOperation } from "../src/api/client";
import type { ProjectIndexAdmission, RepositoryOperation } from "../src/api/contracts";
import { SessionOperationList } from "../src/components/session-operation-list";
import { useOperations } from "../src/operations/operation-context";
import { OperationProvider } from "../src/operations/operation-provider";

vi.mock("../src/api/client", () => ({
  cancelIndexOperation: vi.fn(),
  fetchIndexOperation: vi.fn(),
}));

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const operationId = `op1_${"c".repeat(32)}`;
const semanticOperationId = `op1_${"d".repeat(32)}`;

beforeEach(() => {
  vi.mocked(cancelIndexOperation).mockReset();
  vi.mocked(fetchIndexOperation).mockReset();
});

describe("SessionOperationList", () => {
  it("opens an authoritative published generation and exposes session-only request detail", async () => {
    renderList({
      ...admissionFixture(),
      state: "succeeded",
      publishedGenerationId: generationId,
    });

    expect(await screen.findByText("rootlight")).toBeVisible();
    expect(screen.getByRole("link", { name: "Open project" })).toHaveAttribute(
      "href",
      `/projects/${repositoryId}?generation=${generationId}`,
    );
    await userEvent.click(screen.getByText("Technical detail"));
    expect(screen.getByText("idx_test-request")).toBeVisible();
    expect(fetchIndexOperation).not.toHaveBeenCalled();
  });

  it("requires confirmation and reflects the daemon cancellation revision", async () => {
    vi.mocked(fetchIndexOperation).mockImplementation(
      () => new Promise<RepositoryOperation>(() => undefined),
    );
    vi.mocked(cancelIndexOperation).mockResolvedValue({
      schema: "rootlight.web-operation-cancel/1",
      accepted: true,
      operation: {
        ...operationFixture(operationId),
        state: "cancelling",
        revision: "3",
        cancellationRequested: true,
      },
    });
    renderList(admissionFixture());

    await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    const dialog = screen.getByRole("dialog", { name: "Cancel index operation?" });
    expect(dialog).toBeVisible();
    expect(within(dialog).getByText(operationId)).toBeVisible();
    await userEvent.click(within(dialog).getByRole("button", { name: "Request cancellation" }));

    await waitFor(() => expect(cancelIndexOperation).toHaveBeenCalledWith(operationId));
    expect(await screen.findByText("cancelling")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  });

  it("tracks Auto semantic refinement as an independent child operation", async () => {
    vi.mocked(fetchIndexOperation).mockResolvedValue({
      ...operationFixture(semanticOperationId),
      state: "succeeded",
      revision: "4",
      completedUnits: 4,
      totalUnits: 4,
      publishedGenerationId: generationId,
    });
    renderList({
      ...admissionFixture(),
      state: "succeeded",
      publishedGenerationId: generationId,
      semanticOperationId,
    });

    const semanticTitle = await screen.findByText("Semantic refinement");
    const semanticRow = semanticTitle.closest(".tracked-operation");
    expect(semanticRow).not.toBeNull();
    await waitFor(() =>
      expect(within(semanticRow as HTMLElement).getByText("succeeded")).toBeVisible(),
    );
    expect(fetchIndexOperation).toHaveBeenCalledWith(
      semanticOperationId,
      {},
      expect.any(AbortSignal),
    );
  });

  it("shows determinate failed progress, safe retry guidance, diagnostics, and dismissal", async () => {
    vi.mocked(fetchIndexOperation).mockResolvedValue({
      ...operationFixture(operationId),
      state: "failed",
      revision: "5",
      error: {
        code: 10,
        message: "The isolated adapter did not complete.",
        retryable: true,
        retryAfterMs: "1000",
      },
      peakRssBytes: "2097152",
      writtenBytes: "1024",
      filesExamined: "4",
      bytesExamined: "4096",
    });
    renderList({
      ...admissionFixture(),
      diagnostics: [{ code: "adapter_degraded", message: "Structural fallback was retained." }],
    });

    expect(await screen.findByText("The isolated adapter did not complete.")).toBeVisible();
    expect(screen.getByText("Retryable after a new root selection.")).toBeVisible();
    expect(screen.getByText("Structural fallback was retained.")).toBeVisible();
    expect(
      screen.getByRole("progressbar", { name: "Structural operation progress" }),
    ).toHaveAttribute("value", "50");
    await userEvent.click(screen.getByText("Technical detail"));
    expect(screen.getByText("2 MiB")).toBeVisible();

    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText("Index operations")).not.toBeInTheDocument();
  });

  it("surfaces a polling failure and retries the same correlated operation", async () => {
    vi.mocked(fetchIndexOperation)
      .mockRejectedValueOnce(new Error("offline"))
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue({
        ...operationFixture(operationId),
        state: "succeeded",
        revision: "6",
        completedUnits: 4,
        totalUnits: 4,
        publishedGenerationId: generationId,
      });
    renderList(admissionFixture());

    expect(
      await screen.findByText(
        "The latest daemon revision is temporarily unavailable.",
        {},
        { timeout: 4_000 },
      ),
    ).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("link", { name: "Open project" })).toBeVisible();
  });
});

function renderList(admission: ProjectIndexAdmission) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OperationProvider>
        <MemoryRouter>
          <RegisteredList admission={admission} />
        </MemoryRouter>
      </OperationProvider>
    </QueryClientProvider>,
  );
}

function RegisteredList({ admission }: { admission: ProjectIndexAdmission }) {
  const { register } = useOperations();
  useEffect(() => {
    register(admission, "idx_test-request");
  }, [admission, register]);
  return <SessionOperationList />;
}

function admissionFixture(): ProjectIndexAdmission {
  return {
    schema: "rootlight.web-project-index/1",
    displayLabel: "rootlight",
    repositoryId,
    operationId,
    semanticOperationId: null,
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

function operationFixture(identifier: string): RepositoryOperation {
  return {
    schema: "rootlight.web-repository-operation/1",
    displayLabel: "rootlight",
    mode: "auto",
    ownedBySession: true,
    operationId: identifier,
    state: "running",
    revision: "2",
    completedUnits: 2,
    totalUnits: 4,
    kind: "repository_index",
    stage: "executing",
    detached: true,
    cancellationRequested: false,
    recoveryClass: "not_applicable",
    error: null,
    publishedGenerationId: null,
    semanticOperationId: null,
    startedUnixMs: "1",
    peakRssBytes: "2",
    writtenBytes: "3",
    filesExamined: "4",
    bytesExamined: "5",
    indexStage: "indexing",
    retryAfterMs: 100,
  };
}
