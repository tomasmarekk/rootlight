// Exercises the original renderer's transforms, bounded forces, and imperative lifecycle.
// A recording graphics context verifies uploaded data; browser tests independently compile shaders.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  FirstPartyGraph,
  GraphCamera,
  GraphLayout,
  type GraphConfig,
} from "../src/features/graph/controller/first-party-graph";

const frames = new Map<number, FrameRequestCallback>();
let nextFrame = 0;
let uploads: Float32Array[];

function graphicsFixture() {
  return {
    ARRAY_BUFFER: 34962,
    BLEND: 3042,
    COLOR_BUFFER_BIT: 16384,
    COMPILE_STATUS: 35713,
    DYNAMIC_DRAW: 35048,
    FLOAT: 5126,
    FRAGMENT_SHADER: 35632,
    LINK_STATUS: 35714,
    ONE_MINUS_SRC_ALPHA: 771,
    SRC_ALPHA: 770,
    TRIANGLES: 4,
    VERTEX_SHADER: 35633,
    createProgram: vi.fn(() => ({})),
    createShader: vi.fn(() => ({})),
    createBuffer: vi.fn(() => ({})),
    createVertexArray: vi.fn(() => ({})),
    shaderSource: vi.fn(),
    compileShader: vi.fn(),
    attachShader: vi.fn(),
    linkProgram: vi.fn(),
    getShaderParameter: vi.fn(() => true),
    getProgramParameter: vi.fn(() => true),
    deleteShader: vi.fn(),
    deleteProgram: vi.fn(),
    deleteBuffer: vi.fn(),
    deleteVertexArray: vi.fn(),
    getExtension: vi.fn(() => ({ loseContext: vi.fn() })),
    isContextLost: vi.fn(() => false),
    bindVertexArray: vi.fn(),
    bindBuffer: vi.fn(),
    enableVertexAttribArray: vi.fn(),
    vertexAttribPointer: vi.fn(),
    vertexAttribDivisor: vi.fn(),
    viewport: vi.fn(),
    clearColor: vi.fn(),
    clear: vi.fn(),
    enable: vi.fn(),
    disable: vi.fn(),
    blendFunc: vi.fn(),
    useProgram: vi.fn(),
    getUniformLocation: vi.fn(() => ({})),
    uniform3f: vi.fn(),
    uniform2f: vi.fn(),
    bufferData: vi.fn((_target: number, data: Float32Array) => {
      uploads.push(data.slice());
    }),
    drawArraysInstanced: vi.fn(),
  };
}

beforeEach(() => {
  frames.clear();
  nextFrame = 0;
  uploads = [];
  vi.spyOn(performance, "now").mockReturnValue(0);
  vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    right: 320,
    bottom: 240,
    width: 320,
    height: 240,
    toJSON: () => ({}),
  });
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frames.set(++nextFrame, callback);
    return nextFrame;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    frames.delete(id);
  });
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.replaceChildren();
});

function tick(timestamp = 16) {
  const pending = [...frames.values()];
  frames.clear();
  for (const callback of pending) callback(timestamp);
}

function renderer(config: GraphConfig = {}) {
  const gl = graphicsFixture();
  // Only the browser test can provide native GPU objects; this fixture records every graphics call.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
    gl as unknown as WebGL2RenderingContext,
  );
  const host = document.createElement("div");
  document.body.append(host);
  const graph = new FirstPartyGraph(host, { enableZoom: true, enableSimulation: true, ...config });
  const canvas = host.querySelector("canvas");
  if (canvas === null) throw new Error("Renderer canvas is missing");
  Object.defineProperty(canvas, "setPointerCapture", { value: vi.fn() });
  graph.setPointPositions(new Float32Array([-50, 0, 50, 0]), true);
  graph.setPointSizes(new Float32Array([12, 12]));
  graph.setPointColors(new Float32Array([1, 0, 0, 1, 0, 1, 0, 1]));
  graph.setPointShapes(new Float32Array([0, 1]));
  graph.setLinks(new Float32Array([0, 1]));
  graph.setLinkColors(new Float32Array([0, 0, 1, 1]));
  graph.setLinkWidths(new Float32Array([3]));
  graph.setLinkStyles(new Float32Array([1]));
  graph.setPointClusters([0, 0]);
  graph.setClusterPositions([0, 0]);
  graph.setPointClusterStrength(new Float32Array([0.28, 0.28]));
  graph.setZoomTransformByPointPositions(new Float32Array([-50, 0, 50, 0]), 0, 1, 0.1, false);
  tick();
  return { graph, gl, canvas, host };
}

