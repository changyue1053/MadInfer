#!/usr/bin/env python3
"""Unsigned APK container validation; no install, launch or runtime evidence."""
import importlib.util
from pathlib import Path
import struct
import sys
import zipfile
spec=importlib.util.spec_from_file_location('native',Path(__file__).with_name('check-android-preview.py'))
native=importlib.util.module_from_spec(spec); spec.loader.exec_module(native)
LIB='lib/arm64-v8a/libmadinfer_preview.so'

def inspect(apk, library):
    raw=Path(apk).read_bytes()
    with zipfile.ZipFile(apk) as archive:
        infos=archive.infolist(); names=[i.filename for i in infos]
        if len(names)!=len(set(names)) or set(names)-{LIB,'AndroidManifest.xml','resources.arsc'} or LIB not in names or 'AndroidManifest.xml' not in names:
            raise ValueError('Unexpected APK entries')
        if archive.testzip() is not None: raise ValueError('APK CRC mismatch')
        member=archive.getinfo(LIB)
        if member.compress_type!=zipfile.ZIP_STORED or member.flag_bits&1:
            raise ValueError('Native library must be unencrypted and stored')
        offset=member.header_offset
        if raw[offset:offset+4]!=b'PK\x03\x04' or offset+30>len(raw):
            raise ValueError('Invalid local header')
        flags,method=struct.unpack_from('<HH',raw,offset+6)
        if flags!=member.flag_bits or method!=zipfile.ZIP_STORED:
            raise ValueError('Local and central native header mismatch')
        name_size,extra_size=struct.unpack_from('<HH',raw,offset+26)
        data_offset=offset+30+name_size+extra_size
        if raw[offset+30:offset+30+name_size]!=LIB.encode() or data_offset%16384:
            raise ValueError('Native ZIP entry not 16KiB aligned')
        data=archive.read(LIB)
        if data!=Path(library).read_bytes() or raw[data_offset:data_offset+len(data)]!=data:
            raise ValueError('Packaged native bytes mismatch')
        native.inspect_elf(data)
    print('PASS: unsigned ARM64 APK exact ELF bytes and ZIP 16KiB alignment (not runtime)')

if __name__=='__main__': inspect(*sys.argv[1:])
