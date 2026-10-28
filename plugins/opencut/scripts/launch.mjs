import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { readConfig, dataDirectory, workspaceDatabase, runtimeEnvironment } from "./runtime.mjs";
try {
  const config = readConfig();
  const env = await runtimeEnvironment(config.runtime);
  const directory = dataDirectory();
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const child = spawn(
    path.join(config.runtime, "bin/opencut"),
    [
      "--database",
      workspaceDatabase(config),
      "mcp",
      "serve",
      "--with-worker",
    ],
    { stdio: "inherit", cwd: directory, env },
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
} catch (error) {
  console.error(`OpenCut Plugin: ${error.message}`);
  process.exitCode = 1;
}
