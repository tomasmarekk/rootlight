// Compiles the original shaders on a real WebGL2 canvas under the production-style CSP.
// The probe imports live first-party source; no fake port or HTML companion handles selection.

import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import { ModuleKind, ScriptTarget, transpileModule } from "typescript";

type RendererProbe = {
  clicks: number[];
  hovers: number[];
  interactions: number;
  lastWheelDelta: number;
  pixel: (x: number, y: number) => Promise<number[]>;
  highlight: () => void;
  simulate: () => void;
  stop: () => void;
  pause: () => void;
  resume: () => void;
  dispose: () => void;
  immutable: () => number[];
};

test.use({ viewport: { width: 400, height: 320 }, deviceScaleFactor: 2 });

async function openRenderer(page: Page) {
  const failures: string[] = [];
  page.on("pageerror", (error) => {
    failures.push(error.message);
  });
  page.on("console", (message) => {
    if (message.type() === "error") failures.push(message.text());
  });
  const source = readFileSync(
    new URL("../../src/features/graph/controller/first-party-graph.ts", import.meta.url),
    "utf8",
  );
  const module = transpileModule(source, {
    compilerOptions: { target: ScriptTarget.ES2023, module: ModuleKind.ESNext },
  }).outputText;
  const setup = `
const positions = new Float32Array([-50, 0, 50, 0]);
const clicks = []; const hovers = [];
const probe = { clicks, hovers, interactions: 0, lastWheelDelta: 0 };
const graph = new FirstPartyGraph(document.getElementById('graph'), {
 enableZoom: true, enableSimulation: true, enableSimulationDuringZoom: false,
 transitionDuration: 0, backgroundColor: '#080a0f', pointSamplingDistance: 72,
 onPointClick: (index) => clicks.push(index), onPointMouseOver: (index) => hovers.push(index),
 onPointMouseOut: () => hovers.push(-1), onZoomStart: (event) => { probe.interactions++; if (event instanceof WheelEvent) probe.lastWheelDelta = event.deltaY; }
});
graph.setPointPositions(positions, true);
graph.setPointColors(new Float32Array([1,0,0,1, 0,1,0,1]));
graph.setPointSizes(new Float32Array([12,12]));
graph.setPointShapes(new Float32Array([0,1]));
graph.setLinks(new Float32Array([0,1]));
graph.setLinkColors(new Float32Array([0,0,1,1]));
graph.setLinkWidths(new Float32Array([3])); graph.setLinkStyles(new Float32Array([0]));
graph.setZoomTransformByPointPositions(positions, 0, 1, 0.1, false);
Object.assign(probe, {
 pixel: (x,y) => new Promise((resolve) => {
   graph.render(); requestAnimationFrame(() => {
     const canvas=document.querySelector('canvas'); const gl=canvas.getContext('webgl2');
     const data=new Uint8Array(4); gl.readPixels(Math.round(x*devicePixelRatio), canvas.height-1-Math.round(y*devicePixelRatio), 1,1,gl.RGBA,gl.UNSIGNED_BYTE,data);
     if(gl.getError()!==gl.NO_ERROR) throw new Error('Renderer pixel read failed');
     resolve(Array.from(data));
   });
 }),
 highlight: () => { graph.setConfigPartial({outlinedPointIndices:[0],highlightedPointIndices:[0],highlightedLinkIndices:[0]}); graph.render(); },
 simulate: () => graph.start(0.65), stop: () => graph.stop(), pause: () => graph.pause(),
 resume: () => graph.unpause(), dispose: () => graph.destroy(), immutable: () => Array.from(positions)
});
window.rendererProbe=probe;
`;
  await page.route("**/renderer-probe/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("module.js")) {
      await route.fulfill({ contentType: "text/javascript", body: module + setup });
      return;
    }
    if (path.endsWith("style.css")) {
      await route.fulfill({
        contentType: "text/css",
        body: "body{margin:0}#graph{width:320px;height:240px}.rootlight-cosmos-canvas{display:block;width:100%;height:100%;touch-action:none}",
      });
      return;
    }
    await route.fulfill({
      contentType: "text/html",
      headers: {
        "Content-Security-Policy":
          "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; object-src 'none'",
      },
      body: '<!doctype html><html><head><link rel="stylesheet" href="/renderer-probe/style.css"><title>Renderer probe</title></head><body><div id="graph"></div><script type="module" src="/renderer-probe/module.js"></script></body></html>',
    });
  });
  await page.goto("/renderer-probe/");
  await expect(page.locator("canvas")).toBeVisible();
  await expect.poll(async () => page.evaluate(() => "rendererProbe" in window)).toBe(true);
  return failures;
}

