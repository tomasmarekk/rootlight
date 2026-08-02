// Validates the dark authenticated shell and its initial accessibility baseline.

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

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
  await page.route("**/api/v1/session/bootstrap", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ csrfToken: "csrf", idleTtlSeconds: 1_800 }),
    });
  });
  await page.route("**/api/v1/health", async (route) => {
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(health) });
  });

  await page.goto(`/#bootstrap=${"a".repeat(43)}`);

  await expect(page).toHaveURL(/\/projects$/u);
  await expect(page.getByRole("heading", { name: "Projects", level: 1 })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Primary" })).toBeVisible();
  await expect(page.locator("html")).toHaveClass(/dark/u);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
});
