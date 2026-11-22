#!/usr/bin/env python3
"""Synthetic project/source checks only, not an Apple compile/runtime proof."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('demo_prepare',Path(__file__).with_name('prepare.py'))
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
class Tests(unittest.TestCase):
    def package(self,root):
        stage=root/'installed'
        for name in p.optional.REQUIRED:
            f=stage/name;f.parent.mkdir(parents=True,exist_ok=True)
            f.write_bytes(b'!<arch>\n' if name.endswith('.a') else b'// synthetic fixture\n')
        return stage
    def generate(self,root,mode):
        dest=root/'consumer'
        p.prepare(dest,mode,self.package(root) if mode=='on' else None,root/'removed' if mode=='on' else None)
        return dest
    def test_off_and_on_app_owns_driver_and_resources(self):
        for mode in ('off','on'):
            with tempfile.TemporaryDirectory() as tmp:
                d=self.generate(Path(tmp).resolve(),mode)
                driver=(d/'App/AppChecks.m').read_text()
                self.assertIn('MIDemoInternalCheckPositive',driver)
                self.assertIn('madinfer_core_destroy(core)',driver)
                self.assertIn('state.generated != 3',driver)
                tests=(d/'Tests/NativeChecks.m').read_text()
                self.assertEqual(tests,(p.ROOT/'ios/Smoke/Tests/NativeChecks.m').read_text())
                self.assertNotIn('CheckPositive',tests)
                swift=(d/'Tests/MadInferSmokeTests.swift').read_text()
                self.assertEqual(swift.count('func test'),5)
                self.assertIn('MIDemoSubmit(variant)',swift)
                project=(d/'MadInferSmoke.xcodeproj/project.pbxproj').read_text()
                spec=importlib.util.spec_from_file_location('app_qualification',p.HERE/'qualify.py')
                q=importlib.util.module_from_spec(spec);spec.loader.exec_module(q)
                q.project_boundary(d/'MadInferSmoke.xcodeproj/project.pbxproj',mode)
                file=d/'MadInferSmoke.xcodeproj/project.pbxproj'
                attack=project.replace('"000000000000000000000040" = {',
                    '"000000000000000000000040" = {\n"OTHER_LDFLAGS" = ("forbidden");',1)
                file.write_text(attack)
                with self.assertRaises(ValueError):q.project_boundary(file,mode)
                file.write_text(project)

                app_resources=project.split('"000000000000000000000020" = {')[1].split('"000000000000000000000022" = {')[0]
                self.assertIn('"000000000000000000000110"',app_resources)
                if mode=='on':
                    self.assertTrue((d/'App/Adapter.cpp').is_file())
                    self.assertFalse((d/'Tests/Adapter.cpp').exists())
                    self.assertIn('madinfer_test_obs_after_join',driver)
                    for target in ('000000000000000000000040','000000000000000000000041'):
                        block=project.split('"'+target+'" = {')[1].split('\n\t\t};',1)[0]
                        self.assertNotIn('madobs-installed',block)
                else:
                    self.assertNotIn('madobs',project.lower())
                    self.assertNotIn('madinfer_test_obs',driver)
    def test_exact_original_positive_checks_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=self.generate(Path(tmp).resolve(),'off')
            driver=(d/'App/AppChecks.m').read_text().replace('MIDemoInternalCheck','MadInferCheck').replace('MIDemoSmokeState','SmokeState').replace('MIDemoPositiveState','PositiveState')
            self.assertTrue(driver.endswith((p.ROOT/'ios/Synthetic/PositiveChecks.m').read_text()))
    def test_private_driver_declaration_and_observation_ownership(self):
        declaration='FOUNDATION_EXPORT NSString * _Nullable MIDemoInternalCheckPositive(NSString * _Nullable path, NSString * _Nonnull expected);'
        for mode in ('off','on'):
            with tempfile.TemporaryDirectory() as tmp:
                d=self.generate(Path(tmp).resolve(),mode)
                self.assertIn('#import "Driver.h"',(d/'App/Coordinator.m').read_text())
                self.assertIn(declaration,(d/'App/Driver.h').read_text())
                self.assertIn(declaration,(d/'App/AppChecks.h').read_text())
                driver=(d/'App/AppChecks.m').read_text()
                if mode=='on':
                    positive=driver[driver.index('NSString *MIDemoInternalCheckPositive'):]
                    self.assertLess(positive.index('madinfer_core_destroy(core)'),positive.index('madinfer_test_obs_after_join'))
                    self.assertIn('state.startedCount, state.completeCount, state.failedCount',positive)
                    self.assertIn('failure = @"Optional observation admission/drain mismatch"',positive)
                    adapter=(d/'App/Adapter.cpp').read_text()
                    for predicate in ('stats.enqueued == 2','stats.drained == 2','stats.queue_dropped == 0','stats.wal_dropped == 0'):
                        self.assertIn(predicate,adapter)
                for f in (d/'Tests').iterdir():
                    if f.suffix in ('.m','.h','.swift'):
                        self.assertNotIn('madinfer_test_obs_after_join',f.read_text())
                        self.assertNotIn('madobs_emit_generic_event',f.read_text())

    def test_ingress_has_no_arbitrary_path_and_ui_uses_same_entry(self):
        header=(p.HERE/'Demo.h').read_text()
        self.assertIn('uint32_t variant',header)
        self.assertNotIn('NSString',header)
        coordinator=(p.HERE/'Coordinator.m').read_text()
        self.assertIn('variant > 1',coordinator)
        self.assertLess(coordinator.index('if (busy)'),coordinator.index('dispatch_async(worker'))
        self.assertIn('[[NSBundle mainBundle] pathForResource:',coordinator)
        self.assertIn('if (status == 0) busy = NO',coordinator)
        self.assertIn('dispatch_get_main_queue()',coordinator)
        self.assertIn('MIDemoSubmit', (p.HERE/'AppDelegate.swift').read_text())
        self.assertNotIn('print(', (p.HERE/'AppDelegate.swift').read_text())
if __name__=='__main__':unittest.main()
