import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('plugin_package', Path(__file__).resolve().parents[1] / 'package-plugin.py')
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class PluginPackageTests(unittest.TestCase):
    def test_personal_plugin_keeps_launcher_and_requires_separate_runtime(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = package.package(None, Path(temporary))
            with zipfile.ZipFile(result['archive']) as archive:
                manifest = json.loads(archive.read('opencut/plugin.json'))
                compatibility = json.loads(archive.read('opencut/.codex-plugin/plugin.json'))
                self.assertTrue(manifest['extensions']['com.openai']['requires_local_executor'])
                self.assertTrue(compatibility['requires_local_executor'])
                self.assertEqual(manifest['version'], compatibility['version'])
                self.assertIn('opencut/scripts/launch.sh', archive.namelist())
                self.assertIn('opencut/skills/setup/SKILL.md', archive.namelist())
                self.assertNotIn('opencut/runtime.tar.gz', archive.namelist())
                self.assertNotIn('opencut/release.json', archive.namelist())
                distribution = json.loads(archive.read('opencut/distribution.json'))
                self.assertTrue(distribution['requires_runtime_install'])
                self.assertTrue(distribution['runtime_release_url'].endswith('/v' + manifest['version']))

    def test_upstream_exemptions_require_exact_bytes_and_do_not_hide_other_findings(self):
        name = 'bin/libexec/runtimes/node/bin/node'
        prefix = b'/Users/' + b'upstream-builder/'
        content = prefix + b'build/file.cc'
        with patch.dict(package.PUBLIC_UPSTREAM_FILES, {name: ({hashlib.sha256(content).hexdigest()}, prefix)}):
            self.assertFalse(package.publication_findings(name, content))
            self.assertTrue(package.publication_findings(name, content + b' changed'))
            self.assertTrue(package.publication_findings('bin/other', content))
        content += b'\n-----BEGIN ' + b'PRIVATE KEY-----'
        with patch.dict(package.PUBLIC_UPSTREAM_FILES, {name: ({hashlib.sha256(content).hexdigest()}, prefix)}):
            self.assertIn('private key', [label for _, label in package.publication_findings(name, content)])

    def fixture(self, archive, content=b'fixture executable', extra=None):
        digest = hashlib.sha256(content).hexdigest()
        manifest = {'target': 'x86_64-apple-darwin', 'binary_sha256': digest,
                    'external_dependencies': ['node', 'ffmpeg', 'ffprobe', 'chromium', 'python'],
                    'files': {'bin/opencut': {'size': len(content), 'sha256': digest}}, 'links': {}}
        with tarfile.open(archive, 'w:gz') as tar:
            for name, data in [('host/bin/opencut', content), ('host/manifest.json', json.dumps(manifest).encode()), *(extra or [])]:
                item = tarfile.TarInfo(name); item.size = len(data); tar.addfile(item, io.BytesIO(data))

    def test_portable_archive_and_non_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); runtime = root / 'runtime.tar.gz'; self.fixture(runtime)
            result = package.package(runtime, root / 'out')
            with zipfile.ZipFile(result['archive']) as archive:
                self.assertIn('opencut/plugin.json', archive.namelist())
                self.assertIn('opencut/mcp.json', archive.namelist())
                self.assertIn('opencut/README.zh-CN.md', archive.namelist())
                self.assertIn('opencut/runtime.tar.gz', archive.namelist())
                self.assertTrue(all(name.startswith('opencut/') for name in archive.namelist()))
            with self.assertRaises(FileExistsError): package.package(runtime, root / 'out')

    def test_private_binary_paths_and_traversal_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary) / 'runtime.tar.gz'
            self.fixture(runtime, b'/Users/' + b'fixture-owner/private.rs')
            with self.assertRaisesRegex(ValueError, 'hygiene'): package.inspect_runtime(runtime)
            self.fixture(runtime, extra=[('host/../escape', b'bad')])
            with self.assertRaisesRegex(ValueError, 'Unsafe'): package.inspect_runtime(runtime)

    def test_missing_inventory_member_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary) / 'runtime.tar.gz'
            self.fixture(runtime, extra=[('host/extra.txt', b'undeclared')])
            with self.assertRaisesRegex(ValueError, 'identity'): package.inspect_runtime(runtime)

    def test_public_dependency_template_does_not_hide_other_home_paths(self):
        name = 'bin/libexec/classic-audio-renderer/node_modules/playwright-core/lib/coreBundle.js'
        public = b'/home/' + b'pwuser/.cache/ms-playwright/webkit'
        self.assertFalse(package.publication_findings(name, public))
        self.assertTrue(package.publication_findings(name, public + b' /Users/' + b'fixture-owner/source.rs'))
