#!/usr/bin/env python3
"""Synthetic format regressions, never runtime evidence."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('inspector', Path(__file__).with_name('inspect-ios-package.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def fixture(platform=2, cpu=0x0100000C):
    name = b'@rpath/MadInfer.framework/MadInfer\0'
    name += b'\0' * (-len(name) % 8)
    commands = struct.pack('<6I', 0x32, 24, platform, 16 << 16, 26 << 16, 0)
    commands += struct.pack('<6I', 0xD, 24 + len(name), 24, 0, 0, 0) + name
    return struct.pack('<8I', 0xFEEDFACF, cpu, 0, 6, 2, len(commands), 0, 0) + commands


class InspectionTests(unittest.TestCase):
    def test_device_and_simulator(self):
        module.inspect_macho(fixture(), 2)
        module.inspect_macho(fixture(7), 7)

    def test_wrong_variant(self):
        with self.assertRaises(ValueError):
            module.inspect_macho(fixture(), 7)

    def test_wrong_architecture(self):
        with self.assertRaises(ValueError):
            module.inspect_macho(fixture(cpu=0x01000007), 2)

    def test_truncation(self):
        with self.assertRaises(ValueError):
            module.inspect_macho(fixture()[:-1], 2)


if __name__ == '__main__':
    unittest.main()
