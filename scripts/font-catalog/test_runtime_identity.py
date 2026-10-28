"""Published derivation locks must describe the code actually shipped/executed."""
import hashlib
import json
from pathlib import Path
import unittest
import generate
import generate_catalog


class RuntimeIdentityTests(unittest.TestCase):
    def test_every_saved_runtime_lock_binds_the_actual_immutable_validator(self):
        root = Path(__file__).parent
        actual = hashlib.sha256((root / 'generate.py').read_bytes()).hexdigest()
        provenance = json.loads((root / 'static-validator-source.json').read_text())
        self.assertEqual(actual, provenance['sha256'])
        for filename in ('derived-runtime-lock.json', 'derived-v2-runtime-lock.json', 'color-static-runtime-lock.json'):
            with self.subTest(lock=filename):
                self.assertEqual(actual, json.loads((root / filename).read_text())['static_validator_sha256'])

    def test_current_publication_uses_one_validator_and_current_output_directory(self):
        self.assertIs(generate_catalog.face, generate.face)
        self.assertIs(generate_catalog.static_jobs, generate.static_jobs)
        self.assertEqual(generate_catalog.OUT, generate_catalog.ROOT / 'crates/editor-application/src/font_import')


if __name__ == '__main__':
    unittest.main()
