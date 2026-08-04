// Verifies catalog rendering, filtering, paging, and immutable project navigation.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { deleteProject, fetchProjects, renameProject } from "../src/api/client";
import type { ProjectCatalogPage, ProjectSummary } from "../src/api/contracts";
import { OperationProvider } from "../src/operations/operation-provider";
import { createCatalogLocationState } from "../src/routing/catalog-location-state";
import { ProjectsPage } from "../src/views/projects-page";

vi.mock("../src/api/client", () => ({
  createClientRequestId: vi.fn(() => "idx_test-request"),
  deleteProject: vi.fn(),
  fetchProjects: vi.fn(),
  renameProject: vi.fn(),
  submitProjectIndex: vi.fn(),
}));

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const project: ProjectSummary = {
  repositoryId,
  activeGenerationId: generationId,
  displayName: "Rootlight",
  alias: null,
  rootPath: "C:\\work\\rootlight",
  generationCount: "3",
  lifecycleState: "ready",
  languages: ["rust"],
  structuralFreshness: "current",
  semanticFreshness: "stale",
  coverage: [
    {
      language: "rust",
      tier: "tier_b",
      status: "bounded",
      discoveredFiles: "10",
      indexedFiles: "9",
    },
  ],
};
const catalog = catalogPage([project]);

beforeEach(() => {
  vi.mocked(fetchProjects).mockReset();
  vi.mocked(fetchProjects).mockResolvedValue(catalog);
  vi.mocked(renameProject).mockReset();
  vi.mocked(renameProject).mockResolvedValue({
    schema: "rootlight.web-project-rename/1",
    alias: "Renamed project",
  });
  vi.mocked(deleteProject).mockReset();
  vi.mocked(deleteProject).mockResolvedValue();
});