function pointer(canvas: HTMLCanvasElement, type: string, x: number, y: number) {
  const event = new MouseEvent(type, { clientX: x, clientY: y, button: 0, bubbles: true });
  Object.defineProperty(event, "pointerId", { value: 1 });
  canvas.dispatchEvent(event);
}

describe("GraphCamera", () => {
  it("uses one invertible CSS transform and anchored bounded zoom", () => {
    const camera = new GraphCamera();
    camera.width = 320;
    camera.height = 240;
    camera.state = camera.fit(new Float32Array([-50, -20, 50, 20]), 0.1, 1);
    expect(camera.worldToScreen(-50, 0)).toEqual([110, 120]);
    expect(camera.screenToWorld(110, 120)).toEqual([-50, 0]);
    camera.zoom(-400, 110, 120);
    expect(camera.worldToScreen(-50, 0)).toEqual([110, 120]);
    expect(camera.state.scale).toBeCloseTo(Math.exp(0.4));
    for (let index = 0; index < 50; index += 1) camera.zoom(-10_000, 0, 0);
    expect(camera.state.scale).toBe(16);
    for (let index = 0; index < 50; index += 1) camera.zoom(10_000, 0, 0);
    expect(camera.state.scale).toBe(0.025);
  });
  it("retains an explicit center, handles empty inventory, and rejects invalid pairs", () => {
    const camera = new GraphCamera();
    camera.width = 320;
    camera.height = 240;
    camera.state = { x: 10, y: 20, scale: 2 };
    expect(camera.fit(new Float32Array(), 0.2)).toEqual(camera.state);
    expect(camera.fit(new Float32Array([-50, 0, 50, 0]), 0.1, 1, true)).toEqual({
      x: 10,
      y: 20,
      scale: 1,
    });
    expect(() => camera.fit(new Float32Array([1]), 0)).toThrow("incomplete");
    expect(() => camera.fit(new Float32Array([Infinity, 0]), 0)).toThrow("non-finite");
    expect(() => camera.fit(new Float32Array(100_002), 0)).toThrow("oversized");
  });
});

describe("GraphLayout", () => {
  it("is deterministic, copies immutable positions, and conserves bounded work", () => {
    const source = new Float32Array([-50, 0, 50, 0, 0, 20]);
    const a = new GraphLayout();
    const b = new GraphLayout();
    for (const layout of [a, b]) {
      layout.setPositions(source);
      layout.setLinks(new Float32Array([0, 1, 1, 2]));
      layout.clusters = [0, 0, 0];
      layout.centers = [0, 0];
      layout.strengths = new Float32Array([0.28, 0.28, 0.28]);
      for (let index = 0; index < 20; index += 1)
        expect(layout.step(0.65, {})).toBeLessThanOrEqual(256 + 128 * 17);
    }
    expect(a.positions).toEqual(b.positions);
    expect(a.positions).not.toEqual(source);
    expect([...source]).toEqual([-50, 0, 50, 0, 0, 20]);
    expect(new GraphLayout().step(1, {})).toBe(0);
    expect(() => a.setLinks(new Float32Array([0, 3]))).toThrow("outside");
    expect(() => a.setLinks(new Float32Array([0, 0.5]))).toThrow("outside");
  });
  it("visits late inventory and collision forces without an all-pairs expansion", () => {
    const source = new Float32Array(50_000 * 2).fill(100);
    const layout = new GraphLayout();
    layout.setPositions(source);
    const links = new Float32Array(150_000 * 2);
    for (let index = 0; index < links.length; index += 2) {
      links[index] = (index / 2) % 50_000;
      links[index + 1] = (index / 2 + 1) % 50_000;
    }
    layout.setLinks(links);
    for (let index = 0; index < 600; index += 1)
      expect(layout.step(0.65, { simulationCollision: 1 })).toBeLessThanOrEqual(256 + 128 * 17);
    expect(layout.positions[99_998]).not.toBe(100);
    expect(layout.positions.every(Number.isFinite)).toBe(true);
    expect(source.every((value) => value === 100)).toBe(true);
  });
});

