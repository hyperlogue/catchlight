"""Package and smoke-test one `catchlight-editor` release binary.

    python editor_app.py smoke <binary>
    python editor_app.py package <target> <binary> <out-dir>

Runs on every release runner (Linux, macOS, Windows) with only the standard
library. Asset names carry the target but not the version, so
`releases/latest/download/<asset>` always names the newest build; the
`v<version>` tag on the release is what pins one.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[2]
NAME = 'catchlight-editor'
# Rust target -> npm platform package suffix (`process.platform-process.arch`).
TARGETS = {
    'x86_64-unknown-linux-gnu': 'linux-x64',
    'aarch64-unknown-linux-gnu': 'linux-arm64',
    'aarch64-apple-darwin': 'darwin-arm64',
    'x86_64-apple-darwin': 'darwin-x64',
    'x86_64-pc-windows-msvc': 'win32-x64',
}
EXTRAS = ['README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md']


def asset_name(target):
    if target not in TARGETS:
        raise ValueError(f'unknown target {target}')
    suffix = '.zip' if 'windows' in target else '.tar.gz'
    return f'{NAME}-{target}{suffix}'


def binary_name(target):
    return f'{NAME}.exe' if 'windows' in target else NAME


def package(target, binary, out_dir):
    """Write `<asset>` holding `<name>-<target>/` with the binary and notices."""
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    folder = f'{NAME}-{target}'
    files = [(Path(binary), f'{folder}/{binary_name(target)}')]
    files += [(ROOT / 'crates' / NAME / extra, f'{folder}/{extra}') for extra in EXTRAS]
    asset = out_dir / asset_name(target)
    if asset.name.endswith('.zip'):
        with zipfile.ZipFile(asset, 'w', zipfile.ZIP_DEFLATED) as archive:
            for source, name in files:
                archive.write(source.resolve(), name)
    else:
        with tarfile.open(asset, 'w:gz') as archive:
            for source, name in files:
                info = archive.gettarinfo(source.resolve(), name)
                info.mode = 0o755 if name.endswith(NAME) else 0o644
                info.uid = info.gid = 0
                info.uname = info.gname = ''
                with open(source.resolve(), 'rb') as data:
                    archive.addfile(info, data)
    return asset


def smoke(binary):
    """Serve, load the page, answer a command over HTTP, and stop cleanly."""
    with tempfile.TemporaryDirectory() as temp:
        connection = Path(temp) / 'connection.json'
        env = dict(os.environ,
                   CATCHLIGHT_EDITOR_CONNECTION=str(connection),
                   XDG_RUNTIME_DIR=temp, XDG_CACHE_HOME=temp, LOCALAPPDATA=temp)
        version = subprocess.run([binary, '--version'], env=env, check=True,
                                 capture_output=True, text=True).stdout
        print(version.strip())
        server = subprocess.Popen([binary, '--no-open', '--port', '0'], env=env)
        try:
            deadline = time.monotonic() + 60
            while not connection.exists():
                if server.poll() is not None or time.monotonic() > deadline:
                    raise SystemExit('the editor never recorded its connection')
                time.sleep(0.1)
            url = json.loads(connection.read_text())['url']
            with urllib.request.urlopen(url + '/', timeout=30) as page:
                if b'<div id="root">' not in page.read():
                    raise SystemExit('the editor page is missing from the binary')
            listed = subprocess.run([binary, 'session', 'list'], env=env, check=True,
                                    capture_output=True, text=True, timeout=60).stdout
            if 'no open sessions' not in listed:
                raise SystemExit(f'unexpected session list: {listed!r}')
        finally:
            server.terminate()
            server.wait(timeout=30)
    print(f'{binary}: serves its page and answers commands')


def main(argv):
    if argv[:1] == ['smoke'] and len(argv) == 2:
        smoke(argv[1])
    elif argv[:1] == ['package'] and len(argv) == 4:
        print(package(argv[1], argv[2], argv[3]))
    else:
        raise SystemExit(__doc__)


if __name__ == '__main__':
    main(sys.argv[1:])
