import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const nativePackages = new Map([
  ["darwin-arm64", "@tomasmarekk/rootlight-darwin-arm64"],
  ["darwin-x64", "@tomasmarekk/rootlight-darwin-x64"],
  ["linux-arm64", "@tomasmarekk/rootlight-linux-arm64-gnu"],
  ["linux-x64", "@tomasmarekk/rootlight-linux-x64-gnu"],
  ["win32-x64", "@tomasmarekk/rootlight-win32-x64-msvc"],
]);

const publicExecutables = new Set(["rootlight", "rootlight-mcp"]);

export function runNative(executable) {
  if (!publicExecutables.has(executable)) {
    console.error("rootlight: unsupported npm entry point");
    process.exit(1);
  }

  const platformKey = `${process.platform}-${process.arch}`;
  const nativePackage = nativePackages.get(platformKey);
  if (nativePackage === undefined) {
    console.error(
      `rootlight: unsupported npm platform ${process.platform}/${process.arch}`,
    );
    process.exit(1);
  }

  const require = createRequire(import.meta.url);
  let packageJson;
  try {
    packageJson = require.resolve(`${nativePackage}/package.json`);
  } catch {
    console.error(
      `rootlight: native package ${nativePackage} is unavailable; reinstall on a supported glibc, macOS, or Windows host`,
    );
    process.exit(1);
  }

  const suffix = process.platform === "win32" ? ".exe" : "";
  const result = spawnSync(
    join(dirname(packageJson), "bin", `${executable}${suffix}`),
    process.argv.slice(2),
    { stdio: "inherit" },
  );
  if (result.error !== undefined) {
    console.error(
      `rootlight: failed to start the native executable: ${result.error.message}`,
    );
    process.exit(1);
  }
  process.exit(result.status ?? 1);
}
