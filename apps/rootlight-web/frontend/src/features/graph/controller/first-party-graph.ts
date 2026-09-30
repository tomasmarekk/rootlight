// Owns Rootlight's bounded WebGL2 drawing, camera, picking, and local layout.
// Immutable model arrays never become simulation storage. The surrounding
// controller retains projection identity, context recovery, and lifecycle policy.

/** Controller configuration; positions and seed identity are prepared by graph-layout. */
export type GraphConfig = {
  attribution?: "";
  backgroundColor?: string;
  randomSeed?: string;
  fitViewOnInit?: false;
  enableDrag?: false;
  enableZoom?: boolean;
  enableSimulation?: boolean;
  enableSimulationDuringZoom?: boolean;
  rescalePositions?: false;
  transitionDuration?: number;
  simulationFriction?: number;
  simulationGravity?: number;
  simulationRepulsion?: number;
  simulationCollision?: number;
  simulationCollisionPadding?: number;
  linkBlending?: boolean;
  pointSamplingDistance?: number;
  outlinedPointIndices?: readonly number[];
  highlightedPointIndices?: readonly number[];
  highlightedLinkIndices?: readonly number[];
  onPointClick?: (index: number, position: readonly [number, number], event: MouseEvent) => void;
  onPointMouseOver?: (
    index: number,
    position: readonly [number, number],
    event: MouseEvent | undefined,
    dragging: boolean,
    zooming: boolean,
  ) => void;
  onPointMouseOut?: (event: MouseEvent | undefined) => void;
  onZoomStart?: (event: Event, user: boolean) => void;
  onDragStart?: (event: Event, user: boolean) => void;
};

// These admission bounds match the accumulator; rendering cannot enlarge a projection.
const MAX_POINTS = 50_000;
const MAX_LINKS = 150_000;
const POINT_STRIDE = 10;
const LINK_STRIDE = 13;
const LAYOUT_POINTS_PER_STEP = 128;
const LAYOUT_LINKS_PER_STEP = 256;
const REPULSION_SAMPLES = 16;

/** A world-space center and CSS-pixel scale, independent of device pixel ratio. */
export type GraphCameraState = { x: number; y: number; scale: number };

/** Keeps rendering, hit testing, and pointer-anchored zoom on the same transform. */
export class GraphCamera {
  state: GraphCameraState = { x: 0, y: 0, scale: 1 };
  width = 1;
  height = 1;

  /** Converts a world point into a CSS-pixel viewport position. */
  worldToScreen(x: number, y: number): readonly [number, number] {
    return [
      (x - this.state.x) * this.state.scale + this.width / 2,
      (y - this.state.y) * this.state.scale + this.height / 2,
    ];
  }

  /** Inverts the drawing transform for picking and pointer anchoring. */
  screenToWorld(x: number, y: number): readonly [number, number] {
    return [
      (x - this.width / 2) / this.state.scale + this.state.x,
      (y - this.height / 2) / this.state.scale + this.state.y,
    ];
  }

  /** Fits finite positions with a fractional viewport margin and optional zoom cap. */
  fit(
    positions: Float32Array,
    padding: number,
    maximumScale = 16,
    aroundCenter = false,
  ): GraphCameraState {
    validateArray(positions, MAX_POINTS * 2, 2);
    if (positions.length === 0) return { ...this.state };
    let left = Infinity;
    let right = -Infinity;
    let top = Infinity;
    let bottom = -Infinity;
    for (let index = 0; index < positions.length; index += 2) {
      const x = positions[index] ?? 0;
      const y = positions[index + 1] ?? 0;
      left = Math.min(left, x);
      right = Math.max(right, x);
      top = Math.min(top, y);
      bottom = Math.max(bottom, y);
    }
    const margin = clamp(padding, 0, 0.45);
    const x = aroundCenter ? this.state.x : (left + right) / 2;
    const y = aroundCenter ? this.state.y : (top + bottom) / 2;
    return {
      x,
      y,
      scale: clamp(
        Math.min(
          this.width / Math.max(32, 2 * Math.max(Math.abs(right - x), Math.abs(left - x))),
          this.height / Math.max(32, 2 * Math.max(Math.abs(bottom - y), Math.abs(top - y))),
        ) *
          (1 - margin * 2),
        0.025,
        clamp(maximumScale, 0.025, 16),
      ),
    };
  }

  /** Zooms without moving the world point under the pointer. */
  zoom(delta: number, x: number, y: number): void {
    const world = this.screenToWorld(x, y);
    this.state.scale = clamp(this.state.scale * Math.exp(clamp(-delta * 0.001, -1, 1)), 0.025, 16);
    this.state.x = world[0] - (x - this.width / 2) / this.state.scale;
    this.state.y = world[1] - (y - this.height / 2) / this.state.scale;
  }
}

