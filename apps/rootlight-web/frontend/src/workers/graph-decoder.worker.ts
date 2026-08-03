// Runs graph validation and typed-array preparation away from the browser main thread.
// Cancelled jobs are dropped even when synchronous decoding finishes after route invalidation.

import {
  decodeGraphPage,
  graphPageTransferables,
  type GraphDecoderRequest,
  type GraphDecoderResponse,
} from "./graph-decoder-protocol";

type WorkerScope = {
  addEventListener(
    type: "message",
    listener: (event: MessageEvent<GraphDecoderRequest>) => void,
  ): void;
  postMessage(message: GraphDecoderResponse, transfer: Transferable[]): void;
};

const workerScope = globalThis as unknown as WorkerScope;
const cancelledJobs = new Set<number>();

workerScope.addEventListener("message", (event) => {
  const request = event.data;
  if (request.type === "cancel") {
    cancelledJobs.add(request.jobId);
    return;
  }
  try {
    performance.mark("rootlight.graph.worker.decode.start");
    const page = decodeGraphPage(request);
    performance.mark("rootlight.graph.worker.decode.end");
    if (cancelledJobs.delete(request.jobId)) {
      return;
    }
    workerScope.postMessage(
      { type: "decoded", jobId: request.jobId, page },
      graphPageTransferables(page),
    );
  } catch {
    if (cancelledJobs.delete(request.jobId)) {
      return;
    }
    workerScope.postMessage(
      {
        type: "error",
        jobId: request.jobId,
        code: "invalid_graph_page",
        message: "The graph page failed browser validation.",
      },
      [],
    );
  }
});
