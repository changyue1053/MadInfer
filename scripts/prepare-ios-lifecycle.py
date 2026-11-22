#!/usr/bin/env python3
"""Augment only the generated independent installed iOS consumer."""
import importlib.util
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('positive',ROOT/'scripts/prepare-ios-positive.py')
positive=importlib.util.module_from_spec(spec);spec.loader.exec_module(positive)

def prepare(dest=positive.DEST):
    positive.prepare(dest)
    for name,addition in (('NativeChecks.m','Checks.m'),('MadInferSmokeTests.swift','Tests.swift')):
        with (dest/'Tests'/name).open('a') as f:
            f.write('\n'+(ROOT/'ios/Lifecycle'/addition).read_text())
    with (dest/'Tests/NativeChecks.h').open('a') as f:
        f.write('\nFOUNDATION_EXPORT NSString * _Nullable MadInferCheckLifecycle(NSString * _Nullable path);\n')

if __name__=='__main__':prepare()
