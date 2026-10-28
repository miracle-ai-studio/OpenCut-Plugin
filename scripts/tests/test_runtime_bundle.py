"""Runtime delivery tests use synthetic files, never installed executables."""
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import stat
import struct
import tarfile
import tempfile
import unittest
from unittest.mock import patch


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SCRIPTS = Path(__file__).resolve().parents[1]
runtime = load('runtime_bundle_tests', SCRIPTS / 'distribution/runtime_bundle.py')
package = load('package_bundle_tests', SCRIPTS / 'package-local.py')
doctor = load('doctor_bundle_tests', SCRIPTS / 'distribution/doctor.py')


def macho(libraries=(), rpaths=(), executable=True):
    commands = []
    for command, value in [(0xc, v) for v in libraries] + [(0x8000001c, v) for v in rpaths]:
        start = 24 if command == 0xc else 12
        size = ((start + len(value.encode()) + 1 + 7) // 8) * 8
        commands.append(struct.pack('<III', command, size, start) + b'\0' * (start - 12)
                        + value.encode() + b'\0' * (size - start - len(value.encode())))
    data = b''.join(commands)
    return struct.pack('<IIIIIIII', 0xfeedfacf, 0x1000007, 3, 2 if executable else 6, len(commands), len(data), 0, 0) + data


class RuntimeBundleTest(unittest.TestCase):
    def write(self, path, data=b'fixture', mode=0o644):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(mode)
        return path

    def repository(self, root):
        repository = root / 'repo'
        self.write(repository / 'LICENSE', b'project license')
        for name in ('classic-audio-renderer', 'classic-visual-renderer', 'classic-source-art-renderer'):
            package_root = repository / 'packages' / name
            self.write(package_root / 'package.json', json.dumps({'name': '@opencut/' + name, 'version': '0.1.0', 'type': 'module'}).encode())
            self.write(package_root / 'host/browser.mjs', b'never copied')
            self.write(package_root / 'src/index.mjs', b'never copied')
            self.write(package_root / 'node_modules/playwright-core/index.mjs', b'never copied')
        for name in runtime.SCRIPTS:
            self.write(repository / 'scripts/font-catalog' / name, b'{}' if name.endswith('.json') else b'# fixed script')
        return repository

    def test_native_delivery_has_fixed_scripts_and_no_browser_execution_payload(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); repository = self.repository(root)
            destination = root / 'relocated/bin/libexec'
            manifest = runtime.bundle(repository, destination, 'x86_64-apple-darwin')
            self.assertEqual(manifest['delivery'], 'external_executables')
            self.assertEqual(manifest['components'], {})
            self.assertEqual(manifest['external_dependencies'], ['ffmpeg', 'ffprobe', 'node', 'python'])
            self.assertEqual(runtime.verify(destination), manifest)
            self.assertFalse(any(name.startswith('classic-') for name in manifest['files']))
            self.assertIn('font-catalog/run_derived_v2.py', manifest['files'])
            self.assertIn('font-catalog/run_color_static_v1.py', manifest['files'])
            self.assertEqual((manifest['format'],manifest['schema_version']), ('opencut.native-runtime-bundle',1))
            legacy = json.loads(json.dumps(manifest))
            legacy.update(format='opencut.runtime-bundle.v1',schema_version=1, external_dependencies=['ffmpeg','ffprobe','chromium','node','python'])
            del legacy['scripts']['run_color_static_v1.py']
            (destination / 'runtime-bundle.json').write_text(json.dumps(legacy))
            self.assertEqual(runtime.verify(destination),legacy)
            legacy.update(format='opencut.runtime-bundle.v2',schema_version=2, scripts=manifest['scripts'])
            (destination / 'runtime-bundle.json').write_text(json.dumps(legacy))
            self.assertEqual(runtime.verify(destination),legacy)
            (destination / 'runtime-bundle.json').write_text(json.dumps(manifest))
            self.assertNotIn(str(repository), (destination / 'runtime-bundle.json').read_text())
            (destination / 'unlisted').write_bytes(b'extra')
            with self.assertRaisesRegex(ValueError, 'mirror'):
                runtime.verify(destination)

    def test_internal_links_and_nested_execute_bits_survive_archive(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); source = root / 'input'; target = root / 'bundle'
            self.write(source / 'Framework/Versions/1/Helpers/tool', b'executable fixture', 0o755)
            (source / 'Framework/Versions/Current').symlink_to('1')
            (source / 'Framework/Helpers').symlink_to('Versions/Current/Helpers')
            runtime.copy_tree(source, target)
            entries = runtime.inventory(target)
            links = {name: entry['target'] for name, entry in entries.items() if entry['kind'] == 'symlink'}
            package.archive(target, root / 'bundle.tar.gz', links)
            with tarfile.open(root / 'bundle.tar.gz') as tar:
                self.assertEqual(tar.getmember('bundle/Framework/Versions/1/Helpers/tool').mode, 0o755)
                self.assertTrue(tar.getmember('bundle/Framework/Helpers').issym())
            with self.assertRaisesRegex(ValueError, 'Unexpected symlink'):
                package.archive(target, root / 'undeclared.tar.gz')

    def test_escape_absolute_link_special_mode_and_budget_refused_before_copy(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); source = root / 'input'; source.mkdir()
            self.write(root / 'outside', b'outside')
            link = source / 'link'; link.symlink_to('../outside')
            with self.assertRaisesRegex(ValueError, 'escapes'):
                runtime.copy_tree(source, root / 'copy')
            self.assertFalse((root / 'copy').exists())
            link.unlink(); self.write(source / 'real', b'inside')
            link.symlink_to(source / 'real')
            with self.assertRaisesRegex(ValueError, 'escapes'):
                runtime.inventory(source)
            link.unlink()
            # Some managed/macOS temporary filesystems silently clear SUID and
            # SGID after chmod. Model those observed metadata bits directly so
            # this negative case still exercises the production lstat gate on
            # every host, without requiring privileged executable fixtures.
            original_lstat = Path.lstat
            selected_file = (source / 'real').resolve(strict=True)
            for special in (stat.S_ISUID, stat.S_ISGID, stat.S_ISUID | stat.S_ISGID):
                def special_lstat(path, *args, **kwargs):
                    metadata = original_lstat(path, *args, **kwargs)
                    if path == selected_file:
                        fields = list(metadata)
                        fields[stat.ST_MODE] |= special
                        return os.stat_result(fields)
                    return metadata
                with self.subTest(special_mode=oct(special)), patch.object(Path, 'lstat', special_lstat):
                    self.assertEqual(selected_file.lstat().st_mode & special, special)
                    with self.assertRaisesRegex(ValueError, 'mode'):
                        runtime.inventory(source)
                    target = root / f'special-mode-{special:o}'
                    with self.assertRaisesRegex(ValueError, 'mode'):
                        runtime.copy_tree(source, target)
                    self.assertFalse(target.exists())
            with patch.object(runtime, 'MAX_TOTAL', 2), self.assertRaisesRegex(ValueError, 'budget'):
                runtime.inventory(source)

    def spec(self, root):
        components = {}
        for name in sorted(runtime.NATIVE_COMPONENTS):
            directory = root / name
            self.write(directory / 'bin/tool', macho(), 0o755)
            self.write(directory / 'LICENSE', b'explicit upstream license')
            components[name] = {'root': str(directory), 'entry': 'bin/tool', 'version': '3.14.7',
                                'license_spdx': 'MIT', 'license_files': ['LICENSE'], 'redistributable': True}
        spec = {'format': 'opencut.native-runtime-input', 'target': 'x86_64-apple-darwin', 'components': components}
        filename = root / 'runtime-spec.json'; filename.write_text(json.dumps(spec))
        return filename, spec

    def test_spec_accepts_auxiliary_subsets_and_requires_licenses_target_no_private_fields(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); filename, spec = self.spec(root)
            self.assertEqual(runtime.validate_spec(filename, spec['target']), spec)
            subset = json.loads(json.dumps(spec)); subset['components'].pop('python')
            filename.write_text(json.dumps(subset))
            self.assertEqual(runtime.validate_spec(filename, spec['target']), subset)
            for edit in [lambda value: value['components'].update(chromium=value['components']['node']),
                         lambda value: value.update(env={'TOKEN': 'not permitted'}),
                         lambda value: value['components']['node'].update(cache='/private/cache'),
                         lambda value: value['components']['node'].update(redistributable=False),
                         lambda value: value['components']['python'].update(license_files=[]),
                         lambda value: value['components']['node'].update(entry='../outside')]:
                candidate = json.loads(json.dumps(spec)); edit(candidate); filename.write_text(json.dumps(candidate))
                with self.assertRaises(ValueError):
                    runtime.validate_spec(filename, spec['target'])
            filename.write_text(json.dumps(spec))
            with self.assertRaisesRegex(ValueError, 'target'):
                runtime.validate_spec(filename, 'aarch64-apple-darwin')
            self.write(root / 'python/pyvenv.cfg', b'home = /outside/python')
            with self.assertRaisesRegex(ValueError, 'virtual environment'):
                runtime.validate_spec(filename, spec['target'])
            (root / 'python/pyvenv.cfg').unlink()
            spec['format'] = 'opencut.runtime-input.v1'
            filename.write_text(json.dumps(spec))
            with self.assertRaisesRegex(ValueError, 'Native runtime spec'):
                runtime.validate_spec(filename, spec['target'])

    def test_duplicate_manifest_keys_and_fonttools_without_original_license_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); filename = self.write(root / 'duplicate.json', b'{"components":{},"components":{}}')
            with self.assertRaisesRegex(ValueError, 'Duplicate'):
                runtime.read_json(filename)
            self.write(root / 'lib/python3.14/os.py')
            self.write(root / 'lib/python3.14/encodings/__init__.py')
            self.write(root / 'lib/python3.14/site-packages/fontTools/__init__.py')
            self.write(root / 'lib/python3.14/site-packages/fonttools-4.61.1.dist-info/METADATA', b'Version: 4.61.1\n')
            with self.assertRaisesRegex(ValueError, 'license'):
                runtime.python_identity(root, {'fonttools_version': '4.61.1'})

    def test_complete_spec_copies_exact_python_and_licenses_without_build_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); repository = self.repository(root)
            filename, spec = self.spec(root)
            python = root / 'python'
            self.write(python / 'lib/python3.14/os.py')
            self.write(python / 'lib/python3.14/encodings/__init__.py')
            tools = self.write(python / 'lib/python3.14/site-packages/fontTools/__init__.py', b'__version__ = "4.61.1"\n')
            dist = python / 'lib/python3.14/site-packages/fonttools-4.61.1.dist-info'
            self.write(dist / 'METADATA', b'Version: 4.61.1\n')
            self.write(dist / 'licenses/LICENSE', b'original fontTools license')
            identity = hashlib.sha256(json.dumps([['__init__.py', runtime.sha(tools)]], separators=(',', ':')).encode()).hexdigest()
            lock = {'python_version': '3.14.7', 'fonttools_version': '4.61.1', 'fonttools_implementation_sha256': identity}
            for name in ['derived-runtime-lock.json', 'derived-v2-runtime-lock.json','color-static-runtime-lock.json']:
                (repository / 'scripts/font-catalog' / name).write_text(json.dumps(lock))
            target = root / 'relocated/bin/libexec'
            manifest = runtime.bundle(repository, target, spec['target'], filename)
            self.assertEqual(manifest['delivery'], 'bundled_unverified')
            self.assertEqual(manifest['external_dependencies'], ['ffmpeg', 'ffprobe'])
            self.assertEqual(runtime.verify(target), manifest)
            serialized = (target / 'runtime-bundle.json').read_text()
            self.assertNotIn(str(root), serialized)
            self.assertEqual(manifest['components']['python']['fonttools']['implementation_sha256'], identity)
            copied = target / 'runtimes/python/lib/python3.14/site-packages/fontTools/__init__.py'
            copied.write_bytes(b'tampered implementation')
            with self.assertRaisesRegex(ValueError, 'mirror'):
                runtime.verify(target)

    def test_linkage_accepts_internal_and_system_rejects_homebrew_and_missing_rpath(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = self.write(root / 'bin/tool', macho(['/usr/lib/libSystem.B.dylib', '@rpath/libfixture.dylib'], ['@executable_path/../lib']), 0o755)
            self.write(root / 'lib/libfixture.dylib', macho(['/System/Library/Frameworks/Foundation.framework/Foundation'], executable=False))
            runtime.verify_macos_linkage(root, binary)
            binary.write_bytes(macho(['/usr/local/opt/dependency/lib/hidden.dylib']))
            with self.assertRaisesRegex(ValueError, 'external'):
                runtime.verify_macos_linkage(root, binary)
            binary.write_bytes(macho(['@rpath/missing.dylib'], ['@executable_path/../lib']))
            with self.assertRaisesRegex(ValueError, 'not bundled'):
                runtime.verify_macos_linkage(root, binary)
            binary.write_bytes(macho([], ['@executable_path/../../outside']))
            with self.assertRaisesRegex(ValueError, 'escapes'):
                runtime.verify_macos_linkage(root, binary)

    def test_doctor_binds_runtime_manifest_to_outer_file_and_link_inventories(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); repository = self.repository(root)
            host = root / 'bundle'; libexec = host / 'bin/libexec'
            runtime.bundle(repository, libexec, 'x86_64-apple-darwin')
            entries = runtime.inventory(host)
            outer = {'target': 'x86_64-apple-darwin',
                     'files': {name: {'sha256': value['sha256'], 'size': value['size']} for name, value in entries.items() if value['kind'] == 'file'},
                     'links': {name: value['target'] for name, value in entries.items() if value['kind'] == 'symlink'},
                     'runtime_bundle': {'manifest': 'bin/libexec/runtime-bundle.json', 'sha256': runtime.sha(libexec / 'runtime-bundle.json'), 'delivery': 'external_executables'}}
            doctor.declared_runtime(host, outer)
            outer['links'] = {'bin/libexec/unregistered': 'some-target'}
            with self.assertRaisesRegex(ValueError, 'links'):
                doctor.declared_runtime(host, outer)

    def test_explicit_invalid_environment_never_falls_back_to_bundled_executable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            node = self.write(root / 'bin/libexec/runtimes/node/bin/node', b'fixed', 0o755)
            manifest = {'components': {'node': {'entry': 'runtimes/node/bin/node'}}}
            with patch.dict(doctor.os.environ, {}, clear=True):
                self.assertEqual(doctor.runtime_executable(root, manifest, 'node', ('EXPLICIT_NODE',), True), node.resolve())
            with patch.dict(doctor.os.environ, {'EXPLICIT_NODE': ''}, clear=True), self.assertRaisesRegex(ValueError, 'unavailable'):
                doctor.runtime_executable(root, manifest, 'node', ('EXPLICIT_NODE',), True)
