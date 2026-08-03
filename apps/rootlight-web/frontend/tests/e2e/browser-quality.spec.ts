// Enforces production bundle, maximum supported graph, and lifecycle quality budgets.
// Headless software WebGL proves browser correctness; physical-GPU FPS remains a separate lane.

import { gzipSync } from "node:zlib";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { basename, dirname, extname, join, resolve } from "node:path";

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

import {
  bootstrapUrl,
  expectPrimaryMarkupQuality,
  historicalGenerationId,
  installQualityApplication,
  monitorBrowserQuality,
  repositoryId,
} from "./quality-fixtures";

const distRoot = resolve(import.meta.dirname, "../../dist");
const maximumInitialJavaScriptGzipBytes = 350 * 1_024;
const maximumFirstUsefulMilliseconds = 2_000;
const maximumSelectionP95Milliseconds = 100;

test("keeps the Projects entry bounded and lazy-loads graph resources", async ({
  page,
}, testInfo) => {
  const quality = monitorBrowserQuality(page);
  await installQualityApplication(page);
  await page.goto(bootstrapUrl);
  await expect(page.getByRole("heading", { name: "Projects", level: 1 })).toBeVisible();
  await expectPrimaryMarkupQuality(page);

  const projectsResources = await resourceUrls(page);
  const initialJavaScript = projectsResources.filter((url) => assetExtension(url) === ".js");
  expect(initialJavaScript).toHaveLength(1);
  expect(initialJavaScript.some((url) => url.includes("graph-decoder.worker"))).toBe(false);

  const initialJavaScriptGzipBytes = initialJavaScript.reduce(
    (total, url) => total + gzipBytesForAsset(url),
    0,
  );
  expect(initialJavaScriptGzipBytes).toBeLessThanOrEqual(maximumInitialJavaScriptGzipBytes);
  const distFiles = recursiveFiles(distRoot);
  const publicSourceMaps = distFiles.filter((path) => path.endsWith(".map"));
  const embeddedSourceMaps = distFiles.filter(
    (path) =>
      [".css", ".js"].includes(extname(path)) &&
      /[#@]\s*sourceMappingURL=/u.test(readFileSync(path, "utf8")),
  );
  expect(publicSourceMaps).toEqual([]);
  expect(embeddedSourceMaps).toEqual([]);

  await page.getByRole("link", { name: /Synthetic Atlas/u }).click();
  await expect(page.locator(".graph-viewport__canvas[data-lifecycle='ready'] canvas")).toBeVisible({
    timeout: 15_000,
  });
  await expectPrimaryMarkupQuality(page);

  await expect
    .poll(async () => {
      const graphResources = await resourceUrls(page);
      return graphResources.filter(
        (url) => assetExtension(url) === ".js" && !projectsResources.includes(url),
      ).length;
    })
    .toBeGreaterThanOrEqual(2);
  const graphResources = await resourceUrls(page);
  expect(graphResources.some((url) => url.includes("graph-decoder.worker"))).toBe(true);

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(
    accessibility.violations.filter(
      (violation) => violation.impact === "serious" || violation.impact === "critical",
    ),
  ).toEqual([]);
  await quality.assertClean();

  const initialCss = projectsResources.filter((url) => assetExtension(url) === ".css");
  const evidence = {
    schema: "rootlight.web-bundle-quality/1",
    sourceRevision: process.env.SOURCE_REVISION ?? "local",
    initialJavaScript: initialJavaScript.map((url) => basename(new URL(url).pathname)),
    initialJavaScriptGzipBytes,
    initialJavaScriptGzipLimitBytes: maximumInitialJavaScriptGzipBytes,
    initialCss: initialCss.map((url) => basename(new URL(url).pathname)),
    initialCssGzipBytes: initialCss.reduce((total, url) => total + gzipBytesForAsset(url), 0),
    graphLazyJavaScript: graphResources
      .filter((url) => assetExtension(url) === ".js" && !projectsResources.includes(url))
      .map((url) => basename(new URL(url).pathname)),
    publicSourceMaps: publicSourceMaps.length + embeddedSourceMaps.length,
    externalRuntimeRequests: quality.externalRequests.length,
  };
  const evidencePath = testInfo.outputPath("web-bundle-quality.json");
  mkdirSync(dirname(evidencePath), { recursive: true });
  writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  await testInfo.attach("web-bundle-quality", {
    body: Buffer.from(JSON.stringify(evidence)),
    contentType: "application/json",
  });
});

test("meets maximum supported graph interaction and disposal budgets", async ({
  browser,
  browserName,
  page,
}, testInfo) => {
  test.setTimeout(90_000);
  await installWorkerTracker(page);
  const quality = monitorBrowserQuality(page);
  const application = await installQualityApplication(page);
  await page.goto(bootstrapUrl);
  await expect(page.getByRole("heading", { name: "Projects", level: 1 })).toBeVisible();

  const firstUsefulMilliseconds: number[] = [];
  const selectionMilliseconds: number[] = [];
  const disposalMilliseconds: number[] = [];
  const settleMilliseconds: number[] = [];
  const longTasks: number[] = [];

  for (let iteration = 0; iteration < 5; iteration += 1) {
    const priorFirstUseful = await markCount(page, "rootlight.graph.first-useful");
    const priorSettle = await markCount(page, "rootlight.graph.controller.settle");
    await page.getByRole("link", { name: /Synthetic Atlas/u }).click();
    await expect(
      page.locator(".graph-viewport__canvas[data-lifecycle='ready'] canvas"),
    ).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByText("250 of 250 returned nodes")).toBeVisible();
    await expect
      .poll(() => markCount(page, "rootlight.graph.first-useful"))
      .toBeGreaterThan(priorFirstUseful);
    await expect
      .poll(() => markCount(page, "rootlight.graph.controller.settle"), { timeout: 5_000 })
      .toBeGreaterThan(priorSettle);

    const browserTiming = await latestGraphTiming(page);
    firstUsefulMilliseconds.push(browserTiming.firstUsefulMilliseconds);
    settleMilliseconds.push(browserTiming.settleMilliseconds);
    longTasks.push(...browserTiming.longTasks);

    const firstNode = page.getByRole("button", {
      name: /component-000 symbol synthetic\/module-000/u,
    });
    await expect(firstNode).toBeVisible();
    selectionMilliseconds.push(await measureSelection(firstNode));
    await expect(firstNode).toHaveAttribute("aria-pressed", "true");

    const priorDispose = await markCount(page, "rootlight.graph.controller.dispose");
    disposalMilliseconds.push(
      await measureClickToMark(
        page.locator("#main-content").getByRole("link", { name: "Projects", exact: true }),
        "rootlight.graph.controller.dispose",
        priorDispose,
      ),
    );
    await expect(page.getByRole("heading", { name: "Projects", level: 1 })).toBeVisible();
    await expect.poll(() => application.graphReleaseCount()).toBe(application.graphOpenCount());
    await expect.poll(() => workerStats(page)).toMatchObject({ active: 0 });
  }

  const firstUseful = summarize(firstUsefulMilliseconds);
  const selection = summarize(selectionMilliseconds);
  const disposal = summarize(disposalMilliseconds);
  const settle = summarize(settleMilliseconds);
  expect(firstUseful.p95).toBeLessThanOrEqual(maximumFirstUsefulMilliseconds);
  expect(selection.p95).toBeLessThanOrEqual(maximumSelectionP95Milliseconds);
  expect(disposal.p95).toBeLessThanOrEqual(500);
  expect(settle.p95).toBeLessThanOrEqual(1_600);
  expect(longTasks.filter((duration) => duration > 50)).toEqual([]);
  expect(application.activeProjectionCount()).toBe(0);
  expect(application.graphOpenCount()).toBe(5);
  expect(application.graphReleaseCount()).toBe(5);
  expect(await workerStats(page)).toMatchObject({ active: 0, maximumActive: 1 });

  const environment = await browserEnvironment(page);
  const evidence = {
    schema: "rootlight.web-graph-performance/1",
    sourceRevision: process.env.SOURCE_REVISION ?? "local",
    browser: {
      engine: browserName,
      version: browser.version(),
      renderingMode: "headless-software-webgl-correctness",
    },
    runtime: {
      node: process.version,
      npm: "11.6.2",
      platform: process.platform,
    },
    viewport: page.viewportSize(),
    environment,
    dataset: {
      fingerprint: "synthetic-atlas-max-v1",
      nodes: 250,
      edges: 1_000,
      containsSource: false,
      webProfileMaximumNodes: 250,
      webProfileMaximumEdges: 1_000,
    },
    samples: 5,
    measurementsMilliseconds: {
      firstUseful,
      selection,
      settle,
      disposal,
      maximumMainThreadLongTask: Math.max(0, ...longTasks),
    },
    gpuFpsMeasured: false,
  };
  const evidencePath = testInfo.outputPath("web-graph-performance.json");
  mkdirSync(dirname(evidencePath), { recursive: true });
  writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  await testInfo.attach("web-graph-performance", {
    body: Buffer.from(JSON.stringify(evidence)),
    contentType: "application/json",
  });
  await quality.assertClean();
});

