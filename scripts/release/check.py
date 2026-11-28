"""Check the shared public release version before building or publishing.

`--notes` prints that version's CHANGELOG.md section instead: the GitHub
release body, so the release page carries the reviewed notes.
"""
import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
# A README dependency line such as `catchlight = "0.4"` or
# `catchlight = { version = "0.4", features = [...] }`.
README_REQUIREMENT = re.compile(
    r'^(catchlight[-\w]*) = (?:\{[^}\n]*?version = )?"([^"]+)"', re.M)


def check(root=ROOT):
    workspace = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']
    version = workspace['package']['version']
    errors = []
    for path in sorted((root / 'crates').glob('*/Cargo.toml')):
        package = tomllib.loads(path.read_text())['package']
        if package.get('publish') is not False and package.get('version') != {'workspace': True}:
            errors.append(f'{path.relative_to(root)} must inherit the workspace version')
    for name, dependency in workspace['dependencies'].items():
        if isinstance(dependency, dict) and 'path' in dependency and dependency.get('version') != version:
            errors.append(f'{name} dependency must use {version}')
    for path in sorted((root / 'packages').glob('*/package.json')):
        package = json.loads(path.read_text())
        if not package.get('private') and package.get('version') != version:
            errors.append(f'{package["name"]} must use {version}')
    # Readmes pin major.minor so a patch release leaves them current.
    requirement = '.'.join(version.split('.')[:2])
    readmes = [root / 'README.md', *sorted(root.glob('crates/*/README.md'))]
    for path in readmes:
        if not path.exists():
            continue
        for name, found in README_REQUIREMENT.findall(path.read_text()):
            if found != requirement:
                errors.append(f'{path.relative_to(root)}: {name} must use "{requirement}"')
    lock = tomllib.loads((root / 'Cargo.lock').read_text())
    for package in lock['package']:
        if 'source' not in package and package['version'] != version:
            errors.append(f'Cargo.lock: {package["name"]} must use {version}')
    if errors:
        raise ValueError('\n'.join(errors))
    return version


def release_notes(version, root=ROOT):
    """The body of `## [version]` in CHANGELOG.md, without its heading or link
    definitions. Raises when the section is missing or empty."""
    lines, inside = [], False
    for line in (root / 'CHANGELOG.md').read_text().splitlines():
        if line.startswith('## ['):
            inside = line.startswith(f'## [{version}]')
        elif inside and not re.match(r'\[[^\]]+\]: ', line):
            lines.append(line)
    notes = unwrap('\n'.join(lines).strip())
    if not notes:
        raise ValueError(f'CHANGELOG.md has no notes under ## [{version}]')
    return notes + '\n'


BLOCK_START = re.compile(r'#{1,6} |[-*+] |\d+[.)] |\||>')
NO_CONTINUATION = re.compile(r'#{1,6} |\||```')


def unwrap(text):
    """Joins each paragraph and list item onto one line. A GitHub release
    body breaks the line at every newline, so the changelog's 72-column
    wrapping would show as ragged short lines."""
    out, fenced = [], False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith('```'):
            fenced = not fenced
            out.append(line)
        elif (fenced or not stripped or BLOCK_START.match(stripped) or not out
              or not out[-1].strip() or NO_CONTINUATION.match(out[-1].strip())):
            out.append(line)
        else:
            out[-1] += ' ' + stripped
    return '\n'.join(out)


if __name__ == '__main__':
    try:
        if sys.argv[1:] == ['--notes']:
            print(release_notes(check()), end='')
            sys.exit()
        print(check())
    except ValueError as error:
        sys.exit(str(error))
