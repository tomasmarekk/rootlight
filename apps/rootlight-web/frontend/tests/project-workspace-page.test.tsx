// Verifies exact-generation workspace integration and safe browser-history state.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError, fetchProjectDetail } from "../src/api/client";
import type { ProjectDetail } from "../src/api/contracts";
import { useGraphProjection } from "../src/hooks/use-graph-projection";
import { ProjectWorkspacePage } from "../src/views/project-workspace-page";
import { graphModelFixture } from "./graph-engine-fixtures";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const symbolId = `sym1_${"c".repeat(39)}`;

vi.mock("../src/api/client", () => ({
  ApiError: class ApiError extends Error {
    readonly status: number;
    readonly code: string;

    constructor(status: number, code: string) {
      super(code);
      this.status = status;
      this.code = code;
    }
  },
  fetchProjectDetail: vi.fn(),
}));

vi.mock("../src/hooks/use-graph-projection", () => ({
  useGraphProjection: vi.fn(),
}));

vi.mock("../src/features/graph/components/graph-viewport", () => ({
  GraphViewport: (props: {
    onSelectionChange?: (ordinals: readonly number[]) => void;
    onLabelsVisibleChange?: (visible: boolean) => void;
  }) => (
    <div aria-label="Mock graph viewport">
      <button type="button" onClick={() => props.onSelectionChange?.([1])}>
        Select symbol
      </button>
      <button type="button" onClick={() => props.onLabelsVisibleChange?.(false)}>
        Hide labels
      </button>
    </div>
  ),
}));

const detail: ProjectDetail = {
  schema: "rootlight.web-project-detail/1",
  repositoryId,
  displayName: "Rootlight",
  alias: "local",
  resolvedGenerationId: generationId,
  activeGenerationId: generationId,
  parentGenerationId: null,
  activeParentGenerationId: null,
  activeStructuralFreshness: "current",
  activeSemanticFreshness: "current",
  structuralFreshness: "current",
  semanticFreshness: "current",
  lifecycleState: "ready",
  publicationState: "published",
  coverage: [
    {
      language: "rust",
      tier: "tier_a",
      status: "complete",
      discoveredFiles: "10",
      indexedFiles: "10",
    },
  ],
  operations: [],
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(fetchProjectDetail).mockResolvedValue(detail);
  const fixture = graphModelFixture();
  vi.mocked(useGraphProjection).mockReturnValue({
    model: {
      ...fixture,
      nodes: fixture.nodes.map((node, index) => ({
        ...node,
        stableId: index === 1 ? symbolId : `file1_${"d".repeat(39)}`,
      })),
    },
    loading: false,
    loadingNextPage: false,
    failed: false,
  });
});

describe("ProjectWorkspacePage", () => {
  it("pins graph requests to the resolved generation and keeps safe state in history", async () => {
    renderWorkspace();

    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    expect(fetchProjectDetail).toHaveBeenCalledWith(
      repositoryId,
      "active",
      expect.any(AbortSignal),
    );
    expect(useGraphProjection).toHaveBeenCalledWith(
      expect.objectContaining({
        repositoryId,
        generationId,
        view: "architecture",
        selectedSymbolId: undefined,
      }),
    );
    expect(screen.getByRole("radio", { name: "symbols" })).toBeDisabled();

    await userEvent.click(screen.getByRole("button", { name: "Select symbol" }));
    expect(screen.getByRole("radio", { name: "symbols" })).toBeEnabled();
    await userEvent.click(screen.getByRole("radio", { name: "symbols" }));
    await userEvent.click(screen.getByRole("button", { name: "Hide labels" }));

    await waitFor(() => {
      const search = screen.getByTestId("workspace-location").textContent;
      expect(search).toContain(`selected=${symbolId}`);
      expect(search).toContain("view=symbols");
      expect(search).toContain("labels=false");
      expect(search).not.toContain("projection");
      expect(search).not.toContain("source");
    });
    expect(useGraphProjection).toHaveBeenLastCalledWith(
      expect.objectContaining({
        generationId,
        view: "symbols",
        selectedSymbolId: symbolId,
      }),
    );
  });

  it("normalizes unsafe route state and preserves generation when resetting the view", async () => {
    renderWorkspace("?generation=active%2F..%2Fsecret&selected=C%3A%5Cprivate.rs&view=symbols");

    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    await waitFor(() => {
      const search = screen.getByTestId("workspace-location").textContent;
      expect(search).toContain("generation=active");
      expect(search).toContain("view=architecture");
      expect(search).not.toContain("private");
      expect(search).not.toContain("secret");
    });

    await userEvent.click(screen.getByRole("button", { name: "Reset view" }));
    expect(screen.getByRole("combobox", { name: "Generation" })).toHaveValue("active");
  });

  it("keeps project identity available across graph loading and retry states", async () => {
    vi.mocked(useGraphProjection).mockReturnValue({
      model: null,
      loading: true,
      loadingNextPage: false,
      failed: false,
    });
    renderWorkspace();

    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    expect(screen.getByText("Preparing bounded architecture projection")).toBeVisible();
  });

  it("increments the bounded retry identity after a graph failure", async () => {
    vi.mocked(useGraphProjection).mockReturnValue({
      model: null,
      loading: false,
      loadingNextPage: false,
      failed: true,
    });
    renderWorkspace();

    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Retry projection" }));
    expect(useGraphProjection).toHaveBeenLastCalledWith(expect.objectContaining({ retryKey: 1 }));
  });

  it("renders bounded loading and invalid-project error states", async () => {
    vi.mocked(fetchProjectDetail).mockReturnValueOnce(new Promise(() => undefined));
    const loading = renderWorkspace();
    expect(screen.getByLabelText("Loading project generation")).toBeVisible();
    loading.unmount();

    vi.mocked(fetchProjectDetail).mockRejectedValueOnce(new ApiError(400, "invalid_repository_id"));
    renderWorkspace();
    expect(
      await screen.findByRole("heading", { name: "Project identifier is invalid" }),
    ).toBeVisible();
    expect(screen.getByRole("link", { name: "Return to projects" })).toBeVisible();
  });

  it("shows historical context, an explicit pinned option, and reported operations", async () => {
    vi.mocked(fetchProjectDetail).mockResolvedValueOnce({
      ...detail,
      alias: null,
      activeGenerationId: `gen1_${"e".repeat(39)}`,
      operations: [
        {
          operationId: "op_test",
          kind: "repository_index",
          state: "running",
          completedUnits: 2,
          totalUnits: 5,
          ownedByClient: true,
          startedUnixMs: "1",
        },
      ],
    });
    renderWorkspace(`?generation=${generationId}`);

    expect(
      await screen.findByText("Historical projection pinned independently of the active pointer."),
    ).toBeVisible();
    expect(screen.getByRole("option", { name: /Pinned/u })).toBeVisible();
    expect(screen.getByText("repository index")).toBeVisible();
    expect(screen.getByText("2 / 5 units")).toBeVisible();
  });
});

function renderWorkspace(search = "") {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[`/projects/${repositoryId}${search}`]}>
        <Routes>
          <Route
            path="/projects/:repositoryId"
            element={
              <>
                <ProjectWorkspacePage />
                <LocationProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function LocationProbe() {
  return <output data-testid="workspace-location">{useLocation().search}</output>;
}
