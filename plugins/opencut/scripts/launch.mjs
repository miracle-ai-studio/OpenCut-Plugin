import { spawn } from "node:child_process";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { readConfig, serviceConnection, verifyRuntime } from "./runtime.mjs";

export async function launchMcp(config) {
  verifyRuntime(config.runtime);
  const connection = serviceConnection(config);
  // Stdio carries MCP framing only. The existing API host owns SQLite and jobs.
  // Never start a Worker or fall back to a local database on a transport error.
  const child = spawn(
    connection.executable,
    [
      "--endpoint", connection.endpoint,
      "--credential-file", connection.tokenFile,
      "mcp", "serve",
    ],
    {
      stdio: "inherit",
      env: Object.fromEntries(Object.entries(process.env).filter(([key]) =>
        !key.startsWith("DYLD_") && !key.startsWith("OPENCUT_CLASSIC_") &&
        key !== "OPENCUT_SOURCE_ART_PACKAGE")),
    },
  );
  for (const signal of ["SIGINT", "SIGTERM"])
    process.on(signal, () => child.kill(signal));
  child.on("error", (error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
  child.on("exit", (code, signal) => {
    process.exitCode = code ?? (signal ? 1 : 0);
  });
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    await launchMcp(readConfig());
  } catch (error) {
    console.error(`OpenCut Plugin: ${error.message}`);
    process.exitCode = 1;
  }
}
