import path from "node:path";
import { spawnSync } from "node:child_process";
import { readConfig, dataDirectory, workspaceDatabase, runtimeEnvironment } from "./runtime.mjs";
try {
  const action = process.argv[2] || "start";
  if (!["start", "status", "stop"].includes(action))
    throw new Error("Use start, status or stop");
  const config = readConfig();
  const args = [
    "--database",
    workspaceDatabase(config),
    "--json",
    "server",
    "--directory",
    path.join(dataDirectory(), "server"),
    action,
  ];
  if (action === "start")
    args.push(
      "--listen",
      "127.0.0.1:0",
      "--web-root",
      path.join(config.runtime, "web"),
    );
  const result = spawnSync(path.join(config.runtime, "bin/opencut"), args, {
    env:
      action === "start"
        ? await runtimeEnvironment(config.runtime)
        : process.env,
    encoding: "utf8",
    timeout: 60000,
    maxBuffer: 2 * 1024 * 1024,
  });
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  process.exitCode = result.status ?? 1;
} catch (error) {
  console.error(`OpenCut Plugin Web: ${error.message}`);
  process.exitCode = 1;
}
