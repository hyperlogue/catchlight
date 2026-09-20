"""Bundle a prebuilt server only when the release builder requests it.

Source and editable installations stay dependency-free Python clients. Binary
wheels must advertise their ELF architecture and must never get an `any` tag.
Only auditwheel, after inspecting linkage, may grant a manylinux tag.
"""

from __future__ import annotations

import os
import struct
from pathlib import Path

from hatchling.builders.hooks.plugin.interface import BuildHookInterface

BUNDLE_ENV = "CATCHLIGHT_BUNDLE_SERVER"
BINARY_NAME = "catchlight-editor-server"
LICENSE_FILES = ("LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.md")


def elf_platform(binary: Path) -> str:
    with binary.open("rb") as stream:
        header = stream.read(20)
    if len(header) != 20 or header[:6] != b"\x7fELF\x02\x01":
        raise ValueError("the bundled server must be a 64-bit little-endian Linux ELF")
    machine = struct.unpack_from("<H", header, 18)[0]
    try:
        return {62: "linux_x86_64", 183: "linux_aarch64"}[machine]
    except KeyError:
        raise ValueError(f"unsupported bundled server ELF machine: {machine}") from None


class CustomBuildHook(BuildHookInterface):
    def initialize(self, version: str, build_data: dict) -> None:
        root = Path(self.root)
        # The checkout owns these documents; an sdist carries its own copies.
        licenses = root.parent if (root.parent / "LICENSE-MIT").is_file() else root / "licenses"
        destination = "catchlight/licenses" if self.target_name == "wheel" else "licenses"
        for name in LICENSE_FILES:
            source = licenses / name
            if not source.is_file():
                raise ValueError(f"missing distribution license file: {name}")
            build_data["force_include"][str(source)] = f"{destination}/{name}"

        binary_value = os.environ.get(BUNDLE_ENV)
        if not binary_value or self.target_name != "wheel":
            return
        if version == "editable":
            raise ValueError("bundling a server requires a wheel build, not an editable install")
        binary = Path(binary_value).resolve()
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError(f"{BUNDLE_ENV} must name an executable server file")
        if os.fsencode(Path.home()) in binary.read_bytes():
            raise ValueError("the server contains a build home path; rebuild with path remapping")
        build_data["tag"] = f"py3-none-{elf_platform(binary)}"
        build_data["pure_python"] = False
        build_data["force_include"][str(binary)] = f"catchlight/bin/{BINARY_NAME}"
