#!/usr/bin/env python3
"""Verify vendored engine and actual CMake Android cross configuration, fail closed."""
import hashlib
import os
import json
import shlex
from pathlib import Path
import re
import sys
import subprocess

ENGINE_HASH = '6b48c4a979aa11de3018a13c176c4c648521d534afefa7c552148e89ec4a68b2'

SAFE_CODES = frozenset(('cache-compiler', 'cache-count', 'cache-option-ANDROID_ABI', 'cache-option-ANDROID_PLATFORM', 'cache-option-ANDROID_STL', 'cache-option-GGML_ACCELERATE', 'cache-option-GGML_BLAS', 'cache-option-GGML_CPU_ARM_ARCH', 'cache-option-GGML_METAL', 'cache-option-GGML_NATIVE', 'cache-option-GGML_OPENMP', 'cache-toolchain', 'cmake-version', 'command-arguments', 'command-arm-baseline', 'command-compiler', 'command-graph-target', 'command-host-cpu', 'command-missing', 'command-option-incomplete', 'command-output-count', 'command-output-directory', 'command-override', 'command-shell-syntax', 'command-source', 'command-sysroot', 'command-target-api', 'database-json', 'database-missing', 'database-shape', 'database-size', 'entry-directory', 'entry-language', 'entry-schema', 'entry-source', 'graph-coverage', 'source-pin', 'tracked-model', 'unexpected'))

class ProofRejected(ValueError):
    def __init__(self, code):
        self.code = code if code in SAFE_CODES else 'unexpected'
        super().__init__(self.code)

def check_cmake_version(line):
    # Android SDK's pinned 3.22.1 package may expose its Kitware git revision.
    if not re.fullmatch(r'cmake version 3\.22\.1(?:-g[0-9a-f]{7,40})?\r?\n?', line):
        raise ProofRejected('cmake-version')


