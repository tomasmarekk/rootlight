// Verifies Worker job correlation, abort handling, late-response rejection, and disposal.

import { describe, expect, it, vi } from "vitest";

import {
  GraphDecoderClient,
  type GraphDecoderWorkerPort,
} from "../src/features/graph/controller/graph-decoder-client";
import type {
  GraphDecoderRequest,
  GraphDecoderResponse,
} from "../src/workers/graph-decoder-protocol";
import { decodeGraphPage } from "../src/workers/graph-decoder-protocol";
import {
  graphGenerationId,
  graphLayoutIdentity,
  graphPageFixture,
  graphRepositoryId,
} from "./graph-engine-fixtures";

describe("GraphDecoderClient", () => {
  it("correlates a decoded page and ignores an unrelated Worker response", async () => {
    const worker = new FakeWorker();
    const client = new GraphDecoderClient(worker);
    const result = client.decode(decodeInput());
    const request = worker.messages[0];
    if (request?.type !== "decode") {
      throw new Error("Decode request was not posted");
    }
    const page = decodeGraphPage(request);

    worker.emit({ type: "decoded", jobId: request.jobId + 10, page });
    worker.emit({ type: "decoded", jobId: request.jobId, page });

    await expect(result).resolves.toBe(page);
    client.dispose();
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it("posts cancellation, rejects abort, and ignores a late response", async () => {
    const worker = new FakeWorker();
    const client = new GraphDecoderClient(worker);
    const abort = new AbortController();
    const result = client.decode(decodeInput(), abort.signal);
    const request = worker.messages[0];
    if (request?.type !== "decode") {
      throw new Error("Decode request was not posted");
    }

    abort.abort();

    await expect(result).rejects.toMatchObject({ name: "AbortError" });
    expect(worker.messages[1]).toEqual({ type: "cancel", jobId: request.jobId });
    worker.emit({
      type: "error",
      jobId: request.jobId,
      code: "worker_failure",
      message: "late",
    });
    client.dispose();
  });

  it("rejects Worker errors, pre-aborted requests, pending disposal, and later reuse", async () => {
    const worker = new FakeWorker();
    const client = new GraphDecoderClient(worker);
    const failed = client.decode(decodeInput());
    const request = worker.messages[0];
    if (request?.type !== "decode") {
      throw new Error("Decode request was not posted");
    }
    worker.emit({
      type: "error",
      jobId: request.jobId,
      code: "invalid_graph_page",
      message: "The graph page failed browser validation.",
    });
    await expect(failed).rejects.toThrow("failed browser validation");

    const abort = new AbortController();
    abort.abort();
    await expect(client.decode(decodeInput(), abort.signal)).rejects.toMatchObject({
      name: "AbortError",
    });

    const pending = client.decode(decodeInput());
    client.dispose();
    await expect(pending).rejects.toThrow("disposed");
    await expect(client.decode(decodeInput())).rejects.toThrow("disposed");
  });
});

class FakeWorker implements GraphDecoderWorkerPort {
  readonly messages: GraphDecoderRequest[] = [];
  readonly terminate = vi.fn();
  readonly #listeners = new Set<(event: MessageEvent<GraphDecoderResponse>) => void>();

  postMessage(message: GraphDecoderRequest) {
    this.messages.push(message);
  }

  addEventListener(
    _type: "message",
    listener: (event: MessageEvent<GraphDecoderResponse>) => void,
  ) {
    this.#listeners.add(listener);
  }

  removeEventListener(
    _type: "message",
    listener: (event: MessageEvent<GraphDecoderResponse>) => void,
  ) {
    this.#listeners.delete(listener);
  }

  emit(response: GraphDecoderResponse) {
    const event = new MessageEvent<GraphDecoderResponse>("message", { data: response });
    for (const listener of this.#listeners) {
      listener(event);
    }
  }
}

function decodeInput() {
  return {
    page: graphPageFixture(0),
    expectedRepositoryId: graphRepositoryId,
    expectedGenerationId: graphGenerationId,
    layoutIdentity: graphLayoutIdentity,
  };
}
