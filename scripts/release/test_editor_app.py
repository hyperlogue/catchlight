"""Archive layout for the downloadable editor."""
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile

from editor_app import TARGETS, asset_name, package


class EditorAppTests(unittest.TestCase):
    def test_every_target_has_a_versionless_asset(self):
        names = {asset_name(target) for target in TARGETS}
        self.assertIn('catchlight-editor-x86_64-pc-windows-msvc.zip', names)
        self.assertIn('catchlight-editor-aarch64-apple-darwin.tar.gz', names)
        with self.assertRaises(ValueError):
            asset_name('wasm32-unknown-unknown')

    def test_archives_hold_an_executable_and_the_notices(self):
        with tempfile.TemporaryDirectory() as temp:
            binary = Path(temp) / 'bin'
            binary.write_bytes(b'\x7fELF')
            tar = package('x86_64-unknown-linux-gnu', binary, temp)
            with tarfile.open(tar) as archive:
                members = {m.name: m for m in archive.getmembers()}
            folder = 'catchlight-editor-x86_64-unknown-linux-gnu'
            self.assertEqual(members[f'{folder}/catchlight-editor'].mode, 0o755)
            self.assertIn(f'{folder}/LICENSE-MIT', members)
            self.assertIn(f'{folder}/THIRD-PARTY-NOTICES.md', members)

            zipped = package('x86_64-pc-windows-msvc', binary, temp)
            with zipfile.ZipFile(zipped) as archive:
                self.assertIn('catchlight-editor-x86_64-pc-windows-msvc/catchlight-editor.exe',
                              archive.namelist())


if __name__ == '__main__':
    unittest.main()