def check_cache(text, ndk):
    values = dict(re.findall(r'^([^#/:][^:=]*):[^=]+=(.*)$', text, re.M))
    for key, expected in {'ANDROID_ABI':'arm64-v8a', 'ANDROID_PLATFORM':'android-24',
                          'ANDROID_STL':'c++_static', 'GGML_CPU_ARM_ARCH':'armv8-a',
                          'GGML_NATIVE':'OFF', 'GGML_METAL':'OFF', 'GGML_OPENMP':'OFF',
                          'GGML_ACCELERATE':'OFF', 'GGML_BLAS':'OFF'}.items():
        if values.get(key) != expected:
            raise ProofRejected('cache-option-'+key)
    if Path(values.get('CMAKE_TOOLCHAIN_FILE','')).resolve() != (ndk/'build/cmake/android.toolchain.cmake').resolve():
        raise ProofRejected('cache-toolchain')
    for language in ('C','CXX'):
        key = 'CMAKE_'+language+'_COMPILER'
        # NDK r28c legacy uses normal set(), not CACHE. The actual compile
        # database below is mandatory compiler proof, even when this key is absent.
        if key not in values:
            continue
        compiler = Path(values[key]).resolve()
        if not compiler.is_relative_to((ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin').resolve()):
            raise ProofRejected('cache-compiler')

def check_commands(entries, ndk, source, build):
    """Read actual CMake compile commands, not templates or inferred host defaults."""
    if not isinstance(entries, list) or not 5 <= len(entries) <= 2000:
        raise ProofRejected('database-shape')
    tools = ndk/'toolchains/llvm/prebuilt/linux-x86_64'
    seen = set()
    languages = set()
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get('directory'), str) or not isinstance(entry.get('file'), str):
            raise ProofRejected('entry-schema')
        directory = Path(entry['directory']).resolve()
        if not directory.is_relative_to(build.resolve()):
            raise ProofRejected('entry-directory')
        file = Path(entry['file'])
        file = (directory/file).resolve()
        if not (file.is_relative_to((source/'third_party/llama.cpp').resolve()) or file == (source/'crates/madinfer-backend-llama/csrc/madinfer_llama_bridge.c').resolve()):
            raise ProofRejected('entry-source')
        language = 'C' if file.suffix == '.c' else 'CXX' if file.suffix in ('.cpp','.cc','.cxx') else None
        if language is None:
            raise ProofRejected('entry-language')
        if 'arguments' in entry:
            args = entry['arguments']
        else:
            if not isinstance(entry.get('command'), str):
                raise ProofRejected('command-missing')
            try:
                args = shlex.split(entry['command'])
            except ValueError:
                raise ProofRejected('command-shell-syntax') from None
        if not isinstance(args, list) or not args or not all(isinstance(a,str) for a in args):
            raise ProofRejected('command-arguments')
        expected = tools/'bin'/('clang' if language == 'C' else 'clang++')
        if Path(args[0]).name != expected.name or Path(args[0]).resolve() != expected.resolve():
            raise ProofRejected('command-compiler')
        if any(a.startswith(('@','-target=','-x=','-isysroot')) or a in ('-x','-arch','-m32','-m64') for a in args):
            raise ProofRejected('command-override')
        def values(long, short=None):
            found=[]
            for i,a in enumerate(args[1:],1):
                if a.startswith(long+'='): found.append(a[len(long)+1:])
                elif a == long or (short and a == short):
                    if i+1>=len(args): raise ProofRejected('command-option-incomplete')
                    found.append(args[i+1])
            return found
        if values('--target','-target') not in (['aarch64-none-linux-android24'],['aarch64-linux-android24']):
            raise ProofRejected('command-target-api')
        roots=values('--sysroot')
        if len(roots)!=1 or Path(roots[0]).resolve()!=(tools/'sysroot').resolve():
            raise ProofRejected('command-sysroot')
        if args.count('-c')!=1 or (directory/args[args.index('-c')+1]).resolve()!=file:
            raise ProofRejected('command-source')
        outputs=values('-o')
        if len(outputs)!=1:
            raise ProofRejected('command-output-count')
        output=(directory/outputs[0]).resolve()
        if not output.is_relative_to(build.resolve()):
            raise ProofRejected('command-output-directory')
        targets=re.findall(r'CMakeFiles/([^/]+)\.dir/',output.as_posix())
        if len(targets)!=1 or targets[0] not in {'madinfer_llama_bridge','llama','ggml','ggml-base','ggml-cpu'}:
            raise ProofRejected('command-graph-target')
        if targets[0]=='ggml-cpu' and values('-march')!=['armv8-a']:
            raise ProofRejected('command-arm-baseline')
        if '-march=native' in args or any(a.startswith('-mcpu=') for a in args):
            raise ProofRejected('command-host-cpu')
        seen.add(targets[0]); languages.add(language)
    if seen != {'madinfer_llama_bridge','llama','ggml','ggml-base','ggml-cpu'} or languages != {'C','CXX'}:
        raise ProofRejected('graph-coverage')


def main():
    if sys.argv[1:] == ['--cmake-version']:
        check_cmake_version(sys.stdin.read())
        return
    root = Path('third_party/llama.cpp')
    files = sorted((p for p in root.rglob('*') if p.is_file()), key=lambda p:p.relative_to(root).as_posix().encode())
    lines = b''.join(hashlib.sha256(p.read_bytes()).hexdigest().encode()+b'  ./'+p.relative_to(root).as_posix().encode()+b'\n' for p in files)
    if len(files)!=373 or hashlib.sha256(lines).hexdigest()!=ENGINE_HASH:
        raise ProofRejected('source-pin')
    if subprocess.check_output(['git','ls-files','*.gguf']):
        raise ProofRejected('tracked-model')
    if len(sys.argv)>1:
        target,ndk=map(Path,sys.argv[1:])
        caches=list((target/'aarch64-linux-android/release/build').glob('madinfer-backend-llama-*/out/llama-build/CMakeCache.txt'))
        if len(caches)!=1:
            raise ProofRejected('cache-count')
        check_cache(caches[0].read_text(),ndk)
        database=caches[0].parent/'compile_commands.json'
        if not database.is_file():
            raise ProofRejected('database-missing')
        if database.stat().st_size > 16*1024*1024:
            raise ProofRejected('database-size')
        try:
            entries=json.loads(database.read_text())
        except (ValueError, UnicodeError):
            raise ProofRejected('database-json') from None
        check_commands(entries,ndk,Path.cwd(),caches[0].parent)
    print('PASS: pinned engine source' + (' and Android cross configuration' if len(sys.argv)>1 else ''))

def cli():
    try:
        main()
        return 0
    except Exception as error:
        code = error.code if isinstance(error, ProofRejected) else 'unexpected'
        # Only enumerated labels leave the job-local log. Never error text/paths.
        message = ('FAIL: android-native proof=' + code + '\n').encode()
        try:
            os.write(3, message)
        except OSError:
            sys.stderr.write(message.decode())
        return 1

if __name__ == '__main__': sys.exit(cli())
