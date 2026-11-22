#!/usr/bin/env python3
"""Synthetic orchestration/linkage tests, never a native app qualification."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
H=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('qualification',H/'qualify.py')
q=importlib.util.module_from_spec(spec);spec.loader.exec_module(q)
UUID='00000000-0000-0000-0000-000000000001'
class Tests(unittest.TestCase):
    def test_ownership(self):
        app='0 T _MIDemoSubmit\n0 T _MIDemoInternalCheckPositive'
        test='0 T _MadInferCheckABI';imports='U _MIDemoSubmit\nU _madinfer_core_destroy'
        q.ownership(app,test,imports)
        for a,b,c in ((app,test,''),('',test,imports),(app,test+'\n0 T _MIDemoSubmit',imports),
                      (app,test+'\n0 T _madobs_create',imports),(app,test,imports+'\nU _madobs_start')):
            with self.assertRaises(ValueError):q.ownership(a,b,c)
    def test_exact_execution_reference_and_delta(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp).resolve(); selected=(root/'exact.xctestrun',root/'test',root/'host')
            with patch.object(q.base,'select',return_value=selected), patch.object(q,'verify',return_value=selected), \
                 patch.object(q.base,'snapshot',return_value={'synthetic':'hash'}), patch.object(q.subprocess,'run') as run:
                q.execute(UUID,'on',root)
                command=run.call_args.args[0]
                self.assertEqual(command[:4],['xcodebuild','test-without-building','-xctestrun',str(selected[0])])
                self.assertEqual(run.call_args.kwargs['timeout'],1800)
            with patch.object(q.base,'select',return_value=selected), patch.object(q,'verify',return_value=selected), \
                 patch.object(q.base,'snapshot',side_effect=[{'x':'one'},{'x':'two'}]), patch.object(q.subprocess,'run'):
                with self.assertRaises(q.Rejected) as error:q.execute(UUID,'off',root)
                self.assertEqual(error.exception.code,'post-test-delta')
    def test_nonzero_never_success(self):
        root=Path('/synthetic')
        selected=(root/'exact.xctestrun',root/'test',root/'host')
        with patch.object(q.base,'select',return_value=selected), patch.object(q,'verify',return_value=selected), \
             patch.object(q.base,'snapshot',return_value={}), patch.object(q.subprocess,'run',side_effect=subprocess.CalledProcessError(65,['not-printed'])):
            with self.assertRaises(q.Rejected) as error:q.execute(UUID,'on',root)
            self.assertEqual(error.exception.code,'xcodebuild-nonzero')
    def test_proven_install_and_launch_glue(self):
        original=H.parent/'ios-obs-composition'
        self.assertEqual((H/'install.sh').read_text(),(original/'install.sh').read_text().replace(
            'python3 ios-obs-composition/prepare.py','python3 ios-app-obs-demo/prepare.py'))
        self.assertEqual((H/'simulator.sh').read_text(),(original/'simulator.sh').read_text().replace(
            'python3 ios-obs-composition/execute.py "$uuid" "$mode"',
            'python3 ios-app-obs-demo/qualify.py --uuid "$uuid" --mode "$mode"'))
if __name__=='__main__':unittest.main()
