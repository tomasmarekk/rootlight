// Reads release inputs through one bounded descriptor so path replacement cannot
// redirect the content read after its identity and file type have been checked.

import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";

/**
 * Read a regular non-link file with a strict byte limit.
 * Reject links, identity changes, growth, truncation and non-regular inputs;
 * always close the descriptor, including when validation or reading fails.
 * @param {string} path Input path.
 * @param {number} maximum Maximum accepted file size in bytes.
 * @returns {Promise<Buffer>} The bytes read from the validated descriptor.
 * @throws {Error} If the limit, file identity, type or byte count is invalid.
 */
export async function readRegularFile(path, maximum) {
  if (!Number.isSafeInteger(maximum) || maximum < 0) {
    throw new Error("regular file byte limit is invalid");
  }
  let handle;
  try {
    // Nonblocking open prevents a substituted FIFO from waiting for a writer;
    // no path metadata observed before open is trusted for the descriptor.
    handle = await open(
      path,
      constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0),
    );
  } catch (error) {
    if (error.code === "ELOOP" || error.code === "EISDIR") {
      throw new Error("input is not a bounded regular file", { cause: error });
    }
    throw error;
  }
  try {
    const metadata = await handle.stat();
    if (
      !metadata.isFile() ||
      !Number.isSafeInteger(metadata.size) ||
      metadata.size < 0 ||
      metadata.size > maximum
    ) {
      throw new Error("input is not a bounded regular file");
    }
    // The path check also rejects links where O_NOFOLLOW is unavailable. All
    // subsequent reads remain bound to this descriptor if the path changes.
    const entry = await lstat(path);
    if (!entry.isFile() || entry.isSymbolicLink() || entry.size > maximum) {
      throw new Error("input is not a bounded regular file");
    }
    if (metadata.dev !== entry.dev || metadata.ino !== entry.ino || metadata.size !== entry.size) {
      throw new Error("input identity changed while it was opened");
    }
    const bytes = Buffer.alloc(metadata.size + 1);
    let offset = 0;
    while (offset < bytes.length) {
      const result = await handle.read(bytes, offset, bytes.length - offset, offset);
      if (result.bytesRead === 0) break;
      offset += result.bytesRead;
    }
    const after = await handle.stat();
    if (offset !== metadata.size || after.size !== metadata.size) {
      throw new Error("input size changed while it was read");
    }
    return bytes.subarray(0, offset);
  } finally {
    await handle.close();
  }
}
