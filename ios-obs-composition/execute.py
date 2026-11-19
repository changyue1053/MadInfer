#!/usr/bin/env python3
"""Narrow generated xctestrun v1/v2 contract; execute inspected bytes, never build."""
import argparse
import hashlib
import importlib.util
from pathlib import Path
import plistlib
import re
import subprocess

HERE=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('artifacts', HERE/'inspect.py')
artifacts=importlib.util.module_from_spec(spec);spec.loader.exec_module(artifacts)
PRODUCTS=Path('build/ios/simulator-consumer/Build/Products')
HOST='Debug-iphonesimulator/MadInferSmoke.app/MadInferSmoke'
BUNDLES=('Debug-iphonesimulator/MadInferSmokeTests.xctest',
         'Debug-iphonesimulator/MadInferSmoke.app/PlugIns/MadInferSmokeTests.xctest')

def target(document):
    if not isinstance(document,dict): raise ValueError('plist root')
    version=document.get('__xctestrun_metadata__',{}).get('FormatVersion',1)
    if type(version) is not int: raise ValueError('format type')
    if version==1:
        keys=set(document)-{'__xctestrun_metadata__','CodeCoverageBuildableInfos'}
        if keys!={'MadInferSmokeTests'}: raise ValueError('v1 target set')
        t=document['MadInferSmokeTests']
    elif version==2:
        configs=document.get('TestConfigurations')
        if not isinstance(configs,list) or len(configs)!=1: raise ValueError('configuration set')
        c=configs[0]
        if c.get('IsEnabled',True) is not True: raise ValueError('disabled configuration')
        targets=c.get('TestTargets')
        if not isinstance(targets,list) or len(targets)!=1: raise ValueError('target set')
        t=targets[0]
        if t.get('BlueprintName')!='MadInferSmokeTests': raise ValueError('target identity')
    else: raise ValueError('unsupported xctestrun format')
    if not isinstance(t,dict): raise ValueError('target type')
    for flag in ('UseDestinationArtifacts','IsUITestBundle','IsAppHostedTestBundleDisabled','IsSkipped'):
        if t.get(flag,False) is not False: raise ValueError('unsupported target mode')
    for key in ('OnlyTestIdentifiers','SkipTestIdentifiers'):
        if t.get(key): raise ValueError('test selection override')
    if t.get('TestHostPath')!='__TESTROOT__/'+HOST: raise ValueError('host identity')
    bundle=t.get('TestBundlePath')
    if bundle=='__TESTHOST__/PlugIns/MadInferSmokeTests.xctest':
        bundle='__TESTROOT__/'+BUNDLES[1]
    if bundle not in tuple('__TESTROOT__/'+p for p in BUNDLES): raise ValueError('bundle identity')
    return bundle.removeprefix('__TESTROOT__/')

def select(products):
    products=Path(products)
    if products.is_symlink(): raise ValueError('products symlink')
    manifests=list(products.glob('*.xctestrun'))
    if len(manifests)!=1 or manifests[0].is_symlink(): raise ValueError('manifest set')
    manifest=manifests[0]
    relative=target(plistlib.loads(manifest.read_bytes()))
    binary=artifacts.resolve_bundle(products/'Debug-iphonesimulator')
    expected=(products/relative/'MadInferSmokeTests').resolve()
    if binary!=expected: raise ValueError('executed bundle mismatch')
    host=products/HOST
    if not host.is_file() or host.is_symlink(): raise ValueError('host missing')
    return manifest,binary,host

def snapshot(products,manifest,binary,host):
    # Include all resources and embedded frameworks, not just the main executable.
    files={manifest}
    for directory in (binary.parent,host.parent):
        for p in directory.rglob('*'):
            if p.is_symlink(): raise ValueError('artifact symlink')
            if p.is_file(): files.add(p)
    return {p.relative_to(products).as_posix():hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(files)}

def execute(uuid,mode,products=PRODUCTS):
    if not re.fullmatch(r'[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}',uuid):
        raise ValueError('simulator identifier')
    products=products.resolve()
    manifest,binary,host=select(products)
    artifacts.inspect(binary,mode)
    before=snapshot(products,manifest,binary,host)
    subprocess.run(['xcodebuild','test-without-building','-xctestrun',str(manifest),
                    '-destination','platform=iOS Simulator,id='+uuid,
                    '-destination-timeout','120','-parallel-testing-enabled','NO',
                    '-resultBundlePath','build/ios/simulator-tests.xcresult'],check=True)
    after_manifest,after_binary,after_host=select(products)
    if (manifest,binary,host)!=(after_manifest,after_binary,after_host) or before!=snapshot(products,manifest,binary,host):
        raise ValueError('artifact changed')
    artifacts.inspect(binary,mode)

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('uuid');p.add_argument('mode',choices=('on','off'))
    a=p.parse_args()
    try: execute(a.uuid,a.mode)
    except Exception: raise SystemExit('FAIL: composition-execution-reference')
