#!/usr/bin/env python3
"""Verify vendored engine and actual CMake Android cross configuration, fail closed."""
import hashlib
from pathlib import Path
import re
import sys
import subprocess

ENGINE_HASH = '6b48c4a979aa11de3018a13c176c4c648521d534afefa7c552148e89ec4a68b2'

def check_cmake_version(line):
    # Android SDK's pinned 3.22.1 package may expose its Kitware git revision.
    if not re.fullmatch(r'cmake version 3\.22\.1(?:-g[0-9a-f]{7,40})?\r?\n?', line):
        raise ValueError('Unexpected SDK CMake version')


def check_cache(text, ndk):
    values = dict(re.findall(r'^([^#/:][^:=]*):[^=]+=(.*)$', text, re.M))
    for key, expected in {'ANDROID_ABI':'arm64-v8a', 'ANDROID_PLATFORM':'android-24',
                          'ANDROID_STL':'c++_static', 'GGML_CPU_ARM_ARCH':'armv8-a',
                          'GGML_NATIVE':'OFF', 'GGML_METAL':'OFF', 'GGML_OPENMP':'OFF',
                          'GGML_ACCELERATE':'OFF', 'GGML_BLAS':'OFF'}.items():
        if values.get(key) != expected:
            raise ValueError('Android native CMake option mismatch: '+key)
    if Path(values.get('CMAKE_TOOLCHAIN_FILE','')).resolve() != (ndk/'build/cmake/android.toolchain.cmake').resolve():
        raise ValueError('Wrong NDK toolchain')
    for language in ('C','CXX'):
        compiler = Path(values.get('CMAKE_'+language+'_COMPILER','')).resolve()
        if not compiler.is_relative_to((ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin').resolve()):
            raise ValueError('Compiler outside pinned NDK')

def main():
    if sys.argv[1:] == ['--cmake-version']:
        check_cmake_version(sys.stdin.read())
        return
    root = Path('third_party/llama.cpp')
    files = sorted((p for p in root.rglob('*') if p.is_file()), key=lambda p:p.relative_to(root).as_posix().encode())
    lines = b''.join(hashlib.sha256(p.read_bytes()).hexdigest().encode()+b'  ./'+p.relative_to(root).as_posix().encode()+b'\n' for p in files)
    assert len(files)==373 and hashlib.sha256(lines).hexdigest()==ENGINE_HASH
    assert not subprocess.check_output(['git','ls-files','*.gguf'])
    if len(sys.argv)>1:
        target,ndk=map(Path,sys.argv[1:])
        caches=list((target/'aarch64-linux-android/release/build').glob('madinfer-backend-llama-*/out/llama-build/CMakeCache.txt'))
        if len(caches)!=1:
            raise ValueError('Expected exactly one actual Android engine build')
        check_cache(caches[0].read_text(),ndk)
        cmake_dir=caches[0].parent/'CMakeFiles'
        for language in ('C','CXX'):
            configs=list(cmake_dir.glob('*/CMake'+language+'Compiler.cmake'))
            if len(configs)!=1 or not re.search(r'set\(CMAKE_'+language+r'_COMPILER_TARGET "aarch64(?:-none)?-linux-android24"\)',configs[0].read_text()):
                raise ValueError('Missing exact Android API24 compiler target')
        flags=list(caches[0].parent.glob('**/ggml-cpu.dir/flags.make'))
        if len(flags)!=1 or '-march=armv8-a' not in flags[0].read_text() or '-march=native' in flags[0].read_text():
            raise ValueError('Unverified ARM baseline compile flags')
    print('PASS: pinned engine source' + (' and Android cross configuration' if len(sys.argv)>1 else ''))

if __name__ == '__main__': main()
