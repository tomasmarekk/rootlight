// Validates the dark authenticated shell and its initial accessibility baseline.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const operationId = `op1_${"c".repeat(32)}`;
const browseToken = "d".repeat(43);
const rootCapability = "e".repeat(43);

const health = {
  webReady: true,
  daemonReady: true,
  protocolVersion: "1.10",
  lifecycle: "ready",
  acceptingOperations: true,
  activeOperations: 0,
  queuedOperations: 0,
  runningOperations: 0,
  journalHealthy: true,
  catalogStatus: "healthy",
  generationStatus: "healthy",
  adapterStatus: "healthy",
  watcherStatus: "not_configured",
  endpointStatus: "healthy",
  resourcePressure: "normal",
};

test("opens an accessible dark local workspace", async ({ page }) => {
  await mockApplication(page, []);

  await page.goto(`/#bootstrap=${"a".repeat(43)}`);

  await expect(page).toHaveURL(/\/projects$/u);
  await expect(page.getByRole("heading", { name: "Projects", level: 1 })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Primary" })).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "No projects have been loaded yet" }),
  ).toBeVisible();
  await expect(page.locator("html")).toHaveClass(/dark/u);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
});

test("opens an exact generation from an immutable catalog page", async ({ page }) => {
  await mockApplication(page, [projectSummary()]);
  await page.goto(`/#bootstrap=${"a".repeat(43)}`);

  await page.getByRole("combobox", { name: "State" }).selectOption("ready");
  await page.getByRole("searchbox", { name: "Search projects" }).fill("root");
  await page.getByRole("searchbox", { name: "Search projects" }).press("Enter");
  await page.getByRole("link", { name: /Rootlight/u }).click();

  await expect(page).toHaveURL(
    new RegExp(`/projects/${repositoryId}\\?generation=${generationId}$`, "u"),
  );
  await expect(page.getByRole("heading", { name: "Rootlight", level: 1 })).toBeVisible();
  await expect(page.getByText("published", { exact: true })).toBeVisible();
  await expect(page.getByText("90% indexed coverage")).not.toBeVisible();
  await expect(page.getByText("9 / 10 files")).toBeVisible();
  await expect(page.getByText("repository index")).toBeVisible();
  await expect(page.getByText("2 / 4 units")).toBeVisible();
  await page.locator("#main-content").getByRole("link", { name: "Projects" }).click();
  await expect(page.getByRole("combobox", { name: "State" })).toHaveValue("ready");
  await expect(page.getByRole("searchbox", { name: "Search projects" })).toHaveValue("root");
  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
});

test("admits a capability-bound detached index and opens its publication", async ({ page }) => {
  await mockApplication(page, []);
  const workflow = await mockIndexWorkflow(page, "succeeded");
  await page.goto(`/#bootstrap=${"a".repeat(43)}`);

  await page.getByRole("button", { name: "Add project" }).click();
  await page.getByRole("button", { name: "Home" }).click();
  await expect(page.getByRole("button", { name: "crates" })).toBeVisible();
  await page.getByRole("radio", { name: /Deep/u }).click();
  await page.getByRole("button", { name: "Select this folder" }).click();
  await expect(page.getByRole("heading", { name: "Ready to index" })).toBeVisible();
  await page.getByRole("button", { name: "Start detached index" }).click();

  await expect(page.getByRole("heading", { name: "Index operations" })).toBeVisible();
  const projectLink = page.getByRole("link", { name: "Open project" });
  await expect(projectLink).toHaveAttribute(
    "href",
    `/projects/${repositoryId}?generation=${generationId}`,
  );
  expect(workflow.admissionRequest).toMatchObject({
    rootCapability,
    mode: "deep",
    detached: true,
  });
  expect(workflow.admissionRequest?.clientRequestId).toMatch(/^idx_[a-f0-9]{48}$/u);
  expect(JSON.stringify(workflow.admissionRequest)).not.toContain("\\");

  await projectLink.click();
  await expect(page).toHaveURL(
    new RegExp(`/projects/${repositoryId}\\?generation=${generationId}$`, "u"),
  );
  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
});

test("requires confirmation before cancelling a running index", async ({ page }) => {
  await mockApplication(page, []);
  const workflow = await mockIndexWorkflow(page, "running");
  await page.goto(`/#bootstrap=${"a".repeat(43)}`);

  await page.getByRole("button", { name: "Add project" }).click();
  await page.getByRole("button", { name: "Home" }).click();
  await page.getByRole("button", { name: "Select this folder" }).click();
  await page.getByRole("button", { name: "Start detached index" }).click();
  await page
    .getByRole("region", { name: "Index operations" })
    .getByRole("button", { name: "Cancel" })
    .click();

  const dialog = page.getByRole("dialog", { name: "Cancel index operation?" });
  await expect(dialog).toContainText(operationId);
  await dialog.getByRole("button", { name: "Request cancellation" }).click();
  await expect(page.getByText("cancelling", { exact: true })).toBeVisible();
  expect(workflow.cancelRequests).toBe(1);
  expect(workflow.cancelCsrf).toBe("csrf");
});

