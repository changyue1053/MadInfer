#!/usr/bin/env python3
"""Consumer preparation only. No download, build, launch, signing or CI action."""
import argparse
import importlib.util
from pathlib import Path
import shutil
import re

ROOT = Path(__file__).resolve().parents[1]
HERE = Path(__file__).resolve().parent
MADOBS_COMMIT = '4af77ffa464a48307e5ff2d95f10b835919d205b'
MADOBS_TREE = '3a03801beb18c9601630cc4421b95c2743018dfb'
REQUIRED = ('lib/libmadobs_runtime.a', 'include/madobs/runtime.h',
            'include/madobs/observer.h', 'include/madobs/privacy.h',
            'include/imobs/imobs_c.h')

def replace_once(text, before, after):
    if text.count(before) != 1:
        raise ValueError('consumer source contract changed')
    return text.replace(before, after, 1)

def prepare(dest, mode, installed=None, producer=None):
    dest = Path(dest).resolve()
    if mode not in ('off', 'on') or dest.exists():
        raise ValueError('fresh destination and explicit mode required')
    if mode == 'off':
        if installed is not None or producer is not None:
            raise ValueError('off must not consume MadObs')
    else:
        # Xcode expands PROJECT_DIR inside OTHER_LDFLAGS before tokenization.
        # Reject whitespace and all shell/build-setting metacharacters rather
        # than relying on unproved double-layer PBX/shell quoting.
        if not re.fullmatch(r'/[A-Za-z0-9_./-]+', str(dest)):
            raise ValueError('on destination must be a token-safe absolute path')
        if installed is None or producer is None:
            raise ValueError('on requires preinstalled package and removed producer location')
        installed, producer = Path(installed).resolve(), Path(producer).absolute()
        if producer.exists() or producer.is_symlink():
            raise ValueError('producer must be absent before independent consumption')
        if not installed.is_dir() or installed == dest or installed in dest.parents or dest in installed.parents:
            raise ValueError('separate installed package required')
        # No symlink escape back into the producer or build directories.
        if any(p.is_symlink() for p in installed.rglob('*')):
            raise ValueError('installed package symlinks forbidden')
        for relative in REQUIRED:
            if not (installed / relative).is_file():
                raise ValueError('installed package incomplete')
        if (installed / 'lib/libmadobs_runtime.a').read_bytes()[:8] != b'!<arch>\n':
            raise ValueError('static archive required')
    spec = importlib.util.spec_from_file_location('positive', ROOT / 'scripts/prepare-ios-positive.py')
    positive = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(positive)
    positive.prepare(dest)
    if mode == 'off':
        return
    # Move, not reference, installed headers/archive. Upstream install provenance,
    # pinned source and architecture must be independently gated by orchestration.
    shutil.move(str(installed), str(dest / 'madobs-installed'))
    for filename in ('Adapter.h', 'Adapter.cpp'):
        shutil.copyfile(HERE / filename, dest / 'Tests' / filename)
    source = dest / 'Tests/NativeChecks.m'
    text = source.read_text()
    addition = (HERE / 'AfterJoin.m').read_text()
    original = (ROOT / 'ios/Synthetic/PositiveChecks.m').read_text()
    modified = replace_once(original, '        return failure;\n    }\n}', addition + '        return failure;\n    }\n}')
    text = replace_once(text, original, modified)
    source.write_text('#include "Adapter.h"\n' + text)
    project = dest / 'MadInferSmoke.xcodeproj/project.pbxproj'
    text = project.read_text()
    text = replace_once(text, '"objects" = {', '''"objects" = {
"000000000000000000000201" = { "isa" = "PBXFileReference"; "lastKnownFileType" = "sourcecode.cpp.cpp"; "path" = "Tests/Adapter.cpp"; "sourceTree" = "SOURCE_ROOT"; };
"000000000000000000000202" = { "isa" = "PBXBuildFile"; "fileRef" = "000000000000000000000201"; };''')
    text = replace_once(text, '"files" = ("00000000000000000000000D", "000000000000000000000013");',
                        '"files" = ("00000000000000000000000D", "000000000000000000000013", "000000000000000000000202");')
    # Test target only: production framework and app defaults are untouched.
    key = '"SWIFT_OBJC_BRIDGING_HEADER" = "$(PROJECT_DIR)/Tests/NativeChecks.h";'
    if text.count(key) != 2:
        raise ValueError('test configuration contract changed')
    text = text.replace(key, key + '''
"HEADER_SEARCH_PATHS" = ("$(inherited)", "$(PROJECT_DIR)/madobs-installed/include");
"CLANG_CXX_LANGUAGE_STANDARD" = "c++17";
"OTHER_LDFLAGS" = ("$(inherited)", "$(PROJECT_DIR)/madobs-installed/lib/libmadobs_runtime.a", "-lc++", "-lz");''')
    project.write_text(text)

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=('off', 'on'), required=True)
    parser.add_argument('--dest', type=Path, required=True)
    parser.add_argument('--installed', type=Path)
    parser.add_argument('--removed-producer', type=Path)
    args = parser.parse_args()
    prepare(args.dest, args.mode, args.installed, args.removed_producer)
