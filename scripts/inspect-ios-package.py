#!/usr/bin/env python3
"""Packaging inspection only. Does not execute Swift, Rust or iOS."""
import plistlib
import re
from pathlib import Path
import struct
import subprocess
import sys


def inspect_macho(data, expected_platform):
    if len(data) < 32:
        raise ValueError('truncated Mach-O header')
    magic, cpu, subtype, filetype, count, size, _, _ = struct.unpack_from('<8I', data)
    if (magic, cpu, subtype, filetype) != (0xFEEDFACF, 0x0100000C, 0, 6):
        raise ValueError('requires thin arm64 MH_DYLIB')
    if 32 + size > len(data):
        raise ValueError('truncated Mach-O commands')
    offset, platforms, install_names = 32, [], []
    for _ in range(count):
        if offset + 8 > 32 + size:
            raise ValueError('truncated command')
        cmd, length = struct.unpack_from('<2I', data, offset)
        if length < 8 or offset + length > 32 + size:
            raise ValueError('invalid command length')
        if cmd == 0x32:  # LC_BUILD_VERSION
            if length < 24:
                raise ValueError('truncated build version')
            platform, minimum = struct.unpack_from('<2I', data, offset + 8)
            if minimum != 16 << 16:
                raise ValueError('unexpected minimum iOS deployment version')
            platforms.append(platform)
        if cmd in (0xD, 0xC, 0x80000018, 0x8000001F):
            if length < 24:
                raise ValueError('truncated dylib command')
            start = struct.unpack_from('<I', data, offset + 8)[0]
            if not 24 <= start < length:
                raise ValueError('invalid dylib name offset')
            raw = data[offset + start:offset + length]
            if b'\0' not in raw:
                raise ValueError('unterminated dylib name')
            name = raw.split(b'\0')[0].decode('utf-8')
            if cmd == 0xD:
                install_names.append(name)
            elif not (name.startswith(('/usr/lib/', '/System/Library/')) or
                      re.fullmatch(r'@rpath/libswift[A-Za-z0-9_]+\.dylib', name)):
                raise ValueError(f'non-system dynamic dependency: {name}')
        offset += length
    if offset != 32 + size or platforms != [expected_platform]:
        raise ValueError('wrong or missing Mach-O platform')
    if install_names != ['@rpath/MadInfer.framework/MadInfer']:
        raise ValueError('wrong framework install name')


def inspect(root):
    info = plistlib.loads((root / 'Info.plist').read_bytes())
    libraries = info['AvailableLibraries']
    if len(libraries) != 2:
        raise ValueError('requires exactly two slices')
    expected_symbols = set(Path('ios/Framework/exports.txt').read_text().splitlines())
    original_header = Path('crates/madinfer-ffi/include/madinfer.h').read_bytes()
    seen = set()
    for entry in libraries:
        variant = entry.get('SupportedPlatformVariant', '')
        if entry['SupportedPlatform'] != 'ios' or variant not in ('', 'simulator') or variant in seen:
            raise ValueError('wrong/duplicate platform')
        seen.add(variant)
        if entry['SupportedArchitectures'] != ['arm64']:
            raise ValueError('not exactly arm64')
        framework = root / entry['LibraryIdentifier'] / entry['LibraryPath']
        if framework.name != 'MadInfer.framework': raise ValueError('wrong product')
        inspect_macho((framework / 'MadInfer').read_bytes(), 7 if variant else 2)
        metadata = plistlib.loads((framework / 'Info.plist').read_bytes())
        if metadata.get('MinimumOSVersion') != '16.0' or metadata.get('CFBundlePackageType') != 'FMWK':
            raise ValueError('wrong framework metadata')
        if (framework / 'Headers/madinfer.h').read_bytes() != original_header:
            raise ValueError('public header differs from audited ABI')
        if not (framework / 'Modules/module.modulemap').is_file(): raise ValueError('missing C module')
        actual = set(subprocess.check_output(['xcrun', 'nm', '-gjU', str(framework / 'MadInfer')], text=True).splitlines())
        if actual != expected_symbols: raise ValueError(f'export mismatch: {actual ^ expected_symbols}')
        for name in ['LICENSE', 'THIRD_PARTY.md', 'llama-LICENSE', 'PINS.toml']:
            if not (framework / 'Notices' / name).is_file(): raise ValueError('missing notice: ' + name)
        if list(framework.rglob('*.gguf')): raise ValueError('model weights must not be packaged')
    print('PASS binary/package inspection only; no successful inference claim')

if __name__ == '__main__': inspect(Path(sys.argv[1]))
