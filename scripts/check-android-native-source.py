#!/usr/bin/env python3
"""Verify vendored engine and actual CMake Android cross configuration, fail closed."""
import hashlib
import json
import shlex
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

def check_commands(entries, ndk, source, build):
    """Read actual CMake compile commands, not templates or inferred host defaults."""
    if not isinstance(entries, list) or not 5 <= len(entries) <= 2000:
        raise ValueError('Invalid native compile database')
    tools = ndk/'toolchains/llvm/prebuilt/linux-x86_64'
    seen = set()
    languages = set()
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get('directory'), str) or not isinstance(entry.get('file'), str):
            raise ValueError('Invalid compile entry schema')
        directory = Path(entry['directory']).resolve()
        if not directory.is_relative_to(build.resolve()):
            raise ValueError('Compile directory outside actual build')
        file = Path(entry['file'])
        file = (directory/file).resolve()
        if not (file.is_relative_to((source/'third_party/llama.cpp').resolve()) or file == (source/'crates/madinfer-backend-llama/csrc/madinfer_llama_bridge.c').resolve()):
            raise ValueError('Compile source outside pinned native graph')
        language = 'C' if file.suffix == '.c' else 'CXX' if file.suffix in ('.cpp','.cc','.cxx') else None
        if language is None:
            raise ValueError('Unsupported native source language')
        if 'arguments' in entry:
            args = entry['arguments']
        else:
            if not isinstance(entry.get('command'), str):
                raise ValueError('Missing compile command')
            args = shlex.split(entry['command'])
        if not isinstance(args, list) or not args or not all(isinstance(a,str) for a in args):
            raise ValueError('Invalid compile arguments')
        expected = tools/'bin'/('clang' if language == 'C' else 'clang++')
        if Path(args[0]).name != expected.name or Path(args[0]).resolve() != expected.resolve():
            raise ValueError('Wrong native compiler or language driver')
        if any(a.startswith(('@','-target=','-x=','-isysroot')) or a in ('-x','-arch','-m32','-m64') for a in args):
            raise ValueError('Unsupported hidden or language/architecture override')
        def values(long, short=None):
            found=[]
            for i,a in enumerate(args[1:],1):
                if a.startswith(long+'='): found.append(a[len(long)+1:])
                elif a == long or (short and a == short):
                    if i+1>=len(args): raise ValueError('Incomplete compile option')
                    found.append(args[i+1])
            return found
        if values('--target','-target') not in (['aarch64-none-linux-android24'],['aarch64-linux-android24']):
            raise ValueError('Wrong or conflicting Android API24 target')
        roots=values('--sysroot')
        if len(roots)!=1 or Path(roots[0]).resolve()!=(tools/'sysroot').resolve():
            raise ValueError('Wrong Android sysroot')
        if args.count('-c')!=1 or (directory/args[args.index('-c')+1]).resolve()!=file:
            raise ValueError('Command does not compile declared source')
        outputs=values('-o')
        if len(outputs)!=1:
            raise ValueError('Missing unique compile object')
        output=(directory/outputs[0]).resolve()
        if not output.is_relative_to(build.resolve()):
            raise ValueError('Compile object outside actual build')
        targets=re.findall(r'CMakeFiles/([^/]+)\.dir/',output.as_posix())
        if len(targets)!=1 or targets[0] not in {'madinfer_llama_bridge','llama','ggml','ggml-base','ggml-cpu'}:
            raise ValueError('Unexpected native graph target')
        if targets[0]=='ggml-cpu' and values('-march')!=['armv8-a']:
            raise ValueError('Unverified ARM baseline compile flags')
        if '-march=native' in args or any(a.startswith('-mcpu=') for a in args):
            raise ValueError('Host CPU specialization forbidden')
        seen.add(targets[0]); languages.add(language)
    if seen != {'madinfer_llama_bridge','llama','ggml','ggml-base','ggml-cpu'} or languages != {'C','CXX'}:
        raise ValueError('Incomplete native graph or language coverage')


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
        database=caches[0].parent/'compile_commands.json'
        if database.stat().st_size > 16*1024*1024:
            raise ValueError('Oversized compile database')
        check_commands(json.loads(database.read_text()),ndk,Path.cwd(),caches[0].parent)
    print('PASS: pinned engine source' + (' and Android cross configuration' if len(sys.argv)>1 else ''))

if __name__ == '__main__': main()
