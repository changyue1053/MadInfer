#!/usr/bin/env python3
"""App-owned linkage and exact execution; fixed diagnostics only."""
import argparse
import importlib.util
import os
from pathlib import Path
import re
import subprocess

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('original_execution',ROOT/'ios-obs-composition/execute.py')
base=importlib.util.module_from_spec(spec);spec.loader.exec_module(base)
a=base.artifacts

def ownership(app_defined,test_defined,test_undefined):
    app=a.symbols(app_defined); defined=a.symbols(test_defined); imports=a.symbols(test_undefined)
    if '_MIDemoSubmit' not in app or '_MIDemoInternalCheckPositive' not in app:
        raise ValueError('app-ingress')
    if '_MIDemoSubmit' not in imports or any(s.startswith('_MIDemo') for s in defined):
        raise ValueError('test-ingress')
    if any(s.startswith(('_madobs_','_imobs_','_madinfer_test_obs_')) for s in defined|imports):
        raise ValueError('test-observer')

def project_boundary(project,mode):
    text=project.read_text()
    def block(identifier):
        marker='"'+identifier+'" = {'
        if text.count(marker)!=1: raise ValueError('project identity')
        return text.split(marker,1)[1].split('\n\t\t};',1)[0]
    for identifier in ('000000000000000000000040','000000000000000000000041'):
        test=block(identifier)
        if 'madobs-installed' in test or 'OTHER_LDFLAGS' in test: raise ValueError('test archive input')
    sources=block('000000000000000000000022')
    if '"files" = ("00000000000000000000000D", "000000000000000000000013");' not in sources:
        raise ValueError('test source set')
    for identifier in ('00000000000000000000003E','00000000000000000000003F'):
        app=block(identifier)
        if '"ENABLE_DEBUG_DYLIB" = "NO";' not in app: raise ValueError('app executable placement')
        linked='madobs-installed/lib/libmadobs_runtime.a' in app
        if linked!=(mode=='on'): raise ValueError('app archive input')

def verify(products,mode):
    manifest,binary,host=base.select(products)
    # Legacy three independent regression tests still use public MadInfer.
    # Even ON's test bundle must satisfy OFF observation-symbol isolation.
    a.inspect(binary,'off')
    a.inspect(host,mode,standalone=True)
    framework=host.parent/'Frameworks/MadInfer.framework/MadInfer'
    a.inspect(framework,'off',standalone=True)
    ownership(a.run('xcrun','nm','-g','-U',str(host)),
              a.run('xcrun','nm','-g','-U',str(binary)),a.run('xcrun','nm','-u',str(binary)))
    return manifest,binary,host

CODES=frozenset(('simulator-identifier','execution-reference','app-linkage','closure-snapshot',
                 'xcodebuild-nonzero','xcodebuild-launch','post-test-delta','unexpected'))
class Rejected(ValueError):
    def __init__(self,code): self.code=code if code in CODES else 'unexpected';super().__init__(self.code)

def execute(uuid,mode,products=base.PRODUCTS):
    stage='simulator-identifier'
    try:
        if not re.fullmatch(r'[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}',uuid): raise ValueError('identifier')
        products=products.resolve()
        stage='execution-reference';base.select(products)
        stage='app-linkage'; selected=verify(products,mode)
        stage='closure-snapshot';before=base.snapshot(products,*selected)
        manifest,_,_=selected
        stage='xcodebuild-launch'
        try:
            subprocess.run(['xcodebuild','test-without-building','-xctestrun',str(manifest),
                            '-destination','platform=iOS Simulator,id='+uuid,
                            '-destination-timeout','120','-parallel-testing-enabled','NO',
                            '-resultBundlePath','build/ios/simulator-tests.xcresult'],check=True,timeout=1800)
        except subprocess.CalledProcessError:
            raise Rejected('xcodebuild-nonzero') from None
        stage='post-test-delta'
        if selected!=verify(products,mode) or before!=base.snapshot(products,*selected): raise ValueError('changed')
    except Rejected: raise
    except Exception: raise Rejected(stage) from None

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--mode',choices=('off','on'),required=True)
    p.add_argument('--uuid');p.add_argument('--inspect-only',action='store_true');p.add_argument('--project-only',action='store_true');args=p.parse_args()
    try:
        if args.project_only:
            if args.uuid or args.inspect_only: raise Rejected('unexpected')
            project_boundary(ROOT/'build/ios/positive-consumer/MadInferSmoke.xcodeproj/project.pbxproj',args.mode)
        elif args.inspect_only:
            if args.uuid: raise Rejected('unexpected')
            verify(base.PRODUCTS.resolve(),args.mode)
        else: execute(args.uuid,args.mode)
    except Rejected as error:
        os.write(3,('FAIL: app-demo category='+error.code+'\n').encode('ascii'));raise SystemExit(1)
    except Exception:
        os.write(3,b'FAIL: app-demo category=app-linkage\n');raise SystemExit(1)
    os.write(3,b'PASS: app-demo-artifacts\n')
