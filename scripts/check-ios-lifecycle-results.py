#!/usr/bin/env python3
"""Exact standalone installed iOS lifetime + original five positive/error cases."""
import importlib.util
from pathlib import Path
import sys
spec=importlib.util.spec_from_file_location('positive',Path(__file__).with_name('check-ios-positive-results.py'))
positive=importlib.util.module_from_spec(spec);spec.loader.exec_module(positive)
gate=positive.gate
gate.EXPECTED+=('testInstalledNativePendingCancelAndDestroy',)

if __name__=='__main__':
    try:
        gate.main()
    except gate.ResultRejected as error:
        print(str(error),file=sys.stderr)
        sys.exit(1)
    except Exception:
        print('FAIL: unexpected lifecycle XCTest parser error',file=sys.stderr)
        sys.exit(1)
