#!/usr/bin/env python3
"""Mandatory old ABI/error checks plus both synthetic CPU outputs, no skip."""
import importlib.util
from pathlib import Path
import sys
spec = importlib.util.spec_from_file_location('gate', Path(__file__).with_name('check-ios-test-results.py'))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
gate.EXPECTED += ('testNativeSyntheticAProducesAAA', 'testNativeSyntheticBProducesBBB')
if __name__ == '__main__':
    try:
        gate.main()
    except gate.ResultRejected as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except Exception:
        print('FAIL: unexpected positive XCTest parser error', file=sys.stderr)
        sys.exit(1)
