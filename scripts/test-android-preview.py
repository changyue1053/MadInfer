#!/usr/bin/env python3
"""Synthetic inspector tests; no ELF execution or Android build evidence."""
import importlib.util
from pathlib import Path
import struct
import unittest

def module(name, file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    result=importlib.util.module_from_spec(spec); spec.loader.exec_module(result); return result
elf=module('elf','check-android-preview.py')
cross=module('cross','check-android-native-source.py')

def fixture():
    data=bytearray(120); data[:7]=b'\x7fELF\x02\x01\x01'
    struct.pack_into('<HH',data,16,3,183); struct.pack_into('<Q',data,32,64)
    struct.pack_into('<HH',data,54,56,1)
    struct.pack_into('<IIQQQQQQ',data,64,1,5,0,0,0,120,120,16384)
    return data

class Inspectors(unittest.TestCase):
    def test_sdk_cmake_version(self):
        for line in ('cmake version 3.22.1', 'cmake version 3.22.1\n',
                     'cmake version 3.22.1-g123abcd', 'cmake version 3.22.1-g123abcd\r\n'):
            cross.check_cmake_version(line)
    def test_sdk_cmake_version_rejects_drift(self):
        for line in ('cmake version 3.22.10', 'cmake version 3.22.1-arbitrary',
                     'cmake version 3.22.1-gxyz', 'cmake version 3.22.1-g123',
                     'cmake version 3.22.1-g'+'a'*41, 'cmake version 3.22.1-extra\r\n',
                     'cmake version 3.22.1\nextra', 'prefix cmake version 3.22.1',
                     'cmake version 3.22.1 ', 'cmake version 3.22.2'):
            with self.assertRaises(ValueError): cross.check_cmake_version(line)
    def test_arm64_load(self): elf.inspect_elf(fixture())
    def test_wrong_arch_kind_or_magic(self):
        for offset,fmt,value in [(0,'B',0),(4,'B',1),(16,'H',2),(18,'H',62),(54,'H',32),(56,'H',0)]:
            data=fixture(); struct.pack_into('<'+fmt,data,offset,value)
            with self.assertRaises(ValueError): elf.inspect_elf(data)
    def test_alignment_bounds_permissions(self):
        for offset,fmt,value in [(112,'Q',4096),(112,'Q',16385),(80,'Q',1),(96,'Q',121),(104,'Q',100),(68,'I',7),(64,'I',0)]:
            data=fixture(); struct.pack_into('<'+fmt,data,offset,value)
            with self.assertRaises(ValueError): elf.inspect_elf(data)
    def test_truncated(self):
        for size in (0,6,63,119):
            with self.assertRaises(ValueError): elf.inspect_elf(fixture()[:size])
    def cache(self):
        options={'ANDROID_ABI':'arm64-v8a','ANDROID_PLATFORM':'android-24','ANDROID_STL':'c++_static','GGML_CPU_ARM_ARCH':'armv8-a','GGML_NATIVE':'OFF','GGML_METAL':'OFF','GGML_OPENMP':'OFF','GGML_ACCELERATE':'OFF','GGML_BLAS':'OFF','CMAKE_TOOLCHAIN_FILE':'/ndk/build/cmake/android.toolchain.cmake','CMAKE_C_COMPILER':'/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/clang','CMAKE_CXX_COMPILER':'/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/clang++'}
        return '\n'.join(k+':STRING='+v for k,v in options.items())
    def test_cross_options(self): cross.check_cache(self.cache(),Path('/ndk'))
    def test_wrong_cross_options(self):
        for before,after in [('arm64-v8a','x86_64'),('android-24','android-21'),('armv8-a','native'),('c++_static','c++_shared'),('GGML_METAL:STRING=OFF','GGML_METAL:STRING=ON'),('/ndk/toolchains','/usr/toolchains'),('/ndk/build','/other/build')]:
            with self.assertRaises(ValueError): cross.check_cache(self.cache().replace(before,after),Path('/ndk'))


# Synthetic compile databases: parser qualification only, not compiler execution.
class CompileProof(unittest.TestCase):
    def entries(self):
        sources=[('madinfer_llama_bridge','crates/madinfer-backend-llama/csrc/madinfer_llama_bridge.c'),
                 ('llama','third_party/llama.cpp/src/llama.cpp'),
                 ('ggml','third_party/llama.cpp/ggml/src/ggml-backend-dl.cpp'),
                 ('ggml-base','third_party/llama.cpp/ggml/src/ggml.c'),
                 ('ggml-cpu','third_party/llama.cpp/ggml/src/ggml-cpu/ggml-cpu.cpp')]
        entries=[]
        for target,file in sources:
            compiler='clang' if file.endswith('.c') else 'clang++'
            args=['/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/'+compiler,
                  '--target=aarch64-none-linux-android24',
                  '--sysroot=/ndk/toolchains/llvm/prebuilt/linux-x86_64/sysroot']
            if target=='ggml-cpu': args.append('-march=armv8-a')
            args+=['-o',f'CMakeFiles/{target}.dir/file.o','-c','/source/'+file]
            entries.append({'directory':'/build','file':'/source/'+file,'arguments':args})
        return entries
    def verify(self,entries): cross.check_commands(entries,Path('/ndk'),Path('/source'),Path('/build'))
    def test_synthetic_arguments_pass(self): self.verify(self.entries())
    def test_synthetic_command_strings_pass(self):
        import shlex
        entries=self.entries()
        for e in entries: e['command']=shlex.join(e.pop('arguments'))
        self.verify(entries)
    def test_reject_missing_native_graph(self):
        with self.assertRaises(ValueError): self.verify(self.entries()[:-1])
    def test_reject_target_api_sysroot_and_compiler(self):
        for old,new in [('android24','android23'),('aarch64-none','x86_64-none'),('/ndk/','/foreign/'),('/bin/clang','/bin/gcc'),('armv8-a','native')]:
            entries=self.entries()
            for e in entries: e['arguments']=[a.replace(old,new) for a in e['arguments']]
            with self.assertRaises(ValueError): self.verify(entries)
    def test_reject_hidden_conflicting_flags(self):
        for extra in (['@hidden'],['-x','c++'],['--target=aarch64-none-linux-android23'],['--sysroot=/other'],['-mcpu=native']):
            entries=self.entries(); entries[0]['arguments']+=extra
            with self.assertRaises(ValueError): self.verify(entries)
    def test_reject_wrong_language_and_graph(self):
        for mode in ('driver','source','directory','output','object','missingtarget'):
            entries=self.entries(); e=entries[0]
            if mode=='driver': e['arguments'][0]+='++'
            elif mode=='source': e['file']='/source/not-native.c'
            elif mode=='directory': e['directory']='/outside'
            elif mode=='output': e['arguments'][e['arguments'].index('-o')+1]='/outside/CMakeFiles/llama.dir/file.o'
            elif mode=='object': e['arguments'][e['arguments'].index('-o')+1]='CMakeFiles/fake.dir/file.o'
            else: e['arguments'].pop(1)
            with self.assertRaises(ValueError): self.verify(entries)



class BoundedProofDiagnostics(unittest.TestCase):
    def test_normal_ndk_compiler_variables_need_not_be_cached(self):
        cache=Inspectors().cache()
        cache='\n'.join(line for line in cache.splitlines() if not line.startswith(('CMAKE_C_COMPILER:', 'CMAKE_CXX_COMPILER:')))
        cross.check_cache(cache,Path('/ndk'))
        # Cache is not sufficient: the separate real-command proof stays mandatory.
        proof=CompileProof(); proof.verify(proof.entries())
        with self.assertRaises(cross.ProofRejected): proof.verify([])
    def test_present_wrong_cached_compiler_still_fails(self):
        cache=Inspectors().cache().replace('/bin/clang','/../../outside/clang')
        with self.assertRaises(cross.ProofRejected) as error: cross.check_cache(cache,Path('/ndk'))
        self.assertEqual(error.exception.code,'cache-compiler')
    def test_all_fixed_reasons_emit_only_enum(self):
        from unittest.mock import patch
        for code in cross.SAFE_CODES:
            with patch.object(cross,'main',side_effect=cross.ProofRejected(code)), patch.object(cross.os,'write') as write:
                self.assertEqual(cross.cli(),1)
                write.assert_called_once_with(3,('FAIL: android-native proof='+code+'\n').encode())
    def test_unexpected_or_untrusted_error_never_leaks(self):
        from unittest.mock import patch
        for error in (ValueError('private command /secret/path'),cross.ProofRejected('private command /secret/path')):
            with patch.object(cross,'main',side_effect=error), patch.object(cross.os,'write') as write:
                self.assertEqual(cross.cli(),1)
                write.assert_called_once_with(3,b'FAIL: android-native proof=unexpected\n')
    def test_success_emits_no_failure(self):
        from unittest.mock import patch
        with patch.object(cross,'main'),patch.object(cross.os,'write') as write:
            self.assertEqual(cross.cli(),0); write.assert_not_called()

if __name__=='__main__': unittest.main()
