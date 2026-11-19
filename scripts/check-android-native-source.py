#!/usr/bin/env python3
"""Verify vendored engine and actual CMake Android cross configuration, fail closed."""
import hashlib
import os
import json
import shlex
import struct
from pathlib import Path
import re
import sys
import subprocess

ENGINE_HASH = '6b48c4a979aa11de3018a13c176c4c648521d534afefa7c552148e89ec4a68b2'

SAFE_CODES = frozenset(('cache-compiler', 'cache-count', 'cache-option-ANDROID_ABI', 'cache-option-ANDROID_PLATFORM', 'cache-option-ANDROID_STL', 'cache-option-GGML_ACCELERATE', 'cache-option-GGML_BLAS', 'cache-option-GGML_CPU_ARM_ARCH', 'cache-option-GGML_METAL', 'cache-option-GGML_NATIVE', 'cache-option-GGML_OPENMP', 'cache-toolchain', 'cmake-version', 'command-arguments', 'command-arm-baseline', 'command-compiler', 'command-graph-target', 'command-host-cpu', 'command-missing', 'command-option-incomplete', 'command-output-count', 'command-output-directory', 'command-override', 'command-shell-syntax', 'command-source', 'command-sysroot', 'command-target-api', 'database-json', 'database-missing', 'database-shape', 'database-size', 'entry-directory', 'entry-language', 'entry-schema', 'entry-source', 'graph-coverage', 'source-pin', 'tracked-model', 'unexpected'))

SAFE_CODES = SAFE_CODES | frozenset(('cargo-metadata', 'cargo-package', 'cargo-stream', 'cargo-success', 'cargo-out-count', 'cargo-out-path', 'cargo-cache-missing', 'cache-line-schema', 'cache-key-duplicate'))

SAFE_CODES = SAFE_CODES | frozenset(('command-vendor-pair', 'command-language-override', 'object-evidence', 'vendor-archive-built'))

class ProofRejected(ValueError):
    def __init__(self, code):
        self.code = code if code in SAFE_CODES else 'unexpected'
        super().__init__(self.code)

def check_cmake_version(line):
    # Android SDK's pinned 3.22.1 package may expose its Kitware git revision.
    if not re.fullmatch(r'cmake version 3\.22\.1(?:-g[0-9a-f]{7,40})?\r?\n?', line):
        raise ProofRejected('cmake-version')


def check_cache(text, ndk):
    values = {}
    for line in text.splitlines():
        # CMake documentation/comments are not entries. Do not let a regexp
        # consume a preceding newline into a key; retain exact key/value bytes.
        if not line.strip() or line.startswith(('#', '//')):
            continue
        match = re.fullmatch(r'([^\s:=][^:=\r\n]*):([A-Z][A-Z_]*)=(.*)', line)
        if match is None:
            raise ProofRejected('cache-line-schema')
        key, _, value = match.groups()
        if key in values:
            raise ProofRejected('cache-key-duplicate')
        values[key] = value
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

def check_commands(entries, ndk, source, build, require_objects=False):
    """Read actual CMake compile commands, not templates or inferred host defaults."""
    if not isinstance(entries, list) or not 5 <= len(entries) <= 2000:
        raise ProofRejected('database-shape')
    tools = ndk/'toolchains/llvm/prebuilt/linux-x86_64'
    seen = set()
    languages = set()
    vendor_root = (source/'third_party/llama.cpp/vendor/hash').resolve()
    vendor_sources = {vendor_root/name for name in ('hash.cpp','xxhash/xxhash.c','sha1/sha1.c','sha256/sha256.c')}
    sha1 = vendor_root/'sha1/sha1.c'
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
        language = 'CXX' if file == sha1 else 'C' if file.suffix == '.c' else 'CXX' if file.suffix in ('.cpp','.cc','.cxx') else None
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
        if any((a.startswith('-x') and a != '-x') or a.startswith(('@','-target=','-isysroot')) or a in ('-arch','-m32','-m64') for a in args):
            raise ProofRejected('command-override')
        def values(long, short=None):
            found=[]
            for i,a in enumerate(args[1:],1):
                if a.startswith(long+'='): found.append(a[len(long)+1:])
                elif a == long or (short and a == short):
                    if i+1>=len(args): raise ProofRejected('command-option-incomplete')
                    found.append(args[i+1])
            return found
        overrides=values('-x')
        if (file == sha1 and overrides != ['c++']) or (file != sha1 and overrides):
            raise ProofRejected('command-language-override')
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
        if len(targets)!=1 or targets[0] not in {'madinfer_llama_bridge','llama','ggml','ggml-base','ggml-cpu','vendor-hash'}:
            raise ProofRejected('command-graph-target')
        auxiliary = targets[0] == 'vendor-hash'
        if auxiliary != (file in vendor_sources) or (file.is_relative_to(vendor_root) and not auxiliary):
            raise ProofRejected('command-vendor-pair')
        if targets[0]=='ggml-cpu' and values('-march')!=['armv8-a']:
            raise ProofRejected('command-arm-baseline')
        if '-march=native' in args or any(a.startswith('-mcpu=') for a in args):
            raise ProofRejected('command-host-cpu')
        if not auxiliary:
            if require_objects:
                if not output.is_file():
                    raise ProofRejected('object-evidence')
                with output.open('rb') as obj:
                    header=obj.read(20)
                if len(header)!=20 or header[:7]!=b'\x7fELF\x02\x01\x01' or struct.unpack_from('<HH',header,16)!=(1,183):
                    raise ProofRejected('object-evidence')
            seen.add(targets[0]); languages.add(language)
    if seen != {'madinfer_llama_bridge','llama','ggml','ggml-base','ggml-cpu'} or languages != {'C','CXX'}:
        raise ProofRejected('graph-coverage')


