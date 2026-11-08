import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import {parseArgs} from "node:util";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { registerCodex } from "./codex.mjs";
import {
  digest,
  dataDirectory,
  verifyRuntime,
  runtimeEnvironment,
  workspaceDatabase,
  readConfig,
  serviceConnection,
} from "./runtime.mjs";

const source = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function web(runtime, action) {
  const script = path.join(path.dirname(runtime), "plugins/opencut/scripts/web.mjs");
  const result = spawnSync(process.execPath, [script, action], {
    encoding: "utf8",
    timeout: 70000,
    maxBuffer: 2 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  const response = result.stdout ? JSON.parse(result.stdout) : null;
  // Status returns 1 for a stopped or unhealthy service, with structured state.
  if (action === "status" && typeof response?.data?.state === "string" && !response.error)
    return response.data;
  if (result.status !== 0 || response?.error || !response?.data)
    throw new Error(response?.error?.message || result.stderr.trim() || `OpenCut service ${action} failed.`);
  return response.data;
}

function installDesktopCommand(data, destination) {
  const command = path.join(data, "opencut-mcp");
  const script = fs.readFileSync(path.join(destination, "plugins/opencut/scripts/desktop-mcp.sh"));
  const staging = fs.mkdtempSync(path.join(data, ".mcp-entry-"));
  const pending = path.join(staging, "opencut-mcp");
  try {
    fs.writeFileSync(pending, script, { mode: 0o700 });
    try {
      fs.linkSync(pending, command);
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const existing = fs.lstatSync(command);
      if (!existing.isFile() || !(existing.mode & 0o100) || existing.size !== script.length ||
          !fs.readFileSync(command).equals(script))
        throw new Error("The stable OpenCut MCP command already exists with different contents.");
    }
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }
  return command;
}

try {
  const { values } = parseArgs({
    options: { database: { type: "string" }, "register-codex": { type: "boolean" } },
    allowPositionals: false,
  });
  const data = dataDirectory();
  const installed = fs.existsSync(path.join(data, "install.json"));
  const previous = installed ? readConfig() : {};
  const initialDatabase = installed ? undefined :
    process.env.OPENCUT_PLUGIN_DATABASE ?? path.join(os.homedir(), ".opencut", "workspace.sqlite");
  const database = workspaceDatabase(previous, values.database ?? initialDatabase);
  if(!fs.existsSync(path.join(source,"release.json")))
    throw new Error("This is a source plugin, not an installable release bundle. Download the matching platform ZIP from GitHub Releases and run its scripts/install.mjs; see skills/setup/SKILL.md.");
  const release = JSON.parse(
    fs.readFileSync(path.join(source, "release.json"), "utf8"),
  );
  const archive = path.join(source, "runtime.tar.gz");
  if (digest(archive) !== release.runtime_archive_sha256)
    throw new Error("Runtime archive checksum differs");
  const prefix = path.resolve(
    process.env.OPENCUT_PLUGIN_INSTALL_DIR ||
      path.join(os.homedir(), "Library/Application Support/OpenCut Plugin"),
  );
  const destination = path.join(prefix, release.version);
  const runtime = path.join(destination, "runtime");
  if (installed && fs.existsSync(path.join(data, "server")) &&
      (previous.runtime !== runtime || workspaceDatabase(previous) !== database) &&
      web(previous.runtime, "status").state !== "stopped") {
    throw new Error("Stop the current OpenCut service from its original installation before switching runtime or workspace.");
  }
  fs.mkdirSync(prefix, { recursive: true, mode: 0o700 });
  if (!fs.existsSync(destination)) {
    const stage = fs.mkdtempSync(path.join(prefix, ".install-"));
    try {
      const unpack = path.join(stage, "unpack");
      fs.mkdirSync(unpack);
      const listing = spawnSync("tar", ["-tzf", archive], {
        encoding: "utf8",
        maxBuffer: 16 * 1024 * 1024,
      });
      if (
        listing.status !== 0 ||
        listing.stdout
          .split("\n")
          .filter(Boolean)
          .some(
            (name) =>
              !name.startsWith(release.runtime_top_dir + "/") ||
              name.split("/").includes(".."),
          )
      )
        throw new Error("Invalid runtime archive paths");
      const extracted = spawnSync("tar", ["-xzf", archive, "-C", unpack], {
        encoding: "utf8",
        maxBuffer: 1024 * 1024,
      });
      if (extracted.status !== 0) throw new Error("Runtime extraction failed");
      fs.renameSync(
        path.join(unpack, release.runtime_top_dir),
        path.join(stage, "runtime"),
      );
      fs.rmSync(unpack, { recursive: true });
      verifyRuntime(path.join(stage, "runtime"));
      await runtimeEnvironment(path.join(stage, "runtime"));
      const plugin = path.join(stage, "plugins/opencut");
      fs.mkdirSync(plugin, { recursive: true });
      for (const name of [
        "plugin.json",
        "mcp.json",
        "assets",
        "skills",
        "scripts",
        ".codex-plugin",
        ".mcp.json",
        "release.json",
        "LICENSE",
        "README.md",
        "README.zh-CN.md",
        "docs",
      ])
        if (fs.existsSync(path.join(source, name)))
          fs.cpSync(path.join(source, name), path.join(plugin, name), {
            recursive: true,
          });
      const marketplace = path.join(stage, ".agents/plugins");
      fs.mkdirSync(marketplace, { recursive: true });
      fs.writeFileSync(
        path.join(marketplace, "marketplace.json"),
        JSON.stringify(
          {
            name: "opencut-plugin",
            interface: { displayName: "OpenCut Plugin" },
            plugins: [
              {
                name: "opencut",
                source: { source: "local", path: "./plugins/opencut" },
                policy: {
                  installation: "AVAILABLE",
                  authentication: "ON_INSTALL",
                },
                category: "Creativity",
              },
            ],
          },
          null,
          2,
        ),
      );
      fs.renameSync(stage, destination);
    } catch (error) {
      fs.rmSync(stage, { recursive: true, force: true });
      throw error;
    }
  }
  const manifest = verifyRuntime(runtime);
  await runtimeEnvironment(runtime);
  if (manifest.binary_sha256 !== release.binary_sha256)
    throw new Error(
      "Existing installation belongs to another runtime candidate",
    );
  fs.mkdirSync(data, { recursive: true, mode: 0o700 });
  const desktopCommand = installDesktopCommand(data, destination);
  const config = {
    format: "opencut.plugin-install.v1",
    runtime,
    version: release.version,
    database,
  };
  const pending = path.join(data, `install.json.${process.pid}.tmp`);
  fs.writeFileSync(
    pending,
    JSON.stringify(config, null, 2),
    { mode: 0o600 },
  );
  fs.renameSync(pending, path.join(data, "install.json"));
  const desktopMcp = { name: "opencut_desktop", transport: "stdio", command: desktopCommand, status: "not_requested" };
  if (values["register-codex"]) {
    try {
      const service = web(runtime, "start");
      const connection = serviceConnection(config);
      if (service.ready !== true || service.endpoint !== connection.endpoint)
        throw new Error("The configured OpenCut API service is not ready.");
      desktopMcp.web_url = service.web_url;
      desktopMcp.status = registerCodex(desktopCommand);
    } catch (error) {
      desktopMcp.status = "setup_failed";
      desktopMcp.error = error.message;
      process.exitCode = 1;
    }
  }
  console.log(
    JSON.stringify(
      {
        installed: true,
        runtime,
        database,
        marketplace: destination,
        desktop_mcp: desktopMcp,
        command: "codex plugin marketplace add " + JSON.stringify(destination),
        next: values["register-codex"]
          ? "Check opencut_desktop in Codex MCP settings, then verify the project list in a new local task."
          : "For standalone MCP, start the installed service and add the reported desktop_mcp command in Codex, or rerun with --register-codex. The marketplace route is available for plugin hosts.",
      },
      null,
      2,
    ),
  );
} catch (error) {
  console.error(`OpenCut Plugin install: ${error.message}`);
  process.exitCode = 1;
}
