#!/usr/bin/env python3
"""Pinned job-private tool provisioning. No system install or fallback."""
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT='cmake-4.4.5-macos-universal'
URL='https://github.com/Kitware/CMake/releases/download/v4.4.5/'+ROOT+'.tar.gz'
SIZE=89451542
SHA='679e77a96b9ef9484cc6c1445255fc4f84206b48a65420efbedf16bdab55fbff'
CLI=ROOT+'/CMake.app/Contents/bin/cmake'
LINKS={ROOT+'/CMake.app/Contents/bin/cmake-gui':'../MacOS/CMake'}
for name in ('QtWidgets','QtGui','QtDBus','QtCore'):
    base=ROOT+'/CMake.app/Contents/Frameworks/'+name+'.framework/'
    LINKS.update({base+name:'Versions/Current/'+name,base+'Resources':'Versions/Current/Resources',base+'Versions/Current':'A'})

def manifest(members, exact=True):
    if len(members)>10000: raise ValueError('member cap')
    entries={}; counts=[0,0,0]; total=0
    for m in members:
        name=m.name.rstrip('/') if m.isdir() else m.name
        parts=PurePosixPath(name).parts
        if not name or name.startswith('/') or name!=str(PurePosixPath(name)) or any(p in ('.','..') for p in parts): raise ValueError('name')
        if parts[0]!=ROOT or not re.fullmatch(r'[A-Za-z0-9_./+@ -]+',name): raise ValueError('root or character')
        if name in entries or m.mode & 0o7000: raise ValueError('duplicate or mode')
        if m.isdir(): counts[0]+=1
        elif m.isreg():
            counts[1]+=1;total+=m.size
            if m.size<0 or total>300000000: raise ValueError('size cap')
        elif m.issym():
            counts[2]+=1
            if LINKS.get(name)!=m.linkname: raise ValueError('link allowlist')
        else: raise ValueError('type')
        entries[name]=m
    for name,m in entries.items():
        for parent in PurePosixPath(name).parents:
            if str(parent)=='.': break
            # Exact official tar omits only its single top-level directory.
            if str(parent)==ROOT and ROOT not in entries: continue
            if str(parent) not in entries or not entries[str(parent)].isdir(): raise ValueError('parent')
    def resolve(parts,seen):
        resolved=[]
        for index,part in enumerate(parts):
            if part=='.':continue
            if part=='..':
                if len(resolved)<=1: raise ValueError('escape')
                resolved.pop();continue
            resolved.append(part);key='/'.join(resolved)
            if key in entries and entries[key].issym():
                if key in seen: raise ValueError('cycle')
                return resolve(resolved[:-1]+entries[key].linkname.split('/')+parts[index+1:],seen|{key})
        key='/'.join(resolved)
        if key not in entries or entries[key].issym(): raise ValueError('dangling')
        return key
    for name,m in entries.items():
        if m.issym(): resolve(name.split('/'),set())
    if exact and (len(entries)!=9041 or counts!=[189,8839,13] or total!=252727945 or {n for n,m in entries.items() if m.issym()}!=set(LINKS)):
        raise ValueError('official inventory')
    if exact and (CLI not in entries or not entries[CLI].isreg() or entries[CLI].mode!=0o755): raise ValueError('cli')
    return entries

def extract(archive,dest,exact=True):
    if dest.exists() or dest.is_symlink(): raise ValueError('fresh destination')
    with tarfile.open(archive,'r:gz') as tar:
        entries=manifest(tar.getmembers(),exact)
        dest.mkdir(mode=0o700)
        # Production exact inventory/digest must already pass. No other parent
        # is synthesized; an explicit non-directory root fails validation.
        if ROOT not in entries: (dest/ROOT).mkdir(mode=0o700)
        for name,m in sorted(entries.items(),key=lambda pair:len(PurePosixPath(pair[0]).parts)):
            if m.isdir(): (dest/name).mkdir(mode=0o700)
        for name,m in entries.items():
            if m.isreg():
                source=tar.extractfile(m)
                if source is None: raise ValueError('file payload')
                with source, (dest/name).open('xb') as out: shutil.copyfileobj(source,out,1024*1024)
                if (dest/name).stat().st_size!=m.size: raise ValueError('file size')
                (dest/name).chmod(m.mode & 0o755)
        for name,m in entries.items():
            if m.issym(): (dest/name).symlink_to(m.linkname)

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self,*args,**kwargs): return None

def redirect(location):
    u=urllib.parse.urlsplit(location)
    if u.scheme!='https' or u.hostname!='release-assets.githubusercontent.com' or u.port not in (None,443) or u.username or u.password or u.fragment or not u.path.startswith('/github-production-release-asset/'):
        raise ValueError('redirect')
    return location

def verify_digest(size, digest):
    if size!=SIZE or digest!=SHA: raise ValueError('digest')

def download(path):
    deadline=time.monotonic()+600
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect())
    try: response=opener.open(URL,timeout=60)
    except urllib.error.HTTPError as error:
        if error.code!=302: raise ValueError('origin response') from None
        destination=redirect(error.headers.get('Location',''));error.close()
        response=opener.open(destination,timeout=60) # no second redirect permitted
    digest=hashlib.sha256();size=0
    with response, path.open('xb') as out:
        if response.status!=200: raise ValueError('response')
        while True:
            if time.monotonic()>deadline: raise ValueError('download deadline')
            data=response.read(1024*1024)
            if not data:break
            size+=len(data)
            if size>SIZE: raise ValueError('download cap')
            digest.update(data);out.write(data)
    verify_digest(size,digest.hexdigest())

def main():
    stage='environment';work=None
    try:
        if os.uname().sysname!='Darwin' or os.uname().machine!='arm64': raise ValueError('host')
        if any(k.startswith('DYLD_') for k in os.environ) or os.environ.get('CMAKE_ROOT'): raise ValueError('override')
        parent=Path(os.environ['RUNNER_TEMP']).resolve()
        if not parent.is_dir(): raise ValueError('temp')
        work=Path(tempfile.mkdtemp(prefix='madinfer-cmake-',dir=parent))
        archive=work/'tool.tar.gz';stage='download-digest';download(archive)
        stage='archive';dest=work/'unpacked';extract(archive,dest);archive.unlink()
        binary=dest/CLI
        stage='executable'
        if binary.is_symlink() or not stat.S_ISREG(binary.stat().st_mode) or not os.access(binary,os.X_OK): raise ValueError('cli')
        arches=subprocess.check_output(['/usr/bin/xcrun','lipo','-archs',str(binary)],stderr=subprocess.DEVNULL,text=True,timeout=30).split()
        if set(arches)!={'arm64','x86_64'}: raise ValueError('architecture')
        version=subprocess.check_output(['/usr/bin/arch','-arm64',str(binary),'--version'],stderr=subprocess.DEVNULL,text=True,timeout=30)
        if version.splitlines()[0]!='cmake version 4.4.5': raise ValueError('version')
        stage='job-path'
        value=str(binary.parent)
        if '\n' in value or '\r' in value: raise ValueError('path')
        with open(os.environ['GITHUB_PATH'],'a') as out:out.write(value+'\n')
        print('PASS: pinned-cmake-provision')
    except Exception:
        if work is not None:shutil.rmtree(work,ignore_errors=True)
        print('FAIL: pinned-cmake-'+stage)
        return 1
    return 0
if __name__=='__main__':raise SystemExit(main())
