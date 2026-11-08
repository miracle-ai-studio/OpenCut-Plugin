// Switch an existing desktop installation to the standard workspace. The old
// database remains on disk so a workspace change never deletes user content.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [archive, installer] = process.argv.slice(2);
const dataDirectory = process.env.OPENCUT_PLUGIN_DATA_DIR || path.join(os.homedir(), ".opencut-plugin");
const configPath = path.join(dataDirectory, "install.json");
const standard = path.join(os.homedir(), ".opencut", "workspace.sqlite");
const originalBytes = fs.readFileSync(configPath);
const config = JSON.parse(originalBytes);
if (config.format !== "opencut.plugin-install.v1" || !path.isAbsolute(config.runtime))
  throw new Error("Invalid OpenCut installation record");
const source = config.database || path.join(dataDirectory, "workspace.sqlite");
const root = path.dirname(config.runtime);
const cli = path.join(config.runtime, "bin", "opencut");
const service = path.join(root, "plugins", "opencut", "scripts", "web.mjs");

function invoke(executable, args, timeout = 120000) {
  const result = spawnSync(executable, args, { encoding: "utf8", timeout, maxBuffer: 16 * 1024 * 1024 });
  if (result.error || result.status !== 0)
    throw new Error(result.error?.message || (result.stderr + result.stdout).slice(-1000) || `${executable} failed`);
  return result.stdout;
}

function serviceCall(action) {
  const result = JSON.parse(invoke(process.execPath, [service, action], 90000));
  if (result.error || !result.data) throw new Error(result.error?.message || `Service ${action} failed`);
  return result.data;
}

function projects(database) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "opencut-list-"));
  try {
    const file = path.join(directory, "input.json");
    fs.writeFileSync(file, JSON.stringify({ cursor: null, limit: 100 }), { mode: 0o600 });
    const response = JSON.parse(invoke(cli, ["--json", "--database", database,
      "call", "opencut_project_list", "--input", file], 90000));
    if (response.error || !Array.isArray(response.data?.items))
      throw new Error(response.error?.message || "Cannot read the standard workspace project list");
    return response.data.items.length;
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

function restoreConfig() {
  const temporary = `${configPath}.${process.pid}.restore`;
  fs.writeFileSync(temporary, originalBytes, { mode: 0o600 });
  fs.renameSync(temporary, configPath);
}

let stopped = false;
try {
  // Confirm the existing standard database is readable before stopping the
  // currently running service or changing the installation record.
  if (!fs.existsSync(standard)) throw new Error("The standard workspace database does not exist");
  projects(standard);
  if (source !== standard) {
    serviceCall("stop"); stopped = true;
    invoke("/bin/sh", [installer, "install", "--archive", archive,
      "--no-register-codex", "--database", standard], 300000);
  }
  const current = JSON.parse(fs.readFileSync(configPath, "utf8"));
  if (current.database !== standard) throw new Error("Desktop configuration still selects the old database");
  const ready = serviceCall("start");
  if (ready.ready !== true) throw new Error("Standard workspace service did not become ready");
  const count = projects(standard);
  console.log(JSON.stringify({ ...ready, database: standard, project_count: count,
    codex_note: `标准工作区已启用；可见工程 ${count} 个。旧工作区数据库保留。` }));
} catch (error) {
  if (source !== standard && stopped) {
    try { restoreConfig(); serviceCall("start"); }
    catch (restore) { error.message += `; restoring previous service failed: ${restore.message}`; }
  }
  console.error(error.message);
  process.exitCode = 1;
}