/** Original deterministic local forces with fixed per-frame work rather than an all-pairs scan. */
export class GraphLayout {
  positions: Float32Array = new Float32Array();
  links: Float32Array = new Float32Array();
  clusters: readonly number[] = [];
  centers: readonly number[] = [];
  strengths: Float32Array = new Float32Array();
  sizes: Float32Array = new Float32Array();
  #velocity = new Float32Array();
  #pointCursor = 0;
  #linkCursor = 0;
  #round = 0;

  /** Copies positions so layout steps cannot mutate the accumulated model. */
  setPositions(positions: Float32Array): void {
    validateArray(positions, MAX_POINTS * 2, 2);
    this.positions = positions.slice();
    this.#velocity = new Float32Array(positions.length);
    this.#pointCursor = 0;
    this.#round = 0;
  }

  /** Admits complete index pairs, including parallel edges, before replacing links. */
  setLinks(links: Float32Array): void {
    validateArray(links, MAX_LINKS * 2, 2);
    const count = this.positions.length / 2;
    if (links.some((index) => !Number.isInteger(index) || index < 0 || index >= count))
      throw new Error("Graph link index is outside the model");
    this.links = links;
    this.#linkCursor = 0;
  }

  /** Applies bounded spring, repulsion, collision, gravity, and cluster forces. */
  step(alpha: number, config: GraphConfig): number {
    const count = this.positions.length / 2;
    if (count === 0) return 0;
    const strength = clamp(alpha, 0, 1);
    let operations = 0;
    const edgeCount = this.links.length / 2;
    for (let step = 0; step < Math.min(edgeCount, LAYOUT_LINKS_PER_STEP); step += 1) {
      const edge = (this.#linkCursor + step) % edgeCount;
      const from = (this.links[edge * 2] ?? 0) * 2;
      const to = (this.links[edge * 2 + 1] ?? 0) * 2;
      const dx = (this.positions[to] ?? 0) - (this.positions[from] ?? 0);
      const dy = (this.positions[to + 1] ?? 0) - (this.positions[from + 1] ?? 0);
      const distance = Math.max(1, Math.hypot(dx, dy));
      const force = clamp((distance - 80) * 0.002 * strength, -2, 2) / distance;
      this.#velocity[from] = (this.#velocity[from] ?? 0) + dx * force;
      this.#velocity[from + 1] = (this.#velocity[from + 1] ?? 0) + dy * force;
      this.#velocity[to] = (this.#velocity[to] ?? 0) - dx * force;
      this.#velocity[to + 1] = (this.#velocity[to + 1] ?? 0) - dy * force;
      operations += 1;
    }
    this.#linkCursor = edgeCount === 0 ? 0 : (this.#linkCursor + LAYOUT_LINKS_PER_STEP) % edgeCount;
    const points = Math.min(count, LAYOUT_POINTS_PER_STEP);
    for (let step = 0; step < points; step += 1) {
      const point = (this.#pointCursor + step) % count;
      const index = point * 2;
      const x = this.positions[index] ?? 0;
      const y = this.positions[index + 1] ?? 0;
      let fx = -x * (config.simulationGravity ?? 0.08) * 0.001 * strength;
      let fy = -y * (config.simulationGravity ?? 0.08) * 0.001 * strength;
      const cluster = this.clusters[point];
      if (cluster !== undefined && cluster >= 0) {
        const pull = (this.strengths[point] ?? 0) * 0.01 * strength;
        fx += ((this.centers[cluster * 2] ?? x) - x) * pull;
        fy += ((this.centers[cluster * 2 + 1] ?? y) - y) * pull;
      }
      for (let sample = 0; sample < Math.min(count, REPULSION_SAMPLES); sample += 1) {
        const other = (point + 1 + sample + this.#round * REPULSION_SAMPLES) % count;
        if (point === other) continue;
        let dx = x - (this.positions[other * 2] ?? 0);
        let dy = y - (this.positions[other * 2 + 1] ?? 0);
        if (dx === 0 && dy === 0) {
          dx = point < other ? -1 : 1;
          dy = 0.5;
        }
        const distance = Math.max(1, Math.hypot(dx, dy));
        const collisionDistance =
          (this.sizes[point] ?? 5) +
          (this.sizes[other] ?? 5) +
          (config.simulationCollisionPadding ?? 2);
        const repulsion =
          ((config.simulationRepulsion ?? 0.32) * strength * 20) / (distance * distance);
        const collision =
          (Math.max(0, collisionDistance - distance) *
            (config.simulationCollision ?? 0) *
            strength *
            0.02) /
          distance;
        fx += dx * (repulsion + collision);
        fy += dy * (repulsion + collision);
        operations += 1;
      }
      const damping = 1 - clamp(config.simulationFriction ?? 0.16, 0, 1);
      const vx = clamp(((this.#velocity[index] ?? 0) + fx) * damping, -4, 4);
      const vy = clamp(((this.#velocity[index + 1] ?? 0) + fy) * damping, -4, 4);
      this.#velocity[index] = vx;
      this.#velocity[index + 1] = vy;
      this.positions[index] = x + vx;
      this.positions[index + 1] = y + vy;
      operations += 1;
    }
    this.#pointCursor = (this.#pointCursor + points) % count;
    this.#round = (this.#round + 1) % count;
    return operations;
  }
}

type CameraAnimation = {
  from: GraphCameraState;
  to: GraphCameraState;
  start: number;
  duration: number;
};
type PointerDrag = { id: number; x: number; y: number; camera: GraphCameraState; moved: boolean };
type PositionAnimation = { from: Float32Array; to: Float32Array; start: number; duration: number };

/** First-party GPU renderer implementing only the application's imperative graph contract. */
export class FirstPartyGraph {
  readonly ready = Promise.resolve();
  readonly #canvas: HTMLCanvasElement;
  readonly #gl: WebGL2RenderingContext;
  readonly #camera = new GraphCamera();
  readonly #layout = new GraphLayout();
  #config: GraphConfig;
  #pointProgram: WebGLProgram | null = null;
  #linkProgram: WebGLProgram | null = null;
  #pointBuffer: WebGLBuffer | null = null;
  #linkBuffer: WebGLBuffer | null = null;
  #pointVao: WebGLVertexArrayObject | null = null;
  #linkVao: WebGLVertexArrayObject | null = null;
  #pointColors: Float32Array = new Float32Array();
  #pointSizes: Float32Array = new Float32Array();
  #pointShapes: Float32Array = new Float32Array();
  #linkColors: Float32Array = new Float32Array();
  #linkWidths: Float32Array = new Float32Array();
  #linkStyles: Float32Array = new Float32Array();
  #pointData = new Float32Array();
  #linkData = new Float32Array();
  #frame: number | null = null;
  #animation: CameraAnimation | null = null;
  #positionAnimation: PositionAnimation | null = null;
  readonly #pickingCells = new Map<string, number[]>();
  #pickingCellSize = 72;
  #drag: PointerDrag | null = null;
  #ignoreClick = false;
  #hovered: number | null = null;
  #destroyed = false;
  #paused = false;
  #running = false;
  #alpha = 0;
  #zoomUntil = 0;

  /** Allocates reviewed local shaders and one canvas; failures release all candidates. */
  constructor(container: HTMLDivElement, config: GraphConfig) {
    this.#config = { ...config };
    this.#canvas = document.createElement("canvas");
    this.#canvas.classList.add("rootlight-cosmos-canvas");
    this.#canvas.classList.toggle(
      "rootlight-cosmos-canvas--interactive",
      config.enableZoom === true,
    );
    const gl = this.#canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
    });
    if (gl === null) throw new Error("WebGL2 graph context is unavailable");
    this.#gl = gl;
    try {
      this.#pointProgram = createProgram(gl, POINT_VERTEX, POINT_FRAGMENT);
      this.#linkProgram = createProgram(gl, LINK_VERTEX, LINK_FRAGMENT);
      this.#pointBuffer = required(gl.createBuffer());
      this.#linkBuffer = required(gl.createBuffer());
      this.#pointVao = required(gl.createVertexArray());
      this.#linkVao = required(gl.createVertexArray());
      configureAttributes(gl, this.#pointVao, this.#pointBuffer, POINT_STRIDE, [2, 4, 1, 1, 1, 1]);
      configureAttributes(gl, this.#linkVao, this.#linkBuffer, LINK_STRIDE, [2, 2, 4, 1, 1, 1, 2]);
      this.#canvas.addEventListener("wheel", this.#wheel, { passive: false });
      this.#canvas.addEventListener("pointerdown", this.#pointerDown);
      this.#canvas.addEventListener("pointermove", this.#pointerMove);
      this.#canvas.addEventListener("pointerup", this.#pointerUp);
      this.#canvas.addEventListener("pointercancel", this.#pointerUp);
      this.#canvas.addEventListener("pointerleave", this.#pointerLeave);
      this.#canvas.addEventListener("click", this.#click);
      this.#canvas.addEventListener("webglcontextlost", this.#contextLost);
      container.append(this.#canvas);
    } catch (error) {
      this.destroy();
      throw error;
    }
  }

  /** Copies immutable point positions into private simulation storage. */
  setPointPositions(value: Float32Array, initial = false): void {
    this.#assertActive();
    const previous = this.#layout.positions;
    this.#layout.setPositions(value);
    this.#pickingCells.clear();
    const duration = clamp(this.#config.transitionDuration ?? 0, 0, 1_000);
    if (previous.length > 0 && duration > 0) {
      const from = value.slice();
      from.set(previous.subarray(0, Math.min(previous.length, from.length)));
      this.#positionAnimation = { from, to: value.slice(), start: performance.now(), duration };
      this.#layout.positions.set(from);
    } else this.#positionAnimation = null;
    if (initial) this.#hover(null);
  }
  /** Replaces finite RGBA values for every point. */
  setPointColors(value: Float32Array): void {
    validateArray(value, MAX_POINTS * 4, 4);
    this.#pointColors = value;
  }
  /** Replaces point radii in CSS pixels. */
  setPointSizes(value: Float32Array): void {
    validateArray(value, MAX_POINTS, 1);
    this.#pointSizes = value;
    this.#layout.sizes = value;
  }
  /** Replaces point shapes: circle, square, or generated diamond. */
  setPointShapes(value: Float32Array): void {
    validateArray(value, MAX_POINTS, 1);
    this.#pointShapes = value;
  }
  /** Replaces cluster ordinals without allocating an all-pairs structure. */
  setPointClusters(value: readonly number[]): void {
    validateNumbers(value, MAX_POINTS, 1);
    this.#layout.clusters = value;
  }
  /** Replaces finite world-space cluster centers. */
  setClusterPositions(value: readonly number[]): void {
    validateNumbers(value, MAX_POINTS * 2, 2);
    this.#layout.centers = value;
  }
  /** Replaces each point's cluster attraction strength. */
  setPointClusterStrength(value: Float32Array): void {
    validateArray(value, MAX_POINTS, 1);
    this.#layout.strengths = value;
  }
  /** Replaces complete, admitted point-index pairs. */
  setLinks(value: Float32Array): void {
    this.#assertActive();
    this.#layout.setLinks(value);
  }
  /** Replaces finite link RGBA colors. */
  setLinkColors(value: Float32Array): void {
    validateArray(value, MAX_LINKS * 4, 4);
    this.#linkColors = value;
  }
  /** Replaces link widths in CSS pixels. */
  setLinkWidths(value: Float32Array): void {
    validateArray(value, MAX_LINKS, 1);
    this.#linkWidths = value;
  }
  /** Replaces solid, dashed, or dotted link styles. */
  setLinkStyles(value: Float32Array): void {
    validateArray(value, MAX_LINKS, 1);
    this.#linkStyles = value;
  }
  /** Merges visual highlights and layout policy without resetting the camera. */
  setConfigPartial(value: Partial<GraphConfig>): void {
    this.#assertActive();
    this.#config = { ...this.#config, ...value };
  }

  /** Requests one draw without implicitly resuming a paused simulation. */
  render(alpha?: number, duration?: number): void {
    this.#assertActive();
    if (alpha !== undefined) this.#alpha = clamp(alpha, 0, 1);
    if (duration === 0 && this.#positionAnimation !== null) {
      this.#layout.positions.set(this.#positionAnimation.to);
      this.#positionAnimation = null;
    }
    this.#schedule();
  }
  /** Starts bounded deterministic layout; no timestamp or global random source changes positions. */
  start(alpha = 0.22): void {
    this.#assertActive();
    this.#alpha = clamp(alpha, 0, 1);
    this.#running = this.#config.enableSimulation === true;
    this.#schedule();
  }
  /** Stops layout while retaining a requested static draw or camera animation. */
  stop(): void {
    this.#running = false;
    this.#alpha = 0;
  }
  /** Suspends scheduled drawing and layout until an explicit update or unpause. */
  pause(): void {
    this.#paused = true;
    this.#cancelFrame();
  }
  /** Restores the previous layout status without resetting positions. */
  unpause(): void {
    this.#assertActive();
    this.#paused = false;
    this.#schedule();
  }
  /** Fits current simulated positions without restarting layout. */
  fitView(duration = 0, padding = 0.14, fitAroundCenter = false): void {
    this.setZoomTransformByPointPositions(
      this.#layout.positions,
      duration,
      undefined,
      padding,
      fitAroundCenter,
    );
  }
  /** Fits an explicit finite selection while preserving the renderer's point inventory. */
  setZoomTransformByPointPositions(
    positions: Float32Array,
    duration = 0,
    maximumScale?: number,
    padding = 0.2,
    fitAroundCenter = false,
  ): void {
    this.#assertActive();
    this.#resize();
    const target = this.#camera.fit(positions, padding, maximumScale, fitAroundCenter);
    this.#pickingCells.clear();
    if (duration <= 0) {
      this.#camera.state = target;
      this.#animation = null;
    } else
      this.#animation = {
        from: { ...this.#camera.state },
        to: target,
        start: performance.now(),
        duration: clamp(duration, 0, 1_000),
      };
    this.#schedule();
  }

  /** Releases listeners, frames, GPU objects, and the context exactly once. */
  destroy(): void {
    if (this.#destroyed) return;
    this.#destroyed = true;
    this.#cancelFrame();
    this.#animation = null;
    this.#positionAnimation = null;
    this.#pickingCells.clear();
    this.#drag = null;
    this.#canvas.removeEventListener("wheel", this.#wheel);
    this.#canvas.removeEventListener("pointerdown", this.#pointerDown);
    this.#canvas.removeEventListener("pointermove", this.#pointerMove);
    this.#canvas.removeEventListener("pointerup", this.#pointerUp);
    this.#canvas.removeEventListener("pointercancel", this.#pointerUp);
    this.#canvas.removeEventListener("pointerleave", this.#pointerLeave);
    this.#canvas.removeEventListener("click", this.#click);
    this.#canvas.removeEventListener("webglcontextlost", this.#contextLost);
    this.#gl.deleteBuffer(this.#pointBuffer);
    this.#gl.deleteBuffer(this.#linkBuffer);
    this.#gl.deleteVertexArray(this.#pointVao);
    this.#gl.deleteVertexArray(this.#linkVao);
    this.#gl.deleteProgram(this.#pointProgram);
    this.#gl.deleteProgram(this.#linkProgram);
    this.#gl.getExtension("WEBGL_lose_context")?.loseContext();
    this.#layout.setPositions(new Float32Array());
    this.#canvas.remove();
  }

  #assertActive() {
    if (this.#destroyed) throw new Error("Graph renderer is disposed");
  }
  #cancelFrame() {
    if (this.#frame !== null) cancelAnimationFrame(this.#frame);
    this.#frame = null;
  }
  #schedule() {
    if (!this.#destroyed && this.#frame === null)
      this.#frame = requestAnimationFrame(this.#drawFrame);
  }
  readonly #drawFrame = (timestamp: number) => {
    this.#frame = null;
    if (this.#destroyed || this.#gl.isContextLost()) return;
    this.#resize();
    if (this.#animation !== null) {
      const animation = this.#animation;
      const progress = clamp((timestamp - animation.start) / animation.duration, 0, 1);
      const weight = progress * progress * (3 - 2 * progress);
      this.#camera.state = {
        x: mix(animation.from.x, animation.to.x, weight),
        y: mix(animation.from.y, animation.to.y, weight),
        scale: mix(animation.from.scale, animation.to.scale, weight),
      };
      if (progress === 1) this.#animation = null;
    }
    const zooming = this.#drag?.moved === true || timestamp < this.#zoomUntil;
    if (this.#positionAnimation !== null && !this.#paused && !zooming) {
      const animation = this.#positionAnimation;
      const progress = clamp((timestamp - animation.start) / animation.duration, 0, 1);
      for (let index = 0; index < animation.to.length; index += 1) {
        this.#layout.positions[index] = mix(
          animation.from[index] ?? 0,
          animation.to[index] ?? 0,
          progress,
        );
      }
      if (progress === 1) this.#positionAnimation = null;
    }
    if (
      this.#running &&
      this.#positionAnimation === null &&
      !this.#paused &&
      (!zooming || this.#config.enableSimulationDuringZoom === true)
    ) {
      this.#layout.step(this.#alpha, this.#config);
      // Cool only after a complete bounded sweep; input order must not decide which points settle.
      this.#alpha *= Math.pow(
        0.96,
        Math.min(
          1,
          LAYOUT_POINTS_PER_STEP / Math.max(1, this.#layout.positions.length / 2),
          LAYOUT_LINKS_PER_STEP / Math.max(1, this.#layout.links.length / 2),
        ),
      );
      if (this.#alpha < 0.005) this.stop();
    }
    this.#draw();
    if (
      this.#animation !== null ||
      (!this.#paused && (this.#running || this.#positionAnimation !== null))
    )
      this.#schedule();
  };

  #resize() {
    const rectangle = this.#canvas.getBoundingClientRect();
    this.#camera.width = Math.max(1, rectangle.width);
    this.#camera.height = Math.max(1, rectangle.height);
    const ratio = Math.max(1, window.devicePixelRatio);
    const width = Math.round(this.#camera.width * ratio);
    const height = Math.round(this.#camera.height * ratio);
    if (this.#canvas.width !== width) this.#canvas.width = width;
    if (this.#canvas.height !== height) this.#canvas.height = height;
    this.#gl.viewport(0, 0, width, height);
  }

  #draw() {
    const gl = this.#gl;
    const points = this.#layout.positions.length / 2;
    const links = this.#layout.links.length / 2;
    const selected = new Set(this.#config.outlinedPointIndices);
    const highlighted = new Set(this.#config.highlightedPointIndices);
    const highlightedLinks = new Set(this.#config.highlightedLinkIndices);
    this.#pickingCellSize = clamp(this.#config.pointSamplingDistance ?? 72, 16, 256);
    this.#pickingCells.clear();
    if (this.#pointData.length !== points * POINT_STRIDE)
      this.#pointData = new Float32Array(points * POINT_STRIDE);
    if (this.#linkData.length !== links * LINK_STRIDE)
      this.#linkData = new Float32Array(links * LINK_STRIDE);
    for (let index = 0; index < points; index += 1) {
      const offset = index * POINT_STRIDE;
      this.#pointData[offset] = this.#layout.positions[index * 2] ?? 0;
      this.#pointData[offset + 1] = this.#layout.positions[index * 2 + 1] ?? 0;
      for (let channel = 0; channel < 4; channel += 1)
        this.#pointData[offset + 2 + channel] = this.#pointColors[index * 4 + channel] ?? 1;
      this.#pointData[offset + 6] = clamp(this.#pointSizes[index] ?? 5, 1, 64);
      this.#pointData[offset + 7] = this.#pointShapes[index] ?? 0;
      this.#pointData[offset + 8] = selected.has(index) ? 1 : 0;
      this.#pointData[offset + 9] = highlighted.size === 0 || highlighted.has(index) ? 1 : 0.22;
      const screen = this.#camera.worldToScreen(
        this.#pointData[offset] ?? 0,
        this.#pointData[offset + 1] ?? 0,
      );
      const key = this.#cellKey(screen[0], screen[1]);
      const cell = this.#pickingCells.get(key);
      if (cell === undefined) this.#pickingCells.set(key, [index]);
      else cell.push(index);
    }
    for (let index = 0; index < links; index += 1) {
      const offset = index * LINK_STRIDE;
      const from = (this.#layout.links[index * 2] ?? 0) * 2;
      const to = (this.#layout.links[index * 2 + 1] ?? 0) * 2;
      this.#linkData[offset] = this.#layout.positions[from] ?? 0;
      this.#linkData[offset + 1] = this.#layout.positions[from + 1] ?? 0;
      this.#linkData[offset + 2] = this.#layout.positions[to] ?? 0;
      this.#linkData[offset + 3] = this.#layout.positions[to + 1] ?? 0;
      for (let channel = 0; channel < 4; channel += 1)
        this.#linkData[offset + 4 + channel] = this.#linkColors[index * 4 + channel] ?? 0.6;
      this.#linkData[offset + 8] = clamp(this.#linkWidths[index] ?? 1, 0.25, 16);
      this.#linkData[offset + 9] = this.#linkStyles[index] ?? 0;
      this.#linkData[offset + 10] =
        highlightedLinks.size === 0 || highlightedLinks.has(index) ? 1 : 0.15;
      // Disabled blending cannot dim via alpha; retain the same highlighted subset in RGB.
      if (this.#config.linkBlending === false) {
        for (let channel = 0; channel < 3; channel += 1) {
          this.#linkData[offset + 4 + channel] =
            (this.#linkData[offset + 4 + channel] ?? 0) * (this.#linkData[offset + 10] ?? 1);
        }
      }
      this.#linkData[offset + 11] = 0;
      this.#linkData[offset + 12] = 0;
    }
    const background = /^#[0-9a-f]{6}$/iu.test(this.#config.backgroundColor ?? "")
      ? (this.#config.backgroundColor?.slice(1) ?? "080a0f")
      : "080a0f";
    gl.clearColor(
      Number.parseInt(background.slice(0, 2), 16) / 255,
      Number.parseInt(background.slice(2, 4), 16) / 255,
      Number.parseInt(background.slice(4, 6), 16) / 255,
      1,
    );
    gl.clear(gl.COLOR_BUFFER_BIT);
    if (this.#config.linkBlending === false) gl.disable(gl.BLEND);
    else gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    this.#drawInstances(this.#linkProgram, this.#linkVao, this.#linkBuffer, this.#linkData, links);
    gl.enable(gl.BLEND);
    this.#drawInstances(
      this.#pointProgram,
      this.#pointVao,
      this.#pointBuffer,
      this.#pointData,
      points,
    );
    gl.bindVertexArray(null);
  }

  #drawInstances(
    program: WebGLProgram | null,
    vao: WebGLVertexArrayObject | null,
    buffer: WebGLBuffer | null,
    data: Float32Array,
    count: number,
  ) {
    const gl = this.#gl;
    gl.useProgram(program);
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, data, gl.DYNAMIC_DRAW);
    const camera = this.#camera.state;
    gl.uniform3f(
      gl.getUniformLocation(required(program), "camera"),
      camera.x,
      camera.y,
      camera.scale,
    );
    gl.uniform2f(
      gl.getUniformLocation(required(program), "viewport"),
      this.#camera.width,
      this.#camera.height,
    );
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, count);
  }

  #position(event: MouseEvent): readonly [number, number] {
    const rectangle = this.#canvas.getBoundingClientRect();
    return [event.clientX - rectangle.left, event.clientY - rectangle.top];
  }
  #pick(x: number, y: number): number | null {
    let nearest: number | null = null;
    let distance = Infinity;
    const candidates: number[] = [];
    const reach = Math.ceil(67 / this.#pickingCellSize);
    const column = Math.floor(x / this.#pickingCellSize);
    const row = Math.floor(y / this.#pickingCellSize);
    if (this.#pickingCells.size === 0) {
      for (let index = 0; index < this.#layout.positions.length / 2; index += 1)
        candidates.push(index);
    }
    for (let dx = -reach; dx <= reach; dx += 1)
      for (let dy = -reach; dy <= reach; dy += 1) {
        const cell = this.#pickingCells.get(`${String(column + dx)},${String(row + dy)}`);
        if (cell !== undefined) candidates.push(...cell);
      }
    for (const index of candidates) {
      const screen = this.#camera.worldToScreen(
        this.#layout.positions[index * 2] ?? 0,
        this.#layout.positions[index * 2 + 1] ?? 0,
      );
      const dx = x - screen[0];
      const dy = y - screen[1];
      const radius = clamp(this.#pointSizes[index] ?? 5, 1, 64) + 3;
      const shape = this.#pointShapes[index] ?? 0;
      const inside =
        shape === 1
          ? Math.max(Math.abs(dx), Math.abs(dy)) <= radius
          : shape === 3
            ? Math.abs(dx) + Math.abs(dy) <= radius
            : dx * dx + dy * dy <= radius * radius;
      const squared = dx * dx + dy * dy;
      if (inside && squared < distance) {
        nearest = index;
        distance = squared;
      }
    }
    return nearest;
  }
  #cellKey(x: number, y: number) {
    return `${String(Math.floor(x / this.#pickingCellSize))},${String(Math.floor(y / this.#pickingCellSize))}`;
  }

  #hover(index: number | null, event?: MouseEvent) {
    if (index === this.#hovered) return;
    this.#hovered = index;
    this.#canvas.classList.toggle("rootlight-cosmos-canvas--point", index !== null);
    if (index === null) this.#config.onPointMouseOut?.(event);
    else
      this.#config.onPointMouseOver?.(
        index,
        [this.#layout.positions[index * 2] ?? 0, this.#layout.positions[index * 2 + 1] ?? 0],
        event,
        false,
        false,
      );
  }
  readonly #click = (event: MouseEvent) => {
    if (this.#ignoreClick) {
      this.#ignoreClick = false;
      return;
    }
    const position = this.#position(event);
    const index = this.#pick(...position);
    if (index !== null)
      this.#config.onPointClick?.(index, this.#camera.screenToWorld(...position), event);
  };
  readonly #wheel = (event: WheelEvent) => {
    if (this.#config.enableZoom !== true) return;
    event.preventDefault();
    this.#resize();
    this.#animation = null;
    this.#zoomUntil = performance.now() + 120;
    this.#config.onZoomStart?.(event, true);
    this.#hover(null);
    this.#camera.zoom(event.deltaY, ...this.#position(event));
    this.#pickingCells.clear();
    this.render();
  };
  readonly #pointerDown = (event: PointerEvent) => {
    if (event.button !== 0 || this.#config.enableZoom !== true) return;
    const position = this.#position(event);
    this.#drag = {
      id: event.pointerId,
      x: position[0],
      y: position[1],
      camera: { ...this.#camera.state },
      moved: false,
    };
    this.#canvas.setPointerCapture(event.pointerId);
  };
  readonly #pointerMove = (event: PointerEvent) => {
    const position = this.#position(event);
    const drag = this.#drag;
    if (drag !== null && event.pointerId === drag.id) {
      const dx = position[0] - drag.x;
      const dy = position[1] - drag.y;
      if (drag.moved || Math.hypot(dx, dy) > 4) {
        if (!drag.moved) {
          drag.moved = true;
          this.#animation = null;
          this.#config.onZoomStart?.(event, true);
          this.#hover(null);
        }
        this.#camera.state = {
          ...drag.camera,
          x: drag.camera.x - dx / drag.camera.scale,
          y: drag.camera.y - dy / drag.camera.scale,
        };
        this.#pickingCells.clear();
        this.#canvas.classList.add("rootlight-cosmos-canvas--grabbing");
        this.render();
        return;
      }
    }
    this.#hover(this.#pick(...position), event);
  };
  readonly #pointerUp = () => {
    this.#ignoreClick = this.#drag?.moved ?? false;
    this.#drag = null;
    this.#canvas.classList.remove("rootlight-cosmos-canvas--grabbing");
  };
  readonly #pointerLeave = (event: PointerEvent) => {
    this.#hover(null, event);
  };
  readonly #contextLost = (event: Event) => {
    event.preventDefault();
    this.pause();
  };
}

function validateNumbers(
  value: readonly number[] | Float32Array,
  maximum: number,
  multiple: number,
) {
  if (
    value.length > maximum ||
    value.length % multiple !== 0 ||
    value.some((number) => !Number.isFinite(number))
  )
    throw new Error("Graph array is incomplete, non-finite, or oversized");
}
function validateArray(value: Float32Array, maximum: number, multiple: number) {
  validateNumbers(value, maximum, multiple);
}
function clamp(value: number, minimum: number, maximum: number) {
  return Math.max(minimum, Math.min(maximum, Number.isFinite(value) ? value : minimum));
}
function mix(from: number, to: number, weight: number) {
  return from + (to - from) * weight;
}
function required<T>(value: T | null): T {
  if (value === null) throw new Error("Graph graphics resource is unavailable");
  return value;
}

function createProgram(gl: WebGL2RenderingContext, vertex: string, fragment: string): WebGLProgram {
  const shaders: WebGLShader[] = [];
  const program = required(gl.createProgram());
  try {
    for (const [kind, source] of [
      [gl.VERTEX_SHADER, vertex],
      [gl.FRAGMENT_SHADER, fragment],
    ] as const) {
      const shader = required(gl.createShader(kind));
      shaders.push(shader);
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (gl.getShaderParameter(shader, gl.COMPILE_STATUS) !== true)
        throw new Error("Graph shader compilation failed");
      gl.attachShader(program, shader);
    }
    gl.linkProgram(program);
    if (gl.getProgramParameter(program, gl.LINK_STATUS) !== true)
      throw new Error("Graph shader linking failed");
    return program;
  } catch (error) {
    gl.deleteProgram(program);
    throw error;
  } finally {
    for (const shader of shaders) gl.deleteShader(shader);
  }
}

function configureAttributes(
  gl: WebGL2RenderingContext,
  vao: WebGLVertexArrayObject,
  buffer: WebGLBuffer,
  stride: number,
  fields: readonly number[],
) {
  gl.bindVertexArray(vao);
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  let offset = 0;
  for (let index = 0; index < fields.length; index += 1) {
    const size = fields[index] ?? 1;
    gl.enableVertexAttribArray(index);
    gl.vertexAttribPointer(index, size, gl.FLOAT, false, stride * 4, offset * 4);
    gl.vertexAttribDivisor(index, 1);
    offset += size;
  }
  gl.bindVertexArray(null);
}

const QUAD = `const vec2 corner[6] = vec2[6](vec2(-1,-1),vec2(1,-1),vec2(1,1),vec2(-1,-1),vec2(1,1),vec2(-1,1));`;
const POINT_VERTEX = `#version 300 es
precision highp float;
layout(location=0) in vec2 position;
layout(location=1) in vec4 color;
layout(location=2) in float radius;
layout(location=3) in float shape;
layout(location=4) in float outline;
layout(location=5) in float highlight;
uniform vec3 camera;
uniform vec2 viewport;
out vec2 local;
out vec4 tint;
flat out float glyph;
flat out float border;
${QUAD}
void main(){local=corner[gl_VertexID];vec2 screen=(position-camera.xy)*camera.z+viewport*0.5+local*radius*(1.0+outline*0.3);gl_Position=vec4(screen.x/viewport.x*2.0-1.0,1.0-screen.y/viewport.y*2.0,0,1);tint=vec4(color.rgb,color.a*highlight);glyph=shape;border=outline;}`;
const POINT_FRAGMENT = `#version 300 es
precision highp float;
in vec2 local;
in vec4 tint;
flat in float glyph;
flat in float border;
out vec4 outputColor;
void main(){float extent=glyph==1.0?max(abs(local.x),abs(local.y)):glyph==3.0?abs(local.x)+abs(local.y):length(local);if(extent>1.0)discard;outputColor=border>0.5&&extent>0.72?vec4(0.93,0.96,1.0,1.0):tint;}`;
const LINK_VERTEX = `#version 300 es
precision highp float;
layout(location=0) in vec2 start;
layout(location=1) in vec2 end;
layout(location=2) in vec4 color;
layout(location=3) in float width;
layout(location=4) in float style;
layout(location=5) in float highlight;
uniform vec3 camera;
uniform vec2 viewport;
out vec4 tint;
out float along;
flat out float pattern;
${QUAD}
void main(){vec2 a=(start-camera.xy)*camera.z+viewport*0.5;vec2 b=(end-camera.xy)*camera.z+viewport*0.5;vec2 direction=b-a;float distance=max(length(direction),0.001);vec2 normal=vec2(-direction.y,direction.x)/distance;vec2 vertexCorner=corner[gl_VertexID];float fraction=(vertexCorner.x+1.0)*0.5;vec2 screen=mix(a,b,fraction)+normal*vertexCorner.y*width*0.5;gl_Position=vec4(screen.x/viewport.x*2.0-1.0,1.0-screen.y/viewport.y*2.0,0,1);tint=vec4(color.rgb,color.a*highlight);along=fraction*distance;pattern=style;}`;
const LINK_FRAGMENT = `#version 300 es
precision highp float;
in vec4 tint;
in float along;
flat in float pattern;
out vec4 outputColor;
void main(){if(pattern==1.0&&mod(along,12.0)>7.0)discard;if(pattern==2.0&&mod(along,5.0)>2.0)discard;outputColor=tint;}`;
