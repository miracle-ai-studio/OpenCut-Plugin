#!/bin/sh
set -eu

if [ "$#" -ne 4 ]; then
  echo 'Usage: sh scripts/build-desktop-release.sh TARGET NATIVE_MEDIA_SPEC RUNTIME_SPEC OUTPUT' >&2
  exit 1
fi
target=$1
native_spec=$2
runtime_spec=$3
output=$4
python=${PYTHON:-python3}
"$python" -c 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ is required"'
"$python" - <<'PY'
import json, tomllib
from pathlib import Path
rust = tomllib.loads(Path('Cargo.toml').read_text())['workspace']['package']['version']
plugin = json.loads(Path('plugins/opencut/plugin.json').read_text())['version']
if rust != plugin:
    raise SystemExit('Rust and plugin release versions must match.')
PY
case $target in
  x86_64-apple-darwin|aarch64-apple-darwin) ;;
  *) echo "Unsupported desktop target: $target" >&2; exit 1 ;;
esac
host=$(rustc -vV | awk '/^host:/ {print $2}')
if [ "$host" != "$target" ] || [ "$(/usr/sbin/sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
  echo "Build $target on a native Mac with a matching Rust toolchain (found $host)." >&2
  exit 1
fi
native_root=$("$python" - "$target" "$native_spec" "$runtime_spec" <<'PY'
import json
import sys
from pathlib import Path

target, native, runtime = sys.argv[1:]
media = json.loads(Path(native).read_text())
tools = json.loads(Path(runtime).read_text())
if media.get('format') != 'opencut.native-media-input':
    sys.exit('Compiling Rust requires native media build inputs, including headers and pkg-config files.')
if media.get('target') != target or tools.get('target') != target:
    sys.exit(f'Both input specs must target {target}.')
if set(tools.get('components', {})) != {'ffmpeg', 'ffprobe', 'node', 'python'}:
    sys.exit('A desktop release requires FFmpeg, ffprobe, Node and Python.')
print(media['root'])
PY
)
work=$(mktemp -d "${TMPDIR:-/tmp}/opencut-desktop-build.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
separator=$(printf '\037')
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$work/target}"
export PKG_CONFIG_PATH="$native_root/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=/opencut-build/user${separator}--remap-path-prefix=$PWD=/opencut-build/source${separator}--remap-path-prefix=$work=/opencut-build/output${separator}--remap-path-prefix=$CARGO_TARGET_DIR=/opencut-build/target"
export CFLAGS="${CFLAGS:+$CFLAGS }-ffile-prefix-map=$HOME=/opencut-build/user"

npm ci --ignore-scripts --no-audit --no-fund --prefix packages/sdk-ts
cargo build --locked --release -p opencut-cli -p opencut-render-worker
node packages/sdk-ts/scripts/generate.mjs --check --skip-build --binary "$CARGO_TARGET_DIR/release/opencut"
"$python" scripts/check-canonical-contract.py --binary "$CARGO_TARGET_DIR/release/opencut"
"$python" scripts/package-local.py --binary "$CARGO_TARGET_DIR/release/opencut" \
  --render-worker "$CARGO_TARGET_DIR/release/opencut-render-worker" \
  --native-media-spec "$native_spec" --runtime-spec "$runtime_spec" --output "$work/runtime"
set -- "$work/runtime"/*-"$target".tar.gz
[ "$#" -eq 1 ] && [ -f "$1" ] || { echo 'Expected one native host archive.' >&2; exit 1; }
"$python" scripts/package-plugin.py --runtime-archive "$1" --output "$output"
cp scripts/install-desktop.sh "$output/"
