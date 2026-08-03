// Owns cancellable request correlation for the graph decoder Worker.
// Route changes reject pending jobs and terminate buffers without trusting late responses.

import type { GraphLayoutIdentity } from "../model/graph-layout";
import type { PreparedGraphPage } from "../model/graph-model";
import type {
  GraphDecoderRequest,
  GraphDecoderResponse,
} from "../../../workers/graph-decoder-protocol";

/** The subset of Worker used by the decoder client and its unit-test doubles. */
export type GraphDecoderWorkerPort = {
  postMessage(message: GraphDecoderRequest): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent<GraphDecoderResponse>) => void,
  ): void;
  removeEventListener(
    type: "message",
    listener: (event: MessageEvent<GraphDecoderResponse>) => void,
  ): void;
  terminate(): void;
};

/** Correlation inputs required before a browser page may allocate renderer arrays. */
export type GraphDecodeInput = {
  page: unknown;
  expectedRepositoryId: string;
  expectedGenerationId: string;
  expectedProjectionToken?: string;
  layoutIdentity: GraphLayoutIdentity;
};

type PendingJob = {
  resolve(page: PreparedGraphPage): void;
  reject(reason: Error): void;
  abortSignal?: AbortSignal;
  abortListener?: () => void;
};

/**
 * Correlates Worker decode responses and provides deterministic cancellation/disposal.
 */
export class GraphDecoderClient {
  readonly #worker: GraphDecoderWorkerPort;
  readonly #pendingJobs = new Map<number, PendingJob>();
  #nextJobId = 1;
  #disposed = false;

  /** Creates a decoder client using a lazy module Worker by default. */
  constructor(worker: GraphDecoderWorkerPort = createGraphDecoderWorker()) {
    this.#worker = worker;
    this.#worker.addEventListener("message", this.#handleMessage);
  }

  /**
   * Validates and prepares one graph page outside the main thread.
   *
   * @throws Error when the signal aborts, the Worker rejects the page, or the client is disposed.
   */
  decode(input: GraphDecodeInput, signal?: AbortSignal): Promise<PreparedGraphPage> {
    if (this.#disposed) {
      return Promise.reject(new Error("Graph decoder is disposed"));
    }
    if (signal?.aborted === true) {
      return Promise.reject(new DOMException("Graph decoding was aborted", "AbortError"));
    }
    const jobId = this.#nextJobId;
    this.#nextJobId += 1;
    return new Promise<PreparedGraphPage>((resolve, reject) => {
      const job: PendingJob = { resolve, reject, abortSignal: signal };
      if (signal !== undefined) {
        job.abortListener = () => {
          this.#worker.postMessage({ type: "cancel", jobId });
          this.#settleJob(jobId);
          reject(new DOMException("Graph decoding was aborted", "AbortError"));
        };
        signal.addEventListener("abort", job.abortListener, { once: true });
      }
      this.#pendingJobs.set(jobId, job);
      this.#worker.postMessage({ type: "decode", jobId, ...input });
    });
  }

  /** Terminates the Worker and rejects every pending decode. */
  dispose(): void {
    if (this.#disposed) {
      return;
    }
    this.#disposed = true;
    this.#worker.removeEventListener("message", this.#handleMessage);
    this.#worker.terminate();
    for (const [jobId, job] of this.#pendingJobs) {
      this.#settleJob(jobId);
      job.reject(new Error("Graph decoder was disposed"));
    }
  }

  readonly #handleMessage = (event: MessageEvent<GraphDecoderResponse>) => {
    const response = event.data;
    const job = this.#pendingJobs.get(response.jobId);
    if (job === undefined) {
      return;
    }
    this.#settleJob(response.jobId);
    if (response.type === "decoded") {
      job.resolve(response.page);
      return;
    }
    job.reject(new Error(response.message));
  };

  #settleJob(jobId: number) {
    const job = this.#pendingJobs.get(jobId);
    if (job?.abortSignal !== undefined && job.abortListener !== undefined) {
      job.abortSignal.removeEventListener("abort", job.abortListener);
    }
    this.#pendingJobs.delete(jobId);
  }
}

function createGraphDecoderWorker(): GraphDecoderWorkerPort {
  return new Worker(new URL("../../../workers/graph-decoder.worker.ts", import.meta.url), {
    type: "module",
    name: "rootlight-graph-decoder",
  });
}
