#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import stat
import subprocess
import unittest
from unittest.mock import patch,Mock
spec=importlib.util.spec_from_file_location('probe',Path(__file__).with_name('probe-android-runner.py'))
probe=importlib.util.module_from_spec(spec);spec.loader.exec_module(probe)

class ProbeTests(unittest.TestCase):
    def test_missing_device_never_opened(self):
        path=Mock();path.stat.side_effect=FileNotFoundError()
        with patch.object(probe.os,'open') as opened:
            self.assertEqual(probe.kvm_status(path),'missing');opened.assert_not_called()
    def test_regular_file_is_not_kvm(self):
        path=Mock();path.stat.return_value.st_mode=stat.S_IFREG
        self.assertEqual(probe.kvm_status(path),'not-character-device')
    def test_actual_open_mode_and_close(self):
        path=Mock();path.stat.return_value.st_mode=stat.S_IFCHR
        with patch.object(probe.os,'open',return_value=19) as opened,patch.object(probe.os,'close') as closed:
            self.assertEqual(probe.kvm_status(path),'open-readwrite-ok')
            opened.assert_called_once_with(path,probe.os.O_RDWR|probe.os.O_CLOEXEC|probe.os.O_NONBLOCK)
            closed.assert_called_once_with(19)
    def test_denied_open_no_permission_change(self):
        path=Mock();path.stat.return_value.st_mode=stat.S_IFCHR
        with patch.object(probe.os,'open',side_effect=PermissionError()):
            self.assertEqual(probe.kvm_status(path),'open-denied')
    def test_missing_sdk_no_tool_called(self):
        with patch.object(probe,'kvm_status',return_value='missing'),patch.object(probe.subprocess,'run') as run:
            result=probe.collect(None);run.assert_not_called()
            self.assertEqual(result['emulator'],'missing');self.assertEqual(result['acceleration'],'not-probed')
    def test_root_never_runs_emulator(self):
        with patch.object(probe.os,'geteuid',return_value=0),patch.object(probe,'executable',return_value=True),patch.object(probe,'kvm_status',return_value='open-readwrite-ok'),patch.object(probe.subprocess,'run') as run:
            result=probe.collect(Path('/no-sdk'));run.assert_not_called();self.assertEqual(result['uid'],'root-rejected')

    def test_accel_existing_only_bounded_and_silent(self):
        with patch.object(probe.os,'geteuid',return_value=1001),patch.object(probe.platform,'system',return_value='Linux'),patch.object(probe.platform,'machine',return_value='x86_64'),patch.object(probe,'executable',return_value=True),patch.object(probe,'kvm_status',return_value='open-readwrite-ok'),patch.object(probe.subprocess,'run',return_value=Mock(returncode=0)) as run:
            result=probe.collect(Path('/no-sdk'))
            run.assert_called_once_with(['/no-sdk/emulator/emulator','-accel-check'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=20)
            self.assertEqual(result['acceleration'],'tool-success-not-vm-boot')
    def test_accel_timeout_no_raw_details(self):
        with patch.object(probe.os,'geteuid',return_value=1001),patch.object(probe.platform,'system',return_value='Linux'),patch.object(probe.platform,'machine',return_value='x86_64'),patch.object(probe,'executable',return_value=True),patch.object(probe,'kvm_status',return_value='open-readwrite-ok'),patch.object(probe.subprocess,'run',side_effect=subprocess.TimeoutExpired('private',20)):
            self.assertEqual(probe.collect(Path('/no-sdk'))['acceleration'],'timeout')

if __name__=='__main__':unittest.main()
