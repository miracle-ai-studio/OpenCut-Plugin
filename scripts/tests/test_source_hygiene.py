import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


spec = importlib.util.spec_from_file_location(
    'source_hygiene', Path(__file__).resolve().parents[1] / 'check-source-hygiene.py')
hygiene = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hygiene)


class SourceHygieneTests(unittest.TestCase):
    def test_repeated_findings_preserve_lines_and_pattern_order(self):
        content = b'\n'.join([b'/Users/' + b'fixture-owner/code.rs',
                               b'ordinary text', b'-----BEGIN ' + b'PRIVATE KEY-----',
                               b'/Users/' + b'fixture-owner/other.rs'])
        self.assertEqual(hygiene.inspect_file('fixture.bin', content),
                         [(3, 'private key'), (1, 'personal home path'), (4, 'personal home path')])

    def test_publication_set_includes_staged_and_untracked_but_not_ignored(self):
        with tempfile.TemporaryDirectory(prefix='opencut-source-hygiene-') as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '--quiet', directory], check=True)
            (root / '.gitignore').write_text('ignored/\n*.wav\n')
            (root / 'forced.wav').write_bytes(b'synthetic')
            subprocess.run(['git', 'add', '--force', 'forced.wav'], cwd=root, check=True)
            (root / 'untracked.py').write_text('pass\n')
            (root / 'ignored').mkdir()
            (root / 'ignored' / 'workspace.db').write_bytes(b'private fixture')
            files = hygiene.publication_files(root)
            self.assertEqual(files, ['.gitignore', 'forced.wav', 'untracked.py'])
            self.assertTrue(hygiene.inspect_file('forced.wav', (root / 'forced.wav').read_bytes()))

    def test_private_files_and_renamed_database_are_rejected(self):
        for name in ('source.MOV', 'audio.wav', 'workspace.sqlite-wal', 'workspace.db-journal',
                     '.env.local', 'api-token', '.aws/config', 'backup.opencut'):
            with self.subTest(name=name):
                self.assertTrue(hygiene.inspect_file(name, b''))
        self.assertTrue(hygiene.inspect_file('innocent.bin', b'SQLite format 3\x00'))

    def test_licensed_assets_and_negative_authorization_fixtures_remain_valid(self):
        for name in ('atlas.avif', 'flag.svg', 'LICENSE', '.env.example', 'protocol.d.mts'):
            self.assertFalse(hygiene.inspect_file(name, b'not a secret'))
        self.assertFalse(hygiene.inspect_file('tests.rs', b'"/Users/private/media.wav"'))
        self.assertFalse(hygiene.inspect_file('tests.ts', b'https://user:password@example.test'))
        self.assertFalse(hygiene.inspect_file('inventory.json', b'https://github.com/org/home/blob/source.svg'))

    def test_secrets_and_machine_paths_are_redacted(self):
        values = [b'gh' + b'p_' + b'A' * 36,
                  b'-----BEGIN ' + b'PRIVATE KEY-----',
                  b'AK' + b'IA' + b'A' * 16,
                  b'sk-' + b'proj-' + b'A' * 64,
                  b'/Users/' + b'example-owner/private.json',
                  b'file://' + b'/Users/' + b'example-owner/private.json',
                  b'/private/var/' + b'folders/ab/user/T/file']
        for value in values:
            with self.subTest(kind=value[:4]):
                findings = hygiene.inspect_file('file.txt', b'first line\n' + value)
                self.assertTrue(findings)
                self.assertEqual(findings[0][0], 2)
                self.assertNotIn(value.decode(), str(findings))


if __name__ == '__main__':
    unittest.main()
