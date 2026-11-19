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



class CargoOutDirProof(unittest.TestCase):
    def fixtures(self, out='/dedicated/aarch64-linux-android/release/build/backend-hash/out'):
        pid='path+file:///source/crates/madinfer-backend-llama#0.1.0'
        metadata={'version':1,'workspace_root':'/source','target_directory':'/dedicated',
                  'workspace_members':[pid], 'packages':[{'id':pid,'name':'madinfer-backend-llama',
                  'source':None,'manifest_path':'/source/crates/madinfer-backend-llama/Cargo.toml'}]}
        messages=[{'reason':'build-script-executed','package_id':pid,'out_dir':out},
                  {'reason':'build-finished','success':True}]
        return metadata,messages
    def verify(self,metadata,messages):
        return cross.select_out_dir(metadata,messages,Path('/source'),Path('/dedicated'))
    def test_synthetic_old_and_new_layouts(self):
        # Both are examples only, not a claim of observing these paths on a runner.
        for out in ('/dedicated/aarch64-linux-android/release/build/backend-hash/out',
                    '/dedicated/aarch64-linux-android/release/build-scripts/backend-hash/out'):
            self.assertEqual(self.verify(*self.fixtures(out)),Path(out))
    def test_missing_duplicate_wrong_component(self):
        for mode in ('missing','duplicate','wrong'):
            a,b=self.fixtures()
            if mode=='missing': b.pop(0)
            elif mode=='duplicate': b.insert(0,dict(b[0]))
            else: b[0]['package_id']='path+file:///source/crates/other#0.1.0'
            with self.assertRaises(cross.ProofRejected): self.verify(a,b)
    def test_out_containment_relative_traversal(self):
        for out in ('/elsewhere/out','relative/out','/dedicated/../outside/out','/dedicated','/dedicated-other/out'):
            with self.assertRaises(cross.ProofRejected): self.verify(*self.fixtures(out))
    def test_wrong_metadata_manifest_membership_source(self):
        for mode in ('root','target','manifest','membership','registry','duplicate'):
            a,b=self.fixtures()
            if mode=='root': a['workspace_root']='/other'
            elif mode=='target': a['target_directory']='/other'
            elif mode=='manifest': a['packages'][0]['manifest_path']='/other/Cargo.toml'
            elif mode=='membership': a['workspace_members']=[]
            elif mode=='registry': a['packages'][0]['source']='registry+elsewhere'
            else: a['packages'].append(dict(a['packages'][0]))
            with self.assertRaises(cross.ProofRejected): self.verify(a,b)
    def test_failure_missing_finish_bad_schema(self):
        for mode in ('false','missing','schema','unknown','bool-schema'):
            a,b=self.fixtures()
            if mode=='false': b[-1]['success']=False
            elif mode=='missing': b.pop()
            elif mode=='schema': b[0]['out_dir']=None
            elif mode=='bool-schema': b[-1]['success']=1
            else: b.insert(0,{'reason':'unknown'})
            with self.assertRaises(cross.ProofRejected): self.verify(a,b)
    def test_symlink_escape(self):
        import tempfile
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); inside=root/'dedicated'; inside.mkdir()
            outside=root/'elsewhere'; outside.mkdir()
            (inside/'escape').symlink_to(outside,target_is_directory=True)
            a,b=self.fixtures(str(inside/'escape'/'out')); a['target_directory']=str(inside)
            with self.assertRaises(cross.ProofRejected): cross.select_out_dir(a,b,Path('/source'),inside)



class RealisticCacheSyntax(unittest.TestCase):
    def realistic(self):
        rows=Inspectors().cache().splitlines()
        rows=[row.replace(':STRING=',':UNINITIALIZED=') if row.startswith(('ANDROID_ABI:','ANDROID_PLATFORM:')) else row for row in rows]
        return '# This is the CMakeCache file.\n\n' + '\n\n'.join('// No help, variable specified on the command line.\n'+row for row in rows) + '\n'
    def test_documented_uninitialized_android_values(self):
        cross.check_cache(self.realistic(),Path('/ndk'))
    def test_crlf_blank_comments_and_equals_in_values(self):
        source=self.realistic()+'\n// Other CMake value\nEXTRA:INTERNAL=a=b;c=d\n'
        cross.check_cache(source.replace('\n','\r\n'),Path('/ndk'))
    def test_duplicate_key_same_or_conflicting_rejected(self):
        for value in ('arm64-v8a','x86_64'):
            with self.assertRaises(cross.ProofRejected) as error:
                cross.check_cache(self.realistic()+'ANDROID_ABI:UNINITIALIZED='+value+'\n',Path('/ndk'))
            self.assertEqual(error.exception.code,'cache-key-duplicate')
    def test_expected_values_not_trimmed_or_weakened(self):
        for value in ('arm64-v8a ', ' arm64-v8a', 'ARM64-V8A', 'x86_64'):
            with self.assertRaises(cross.ProofRejected) as error:
                cross.check_cache(self.realistic().replace('=arm64-v8a','='+value),Path('/ndk'))
            self.assertEqual(error.exception.code,'cache-option-ANDROID_ABI')
    def test_malformed_noncomment_line_rejected(self):
        for line in ('not a cache entry', 'ANDROID_ABI=arm64-v8a','ANDROID_ABI:STRING', ' ANDROID_ABI:STRING=arm64-v8a'):
            with self.assertRaises(cross.ProofRejected) as error:
                cross.check_cache(self.realistic()+line,Path('/ndk'))
            self.assertEqual(error.exception.code,'cache-line-schema')



