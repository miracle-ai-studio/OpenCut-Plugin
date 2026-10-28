"""Native delivery closure: no developer-library search or untracked codecs."""
import json
from pathlib import Path
import tempfile
import unittest
from test_runtime_bundle import load, macho

native = load('native_runtime_tests', Path(__file__).resolve().parents[1] / 'distribution/native_runtime.py')


class NativeRuntimeTest(unittest.TestCase):
    def fixture(self, root):
        (root / 'bin/lib').mkdir(parents=True)
        libraries = {name: 'bin/lib/lib' + name + '.1.dylib' for name in native.LIBRARIES}
        files = {}
        for relative in ['bin/opencut', 'bin/opencut-render-worker', *libraries.values()]:
            path = root / relative
            executable = path.parent.name == 'bin'
            path.write_bytes(macho(['/usr/lib/libSystem.B.dylib'] + [('@executable_path/lib/' if executable else '@loader_path/') + Path(p).name for p in libraries.values()], executable=executable))
            path.chmod(0o755 if executable else 0o644)
            files[relative] = {'sha256': native.runtime.sha(path), 'size': path.stat().st_size}
        build = root / 'THIRD_PARTY/native-media/build.json'
        build.parent.mkdir(parents=True); build.write_text('{}')
        return {'format': 'opencut.native-renderer', 'target': 'x86_64-apple-darwin', 'cli': 'bin/opencut',
                'worker': 'bin/opencut-render-worker', 'libraries': libraries, 'build': str(build.relative_to(root)),
                'build_sha256': native.runtime.sha(build), 'files': files}

    def test_closed_relative_dependencies_verify_but_external_or_unrecorded_codecs_do_not(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); value = self.fixture(root)
            self.assertEqual(native.verify(root, value), value)
            path = root / value['worker']
            for dependency in ['/usr/local/opt/ffmpeg/lib/libavcodec.1.dylib', '@rpath/libavcodec.1.dylib', '@executable_path/lib/libdav1d.7.dylib', '/usr/lib/../local/untracked.dylib']:
                path.write_bytes(macho([dependency]))
                value['files'][value['worker']] = {'sha256': native.runtime.sha(path), 'size': path.stat().st_size}
                with self.subTest(dependency=dependency), self.assertRaisesRegex(ValueError, 'unidentified'):
                    native.verify(root, value)

    def test_dependency_search_paths_and_retagged_or_missing_files_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); value = self.fixture(root)
            path = root / value['cli']
            path.write_bytes(macho([], ['/usr/local/lib']))
            value['files'][value['cli']] = {'sha256': native.runtime.sha(path), 'size': path.stat().st_size}
            with self.assertRaisesRegex(ValueError, 'dynamic dependency'):
                native.verify(root, value)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); value = self.fixture(root)
            path = root / value['libraries']['avcodec']; original = path.read_bytes()
            path.write_bytes(original + b'changed')
            with self.assertRaisesRegex(ValueError, 'bytes differ'):
                native.verify(root, value)
            path.write_bytes(original)
            outside = root / 'outside'; outside.write_bytes(original)
            path.unlink(); path.symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'bytes differ'):
                native.verify(root, value)

    def test_native_source_requires_explicit_inputs_and_corresponding_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); spec = root / 'spec.json'
            spec.write_text(json.dumps({'format': 'opencut.native-media-input', 'target': 'x86_64-apple-darwin', 'root': str(root), 'provenance_root': str(root)}))
            (root / 'build.json').write_text('{}')
            with self.assertRaisesRegex(ValueError, 'LGPL'):
                native.source_spec(spec, 'x86_64-apple-darwin')
            with self.assertRaisesRegex(ValueError, 'matching target'):
                native.source_spec(spec, 'aarch64-apple-darwin')
