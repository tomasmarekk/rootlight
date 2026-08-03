// Runs browser acceptance tests against deterministic assets and the production CSP.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  forbidOnly: true,
  retries: 0,
  timeout: 30_000,
  use: {
    baseURL: "http://127.0.0.1:4173",
    colorScheme: "dark",
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "npm run build && node tests/e2e/serve-dist.mjs",
    port: 4173,
    reuseExistingServer: false,
    timeout: 45_000,
  },
  projects: [
    {
      name: "chromium",
      testIgnore: /browser-compatibility\.spec\.ts/u,
      use: {
        ...devices["Desktop Chrome"],
        launchOptions: {
          // Chromium requires an explicit software WebGL opt-in on headless CI runners.
          args: ["--enable-unsafe-swiftshader", "--use-angle=swiftshader"],
        },
      },
    },
    {
      name: "chromium-fallback",
      testMatch: /browser-compatibility\.spec\.ts/u,
      use: {
        ...devices["Desktop Chrome"],
        deviceScaleFactor: 2,
        viewport: { width: 640, height: 450 },
      },
    },
    {
      name: "firefox-fallback",
      testMatch: /browser-compatibility\.spec\.ts/u,
      use: {
        ...devices["Desktop Firefox"],
        deviceScaleFactor: 2,
        launchOptions: {
          // Firefox desktop ignores Playwright's deviceScaleFactor without its native scale pref.
          firefoxUserPrefs: {
            "layout.css.devPixelsPerPx": "2.0",
          },
        },
        viewport: { width: 640, height: 450 },
      },
    },
    {
      name: "webkit-fallback",
      testMatch: /browser-compatibility\.spec\.ts/u,
      use: {
        ...devices["Desktop Safari"],
        deviceScaleFactor: 2,
        viewport: { width: 640, height: 450 },
      },
    },
  ],
});
