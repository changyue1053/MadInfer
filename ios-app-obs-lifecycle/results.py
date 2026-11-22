#!/usr/bin/env python3
"""Use the original result policy with exactly one more named XCTest."""
import importlib.util
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location(
    'original_positive_results', Path(__file__).resolve().parents[1] / 'scripts/check-ios-positive-results.py')
positive = importlib.util.module_from_spec(spec)
spec.loader.exec_module(positive)
gate = positive.gate
gate.EXPECTED += ('testInstalledNativePendingCancelAndDestroy',)

if __name__ == '__main__':
    try:
        gate.main()
    except gate.ResultRejected as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except Exception:
        print('FAIL: unexpected app cancellation XCTest parser error', file=sys.stderr)
        sys.exit(1)
