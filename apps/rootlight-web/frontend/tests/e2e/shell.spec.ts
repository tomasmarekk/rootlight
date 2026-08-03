// Exercises authenticated product flows, accessibility states, and keyboard operation.
// Strict serving policy checks ensure browser acceptance matches the native local host.

import { createHash } from "node:crypto";

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";

import { expectPrimaryMarkupQuality, monitorBrowserQuality } from "./quality-fixtures";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const operationId = `op1_${"c".repeat(32)}`;
const symbolId = `sym1_${"c".repeat(39)}`;
const targetSymbolId = `sym1_${"d".repeat(39)}`;
const browseToken = "d".repeat(43);
const rootCapability = "e".repeat(43);
const projectionToken = "f".repeat(43);
const qualityMonitors = new WeakMap<Page, ReturnType<typeof monitorBrowserQuality>>();

const health = {
  webReady: true,
  daemonReady: true,
  protocolVersion: "1.10",
  lifecycle: "ready",
  acceptingOperations: true,
  activeOperations: 0,
  admittedOperations: 0,
  queuedOperations: 0,
  runningOperations: 0,
  activeConnections: 1,
  connectionLimit: 64,
  operationQueueLimit: 128,
  journalHealthy: true,
  catalogSchemaVersion: 4,
  endpointSchemaVersion: 1,
  catalogStatus: "healthy",
  generationStatus: "healthy",
  adapterStatus: "healthy",
  watcherStatus: "not_configured",
  endpointStatus: "healthy",
  resourcePressure: "normal",
};

test.beforeEach(({ page }) => {
  qualityMonitors.set(page, monitorBrowserQuality(page));
});

test.afterEach(async ({ page }) => {
  await qualityMonitors.get(page)?.assertClean();
});

test("opens an accessible dark local workspace", async ({ page }) => {
  await mockApplication(page, []);

  await page.goto("/");

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
  await page.goto("/");

  await page.getByRole("combobox", { name: "State" }).selectOption("ready");
  await page.getByRole("searchbox", { name: "Search projects" }).fill("root");
  await page.getByRole("searchbox", { name: "Search projects" }).press("Enter");
  await page.getByRole("link", { name: /Rootlight/u }).click();

  await expect(page).toHaveURL((url) => {
    return (
      url.pathname === `/projects/${repositoryId}` &&
      url.searchParams.get("generation") === generationId
    );
  });
  await expect(page.getByRole("heading", { name: "Rootlight", level: 1 })).toBeVisible();
  await expect(
    page.getByLabel("Project status").getByText("published", { exact: true }),
  ).toBeVisible();
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
  await page.goto("/");

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
  await expect(page).toHaveURL((url) => {
    return (
      url.pathname === `/projects/${repositoryId}` &&
      url.searchParams.get("generation") === generationId
    );
  });
  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
});

test("requires confirmation before cancelling a running index", async ({ page }) => {
  await mockApplication(page, []);
  const workflow = await mockIndexWorkflow(page, "running");
  await page.goto("/");

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

test("explores exact-generation evidence through the accessible graph companion", async ({
  page,
}) => {
  await mockApplication(page, [projectSummary()]);
  await mockEvidence(page);
  await page.goto("/");
  await page.getByRole("link", { name: /Rootlight/u }).click();

  const runNode = page.getByRole("button", { name: /run symbol src\/main\.rs/u });
  await expect(runNode).toBeVisible();
  await runNode.click();
  await expect(page.getByRole("heading", { name: "run", level: 2 })).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);

  await page.getByRole("button", { name: "Show source" }).click();
  const source = page.getByLabel("Explicitly loaded source");
  await expect(source).toContainText("<img src=x onerror=repositoryAttack()>");
  await expect(page.locator("img")).toHaveCount(0);

  await page.getByRole("button", { name: "Calculate impact" }).click();
  await expect(page.getByText("medium risk")).toBeVisible();
  await expect(page.getByText("Impact overlay · 1")).toBeVisible();
  await expect(
    page.getByRole("button", { name: /dispatch symbol src\/dispatch\.rs.*Change impact/u }),
  ).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);

  await page.keyboard.press("Escape");
  await expect(page.getByRole("complementary", { name: "run" })).not.toBeVisible();
});

