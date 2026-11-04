import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { readConfig, dataDirectory, workspaceDatabase, runtimeEnvironment } from "./runtime.mjs";

function requireSelectedDatabase(record, selected) {
  if (record?.database !== undefined && workspaceDatabase(record) !== selected)
    throw new Error("The running Web service uses a different database. Stop the old service with its original configuration before starting the selected workspace.");
}

try {
  const action = process.argv[2] || "start";
  if (!["start", "status", "stop"].includes(action))
    throw new Error("Use start, status or stop");
  const config = readConfig();
  const database = workspaceDatabase(config);
  const directory = path.join(dataDirectory(), "server");
  if (action === "stop") {
    const stateFile = path.join(directory, "state.json");
    try {
      requireSelectedDatabase(JSON.parse(fs.readFileSync(stateFile, "utf8")), database);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  const args = [
    "--database",
    database,
    "--json",
    "server",
    "--directory",
    directory,
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
  if (result.error) throw result.error;
  if (result.stdout && action !== "stop")
    requireSelectedDatabase(JSON.parse(result.stdout).data, database);
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  process.exitCode = result.status ?? 1;
} catch (error) {
  console.error(`OpenCut Plugin Web: ${error.message}`);
  process.exitCode = 1;
}
