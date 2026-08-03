// Verifies the supported text-first path across Chromium, Firefox, and WebKit.
// WebGL is deliberately unavailable so engine compatibility does not depend on CI GPU drivers.

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

import {
  bootstrapUrl,
  expectPrimaryMarkupQuality,
  installQualityApplication,
  monitorBrowserQuality,
} from "./quality-fixtures";

test("keeps the keyboard fallback usable at 200% zoom equivalent", async ({ page }) => {
  await page.emulateMedia({ forcedColors: "active", reducedMotion: "reduce" });
  await page.addInitScript(() => {
    Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
      configurable: true,
      value: () => null,
    });
  });
  const quality = monitorBrowserQuality(page);
  await installQualityApplication(page, { edgeCount: 24, nodeCount: 12 });
  await page.goto(bootstrapUrl);

  const project = page.getByRole("link", { name: /Synthetic Atlas/u });
  await project.focus();
  await expect(project).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "Graphical view is unavailable" })).toBeVisible();
  await expect(page.getByText("12 of 12 returned nodes")).toBeVisible();
  await expectPrimaryMarkupQuality(page);

  const search = page.getByRole("searchbox", { name: "Search visible nodes" });
  await search.focus();
  await expect(search).toBeFocused();
  await page.keyboard.type("component-005");
  const node = page.getByRole("button", {
    name: /component-005 symbol synthetic\/module-005/u,
  });
  await node.focus();
  await expect(node).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(node).toHaveAttribute("aria-pressed", "true");

  const horizontalOverflow = await page.evaluate(() => {
    const viewportWidth = document.documentElement.clientWidth;
    return {
      documentWidth: document.documentElement.scrollWidth,
      viewportWidth,
      offenders: [...document.querySelectorAll<HTMLElement>("body *")]
        .map((element) => {
          const rect = element.getBoundingClientRect();
          return {
            className: element.className,
            right: Math.round(rect.right),
            tagName: element.tagName,
            width: Math.round(rect.width),
          };
        })
        .filter((element) => element.right > viewportWidth + 1 || element.width > viewportWidth + 1)
        .slice(0, 20),
    };
  });
  expect(horizontalOverflow).toEqual({
    documentWidth: horizontalOverflow.viewportWidth,
    viewportWidth: horizontalOverflow.viewportWidth,
    offenders: [],
  });
  expect(await page.evaluate(() => window.devicePixelRatio)).toBe(2);
  expect(await page.evaluate(() => window.innerWidth)).toBe(640);
  expect(await page.evaluate(() => matchMedia("(forced-colors: active)").matches)).toBe(true);
  expect(await page.evaluate(() => matchMedia("(prefers-reduced-motion: reduce)").matches)).toBe(
    true,
  );
  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(
    accessibility.violations.filter(
      (violation) => violation.impact === "serious" || violation.impact === "critical",
    ),
  ).toEqual([]);
  await quality.assertClean();
});
