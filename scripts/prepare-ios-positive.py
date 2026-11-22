#!/usr/bin/env python3
"""Generate an isolated independent consumer and resources; never alter the original."""
import hashlib
import importlib.util
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'build/ios/positive-consumer'
HASHES = {
    'A': '23696b2e27a65419bc4b81ddb49014063c09af369ea542188bb98b70c15ff9c2',
    'B': '1cfe72cce2f88fdae105088ad24574ff47421e1b8e9bdbcd569d7e8347cb3703',
}

def prepare(dest=DEST):
    if dest.exists():
        raise RuntimeError('Use a fresh positive consumer directory')
    spec = importlib.util.spec_from_file_location('generator', ROOT / 'scripts/generate-synthetic-gguf.py')
    generator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(generator)
    fixtures = {v: generator.generate(v) for v in HASHES}
    for v, data in fixtures.items():
        assert len(data) == 115584 and hashlib.sha256(data).hexdigest() == HASHES[v]
    shutil.copytree(ROOT / 'ios/Smoke', dest)
    for target, addition in [('NativeChecks.m', 'PositiveChecks.m'), ('MadInferSmokeTests.swift', 'PositiveTests.swift')]:
        path = dest / 'Tests' / target
        path.write_text(path.read_text() + '\n' + (ROOT / 'ios/Synthetic' / addition).read_text())
    with (dest / 'Tests/NativeChecks.h').open('a') as out:
        out.write('\nFOUNDATION_EXPORT NSString * _Nullable MadInferCheckPositive(NSString * _Nullable path, NSString * _Nonnull expected);\n')
    project = dest / 'MadInferSmoke.xcodeproj/project.pbxproj'
    source = project.read_text().replace('../../build/ios/MadInfer.xcframework', '../MadInfer.xcframework')
    entries = []
    for index, (variant, data) in enumerate(fixtures.items()):
        filename = f'synthetic-{variant}.gguf'
        (dest / filename).write_bytes(data)
        ref = f'00000000000000000000010{index}'
        build = f'00000000000000000000011{index}'
        entries.append(f'"{ref}" = {{ "isa" = "PBXFileReference"; "lastKnownFileType" = "file"; "path" = "{filename}"; "sourceTree" = "SOURCE_ROOT"; }};\n"{build}" = {{ "isa" = "PBXBuildFile"; "fileRef" = "{ref}"; }};')
    source = source.replace('"objects" = {', '"objects" = {\n' + '\n'.join(entries), 1)
    start = source.index('"000000000000000000000024" = {')
    prefix, resources = source[:start], source[start:]
    resources = resources.replace('"files" = ();', '"files" = ("000000000000000000000110", "000000000000000000000111");', 1)
    project.write_text(prefix + resources)
    print('PASS: generated isolated consumer and two hash-pinned synthetic resources')

if __name__ == '__main__':
    prepare()