test("draws native shader pixels, highlights, and picks transformed points with strict CSP", async ({
  page,
}) => {
  const failures = await openRenderer(page);
  expect(
    await page.evaluate(async () =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(110, 120),
    ),
  ).toEqual([255, 0, 0, 255]);
  expect(
    await page.evaluate(async () =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(210, 120),
    ),
  ).toEqual([0, 255, 0, 255]);
  expect(
    await page.evaluate(async () =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(160, 120),
    ),
  ).toEqual([0, 0, 255, 255]);
  expect(await page.locator("canvas").evaluate((canvas) => canvas.hasAttribute("style"))).toBe(
    false,
  );
  await page.mouse.move(110, 120);
  await page.mouse.click(110, 120);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.clicks,
      ),
    )
    .toEqual([0]);
  await page.mouse.wheel(0, -400);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.lastWheelDelta,
      ),
    )
    .toBeLessThan(0);
  const delta = await page.evaluate(
    () => (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.lastWheelDelta,
  );
  const secondPointX = 110 + 100 * Math.exp(-delta * 0.001);
  await page.evaluate(async () =>
    (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(110, 120),
  );
  await page.mouse.click(secondPointX, 120);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.clicks,
      ),
    )
    .toEqual([0, 1]);
  await page.mouse.move(20, 20);
  await page.mouse.down();
  await page.mouse.move(50, 40);
  await page.mouse.up();
  await page.evaluate(async () =>
    (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(140, 140),
  );
  await page.mouse.click(140, 140);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.clicks,
      ),
    )
    .toEqual([0, 1, 0]);
  await page.evaluate(() =>
    (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.highlight(),
  );
  const border = await page.evaluate(async () =>
    (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(153, 140),
  );
  expect(border[0]).toBeGreaterThan(230);
  expect(border[1]).toBeGreaterThan(230);
  expect(border[2]).toBe(255);
  const dimmed = await page.evaluate(
    async (x) =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(x, 140),
    secondPointX + 30,
  );
  expect(dimmed[1]).toBeLessThan(100);
  expect(
    await page.evaluate(() =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.immutable(),
    ),
  ).toEqual([-50, 0, 50, 0]);
  expect(failures).toEqual([]);
  await page.evaluate(() =>
    (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.dispose(),
  );
  await expect(page.locator("canvas")).toHaveCount(0);
});

test("renders paused and stopped simulation without mutating the input inventory", async ({
  page,
}) => {
  const failures = await openRenderer(page);
  await page.evaluate(() => {
    const probe = (window as unknown as { rendererProbe: RendererProbe }).rendererProbe;
    probe.simulate();
    probe.pause();
  });
  expect(
    await page.evaluate(async () =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.pixel(110, 120),
    ),
  ).toEqual([255, 0, 0, 255]);
  await page.evaluate(() => {
    const probe = (window as unknown as { rendererProbe: RendererProbe }).rendererProbe;
    probe.resume();
  });
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.stop();
            resolve();
          }),
        ),
      ),
  );
  expect(
    await page.evaluate(() =>
      (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.immutable(),
    ),
  ).toEqual([-50, 0, 50, 0]);
  expect(failures).toEqual([]);
  await page.evaluate(() =>
    (window as unknown as { rendererProbe: RendererProbe }).rendererProbe.dispose(),
  );
  await expect(page.locator("canvas")).toHaveCount(0);
});
