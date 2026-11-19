#!/usr/bin/env python3
"""Synthetic archive and redirect tests only. No network or tool execution."""
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('provision',Path(__file__).with_name('provision_cmake.py'))
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
def item(name,kind=tarfile.DIRTYPE,link=''):
    m=tarfile.TarInfo(name);m.type=kind;m.mode=0o755;m.linkname=link
    if kind==tarfile.REGTYPE:m.size=1
    return m

def fixture():
    r=p.ROOT; c=r+'/CMake.app/Contents'
    return [item(r),item(r+'/CMake.app'),item(c),item(c+'/bin'),item(c+'/MacOS'),
            item(c+'/MacOS/CMake',tarfile.REGTYPE),
            item(c+'/bin/cmake-gui',tarfile.SYMTYPE,'../MacOS/CMake')]
class Tests(unittest.TestCase):
    def test_safe_internal_link_extraction(self):
        members=fixture();p.manifest(members,False)
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);archive=root/'fixture.tar.gz';dest=root/'out'
            with tarfile.open(archive,'w:gz') as tar:
                for m in members:tar.addfile(m,io.BytesIO(b'x') if m.isreg() else None)
            p.extract(archive,dest,False)
            link=dest/p.ROOT/'CMake.app/Contents/bin/cmake-gui'
            self.assertEqual(link.read_bytes(),b'x')
            self.assertTrue(link.resolve().is_relative_to(dest.resolve()))
            with self.assertRaises(ValueError):p.extract(archive,dest,False)
    def test_only_exact_implicit_root(self):
        members=fixture()[1:]
        self.assertNotIn(p.ROOT,p.manifest(members,False))
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);archive=root/'fixture.tar.gz';dest=root/'out'
            with tarfile.open(archive,'w:gz') as tar:
                for m in members:tar.addfile(m,io.BytesIO(b'x') if m.isreg() else None)
            p.extract(archive,dest,False)
            self.assertEqual((dest/p.ROOT).stat().st_mode & 0o777,0o700)
            self.assertTrue((dest/p.ROOT/'CMake.app/Contents/bin/cmake-gui').is_file())
        # Every nested directory remains explicit, even when root is absent.
        bad=[m for m in members if m.name!=p.ROOT+'/CMake.app']
        with self.assertRaises(ValueError):p.manifest(bad,False)
        with self.assertRaises(ValueError):p.manifest([item(p.ROOT,tarfile.REGTYPE)]+members,False)
        # The synthetic inventory cannot qualify as the real pinned package.
        with self.assertRaises(ValueError):p.manifest(members,True)

    def test_attacks(self):
        variants=[fixture()+[item('/escape')],fixture()+[item(p.ROOT+'/../escape')],
                  fixture()+[item(p.ROOT+'/pipe',tarfile.FIFOTYPE)],
                  fixture()+[item(p.ROOT+'/hard',tarfile.LNKTYPE,'/etc/passwd')],
                  fixture()+[item(p.ROOT)],
                  fixture()+[item(p.ROOT+'/missing/child',tarfile.REGTYPE)],
                  fixture()+[item(p.ROOT+'/CMake.app/Contents/bin/cmake-gui/child',tarfile.REGTYPE)]]
        m=fixture();m[-1].linkname='/outside';variants.append(m)
        m=fixture();m[-2].mode=0o4755;variants.append(m)
        m=fixture();m[-2].size=300000001;variants.append(m)
        m=fixture();del m[-2];variants.append(m)
        for members in variants:
            with self.assertRaises(ValueError):p.manifest(members,False)
    def test_size_and_digest_required(self):
        p.verify_digest(p.SIZE,p.SHA)
        for size,digest in ((p.SIZE-1,p.SHA),(p.SIZE,'0'*64),(p.SIZE+1,p.SHA)):
            with self.assertRaises(ValueError):p.verify_digest(size,digest)

    def test_real_inventory_required(self):
        with self.assertRaises(ValueError):p.manifest(fixture())
    def test_redirect_allowlist(self):
        good='https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sig=synthetic'
        self.assertEqual(p.redirect(good),good)
        for url in ('http://release-assets.githubusercontent.com/github-production-release-asset/x',
                    'https://github.com/other','https://release-assets.githubusercontent.com/other',
                    'https://user:secret@release-assets.githubusercontent.com/github-production-release-asset/x',
                    good+'#fragment'):
            with self.assertRaises(ValueError):p.redirect(url)
    def test_pins_and_workflow(self):
        self.assertEqual(p.SIZE,89451542)
        self.assertEqual(p.SHA,'679e77a96b9ef9484cc6c1445255fc4f84206b48a65420efbedf16bdab55fbff')
        self.assertEqual(len(p.LINKS),13)
        text=(Path(__file__).resolve().parents[1]/'.github/workflows/ios-app-obs-demo.yml').read_text()
        self.assertEqual(text.count('run: python3 ios-app-obs-demo/provision_cmake.py'),2)
        for job in text.split('runs-on: macos-15')[1:]:
            self.assertLess(job.index('provision_cmake.py'),job.index('prerequisites.sh'))
if __name__=='__main__':unittest.main()
