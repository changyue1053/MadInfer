#!/usr/bin/env python3
"""Fail-closed Apple artifact inspection, fixed messages only."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

EXPORTS = {'_madobs_create', '_madobs_start', '_madobs_stop', '_madobs_destroy',
           '_madobs_emit_generic_event', '_madobs_get_generic_stats',
           '_madobs_check_private_batch'}

def symbols(text):
    return {line.split()[-1] for line in text.splitlines() if line.split() and
            re.fullmatch(r'_[A-Za-z0-9_]+', line.split()[-1])}

def platform(text):
    if 'LC_VERSION_MIN_' in text:
        raise ValueError('legacy platform proof')
    found = re.findall(r'^\s*platform\s+(\S+)\s*$', text, re.M)
    if not found or any(p not in ('7', 'IOSSIMULATOR') for p in found):
        raise ValueError('simulator platform proof')

def check_symbols(defined, undefined, mode, archive=False):
    d, u = symbols(defined), symbols(undefined)
    obs = lambda s: {x for x in s if x.startswith(('_madobs_', '_imobs_', '_madinfer_test_obs_'))}
    if mode == 'off':
        if obs(d) or obs(u): raise ValueError('off dependency')
    elif not EXPORTS <= d or (not archive and ('_madinfer_test_obs_after_join' not in d or obs(u))):
        raise ValueError('on linkage')

def resolve_bundle(products):
    products = Path(products)
    if products.is_symlink() or not products.is_dir():
        raise ValueError('products root')
    root = products.resolve()
    candidates = [root / 'MadInferSmokeTests.xctest/MadInferSmokeTests',
                  root / 'MadInferSmoke.app/PlugIns/MadInferSmokeTests.xctest/MadInferSmokeTests']
    found = []
    for candidate in candidates:
        # Reject dangling links, escapes or symlinked ancestors even if the
        # other layout exists. Exactly two approved layouts, never recursive.
        for node in (candidate, *candidate.parents):
            if node == root: break
            if node.is_symlink(): raise ValueError('bundle symlink')
        if candidate.exists():
            if not candidate.is_file() or not candidate.resolve().is_relative_to(root):
                raise ValueError('bundle containment')
            found.append(candidate)
    if len(found) != 1: raise ValueError('bundle missing or ambiguous')
    return found[0]

def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.DEVNULL, timeout=120)

def inspect(path, mode, archive=False, standalone=False):
    if not path.is_file() or path.is_symlink(): raise ValueError('artifact missing')
    if run('xcrun', 'lipo', '-archs', str(path)).strip() != 'arm64': raise ValueError('architecture')
    if archive:
        members=run('xcrun','ar','-t',str(path)).splitlines()
        # Darwin archive symbol indexes are metadata, never linkable objects.
        indexes=[m for m in members if m in ('__.SYMDEF','__.SYMDEF SORTED','__.SYMDEF_64','__.SYMDEF_64 SORTED')]
        if len(indexes)>1: raise ValueError('archive index set')
        members=[m for m in members if m not in indexes]
        if not members or len(members)!=len(set(members)) or any(not re.fullmatch(r'[A-Za-z0-9_.+-]+\.o',m) for m in members):
            raise ValueError('archive member set')
        with tempfile.TemporaryDirectory() as tmp:
            obj=Path(tmp)/'member.o'
            for member in members:
                data=subprocess.check_output(['xcrun','ar','-p',str(path),member],stderr=subprocess.DEVNULL,timeout=120)
                obj.write_bytes(data)
                if run('xcrun','lipo','-archs',str(obj)).strip()!='arm64': raise ValueError('object architecture')
                platform(run('xcrun','otool','-l',str(obj)))
    else:
        platform(run('xcrun', 'otool', '-l', str(path)))
    check_symbols(run('xcrun', 'nm', '-g', '-U', str(path)),
                  run('xcrun', 'nm', '-u', str(path)), mode, archive)
    if not archive:
        libs = run('xcrun', 'otool', '-L', str(path)).splitlines()[1:]
        if any(re.search(r'madobs|imobs', line, re.I) for line in libs): raise ValueError('dynamic obs dependency')
        if not standalone and not any('@rpath/MadInfer.framework/MadInfer ' in line for line in libs): raise ValueError('inference linkage')

if __name__ == '__main__':
    p=argparse.ArgumentParser(); p.add_argument('--mode', choices=('on','off'),required=True)
    p.add_argument('--resolve-bundle',action='store_true'); p.add_argument('--archive',action='store_true'); p.add_argument('--standalone-binary',action='store_true'); p.add_argument('path',type=Path)
    a=p.parse_args()
    try:
        target = resolve_bundle(a.path) if a.resolve_bundle else a.path
        inspect(target,a.mode,a.archive,a.standalone_binary)
    except Exception: raise SystemExit('FAIL: composition-artifact')
    print('PASS: composition-artifact')
