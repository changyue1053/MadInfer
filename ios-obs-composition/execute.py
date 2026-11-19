#!/usr/bin/env python3
"""Narrow generated xctestrun v1/v2 contract; execute inspected bytes, never build."""
import argparse
import hashlib
import importlib.util
from pathlib import Path
import plistlib
import re
import subprocess
import os

HERE=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('artifacts', HERE/'artifact_inspector.py')
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
    # Apple app-hosted example uses the .app directory; both exact own host
    # forms map to the same independently verified executable below.
    allowed_hosts=('__TESTROOT__/'+HOST, '__TESTROOT__/'+HOST.rsplit('/',1)[0])
    if t.get('TestHostPath') not in allowed_hosts: raise ValueError('host identity')
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

FAILURE_CODES = {
    'products symlink': 'manifest-set', 'manifest set': 'manifest-set',
    'host identity': 'host-identity', 'host missing': 'host-identity',
    'bundle identity': 'bundle-identity', 'executed bundle mismatch': 'bundle-identity',
    'products root': 'bundle-identity', 'bundle symlink': 'bundle-identity',
    'bundle containment': 'bundle-identity', 'bundle missing or ambiguous': 'bundle-identity',
    'legacy platform proof': 'platform', 'simulator platform proof': 'platform',
    'architecture': 'platform', 'artifact missing': 'artifact-inspection',
    'off dependency': 'symbol-linkage', 'on linkage': 'symbol-linkage',
    'dynamic obs dependency': 'symbol-linkage', 'inference linkage': 'symbol-linkage',
}
STAGES = frozenset(('simulator-identifier', 'manifest-format', 'artifact-inspection',
                    'closure-snapshot', 'xcodebuild-nonzero', 'post-test-delta'))
CODES = STAGES | frozenset(FAILURE_CODES.values()) | {'unexpected','xcodebuild-launch'}
class ExecutionRejected(ValueError):
    def __init__(self, code):
        self.code=code if code in CODES else 'unexpected'
        super().__init__(self.code)

def classify(stage, error):
    if stage not in STAGES: return 'unexpected'
    # Never relay exception text. Compare only exact known internal literals.
    if stage=='xcodebuild-nonzero':
        return 'xcodebuild-nonzero' if isinstance(error,subprocess.CalledProcessError) else 'xcodebuild-launch'
    if stage in ('post-test-delta','closure-snapshot'):
        return stage
    if type(error) is ValueError and len(error.args)==1 and isinstance(error.args[0],str):
        return FAILURE_CODES.get(error.args[0],stage)
    return stage

def execute(uuid,mode,products=PRODUCTS):
    stage='simulator-identifier'
    try:
        if not re.fullmatch(r'[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}',uuid):
            raise ValueError('simulator identifier')
        products=products.resolve()
        stage="manifest-format"
        manifest,binary,host=select(products)
        stage="artifact-inspection"
        artifacts.inspect(binary,mode)
        stage="closure-snapshot"
        before=snapshot(products,manifest,binary,host)
        stage="xcodebuild-nonzero"
        subprocess.run(['xcodebuild','test-without-building','-xctestrun',str(manifest),
                        '-destination','platform=iOS Simulator,id='+uuid,
                        '-destination-timeout','120','-parallel-testing-enabled','NO',
                        '-resultBundlePath','build/ios/simulator-tests.xcresult'],check=True)
        stage="post-test-delta"
        after_manifest,after_binary,after_host=select(products)
        if (manifest,binary,host)!=(after_manifest,after_binary,after_host) or before!=snapshot(products,manifest,binary,host):
            raise ValueError('artifact changed')
        artifacts.inspect(binary,mode)

    except Exception as error:
        raise ExecutionRejected(classify(stage,error)) from None

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('uuid');p.add_argument('mode',choices=('on','off'))
    a=p.parse_args()
    try: execute(a.uuid,a.mode)
    except ExecutionRejected as error:
        os.write(3, ('FAIL: composition-execution category='+error.code+'\n').encode('ascii'))
        raise SystemExit(1)
    except Exception:
        os.write(3,b'FAIL: composition-execution category=unexpected\n')
        raise SystemExit(1)
