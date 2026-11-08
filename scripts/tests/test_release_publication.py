import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('publication', Path(__file__).resolve().parents[1] / 'publish-release.py')
publication = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publication)


class ReleasePublicationTests(unittest.TestCase):
    def fixture(self, root):
        version = json.loads((publication.ROOT / 'plugins/opencut/plugin.json').read_text())['version']
        for target in publication.TARGETS:
            for prefix in ('opencut-plugin', 'OpenCut'):
                archive = root / f'{prefix}-{version}-{target}.zip'
                with zipfile.ZipFile(archive, 'w') as packed:
                    packed.writestr('opencut/release.json', json.dumps({'version': version, 'target': target}))
                archive.with_suffix('.zip.sha256').write_text(publication.digest(archive) + '  ' + archive.name + '\n')
        (root / 'install-desktop.sh').write_bytes((publication.ROOT / 'scripts/install-desktop.sh').read_bytes())
        return version

    def test_missing_platform_blocks_all_remote_mutations(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(publication, 'gh') as gh:
            with self.assertRaises(FileNotFoundError):
                publication.publish(Path(folder), Path('notes'), 'a' * 40, True)
            gh.assert_not_called()

    def test_checksums_are_portable_and_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            version = self.fixture(root)
            self.assertEqual(publication.validate(root)[0], version)
            archive = next(root.glob('*x86_64-apple-darwin.zip'))
            checksum = archive.with_suffix('.zip.sha256')
            checksum.write_text(publication.digest(archive) + '  ' + str(archive) + '\n')
            with self.assertRaisesRegex(ValueError, 'nonportable'):
                publication.validate(root)
            checksum.write_text('0' * 64 + '  ' + archive.name + '\n')
            with self.assertRaises(ValueError):
                publication.validate(root)

    def test_failed_upload_keeps_old_releases(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.fixture(root)
            calls = []
            def gh(*args):
                calls.append(args)
                if args[:2] == ('api', '--paginate'):
                    return json.dumps([[{'tag_name': 'v0.5.0-beta.6'}]])
                if args[:2] == ('release', 'upload'):
                    raise RuntimeError('network failure')
                return '{}'
            with patch.object(publication, 'gh', gh), self.assertRaisesRegex(RuntimeError, 'network'):
                publication.publish(root, Path('notes'), 'a' * 40, True)
            self.assertFalse(any(call[:2] == ('release', 'delete') for call in calls))

    def test_published_bytes_verified_before_cleanup_and_cleanup_is_resumable(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            version = self.fixture(root)
            _, files = publication.validate(root)
            new = {'tag_name': 'v' + version, 'target_commitish': 'a' * 40, 'draft': False,
                   'published_at': '2026-11-08', 'html_url': 'https://github.com/example/release',
                   'assets': [{'name': p.name, 'size': p.stat().st_size, 'digest': 'sha256:' + publication.digest(p)} for p in files]}
            calls = []
            def gh(*args):
                calls.append(args)
                if args[:2] == ('api', '--paginate'):
                    return json.dumps([[new, {'tag_name': 'v0.5.0-beta.6'}, {'tag_name': 'unrelated'}]])
                return json.dumps(new)
            with patch.object(publication, 'gh', gh):
                result = publication.publish(root, Path('notes'), 'a' * 40, True)
            self.assertEqual(result['removed_releases'], ['v0.5.0-beta.6'])
            self.assertFalse(any(call[:2] == ('release', 'upload') for call in calls))
            new['assets'][0]['digest'] = 'sha256:' + '0' * 64
            calls.clear()
            with patch.object(publication, 'gh', gh), self.assertRaisesRegex(ValueError, 'identity'):
                publication.publish(root, Path('notes'), 'a' * 40, True)
            self.assertFalse(any(call[:2] == ('release', 'delete') for call in calls))

    def test_resume_verified_draft_without_tag_lookup_or_reupload(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            version = self.fixture(root)
            _, files = publication.validate(root)
            draft = {'tag_name': 'v' + version, 'target_commitish': 'a' * 40, 'draft': True,
                     'assets': [{'name': p.name, 'size': p.stat().st_size,
                                 'digest': 'sha256:' + publication.digest(p)} for p in files]}
            def gh(*args):
                if args[:2] == ('api', '--paginate'):
                    return json.dumps([[draft]])
                if args[:2] == ('release', 'edit'):
                    draft.update(draft=False, published_at='2026-11-08',
                                 html_url='https://github.com/example/release')
                    return ''
                if args[0] == 'api' and not draft['draft']:
                    return json.dumps(draft)
                self.fail(f'Unexpected GitHub call: {args}')
            with patch.object(publication, 'gh', gh):
                result = publication.publish(root, Path('notes'), 'a' * 40)
            self.assertEqual(result['published'], 'https://github.com/example/release')

    def test_installer_accepts_bare_beta_and_still_refuses_downgrades(self):
        installer = (publication.ROOT / 'scripts/install-desktop.sh').read_text()
        start = installer.index('import { readFileSync } from "node:fs";\nfunction version(file)')
        end = installer.index("' \"$config\" \"$release\"", start)
        code = installer[start:end]
        with tempfile.TemporaryDirectory() as folder:
            current, staged = Path(folder) / 'current.json', Path(folder) / 'release.json'
            for before, after, success in [
                ('0.0.1-beta', '0.0.1-beta', True),
                ('0.0.1-beta', '0.0.1-beta.1', True),
                ('0.0.1-beta.1', '0.0.1-beta', False),
                ('0.0.1-beta', '0.0.1', True),
                ('0.5.0-beta.7', '0.0.1-beta', False),
                ('0.0.1-beta', 'invalid', False),
            ]:
                current.write_text(json.dumps({'version': before}))
                staged.write_text(json.dumps({'version': after}))
                result = subprocess.run(['node', '--input-type=module', '--eval', code, str(current), str(staged)], capture_output=True)
                self.assertEqual(result.returncode == 0, success, (before, after, result.stderr))

    def test_deferred_arm64_does_not_block_intel_but_explicit_arm64_is_required(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            version = self.fixture(root)
            for artifact in root.glob('*aarch64*'):
                artifact.unlink()
            self.assertEqual(publication.validate(root)[0], version)
            with self.assertRaises(FileNotFoundError):
                publication.validate(root, publication.TARGETS)
