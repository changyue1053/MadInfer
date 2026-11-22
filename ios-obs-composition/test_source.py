#!/usr/bin/env python3
"""Portable structural tests only; fake archive never qualifies native linkage."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
spec = importlib.util.spec_from_file_location('prepare', Path(__file__).with_name('prepare.py'))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)

class SourceTests(unittest.TestCase):
    def package(self, root):
        package = root / 'installed'
        for name in p.REQUIRED:
            path = package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'!<arch>\n' if name.endswith('.a') else b'// structural fixture only\n')
        return package

    def test_off_no_obs_dependency(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / 'consumer'
            p.prepare(dest, 'off')
            for f in dest.rglob('*'):
                if f.suffix in ('.m', '.h', '.swift', '.pbxproj'):
                    self.assertNotIn('madobs', f.read_text().lower())
            self.assertFalse((dest / 'Tests/Adapter.cpp').exists())
            self.assertFalse((dest / 'madobs-installed').exists())

    def test_on_relocates_and_preserves_positive_contract(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); package = self.package(root); dest = root / 'consumer'
            p.prepare(dest, 'on', package, root / 'removed-producer')
            self.assertFalse(package.exists())
            self.assertTrue((dest / 'madobs-installed/lib/libmadobs_runtime.a').is_file())
            source = (dest / 'Tests/NativeChecks.m').read_text()
            original = (p.ROOT / 'ios/Synthetic/PositiveChecks.m').read_text()
            restored = source.replace('#include "Adapter.h"\n', '', 1).replace((p.HERE / 'AfterJoin.m').read_text(), '', 1)
            self.assertTrue(restored.endswith(original))
            self.assertLess(source.index('madinfer_core_destroy(core);', source.index('NSString *MadInferCheckPositive')), source.index('madinfer_test_obs_after_join'))
            project = (dest / 'MadInferSmoke.xcodeproj/project.pbxproj').read_text()
            self.assertEqual(project.count('"OTHER_LDFLAGS"'), 2)
            self.assertEqual(project.count('"000000000000000000000202"'), 2)
            self.assertEqual((dest / 'Tests/MadInferSmokeTests.swift').read_text().count('func test'), 5)

    def test_zero_delta_forwarding(self):
        # Adapter has no content/delta entry point or access to inference types.
        header = (p.HERE / 'Adapter.h').read_text()
        code = (p.HERE / 'Adapter.cpp').read_text()
        self.assertNotIn('madinfer_delta', header + code)
        self.assertNotIn('telemetry_send', header + code)
        self.assertNotIn('on_delta', (p.HERE / 'AfterJoin.m').read_text())
        self.assertNotIn('attribute_count =', code)
        original = (p.ROOT / 'ios/Synthetic/PositiveChecks.m').read_text()
        self.assertNotIn('madobs', original)

    def test_reject_live_producer_and_package_escape(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); package = self.package(root); producer = root / 'producer'
            producer.mkdir()
            with self.assertRaises(ValueError): p.prepare(root/'consumer', 'on', package, producer)
            producer.rmdir()
            (package/'escape').symlink_to(root)
            with self.assertRaises(ValueError): p.prepare(root/'consumer', 'on', package, producer)
            self.assertFalse((root/'consumer').exists())

    def test_linker_path_token_attacks_rejected_before_move(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); package = self.package(root)
            for name in ('has space', 'has\ttab', 'has\nnewline', 'quote"',
                         "quote'", '$(injected)', 'semi;colon', 'back`tick',
                         'back\\slash'):
                dest = root / name
                with self.assertRaises(ValueError):
                    p.prepare(dest, 'on', package, root/'gone')
                self.assertFalse(dest.exists())
                self.assertTrue(package.exists())

    def test_reject_missing_bad_archive_and_off_arguments(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); package = self.package(root)
            with self.assertRaises(ValueError): p.prepare(root/'consumer', 'off', package)
            with self.assertRaises(ValueError): p.prepare(root/'consumer', 'on')
            (package/'lib/libmadobs_runtime.a').write_bytes(b'not an archive')
            with self.assertRaises(ValueError): p.prepare(root/'consumer', 'on', package, root/'gone')

if __name__ == '__main__': unittest.main()
