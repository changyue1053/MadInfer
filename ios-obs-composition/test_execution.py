#!/usr/bin/env python3
"""Synthetic v1/v2 plist contracts, not captured Xcode output or native tests."""
import copy
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
H=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('execution',H/'execute.py')
e=importlib.util.module_from_spec(spec);spec.loader.exec_module(e)

def fixture(version=1,bundle=0):
    t={'BlueprintName':'MadInferSmokeTests','TestHostPath':'__TESTROOT__/'+e.HOST,
       'TestBundlePath':'__TESTROOT__/'+e.BUNDLES[bundle]}
    if version==1:return {'MadInferSmokeTests':t}
    return {'__xctestrun_metadata__':{'FormatVersion':2},
            'TestConfigurations':[{'IsEnabled':True,'TestTargets':[t]}]}

class Tests(unittest.TestCase):
    def test_versions_and_layouts(self):
        for version in (1,2):
            for bundle in (0,1):self.assertEqual(e.target(fixture(version,bundle)),e.BUNDLES[bundle])
        d=fixture();d['MadInferSmokeTests']['TestBundlePath']='__TESTHOST__/PlugIns/MadInferSmokeTests.xctest'
        self.assertEqual(e.target(d),e.BUNDLES[1])
    def test_invalid_schema_modes_and_paths(self):
        cases=[]
        for value in (3,True,'2'):
            d=fixture();d['__xctestrun_metadata__']={'FormatVersion':value};cases.append(d)
        d=fixture(2);d['TestConfigurations']*=2;cases.append(d)
        d=fixture(2);d['TestConfigurations'][0]['TestTargets']*=2;cases.append(d)
        d=fixture(2);d['TestConfigurations'][0]['IsEnabled']=False;cases.append(d)
        for key,value in (('TestBundlePath','__TESTROOT__/../outside.xctest'),('TestBundlePath','/tmp/foreign.xctest'),
                          ('TestHostPath','/tmp/foreign'),('UseDestinationArtifacts',True),('IsUITestBundle',True),
                          ('SkipTestIdentifiers',['test']),('OnlyTestIdentifiers',['test'])):
            d=fixture();d['MadInferSmokeTests'][key]=value;cases.append(d)
        for d in cases:
            with self.assertRaises(ValueError):e.target(d)
    def materialize(self,root,bundle=0):
        manifest=root/'MadInferSmoke_iphonesimulator26.2-arm64.xctestrun'
        manifest.write_bytes(plistlib.dumps(fixture(1,bundle)))
        binary=root/e.BUNDLES[bundle]/'MadInferSmokeTests'
        binary.parent.mkdir(parents=True);binary.write_bytes(b'synthetic-not-mach-o')
        host=root/e.HOST;host.parent.mkdir(parents=True,exist_ok=True);host.write_bytes(b'synthetic-host')
        return manifest,binary,host
    def test_selection_and_hash_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);expected=self.materialize(root)
            self.assertEqual(e.select(root),expected)
            before=e.snapshot(root,*expected)
            expected[1].write_bytes(b'changed')
            self.assertNotEqual(before,e.snapshot(root,*expected))
            (root/'second.xctestrun').write_bytes(expected[0].read_bytes())
            with self.assertRaises(ValueError):e.select(root)
    def test_bundle_pointer_mismatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);manifest,_,_=self.materialize(root)
            manifest.write_bytes(plistlib.dumps(fixture(1,1)))
            with self.assertRaises(ValueError):e.select(root)
    def test_original_simulator_contract_unchanged(self):
        original=(H.parent/'scripts/test-ios-positive-simulator.sh').read_text()
        isolated=(H/'simulator.sh').read_text()
        for token in ('com.apple.CoreSimulator.SimRuntime.iOS-26-2','com.apple.CoreSimulator.SimDeviceType.iPhone-16',
                      'xcrun simctl shutdown "$uuid"','xcrun simctl delete "$uuid"',
                      '[[ $(xcrun simctl spawn "$uuid" uname -m) == arm64 ]]',
                      'python3 scripts/check-ios-positive-results.py'):
            self.assertIn(token,original);self.assertIn(token,isolated)
        self.assertNotIn('xcodebuild -project',isolated)
        self.assertIn("'test-without-building','-xctestrun'",(H/'execute.py').read_text())
if __name__=='__main__':unittest.main()
