#!/usr/bin/env python3
"""Source/resource/result-gate tests only. These do not execute iOS or inference."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import copy

def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
prep = load('prep', 'prepare-ios-positive.py')
positive = load('positive', 'check-ios-positive-results.py').gate

class PositiveTests(unittest.TestCase):
    def fixture(self):
        return ({'totalTestCount':5,'passedTests':5,'failedTests':0,'skippedTests':0},
                {'testNodes':[{'nodeType':'Test Case', 'nodeIdentifier':'MadInferSmokeTests/'+name+'()', 'result':'Passed'} for name in positive.EXPECTED]})
    def test_exact_five_pass(self):
        positive.verify(*self.fixture())
    def test_old_three_alone_cannot_pass(self):
        a,b=self.fixture(); a['totalTestCount']=a['passedTests']=3; b['testNodes']=b['testNodes'][:3]
        with self.assertRaises(positive.ResultRejected): positive.verify(a,b)
    def test_missing_swapped_duplicate_skip_fail(self):
        for mode in ('missing','wrong','duplicate','skip','failure'):
            a,b=self.fixture()
            if mode=='missing': b['testNodes'].pop()
            elif mode=='wrong': b['testNodes'][-1]['nodeIdentifier']='MadInferSmokeTests/testNativeSyntheticAProducesBBB()'
            elif mode=='duplicate': b['testNodes'][-1]=copy.deepcopy(b['testNodes'][-2])
            else: b['testNodes'][-1]['result']='Skipped' if mode=='skip' else 'Failed'
            with self.assertRaises(positive.ResultRejected): positive.verify(a,b)
    def test_generate_isolated_resources_and_original_checks(self):
        with tempfile.TemporaryDirectory() as root:
            dest=Path(root)/'consumer'
            prep.prepare(dest)
            source=prep.ROOT/'ios/Smoke'
            for original in source.rglob('*'):
                if original.is_file() and original.suffix in ('.swift','.m','.h'):
                    self.assertTrue((dest/original.relative_to(source)).read_bytes().startswith(original.read_bytes()))
            project=(dest/'MadInferSmoke.xcodeproj/project.pbxproj').read_text()
            self.assertIn('../MadInfer.xcframework',project)
            for v in 'AB':
                self.assertEqual(prep.hashlib.sha256((dest/f'synthetic-{v}.gguf').read_bytes()).hexdigest(),prep.HASHES[v])
                self.assertIn(f'synthetic-{v}.gguf',project)
            self.assertIn('"files" = ("000000000000000000000110", "000000000000000000000111");',project)
            with self.assertRaises(RuntimeError): prep.prepare(dest)
    def test_wrong_fixture_hash_rejected(self):
        original=prep.HASHES['B']
        try:
            prep.HASHES['B']='0'*64
            with tempfile.TemporaryDirectory() as root:
                dest=Path(root)/'consumer'
                with self.assertRaises(AssertionError): prep.prepare(dest)
                self.assertFalse(dest.exists())
        finally:
            prep.HASHES['B']=original

if __name__ == '__main__': unittest.main()
