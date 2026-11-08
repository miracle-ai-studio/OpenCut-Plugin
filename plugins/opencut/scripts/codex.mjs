import { spawnSync } from "node:child_process";

const serverName = "opencut_desktop";

function codex(args) {
  const result = spawnSync("codex", args, {
    encoding: "utf8",
    timeout: 15000,
    maxBuffer: 2 * 1024 * 1024,
  });
  if (result.error?.code === "ENOENT")
    throw new Error("Codex CLI was not found on PATH. Add the local MCP command in Codex settings.");
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const denied = /permission denied|operation not permitted|access is denied/i.test(result.stderr);
    throw new Error(`Codex could not ${args[1]} the MCP configuration${denied ? ": permission denied" : ""}.`);
  }
  return result.stdout;
}

function lookup() {
  const servers = JSON.parse(codex(["mcp", "list", "--json"]));
  if (!Array.isArray(servers)) throw new Error("Unexpected Codex MCP configuration format.");
  return servers.find(server => server.name === serverName);
}

function status(server, command) {
  const transport = server.transport;
  if (transport?.type !== "stdio" || transport.command !== command ||
      transport.args?.length || transport.env_vars?.length ||
      Object.keys(transport.env ?? {}).length || transport.cwd) {
    throw new Error(`Codex already has a different ${serverName} entry. Review it in MCP settings.`);
  }
  return server.enabled === true ? "registered" : "disabled";
}

export function registerCodex(command) {
  const existing = lookup();
  if (existing) return status(existing, command);
  codex(["mcp", "add", serverName, "--", command]);
  const saved = lookup();
  if (!saved) throw new Error("Codex did not report the new MCP registration.");
  return status(saved, command);
}
