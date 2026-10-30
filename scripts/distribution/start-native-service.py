#!/usr/bin/env python3
"""Verify a pinned local installation and assemble its existing service command.

Explicit tool paths avoid whole-installation discovery on every preview frame.
The host retains its normal source/runtime identity and authorization checks.
This is deployment assembly, not another service or business implementation.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def environment(root, expected_manifest):
    root = root.resolve(strict=True)
    manifest_path = root / 'manifest.json'
    if digest(manifest_path) != expected_manifest:
        raise ValueError('Installation manifest differs from the pinned identity')
    manifest = json.loads(manifest_path.read_text())
    # Verify all declared bytes before executing even the packaged validators.
    for name, expected in manifest['files'].items():
        path = root / name
        if (Path(name).is_absolute() or '..' in Path(name).parts
                or path.is_symlink() or not path.resolve(strict=True).is_relative_to(root)
                or path.stat().st_size != expected['size']
                or digest(path) != expected['sha256']):
            raise ValueError('Installation file differs: ' + name)

    def module(name):
        spec = importlib.util.spec_from_file_location(name, root / (name + '.py'))
        value = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(value)
        return value

    module('native_runtime').verify(root, manifest['native_renderer'])
    bundle = module('runtime_bundle').verify(root / 'bin/libexec')
    if bundle['format'] != 'opencut.native-runtime-bundle':
        raise ValueError('A native runtime installation is required')
    variables = {'ffmpeg': 'OPENCUT_FFMPEG', 'ffprobe': 'OPENCUT_FFPROBE',
                 'python': 'OPENCUT_FONT_DERIVE_PYTHON',
                 'node': 'OPENCUT_CLASSIC_VISUAL_NODE'}
    result = {key: value for key, value in os.environ.items()
              if not key.startswith(('OPENCUT_CLASSIC_', 'DYLD_'))
              and key not in (*variables.values(), 'OPENCUT_RENDER_WORKER')}
    for component, variable in variables.items():
        # Missing declared components fail; there is no PATH or browser fallback.
        result[variable] = str(root / 'bin/libexec' / bundle['components'][component]['entry'])
    result['OPENCUT_RENDER_WORKER'] = str(root / 'bin/opencut-render-worker')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--manifest-sha256', required=True)
    parser.add_argument('--database', type=Path, required=True)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--listen', default='127.0.0.1:8788')
    args = parser.parse_args()
    root = args.runtime.resolve(strict=True)
    env = environment(root, args.manifest_sha256)
    config = args.directory / 'config.json'
    if config.exists():
        value = json.loads(config.read_text())
        web = Path(value['web_root'])
        web = web if web.is_absolute() else config.parent / web
        if web.resolve(strict=True) != root / 'web':
            raise ValueError('Service configuration must use the same installation Web')
    return subprocess.call([
        str(root / 'bin/opencut'), '--json', '--database', str(args.database.resolve()),
        'server', '--directory', str(args.directory.resolve()), 'start',
        '--listen', args.listen, '--web-root', str(root / 'web'),
    ], env=env)


if __name__ == '__main__':
    raise SystemExit(main())
