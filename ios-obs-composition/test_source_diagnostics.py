#!/usr/bin/env python3
"""Inject secret-shaped messages to prove only allowlisted diagnostics escape."""
import importlib.util
from pathlib import Path
import sys
import unittest
spec=importlib.util.spec_from_file_location('diagnostics',Path(__file__).with_name('source_diagnostics.py'))
d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)
class Tests(unittest.TestCase):
    def test_exception_redaction_and_status(self):
        for exception,category in ((AssertionError,'assertion'),(ValueError,'value'),
                                   (PermissionError,'permission'),(KeyError,'other-exception')):
            case=unittest.FunctionTestCase(lambda:None)
            case._testMethodName=d.CASES[0]
            result=d.FixedResult();result.testsRun=6
            try:raise exception('sensitive /private/path token=do-not-output')
            except Exception:result.addError(case,sys.exc_info())
            ok,lines=d.render(result)
            self.assertFalse(ok)
            self.assertEqual(lines,['FAIL: source-fixture case='+d.CASES[0]+' category='+category])
    def test_unknown_case_and_skip(self):
        case=unittest.FunctionTestCase(lambda:None);case._testMethodName='secret/path'
        result=d.FixedResult();result.testsRun=6
        result.addSkip(case,'secret-reason')
        self.assertEqual(d.render(result),(False,['FAIL: source-fixture case=unknown-case category=skipped']))
    def test_count_and_success(self):
        result=d.FixedResult()
        self.assertEqual(d.render(result),(False,['FAIL: source-fixture category=case-count']))
        result.testsRun=6
        self.assertEqual(d.render(result),(True,['PASS: composition-source-fixtures']))
        for name,(_,_,cases) in d.SUITES.items():
            result=d.FixedResult(cases,name)
            self.assertFalse(d.render(result)[0])
            result.testsRun=len(cases)
            self.assertEqual(d.render(result),(True,['PASS: composition-'+name+'-fixtures']))
            result.testsRun+=1
            self.assertFalse(d.render(result)[0])

if __name__=='__main__':unittest.main()
