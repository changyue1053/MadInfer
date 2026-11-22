#!/usr/bin/env python3
"""Extend only the generated app-owned consumer; no acquisition/build/launch."""
import argparse
import hashlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
FIXTURE_BLOB = 'e95856a72e6b68c0b8f52a5d6b823c3756171d4a'
CASE = 'testInstalledNativePendingCancelAndDestroy'


def once(text, old, new):
    if text.count(old) != 1:
        raise ValueError('lifecycle-source-contract')
    return text.replace(old, new, 1)


def read(path):
    if path.resolve(strict=True) != path or not path.is_file():
        raise ValueError('lifecycle-source-path')
    return path.read_text(encoding='utf-8')


def pinned_fixture(path=HERE / 'Fixture.m'):
    raw = path.read_bytes()
    object_id = hashlib.sha1(f'blob {len(raw)}\0'.encode('ascii') + raw).hexdigest()
    if object_id != FIXTURE_BLOB:
        raise ValueError('lifecycle-fixture-pin')
    return raw.decode('utf-8')


def transform_fixture(mode, path=HERE / 'Fixture.m'):
    if mode not in ('off', 'on'):
        raise ValueError('lifecycle-mode-or-path')
    original = pinned_fixture(path)
    text = original.replace('SmokeState', 'MIDemoSmokeState')
    text = once(text, 'MadInferCheckLifecycle', 'MIDemoInternalCheckLifecycle')
    if mode == 'on':
        text = once(text, '    return nil;\n}', read(HERE / 'ObserveAfterJoin.m') + '    return nil;\n}')
    return text


def transform_adapter(source):
    old = '''extern "C" int32_t madinfer_test_obs_after_join(const char *wal_dir,
    uint64_t started, uint64_t completed, uint64_t failed) {
  // Reject duplicates, failures and absent observations before any runtime IO.
  if (!wal_dir || started != 1 || completed != 1 || failed != 0) return 0;'''
    new = '''static int32_t emit_after_join(const char *wal_dir, uint32_t first, uint32_t second) {
  const uint32_t events[2] = {first, second};'''
    source = once(source, old, new)
    source = once(source, 's->rules[i].event_id = i + 1; // actual STARTED, then COMPLETED',
                  's->rules[i].event_id = events[i]; // Exact bounded case profile.')
    source = once(source, 'event.event_id = i + 1;', 'event.event_id = events[i];')
    return source + '\n' + read(HERE / 'observe.cpp')


def apply(dest, mode):
    if mode not in ('off', 'on') or dest.resolve(strict=True) != dest:
        raise ValueError('lifecycle-mode-or-path')
    app, tests = dest / 'App', dest / 'Tests'
    driver, header = app / 'AppChecks.m', app / 'Driver.h'
    coordinator, swift = app / 'Coordinator.m', tests / 'MadInferSmokeTests.swift'
    adapter_h, adapter_cpp = app / 'Adapter.h', app / 'Adapter.cpp'
    packaged = dest / 'madobs-installed'
    if mode == 'off' and any(path.exists() or path.is_symlink() for path in (adapter_h, adapter_cpp, packaged)):
        raise ValueError('lifecycle-unexpected-observer')
    if mode == 'on' and (not packaged.is_dir() or packaged.resolve(strict=True) != packaged):
        raise ValueError('lifecycle-missing-observer')
    proper_driver = read(driver)
    if 'MIDemoInternalCheckLifecycle' in proper_driver:
        raise ValueError('lifecycle-source-already-extended')
    declaration = ('FOUNDATION_EXPORT NSString * _Nullable '
                   'MIDemoInternalCheckLifecycle(NSString * _Nullable path);\n')
    app_header = app / 'AppChecks.h'
    altered = {
        driver: proper_driver + '\n' + transform_fixture(mode),
        app_header: read(app_header) + '\n' + declaration,
        header: once(read(header), '#endif', declaration + '#endif'),
    }
    if mode == 'on':
        altered[adapter_h] = once(
            read(adapter_h), '#endif\n#endif\n', '#endif\n\n' + read(HERE / 'Observation.h') + '#endif\n')
        altered[adapter_cpp] = transform_adapter(read(adapter_cpp))
    ingress = once(read(coordinator), 'variant > 1', 'variant > 2')
    ingress = once(ingress, 'variant == 0 ? @"synthetic-A" : @"synthetic-B"',
                   'variant == 1 ? @"synthetic-B" : @"synthetic-A"')
    ingress = once(ingress, 'NSString *failure = MIDemoInternalCheckPositive(path, variant == 0 ? @"AAA" : @"BBB");',
                   'NSString *failure = variant == 2 ? MIDemoInternalCheckLifecycle(path) :\n'
                   '                MIDemoInternalCheckPositive(path, variant == 0 ? @"AAA" : @"BBB");')
    altered[coordinator] = ingress
    final = '    func testNativeSyntheticBProducesBBB() { runAppScenario(1) }\n}'
    altered[swift] = once(read(swift), final, final[:-1] + f'    func {CASE}() {{ runAppScenario(2) }}\n}}')
    # No PBX/scheme/framework/resource modifications; old target assignment
    # and exact artifact/receipt launch remain authoritative.
    for path, content in altered.items():
        path.write_text(content, encoding='utf-8')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=('off', 'on'), required=True)
    parser.add_argument('--dest', type=Path, required=True)
    args = parser.parse_args()
    try:
        apply(args.dest.absolute(), args.mode)
    except Exception:
        raise SystemExit('FAIL: lifecycle-overlay-contract') from None
