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
    def __init__(self, cases=CASES, suite="source"):
        super().__init__()
        self.cases=cases
        self.suite=suite
        self.records = []
        self.bad = False
    def record(self, test, category):
        self.bad = True
        name = getattr(test, '_testMethodName', None)
        if name not in self.cases: name = 'unknown-case'
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
    lines=['FAIL: '+result.suite+'-fixture case='+name+' category='+category
           for name,category in result.records]
    if result.testsRun != len(result.cases):
        lines.append('FAIL: '+result.suite+'-fixture category=case-count')
    good=not result.bad and result.testsRun==len(result.cases)
    if good: lines.append('PASS: composition-'+result.suite+'-fixtures')
    return good,lines

SUITES = {
    'source': ('test_source.py', 'SourceTests', CASES),
    'orchestration': ('test_orchestration.py', 'Tests', (
        'test_canonical_symlink_ancestor', 'test_exact_bundle_layouts',
        'test_fixed_distinct_preparation_labels', 'test_git_header_status_gate',
        'test_installed_and_removed_before_consumption', 'test_simulator_platform',
        'test_stdlib_inspect_not_shadowed', 'test_symbols_on_off', 'test_workflow_boundaries')),
    'execution': ('test_execution.py', 'Tests', (
        'test_bundle_pointer_mismatch', 'test_execution_canonicalizes_symlink_ancestor',
        'test_invalid_schema_modes_and_paths', 'test_original_simulator_contract_unchanged',
        'test_selection_and_hash_changes', 'test_versions_and_layouts')),
    'diagnostics': ('test_source_diagnostics.py', 'Tests', (
        'test_count_and_success', 'test_exception_redaction_and_status',
        'test_unknown_case_and_skip')),
}

def main(suite_name='source'):
    if suite_name not in SUITES:
        print('FAIL: fixture category=unknown-suite')
        return 1
    try:
        with open(os.devnull, 'w') as sink, contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            filename,classname,cases=SUITES[suite_name]
            spec=importlib.util.spec_from_file_location('selected_fixture',Path(__file__).with_name(filename))
            module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
            cls=getattr(module,classname)
            names=unittest.defaultTestLoader.getTestCaseNames(cls)
            if tuple(names)!=cases:
                raise RuntimeError('case set')
            suite=unittest.TestSuite(cls(name) for name in cases)
            result=FixedResult(cases,suite_name);suite.run(result)
            good,lines=render(result)
    except BaseException:
        good=False;lines=['FAIL: '+suite_name+'-fixture category=runner']
    for line in lines: print(line)
    return 0 if good else 1

if __name__=='__main__':
    if len(sys.argv)>2:
        print('FAIL: fixture category=arguments');sys.exit(1)
    sys.exit(main(sys.argv[1] if len(sys.argv)==2 else 'source'))
