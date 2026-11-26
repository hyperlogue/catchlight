"""The GitHub release step, run against a stub `gh` that records its calls."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from check import ROOT, check, release_notes

SCRIPT = ROOT / '.github/scripts/attach-release-assets.sh'
# A `gh` that keeps one release in a JSON file and logs every call. Like the
# real one, `release create` with files publishes only after uploading them.
STUB = r'''#!/usr/bin/env python3
import json, os, sys
state = os.environ["GH_STATE"]
args = sys.argv[1:]
record = list(args)
if "--notes-file" in args:
    record[args.index("--notes-file") + 1] = open(args[args.index("--notes-file") + 1]).read()
with open(os.environ["GH_LOG"], "a") as log:
    log.write(json.dumps(record) + "\n")
release = json.load(open(state)) if os.path.exists(state) else None
def save():
    json.dump(release, open(state, "w"))
if args[:2] == ["release", "view"]:
    if release is None:
        sys.exit(1)
    field = args[args.index("--json") + 1]
    if field == "isDraft":
        print("true" if release["draft"] else "false")
    else:
        print("\n".join(release["assets"]))
elif args[:2] == ["release", "create"]:
    files = [a for a in args[3:] if os.path.isfile(a)]
    release = {"draft": False, "assets": [os.path.basename(f) for f in files]}
    save()
elif args[:2] == ["release", "upload"]:
    if not release["draft"]:
        sys.exit("HTTP 422: Cannot upload assets to an immutable release.")
    release["assets"] += [os.path.basename(f) for f in args[3:]]
    save()
elif args[:2] == ["release", "edit"] and "--draft=false" in args:
    release["draft"] = False
    save()
'''
FILES = ['catchlight-editor-x86_64-unknown-linux-gnu.tar.gz', 'install.sh', 'SHA256SUMS']


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
        for name in FILES:
            (self.assets / name).write_text(name)
        self.state = self.root / 'release.json'
        self.log = self.root / 'gh.log'
        self.env = dict(os.environ, PATH=f'{bin_dir}{os.pathsep}{os.environ["PATH"]}',
                        GH_STATE=str(self.state), GH_LOG=str(self.log),
                        GITHUB_REPOSITORY='owner/repo')
        self.tag = f'v{check()}'

    def release(self, draft, assets):
        self.state.write_text(json.dumps({'draft': draft, 'assets': assets}))

    def run_script(self, ok=True):
        self.log.write_text('')
        result = subprocess.run(['bash', str(SCRIPT), str(self.assets)], env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, ok, result.stderr)
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def uploads(self, calls):
        return sorted(Path(f).name for c in calls if c[:2] == ['release', 'upload'] for f in c[3:])

    def test_a_new_release_is_created_with_every_file_and_the_changelog_notes(self):
        calls = self.run_script()
        [create] = [c for c in calls if c[:2] == ['release', 'create']]
        self.assertEqual(create[2], self.tag)
        self.assertIn('--verify-tag', create)
        self.assertEqual(create[create.index('--notes-file') + 1], release_notes(check()))
        self.assertEqual(sorted(Path(a).name for a in create[3:3 + len(FILES)]), sorted(FILES))
        self.assertEqual(self.uploads(calls), [])
        self.assertFalse(json.loads(self.state.read_text())['draft'])

    def test_an_interrupted_draft_gets_its_missing_files_then_publishes(self):
        self.release(draft=True, assets=['install.sh'])
        calls = self.run_script()
        self.assertFalse(any(c[:2] == ['release', 'create'] for c in calls))
        self.assertEqual(self.uploads(calls), sorted(set(FILES) - {'install.sh'}))
        self.assertEqual(calls[-1][:2], ['release', 'edit'])
        self.assertFalse(json.loads(self.state.read_text())['draft'])

    def test_a_complete_published_release_is_left_alone(self):
        self.release(draft=False, assets=FILES)
        calls = self.run_script()
        self.assertTrue(all(c[:2] == ['release', 'view'] for c in calls))

    def test_an_incomplete_published_release_fails_without_uploading(self):
        self.release(draft=False, assets=[])
        calls = self.run_script(ok=False)
        self.assertEqual(self.uploads(calls), [])


class ReleaseNotesTests(unittest.TestCase):
    def notes(self, changelog, version='1.2.0'):
        with tempfile.TemporaryDirectory() as temp:
            (Path(temp) / 'CHANGELOG.md').write_text(changelog)
            return release_notes(version, Path(temp))

    def test_takes_only_the_version_section_without_link_definitions(self):
        changelog = ('# Changelog\n\n## [1.2.0] - 2026-01-02\n\n### New\n\n- **A.** a\n\n'
                     '## [1.1.0] - 2026-01-01\n\n- old\n\n[1.2.0]: https://x/v1.2.0\n')
        self.assertEqual(self.notes(changelog), '### New\n\n- **A.** a\n')

    def test_wrapped_paragraphs_and_items_fill_one_line_each(self):
        changelog = ('# Changelog\n\n## [1.2.0] - 2026-01-02\n\nBreaking for\ncallers.\n\n'
                     '### New\n\n- **A.** Run `tool\n  --flag` and\n  see.\n- **B.** b\n'
                     '  - nested\n    item\n\n```sh\ncargo build\ncargo test\n```\n\n'
                     '| a | b |\n| - | - |\n')
        self.assertEqual(
            self.notes(changelog),
            'Breaking for callers.\n\n### New\n\n- **A.** Run `tool --flag` and see.\n'
            '- **B.** b\n  - nested item\n\n```sh\ncargo build\ncargo test\n```\n\n'
            '| a | b |\n| - | - |\n')

    def test_a_missing_section_is_an_error(self):
        with self.assertRaises(ValueError):
            self.notes('# Changelog\n\n## [1.1.0]\n\n- old\n')


if __name__ == '__main__':
    unittest.main()