async function mockApplication(page: Page, projects: ReturnType<typeof projectSummary>[]) {
  await page.route("**/api/v1/session/bootstrap", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ csrfToken: "csrf", idleTtlSeconds: 1_800 }),
    });
  });
  await page.route("**/api/v1/health", async (route) => {
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(health) });
  });
  await page.route("**/api/v1/projects**", async (route) => {
    const url = new URL(route.request().url());
    const body =
      url.pathname === "/api/v1/projects"
        ? {
            schema: "rootlight.web-project-catalog-page/1",
            projects,
            snapshot: "snapshot",
            nextAfter: null,
            totalCount: String(projects.length),
            truncated: false,
            sortVersion: 1,
          }
        : projectDetail();
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
}

async function mockIndexWorkflow(page: Page, terminalState: "running" | "succeeded") {
  const workflow: {
    admissionRequest?: Record<string, unknown>;
    cancelRequests: number;
    cancelCsrf?: string;
  } = { cancelRequests: 0 };
  let cancelled = false;

  await page.route("**/api/v1/filesystem/roots", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        schema: "rootlight.web-filesystem-roots/1",
        roots: [{ label: "Home", browseToken, readable: true, selectable: true }],
      }),
    });
  });
  await page.route("**/api/v1/filesystem/browse", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        schema: "rootlight.web-filesystem-browse/1",
        browseToken,
        label: "Home",
        depth: 0,
        maximumDepth: 32,
        breadcrumbs: [{ label: "Home", browseToken }],
        directories: [{ name: "crates", kind: "directory", readable: true, selectable: true }],
        nextCursor: null,
      }),
    });
  });
  await page.route("**/api/v1/filesystem/preflight-index", async (route) => {
    const request = (await route.request().postDataJSON()) as Record<string, unknown>;
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        schema: "rootlight.web-index-preflight/1",
        selectable: true,
        normalizedDisplayLabel: "rootlight",
        daemonAcceptingOperations: true,
        selectedMode: request.mode,
        supportedModes: ["auto", "structural", "deep"],
        adapterIsolation: "available",
        estimatedLimitations: [],
        warnings: ["repository_contents_not_scanned"],
        rootCapability,
        rootCapabilityExpiresInSeconds: 120,
      }),
    });
  });
  await page.route("**/api/v1/projects/index", async (route) => {
    workflow.admissionRequest = (await route.request().postDataJSON()) as Record<string, unknown>;
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(indexAdmission()),
    });
  });
  await page.route("**/api/v1/operations/**", async (route) => {
    if (new URL(route.request().url()).pathname.endsWith("/cancel")) {
      cancelled = true;
      workflow.cancelRequests += 1;
      workflow.cancelCsrf = route.request().headers()["x-rootlight-csrf"];
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          schema: "rootlight.web-operation-cancel/1",
          accepted: true,
          operation: operationStatus("cancelling"),
        }),
      });
      return;
    }
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(operationStatus(cancelled ? "cancelling" : terminalState)),
    });
  });

  return workflow;
}

function indexAdmission() {
  return {
    schema: "rootlight.web-project-index/1",
    displayLabel: "rootlight",
    repositoryId,
    operationId,
    semanticOperationId: null,
    state: "queued",
    revision: "1",
    mode: "deep",
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

function operationStatus(state: "cancelling" | "running" | "succeeded") {
  const succeeded = state === "succeeded";
  return {
    schema: "rootlight.web-repository-operation/1",
    displayLabel: "rootlight",
    mode: "deep",
    ownedBySession: true,
    operationId,
    state,
    revision: state === "running" ? "2" : "3",
    completedUnits: succeeded ? 4 : 2,
    totalUnits: 4,
    kind: "repository_index",
    stage: succeeded ? "completed" : "executing",
    detached: true,
    cancellationRequested: state === "cancelling",
    recoveryClass: "not_applicable",
    error: null,
    publishedGenerationId: succeeded ? generationId : null,
    semanticOperationId: null,
    startedUnixMs: "1",
    peakRssBytes: "2",
    writtenBytes: "3",
    filesExamined: "4",
    bytesExamined: "5",
    indexStage: succeeded ? "published" : "indexing",
    retryAfterMs: state === "running" ? 100 : null,
  };
}

function projectSummary() {
  return {
    repositoryId,
    activeGenerationId: generationId,
    displayName: "Rootlight",
    alias: null,
    generationCount: "2",
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
}

function projectDetail() {
  return {
    schema: "rootlight.web-project-detail/1",
    repositoryId,
    displayName: "Rootlight",
    alias: null,
    resolvedGenerationId: generationId,
    activeGenerationId: generationId,
    parentGenerationId: null,
    activeParentGenerationId: null,
    activeStructuralFreshness: "current",
    activeSemanticFreshness: "stale",
    structuralFreshness: "current",
    semanticFreshness: "stale",
    lifecycleState: "ready",
    publicationState: "published",
    coverage: projectSummary().coverage,
    operations: [
      {
        operationId,
        kind: "repository_index",
        state: "running",
        completedUnits: 2,
        totalUnits: 4,
        ownedByClient: true,
        startedUnixMs: "1",
      },
    ],
  };
}
