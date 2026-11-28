"""Exercise release drift rejection without editing the working tree."""
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from check import ROOT, check


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        paths = [ROOT / 'Cargo.toml', ROOT / 'Cargo.lock']
        paths += list((ROOT / 'crates').glob('*/Cargo.toml'))
        paths += list((ROOT / 'packages').glob('*/package.json'))
        paths += [ROOT / 'README.md', *(ROOT / 'crates').glob('*/README.md')]
        for path in paths:
            dest = self.root / path.relative_to(ROOT)
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, dest)

    def test_current_release(self):
        self.assertEqual(check(self.root), check())

    def test_npm_drift(self):
        path = self.root / 'packages/core/package.json'
        package = json.loads(path.read_text())
        package['version'] = '99.0.0'
        path.write_text(json.dumps(package))
        with self.assertRaisesRegex(ValueError, '@catchlight/core'):
            check(self.root)

    def test_dependency_drift(self):
        path = self.root / 'Cargo.toml'
        path.write_text(path.read_text().replace(
            'path = "crates/catchlight-core", version = "' + check() + '"',
            'path = "crates/catchlight-core", version = "99.0.0"'))
        with self.assertRaisesRegex(ValueError, 'catchlight-core dependency'):
            check(self.root)

    def test_lock_drift(self):
        path = self.root / 'Cargo.lock'
        path.write_text(path.read_text().replace(
            'name = "catchlight"\nversion = "' + check() + '"',
            'name = "catchlight"\nversion = "99.0.0"'))
        with self.assertRaisesRegex(ValueError, 'Cargo.lock: catchlight'):
            check(self.root)

    def test_readme_drift(self):
        path = self.root / 'README.md'
        path.write_text(path.read_text() + '\n```toml\ncatchlight = "0.1.0"\n```\n')
        with self.assertRaisesRegex(ValueError, 'README.md: catchlight must use'):
            check(self.root)


if __name__ == '__main__':
    unittest.main()
