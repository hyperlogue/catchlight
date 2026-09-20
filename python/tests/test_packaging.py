"""Distribution contracts: native tags, executable contents, and source fallback."""

from __future__ import annotations

import importlib.util
import runpy
import shutil
import struct
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

import pytest

from catchlight import ServerError
from catchlight import server

PYTHON_ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("hatch_build", PYTHON_ROOT / "hatch_build.py")
assert spec is not None and spec.loader is not None
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


def elf(path: Path, machine: int = 62) -> Path:
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", header, 18, machine)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(header)
    path.chmod(0o755)
    return path


@pytest.mark.parametrize(("machine", "platform"), [(62, "linux_x86_64"), (183, "linux_aarch64")])
def test_platform_comes_from_the_binary(tmp_path: Path, machine: int, platform: str) -> None:
    assert hook.elf_platform(elf(tmp_path / "server", machine)) == platform


@pytest.mark.parametrize("contents", [b"#!/bin/sh\n", b"\x7fELF\x01\x01" + bytes(58)])
def test_non_64_bit_elf_is_rejected(tmp_path: Path, contents: bytes) -> None:
    binary = tmp_path / "server"
    binary.write_bytes(contents)
    with pytest.raises(ValueError, match="64-bit"):
        hook.elf_platform(binary)


def test_unknown_architecture_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="unsupported"):
        hook.elf_platform(elf(tmp_path / "server", machine=243))


def test_explicit_and_environment_override_bundle_which_overrides_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(server, "__file__", str(tmp_path / "catchlight" / "server.py"))
    bundled = elf(tmp_path / "catchlight" / "bin" / server.BINARY_NAME)
    ambient = elf(tmp_path / "path" / server.BINARY_NAME)
    explicit = elf(tmp_path / "explicit")
    override = elf(tmp_path / "override")
    monkeypatch.setenv("PATH", str(ambient.parent))
    monkeypatch.delenv(server.BINARY_ENV, raising=False)
    assert server._find_binary(None) == str(bundled)
    monkeypatch.setenv(server.BINARY_ENV, str(override))
    assert server._find_binary(None) == str(override)
    assert server._find_binary(explicit) == str(explicit)
    monkeypatch.setenv(server.BINARY_ENV, str(tmp_path / "missing"))
    with pytest.raises(ServerError, match="not there"):
        server._find_binary(None)
    monkeypatch.delenv(server.BINARY_ENV)
    bundled.chmod(0o644)
    with pytest.raises(ServerError, match="not executable"):
        server._find_binary(None)
    bundled.unlink()
    assert server._find_binary(None) == str(ambient)


@pytest.fixture
def project(tmp_path: Path) -> Path:
    root = tmp_path / "project"
    root.mkdir()
    for name in ("pyproject.toml", "README.md", "hatch_build.py"):
        shutil.copyfile(PYTHON_ROOT / name, root / name)
    shutil.copytree(PYTHON_ROOT / "catchlight", root / "catchlight", ignore=shutil.ignore_patterns("__pycache__", "bin"))
    for name in hook.LICENSE_FILES:
        shutil.copyfile(PYTHON_ROOT.parent / name, tmp_path / name)
    return root


def build(project: Path, target: str) -> Path:
    subprocess.run([sys.executable, "-m", "hatchling", "build", "-t", target], cwd=project, check=True, capture_output=True)
    extension = "whl" if target == "wheel" else "tar.gz"
    return next((project / "dist").glob(f"*.{extension}"))


@pytest.mark.parametrize(("machine", "platform"), [(62, "linux_x86_64"), (183, "linux_aarch64")])
def test_wheel_contains_only_the_server_and_is_platform_tagged(
    project: Path, monkeypatch: pytest.MonkeyPatch, machine: int, platform: str,
) -> None:
    binary = elf(project / "native" / server.BINARY_NAME, machine)
    monkeypatch.setenv(hook.BUNDLE_ENV, str(binary))
    wheel = build(project, "wheel")
    assert wheel.name.endswith(f"-py3-none-{platform}.whl")
    with zipfile.ZipFile(wheel) as archive:
        name = f"catchlight/bin/{server.BINARY_NAME}"
        assert archive.read(name) == binary.read_bytes()
        assert archive.getinfo(name).external_attr >> 16 & 0o111
        assert [item for item in archive.namelist() if item.startswith("catchlight/bin/")] == [name]
        metadata = next(item for item in archive.namelist() if item.endswith(".dist-info/WHEEL"))
        assert "Root-Is-Purelib: false" in archive.read(metadata).decode()
        assert not any(item.startswith("tests/") for item in archive.namelist())
        for license_name in hook.LICENSE_FILES:
            assert f"catchlight/licenses/{license_name}" in archive.namelist()


def test_sdist_builds_a_client_only_wheel_without_the_workspace(
    project: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv(hook.BUNDLE_ENV, raising=False)
    source = build(project, "sdist")
    unpacked = tmp_path / "unpacked"
    with tarfile.open(source) as archive:
        assert not any("/bin/" in name for name in archive.getnames())
        archive.extractall(unpacked, filter="data")
    # The extracted sdist must not accidentally obtain licenses from a checkout.
    extracted = next(unpacked.iterdir())
    wheel = build(extracted, "wheel")
    assert wheel.name.endswith("-py3-none-any.whl")
    with zipfile.ZipFile(wheel) as archive:
        assert not any(name.startswith("catchlight/bin/") for name in archive.namelist())
        assert "catchlight/licenses/LICENSE-MIT" in archive.namelist()


def test_missing_requested_bundle_fails_instead_of_silently_building_pure_wheel(
    project: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(hook.BUNDLE_ENV, str(project / "missing"))
    with pytest.raises(subprocess.CalledProcessError):
        build(project, "wheel")


def test_builder_rejects_version_drift_before_compiling(project: Path) -> None:
    shutil.copyfile(PYTHON_ROOT / "build_wheel.py", project / "build_wheel.py")
    (project.parent / "Cargo.toml").write_text('[workspace.package]\nversion = "9.9.9"\n')
    result = subprocess.run(
        [sys.executable, str(project / "build_wheel.py")],
        cwd=project, capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "versions must match" in result.stderr


def test_builder_drops_only_nix_development_output_rpath() -> None:
    clean = runpy.run_path(str(PYTHON_ROOT / "build_wheel.py"))["without_development_rpath"]
    assert clean(
        "-rpath /src/project/outputs/out/lib -L/runtime/lib -rpath /runtime/lib",
        Path("/src/project"),
    ) == "-L/runtime/lib -rpath /runtime/lib"


def test_bundle_rejects_embedded_build_home_paths(
    project: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    binary = elf(project / "native" / server.BINARY_NAME)
    with binary.open("ab") as stream:
        stream.write(b"/build-private/compiler-source")
    monkeypatch.setattr(hook.Path, "home", classmethod(lambda cls: Path("/build-private")))
    monkeypatch.setenv(hook.BUNDLE_ENV, str(binary))
    build_hook = hook.CustomBuildHook(str(project), {}, None, None, str(project / "dist"), "wheel")
    with pytest.raises(ValueError, match="contains a build home path"):
        build_hook.initialize("standard", {"force_include": {}})
