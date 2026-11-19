#!/usr/bin/env python3
"""Strict ELF64 ARM64 16KiB/load and NDK-only dependency package inspection."""
import struct
import subprocess
import sys
from pathlib import Path

REQUIRED = {'madinfer_abi_version', 'madinfer_core_create_with_backend',
            'madinfer_backend_kind', 'madinfer_core_destroy'}
ENGINE = {'madinfer_llama_build_number', 'llama_decode', 'llama_model_load_from_file',
          'ggml_backend_cpu_init'}

def inspect_elf(data):
    if len(data) < 64 or data[:7] != b'\x7fELF\x02\x01\x01':
        raise ValueError('Expected little-endian ELF64')
    kind, machine = struct.unpack_from('<HH', data, 16)
    if kind != 3 or machine != 183:
        raise ValueError('Expected AArch64 ET_DYN')
    phoff = struct.unpack_from('<Q', data, 32)[0]
    entry, count = struct.unpack_from('<HH', data, 54)
    if entry != 56 or not 1 <= count <= 128 or phoff + entry * count > len(data):
        raise ValueError('Invalid program headers')
    loads = 0
    for i in range(count):
        ptype, flags, offset, vaddr, _, filesz, memsz, align = struct.unpack_from('<IIQQQQQQ', data, phoff + i * entry)
        if ptype == 1:
            loads += 1
            if align < 16384 or align & (align - 1) or (vaddr-offset) % align or filesz > memsz or offset+filesz > len(data):
                raise ValueError('Invalid or sub-16KiB PT_LOAD')
            if flags & 3 == 3:
                raise ValueError('Writable executable segment')
    if not loads:
        raise ValueError('No load segments')

def defined(nm, path, dynamic=False):
    args = [str(nm), '--defined-only', '--extern-only']
    if dynamic:
        args.append('--dynamic')
    output = subprocess.check_output(args+[str(path)], stderr=subprocess.DEVNULL, text=True)
    return {line.split()[-1] for line in output.splitlines() if len(line.split()) >= 2}

def main():
    package, consumer, tools = map(Path, sys.argv[1:])
    archive = package/'lib/arm64-v8a/libmadinfer_ffi.a'
    if archive.read_bytes()[:8] != b'!<arch>\n':
        raise ValueError('Expected non-thin archive')
    if not (REQUIRED | ENGINE) <= defined(tools/'llvm-nm', archive):
        raise ValueError('Native archive ABI/engine symbols missing')
    inspect_elf(consumer.read_bytes())
    if not (REQUIRED | {'madinfer_preview_link_probe'}) <= defined(tools/'llvm-nm', consumer, True):
        raise ValueError('Independent linked consumer ABI missing')
    dynamic = subprocess.check_output([str(tools/'llvm-readelf'), '--dynamic', str(consumer)], text=True)
    import re
    needed = re.findall(r'\(NEEDED\).*?\[([^]]+)\]', dynamic)
    if not needed or not set(needed) <= {'libc.so','libm.so','libdl.so','liblog.so'}:
        raise ValueError('Unexpected shared dependency')
    if '(RPATH)' in dynamic or '(RUNPATH)' in dynamic or '(TEXTREL)' in dynamic:
        raise ValueError('Non-relocatable or text relocation dependency')
    print('PASS: actual native archive symbols and relocated ARM64 16KiB C link (not runtime)')

if __name__ == '__main__':
    main()
