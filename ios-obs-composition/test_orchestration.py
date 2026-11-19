#!/usr/bin/env python3
"""Synthetic parser and orchestration contracts, never an Apple artifact proof."""
import importlib.util
from pathlib import Path
import unittest
import tempfile
import subprocess
H=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('artifact',H/'inspect.py')
a=importlib.util.module_from_spec(spec);spec.loader.exec_module(a)
class Tests(unittest.TestCase):
    def test_simulator_platform(self):
        a.platform(' cmd LC_BUILD_VERSION\n platform 7\n cmd LC_BUILD_VERSION\n platform IOSSIMULATOR\n')
        for text in ('', ' platform 2\n', ' platform 7\n platform 2\n', 'LC_VERSION_MIN_IPHONEOS\n platform 7\n'):
            with self.assertRaises(ValueError): a.platform(text)
    def test_symbols_on_off(self):
        defs='\n'.join('0000 T '+s for s in a.EXPORTS|{'_madinfer_test_obs_after_join'})
        a.check_symbols(defs,' U _malloc','on')
        a.check_symbols('0000 T _main',' U _malloc','off')
        for d,u,m in ((defs,'','off'),('', ' U _madobs_create','off'),('', '', 'on'),(defs,' U _madobs_create','on')):
            with self.assertRaises(ValueError): a.check_symbols(d,u,m)
    def test_exact_bundle_layouts(self):
        layouts=('MadInferSmokeTests.xctest/MadInferSmokeTests',
                 'MadInferSmoke.app/PlugIns/MadInferSmokeTests.xctest/MadInferSmokeTests')
        for layout in layouts:
            with tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp); f=root/layout; f.parent.mkdir(parents=True); f.touch()
                self.assertEqual(a.resolve_bundle(root),f)
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            with self.assertRaises(ValueError): a.resolve_bundle(root)
            for layout in layouts:
                f=root/layout; f.parent.mkdir(parents=True); f.touch()
            with self.assertRaises(ValueError): a.resolve_bundle(root)
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); (root/'MadInferSmokeTests.xctest').symlink_to(root/'missing')
            with self.assertRaises(ValueError): a.resolve_bundle(root)

    def test_git_header_status_gate(self):
        script=(H/'install.sh').read_text()
        snippet=script[script.index('header_status=0'):script.index('[[ $(uname')]
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            subprocess.run(['git','init','-q',str(root)],check=True)
            def check():
                return subprocess.run(['bash','-euo','pipefail','-c','producer="$1"; '+snippet,'test',str(root)],capture_output=True)
            self.assertEqual(check().returncode,0)
            subprocess.run(['git','-C',str(root),'config','http.https://github.com/.extraheader','synthetic-secret-marker'],check=True)
            result=check(); self.assertNotEqual(result.returncode,0)
            self.assertNotIn(b'synthetic-secret-marker',result.stdout+result.stderr)
            (root/'.git/config').write_text('invalid config [')
            result=check(); self.assertNotEqual(result.returncode,0)
            self.assertEqual(result.stdout+result.stderr,b'')

    def test_workflow_boundaries(self):
        source=(H.parent/'.github/workflows/ios-obs-composition.yml').read_text()
        off,on=source.split('  optional-on:\n',1)
        self.assertNotIn('repository: changyue1053/MadObs',off)
        self.assertIn('ref: 4af77ffa464a48307e5ff2d95f10b835919d205b',on)
        self.assertEqual(source.count('runs-on: macos-15'),2)
        self.assertEqual(source.count('persist-credentials: false'),3)
        self.assertEqual(source.count('bash ci/ios-positive-preflight.sh'),2)
        for forbidden in ('upload-artifact','workflow_dispatch','pull_request_target','secrets.', 'sudo ', 'xcodebuild -license','ci/native.sh'):
            self.assertNotIn(forbidden,source)
    def test_installed_and_removed_before_consumption(self):
        script=(H/'install.sh').read_text()
        self.assertLess(script.index('cmake --install'),script.index('rm -rf --'))
        self.assertLess(script.index('rm -rf --'),script.index('python3 ios-obs-composition/prepare.py'))
        self.assertIn('3a03801beb18c9601630cc4421b95c2743018dfb',script)
        self.assertIn('-DMADOBS_PACKAGE_BORINGSSL=OFF',script)
        run=(H/'run.sh').read_text()
        self.assertLess(run.index('stage=composition-symbols'),run.index('bash ios-obs-composition/simulator.sh'))
        self.assertNotIn('curl ',run)
        self.assertNotIn('git clone',run)
if __name__=='__main__': unittest.main()
