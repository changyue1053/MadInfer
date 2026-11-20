#!/usr/bin/env python3
"""Portable structural tests. Synthetic archives cannot qualify native linkage."""
import hashlib
import importlib.util
from pathlib import Path
import re
import tempfile
import unittest

import overlay
import ownership
import results

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
BASE = ROOT / 'ios-app-obs-demo'
spec = importlib.util.spec_from_file_location('app_owned_preparer', BASE / 'prepare.py')
original = importlib.util.module_from_spec(spec)
spec.loader.exec_module(original)
FIVE = ('testABIVersionTwo', 'testNativeMissingFileIs4003NotFeatureDisabled',
        'testNativeSelectionAndCorruptNonModelFailure', 'testNativeSyntheticAProducesAAA',
        'testNativeSyntheticBProducesBBB')
SIX = FIVE + (overlay.CASE,)


class TestOverlay(unittest.TestCase):
    def setup(self, root, mode):
        dest = root / 'consumer'
        installed = root / 'relocated'
        if mode == 'on':
            for path in original.optional.REQUIRED:
                f = installed / path
                f.parent.mkdir(parents=True, exist_ok=True)
                f.write_bytes(b'!<arch>\n' if path.endswith('.a') else b'// synthetic fixture')
        original.prepare(dest, mode, installed if mode == 'on' else None,
                         root / 'removed' if mode == 'on' else None)
        return dest

    def test_exact_fixture_transform_preserves_join_no_partial_and_never_authorizes(self):
        fixture = overlay.pinned_fixture()
        self.assertEqual(overlay.transform_fixture('off'), fixture.replace('SmokeState', 'MIDemoSmokeState')
                         .replace('MadInferCheckLifecycle', 'MIDemoInternalCheckLifecycle'))
        on = overlay.transform_fixture('on')
        call = on.index('madinfer_test_obs_lifecycle_after_join')
        self.assertLess(on.index('madinfer_core_destroy(core)'), call)
        self.assertLess(on.index('lifecycle_exact(second,secondID,YES)'), call)
        self.assertIn('!event->has_partial_text && !event->partial_text.len', on)
        self.assertNotIn('madinfer_authorize_reply', on)
        self.assertNotIn('madinfer_test_obs_lifecycle_after_join', overlay.transform_fixture('off'))
        with tempfile.TemporaryDirectory() as tmp:
            f = Path(tmp) / 'other.m'
            f.write_text(fixture + 'tampered')
            with self.assertRaises(ValueError):
                overlay.transform_fixture('on', f)

    def test_both_modes_preserve_graph_resources_and_exact_test_ownership(self):
        for mode in ('off', 'on'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                dest = self.setup(Path(tmp).resolve(), mode)
                prior = {f.relative_to(dest).as_posix(): hashlib.sha256(f.read_bytes()).digest()
                         for f in dest.rglob('*') if f.is_file()}
                overlay.apply(dest, mode)
                after = {f.relative_to(dest).as_posix(): hashlib.sha256(f.read_bytes()).digest()
                         for f in dest.rglob('*') if f.is_file()}
                edited = {'App/AppChecks.m', 'App/AppChecks.h', 'App/Driver.h', 'App/Coordinator.m',
                          'Tests/MadInferSmokeTests.swift'}
                if mode == 'on':
                    edited |= {'App/Adapter.h', 'App/Adapter.cpp'}
                self.assertEqual(set(after), set(prior))
                self.assertEqual({p for p in prior if prior[p] != after[p]}, edited)
                test = (dest / 'Tests/MadInferSmokeTests.swift').read_text()
                self.assertEqual(tuple(re.findall(r'func (test[A-Za-z0-9]+)\(', test)), SIX)
                self.assertIn(f'func {overlay.CASE}() {{ runAppScenario(2) }}', test)
                self.assertNotIn('madinfer_test_obs_lifecycle', test)
                self.assertNotIn('MIDemoInternalCheckLifecycle', test)
                ingress = (dest / 'App/Coordinator.m').read_text()
                self.assertIn('variant > 2', ingress)
                self.assertIn('variant == 2 ? MIDemoInternalCheckLifecycle(path)', ingress)
                self.assertIn('variant == 1 ? @"synthetic-B" : @"synthetic-A"', ingress)
                project = dest / 'MadInferSmoke.xcodeproj/project.pbxproj'
                ownership.original.project_boundary(project, mode)
                self.assertEqual((dest / 'madobs-installed').exists(), mode == 'on')
                self.assertEqual((dest / 'App/Adapter.cpp').exists(), mode == 'on')
                with self.assertRaises(ValueError):
                    overlay.apply(dest, mode)

    def test_exact_case_parser_rejects_wrong_duplicate_or_partial_result(self):
        self.assertEqual(results.gate.EXPECTED, SIX)
        summary = {'totalTestCount': 6, 'passedTests': 6, 'failedTests': 0, 'skippedTests': 0}
        def nodes(names):
            return {'testNodes': [{'nodeType': 'Test Case', 'result': 'Passed',
                                   'nodeIdentifier': 'MadInferSmokeTests/' + name + '()'} for name in names]}
        results.gate.verify(summary, nodes(SIX))
        for bad in (FIVE, FIVE + (FIVE[-1],), FIVE + ('testUnreviewed',), SIX + ('testUnreviewed',)):
            with self.assertRaises(results.gate.ResultRejected):
                results.gate.verify(summary, nodes(bad))
        with self.assertRaises(results.gate.ResultRejected):
            results.gate.verify({**summary, 'skippedTests': 1}, nodes(SIX))

    def test_record_is_two_typed_fields_and_adapter_keeps_old_profile(self):
        header = (HERE / 'Observation.h').read_text()
        shape = r'typedef struct \{\s*mi_demo_cancellation_status explicit_cancel;\s*mi_demo_cancellation_status destroy_cancel;\s*\} mi_demo_cancellation_record;'
        self.assertRegex(header, shape)
        adapter = overlay.transform_adapter((ROOT / 'ios-obs-composition/Adapter.cpp').read_text())
        for required in ('started != 1 || completed != 1 || failed != 0', 'emit_after_join(wal_dir, 1, 2)',
                         'record->explicit_cancel != MI_DEMO_EXPLICIT_CANCELLATION',
                         'record->destroy_cancel != MI_DEMO_DESTROY_CANCELLATION',
                         'emit_after_join(wal_dir, MI_DEMO_EXPLICIT_CANCELLATION, MI_DEMO_DESTROY_CANCELLATION)',
                         'stats.enqueued == 2 && stats.drained == 2', 'stats.queue_dropped == 0 && stats.wal_dropped == 0'):
            self.assertIn(required, adapter)

    def test_strict_symbol_contract_app_only_even_when_on(self):
        submit = {'_MIDemoSubmit'}
        for mode in ('off', 'on'):
            app = {ownership.NATIVE} | ({ownership.OBS} if mode == 'on' else set())
            ownership.boundary(app, set(), submit, mode)
            for wrong in ((app, set(), submit | {ownership.NATIVE}), (app, {ownership.OBS}, submit),
                          (app, set(), submit | {ownership.OBS}), (app - {ownership.NATIVE}, set(), submit)):
                with self.assertRaises(ValueError):
                    ownership.boundary(*wrong, mode)
        with self.assertRaises(ValueError):
            ownership.boundary({ownership.NATIVE, ownership.OBS}, set(), submit, 'off')

    def test_same_explicit_receipt_launcher_and_exact_no_model_workflow_boundary(self):
        self.assertEqual((HERE / 'simulator.sh').read_text(),
                         (BASE / 'simulator.sh').read_text().replace('scripts/check-ios-positive-results.py',
                                                                    'ios-app-obs-lifecycle/results.py'))
        wf = (ROOT / '.github/workflows/ios-app-obs-lifecycle.yml').read_text()
        off, on = wf.split('  optional-on:\n', 1)
        self.assertNotIn('repository: changyue1053/MadObs', off)
        self.assertIn('ref: 4af77ffa464a48307e5ff2d95f10b835919d205b', on)
        self.assertNotIn('tools/models/fetch', wf)
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        branch = 'checkpoint/madinfer-ios-app-obs-lifecycle'
        self.assertIn(f"github.ref == 'refs/heads/{branch}'", ci)
        self.assertIn(f"github.head_ref == '{branch}'", ci)


if __name__ == '__main__':
    unittest.main()
