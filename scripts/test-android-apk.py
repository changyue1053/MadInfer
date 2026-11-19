#!/usr/bin/env python3
"""Synthetic ZIP/ELF inspector tests only, not APK build or Android execution."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile
spec=importlib.util.spec_from_file_location('apk',Path(__file__).with_name('check-android-apk.py'))
apk=importlib.util.module_from_spec(spec);spec.loader.exec_module(apk)

def elf():
    data=bytearray(120);data[:7]=b'\x7fELF\x02\x01\x01'
    struct.pack_into('<HH',data,16,3,183);struct.pack_into('<Q',data,32,64)
    struct.pack_into('<HH',data,54,56,1)
    struct.pack_into('<IIQQQQQQ',data,64,1,5,0,0,0,120,120,16384)
    return data

class APKTests(unittest.TestCase):
    def make(self,root,mode='valid'):
        library=root/'library.so';library.write_bytes(elf())
        if mode=='wrongarch':
            data=bytearray(library.read_bytes());struct.pack_into('<H',data,18,62);library.write_bytes(data)
        path=root/'preview.apk'
        with zipfile.ZipFile(path,'w') as z:
            z.writestr('AndroidManifest.xml',b'synthetic manifest; not installable')
            name=apk.LIB if mode!='wrongabi' else 'lib/x86_64/libmadinfer_preview.so'
            info=zipfile.ZipInfo(name)
            pad=(-(z.fp.tell()+30+len(name)))%16384
            if mode!='unaligned': info.extra=struct.pack('<HH',0xbeef,pad-4)+bytes(pad-4)
            if mode=='compressed': info.compress_type=zipfile.ZIP_DEFLATED
            z.writestr(info,library.read_bytes())
            if mode=='extra': z.writestr('classes.dex',b'no code allowed')
        if mode=='mismatch': library.write_bytes(b'different')
        return path,library
    def test_aligned_stored_identical_elf(self):
        with tempfile.TemporaryDirectory() as temp: apk.inspect(*self.make(Path(temp)))
    def test_wrong_abi_compression_alignment_bytes_or_entries(self):
        for mode in ('wrongabi','compressed','unaligned','mismatch','extra'):
            with self.subTest(mode=mode),tempfile.TemporaryDirectory() as temp:
                with self.assertRaises(ValueError): apk.inspect(*self.make(Path(temp),mode))
    def test_wrong_elf_arch(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaises(ValueError): apk.inspect(*self.make(Path(temp),'wrongarch'))
    def test_duplicate_rejected(self):
        import warnings
        with tempfile.TemporaryDirectory() as temp:
            path,library=self.make(Path(temp))
            with warnings.catch_warnings():
                warnings.simplefilter('ignore',UserWarning)
                with zipfile.ZipFile(path,'a') as z:z.writestr(apk.LIB,library.read_bytes())
            with self.assertRaises(ValueError):apk.inspect(path,library)

class SymbolPipeline(unittest.TestCase):
    def test_long_output_consumed_and_both_statuses_checked(self):
        import subprocess
        script=Path(__file__).resolve().parents[1]/'ci/android-app-preview.sh'
        line=next(line for line in script.read_text().splitlines() if 'llvm-nm' in line)
        suffix=line.split(' | ',1)[1]
        self.assertEqual(suffix, "grep -E ' T ANativeActivity_onCreate$' >/dev/null")
        producer="printf '00000000 T ANativeActivity_onCreate\\n'; printf '%s\\n' {1..100000}"
        self.assertEqual(subprocess.run(['bash','-o','pipefail','-c','{ '+producer+'; } | '+suffix]).returncode,0)
        self.assertNotEqual(subprocess.run(['bash','-o','pipefail','-c',"printf '00000000 T wrong_symbol\\n' | "+suffix]).returncode,0)
        self.assertNotEqual(subprocess.run(['bash','-o','pipefail','-c',"{ "+producer+"; exit 7; } | "+suffix]).returncode,0)

if __name__=='__main__':unittest.main()