class ConfiguredVendorGraph(unittest.TestCase):
    def entries(self):
        entries=CompileProof().entries()
        for filename in ('hash.cpp','xxhash/xxhash.c','sha1/sha1.c','sha256/sha256.c'):
            cxx=filename in ('hash.cpp','sha1/sha1.c')
            file='/source/third_party/llama.cpp/vendor/hash/'+filename
            args=['/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/'+('clang++' if cxx else 'clang'),
                  '--target=aarch64-none-linux-android24',
                  '--sysroot=/ndk/toolchains/llvm/prebuilt/linux-x86_64/sysroot']
            if filename=='sha1/sha1.c': args+=['-x','c++']
            args+=['-o','vendor/hash/CMakeFiles/vendor-hash.dir/'+filename+'.o','-c',file]
            entries.append({'directory':'/build','file':file,'arguments':args})
        return entries
    def verify(self, entries): CompileProof().verify(entries)
    def test_vendor_members_rejected(self):
        elf.reject_vendor_hash_members(['madinfer.o','ggml.c.o'])
        for name in ('hash.cpp.o','xxhash.c.o','sha1.c.o','sha256.c.o'):
            with self.assertRaises(ValueError): elf.reject_vendor_hash_members(['folder/'+name])
    def test_exact_configured_auxiliary_pass(self): self.verify(self.entries())
    def test_vendor_does_not_replace_required_graph(self):
        with self.assertRaises(cross.ProofRejected): self.verify(self.entries()[1:])
    def test_exact_source_target_pair_only(self):
        for mode in ('unknown-source','wrong-target','unknown-target'):
            entries=self.entries(); item=entries[-1]
            if mode=='unknown-source':
                item['file']=item['file'].replace('sha256.c','other.c')
                item['arguments'][-1]=item['file']
            else:
                new='llama' if mode=='wrong-target' else 'vendor-other'
                item['arguments']=[a.replace('vendor-hash.dir',new+'.dir') for a in item['arguments']]
            with self.assertRaises(cross.ProofRejected): self.verify(entries)
    def test_sha1_only_exact_cxx_override(self):
        for mode in ('missing','c','duplicate','elsewhere','compact'):
            entries=self.entries(); item=entries[-2]
            if mode=='missing': item['arguments'].remove('-x'); item['arguments'].remove('c++')
            elif mode=='c': item['arguments'][item['arguments'].index('-x')+1]='c'
            elif mode=='duplicate': item['arguments']+=['-x','c++']
            elif mode=='compact': entries[0]['arguments']+=['-xc++']
            else: entries[0]['arguments']+=['-x','c++']
            with self.assertRaises(cross.ProofRejected): self.verify(entries)
    def test_auxiliary_still_requires_sdk_api_sysroot(self):
        for old,new in [('android24','android23'),('/ndk/','/foreign/'),('bin/clang++','bin/clang')]:
            entries=self.entries(); item=entries[-2]
            item['arguments']=[a.replace(old,new) for a in item['arguments']]
            with self.assertRaises(cross.ProofRejected): self.verify(entries)
    def test_required_object_evidence_synthetic_headers(self):
        import tempfile
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); build=root/'build'; build.mkdir()
            entries=self.entries()
            for item in entries: item['directory']=str(build)
            with self.assertRaises(cross.ProofRejected): cross.check_commands(entries,Path('/ndk'),Path('/source'),build,True)
            for item in entries[:5]:
                obj=build/item['arguments'][item['arguments'].index('-o')+1]
                obj.parent.mkdir(parents=True,exist_ok=True)
                header=bytearray(20);header[:7]=b'\x7fELF\x02\x01\x01';struct.pack_into('<HH',header,16,1,183)
                obj.write_bytes(header)
            cross.check_commands(entries,Path('/ndk'),Path('/source'),build,True)
            obj.write_bytes(b'not an object')
            with self.assertRaises(cross.ProofRejected): cross.check_commands(entries,Path('/ndk'),Path('/source'),build,True)

if __name__=='__main__': unittest.main()
