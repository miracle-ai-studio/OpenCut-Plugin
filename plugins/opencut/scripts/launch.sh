#!/bin/sh
set -eu
desktop=${OPENCUT_PLUGIN_DATA_DIR:-$HOME/.opencut-plugin}/opencut-mcp
if [ -x "$desktop" ]; then
  exec "$desktop"
fi
PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH
command -v node >/dev/null 2>&1 || {
  echo 'OpenCut Plugin requires Node.js 22.12+; install Node before starting the plugin.' >&2
  exit 1
}
exec node "$(dirname "$0")/launch.mjs"
