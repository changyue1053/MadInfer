#!/usr/bin/env python3
"""Portable structure only; Apple runtime evidence requires the named hosted gate."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[1]
CASE='testInstalledNativePendingCancelAndDestroy'

def load(filename):
    spec=importlib.util.spec_from_file_location(filename,Path(__file__).with_name(filename))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module

class Tests(unittest.TestCase):
    def test_generation_is_only_additive_to_positive_consumer(self):
        lifecycle=load('prepare-ios-lifecycle.py')
        with tempfile.TemporaryDirectory() as work:
            root=Path(work)
            lifecycle.prepare(root/'lifecycle')
            lifecycle.positive.prepare(root/'baseline')
            previous=root/'baseline'
            for file in previous.rglob('*'):
                if not file.is_file():continue
                actual=(root/'lifecycle'/file.relative_to(previous)).read_bytes()
                if file.name in ('NativeChecks.m','NativeChecks.h','MadInferSmokeTests.swift'):
                    self.assertTrue(actual.startswith(file.read_bytes()))
                else:self.assertEqual(actual,file.read_bytes())
            with self.assertRaises(RuntimeError):lifecycle.prepare(root/'lifecycle')

    def test_exact_named_results_reject_downgrades_and_duplicates(self):
        old=load('check-ios-positive-results.py').gate
        gate=load('check-ios-lifecycle-results.py').gate
        self.assertEqual(gate.EXPECTED,old.EXPECTED+(CASE,))
        summary=dict(totalTestCount=6,passedTests=6,failedTests=0,skippedTests=0)
        cases=[dict(nodeType='Test Case',nodeIdentifier='MadInferSmokeTests/'+name+'()',result='Passed') for name in gate.EXPECTED]
        gate.verify(summary,{'testNodes':cases})
        for nodes in (cases[:-1],cases[:-1]+cases[:1],cases[:-1]+[dict(cases[-1],result='Skipped')],
                      cases[:-1]+[dict(cases[-1],result='Failed')]):
            with self.assertRaises(gate.ResultRejected):gate.verify(summary,{'testNodes':nodes})

    def test_lifecycle_is_offline_and_bounded(self):
        source=(ROOT/'ios/Lifecycle/Checks.m').read_text()
        for required in ('event->code==3008','!event->has_partial_text','kind!=1',
                         'dispatch_semaphore_wait(second.terminal,DISPATCH_TIME_NOW)',
                         'objc_precise_lifetime','madinfer_stream_cancel(core,UINT64_MAX)'):
            self.assertIn(required,source)
        for forbidden in ('sleep','madinfer_host_reply','http://','https://','MADINFER_CONSENT_ALLOW'):
            self.assertNotIn(forbidden,source)
        self.assertEqual(source.count('madinfer_core_destroy('),1)
        original=(ROOT/'scripts/test-ios-positive-simulator.sh').read_text()
        new=(ROOT/'scripts/test-ios-lifecycle-simulator.sh').read_text()
        self.assertEqual(new,original.replace('check-ios-positive-results.py','check-ios-lifecycle-results.py'))

if __name__=='__main__':unittest.main()