test("bounds retained workers and projections across generation and view churn", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await installWorkerTracker(page);
  const quality = monitorBrowserQuality(page);
  const application = await installQualityApplication(page, { edgeCount: 24, nodeCount: 12 });
  await page.goto(
    `/projects/${repositoryId}?generation=${historicalGenerationId}#bootstrap=${"a".repeat(43)}`,
  );
  await expect(page.getByRole("heading", { name: "Synthetic Atlas", level: 1 })).toBeVisible();
  await expect(page.getByText("12 of 12 returned nodes")).toBeVisible();

  const generation = page.getByRole("combobox", { name: "Generation" });
  await generation.selectOption("active");
  await waitForNewProjection(page, application, 1);
  for (let iteration = 0; iteration < 5; iteration += 1) {
    const beforeBack = application.graphOpenCount();
    await page.goBack();
    await waitForNewProjection(page, application, beforeBack);
    const beforeForward = application.graphOpenCount();
    await page.goForward();
    await waitForNewProjection(page, application, beforeForward);
  }

  for (let iteration = 0; iteration < 20; iteration += 1) {
    const files = page.getByRole("radio", { name: "files", exact: true });
    const targetView = (await files.isChecked()) ? "architecture" : "files";
    const target = page.getByRole("radio", { name: targetView, exact: true });
    const beforeChange = application.graphOpenCount();
    await target.click();
    await expect(target).toBeChecked();
    await waitForNewProjection(page, application, beforeChange);
    expect(await page.locator(".graph-viewport__canvas canvas").count()).toBeLessThanOrEqual(1);
    expect(await workerStats(page)).toMatchObject({ active: 1, maximumActive: 1 });
  }

  const openBeforeExit = application.graphOpenCount();
  await page.locator("#main-content").getByRole("link", { name: "Projects", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Projects", level: 1 })).toBeVisible();
  await expect.poll(() => application.graphReleaseCount()).toBe(openBeforeExit);
  await expect.poll(() => workerStats(page)).toMatchObject({ active: 0, maximumActive: 1 });
  expect(await markCount(page, "rootlight.graph.controller.dispose")).toBeGreaterThanOrEqual(30);
  expect(application.activeProjectionCount()).toBe(0);
  await quality.assertClean();
});

async function resourceUrls(page: Page) {
  return page.evaluate(() =>
    performance
      .getEntriesByType("resource")
      .map((entry) => entry.name)
      .filter((name) => name.startsWith(`${window.location.origin}/assets/`)),
  );
}

function assetExtension(url: string) {
  return extname(new URL(url).pathname);
}

function gzipBytesForAsset(url: string) {
  const path = resolve(distRoot, `.${new URL(url).pathname}`);
  if (!path.startsWith(distRoot)) {
    throw new Error("browser resource escaped the production dist");
  }
  return gzipSync(readFileSync(path), { level: 9 }).byteLength;
}

function recursiveFiles(root: string): string[] {
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = join(root, entry.name);
    return entry.isDirectory() ? recursiveFiles(path) : [path];
  });
}

