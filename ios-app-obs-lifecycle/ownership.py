#!/usr/bin/env python3
"""Additional symbol assertions; original exact launch authority stays unchanged."""
import argparse
import importlib.util
from pathlib import Path
import os

spec = importlib.util.spec_from_file_location(
    'original_app_qualification', Path(__file__).resolve().parents[1] / 'ios-app-obs-demo/qualify.py')
original = importlib.util.module_from_spec(spec)
spec.loader.exec_module(original)
NATIVE = '_MIDemoInternalCheckLifecycle'
OBS = '_madinfer_test_obs_lifecycle_after_join'


def boundary(app, test_defined, test_imports, mode):
    if mode not in ('off', 'on') or NATIVE not in app or ((OBS in app) != (mode == 'on')):
        raise ValueError('app-lifecycle-symbol')
    if (NATIVE in test_defined or NATIVE in test_imports or OBS in test_defined or OBS in test_imports or
            {s for s in test_imports if s.startswith('_MIDemo')} != {'_MIDemoSubmit'}):
        raise ValueError('test-lifecycle-symbol')


def inspect(mode):
    # Use exactly the same canonical product selection, installed frameworks,
    # closure and stock binary inspectors as the existing app-owned lane.
    _, test, app = original.verify(original.base.PRODUCTS.resolve(), mode)
    nm, symbols = original.a.run, original.a.symbols
    boundary(symbols(nm('xcrun', 'nm', '-g', '-U', str(app))),
             symbols(nm('xcrun', 'nm', '-g', '-U', str(test))),
             symbols(nm('xcrun', 'nm', '-u', str(test))), mode)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=('off', 'on'), required=True)
    args = parser.parse_args()
    try:
        inspect(args.mode)
    except Exception:
        os.write(3, b'FAIL: app-cancellation-ownership\n')
        raise SystemExit(1) from None
    os.write(3, b'PASS: app-cancellation-ownership\n')
