#!/bin/sh
set -eu

fail() { printf 'OpenCut Desktop: %s\n' "$*" >&2; exit 1; }

[ "$(/usr/bin/uname -s)" = Darwin ] || fail 'This command requires macOS.'
PATH="$PATH:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin"
export PATH
export OPENCUT_PLUGIN_DATA_DIR="${OPENCUT_PLUGIN_DATA_DIR:-$HOME/.opencut-plugin}"

installed_runtime() {
  config=$OPENCUT_PLUGIN_DATA_DIR/install.json
  [ -f "$config" ] || fail 'No OpenCut installation record. Install a platform release first.'
  runtime=$(/usr/bin/plutil -extract runtime raw -o - "$config" 2>/dev/null) || fail 'Cannot read the OpenCut installation record.'
  case $runtime in
    /*/runtime) ;;
    *) fail 'Invalid runtime path in the OpenCut installation record.' ;;
  esac
  installed_node=$runtime/bin/libexec/runtimes/node/bin/node
  scripts=${runtime%/runtime}/plugins/opencut/scripts
  [ -x "$installed_node" ] || fail 'The installed runtime is missing its executable Node. Reinstall the matching platform release.'
}

mode=${1:-}
case $mode in
  doctor|start|stop|check)
    [ "$#" -eq 1 ] || fail "Usage: sh install-desktop.sh $mode"
    installed_runtime
    case $mode in
      doctor)
        [ -f "$scripts/doctor.mjs" ] || fail 'The installed runtime is missing doctor.mjs. Reinstall the matching platform release.'
        exec "$installed_node" "$scripts/doctor.mjs" ;;
      start|stop) exec "$installed_node" "$scripts/web.mjs" "$mode" ;;
    esac
    exec "$installed_node" --input-type=module --eval '
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
const [script, config] = process.argv.slice(1);
const report = { ready: false };
try {
  report.version = JSON.parse(readFileSync(config, "utf8")).version;
  const status = spawnSync(process.execPath, [script, "status"], { encoding: "utf8", timeout: 70000 });
  if (status.error) throw status.error;
  const service = status.stdout && JSON.parse(status.stdout).data;
  report.service = service?.state ?? "unknown";
  if (!service?.ready || status.status !== 0) throw new Error(service?.diagnostic || status.stderr.trim() || "OpenCut service is not ready.");
  const endpoint = new URL(service.endpoint);
  if (endpoint.origin !== service.endpoint || endpoint.protocol !== "http:" || endpoint.hostname !== "127.0.0.1" || !endpoint.port)
    throw new Error("Expected the local OpenCut API endpoint.");
  const directory = path.dirname(config);
  const token = readFileSync(path.join(directory, "server", "api-token"), "utf8").trim();
  const response = await fetch(new URL("/api/projects?limit=1", endpoint), {
    headers: { Authorization: `Bearer ${token}` }, redirect: "error", signal: AbortSignal.timeout(5000),
  });
  const projects = await response.json();
  report.projects_readable = response.ok && !projects.error && Array.isArray(projects.data?.items);
  if (!report.projects_readable) throw new Error("The project list is unavailable.");
  const codex = spawnSync("codex", ["mcp", "list", "--json"], { encoding: "utf8", timeout: 15000 });
  if (codex.error || codex.status !== 0) throw new Error("Could not read the Codex MCP registry.");
  const entry = JSON.parse(codex.stdout).find(server => server.name === "opencut_desktop");
  report.mcp = !entry ? "missing" : entry.enabled !== true ? "disabled" :
    entry.transport?.type === "stdio" && entry.transport.command === path.resolve(directory, "opencut-mcp") &&
    !entry.transport.args?.length && !entry.transport.env_vars?.length && !entry.transport.cwd &&
    !Object.keys(entry.transport.env ?? {}).length ? "registered" : "conflict";
  if (report.mcp !== "registered") throw new Error(`Codex MCP registration: ${report.mcp}`);
  report.ready = true;
} catch (error) {
  report.error = error.message;
  process.exitCode = 1;
}
console.log(JSON.stringify(report, null, 2));
' "$scripts/web.mjs" "$config" ;;
  update) installed_runtime; shift ;;
  install) shift ;;
esac

if [ "$(/usr/sbin/sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
  target=aarch64-apple-darwin
else
  case $(/usr/bin/uname -m) in
    arm64) target=aarch64-apple-darwin ;;
    x86_64) target=x86_64-apple-darwin ;;
    *) fail 'No OpenCut release target for this CPU.' ;;
  esac
fi

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/opencut-desktop.XXXXXX")
trap '/bin/rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

case ${1:-} in
  --archive)
    [ "$#" -ge 2 ] || fail 'Pass the official ZIP path after --archive.'
    archive=$2
    case $archive in /*) ;; *) archive=$PWD/$archive ;; esac
    shift 2
    ;;
  v*)
    version=${1#v}
    case $version in ''|*[!0-9a-z.-]*) fail 'Pass an OpenCut release tag.' ;; esac
    shift
    name=opencut-plugin-$version-$target.zip
    url=https://github.com/miracle-ai-studio/OpenCut-Plugin/releases/download/v$version/$name
    archive=$work/$name
    for suffix in .sha256 ''; do
      /usr/bin/curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --output "$archive$suffix" "$url$suffix" || fail "Could not download $name$suffix. Check that v$version publishes a package for $target."
    done
    ;;
  *) fail 'Use install or update with a release tag or --archive ZIP, or run start, stop, check or doctor.' ;;
esac

register_codex=true
if [ "${1:-}" = --no-register-codex ]; then
  register_codex=false
  shift
fi

[ -f "$archive" ] && [ -f "$archive.sha256" ] || fail 'The ZIP and its official .sha256 companion are required.'
expected=$(/usr/bin/awk 'NR == 1 { print $1 }' < "$archive.sha256")
actual=$(/usr/bin/shasum -a 256 < "$archive" | /usr/bin/awk '{ print $1 }')
[ "${#expected}" -eq 64 ] && [ "$actual" = "$expected" ] || fail 'Release ZIP checksum mismatch.'
/usr/bin/unzip -q "$archive" -d "$work/unpacked"
plugin=$work/unpacked/opencut
release=$plugin/release.json
[ -f "$plugin/scripts/desktop-mcp.sh" ] || fail 'This package predates the OpenCut Desktop installation entry point.'
[ "$(/usr/bin/plutil -extract target raw -o - "$release")" = "$target" ] || fail "This Mac requires $target."
if [ "$mode" = update ]; then
  [ "$#" -eq 0 ] || fail 'Update preserves the current workspace and accepts only a tag or ZIP.'
  "$installed_node" --input-type=module --eval '
import { readFileSync } from "node:fs";
function version(file) {
  const value = JSON.parse(readFileSync(file, "utf8")).version;
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta|rc)(?:\.(\d+))?)?$/.exec(value);
  if (!match) throw new Error("Unrecognized OpenCut release version.");
  return [...match.slice(1, 4).map(Number), ["alpha", "beta", "rc", undefined].indexOf(match[4]), Number(match[5] ?? 0)];
}
try {
  const current = version(process.argv[1]);
  const next = version(process.argv[2]);
  const index = next.findIndex((part, i) => part !== current[i]);
  if (index >= 0 && next[index] < current[index]) throw new Error("Downgrades are disabled by update.");
} catch (error) {
  console.error(`OpenCut Desktop: ${error.message}`);
  process.exitCode = 1;
}
' "$config" "$release"
fi
top=$(/usr/bin/plutil -extract runtime_top_dir raw -o - "$release")
case $top in ''|.*|*[!a-zA-Z0-9._-]*) fail 'Invalid runtime archive root.' ;; esac

# The official ZIP checksum covers this bootstrap as well as the installer.
# Extract a single member. The JS installer validates and installs the full runtime.
node=$work/node
/usr/bin/tar -xOzf "$plugin/runtime.tar.gz" "$top/bin/libexec/runtimes/node/bin/node" > "$node"
/bin/chmod 700 "$node"
if [ "$register_codex" = true ]; then
  "$node" "$plugin/scripts/install.mjs" --register-codex "$@"
else
  "$node" "$plugin/scripts/install.mjs" "$@"
fi
