#!/usr/bin/env python3
"""Fail-closed validation for a Decky Vox install ZIP."""

from __future__ import annotations

import hashlib
import json
import stat
import struct
import sys
import zipfile
from pathlib import Path, PurePosixPath
from typing import Dict, Iterable, List


TOP_LEVEL = "decky-vox"
REQUIRED_FILES = {
    "dist/index.js",
    "bin/decky-vox-core",
    "bin/voxtype",
    "bin/voxtype-vulkan",
    "main.py",
    "package.json",
    "plugin.json",
    "README.md",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "licenses/decky-plugin-template-BSD-3-Clause.txt",
    "licenses/decky-voxtype-BSD-3-Clause.txt",
    "licenses/voxtype-MIT.txt",
    "licenses/whisper.cpp-MIT.txt",
}
EXECUTABLES = {
    "bin/decky-vox-core",
    "bin/voxtype",
    "bin/voxtype-vulkan",
}
BANNED_PARTS = {
    "__pycache__",
    ".pnpm-store",
    "node_modules",
    "target",
    "tests",
}


class ZipValidationError(RuntimeError):
    pass


def _relative_names(infos: Iterable[zipfile.ZipInfo]) -> Dict[str, zipfile.ZipInfo]:
    result: Dict[str, zipfile.ZipInfo] = {}
    roots = set()

    for info in infos:
        path = PurePosixPath(info.filename)
        if path.is_absolute() or ".." in path.parts:
            raise ZipValidationError(f"unsafe ZIP path: {info.filename}")
        if not path.parts:
            continue
        roots.add(path.parts[0])
        if path.parts[0] != TOP_LEVEL or info.is_dir():
            continue
        relative = PurePosixPath(*path.parts[1:]).as_posix()
        if relative in result:
            raise ZipValidationError(f"duplicate ZIP path: {relative}")
        result[relative] = info

    if roots != {TOP_LEVEL}:
        raise ZipValidationError(
            f"ZIP must contain exactly one '{TOP_LEVEL}/' root; got {sorted(roots)}"
        )
    return result


def _check_elf_x86_64(data: bytes, name: str) -> None:
    if len(data) < 20 or data[:4] != b"\x7fELF":
        raise ZipValidationError(f"{name} is not an ELF binary")
    if data[4] != 2:
        raise ZipValidationError(f"{name} is not ELF64")
    byte_order = "<" if data[5] == 1 else ">" if data[5] == 2 else None
    if byte_order is None:
        raise ZipValidationError(f"{name} has an invalid ELF byte order")
    machine = struct.unpack(f"{byte_order}H", data[18:20])[0]
    if machine != 62:
        raise ZipValidationError(f"{name} is not x86-64 ELF (e_machine={machine})")


def _check_executable(info: zipfile.ZipInfo, name: str) -> None:
    mode = info.external_attr >> 16
    if not stat.S_ISREG(mode) or mode & 0o111 == 0:
        raise ZipValidationError(f"{name} is not stored as an executable regular file")


def validate(path: Path) -> List[str]:
    if not path.is_file():
        raise ZipValidationError(f"ZIP does not exist: {path}")

    messages: List[str] = []
    with zipfile.ZipFile(path) as archive:
        if archive.testzip() is not None:
            raise ZipValidationError("ZIP CRC check failed")
        files = _relative_names(archive.infolist())

        missing = REQUIRED_FILES.difference(files)
        if missing:
            raise ZipValidationError(f"missing required files: {sorted(missing)}")

        for name in files:
            parts = PurePosixPath(name).parts
            if BANNED_PARTS.intersection(parts) or name.endswith((".part", ".wav")):
                raise ZipValidationError(f"development or temporary artifact included: {name}")

        package = json.loads(archive.read(files["package.json"]))
        plugin = json.loads(archive.read(files["plugin.json"]))
        if package.get("name") != TOP_LEVEL:
            raise ZipValidationError("package.json name must be decky-vox")
        if plugin.get("name") != "Decky Vox" or plugin.get("flags") != []:
            raise ZipValidationError("plugin.json identity or release flags are incorrect")

        declared = package.get("remote_binary")
        if not package.get("remote_binary_bundling") or not isinstance(declared, list):
            raise ZipValidationError("remote binary bundling is not declared")
        declared_by_name = {item.get("name"): item for item in declared}
        if set(declared_by_name) != {"voxtype", "voxtype-vulkan"}:
            raise ZipValidationError("unexpected remote binary declarations")

        for relative in sorted(EXECUTABLES):
            info = files[relative]
            data = archive.read(info)
            _check_executable(info, relative)
            _check_elf_x86_64(data, relative)
            binary_name = PurePosixPath(relative).name
            if binary_name in declared_by_name:
                expected = declared_by_name[binary_name].get("sha256hash")
                actual = hashlib.sha256(data).hexdigest()
                if actual != expected:
                    raise ZipValidationError(
                        f"{binary_name} SHA-256 mismatch: expected {expected}, got {actual}"
                    )

        messages.append(f"validated {len(files)} packaged files")
        messages.append("verified executable ELF64 x86-64 core and voxtype binaries")
        messages.append("verified remote binary declarations and SHA-256 digests")
    return messages


def main(argv: List[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} PATH_TO_ZIP", file=sys.stderr)
        return 2
    try:
        messages = validate(Path(argv[1]))
    except (OSError, ValueError, zipfile.BadZipFile, ZipValidationError) as error:
        print(f"ZIP validation failed: {error}", file=sys.stderr)
        return 1
    for message in messages:
        print(message)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