test("keeps session-owned operations and local diagnostics usable end to end", async ({ page }) => {
  await mockApplication(page, []);
  await mockIndexWorkflow(page, "running");
  await mockDiagnostics(page);
  await page.goto("/");

  await page.getByRole("button", { name: "Add project" }).click();
  await page.getByRole("button", { name: "Home" }).click();
  await expect(page.getByRole("button", { name: "crates" })).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);
  await page.getByRole("button", { name: "Select this folder" }).click();
  await page.getByRole("button", { name: "Start detached index" }).click();
  await page.getByRole("link", { name: "Operations" }).click();

  await expect(page.getByRole("heading", { name: "Operations", level: 1 })).toBeVisible();
  await expect(page.getByRole("region", { name: "Index operations" })).toContainText(operationId);
  await page
    .getByRole("region", { name: "Index operations" })
    .getByRole("button", { name: "Cancel" })
    .click();
  const cancelDialog = page.getByRole("dialog", { name: "Cancel index operation?" });
  await cancelDialog.getByRole("button", { name: "Request cancellation" }).click();
  await expect(page.getByText("cancelling", { exact: true })).toBeVisible();
  await expect(cancelDialog).toHaveCount(0);
  await expectNoSeriousAccessibilityViolations(page);

  await page.getByRole("link", { name: "Diagnostics" }).click();
  await expect(page.getByRole("heading", { name: "Diagnostics", level: 1 })).toBeVisible();
  await page.getByRole("button", { name: "Quick diagnostics" }).click();
  await expect(page.getByText("Catalog check timed out")).toBeVisible();
  await page.getByRole("button", { name: "Prepare support bundle" }).click();
  await expect(page.getByText("9 bytes")).toBeVisible();
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Verify and download" }).click();
  expect((await download).suggestedFilename()).toBe("rootlight-support-bundle.zip");
  await expect(page.getByText(/This single-use archive was downloaded/u)).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);
});

test("reopens a fallback projection, clears source, and expires the session fail closed", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1_024, height: 800 });
  await page.emulateMedia({ forcedColors: "active", reducedMotion: "reduce" });
  await page.addInitScript(() => {
    Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
      configurable: true,
      value: () => null,
    });
  });
  const application = await mockApplication(page, [projectSummary()]);
  const evidence = await mockEvidence(page);
  await page.goto("/");
  await page.getByRole("link", { name: /Rootlight/u }).click();

  await expect(page.getByRole("heading", { name: "Graphical view is unavailable" })).toBeVisible();
  await page.getByRole("button", { name: /run symbol src\/main\.rs/u }).click();
  await page.getByRole("button", { name: "Show source" }).click();
  await expect(page.getByLabel("Explicitly loaded source")).toBeVisible();
  const openCount = application.graphOpenCount();

  await page.evaluate(() => {
    window.dispatchEvent(new Event("rootlight:daemon-reconnected"));
  });

  await expect(page.getByLabel("Explicitly loaded source")).not.toBeVisible();
  await expect
    .poll(() => application.graphOpenCount(), { timeout: 5_000 })
    .toBeGreaterThan(openCount);
  await expect(page.getByRole("heading", { name: "Graphical view is unavailable" })).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
    ),
  ).toBe(true);

  evidence.expireSession();
  await page.getByRole("button", { name: "Calculate impact" }).click();
  await expect(page.getByRole("heading", { name: "This local session has ended" })).toBeVisible();
  await expect(page.getByLabel("Explicitly loaded source")).not.toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);
});

test("completes the critical local path using keyboard input only", async ({ page }) => {
  await mockApplication(page, [projectSummary()]);
  await mockIndexWorkflow(page, "running");
  await mockEvidence(page);
  await mockDiagnostics(page);
  await page.goto("/");

  const addProject = page.getByRole("button", { name: "Add project" });
  await activate(addProject);
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(addProject).toBeFocused();
  await activate(addProject);
  await activate(page.getByRole("button", { name: "Home" }));
  await expect(page.getByRole("button", { name: "crates" })).toBeVisible();
  await activate(page.getByRole("radio", { name: /Deep/u }), "Space");
  await expect(page.getByRole("button", { name: "Select this folder" })).toBeEnabled();
  await activate(page.getByRole("button", { name: "Select this folder" }));
  await expect(page.getByRole("heading", { name: "Ready to index" })).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);
  await activate(page.getByRole("button", { name: "Start detached index" }));
  await expect(page.getByRole("dialog")).toHaveCount(0);

  await activate(page.getByRole("link", { name: /Rootlight/u }));
  const companionSearch = page.getByRole("searchbox", { name: "Search visible nodes" });
  await companionSearch.focus();
  await expect(companionSearch).toBeFocused();
  await page.keyboard.type("run");
  const runNode = page.getByRole("button", { name: /run symbol src\/main\.rs/u });
  await activate(runNode);
  await expect(page.getByRole("heading", { name: "run", level: 2 })).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);

  await page.keyboard.press("Escape");
  await expect(page.getByRole("complementary", { name: "run" })).not.toBeVisible();
  await activate(page.getByRole("link", { name: "Operations" }));
  const cancel = page
    .getByRole("region", { name: "Index operations" })
    .getByRole("button", { name: "Cancel" });
  await activate(cancel);
  const cancelDialog = page.getByRole("dialog", { name: "Cancel index operation?" });
  await activate(cancelDialog.getByRole("button", { name: "Request cancellation" }));
  await expect(page.getByText("cancelling", { exact: true })).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);

  await activate(page.getByRole("link", { name: "Diagnostics" }));
  await activate(page.getByRole("button", { name: "Quick diagnostics" }));
  await expect(page.getByText("Catalog check timed out")).toBeVisible();
  await activate(page.getByRole("button", { name: "Prepare support bundle" }));
  await expect(page.getByText("9 bytes")).toBeVisible();
  await expectNoSeriousAccessibilityViolations(page);
});