describe("ProjectsPage", () => {
  it("renders authoritative counts and generation-bound navigation", async () => {
    renderPage();

    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    expect(screen.getByText("90% indexed coverage")).toBeVisible();
    expect(screen.getByText(`Active gen1_${"b".repeat(8)}…bbbb`)).toBeVisible();
    expect(screen.getByRole("link", { name: "Open project" })).toHaveAttribute(
      "href",
      `/projects/${repositoryId}?generation=${generationId}`,
    );
    expect(screen.getByText("C:\\work\\rootlight")).toBeVisible();
    expect(screen.getAllByText("Current page only")[0]?.previousSibling).toHaveTextContent("1");
  });

  it("renders loading, connection error, retry, and refresh states", async () => {
    vi.mocked(fetchProjects).mockRejectedValueOnce(new Error("offline")).mockResolvedValue(catalog);
    renderPage();

    expect(screen.getByLabelText("Loading project catalog")).toHaveAttribute("aria-busy", "true");
    expect(await screen.findByText("Catalog unavailable")).toBeVisible();

    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();

    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(fetchProjects).toHaveBeenCalledTimes(3));
  });

  it("keeps the last immutable page visible when a background refresh fails", async () => {
    vi.mocked(fetchProjects)
      .mockResolvedValueOnce(catalog)
      .mockRejectedValueOnce(new Error("offline"));
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(
      await screen.findByText(
        "Refresh failed. The previous immutable catalog page remains visible.",
      ),
    ).toBeVisible();
    expect(screen.getByRole("heading", { name: "Rootlight" })).toBeVisible();
  });

  it("renders the initial empty state when the exact catalog total is unavailable", async () => {
    vi.mocked(fetchProjects).mockResolvedValue(catalogPage([], { totalCount: null }));
    renderPage();

    expect(await screen.findByText("No projects have been loaded yet")).toBeVisible();
    expect(screen.getByText("Exact total is unavailable")).toBeVisible();
    expect(screen.getByRole("button", { name: "Add your first project" })).toBeEnabled();
  });

  it("sends closed lifecycle and debounced search filters, then clears them", async () => {
    vi.mocked(fetchProjects)
      .mockResolvedValueOnce(catalog)
      .mockResolvedValueOnce(catalogPage([]))
      .mockResolvedValueOnce(catalogPage([]))
      .mockResolvedValue(catalog);
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.selectOptions(screen.getByRole("combobox", { name: "State" }), "degraded");
    expect(await screen.findByText("No projects match these filters")).toBeVisible();
    expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
      expect.objectContaining({ states: ["degraded"] }),
      expect.any(AbortSignal),
    );

    await userEvent.type(screen.getByRole("searchbox", { name: "Search projects" }), "  local  ");
    await waitFor(
      () => {
        expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
          expect.objectContaining({ query: "local", states: ["degraded"] }),
          expect.any(AbortSignal),
        );
      },
      { timeout: 1_000 },
    );

    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    await waitFor(() => {
      expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
        expect.objectContaining({ query: undefined, states: undefined }),
        expect.any(AbortSignal),
      );
    });
  });

  it("applies search immediately on Enter and clears it on Escape", async () => {
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });
    const search = screen.getByRole("searchbox", { name: "Search projects" });

    fireEvent.change(search, { target: { value: "  immediate  " } });
    fireEvent.keyDown(search, { key: "Enter" });

    await waitFor(() => {
      expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
        expect.objectContaining({ query: "immediate" }),
        expect.any(AbortSignal),
      );
    });

    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveValue("");
    await waitFor(() => {
      expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
        expect.objectContaining({ query: undefined }),
        expect.any(AbortSignal),
      );
    });
  });

  it("restores bounded catalog filters and the immutable cursor without resetting them", async () => {
    const restoredCatalog = catalogPage([project]);
    vi.mocked(fetchProjects).mockResolvedValue(restoredCatalog);
    renderPage(
      createCatalogLocationState({
        searchInput: "root",
        query: "root",
        stateFilter: "ready",
        history: [{ snapshot: "snapshot", after: "cursor_2", sortVersion: 1 }],
      }),
    );

    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    expect(screen.getByRole("searchbox", { name: "Search projects" })).toHaveValue("root");
    expect(screen.getByRole("combobox", { name: "State" })).toHaveValue("ready");
    expect(screen.getByText("Page 2")).toBeVisible();
    expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
      expect.objectContaining({
        query: "root",
        states: ["ready"],
        snapshot: "snapshot",
        after: "cursor_2",
        sortVersion: 1,
      }),
      expect.any(AbortSignal),
    );

    await new Promise((resolve) => window.setTimeout(resolve, 300));
    expect(screen.getByText("Page 2")).toBeVisible();
    expect(vi.mocked(fetchProjects)).toHaveBeenCalledTimes(1);
  });

  it("keeps the internal repository identity out of the visible project card", async () => {
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    expect(screen.queryByText(repositoryId)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /repository ID/iu })).not.toBeInTheDocument();
    expect(screen.getByPlaceholderText("Search projects or paths")).toBeVisible();
  });

  it("renames the effective project name from its clickable heading", async () => {
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.click(screen.getByRole("button", { name: "Rename Rootlight" }));
    const name = screen.getByRole("textbox", { name: "Project name" });
    await userEvent.clear(name);
    await userEvent.type(name, "Renamed project");
    await userEvent.click(screen.getByRole("button", { name: "Save name" }));

    await waitFor(() =>
      expect(renameProject).toHaveBeenCalledWith(repositoryId, "Renamed project"),
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("requires confirmation before deleting only Rootlight-owned project data", async () => {
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(
      screen.getByText(/The source directory at C:\\work\\rootlight will not be changed/u),
    ).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Delete Rootlight data" }));

    await waitFor(() => expect(deleteProject).toHaveBeenCalledWith(repositoryId));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("keeps next and previous pages on the same immutable snapshot", async () => {
    const firstPage = catalogPage([project], { nextAfter: "cursor_2" });
    const secondProject = {
      ...project,
      repositoryId: `repo1_${"c".repeat(32)}`,
      displayName: "Second repository",
    };
    vi.mocked(fetchProjects)
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(catalogPage([secondProject]))
      .mockResolvedValue(firstPage);
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByRole("heading", { name: "Second repository" })).toBeVisible();
    expect(screen.getByText("Page 2")).toBeVisible();
    expect(vi.mocked(fetchProjects)).toHaveBeenLastCalledWith(
      expect.objectContaining({
        after: "cursor_2",
        snapshot: "snapshot",
        sortVersion: 1,
      }),
      expect.any(AbortSignal),
    );

    await userEvent.click(screen.getByRole("button", { name: "Previous" }));
    expect(await screen.findByRole("heading", { name: "Rootlight" })).toBeVisible();
    expect(screen.getByText("Page 1")).toBeVisible();
  });

  it("does not present rows from the previous query as a new filtered page", async () => {
    let resolveFiltered: ((page: ProjectCatalogPage) => void) | undefined;
    vi.mocked(fetchProjects)
      .mockResolvedValueOnce(catalog)
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFiltered = resolve;
          }),
      );
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.selectOptions(screen.getByRole("combobox", { name: "State" }), "degraded");

    expect(screen.getByLabelText("Loading project catalog")).toBeVisible();
    expect(screen.queryByRole("heading", { name: "Rootlight" })).not.toBeInTheDocument();

    resolveFiltered?.(catalogPage([]));
    expect(await screen.findByText("No projects match these filters")).toBeVisible();
  });

  it("does not advance the page label until the next snapshot page arrives", async () => {
    let resolveNext: ((page: ProjectCatalogPage) => void) | undefined;
    vi.mocked(fetchProjects)
      .mockResolvedValueOnce(catalogPage([project], { nextAfter: "cursor_2" }))
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveNext = resolve;
          }),
      );
    renderPage();
    await screen.findByRole("heading", { name: "Rootlight" });

    await userEvent.click(screen.getByRole("button", { name: "Next" }));

    expect(screen.getByLabelText("Loading project catalog")).toBeVisible();
    expect(screen.queryByText("Page 2")).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Rootlight" })).not.toBeInTheDocument();

    resolveNext?.(catalogPage([project]));
    expect(await screen.findByText("Page 2")).toBeVisible();
  });

  it("reports all lifecycle groups and disables projects without a published generation", async () => {
    const indexingProject: ProjectSummary = {
      ...project,
      repositoryId: `repo1_${"d".repeat(32)}`,
      displayName: "Indexing repository",
      lifecycleState: "indexing",
    };
    const unavailableProject: ProjectSummary = {
      ...project,
      repositoryId: `repo1_${"e".repeat(32)}`,
      activeGenerationId: null,
      displayName: "Unavailable repository",
      lifecycleState: "unknown",
      languages: [],
      coverage: [],
    };
    vi.mocked(fetchProjects).mockResolvedValue(
      catalogPage([project, indexingProject, unavailableProject]),
    );
    renderPage();

    expect(await screen.findByText("Unavailable repository")).toBeVisible();
    expect(screen.getByText("Coverage not reported")).toBeVisible();
    expect(screen.getByText("No language data")).toBeVisible();
    expect(screen.getByRole("article", { name: "Unavailable repository" })).toHaveClass(
      "project-card--disabled",
    );
    expect(screen.getByText("unknown")).toHaveClass("state-badge--unknown");
    expect(screen.getAllByText("Current page only")[1]?.previousSibling).toHaveTextContent("1");
    expect(screen.getAllByText("Current page only")[2]?.previousSibling).toHaveTextContent("1");
  });

  it("caps a malformed over-complete coverage indicator without hiding the source ratio", async () => {
    const overCompleteProject: ProjectSummary = {
      ...project,
      coverage: [
        {
          language: "rust",
          tier: "tier_b",
          status: "bounded",
          discoveredFiles: "1",
          indexedFiles: "2",
        },
      ],
    };
    vi.mocked(fetchProjects).mockResolvedValue(catalogPage([overCompleteProject]));
    renderPage();

    expect(await screen.findByText("200% indexed coverage")).toBeVisible();
    expect(screen.getByRole("progressbar", { name: "200% indexed coverage" })).toHaveAttribute(
      "value",
      "100",
    );
  });
});

function catalogPage(
  projects: ProjectSummary[],
  overrides: Partial<ProjectCatalogPage> = {},
): ProjectCatalogPage {
  return {
    schema: "rootlight.web-project-catalog-page/1",
    projects,
    snapshot: "snapshot",
    nextAfter: null,
    totalCount: String(projects.length),
    truncated: false,
    sortVersion: 1,
    ...overrides,
  };
}

function renderPage(locationState?: unknown) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OperationProvider>
        <MemoryRouter initialEntries={[{ pathname: "/projects", state: locationState }]}>
          <ProjectsPage />
        </MemoryRouter>
      </OperationProvider>
    </QueryClientProvider>,
  );
}
