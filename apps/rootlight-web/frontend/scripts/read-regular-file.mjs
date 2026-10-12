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
  const before = await lstat(path);
  if (!before.isFile() || before.isSymbolicLink() || before.size > maximum) {
    throw new Error("input is not a bounded regular file");
  }
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const metadata = await handle.stat();
    if (
      !metadata.isFile() ||
      metadata.dev !== before.dev ||
      metadata.ino !== before.ino ||
      metadata.size !== before.size
    ) {
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