async function installWorkerTracker(page: Page) {
  await page.addInitScript(() => {
    const NativeWorker = window.Worker;
    const stats = { active: 0, created: 0, maximumActive: 0, terminated: 0 };
    const activeWorkers = new WeakSet<Worker>();
    class TrackedWorker extends NativeWorker {
      constructor(scriptURL: string | URL, options?: WorkerOptions) {
        super(scriptURL, options);
        activeWorkers.add(this);
        stats.active += 1;
        stats.created += 1;
        stats.maximumActive = Math.max(stats.maximumActive, stats.active);
      }

      override terminate() {
        if (activeWorkers.delete(this)) {
          stats.active -= 1;
          stats.terminated += 1;
        }
        super.terminate();
      }
    }
    Object.defineProperty(window, "Worker", { configurable: true, value: TrackedWorker });
    Object.defineProperty(window, "__rootlightWorkerStats", {
      configurable: false,
      value: stats,
    });
  });
}

async function workerStats(page: Page) {
  return page.evaluate(() => {
    const trackedWindow = window as Window & {
      __rootlightWorkerStats?: {
        active: number;
        created: number;
        maximumActive: number;
        terminated: number;
      };
    };
    return (
      trackedWindow.__rootlightWorkerStats ?? {
        active: 0,
        created: 0,
        maximumActive: 0,
        terminated: 0,
      }
    );
  });
}

async function markCount(page: Page, name: string) {
  return page.evaluate((markName) => performance.getEntriesByName(markName, "mark").length, name);
}

