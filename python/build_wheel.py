"""Build the matching Rust server and a native Python wheel from one checkout.

Run with a Python environment containing hatchling. The local linux wheel is
for the build system's runtime; release wheels must also pass auditwheel in
the manylinux build container (see the Python wheels workflow).
"""

from __future__ import annotations

import argparse
import os
import shlex
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


def without_development_rpath(flags: str, repo: Path) -> str:
    """Drop Nix's unused development output RPATH, keeping toolchain paths."""
    parts = shlex.split(flags)
    cleaned = []
    index = 0
    while index < len(parts):
        if parts[index:index + 2] == ["-rpath", str(repo / "outputs" / "out" / "lib")]:
            index += 2
        else:
            cleaned.append(parts[index])
            index += 1
    return shlex.join(cleaned)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=Path("dist"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    repo = root.parent
    python_version = tomllib.loads((root / "pyproject.toml").read_text())["project"]["version"]
    rust_version = tomllib.loads((repo / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if python_version != rust_version:
        parser.error("Python and Rust workspace versions must match before bundling")
    if sys.platform != "linux":
        parser.error("bundled server wheels currently support Linux only")
    output = args.out.resolve()
    # An explicit target dir avoids accidentally packaging a stale default-
    # target binary when a contributor sets CARGO_BUILD_TARGET.
    with tempfile.TemporaryDirectory(prefix="catchlight-wheel-") as temporary:
        env = dict(os.environ)
        env.pop("CARGO_BUILD_TARGET", None)
        env["CARGO_TARGET_DIR"] = temporary
        env["CARGO_PROFILE_RELEASE_STRIP"] = "symbols"
        for name, value in env.copy().items():
            if name.startswith("NIX_LDFLAGS"):
                env[name] = without_development_rpath(value, repo)
        # Paths in compiler diagnostics/debug strings must not reveal the
        # build machine in the executable distributed to other people.
        rustflags = env.get("CARGO_ENCODED_RUSTFLAGS")
        if rustflags is None:
            flags = shlex.split(env.get("RUSTFLAGS", ""))
        else:
            flags = rustflags.split("\x1f")
        flags += ["--remap-path-prefix", f"{repo}=/src/catchlight"]
        flags += ["--remap-path-prefix", f"{Path.home()}=/build"]
        env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
        subprocess.run(
            ["cargo", "build", "--locked", "--release", "--package",
             "catchlight-editor-server", "--bin", "catchlight-editor-server"],
            cwd=repo, env=env, check=True,
        )
        env["CATCHLIGHT_BUNDLE_SERVER"] = str(Path(temporary) / "release" / "catchlight-editor-server")
        subprocess.run(
            [sys.executable, "-m", "hatchling", "build", "-t", "wheel", "-d", str(output)],
            cwd=root, env=env, check=True,
        )


if __name__ == "__main__":
    main()
