#!/usr/bin/env python3
"""Check files eligible for publication, without inspecting ignored workspaces or Git history."""

import re
import subprocess
import sys
from pathlib import Path


PRIVATE_SUFFIXES = {
    '.mp4', '.mov', '.m4v', '.mkv', '.webm', '.avi', '.m2ts',
    '.mp3', '.m4a', '.wav', '.flac', '.ogg', '.aac', '.aif', '.aiff',
    '.sqlite', '.sqlite3', '.db', '.log', '.opencut', '.pem', '.p12', '.pfx', '.zip', '.7z',
}
PRIVATE_NAMES = {'api-token', 'credentials.json', 'id_rsa', 'id_ed25519'}
PRIVATE_DIRECTORIES = {'.aws', '.ssh', '.opencut', '.codex', '.claude'}
PATTERNS = {
    'private key': re.compile(rb'-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----'),
    'GitHub token': re.compile(rb'\b(?:gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{60,255})\b'),
    'API secret': re.compile(rb'\b(?:sk-(?:proj-|ant-api\d+-)?[A-Za-z0-9_-]{40,255}|sk_live_[A-Za-z0-9]{24,255})\b'),
    'AWS access key': re.compile(rb'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b'),
    'Slack credential': re.compile(rb'\bxox[baprs]-[A-Za-z0-9-]{20,255}\b'),
    'personal home path': re.compile(rb'(?<![A-Za-z0-9_/.:])(?:file://)?/(?:Users|home)/(?!private(?:/|[\s"\']))[A-Za-z0-9_.-]+/'),
    'machine temporary path': re.compile(rb'/private/var/folders/[A-Za-z0-9_/.-]+'),
}


def inspect_file(relative, content):
    """Return redacted findings; never include matched secrets or file contents."""
    path = Path(relative)
    name = path.name.lower()
    findings = []
    database_sidecar = any(name.endswith(suffix + tail)
                           for suffix in ('.db', '.sqlite', '.sqlite3')
                           for tail in ('-wal', '-shm', '-journal'))
    private_env = name == '.env' or (name.startswith('.env.') and name != '.env.example')
    if (path.suffix.lower() in PRIVATE_SUFFIXES or name in PRIVATE_NAMES
            or PRIVATE_DIRECTORIES.intersection(path.parts) or database_sidecar or private_env):
        findings.append((0, 'private workspace/media/credential file'))
    if content.startswith(b'SQLite format 3\x00'):
        findings.append((0, 'SQLite database content'))
    # Approved source assets include font-atlas AVIFs and licensed vector artwork.
    # The synthetic username "private" is used in authorization rejection fixtures.
    matches = [(match.start(), label) for label, pattern in PATTERNS.items()
               for match in pattern.finditer(content)]
    # Binaries can contain thousands of compiler paths. Count each interval
    # once instead of rescanning the entire prefix for every finding.
    lines = {}
    previous, line = 0, 1
    for offset in sorted({offset for offset, _ in matches}):
        line += content.count(b'\n', previous, offset)
        lines[offset] = line
        previous = offset
    findings.extend((lines[offset], label) for offset, label in matches)
    return findings


def publication_files(root):
    result = subprocess.run(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'],
        cwd=root, check=True, stdout=subprocess.PIPE,
    )
    return sorted(set(result.stdout.decode('utf-8').split('\0')) - {''})


def main():
    root = Path(__file__).resolve().parent.parent
    checked = 0
    failed = False
    for relative in publication_files(root):
        source = root / relative
        if source.is_symlink():
            content = str(source.readlink()).encode('utf-8')
        elif source.is_file():
            content = source.read_bytes()
        else:
            continue  # Unstaged deletions remain in the index until committed.
        checked += 1
        for line, label in inspect_file(relative, content):
            failed = True
            print(f'{relative}:{line}: {label}')
    print(f'Source hygiene: {checked} publication files checked; '
          f'{"failed" if failed else "passed"}. History and ignored files are outside this check.')
    return int(failed)


if __name__ == '__main__':
    sys.exit(main())
