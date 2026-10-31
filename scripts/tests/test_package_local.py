"""Delivery must exclude local residue and reject undeclared package contents."""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("package_local", Path(__file__).resolve().parents[1] / "package-local.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class PackageContentsTest(unittest.TestCase):
    def test_acceptance_unpack_only_admits_declared_internal_runtime_links(self):
        scripts = Path(__file__).resolve().parents[1]
        with patch.object(sys, 'path', [str(scripts), *sys.path]):
            spec = importlib.util.spec_from_file_location('verify_local_package', scripts / 'verify-local-package.py')
            verifier = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(verifier)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def archive(name, links, manifest_links=None, extra=(), hard=False):
                file = root / (name + '.tar')
                with tarfile.open(file, 'w') as tar:
                    files = {'bundle/manifest.json': json.dumps({'links': links if manifest_links is None else manifest_links}).encode(),
                             'bundle/bin/libexec/real': b'fixed-runtime'}
                    files.update(extra)
                    for path, value in files.items():
                        item = tarfile.TarInfo(path); item.size = len(value)
                        tar.addfile(item, io.BytesIO(value))
                    for path, target in links.items():
                        item = tarfile.TarInfo('bundle/' + path)
                        item.type = tarfile.LNKTYPE if hard else tarfile.SYMTYPE
                        item.linkname = target; tar.addfile(item)
                return file

            good = archive('valid', {'bin/libexec/alias': 'real'})
            verifier.unpack(good, root / 'accepted')
            self.assertTrue((root / 'accepted/bundle/bin/libexec/alias').is_symlink())
            self.assertEqual((root / 'accepted/bundle/bin/libexec/alias').read_bytes(), b'fixed-runtime')
            cases = [
                ('undeclared', {'bin/libexec/alias': 'real'}, {}, (), False),
                ('absolute', {'bin/libexec/alias': '/private/tmp/outside'}, None, (), False),
                ('escape', {'bin/libexec/alias': '../../../outside'}, None, (), False),
                ('cycle', {'bin/libexec/a': 'b', 'bin/libexec/b': 'a'}, None, (), False),
                ('hard', {'bin/libexec/alias': 'real'}, None, (), True),
                ('write-through-link', {'bin/libexec/alias': '.'}, None, (('bundle/bin/libexec/alias/overwrite', b'bad'),), False),
                # Normalizing '..' before resolving alias would incorrectly
                # consider this nested target inside the archive root.
                ('alias-escape', {'bin/libexec/up': '../..', 'bin/libexec/alias': 'up/../outside'}, None, (), False),
            ]
            for name, links, declared, extra, hard in cases:
                with self.subTest(name=name), self.assertRaises(ValueError):
                    verifier.unpack(archive(name, links, declared, extra, hard), root / name)
                self.assertFalse((root / name).exists(), 'Reject before publishing any files')

    def test_catalog_delivery_is_exact_two_file_subset_with_identity_and_no_source_audio(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); source = root / 'packages'; visual = source / 'classic-visual-renderer'
            (visual / 'host').mkdir(parents=True)
            (visual / 'package.json').write_text(json.dumps({'name': '@opencut/classic-visual-renderer', 'version': '0.1.0', 'type': 'module'}))
            (visual / 'host/catalog-archive.mjs').write_text('// fixed fixture adapter')
            (visual / 'host/browser.mjs').write_text('not bundled')
            (visual / 'user.ogg').write_text('not bundled')
            destination = root / 'bundle/bin/libexec'
            record = package.copy_catalog_runtime(source, destination)
            copied = {str(p.relative_to(root / 'bundle')) for p in destination.rglob('*') if p.is_file()}
            self.assertEqual(copied, set(record['files']))
            self.assertEqual(len(copied), 2)
            self.assertFalse(record['source_archive_included'])
            self.assertEqual(record['external_runtime'], 'node')
            for name, expected in record['files'].items():
                path = root / 'bundle' / name
                self.assertEqual(expected, {'sha256': package.sha(path), 'size': path.stat().st_size})
            helper = visual / 'host/catalog-archive.mjs'
            helper.unlink(); helper.symlink_to(visual / 'host/browser.mjs')
            with self.assertRaisesRegex(ValueError, 'Symlink'):
                package.copy_catalog_runtime(source, root / 'bad-link')
            helper.unlink(); helper.write_bytes(b'x' * 262145)
            with self.assertRaisesRegex(ValueError, 'budget'):
                package.copy_catalog_runtime(source, root / 'oversize')
            helper.write_text('// fixed fixture adapter')
            (visual / 'package.json').write_text(json.dumps({'name': '@opencut/classic-visual-renderer', 'version': '9.0.0', 'type': 'module'}))
            with self.assertRaisesRegex(ValueError, 'Unpinned'):
                package.copy_catalog_runtime(source, root / 'wrong-package')

    def test_native_packet_provenance_is_complete_and_rejects_changed_or_linked_source(self):
        import shutil
        original = Path(__file__).resolve().parents[2] / 'crates/media/vendor/matroska-demuxer'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source'
            shutil.copytree(original, source)
            destination = root / 'delivery'
            metadata = package.copy_packet_probe_provenance(source, destination)
            self.assertEqual(metadata['implementation'], 'native-worker')
            self.assertEqual(metadata['license'], 'MIT')
            self.assertEqual({p.name for p in destination.iterdir()}, {'LICENSE-MIT', 'LICENSE-APACHE', 'LICENSE-ZLIB', 'source.json', 'opencut.patch'})
            self.assertEqual(package.sha(destination / 'source.json'), metadata['provenance_sha256'])
            module = source / 'src/lib.rs'
            original_bytes = module.read_bytes()
            module.write_bytes(original_bytes + b'\n// changed\n')
            with self.assertRaisesRegex(ValueError, 'differs'):
                package.copy_packet_probe_provenance(source, root / 'changed')
            module.unlink()
            outside = root / 'outside.rs'
            outside.write_bytes(original_bytes)
            module.symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'Symlink|escape'):
                package.copy_packet_probe_provenance(source, root / 'linked')

    def test_rust_source_retains_test_modules_but_excludes_local_residue(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "src"
            source.mkdir()
            for name in ["lib.rs", "tests.rs", ".DS_Store", "lib.rs.bak"]:
                (source / name).write_text("fixture")
            (source / "target").mkdir()
            (source / "target" / "private.log").write_text("not distributable")
            package.copy_sdk_sources(source, root / "copy")
            self.assertEqual(sorted(p.name for p in (root / "copy").iterdir()), ["lib.rs", "tests.rs"])

    def test_rust_source_rejects_unreviewed_files_and_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "src"
            source.mkdir()
            (source / "workspace.sqlite").write_bytes(b"private")
            with self.assertRaisesRegex(ValueError, "Unreviewed"):
                package.copy_sdk_sources(source, root / "copy")
            (source / "workspace.sqlite").unlink()
            (source / "lib.rs").symlink_to(root / "outside")
            with self.assertRaisesRegex(ValueError, "Symlink"):
                package.copy_sdk_sources(source, root / "second")

    def test_npm_delivery_rejects_tests_caches_and_unexpected_files(self):
        package.validate_npm_files([{"path":p} for p in ["package.json", "README.md", "dist/index.js", "dist/index.d.ts"]])
        for name in ["tests/transport.test.mjs", "dist/index.test.js", "dist/index.spec.d.ts", "dist/.DS_Store", "dist/nested/private.js", "node_modules/module.js"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                package.validate_npm_files([{"path":name}])

    def test_web_delivery_license_requires_exact_reviewed_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'build'
            name = Path('assets/licenses/preview-digest.txt')
            original = root / 'apps/web/public' / name
            original.parent.mkdir(parents=True)
            original.write_text('reviewed license fixture')
            (source / name).parent.mkdir(parents=True)
            (source / 'index.html').write_text('<title>Editor</title>')
            (source / name).write_bytes(original.read_bytes())
            with patch.object(package, 'ROOT', root):
                package.copy_web_build(source, root / 'accepted')
                self.assertEqual((root / 'accepted' / name).read_bytes(), original.read_bytes())
                (source / name).write_text('altered license')
                with self.assertRaisesRegex(ValueError, 'bytes differ'):
                    package.copy_web_build(source, root / 'altered')
                (source / name).write_bytes(original.read_bytes())
                (source / name.parent / 'private.txt').write_text('private')
                with self.assertRaisesRegex(ValueError, 'Unreviewed Web'):
                    package.copy_web_build(source, root / 'extra')

    def test_web_delivery_keeps_public_assets_and_rejects_private_residue(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'build'
            (source / 'assets').mkdir(parents=True)
            (source / 'index.html').write_text('<title>Editor</title>')
            (source / 'assets/editor.js').write_text('export const editor = true')
            (source / 'assets/editor-worker.js').write_text('void (() => {})()')
            package.copy_web_build(source, root / 'web')
            self.assertTrue((root / 'web/assets/editor.js').is_file())
            self.assertEqual((root / 'web/assets/editor-worker.js').read_bytes(), (source / 'assets/editor-worker.js').read_bytes())
            (source / 'unreviewed.js').write_text('private')
            with self.assertRaisesRegex(ValueError, 'Unreviewed Web'):
                package.copy_web_build(source, root / 'unreviewed')
            (source / 'unreviewed.js').unlink()
            (source / 'api-token').write_text('private')
            with self.assertRaisesRegex(ValueError, 'Unreviewed Web'):
                package.copy_web_build(source, root / 'rejected')
            (source / 'api-token').unlink()
            atlas = Path('assets/classic-font-atlas')
            original = root / 'apps/web/public' / atlas
            original.mkdir(parents=True)
            (source / atlas).mkdir(parents=True)
            names = ['README.md', 'font-atlas.json'] + [f'font-chunk-{i}.avif' for i in range(15)]
            for name in names:
                (original / name).write_text('reviewed public fixture ' + name)
                (source / atlas / name).write_bytes((original / name).read_bytes())
            with patch.object(package, 'ROOT', root):
                package.copy_web_build(source, root / 'with-atlas')
                self.assertEqual({p.name for p in (root / 'with-atlas' / atlas).iterdir()}, set(names))
                (source / atlas / 'project.json').write_text('private')
                with self.assertRaisesRegex(ValueError, 'complete file set'):
                    package.copy_web_build(source, root / 'private-atlas')
                (source / atlas / 'project.json').unlink()
                (source / atlas / 'font-atlas.json').write_text('changed')
                with self.assertRaisesRegex(ValueError, 'bytes differ'):
                    package.copy_web_build(source, root / 'changed-atlas')
