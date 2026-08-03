// Verifies capability-based project selection without exposing local paths as navigation state.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  browseFilesystem,
  fetchFilesystemRoots,
  openFilesystemPath,
  preflightFilesystemIndex,
} from "../src/api/client";
import { AddProjectDialog } from "../src/components/add-project-dialog";

vi.mock("../src/api/client", () => ({
  browseFilesystem: vi.fn(),
  fetchFilesystemRoots: vi.fn(),
  openFilesystemPath: vi.fn(),
  preflightFilesystemIndex: vi.fn(),
}));

const rootToken = "a".repeat(43);
const childToken = "b".repeat(43);
const capability = "c".repeat(43);

beforeEach(() => {
  vi.mocked(fetchFilesystemRoots).mockReset();
  vi.mocked(browseFilesystem).mockReset();
  vi.mocked(openFilesystemPath).mockReset();
  vi.mocked(preflightFilesystemIndex).mockReset();
  vi.mocked(fetchFilesystemRoots).mockResolvedValue({
    schema: "rootlight.web-filesystem-roots/1",
    roots: [{ label: "Home", browseToken: rootToken, readable: true, selectable: true }],
  });
  vi.mocked(browseFilesystem).mockImplementation((request) =>
    Promise.resolve(request.action.type === "child" ? childPage() : rootPage()),
  );
  vi.mocked(preflightFilesystemIndex).mockResolvedValue({
    schema: "rootlight.web-index-preflight/1",
    selectable: true,
    normalizedDisplayLabel: "crates",
    daemonAcceptingOperations: true,
    selectedMode: "deep",
    supportedModes: ["auto", "structural", "deep"],
    adapterIsolation: "available",
    estimatedLimitations: [],
    warnings: ["repository_contents_not_scanned"],
    rootCapability: capability,
    rootCapabilityExpiresInSeconds: 120,
  });
});