describe("FirstPartyGraph", () => {
  it("uploads every point/link field, highlights and CSS-only interactive state", () => {
    const { graph, gl, canvas } = renderer({ backgroundColor: "#102030", linkBlending: false });
    graph.setConfigPartial({
      outlinedPointIndices: [1],
      highlightedPointIndices: [0],
      highlightedLinkIndices: [99],
    });
    graph.render();
    tick();
    const pointData = uploads.at(-1);
    const linkData = uploads.at(-2);
    expect(pointData?.length).toBe(20);
    expect(pointData?.[18]).toBe(1);
    expect(pointData?.[19]).toBeCloseTo(0.22);
    expect(linkData?.[10]).toBeCloseTo(0.15);
    expect(linkData?.[6]).toBeCloseTo(0.15);
    expect(linkData?.slice(0, 4)).toEqual(new Float32Array([-50, 0, 50, 0]));
    expect(gl.clearColor).toHaveBeenCalledWith(16 / 255, 32 / 255, 48 / 255, 1);
    expect(gl.disable).toHaveBeenCalledWith(gl.BLEND);
    expect(gl.drawArraysInstanced).toHaveBeenCalledWith(gl.TRIANGLES, 0, 6, 2);
    expect(canvas.hasAttribute("style")).toBe(false);
    graph.destroy();
  });
  it("picks and hovers the actual transformed canvas, including zoom and pan", () => {
    const click = vi.fn();
    const hover = vi.fn();
    const out = vi.fn();
    const interaction = vi.fn();
    const { graph, canvas } = renderer({
      onPointClick: click,
      onPointMouseOver: hover,
      onPointMouseOut: out,
      onZoomStart: interaction,
    });
    pointer(canvas, "pointermove", 110, 120);
    pointer(canvas, "pointermove", 110, 120);
    expect(hover).toHaveBeenCalledTimes(1);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 110, clientY: 120, ctrlKey: true }));
    expect(click.mock.calls[0]?.[0]).toBe(0);
    expect((click.mock.calls[0]?.[2] as MouseEvent).ctrlKey).toBe(true);
    canvas.dispatchEvent(
      new WheelEvent("wheel", { clientX: 110, clientY: 120, deltaY: -400, cancelable: true }),
    );
    tick();
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 259, clientY: 120 }));
    expect(click.mock.calls[1]?.[0]).toBe(1);
    pointer(canvas, "pointerdown", 20, 20);
    pointer(canvas, "pointermove", 50, 40);
    tick();
    pointer(canvas, "pointerup", 50, 40);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 50, clientY: 40 }));
    expect(click).toHaveBeenCalledTimes(2);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 140, clientY: 140 }));
    expect(click.mock.calls[2]?.[0]).toBe(0);
    pointer(canvas, "pointerleave", 400, 400);
    expect(out).toHaveBeenCalled();
    expect(interaction).toHaveBeenCalledTimes(2);
    graph.destroy();
  });
  it("stops layout during camera gestures and resumes after the bounded wheel lifetime", () => {
    const { graph, canvas } = renderer({ enableSimulationDuringZoom: false });
    graph.start(0.65);
    tick(16);
    const baseline = uploads.at(-1)?.slice();
    canvas.dispatchEvent(new WheelEvent("wheel", { clientX: 0, clientY: 0, deltaY: -100 }));
    tick(32);
    expect(uploads.at(-1)).toEqual(baseline);
    tick(140);
    expect(uploads.at(-1)).not.toEqual(baseline);
    pointer(canvas, "pointerdown", 20, 20);
    pointer(canvas, "pointermove", 50, 40);
    const beforePan = uploads.at(-1)?.slice();
    tick(160);
    expect(uploads.at(-1)).toEqual(beforePan);
    pointer(canvas, "pointerup", 50, 40);
    tick(180);
    expect(uploads.at(-1)).not.toEqual(beforePan);
    graph.destroy();
  });
  it("picks camera and inventory changes before the next draw rather than using stale cells", () => {
    const click = vi.fn();
    // Fine cells make each visible destination fall outside its previous candidate neighborhood.
    const { graph, canvas } = renderer({ onPointClick: click, pointSamplingDistance: 16 });
    canvas.dispatchEvent(new WheelEvent("wheel", { clientX: 0, clientY: 120, deltaY: -400 }));
    expect(frames.size).toBe(1);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 313, clientY: 120 }));
    expect(click.mock.calls[0]?.[0]).toBe(1);
    tick();
    graph.setConfigPartial({ transitionDuration: 0 });
    graph.setPointPositions(new Float32Array([15, 0, 50, 0]));
    graph.render();
    expect(frames.size).toBe(1);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 261, clientY: 120 }));
    expect(click.mock.calls[1]?.[0]).toBe(0);
    tick();
    graph.setZoomTransformByPointPositions(new Float32Array([15, 0]), 0, 1, 0.1, false);
    expect(frames.size).toBe(1);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 160, clientY: 120 }));
    expect(click.mock.calls[2]?.[0]).toBe(0);
    graph.destroy();
  });
  it("does not prematurely cool a larger admitted inventory at the small-inventory frame count", () => {
    const { graph } = renderer();
    graph.setPointPositions(new Float32Array(4_000).fill(100));
    graph.start(0.65);
    for (let frame = 0; frame < 160; frame += 1) tick(frame * 16);
    expect(frames.size).toBe(1);
    graph.stop();
    tick(2_600);
    expect(frames.size).toBe(0);
    graph.destroy();
  });
  it("pauses, stops, animates updates, and applies reduced-motion requests without resuming", () => {
    const { graph } = renderer({ transitionDuration: 100 });
    graph.start();
    tick(16);
    graph.pause();
    expect(frames.size).toBe(0);
    const baseline = uploads.at(-1)?.slice();
    graph.render();
    tick(32);
    expect(uploads.at(-1)).toEqual(baseline);
    expect(frames.size).toBe(0);
    graph.unpause();
    tick(48);
    expect(uploads.at(-1)).not.toEqual(baseline);
    graph.stop();
    tick(64);
    expect(frames.size).toBe(0);
    graph.setPointPositions(new Float32Array([-20, 0, 20, 0]), true);
    graph.render(0);
    tick(50);
    expect(uploads.at(-1)?.[0]).not.toBe(-20);
    tick(110);
    expect(uploads.at(-1)?.[0]).toBe(-20);
    graph.setPointPositions(new Float32Array([-10, 0, 10, 0]));
    graph.render(0, 0);
    tick(120);
    expect(uploads.at(-1)?.[0]).toBe(-10);
    expect(frames.size).toBe(0);
    graph.destroy();
  });
  it("animates a capped camera and retains center when requested", () => {
    const { graph, gl } = renderer();
    graph.fitView(100, 0.1, true);
    tick(50);
    expect(frames.size).toBe(1);
    tick(110);
    expect(frames.size).toBe(0);
    expect(gl.uniform3f.mock.calls.at(-1)?.slice(1, 3)).toEqual([0, 0]);
    expect(gl.uniform3f.mock.calls.at(-1)?.[3]).toBeCloseTo(2.56);
    graph.destroy();
  });
  it("disables zoom and releases pending frames, listeners and GPU resources exactly once", async () => {
    const click = vi.fn();
    const { graph, gl, canvas, host } = renderer({ enableZoom: false, onPointClick: click });
    await graph.ready;
    canvas.dispatchEvent(new WheelEvent("wheel", { deltaY: 100 }));
    pointer(canvas, "pointerdown", 20, 20);
    expect(frames.size).toBe(0);
    graph.start();
    graph.destroy();
    graph.destroy();
    expect(frames.size).toBe(0);
    expect(host.children).toHaveLength(0);
    expect(gl.deleteProgram).toHaveBeenCalledTimes(2);
    expect(gl.deleteBuffer).toHaveBeenCalledTimes(2);
    expect(gl.deleteVertexArray).toHaveBeenCalledTimes(2);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 110, clientY: 120 }));
    expect(click).not.toHaveBeenCalled();
    expect(() => graph.render()).toThrow("disposed");
  });
  it("handles context loss without keeping simulation frames alive", () => {
    const { graph, canvas, gl } = renderer();
    graph.start();
    const loss = new Event("webglcontextlost", { cancelable: true });
    canvas.dispatchEvent(loss);
    expect(loss.defaultPrevented).toBe(true);
    expect(frames.size).toBe(0);
    gl.isContextLost.mockReturnValue(true);
    graph.render();
    tick();
    expect(frames.size).toBe(0);
    graph.destroy();
  });
  it("rejects invalid arrays and releases partially allocated shader candidates", () => {
    const { graph, gl } = renderer();
    expect(() => graph.setPointColors(new Float32Array([NaN, 0, 0, 1]))).toThrow("non-finite");
    expect(() => graph.setLinks(new Float32Array([0, 9]))).toThrow("outside");
    graph.destroy();
    gl.getShaderParameter.mockReturnValue(false);
    expect(() => new FirstPartyGraph(document.createElement("div"), {})).toThrow("compilation");
    expect(gl.deleteShader).toHaveBeenCalledTimes(5);
    gl.getShaderParameter.mockReturnValue(true);
    gl.getProgramParameter.mockReturnValue(false);
    expect(() => new FirstPartyGraph(document.createElement("div"), {})).toThrow("linking");
  });
  it("renders admitted inventory with default visual channels and no implicit simulation", () => {
    const gl = graphicsFixture();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
      gl as unknown as WebGL2RenderingContext,
    );
    const host = document.createElement("div");
    const graph = new FirstPartyGraph(host, { backgroundColor: "invalid" });
    graph.setPointPositions(new Float32Array([0, 0, 20, 0]));
    graph.setLinks(new Float32Array([0, 1]));
    graph.start();
    tick();
    expect(frames.size).toBe(0);
    expect(gl.clearColor).toHaveBeenCalledWith(8 / 255, 10 / 255, 15 / 255, 1);
    expect(uploads.at(-1)?.slice(2, 8)).toEqual(new Float32Array([1, 1, 1, 1, 5, 0]));
    expect(uploads.at(-2)?.[8]).toBe(1);
    expect(uploads.at(-2)?.[9]).toBe(0);
    graph.destroy();
  });
  it("distinguishes diamond and square hit regions and rejects empty background clicks", () => {
    const click = vi.fn();
    const { graph, canvas } = renderer({ onPointClick: click });
    graph.setPointShapes(new Float32Array([3, 1]));
    graph.render();
    tick();
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 120, clientY: 130 }));
    expect(click).not.toHaveBeenCalled();
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 220, clientY: 130 }));
    expect(click.mock.calls[0]?.[0]).toBe(1);
    canvas.dispatchEvent(new MouseEvent("click", { clientX: 120, clientY: 120 }));
    expect(click.mock.calls[1]?.[0]).toBe(0);
    graph.destroy();
  });
  it("cleans allocation failures and rejects an unavailable WebGL2 context", () => {
    const gl = graphicsFixture();
    const context = vi
      .spyOn(HTMLCanvasElement.prototype, "getContext")
      .mockReturnValue(gl as unknown as WebGL2RenderingContext);
    gl.createBuffer.mockReturnValueOnce(null as unknown as WebGLBuffer);
    expect(() => new FirstPartyGraph(document.createElement("div"), {})).toThrow("unavailable");
    expect(gl.deleteProgram).toHaveBeenCalledTimes(2);
    gl.createShader.mockReturnValueOnce(null as unknown as WebGLShader);
    expect(() => new FirstPartyGraph(document.createElement("div"), {})).toThrow("unavailable");
    context.mockReturnValue(null);
    expect(() => new FirstPartyGraph(document.createElement("div"), {})).toThrow("WebGL2");
  });
});