async function latestGraphTiming(page: Page) {
  return page.evaluate(() => {
    const firstUseful = performance.getEntriesByName("rootlight.graph.first-useful", "mark").at(-1);
    const settle = performance.getEntriesByName("rootlight.graph.controller.settle", "mark").at(-1);
    const response = performance
      .getEntriesByType("resource")
      .filter((entry) => new URL(entry.name).pathname === "/api/v1/graph/projections")
      .at(-1) as PerformanceResourceTiming | undefined;
    if (firstUseful === undefined || settle === undefined || response === undefined) {
      throw new Error("graph performance marks are incomplete");
    }
    const longTasks = performance
      .getEntriesByType("longtask")
      .filter(
        (entry) =>
          entry.startTime >= response.responseStart && entry.startTime <= firstUseful.startTime,
      )
      .map((entry) => entry.duration);
    return {
      firstUsefulMilliseconds: firstUseful.startTime - response.responseStart,
      settleMilliseconds: settle.startTime - firstUseful.startTime,
      longTasks,
    };
  });
}

async function measureSelection(locator: ReturnType<Page["getByRole"]>) {
  return locator.evaluate((element) => {
    const button = element as HTMLButtonElement;
    return new Promise<number>((resolve, reject) => {
      const started = performance.now();
      const timeout = window.setTimeout(() => {
        observer.disconnect();
        reject(new Error("selection did not update"));
      }, 2_000);
      const observer = new MutationObserver(() => {
        if (button.getAttribute("aria-pressed") === "true") {
          window.clearTimeout(timeout);
          observer.disconnect();
          resolve(performance.now() - started);
        }
      });
      observer.observe(button, { attributeFilter: ["aria-pressed"], attributes: true });
      button.click();
    });
  });
}

async function measureClickToMark(
  locator: ReturnType<Page["getByRole"]>,
  markName: string,
  priorCount: number,
) {
  return locator.evaluate(
    (element, input) =>
      new Promise<number>((resolve, reject) => {
        const started = performance.now();
        const timeout = window.setTimeout(() => {
          observer.disconnect();
          reject(new Error(`mark ${input.markName} was not emitted`));
        }, 2_000);
        const observer = new PerformanceObserver(() => {
          const marks = performance.getEntriesByName(input.markName, "mark");
          if (marks.length > input.priorCount) {
            window.clearTimeout(timeout);
            observer.disconnect();
            resolve((marks.at(-1)?.startTime ?? performance.now()) - started);
          }
        });
        observer.observe({ entryTypes: ["mark"] });
        (element as HTMLElement).click();
      }),
    { markName, priorCount },
  );
}

async function waitForNewProjection(
  page: Page,
  application: Awaited<ReturnType<typeof installQualityApplication>>,
  priorOpenCount: number,
) {
  await expect.poll(() => application.graphOpenCount()).toBeGreaterThan(priorOpenCount);
  await expect(page.getByText("12 of 12 returned nodes")).toBeVisible();
  await expect(page.locator(".graph-viewport__canvas[data-lifecycle='ready'] canvas")).toBeVisible({
    timeout: 15_000,
  });
}

function summarize(samples: readonly number[]) {
  const sorted = [...samples].sort((left, right) => left - right);
  return {
    maximum: round(sorted.at(-1) ?? 0),
    p50: round(percentile(sorted, 0.5)),
    p95: round(percentile(sorted, 0.95)),
  };
}

function percentile(sorted: readonly number[], quantile: number) {
  if (sorted.length === 0) {
    return 0;
  }
  const index = Math.min(sorted.length - 1, Math.ceil(sorted.length * quantile) - 1);
  return sorted[index] ?? 0;
}

function round(value: number) {
  return Math.round(value * 100) / 100;
}

async function browserEnvironment(page: Page) {
  return page.evaluate(() => {
    const canvas = document.createElement("canvas");
    const gl = canvas.getContext("webgl2");
    const debug = gl?.getExtension("WEBGL_debug_renderer_info");
    return {
      hardwareConcurrency: navigator.hardwareConcurrency,
      deviceMemory:
        "deviceMemory" in navigator
          ? (navigator as Navigator & { deviceMemory: number }).deviceMemory
          : null,
      webglVendor:
        gl === null || debug === null || debug === undefined
          ? null
          : String(gl.getParameter(debug.UNMASKED_VENDOR_WEBGL)),
      webglRenderer:
        gl === null || debug === null || debug === undefined
          ? null
          : String(gl.getParameter(debug.UNMASKED_RENDERER_WEBGL)),
    };
  });
}
