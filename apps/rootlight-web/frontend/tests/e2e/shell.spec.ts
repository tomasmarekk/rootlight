// Validates the dark authenticated shell and its initial accessibility baseline.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;

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
  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
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
        operationId: `op1_${"c".repeat(32)}`,
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
