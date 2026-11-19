#!/usr/bin/env python3
"""Generate isolated app-owned source consumer; no SDK/default/CI modifications."""
import argparse
import importlib.util
from pathlib import Path
import shutil

HERE=Path(__file__).resolve().parent
ROOT=HERE.parent
spec=importlib.util.spec_from_file_location('optional_prepare',ROOT/'ios-obs-composition/prepare.py')
optional=importlib.util.module_from_spec(spec);spec.loader.exec_module(optional)
once=optional.replace_once

def prepare(dest,mode,installed=None,producer=None):
    # Reuse exact reviewed resource and installed-package logic; not an installer.
    dest=Path(dest).resolve()
    optional.prepare(dest,mode,installed,producer)
    tests=dest/'Tests'; app=dest/'App'
    # Reuse original native output/order/stat checks byte-for-byte apart from
    # namespacing and include path. They now execute and own callbacks in app.
    driver=(tests/'NativeChecks.m').read_text()
    driver=driver.replace('#import "NativeChecks.h"','#import "AppChecks.h"')
    driver=driver.replace('MadInferCheck','MIDemoInternalCheck')
    driver=driver.replace('SmokeState','MIDemoSmokeState').replace('PositiveState','MIDemoPositiveState')
    (app/'AppChecks.m').write_text(driver)
    header=(tests/'NativeChecks.h').read_text().replace('MadInferCheck','MIDemoInternalCheck')
    (app/'AppChecks.h').write_text(header)
    for name in ('Demo.h','Driver.h','Coordinator.m','AppDelegate.swift'):
        shutil.copyfile(HERE/name,app/name)
    # Test side contains only original three regressions and app-ingress calls.
    shutil.copyfile(ROOT/'ios/Smoke/Tests/NativeChecks.m',tests/'NativeChecks.m')
    (tests/'NativeChecks.h').write_text((ROOT/'ios/Smoke/Tests/NativeChecks.h').read_text()+'\n#import "../App/Demo.h"\n')
    (tests/'MadInferSmokeTests.swift').write_text((ROOT/'ios/Smoke/Tests/MadInferSmokeTests.swift').read_text()+'\n'+(HERE/'PositiveTests.swift').read_text())
    if mode=='on':
        for name in ('Adapter.h','Adapter.cpp'):
            shutil.move(str(tests/name),str(app/name))
    project=dest/'MadInferSmoke.xcodeproj/project.pbxproj'
    text=project.read_text()
    entries='''
"000000000000000000000301" = { "isa" = "PBXFileReference"; "lastKnownFileType" = "sourcecode.c.objc"; "path" = "App/AppChecks.m"; "sourceTree" = "SOURCE_ROOT"; };
"000000000000000000000302" = { "isa" = "PBXBuildFile"; "fileRef" = "000000000000000000000301"; };
"000000000000000000000303" = { "isa" = "PBXFileReference"; "lastKnownFileType" = "sourcecode.c.objc"; "path" = "App/Coordinator.m"; "sourceTree" = "SOURCE_ROOT"; };
"000000000000000000000304" = { "isa" = "PBXBuildFile"; "fileRef" = "000000000000000000000303"; };
'''
    text=once(text,'"objects" = {','"objects" = {'+entries)
    app_sources='"00000000000000000000000C", "000000000000000000000302", "000000000000000000000304"'
    config='"SWIFT_OBJC_BRIDGING_HEADER" = "$(PROJECT_DIR)/App/Demo.h";\n"ENABLE_DEBUG_DYLIB" = "NO";'
    if mode=='on':
        text=once(text,'"path" = "Tests/Adapter.cpp"','"path" = "App/Adapter.cpp"')
        text=once(text,'"files" = ("00000000000000000000000D", "000000000000000000000013", "000000000000000000000202");',
                  '"files" = ("00000000000000000000000D", "000000000000000000000013");')
        app_sources+=', "000000000000000000000202"'
        obs_settings='''
"HEADER_SEARCH_PATHS" = ("$(inherited)", "$(PROJECT_DIR)/madobs-installed/include");
"CLANG_CXX_LANGUAGE_STANDARD" = "c++17";
"OTHER_LDFLAGS" = ("$(inherited)", "$(PROJECT_DIR)/madobs-installed/lib/libmadobs_runtime.a", "-lc++", "-lz");'''
        if text.count(obs_settings)!=2: raise ValueError('optional config changed')
        text=text.replace(obs_settings,'')
        config+=obs_settings
    text=once(text,'"files" = ("00000000000000000000000C");','"files" = ('+app_sources+');')
    key='"PRODUCT_BUNDLE_IDENTIFIER" = "org.example.madinfer.binarysmoke";'
    if text.count(key)!=2: raise ValueError('app config changed')
    text=text.replace(key,key+'\n'+config)
    # Resources belong to ordinary application, not its test bundle.
    resource_list='"files" = ("000000000000000000000110", "000000000000000000000111");'
    text=once(text,resource_list,'"files" = ();')
    start=text.index('"000000000000000000000020" = {')
    stop=text.index('"000000000000000000000022" = {',start)
    block=once(text[start:stop],'"files" = ();',resource_list)
    text=text[:start]+block+text[stop:]
    project.write_text(text)

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--dest',type=Path,required=True)
    p.add_argument('--mode',choices=('on','off'),required=True)
    p.add_argument('--installed',type=Path);p.add_argument('--removed-producer',type=Path)
    a=p.parse_args();prepare(a.dest,a.mode,a.installed,a.removed_producer)
