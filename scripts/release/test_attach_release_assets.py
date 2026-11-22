"""The GitHub release step, run against a stub `gh` that records its calls."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from check import ROOT, check

SCRIPT = ROOT / '.github/scripts/attach-release-assets.sh'
# A `gh` that keeps one release in a JSON file and logs every call.
STUB = r'''#!/usr/bin/env python3
import json, os, sys
state = os.environ["GH_STATE"]
with open(os.environ["GH_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\n")
release = json.load(open(state)) if os.path.exists(state) else None
args = sys.argv[1:]
if args[:2] == ["release", "view"]:
    if release is None:
        sys.exit(1)
    if "--json" in args:
        print("\n".join(release["assets"]))
elif args[:2] == ["release", "create"]:
    json.dump({"assets": []}, open(state, "w"))
elif args[:2] == ["release", "upload"]:
    release["assets"].append(os.path.basename(args[3]))
    json.dump(release, open(state, "w"))
'''


class AttachReleaseAssetsTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        (bin_dir / 'gh').write_text(STUB)
        (bin_dir / 'gh').chmod(0o755)
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        for name in ['catchlight-editor-x86_64-unknown-linux-gnu.tar.gz', 'install.sh', 'SHA256SUMS']:
            (self.assets / name).write_text(name)
        self.state = self.root / 'release.json'
        self.log = self.root / 'gh.log'
        self.env = dict(os.environ, PATH=f'{bin_dir}{os.pathsep}{os.environ["PATH"]}',
                        GH_STATE=str(self.state), GH_LOG=str(self.log),
                        GITHUB_REPOSITORY='owner/repo')

    def run_script(self):
        self.log.write_text('')
        subprocess.run(['bash', str(SCRIPT), str(self.assets)], env=self.env, check=True,
                       capture_output=True, text=True)
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_a_new_release_is_created_with_changelog_notes_and_every_file(self):
        calls = self.run_script()
        tag = f'v{check()}'
        create = next(c for c in calls if c[:2] == ['release', 'create'])
        self.assertEqual(create[2], tag)
        self.assertIn('--verify-tag', create)
        notes = create[create.index('--notes') + 1]
        self.assertEqual(notes, f'See [CHANGELOG.md](https://github.com/owner/repo/blob/{tag}/CHANGELOG.md).')
        self.assertEqual(sorted(json.loads(self.state.read_text())['assets']),
                         sorted(p.name for p in self.assets.iterdir()))

    def test_a_rerun_uploads_only_what_is_missing(self):
        self.state.write_text(json.dumps({'assets': ['install.sh']}))
        calls = self.run_script()
        self.assertFalse(any(c[:2] == ['release', 'create'] for c in calls))
        uploaded = [Path(c[3]).name for c in calls if c[:2] == ['release', 'upload']]
        self.assertEqual(sorted(uploaded),
                         ['SHA256SUMS', 'catchlight-editor-x86_64-unknown-linux-gnu.tar.gz'])
        self.assertFalse(any('--clobber' in c for c in calls))


if __name__ == '__main__':
    unittest.main()
