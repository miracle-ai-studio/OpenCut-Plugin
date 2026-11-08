#!/usr/bin/env python3
"""Validate the complete native release before publishing or retiring older releases."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = 'miracle-ai-studio/OpenCut-Plugin'
TARGETS = ('x86_64-apple-darwin', 'aarch64-apple-darwin')


def digest(file):
    with file.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def validate(directory, targets=TARGETS[:1]):
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    if json.loads((ROOT / 'plugins/opencut/plugin.json').read_text())['version'] != version:
        raise ValueError('Rust and plugin versions differ')
    files = []
    for target in targets:
        for prefix in ('opencut-plugin', 'OpenCut'):
            archive = directory / f'{prefix}-{version}-{target}.zip'
            checksum = archive.with_suffix('.zip.sha256')
            expected = checksum.read_text().split()
            if expected != [digest(archive), archive.name]:
                raise ValueError(f'Invalid or nonportable checksum: {checksum.name}')
            with zipfile.ZipFile(archive) as packed:
                if packed.testzip() is not None:
                    raise ValueError(f'Invalid ZIP: {archive.name}')
                if prefix == 'opencut-plugin':
                    release = json.loads(packed.read('opencut/release.json'))
                    if (release['version'], release['target']) != (version, target):
                        raise ValueError(f'Mismatched runtime: {archive.name}')
            files.extend((archive, checksum))
    installer = directory / 'install-desktop.sh'
    if installer.read_bytes() != (ROOT / 'scripts/install-desktop.sh').read_bytes():
        raise ValueError('Installer differs from the current source')
    files.append(installer)
    return version, files


def gh(*args):
    return subprocess.run(['gh', *args], check=True, capture_output=True, text=True).stdout


def release_inventory():
    pages = json.loads(gh('api', '--paginate', '--slurp', f'repos/{REPOSITORY}/releases?per_page=100'))
    return [release for page in pages for release in page]


def publish(directory, notes, commit, remove_previous=False, targets=TARGETS[:1]):
    version, files = validate(directory, targets)
    tag = 'v' + version
    # Fail before mutation if the complete existing release inventory is unavailable.
    releases = release_inventory()
    existing = next((r for r in releases if r['tag_name'] == tag), None)
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Publication requires the exact tested source commit')
    if existing and existing['target_commitish'] != commit:
        raise ValueError('Existing release belongs to a different source candidate')
    if existing is None:
        gh('release', 'create', tag, '--repo', REPOSITORY, '--target', commit, '--draft',
           '--prerelease', '--title', f'OpenCut {version}', '--notes-file', str(notes))
    identities = {file.name: (file.stat().st_size, 'sha256:' + digest(file)) for file in files}
    if existing is None or existing['draft']:
        retained = {a['name']: (a['size'], a.get('digest')) for a in (existing or {}).get('assets', [])}
        pending = [str(p) for p in files if retained.get(p.name) != identities[p.name]]
        if pending:
            gh('release', 'upload', tag, '--repo', REPOSITORY, '--clobber', *pending)
        # GitHub's tag lookup excludes drafts, even for a repository maintainer.
        uploaded = next(r for r in release_inventory() if r['tag_name'] == tag)
    else:
        uploaded = existing
    actual = {a['name']: a for a in uploaded['assets']}
    if set(actual) != {p.name for p in files}:
        raise ValueError('Uploaded release inventory differs; kept as draft')
    for file in files:
        if (actual[file.name]['size'], actual[file.name].get('digest')) != identities[file.name]:
            raise ValueError(f'Uploaded identity differs: {file.name}; kept as draft')
    if uploaded['draft']:
        gh('release', 'edit', tag, '--repo', REPOSITORY, '--draft=false', '--prerelease', '--notes-file', str(notes))
    published = json.loads(gh('api', f'repos/{REPOSITORY}/releases/tags/{tag}'))
    if published['draft'] or not published['published_at']:
        raise ValueError('New release publication was not confirmed; older releases retained')
    removed = []
    if remove_previous:
        for release in releases:
            old = release['tag_name']
            if old != tag and re.fullmatch(r'v0\.5\.0-beta\.\d+', old):
                gh('release', 'delete', old, '--repo', REPOSITORY, '--yes', '--cleanup-tag')
                removed.append(old)
    return {'version': version, 'published': published['html_url'], 'removed_releases': removed}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', required=True, type=Path)
    parser.add_argument('--publish', action='store_true')
    parser.add_argument('--include-apple-silicon', action='store_true', help='Require the additional ARM64 artifacts after validation on compatible hardware')
    parser.add_argument('--commit', help='Exact tested 40-character source commit required for publication')
    parser.add_argument('--notes', type=Path, default=ROOT / 'docs/release-notes.md')
    parser.add_argument('--remove-previous-releases', action='store_true')
    args = parser.parse_args()
    if args.remove_previous_releases and not args.publish:
        parser.error('--remove-previous-releases requires --publish')
    targets = TARGETS if args.include_apple_silicon else TARGETS[:1]
    if args.publish:
        if not args.commit:
            parser.error('--publish requires --commit')
        result = publish(args.directory, args.notes, args.commit, args.remove_previous_releases, targets)
    else:
        version, files = validate(args.directory, targets)
        result = {'version': version, 'validated': True, 'files': [p.name for p in files], 'published': False}
    print(json.dumps(result, indent=2))
