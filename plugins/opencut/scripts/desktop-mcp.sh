#!/bin/sh
set -eu

# Installed as opencut-mcp beside the atomic version selection record.
data=$(CDPATH='' cd -P -- "$(/usr/bin/dirname -- "$0")" && pwd)
config=$data/install.json
runtime=$(/usr/bin/plutil -extract runtime raw -o - "$config") || {
  echo 'OpenCut MCP could not read its installation record.' >&2
  exit 1
}
case $runtime in
  /*/runtime) ;;
  *) echo 'OpenCut MCP has an invalid installation record.' >&2; exit 1 ;;
esac
export OPENCUT_PLUGIN_DATA_DIR="$data"
exec "$runtime/bin/libexec/runtimes/node/bin/node" \
  "${runtime%/runtime}/plugins/opencut/scripts/launch.mjs"
