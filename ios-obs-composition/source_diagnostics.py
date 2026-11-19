#!/usr/bin/env python3
"""Run unchanged source fixtures; emit only fixed case/category diagnostics."""
import contextlib
import importlib.util
import os
from pathlib import Path
import sys
import unittest

CASES = (
    'test_linker_path_token_attacks_rejected_before_move',
    'test_off_no_obs_dependency',
    'test_on_relocates_and_preserves_positive_contract',
    'test_reject_live_producer_and_package_escape',
    'test_reject_missing_bad_archive_and_off_arguments',
    'test_zero_delta_forwarding',
)
CATEGORIES = {AssertionError: 'assertion', ValueError: 'value',
              FileNotFoundError: 'file-missing', FileExistsError: 'file-exists',
              PermissionError: 'permission', OSError: 'filesystem',
              TypeError: 'type', AttributeError: 'attribute',
              RuntimeError: 'runtime'}

class FixedResult(unittest.TestResult):
    def __init__(self):
        super().__init__()
        self.records = []
        self.bad = False
    def record(self, test, category):
        self.bad = True
        name = getattr(test, '_testMethodName', None)
        if name not in CASES: name = 'unknown-case'
        self.records.append((name, category))
    def addFailure(self, test, err):
        self.record(test, CATEGORIES.get(err[0], 'other-exception'))
    def addError(self, test, err):
        self.record(test, CATEGORIES.get(err[0], 'other-exception'))
    def addSkip(self, test, reason):
        self.record(test, 'skipped')
    def addExpectedFailure(self, test, err):
        self.record(test, 'expected-failure')
    def addUnexpectedSuccess(self, test):
        self.record(test, 'unexpected-success')
    def addSubTest(self, test, subtest, err):
        if err is not None: self.record(test, CATEGORIES.get(err[0], 'other-exception'))

def render(result):
    # Values cannot be formatted from exceptions, IDs, traceback or paths.
    lines=['FAIL: source-fixture case='+name+' category='+category
           for name,category in result.records]
    if result.testsRun != len(CASES):
        lines.append('FAIL: source-fixture category=case-count')
    good=not result.bad and result.testsRun==len(CASES)
    if good: lines.append('PASS: composition-source-fixtures')
    return good,lines

def main():
    try:
        with open(os.devnull, 'w') as sink, contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            spec=importlib.util.spec_from_file_location('source_fixture',Path(__file__).with_name('test_source.py'))
            module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
            names=unittest.defaultTestLoader.getTestCaseNames(module.SourceTests)
            if tuple(names)!=CASES:
                raise RuntimeError('case set')
            suite=unittest.TestSuite(module.SourceTests(name) for name in CASES)
            result=FixedResult();suite.run(result)
            good,lines=render(result)
    except BaseException:
        good=False;lines=['FAIL: source-fixture category=runner']
    for line in lines: print(line)
    return 0 if good else 1

if __name__=='__main__':sys.exit(main())