def select_out_dir(metadata, messages, source, dedicated):
    """Resolve the exact workspace backend build event; never guess Cargo layout."""
    if not isinstance(metadata, dict) or type(metadata.get('version')) is not int or metadata.get('version') != 1 or not isinstance(metadata.get('packages'),list) or not isinstance(metadata.get('workspace_members'),list):
        raise ProofRejected('cargo-metadata')
    if not isinstance(metadata.get('workspace_root'),str) or Path(metadata['workspace_root']).resolve()!=source.resolve():
        raise ProofRejected('cargo-metadata')
    if not isinstance(metadata.get('target_directory'),str) or Path(metadata['target_directory']).resolve()!=dedicated.resolve():
        raise ProofRejected('cargo-metadata')
    if not all(isinstance(member,str) for member in metadata['workspace_members']):
        raise ProofRejected('cargo-metadata')
    manifest=(source/'crates/madinfer-backend-llama/Cargo.toml').resolve()
    selected=[]
    for package in metadata['packages']:
        if not isinstance(package,dict) or not isinstance(package.get('manifest_path'),str):
            raise ProofRejected('cargo-metadata')
        if Path(package['manifest_path']).resolve()==manifest:
            if package.get('name')!='madinfer-backend-llama' or 'source' not in package or package.get('source') is not None or not isinstance(package.get('id'),str) or package['id'] not in metadata['workspace_members']:
                raise ProofRejected('cargo-package')
            selected.append(package['id'])
    if len(selected)!=1:
        raise ProofRejected('cargo-package')
    if not isinstance(messages,list) or not messages:
        raise ProofRejected('cargo-stream')
    outputs=[]; completed=[]
    for message in messages:
        if not isinstance(message,dict) or message.get('reason') not in ('compiler-artifact','compiler-message','build-script-executed','build-finished'):
            raise ProofRejected('cargo-stream')
        if message['reason']=='build-finished':
            if type(message.get('success')) is not bool:
                raise ProofRejected('cargo-success')
            completed.append(message['success'])
        if message['reason']=='build-script-executed':
            if not isinstance(message.get('package_id'),str) or not isinstance(message.get('out_dir'),str):
                raise ProofRejected('cargo-stream')
            if message['package_id']==selected[0]:
                raw=Path(message['out_dir'])
                resolved=raw.resolve()
                if not raw.is_absolute() or resolved==dedicated.resolve() or not resolved.is_relative_to(dedicated.resolve()):
                    raise ProofRejected('cargo-out-path')
                outputs.append(resolved)
    if completed != [True] or messages[-1].get('reason')!='build-finished':
        raise ProofRejected('cargo-success')
    if len(outputs)!=1:
        raise ProofRejected('cargo-out-count')
    return outputs[0]


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
        target,ndk,metadata_path,messages_path=map(Path,sys.argv[1:])
        if metadata_path.stat().st_size>4*1024*1024 or messages_path.stat().st_size>64*1024*1024:
            raise ProofRejected('cargo-stream')
        try:
            metadata=json.loads(metadata_path.read_text())
            messages=[json.loads(line) for line in messages_path.read_text().splitlines()]
        except (ValueError,UnicodeError):
            raise ProofRejected('cargo-stream') from None
        out=select_out_dir(metadata,messages,Path.cwd(),target)
        cache=out/'llama-build/CMakeCache.txt'
        if not cache.is_file() or not cache.resolve().is_relative_to(out):
            raise ProofRejected('cargo-cache-missing')
        check_cache(cache.read_text(),ndk)
        database=cache.parent/'compile_commands.json'
        if not database.is_file() or not database.resolve().is_relative_to(out):
            raise ProofRejected('database-missing')
        if database.stat().st_size > 16*1024*1024:
            raise ProofRejected('database-size')
        try:
            entries=json.loads(database.read_text())
        except (ValueError, UnicodeError):
            raise ProofRejected('database-json') from None
        check_commands(entries,ndk,Path.cwd(),cache.parent,require_objects=True)
        if (cache.parent/'lib/libvendor-hash.a').exists():
            raise ProofRejected('vendor-archive-built')
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
