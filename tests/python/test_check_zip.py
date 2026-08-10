import hashlib
import json
import stat
import struct
import tempfile
import unittest
import zipfile
from pathlib import Path

from scripts.check_zip import REQUIRED_FILES, ZipValidationError, validate


def fake_elf(machine=62):
    data = bytearray(64)
    data[0:4] = b"\x7fELF"
    data[4] = 2  # ELF64
    data[5] = 1  # little endian
    data[6] = 1
    data[18:20] = struct.pack("<H", machine)
    return bytes(data)


def add_file(archive, relative, data=b"fixture", executable=False):
    info = zipfile.ZipInfo("decky-vox/" + relative)
    info.create_system = 3
    mode = stat.S_IFREG | (0o755 if executable else 0o644)
    info.external_attr = mode << 16
    archive.writestr(info, data)


class CheckZipTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.zip_path = Path(self.temporary_directory.name) / "decky-vox.zip"

    def tearDown(self):
        self.temporary_directory.cleanup()

    def write_fixture(self, core_machine=62, omit=None):
        omit = omit or set()
        core = fake_elf(core_machine)
        cpu = fake_elf()
        vulkan = fake_elf()
        binaries = {
            "bin/decky-vox-core": core,
            "bin/voxtype": cpu,
            "bin/voxtype-vulkan": vulkan,
        }
        package = {
            "name": "decky-vox",
            "remote_binary_bundling": True,
            "remote_binary": [
                {
                    "name": "voxtype",
                    "sha256hash": hashlib.sha256(cpu).hexdigest(),
                },
                {
                    "name": "voxtype-vulkan",
                    "sha256hash": hashlib.sha256(vulkan).hexdigest(),
                },
            ],
        }
        plugin = {"name": "Decky Vox", "flags": []}

        with zipfile.ZipFile(self.zip_path, "w") as archive:
            for relative in sorted(REQUIRED_FILES):
                if relative in omit:
                    continue
                if relative in binaries:
                    add_file(archive, relative, binaries[relative], executable=True)
                elif relative == "package.json":
                    add_file(archive, relative, json.dumps(package).encode())
                elif relative == "plugin.json":
                    add_file(archive, relative, json.dumps(plugin).encode())
                else:
                    add_file(archive, relative)

    def test_accepts_complete_x86_64_fixture(self):
        self.write_fixture()
        messages = validate(self.zip_path)
        self.assertIn(
            "verified executable ELF64 x86-64 core and voxtype binaries", messages
        )

    def test_rejects_missing_runtime_file(self):
        self.write_fixture(omit={"main.py"})
        with self.assertRaisesRegex(ZipValidationError, "missing required files"):
            validate(self.zip_path)

    def test_rejects_non_x86_64_core(self):
        self.write_fixture(core_machine=183)
        with self.assertRaisesRegex(ZipValidationError, "not x86-64 ELF"):
            validate(self.zip_path)


if __name__ == "__main__":
    unittest.main()