async function mockApplication(page: Page, projects: ReturnType<typeof projectSummary>[]) {
  let graphOpenCount = 0;
  await page.route("**/api/v1/session", async (route) => {
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
  await page.route("**/api/v1/graph/projections**", async (route) => {
    if (route.request().method() === "DELETE") {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          schema: "rootlight.web-graph-release/1",
          projectionToken,
          released: true,
        }),
      });
      return;
    }
    if (new URL(route.request().url()).pathname === "/api/v1/graph/projections") {
      graphOpenCount += 1;
    }
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(graphPage()),
    });
  });
  return {
    graphOpenCount: () => graphOpenCount,
  };
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

async function mockEvidence(page: Page) {
  let expired = false;
  await page.route("**/api/v1/projects/*/nodes/*", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(nodeDetail()),
    });
  });
  await page.route("**/api/v1/projects/*/relationships", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(relationships()),
    });
  });
  await page.route("**/api/v1/projects/*/source", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(sourceRead()),
    });
  });
  await page.route("**/api/v1/projects/*/change-impact", async (route) => {
    if (expired) {
      await route.fulfill({
        status: 401,
        contentType: "application/json",
        body: JSON.stringify({ error: { code: "session_required" } }),
      });
      return;
    }
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(changeImpact()),
    });
  });
  return {
    expireSession: () => {
      expired = true;
    },
  };
}

async function mockDiagnostics(page: Page) {
  const archive = Buffer.from("rootlight", "utf8");
  const digest = createHash("sha256").update(archive).digest("hex");
  const receipt = "s".repeat(43);
  await page.route("**/api/v1/diagnostics/quick", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        schema: "rootlight.web-quick-diagnostics/1",
        schemaVersion: 1,
        overallStatus: "degraded",
        durationMs: 125,
        checks: [
          {
            name: "catalog",
            outcome: "timed_out",
            durationMs: 125,
            error: {
              code: 12,
              message: "Catalog check timed out",
              retryable: true,
              retryAfterMs: "1000",
            },
          },
        ],
      }),
    });
  });
  await page.route("**/api/v1/diagnostics/support-bundle", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        schema: "rootlight.web-support-bundle/1",
        receipt,
        downloadPath: `/api/v1/diagnostics/support-bundles/${receipt}`,
        archiveBytes: String(archive.byteLength),
        sha256: digest,
        containsSource: false,
        expiresInSeconds: 120,
      }),
    });
  });
  await page.route(`**/api/v1/diagnostics/support-bundles/${receipt}`, async (route) => {
    await route.fulfill({
      status: 200,
      body: archive,
      headers: {
        "content-length": String(archive.byteLength),
        "content-type": "application/zip",
        "x-rootlight-sha256": digest,
      },
    });
  });
}

async function expectNoSeriousAccessibilityViolations(page: Page) {
  await expectPrimaryMarkupQuality(page);
  const result = await new AxeBuilder({ page }).analyze();
  expect(
    result.violations.filter(
      (violation) => violation.impact === "serious" || violation.impact === "critical",
    ),
  ).toEqual([]);
}

