"""Check the shared public release version before building or publishing."""
import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]


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
    lock = tomllib.loads((root / 'Cargo.lock').read_text())
    for package in lock['package']:
        if 'source' not in package and package['version'] != version:
            errors.append(f'Cargo.lock: {package["name"]} must use {version}')
    if errors:
        raise ValueError('\n'.join(errors))
    return version


if __name__ == '__main__':
    try:
        print(check())
    except ValueError as error:
        sys.exit(str(error))
