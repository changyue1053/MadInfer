"""Mock-only tests; never open a real KVM node."""
import errno
import os
from pathlib import Path
import stat
import textwrap
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

workflow = Path(__file__).resolve().parents[1] / '.github/workflows/android-arm64-kvm-prereq.yml'
literal = workflow.read_text(encoding='utf-8')
start = "          python3 -I -B - <<'PYTHON'\n"
end = '          PYTHON\n'
assert literal.count(start) == literal.count(end) == 1
_, _, block = literal.partition(start)
body, _, remainder = block.partition(end)
assert not remainder and all(not line or line.startswith(' ' * 10) for line in body.splitlines())
probe = ModuleType('workflow_inline_probe')
exec(compile(textwrap.dedent(body), str(workflow), 'exec'), vars(probe))

def device(kind=stat.S_IFCHR, minor=232, ino=8):
    return SimpleNamespace(st_mode=kind, st_dev=3, st_ino=ino, st_rdev=os.makedev(10, minor))

class Probe(unittest.TestCase):
    def test_missing_denied_and_wrong_device_before_open(self):
        for receipt, expected in ((FileNotFoundError(), 'missing'),
                                  (PermissionError(), 'permission-denied'),
                                  (device(stat.S_IFLNK), 'wrong-type-or-device'),
                                  (device(stat.S_IFREG), 'wrong-type-or-device'),
                                  (device(minor=231), 'wrong-type-or-device')):
            with self.subTest(expected=expected), patch.object(Path, 'lstat') as ls, patch.object(probe.os, 'open') as op:
                if isinstance(receipt, Exception): ls.side_effect = receipt
                else: ls.return_value = receipt
                self.assertEqual(probe.observe(), expected)
                op.assert_not_called()

    def test_open_flags_api_query_and_close(self):
        for answer, expected in ((12, 'api-v12-query-ok'), (11, 'api-unsupported'),
                                 (OSError(errno.ENOTTY, ''), 'api-unsupported'),
                                 (PermissionError(), 'permission-denied'), (OSError(errno.EIO, ''), 'api-query-failed')):
            with (self.subTest(expected=expected), patch.object(Path, 'lstat', return_value=device()),
                  patch.object(probe.os, 'open', return_value=6) as op,
                  patch.object(probe.os, 'fstat', return_value=device()),
                  patch.object(probe.os, 'close') as close, patch.object(probe.fcntl, 'ioctl') as call):
                if isinstance(answer, Exception): call.side_effect = answer
                else: call.return_value = answer
                self.assertEqual(probe.observe(), expected)
                op.assert_called_once_with(Path('/dev/kvm'), os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
                call.assert_called_once_with(6, 0xAE00, 0)
                close.assert_called_once_with(6)

    def test_open_error_never_queries_or_closes(self):
        for error, expected in ((PermissionError(), 'permission-denied'),
                              (FileNotFoundError(), 'missing'),
                              (OSError(errno.ELOOP, ''), 'wrong-type-or-device'),
                              (OSError(errno.EIO, ''), 'open-failed')):
            with self.subTest(expected=expected), patch.object(Path, 'lstat', return_value=device()), patch.object(
                    probe.os, 'open', side_effect=error), patch.object(probe.os, 'close') as close, patch.object(
                    probe.fcntl, 'ioctl') as call:
                self.assertEqual(probe.observe(), expected)
                call.assert_not_called()
                close.assert_not_called()

    def test_changed_inode_rejected_before_query(self):
        with (patch.object(Path, 'lstat', return_value=device()),
              patch.object(probe.os, 'open', return_value=6),
              patch.object(probe.os, 'fstat', return_value=device(ino=9)),
              patch.object(probe.os, 'close') as close, patch.object(probe.fcntl, 'ioctl') as call):
            self.assertEqual(probe.observe(), 'wrong-type-or-device')
            call.assert_not_called()
            close.assert_called_once_with(6)

    def test_wrong_host_or_root_never_probes_kvm(self):
        for machine, uid in (('x86_64', 999), ('aarch64', 0)):
            with (patch.object(probe.os, 'uname', return_value=SimpleNamespace(sysname='Linux', machine=machine)),
                  patch.object(probe.os, 'getuid', return_value=uid),
                  patch.object(probe.os, 'geteuid', return_value=uid),
                  patch.object(probe, 'observe') as observe, patch('builtins.print')):
                self.assertEqual(probe.main(), 1)
                observe.assert_not_called()

if __name__ == '__main__':
    unittest.main()