async function activate(locator: Locator, key: "Enter" | "Space" = "Enter") {
  await locator.focus();
  await expect(locator).toBeFocused();
  await locator.page().keyboard.press(key);
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

function nodeDetail() {
  return {
    schema: "rootlight.web-node-detail/1",
    repositoryId,
    generationId,
    nodeId: symbolId,
    idKind: "symbol",
    kind: "function",
    displayName: "run",
    qualifiedName: null,
    signature: "fn run()",
    language: "rust",
    tier: "tier_a",
    confidence: 950,
    provider: "scip",
    evidence: "definition",
    outboundExact: "1",
    outboundCandidates: "0",
    inboundExact: "0",
    inboundCandidates: "0",
    referenceCount: "1",
    generated: null,
    sourceReferences: [{ capability: rootCapability, expiresInSeconds: 60 }],
    context: evidenceContext(),
    completeness: complete(),
  };
}

function relationships() {
  return {
    schema: "rootlight.web-relationships/1",
    context: evidenceContext(),
    groups: [
      {
        seedId: symbolId,
        relation: "calls",
        direction: "outbound",
        totalCount: "1",
        targets: [{ symbolId: targetSymbolId, confidence: 900, sourceReferences: [] }],
      },
    ],
    returnedEdges: "1",
    totalEdges: "1",
    exact: true,
    truncated: false,
    nextPageOffset: null,
    completeness: complete(),
  };
}

function sourceRead() {
  const content = "<img src=x onerror=repositoryAttack()>";
  const sourceBytes = String(new TextEncoder().encode(content).byteLength);
  return {
    schema: "rootlight.web-source/1",
    repositoryId,
    generationId,
    chunks: [
      {
        fileId: `file1_${"f".repeat(39)}`,
        path: "src/untrusted.rs",
        requestedStartByte: "0",
        requestedEndByte: sourceBytes,
        includedStartByte: "0",
        includedEndByte: sourceBytes,
        includedStartLine: "1",
        includedEndLine: "1",
        content,
        encoding: "utf8",
        contentHash: `b3_${"a".repeat(58)}`,
        language: "rust",
        tier: "tier_a",
        generated: false,
      },
    ],
    totalSourceBytes: sourceBytes,
    truncated: false,
    context: evidenceContext(sourceBytes),
    completeness: complete(),
  };
}

function changeImpact() {
  return {
    schema: "rootlight.web-change-impact/1",
    context: evidenceContext(),
    resolvedChanges: [{ symbolId, fileId: null, classification: "resolved", kind: "function" }],
    impacted: [
      {
        sourceIndex: 0,
        dependents: [
          {
            symbolId: targetSymbolId,
            kind: "function",
            distance: 1,
            confidence: 900,
            via: ["calls"],
            isPublic: true,
          },
        ],
      },
    ],
    tests: [],
    riskSummary: {
      level: "medium",
      reasons: ["public_fanout"],
      coverage: "bounded",
      breakingSurface: true,
      fanout: 1,
      dynamicBlindSpots: false,
    },
    completeness: complete(),
  };
}

function evidenceContext(sourceBytes = "0") {
  return {
    repositoryId,
    generationId,
    parentGenerationId: null,
    activeGeneration: true,
    structuralFreshness: "current",
    semanticFreshness: "current",
    tier: "tier_a",
    coverageStatus: "complete",
    skippedInputs: "0",
    usage: {
      rows: "1",
      edges: "1",
      results: "1",
      sourceBytes,
      jsonBytes: "128",
      estimatedTokens: "32",
      tokenAccountingProfile: null,
      memoryBytes: null,
      elapsedMicros: "10",
    },
  };
}

function graphPage() {
  return {
    schema: "rootlight.web-graph-page/1",
    projectionToken,
    pageOrdinal: 0,
    context: {
      repositoryId,
      generationId,
      parentGenerationId: null,
      activeGeneration: true,
      structuralFreshness: "current",
      semanticFreshness: "current",
      tier: "tier_a",
      coverageStatus: "complete",
      skippedInputs: "0",
    },
    nodes: [
      graphNode(0, symbolId, "run", "src/main.rs", 950),
      graphNode(1, targetSymbolId, "dispatch", "src/dispatch.rs", 900),
    ],
    edges: [
      {
        sourceOrdinal: 0,
        targetOrdinal: 1,
        relation: "calls",
        weight: 1,
        confidence: 900,
        exact: true,
        inferred: false,
        evidenceCount: 1,
        overlay: "none",
      },
    ],
    completeness: complete(),
    effectiveBudget: {
      pageNodes: 200,
      pageEdges: 500,
      aggregateNodes: 512,
      aggregateEdges: 1_000,
    },
    returnedNodesCumulative: "2",
    returnedEdgesCumulative: "1",
    totalMatchingNodes: "2",
    totalMatchingEdges: "1",
    totalKnownNodes: "2",
    totalKnownEdges: "1",
    edgesOmittedForUnavailableEndpoints: "0",
    skippedForCoverage: "0",
    hasNextPage: false,
  };
}

function graphNode(
  ordinal: number,
  stableId: string,
  label: string,
  path: string,
  confidence: number,
) {
  return {
    ordinal,
    stableId,
    idKind: "symbol",
    label,
    path,
    kind: "symbol",
    confidence,
    generated: false,
    community: "core",
    component: null,
    symbolCount: null,
    fanIn: ordinal,
    fanOut: 1,
    hotspotScore: ordinal * 20,
    evidence: "structural",
  };
}

function complete() {
  return {
    state: "complete",
    limitingResources: [],
    continuation: "not_applicable",
    guidance: [],
  };
}