describe("AddProjectDialog", () => {
  it("browses with opaque capabilities, supports keyboard rows, and submits preflight", async () => {
    const onSubmit = vi.fn().mockResolvedValue(undefined);
    const onOpenChange = vi.fn();
    renderDialog(onSubmit, onOpenChange);

    expect(await screen.findByRole("heading", { name: "Add a project" })).toBeVisible();
    expect(screen.getByText(/Source is not uploaded/u)).toBeVisible();
    await userEvent.click(await screen.findByRole("button", { name: "Home" }));

    const crates = await screen.findByRole("button", { name: "crates" });
    const docs = screen.getByRole("button", { name: "docs" });
    crates.focus();
    fireEvent.keyDown(crates, { key: "PageDown" });
    expect(crates).toHaveFocus();
    fireEvent.keyDown(crates, { key: "ArrowDown" });
    expect(docs).toHaveFocus();
    fireEvent.keyDown(docs, { key: "ArrowUp" });
    expect(crates).toHaveFocus();
    fireEvent.keyDown(crates, { key: "ArrowUp" });
    expect(crates).toHaveFocus();

    await userEvent.click(crates);
    expect(await screen.findByText("src")).toBeVisible();
    await userEvent.click(screen.getByRole("radio", { name: /Deep/u }));
    await userEvent.click(screen.getByRole("button", { name: "Select this folder" }));

    expect(await screen.findByRole("heading", { name: "Ready to index" })).toBeVisible();
    expect(screen.getByText("repository contents not scanned")).toBeVisible();
    expect(preflightFilesystemIndex).toHaveBeenCalledWith(childToken, "deep");

    await userEvent.click(screen.getByRole("button", { name: "Start detached index" }));
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith({
        rootCapability: capability,
        mode: "deep",
        displayLabel: "crates",
      }),
    );
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("opens a direct absolute path through the server and reports safe failures", async () => {
    vi.mocked(openFilesystemPath)
      .mockRejectedValueOnce(new Error("unavailable"))
      .mockResolvedValueOnce({
        schema: "rootlight.web-filesystem-open-path/1",
        label: "rootlight",
        browseToken: rootToken,
      });
    renderDialog(vi.fn(), vi.fn());
    const path = await screen.findByLabelText("Direct absolute path");

    await userEvent.type(path, "C:\\work\\rootlight");
    await userEvent.click(screen.getByRole("button", { name: "Open path" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The local folder request could not be completed",
    );

    await userEvent.click(screen.getByRole("button", { name: "Open path" }));
    expect(await screen.findByRole("button", { name: "crates" })).toBeVisible();
    expect(openFilesystemPath).toHaveBeenLastCalledWith("C:\\work\\rootlight");
  });

  it("applies a bounded filter, follows breadcrumbs, and appends cursor pages", async () => {
    const cursor = "z".repeat(43);
    vi.mocked(browseFilesystem)
      .mockResolvedValueOnce({ ...rootPage(), nextCursor: cursor })
      .mockResolvedValueOnce({
        ...rootPage(),
        directories: [{ name: "examples", kind: "directory", readable: true, selectable: true }],
        nextCursor: null,
      })
      .mockResolvedValueOnce(childPage())
      .mockResolvedValueOnce(rootPage())
      .mockResolvedValueOnce(rootPage());
    renderDialog(vi.fn(), vi.fn());
    await userEvent.click(await screen.findByRole("button", { name: "Home" }));

    await userEvent.click(screen.getByRole("button", { name: "Load more folders" }));
    expect(await screen.findByRole("button", { name: "examples" })).toBeVisible();
    expect(browseFilesystem).toHaveBeenNthCalledWith(2, {
      browseToken: rootToken,
      action: { type: "current" },
      pageSize: 64,
      cursor,
      filter: undefined,
    });

    await userEvent.click(screen.getByRole("button", { name: "crates" }));
    const breadcrumbs = await screen.findByRole("navigation", {
      name: "Selected folder breadcrumbs",
    });
    await userEvent.click(within(breadcrumbs).getByRole("button", { name: "Home" }));
    const filter = await screen.findByRole("textbox", { name: "Filter directories" });
    await userEvent.type(filter, "docs{Enter}");
    expect(browseFilesystem).toHaveBeenLastCalledWith({
      browseToken: rootToken,
      action: { type: "current" },
      pageSize: 64,
      cursor: undefined,
      filter: "docs",
    });
  });

  it("keeps an admission failure in review and lets the user change selection", async () => {
    const onSubmit = vi.fn().mockRejectedValue(new Error("busy"));
    renderDialog(onSubmit, vi.fn());
    await userEvent.click(await screen.findByRole("button", { name: "Home" }));
    await userEvent.click(screen.getByRole("radio", { name: /Deep/u }));
    await userEvent.click(screen.getByRole("button", { name: "Select this folder" }));
    await userEvent.click(await screen.findByRole("button", { name: "Start detached index" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Rootlight could not admit this index operation",
    );
    await userEvent.click(screen.getByRole("button", { name: "Change selection" }));
    expect(screen.getByRole("button", { name: "Select this folder" })).toBeEnabled();
  });

  it("renders an explicit empty directory state", async () => {
    vi.mocked(browseFilesystem).mockResolvedValue({
      ...rootPage(),
      directories: [],
    });
    renderDialog(vi.fn(), vi.fn());
    await userEvent.click(await screen.findByRole("button", { name: "Home" }));
    expect(await screen.findByText("This folder has no visible child directories.")).toBeVisible();
  });

  it("keeps unavailable roots inert and closes without issuing a blank direct-path request", async () => {
    vi.mocked(fetchFilesystemRoots).mockResolvedValue({
      schema: "rootlight.web-filesystem-roots/1",
      roots: [{ label: "Unavailable", browseToken: rootToken, readable: false, selectable: false }],
    });
    const onOpenChange = vi.fn();
    renderDialog(vi.fn(), onOpenChange);

    const root = await screen.findByRole("button", { name: "Unavailable" });
    expect(root).toBeDisabled();
    fireEvent.keyDown(screen.getByLabelText("Direct absolute path"), { key: "Enter" });
    expect(openFilesystemPath).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Close add project dialog" }));
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("reports browse failures without exposing the rejected error", async () => {
    vi.mocked(browseFilesystem).mockRejectedValue(new Error("C:\\private\\repository"));
    renderDialog(vi.fn(), vi.fn());

    await userEvent.click(await screen.findByRole("button", { name: "Home" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The local folder request could not be completed",
    );
    expect(screen.queryByText(/private.*repository/u)).not.toBeInTheDocument();
  });

  it("renders a non-admissible preflight and keeps unsafe directory rows disabled", async () => {
    vi.mocked(browseFilesystem).mockResolvedValue({
      ...rootPage(),
      directories: [
        { name: "unreadable", kind: "directory", readable: false, selectable: true },
        { name: "unselectable", kind: "directory", readable: true, selectable: false },
      ],
    });
    vi.mocked(preflightFilesystemIndex).mockResolvedValue({
      schema: "rootlight.web-index-preflight/1",
      selectable: false,
      normalizedDisplayLabel: "blocked",
      daemonAcceptingOperations: false,
      selectedMode: "auto",
      supportedModes: ["auto"],
      adapterIsolation: "unavailable",
      estimatedLimitations: ["adapter_unavailable"],
      warnings: [],
      rootCapability: capability,
      rootCapabilityExpiresInSeconds: 30,
    });
    const onSubmit = vi.fn().mockResolvedValue(undefined);
    renderDialog(onSubmit, vi.fn());

    await userEvent.click(await screen.findByRole("button", { name: "Home" }));
    expect(await screen.findByRole("button", { name: "unreadable" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "unselectable" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Select this folder" }));

    expect(
      await screen.findByRole("heading", { name: "Index admission unavailable" }),
    ).toBeVisible();
    expect(screen.getByText("paused")).toBeVisible();
    expect(screen.queryByRole("list")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start detached index" })).toBeDisabled();
    expect(onSubmit).not.toHaveBeenCalled();
  });
});

function renderDialog(onSubmit: () => Promise<void>, onOpenChange: (open: boolean) => void) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <AddProjectDialog isOpen onOpenChange={onOpenChange} onSubmit={onSubmit} />
    </QueryClientProvider>,
  );
}

function rootPage() {
  return {
    schema: "rootlight.web-filesystem-browse/1" as const,
    browseToken: rootToken,
    label: "Home",
    depth: 0,
    maximumDepth: 32,
    breadcrumbs: [{ label: "Home", browseToken: rootToken }],
    directories: [
      { name: "crates", kind: "directory" as const, readable: true, selectable: true },
      { name: "docs", kind: "directory" as const, readable: true, selectable: true },
    ],
    nextCursor: null,
  };
}

function childPage() {
  return {
    schema: "rootlight.web-filesystem-browse/1" as const,
    browseToken: childToken,
    label: "crates",
    depth: 1,
    maximumDepth: 32,
    breadcrumbs: [
      { label: "Home", browseToken: rootToken },
      { label: "crates", browseToken: childToken },
    ],
    directories: [{ name: "src", kind: "directory" as const, readable: true, selectable: true }],
    nextCursor: null,
  };
}
