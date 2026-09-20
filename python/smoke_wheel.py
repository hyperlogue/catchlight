"""Check a release wheel, install it in isolation, and launch its real server.

Requires pip in the interpreter's venv seed. Pass the wheel path as the only
argument. Neither the checkout nor any server on PATH can satisfy this check.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import venv
import zipfile
from pathlib import Path


def main() -> None:
    wheel = Path(sys.argv[1]).resolve()
    binary = "catchlight/bin/catchlight-editor-server"
    with zipfile.ZipFile(wheel) as archive:
        assert archive.getinfo(binary).external_attr >> 16 & 0o111, "server lost executable mode"
        assert archive.read(binary)[:4] == b"\x7fELF", "server is not ELF"
        assert not any(Path(name).name in {"catchlight-cli", "catchlight-editor-cli"}
                       for name in archive.namelist()), "unrequested CLI bundled"
        metadata = next(name for name in archive.namelist() if name.endswith(".dist-info/WHEEL"))
        metadata_text = archive.read(metadata).decode()
        assert "Root-Is-Purelib: false" in metadata_text
        assert "Tag: py3-none-" in metadata_text and "none-any" not in metadata_text
        for name in ("LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.md"):
            assert f"catchlight/licenses/{name}" in archive.namelist()
    with tempfile.TemporaryDirectory(prefix="catchlight-installed-") as temporary:
        root = Path(temporary)
        venv.EnvBuilder(with_pip=True).create(root / "venv")
        python = root / "venv" / "bin" / "python"
        subprocess.run([str(python), "-m", "pip", "install", "--no-index", "--no-deps", str(wheel)], check=True)
        env = dict(os.environ)
        env.pop("CATCHLIGHT_EDITOR_SERVER", None)
        env.pop("PYTHONPATH", None)
        env["PATH"] = str(root / "empty-path")
        subprocess.run([str(python), "-I", "-c", """
from pathlib import Path
import catchlight
from catchlight.server import _find_binary
assert Path(_find_binary(None)).parent == Path(catchlight.__file__).parent / "bin"
with catchlight.launch() as server:
    assert server.healthy()
    session = server.client().new("Installed wheel")
    saved = Path(server.client().save_to(session, "wheel.clm"))
    assert saved.is_file()
assert not server.directory.exists()
"""], cwd=root, env=env, check=True)


if __name__ == "__main__":
    main()
